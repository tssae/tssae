import { execFile } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { packWorkspaceArtifacts } from "../artifacts.ts";
import {
  prepareBaselineArtifact,
  readRepoHeadState,
  resolveBaselineRevision,
  resolveRepoRoot,
} from "../baseline.ts";
import {
  prepareAcceptanceHost,
  type PreparedAcceptanceHost,
} from "../host-lifecycle.ts";
import { materializeCorpus } from "../perf/corpus.ts";
import type { CorpusProfileId } from "../perf/types.ts";
import { diffDigests } from "./diff.ts";
import type {
  CollectQueryDigestOptions,
  QueryDigest,
  SemanticDiffResult,
  SemanticDiffWaiver,
} from "./types.ts";
import {
  loadSemanticDiffWaivers,
  resolveDefaultWaiversPath,
} from "./waivers.ts";
import type { SemanticDiffWorkerConfig } from "./worker.ts";

const execFileAsync = promisify(execFile);

export type RunSemanticDiffOptions = {
  readonly profile?: CorpusProfileId;
  readonly projectDir?: string;
  readonly tsconfig?: string;
  readonly baselineRev?: string;
  readonly baselineCommit?: string;
  readonly waiversPath?: string;
  readonly waivers?: readonly SemanticDiffWaiver[];
  readonly forceDualRun?: boolean;
  readonly refreshCorpus?: boolean;
  readonly noCorpusCache?: boolean;
  readonly refreshBaseline?: boolean;
  readonly corpusCacheDir?: string;
  readonly perfCacheDir?: string;
  readonly repoRoot?: string;
  readonly baselineEntryPath?: string;
  readonly candidateEntryPath?: string;
  readonly collectOptions?: CollectQueryDigestOptions;
  readonly logProgress?: (message: string) => void;
};

export type SemanticDiffGateRunResult = {
  readonly diff: SemanticDiffResult;
  readonly baselineRev: string;
  readonly baselineCommit: string;
  readonly corpusDir: string;
  readonly tsconfig: string | undefined;
  readonly fastPath: boolean;
  readonly baselineDurationMs: number;
  readonly candidateDurationMs: number;
  readonly totalDurationMs: number;
};

function resolveWorkerInvocation(): {
  readonly nodeArgsPrefix: readonly string[];
  readonly scriptPath: string;
} {
  const currentDir = dirname(fileURLToPath(import.meta.url));
  const compiledSameDir = resolve(currentDir, "worker.js");
  if (existsSync(compiledSameDir)) {
    return { nodeArgsPrefix: [], scriptPath: compiledSameDir };
  }
  const compiledInDist = resolve(
    currentDir,
    "../../dist/semantic-diff/worker.js",
  );
  if (existsSync(compiledInDist)) {
    return { nodeArgsPrefix: [], scriptPath: compiledInDist };
  }
  const sourceWorker = resolve(currentDir, "worker.ts");
  if (existsSync(sourceWorker)) {
    return {
      nodeArgsPrefix: ["--conditions=source"],
      scriptPath: sourceWorker,
    };
  }
  throw new Error(
    `Semantic diff worker not found at ${compiledSameDir}, ${compiledInDist}, or ${sourceWorker}.`,
  );
}

function resolveInstalledTssaeEntry(host: PreparedAcceptanceHost): string {
  return resolve(host.hostDir, "node_modules", "tssae", "dist", "index.js");
}

async function runDigestWorker(options: {
  readonly worker: {
    readonly nodeArgsPrefix: readonly string[];
    readonly scriptPath: string;
  };
  readonly tssaeEntryPath: string;
  readonly cwd: string;
  readonly projectDir: string;
  readonly tsconfig?: string;
  readonly tempDir: string;
  readonly label: string;
  readonly collectOptions?: CollectQueryDigestOptions;
}): Promise<{
  readonly digest: QueryDigest;
  readonly durationMs: number;
}> {
  const configPath = join(options.tempDir, `${options.label}-config.json`);
  const digestOutPath = join(options.tempDir, `${options.label}-digest.json`);
  const workerConfig: SemanticDiffWorkerConfig = {
    tssaeEntryPath: options.tssaeEntryPath,
    projectDir: options.projectDir,
    ...(options.tsconfig === undefined ? {} : { tsconfig: options.tsconfig }),
    digestOutPath,
    ...(options.collectOptions === undefined
      ? {}
      : { collectOptions: options.collectOptions }),
  };
  writeFileSync(configPath, `${JSON.stringify(workerConfig)}\n`, "utf8");

  const { stdout } = await execFileAsync(
    process.execPath,
    [...options.worker.nodeArgsPrefix, options.worker.scriptPath, configPath],
    {
      cwd: options.cwd,
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
      timeout: 300_000,
    },
  );
  const lines = stdout
    .trim()
    .split("\n")
    .filter((line) => line.trim().length > 0);
  const lastLine = lines.at(-1);
  if (lastLine === undefined) {
    throw new Error(
      `Semantic diff worker (${options.label}) produced no summary output.`,
    );
  }
  const summary = JSON.parse(lastLine) as { readonly durationMs: number };
  const digest = JSON.parse(readFileSync(digestOutPath, "utf8")) as QueryDigest;
  return { digest, durationMs: summary.durationMs };
}

