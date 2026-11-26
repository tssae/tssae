import {
  cpSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { pathToFileURL } from "node:url";

import type {
  EngineCallIoMetrics,
  ProjectContext,
  ProjectSession,
  QueryCompletedEvent,
  QueryObserver,
} from "tssae";

import type {
  ObserveModeName,
  PerfScenarioId,
  SampleEngineStepRecord,
  SampleQueryRecord,
  WorkerRunSample,
} from "./types.ts";

export type WorkerInputConfig = {
  readonly tssaeEntryPath: string;
  readonly corpusDir: string;
  readonly tsconfig: string;
  readonly scenario: PerfScenarioId;
  readonly observeMode: ObserveModeName;
  readonly collectAttribution: boolean;
  readonly primaryFile: string;
  readonly secondaryFile: string;
};

type TssaeModule = {
  readonly openProject: (options: {
    readonly cwd: string;
    readonly tsconfig?: string;
    readonly observe?: ObserveModeName;
    readonly onRpcMetrics?: (metrics: EngineCallIoMetrics) => void;
  }) => Promise<ProjectSession>;
};

function parseVmHwmBytesFromStatus(statusText: string): number {
  const match = /^VmHWM:\s+(\d+)\s+kB$/mu.exec(statusText);
  if (match === null) {
    return 0;
  }
  return Number(match[1]) * 1024;
}

function readSelfVmHwmBytes(): number {
  if (process.platform !== "linux") {
    return process.resourceUsage().maxRSS * 1024;
  }
  try {
    return parseVmHwmBytesFromStatus(readFileSync("/proc/self/status", "utf8"));
  } catch {
    return process.resourceUsage().maxRSS * 1024;
  }
}

function findChildEngineVmHwmBytes(parentPid: number): number {
  if (process.platform !== "linux") {
    return 0;
  }
  let maxBytes = 0;
  try {
    const entries = readdirSync("/proc", { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isDirectory() || !/^\d+$/u.test(entry.name)) {
        continue;
      }
      try {
        const statText = readFileSync(`/proc/${entry.name}/stat`, "utf8");
        const openParen = statText.indexOf("(");
        const closeParen = statText.lastIndexOf(")");
        if (openParen === -1 || closeParen === -1) {
          continue;
        }
        const comm = statText.slice(openParen + 1, closeParen);
        const rest = statText.slice(closeParen + 2).split(" ");
        const ppid = Number(rest[1]);
        if (ppid === parentPid && comm.startsWith("tssae-engine")) {
          const statusText = readFileSync(`/proc/${entry.name}/status`, "utf8");
          const hwm = parseVmHwmBytesFromStatus(statusText);
          if (hwm > maxBytes) {
            maxBytes = hwm;
          }
        }
      } catch {
        // Process may have exited between readdir and read.
      }
    }
  } catch {
    return 0;
  }
  return maxBytes;
}

async function runQueryWorkload(
  ctx: ProjectContext,
  primaryFile: string,
  secondaryFile: string,
): Promise<void> {
  const [primaryCalls, primaryFns, primaryVars, secondaryCalls] =
    await Promise.all([
      ctx.findSyntax({ files: [primaryFile], kinds: ["CallExpression"] }),
      ctx.findSyntax({
        files: [primaryFile],
        kinds: ["FunctionDeclaration", "MethodDeclaration"],
      }),
      ctx.findSyntax({ files: [primaryFile], kinds: ["VariableDeclaration"] }),
      ctx.findSyntax({ files: [secondaryFile], kinds: ["CallExpression"] }),
    ]);

  const callSlice = [
    ...primaryCalls.slice(0, 24),
    ...secondaryCalls.slice(0, 16),
  ];
  const callRefs = callSlice.map((item) => item.ref);

  if (callRefs.length > 0) {
    await ctx.typeAt(callRefs.slice(0, 24));
    const resolvedCalls = await ctx.resolveCalls(callRefs, {
      select: { signature: true, calleeDeclaration: true, arguments: true },
    });
    const validCallRefs = resolvedCalls
      .map((row) => row.call)
      .filter((ref): ref is NonNullable<typeof ref> => ref !== null);

    if (validCallRefs.length > 0) {
      const callees = await ctx.calleesOf(validCallRefs.slice(0, 20));
      const declRefs = callees
        .flatMap((item) =>
          item.resolvedTargets.map((target) => target.declaration),
        )
        .slice(0, 12);
      if (declRefs.length > 0) {
        await ctx.callersOf(declRefs, {
          lookupFiles: [primaryFile, secondaryFile],
        });
      }
      await ctx.resolveImplementations(validCallRefs.slice(0, 16));
      await ctx.callEffects({
        targets: validCallRefs.slice(0, 12),
        followCallbacks: true,
        includeReads: true,
        includeWrites: true,
      });
    }
  }

  const fnSlice = primaryFns.slice(0, 12);
  if (fnSlice.length > 0) {
    const branchLists = await ctx.listBranches(fnSlice.map((fn) => fn.ref));
    const targetFn = branchLists.find((item) => item.branches.length > 0);
    if (targetFn !== undefined && primaryCalls.length > 0) {
      await ctx.branchCoverage({
        function: targetFn.target,
        branches: targetFn.branches.slice(0, 4).map((b) => b.ref),
        targets: primaryCalls.slice(0, 6).map((c) => c.ref),
      });
    }
    const scopes = fnSlice.slice(0, 8).map((fn) => fn.ref);
    await ctx.instanceCleanup({ scopes, lookupFiles: [primaryFile] });
  }

  const varInits = primaryVars
    .map((item) => item.initializer)
    .filter((init): init is NonNullable<typeof init> => init !== undefined);
  if (varInits.length >= 2) {
    await ctx.valuePropagation({
      from: varInits.slice(0, 6),
      to: varInits.slice(6, 12),
      includeMemberAccess: true,
      lookupFiles: [primaryFile],
    });
    await ctx.resolveInstances(varInits.slice(0, 10), {
      lookupFiles: [primaryFile],
    });
  }
}

async function runWorker(): Promise<void> {
  const configPath = process.argv[2];
  if (configPath === undefined) {
    throw new Error("Worker requires a JSON config file path argument.");
  }
  const config = JSON.parse(
    readFileSync(resolve(configPath), "utf8"),
  ) as WorkerInputConfig;

  const tssae = (await import(
    pathToFileURL(resolve(config.tssaeEntryPath)).href
  )) as TssaeModule;

  const steps: SampleEngineStepRecord[] = [];
  const queries: SampleQueryRecord[] = [];
  let rpcBytesSent = 0;
  let rpcBytesReceived = 0;

  const onRpcMetrics = (metrics: EngineCallIoMetrics): void => {
    rpcBytesSent += metrics.bytesSent;
    rpcBytesReceived += metrics.bytesReceived;
    if (
      config.collectAttribution &&
      metrics.engineMetrics?.steps !== undefined
    ) {
      for (const step of metrics.engineMetrics.steps) {
        steps.push({
          op: step.op,
          provider: step.provider,
          cacheStatus: step.cacheStatus,
          durationMs: step.durationMs,
          candidateCount: step.candidateCount,
        });
      }
    }
  };

  const onQueryCompleted = (event: QueryCompletedEvent): void => {
    if (config.collectAttribution) {
      queries.push({
        op: event.op,
        cacheStatus: event.cacheStatus,
        durationMs: event.durationMs,
        candidateCount: event.candidateCount,
      });
    }
  };

  const buildObserver = (): QueryObserver | undefined => {
    if (config.observeMode === "off" && !config.collectAttribution) {
      return undefined;
    }
    return {
      observe: config.observeMode,
      onRpcMetrics,
      ...(config.collectAttribution ? { onQueryCompleted } : {}),
    };
  };

  let mutableCopyDir: string | undefined;
  let workCwd = config.corpusDir;
  if (config.scenario === "updateQuery") {
    mutableCopyDir = mkdtempSync(join(tmpdir(), "tssae-perf-mutable-"));
    cpSync(config.corpusDir, mutableCopyDir, { recursive: true });
    workCwd = mutableCopyDir;
  }

  let session: ProjectSession | undefined;
  let wallMs = 0;
  let peakHostRssBytes = 0;
  let peakEngineRssBytes = 0;
  let sessionHostRssBytes = 0;
  let sessionEngineRssBytes: number | undefined;

  try {
    const useOpenObserve =
      config.scenario === "openProject" &&
      (config.observeMode !== "off" || config.collectAttribution);

    const openStartedAt = performance.now();
    session = await tssae.openProject({
      cwd: workCwd,
      tsconfig: config.tsconfig,
      ...(useOpenObserve ? { observe: config.observeMode, onRpcMetrics } : {}),
    });

    if (config.scenario === "openProject") {
      session.project();
      wallMs = performance.now() - openStartedAt;
    } else if (config.scenario === "coldQuery") {
      const observer = buildObserver();
      const ctx = session.project(
        undefined,
        observer !== undefined ? { observer } : undefined,
      );
      const startedAt = performance.now();
      await runQueryWorkload(ctx, config.primaryFile, config.secondaryFile);
      wallMs = performance.now() - startedAt;
    } else if (config.scenario === "cachedQuery") {
      const warmupCtx = session.project();
      await runQueryWorkload(
        warmupCtx,
        config.primaryFile,
        config.secondaryFile,
      );
      const observer = buildObserver();
      const timedCtx = session.project(
        undefined,
        observer !== undefined ? { observer } : undefined,
      );
      const startedAt = performance.now();
      for (let iteration = 0; iteration < 15; iteration += 1) {
        await runQueryWorkload(
          timedCtx,
          config.primaryFile,
          config.secondaryFile,
        );
      }
      wallMs = performance.now() - startedAt;
    } else {
      // updateQuery: warm up initial snapshot outside timer, mutate file, time update + query
      const warmupCtx = session.project();
      await runQueryWorkload(
        warmupCtx,
        config.primaryFile,
        config.secondaryFile,
      );

      const targetPath = join(workCwd, config.primaryFile);
      const original = readFileSync(targetPath, "utf8");
      writeFileSync(
        targetPath,
        `${original}\nexport function __tssaePerfProbe(x: number): number { if (x > 0) { return x + 1; } return 0; }\n`,
        "utf8",
      );

      const observer = buildObserver();
      const useUpdateObserve =
        config.observeMode !== "off" || config.collectAttribution;
      const startedAt = performance.now();
      await session.update({
        changed: [config.primaryFile],
        ...(useUpdateObserve
          ? { observe: config.observeMode, onRpcMetrics }
          : {}),
      });
      const updatedCtx = session.project(
        undefined,
        observer !== undefined ? { observer } : undefined,
      );
      await runQueryWorkload(
        updatedCtx,
        config.primaryFile,
        config.secondaryFile,
      );
      wallMs = performance.now() - startedAt;
    }

    // Measure Linux VmHWM before closing session so the child engine PID is still alive.
    peakHostRssBytes = readSelfVmHwmBytes();
    peakEngineRssBytes = findChildEngineVmHwmBytes(process.pid);
    sessionHostRssBytes = session.memory.hostRssBytes;
    sessionEngineRssBytes = session.memory.engineRssBytes;
    if (peakEngineRssBytes === 0 && sessionEngineRssBytes !== undefined) {
      peakEngineRssBytes = sessionEngineRssBytes;
    }
  } finally {
    if (session !== undefined) {
      await session.close();
    }
    if (mutableCopyDir !== undefined) {
      rmSync(mutableCopyDir, { recursive: true, force: true });
    }
  }

  const output: WorkerRunSample = {
    wallMs,
    peakHostRssBytes,
    peakEngineRssBytes,
    peakTotalRssBytes: peakHostRssBytes + peakEngineRssBytes,
    sessionHostRssBytes,
    ...(sessionEngineRssBytes !== undefined ? { sessionEngineRssBytes } : {}),
    observeMode: config.observeMode,
    rpcBytesSent,
    rpcBytesReceived,
    steps,
    queries,
  };
  process.stdout.write(`${JSON.stringify(output)}\n`);
}

void runWorker().catch((error: unknown) => {
  process.stderr.write(
    `${error instanceof Error ? (error.stack ?? error.message) : String(error)}\n`,
  );
  process.exitCode = 1;
});
