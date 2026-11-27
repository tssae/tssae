import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";

import {
  collectMatchingCorpusFiles,
  materializeExternalCorpus,
  type ExternalCorpusSpec,
} from "../corpus.ts";
import { CONFORMANCE_CORPORA, DEFAULT_CONFORMANCE_CORPORA } from "./corpora.ts";
import type {
  ConformanceBatchWorkerConfig,
  ConformanceBatchWorkerResult,
  ConformanceCorpusId,
  ConformanceCorpusResult,
  ConformanceFailureRecord,
  ConformanceReport,
} from "./types.ts";

export const DEFAULT_CONFORMANCE_BATCH_SIZE = 300;
export const DEFAULT_CONFORMANCE_CONCURRENCY = 4;
export const DEFAULT_CONFORMANCE_BATCH_TIMEOUT_MS = 120_000;

export type RunConformanceSweepOptions = {
  readonly corpora?: readonly (ConformanceCorpusId | ExternalCorpusSpec)[];
  readonly corpusCacheDir?: string;
  readonly refreshCorpus?: boolean;
  readonly noCorpusCache?: boolean;
  readonly hostMirrorPaths?: readonly string[];
  readonly batchSize?: number;
  readonly concurrency?: number;
  readonly batchTimeoutMs?: number;
  readonly tssaeEntryPath?: string;
  readonly workerScriptPath?: string;
  readonly logProgress?: (message: string) => void;
};

type WorkerExecutionOutcome =
  | {
      readonly ok: true;
      readonly result: ConformanceBatchWorkerResult;
    }
  | {
      readonly ok: false;
      readonly kind: "timeout" | "crash" | "error";
      readonly message: string;
    };

function resolveWorkerScript(): {
  readonly path: string;
  readonly needsSourceCondition: boolean;
} {
  const currentDir = dirname(fileURLToPath(import.meta.url));
  const compiledSameDir = resolve(currentDir, "worker.js");
  if (existsSync(compiledSameDir)) {
    return { path: compiledSameDir, needsSourceCondition: false };
  }
  const compiledDist = resolve(currentDir, "../../dist/conformance/worker.js");
  if (existsSync(compiledDist)) {
    return { path: compiledDist, needsSourceCondition: false };
  }
  const sourceWorker = resolve(currentDir, "worker.ts");
  if (existsSync(sourceWorker)) {
    return { path: sourceWorker, needsSourceCondition: true };
  }
  throw new Error(
    `Conformance worker not found at ${compiledSameDir}, ${compiledDist}, or ${sourceWorker}.`,
  );
}

function killWorkerProcessTree(pid: number | undefined): void {
  if (pid === undefined) {
    return;
  }
  if (process.platform !== "win32") {
    try {
      process.kill(-pid, "SIGKILL");
      return;
    } catch {
      // Process group may already have exited; fall back to direct pid kill.
    }
  }
  try {
    process.kill(pid, "SIGKILL");
  } catch {
    // Already exited.
  }
}

async function runBatchWorker(options: {
  readonly workerScript: {
    readonly path: string;
    readonly needsSourceCondition: boolean;
  };
  readonly tssaeEntryPath: string;
  readonly corpusId: string;
  readonly corpusDir: string;
  readonly files: readonly string[];
  readonly configDir: string;
  readonly label: string;
  readonly timeoutMs: number;
}): Promise<WorkerExecutionOutcome> {
  const configPath = join(options.configDir, `${options.label}.json`);
  const config: ConformanceBatchWorkerConfig = {
    tssaeEntryPath: options.tssaeEntryPath,
    corpusDir: options.corpusDir,
    corpusId: options.corpusId,
    files: options.files,
  };
  writeFileSync(configPath, `${JSON.stringify(config)}\n`, "utf8");

  const args = options.workerScript.needsSourceCondition
    ? ["--conditions=source", options.workerScript.path, configPath]
    : [options.workerScript.path, configPath];

  return new Promise<WorkerExecutionOutcome>((resolveOutcome) => {
    const child = spawn(process.execPath, args, {
      cwd: options.configDir,
      detached: process.platform !== "win32",
      stdio: ["ignore", "pipe", "pipe"],
    });

    const stdoutChunks: string[] = [];
    const stderrChunks: string[] = [];
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => stdoutChunks.push(chunk));
    child.stderr.on("data", (chunk: string) => stderrChunks.push(chunk));

    let settled = false;
    let timedOut = false;

    const timer = setTimeout(() => {
      timedOut = true;
      killWorkerProcessTree(child.pid);
    }, options.timeoutMs);

    child.once("error", (error) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      resolveOutcome({
        ok: false,
        kind: "crash",
        message: error.message,
      });
    });

    child.once("close", (code, signal) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);

      if (timedOut) {
        resolveOutcome({
          ok: false,
          kind: "timeout",
          message: `Batch timed out after ${String(options.timeoutMs)}ms (${String(options.files.length)} files).`,
        });
        return;
      }

      const stderrText = stderrChunks.join("").trim();
      if (code !== 0 || signal !== null) {
        const detail =
          stderrText.length > 0
            ? stderrText
            : `Worker exited abnormally (code=${String(code)}, signal=${String(signal)}).`;
        resolveOutcome({
          ok: false,
          kind: "crash",
          message: detail,
        });
        return;
      }

      const lines = stdoutChunks
        .join("")
        .trim()
        .split("\n")
        .filter((line) => line.trim().length > 0);
      const lastLine = lines.at(-1);
      if (lastLine === undefined) {
        resolveOutcome({
          ok: false,
          kind: "crash",
          message: `Worker produced no JSON output.${stderrText.length > 0 ? ` stderr: ${stderrText}` : ""}`,
        });
        return;
      }

      try {
        const parsed = JSON.parse(lastLine) as ConformanceBatchWorkerResult;
        resolveOutcome({ ok: true, result: parsed });
      } catch (error) {
        resolveOutcome({
          ok: false,
          kind: "error",
          message: `Failed to parse worker output: ${error instanceof Error ? error.message : String(error)}`,
        });
      }
    });
  });
}