/**
 * Executes the A/B semantic diff gate comparing the resolved baseline `tssae`
 * package against the current candidate on the cached `vscode` corpus or a
 * target project directory, applying only active `baselineCommit` waivers.
 */
export async function runSemanticDiffGate(
  options: RunSemanticDiffOptions = {},
): Promise<SemanticDiffGateRunResult> {
  const totalStart = performance.now();
  const log =
    options.logProgress ?? ((msg: string) => process.stderr.write(`${msg}\n`));

  const customEntriesProvided =
    options.baselineEntryPath !== undefined &&
    options.candidateEntryPath !== undefined;

  let repoRoot = options.repoRoot;
  let baselineRev = options.baselineRev ?? "main";
  let baselineCommit = options.baselineCommit;
  let canUseFastPath = false;

  if (!customEntriesProvided) {
    repoRoot = repoRoot ?? resolveRepoRoot();
    const resolvedRev = resolveBaselineRevision(repoRoot, options.baselineRev);
    baselineRev = resolvedRev.rev;
    baselineCommit = baselineCommit ?? resolvedRev.commitSha;
    const headState = readRepoHeadState(repoRoot);
    canUseFastPath =
      options.forceDualRun !== true &&
      options.baselineEntryPath === undefined &&
      !headState.dirty &&
      headState.commitSha === resolvedRev.commitSha;
  } else if (baselineCommit === undefined) {
    repoRoot = repoRoot ?? resolveRepoRoot();
    const resolvedRev = resolveBaselineRevision(repoRoot, options.baselineRev);
    baselineRev = resolvedRev.rev;
    baselineCommit = resolvedRev.commitSha;
  }

  let projectDir: string;
  let tsconfig: string | undefined;
  if (options.projectDir !== undefined) {
    projectDir = resolve(options.projectDir);
    tsconfig = options.tsconfig ?? "tsconfig.json";
  } else {
    const profile = options.profile ?? "medium";
    log(`[semantic-diff] Materializing ${profile} corpus...`);
    const corpus = await materializeCorpus({
      profile,
      ...(options.corpusCacheDir === undefined
        ? {}
        : { cacheDir: options.corpusCacheDir }),
      ...(options.refreshCorpus === undefined
        ? {}
        : { refreshCorpus: options.refreshCorpus }),
      ...(options.noCorpusCache === undefined
        ? {}
        : { noCorpusCache: options.noCorpusCache }),
    });
    projectDir = corpus.corpusDir;
    tsconfig = options.tsconfig ?? "tsconfig.perf.json";
  }

  const waivers =
    options.waivers ??
    loadSemanticDiffWaivers(options.waiversPath ?? resolveDefaultWaiversPath());

  const worker = resolveWorkerInvocation();
  const tempDir = mkdtempSync(join(tmpdir(), "tssae-semantic-diff-"));

  let hostA: PreparedAcceptanceHost | undefined;
  let hostB: PreparedAcceptanceHost | undefined;
  let baselineBundle:
    | Awaited<ReturnType<typeof packWorkspaceArtifacts>>
    | undefined;
  let candidateBundle:
    | Awaited<ReturnType<typeof packWorkspaceArtifacts>>
    | undefined;

  try {
    if (canUseFastPath) {
      log(
        `[semantic-diff] HEAD matches clean baseline ${baselineCommit.slice(0, 12)}; running fast single-side verification...`,
      );
      let candidateEntry: string;
      if (options.candidateEntryPath !== undefined) {
        candidateEntry = resolve(options.candidateEntryPath);
      } else {
        candidateBundle = await packWorkspaceArtifacts({
          packages: ["tssae"],
          cwd: repoRoot!,
        });
        hostB = await prepareAcceptanceHost({ bundle: candidateBundle });
        candidateEntry = resolveInstalledTssaeEntry(hostB);
      }

      const candidateRun = await runDigestWorker({
        worker,
        tssaeEntryPath: candidateEntry,
        cwd: hostB?.hostDir ?? projectDir,
        projectDir,
        ...(tsconfig === undefined ? {} : { tsconfig }),
        tempDir,
        label: "candidate",
        ...(options.collectOptions === undefined
          ? {}
          : { collectOptions: options.collectOptions }),
      });

      const diff = diffDigests(candidateRun.digest, candidateRun.digest, {
        baselineCommit,
        waivers,
      });

      return {
        diff,
        baselineRev,
        baselineCommit,
        corpusDir: projectDir,
        tsconfig,
        fastPath: true,
        baselineDurationMs: 0,
        candidateDurationMs: candidateRun.durationMs,
        totalDurationMs: performance.now() - totalStart,
      };
    }

    let baselineEntry: string;
    let candidateEntry: string;
    let baselineCwd: string;
    let candidateCwd: string;

    if (customEntriesProvided) {
      baselineEntry = resolve(options.baselineEntryPath!);
      candidateEntry = resolve(options.candidateEntryPath!);
      baselineCwd = projectDir;
      candidateCwd = projectDir;
    } else {
      log(
        `[semantic-diff] Preparing baseline (${baselineCommit.slice(0, 12)}) and candidate artifacts...`,
      );
      const baselineArtifact = await prepareBaselineArtifact({
        repoRoot: repoRoot!,
        ...(options.baselineRev === undefined
          ? {}
          : { baselineRev: options.baselineRev }),
        ...(options.perfCacheDir === undefined
          ? {}
          : { cacheDir: options.perfCacheDir }),
        ...(options.refreshBaseline === undefined
          ? {}
          : { refreshBaseline: options.refreshBaseline }),
      });
      baselineBundle = await packWorkspaceArtifacts({
        packages: ["tssae"],
        fromPackDir: baselineArtifact.cacheDir,
      });
      candidateBundle = await packWorkspaceArtifacts({
        packages: ["tssae"],
        cwd: repoRoot!,
      });

      log(
        "[semantic-diff] Installing baseline and candidate packages into isolated hosts...",
      );
      hostA = await prepareAcceptanceHost({ bundle: baselineBundle });
      hostB = await prepareAcceptanceHost({ bundle: candidateBundle });
      baselineEntry = resolveInstalledTssaeEntry(hostA);
      candidateEntry = resolveInstalledTssaeEntry(hostB);
      baselineCwd = hostA.hostDir;
      candidateCwd = hostB.hostDir;
    }

    log("[semantic-diff] Collecting baseline query digest...");
    const baselineRun = await runDigestWorker({
      worker,
      tssaeEntryPath: baselineEntry,
      cwd: baselineCwd,
      projectDir,
      ...(tsconfig === undefined ? {} : { tsconfig }),
      tempDir,
      label: "baseline",
      ...(options.collectOptions === undefined
        ? {}
        : { collectOptions: options.collectOptions }),
    });

    log("[semantic-diff] Collecting candidate query digest...");
    const candidateRun = await runDigestWorker({
      worker,
      tssaeEntryPath: candidateEntry,
      cwd: candidateCwd,
      projectDir,
      ...(tsconfig === undefined ? {} : { tsconfig }),
      tempDir,
      label: "candidate",
      ...(options.collectOptions === undefined
        ? {}
        : { collectOptions: options.collectOptions }),
    });

    const diff = diffDigests(baselineRun.digest, candidateRun.digest, {
      baselineCommit,
      waivers,
    });

    return {
      diff,
      baselineRev,
      baselineCommit,
      corpusDir: projectDir,
      tsconfig,
      fastPath: false,
      baselineDurationMs: baselineRun.durationMs,
      candidateDurationMs: candidateRun.durationMs,
      totalDurationMs: performance.now() - totalStart,
    };
  } finally {
    await hostA?.dispose();
    await hostB?.dispose();
    baselineBundle?.dispose();
    candidateBundle?.dispose();
    rmSync(tempDir, { recursive: true, force: true });
  }
}