type AggregateBatchOutcome = {
  readonly filesScanned: number;
  readonly scopesChecked: number;
  readonly queriesExecuted: number;
  readonly failures: readonly ConformanceFailureRecord[];
};

async function executeBatchWithBisection(options: {
  readonly workerScript: {
    readonly path: string;
    readonly needsSourceCondition: boolean;
  };
  readonly tssaeEntryPath: string;
  readonly corpusId: string;
  readonly corpusDir: string;
  readonly files: readonly string[];
  readonly batchIndex: number;
  readonly configDir: string;
  readonly nextLabel: () => string;
  readonly timeoutMs: number;
}): Promise<AggregateBatchOutcome> {
  const outcome = await runBatchWorker({
    workerScript: options.workerScript,
    tssaeEntryPath: options.tssaeEntryPath,
    corpusId: options.corpusId,
    corpusDir: options.corpusDir,
    files: options.files,
    configDir: options.configDir,
    label: options.nextLabel(),
    timeoutMs: options.timeoutMs,
  });

  if (outcome.ok) {
    return {
      filesScanned: outcome.result.filesScanned,
      scopesChecked: outcome.result.scopesChecked,
      queriesExecuted: outcome.result.queriesExecuted,
      failures: outcome.result.violations.map((v) => ({
        corpusId: v.corpusId,
        kind: v.kind,
        message: v.message,
        files: [v.file],
        file: v.file,
        query: v.query,
        ...(v.at !== undefined ? { at: v.at } : {}),
        batchIndex: options.batchIndex,
      })),
    };
  }

  if (options.files.length <= 1) {
    const file = options.files[0];
    return {
      filesScanned: 0,
      scopesChecked: 0,
      queriesExecuted: 0,
      failures: [
        {
          corpusId: options.corpusId,
          kind: outcome.kind,
          message: outcome.message,
          files: options.files,
          ...(file !== undefined ? { file } : {}),
          batchIndex: options.batchIndex,
        },
      ],
    };
  }

  const mid = Math.ceil(options.files.length / 2);
  const left = await executeBatchWithBisection({
    ...options,
    files: options.files.slice(0, mid),
  });
  const right = await executeBatchWithBisection({
    ...options,
    files: options.files.slice(mid),
  });

  return {
    filesScanned: left.filesScanned + right.filesScanned,
    scopesChecked: left.scopesChecked + right.scopesChecked,
    queriesExecuted: left.queriesExecuted + right.queriesExecuted,
    failures: [...left.failures, ...right.failures],
  };
}

function resolveCorpusSpecs(
  corpora?: readonly (ConformanceCorpusId | ExternalCorpusSpec)[],
): readonly ExternalCorpusSpec[] {
  const requested = corpora ?? DEFAULT_CONFORMANCE_CORPORA;
  return requested.map((entry) =>
    typeof entry === "string" ? CONFORMANCE_CORPORA[entry] : entry,
  );
}

/**
 * Runs a static conformance corpus sweep over `test262` and `typescript-conformance`
 * (or custom `ExternalCorpusSpec` entries), failing on any engine crash, batch timeout,
 * or `FunctionIr` graph invariant violation.
 */
export async function runConformanceSweep(
  options: RunConformanceSweepOptions = {},
): Promise<ConformanceReport> {
  const totalStart = performance.now();
  const log =
    options.logProgress ?? ((msg: string) => process.stderr.write(`${msg}\n`));
  const batchSize = options.batchSize ?? DEFAULT_CONFORMANCE_BATCH_SIZE;
  const concurrency = options.concurrency ?? DEFAULT_CONFORMANCE_CONCURRENCY;
  const batchTimeoutMs =
    options.batchTimeoutMs ?? DEFAULT_CONFORMANCE_BATCH_TIMEOUT_MS;

  if (!Number.isInteger(batchSize) || batchSize < 1) {
    throw new Error(`Invalid batchSize ${String(batchSize)}: expected >= 1.`);
  }
  if (!Number.isInteger(concurrency) || concurrency < 1) {
    throw new Error(
      `Invalid concurrency ${String(concurrency)}: expected >= 1.`,
    );
  }
  if (!Number.isInteger(batchTimeoutMs) || batchTimeoutMs < 1) {
    throw new Error(
      `Invalid batchTimeoutMs ${String(batchTimeoutMs)}: expected >= 1.`,
    );
  }

  const workerScript =
    options.workerScriptPath !== undefined
      ? {
          path: resolve(options.workerScriptPath),
          needsSourceCondition: options.workerScriptPath.endsWith(".ts"),
        }
      : resolveWorkerScript();
  const tssaeEntryPath = options.tssaeEntryPath ?? "tssae";
  const specs = resolveCorpusSpecs(options.corpora);
  const configDir = mkdtempSync(join(tmpdir(), "tssae-conformance-cfg-"));
  let labelSeq = 0;
  const nextLabel = (): string => `batch-${String(labelSeq++)}`;

  const corpusResults: ConformanceCorpusResult[] = [];
  const allFailures: ConformanceFailureRecord[] = [];

  try {
    for (const spec of specs) {
      const corpusStart = performance.now();
      log(`[conformance] Materializing corpus "${spec.id}"...`);
      const materialized = await materializeExternalCorpus({
        spec,
        ...(options.corpusCacheDir === undefined
          ? {}
          : { cacheDir: options.corpusCacheDir }),
        ...(options.refreshCorpus === undefined
          ? {}
          : { refreshCorpus: options.refreshCorpus }),
        ...(options.noCorpusCache === undefined
          ? {}
          : { noCorpusCache: options.noCorpusCache }),
        ...(options.hostMirrorPaths === undefined
          ? {}
          : { hostMirrorPaths: options.hostMirrorPaths }),
      });

      const matchedFiles = collectMatchingCorpusFiles(
        materialized.corpusDir,
        spec,
      );
      const batches: (readonly string[])[] = [];
      for (let i = 0; i < matchedFiles.length; i += batchSize) {
        batches.push(matchedFiles.slice(i, i + batchSize));
      }

      log(
        `[conformance] Sweeping "${spec.id}" (${String(matchedFiles.length)} files in ${String(batches.length)} batches, concurrency=${String(concurrency)}, origin=${materialized.sourceOrigin})...`,
      );

      const batchOutcomes: AggregateBatchOutcome[] = Array.from({
        length: batches.length,
      });
      let cursor = 0;
      let completedBatches = 0;

      const workerLoop = async (): Promise<void> => {
        while (true) {
          const idx = cursor;
          cursor += 1;
          if (idx >= batches.length) {
            return;
          }
          const files = batches[idx]!;
          const outcome = await executeBatchWithBisection({
            workerScript,
            tssaeEntryPath,
            corpusId: spec.id,
            corpusDir: materialized.corpusDir,
            files,
            batchIndex: idx,
            configDir,
            nextLabel,
            timeoutMs: batchTimeoutMs,
          });
          batchOutcomes[idx] = outcome;
          completedBatches += 1;
          if (
            completedBatches % 20 === 0 ||
            completedBatches === batches.length ||
            outcome.failures.length > 0
          ) {
            log(
              `[conformance] ${spec.id}: ${String(completedBatches)}/${String(batches.length)} batches completed${outcome.failures.length > 0 ? ` (${String(outcome.failures.length)} failures in batch ${String(idx)})` : ""}`,
            );
          }
        }
      };

      const poolSize = Math.min(concurrency, Math.max(1, batches.length));
      await Promise.all(Array.from({ length: poolSize }, () => workerLoop()));

      let scannedFiles = 0;
      let scopesChecked = 0;
      let queriesExecuted = 0;
      const corpusFailures: ConformanceFailureRecord[] = [];
      for (const outcome of batchOutcomes) {
        scannedFiles += outcome.filesScanned;
        scopesChecked += outcome.scopesChecked;
        queriesExecuted += outcome.queriesExecuted;
        corpusFailures.push(...outcome.failures);
      }
      allFailures.push(...corpusFailures);

      corpusResults.push({
        id: spec.id,
        repoName: materialized.repoName,
        commitSha: materialized.commitSha,
        corpusDir: materialized.corpusDir,
        sourceOrigin: materialized.sourceOrigin,
        cacheHit: materialized.cacheHit,
        cacheBypassed: materialized.cacheBypassed,
        matchedFiles: matchedFiles.length,
        scannedFiles,
        scopesChecked,
        queriesExecuted,
        batchesTotal: batches.length,
        durationMs: performance.now() - corpusStart,
        passed: corpusFailures.length === 0,
        failures: corpusFailures,
      });
    }
  } finally {
    rmSync(configDir, { recursive: true, force: true });
  }

  const totalFiles = corpusResults.reduce((s, c) => s + c.scannedFiles, 0);
  const totalScopes = corpusResults.reduce((s, c) => s + c.scopesChecked, 0);
  const totalQueries = corpusResults.reduce((s, c) => s + c.queriesExecuted, 0);

  return {
    passed: allFailures.length === 0,
    totalDurationMs: performance.now() - totalStart,
    totalFiles,
    totalScopes,
    totalQueries,
    batchSize,
    concurrency,
    batchTimeoutMs,
    corpora: corpusResults,
    failures: allFailures,
  };
}
