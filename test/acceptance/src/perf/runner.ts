import { execFile, execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import os, { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { packWorkspaceArtifacts } from "../artifacts.ts";
import {
  prepareBaselineArtifact,
  readRepoHeadState,
  resolveRepoRoot,
} from "../baseline.ts";
import {
  prepareAcceptanceHost,
  type PreparedAcceptanceHost,
} from "../host-lifecycle.ts";
import { materializeCorpus } from "./corpus.ts";
import { SCENARIO_LABELS } from "./report.ts";
import {
  DEFAULT_PERF_BUDGET,
  buildAttributionReport,
  computeRelativeDelta,
  evaluateScenarioBudget,
  summarizeScenarioSamples,
} from "./stats.ts";
import type {
  CorpusProfileId,
  MaterializedCorpus,
  ObserveModeName,
  ObserveOverheadEntry,
  PerfBudgetConfig,
  PerfReport,
  PerfScenarioId,
  ScenarioComparison,
  WorkerRunSample,
} from "./types.ts";
import type { WorkerInputConfig } from "./worker.ts";

const execFileAsync = promisify(execFile);

export type VersionSide = "baseline" | "candidate";

export type RunPerfAbOptions = {
  readonly profile: CorpusProfileId;
  readonly baselineRev?: string;
  readonly rounds?: number;
  readonly warmup?: number;
  readonly scenarios?: readonly PerfScenarioId[];
  readonly includeObserveOverhead?: boolean;
  readonly refreshCorpus?: boolean;
  readonly noCorpusCache?: boolean;
  readonly refreshBaseline?: boolean;
  readonly budget?: PerfBudgetConfig;
  readonly corpusCacheDir?: string;
  readonly perfCacheDir?: string;
  readonly repoRoot?: string;
  readonly logProgress?: (message: string) => void;
};

/**
 * Generates sequential ABBA-alternating pairs for `rounds` rounds without
 * concurrent overlap so thermal or background drift does not bias either side.
 */
export function buildPairedSamplingSchedule(
  rounds: number,
): readonly (readonly [VersionSide, VersionSide])[] {
  const schedule: (readonly [VersionSide, VersionSide])[] = [];
  for (let i = 0; i < rounds; i += 1) {
    schedule.push(
      i % 2 === 0 ? ["baseline", "candidate"] : ["candidate", "baseline"],
    );
  }
  return schedule;
}

function resolveWorkerScriptPath(): string {
  const currentDir = dirname(fileURLToPath(import.meta.url));
  const compiledInDist = resolve(currentDir, "worker.js");
  if (existsSync(compiledInDist)) {
    return compiledInDist;
  }
  const packageDistWorker = resolve(currentDir, "../../dist/perf/worker.js");
  if (existsSync(packageDistWorker)) {
    return packageDistWorker;
  }
  throw new Error(
    `Compiled perf worker not found at ${compiledInDist} or ${packageDistWorker}. Run package build first.`,
  );
}

function resolveInstalledTssaeEntry(host: PreparedAcceptanceHost): string {
  return createRequire(join(host.hostDir, "package.json")).resolve("tssae");
}

async function runWorkerSubprocess(options: {
  readonly workerScriptPath: string;
  readonly host: PreparedAcceptanceHost;
  readonly corpus: MaterializedCorpus;
  readonly scenario: PerfScenarioId;
  readonly observeMode: ObserveModeName;
  readonly collectAttribution: boolean;
  readonly configDir: string;
  readonly label: string;
}): Promise<WorkerRunSample> {
  const configPath = join(options.configDir, `${options.label}.json`);
  const workerConfig: WorkerInputConfig = {
    tssaeEntryPath: resolveInstalledTssaeEntry(options.host),
    corpusDir: options.corpus.corpusDir,
    tsconfig: "tsconfig.perf.json",
    scenario: options.scenario,
    observeMode: options.observeMode,
    collectAttribution: options.collectAttribution,
    primaryFile: options.corpus.primaryFile,
    secondaryFile: options.corpus.secondaryFile,
  };
  writeFileSync(configPath, `${JSON.stringify(workerConfig)}\n`, "utf8");

  const { stdout } = await execFileAsync(
    process.execPath,
    [options.workerScriptPath, configPath],
    {
      cwd: options.host.hostDir,
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
      timeout: 180_000,
    },
  );
  const lines = stdout
    .trim()
    .split("\n")
    .filter((line) => line.trim().length > 0);
  const lastLine = lines.at(-1);
  if (lastLine === undefined) {
    throw new Error(
      `Perf worker (${options.scenario}/${options.observeMode}) produced no JSON output.`,
    );
  }
  return JSON.parse(lastLine) as WorkerRunSample;
}

function readPnpmVersion(): string {
  return execFileSync("pnpm", ["--version"], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
}

/**
 * Executes the full A/B performance comparison in isolated hosts and independent
 * Node subprocesses, enforcing budget gates and collecting summary attributions on failure.
 */
export async function runPerfAbBenchmark(
  options: RunPerfAbOptions,
): Promise<PerfReport> {
  const totalStart = performance.now();
  const log =
    options.logProgress ?? ((msg: string) => process.stderr.write(`${msg}\n`));
  const repoRoot = options.repoRoot ?? resolveRepoRoot();
  const rounds = options.rounds ?? 7;
  const warmup = options.warmup ?? 1;
  const scenarios = options.scenarios ?? [
    "openProject",
    "coldQuery",
    "cachedQuery",
    "updateQuery",
  ];
  const budget = options.budget ?? DEFAULT_PERF_BUDGET;
  const workerScriptPath = resolveWorkerScriptPath();

  log(`[perf] Materializing ${options.profile} corpus...`);
  const corpus = await materializeCorpus({
    profile: options.profile,
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

  log("[perf] Preparing baseline (merge-base) and candidate artifacts...");
  const baselineArtifact = await prepareBaselineArtifact({
    repoRoot,
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
  const baselineBundle = await packWorkspaceArtifacts({
    packages: ["tssae"],
    fromPackDir: baselineArtifact.cacheDir,
  });
  const candidateBundle = await packWorkspaceArtifacts({
    packages: ["tssae"],
    cwd: repoRoot,
  });

  const candidateArtifact = candidateBundle.artifacts[0];
  if (
    candidateArtifact === undefined ||
    candidateArtifact.binarySha256 === undefined
  ) {
    candidateBundle.dispose();
    throw new Error("Candidate tssae artifact or engine binary is missing.");
  }

  const headState = readRepoHeadState(repoRoot);
  const configTempDir = mkdtempSync(join(tmpdir(), "tssae-perf-cfg-"));
  let hostA: PreparedAcceptanceHost | undefined;
  let hostB: PreparedAcceptanceHost | undefined;

  try {
    log(
      "[perf] Installing baseline and candidate packages into isolated hosts...",
    );
    hostA = await prepareAcceptanceHost({ bundle: baselineBundle });
    hostB = await prepareAcceptanceHost({ bundle: candidateBundle });

    const schedule = buildPairedSamplingSchedule(rounds);
    const comparisons: ScenarioComparison[] = [];
    let sampleSeq = 0;

    for (const scenario of scenarios) {
      log(
        `[perf] Running scenario "${scenario}" (${String(warmup)} warmup + ${String(rounds)} paired ABBA rounds)...`,
      );
      for (let w = 0; w < warmup; w += 1) {
        await runWorkerSubprocess({
          workerScriptPath,
          host: hostA,
          corpus,
          scenario,
          observeMode: "off",
          collectAttribution: false,
          configDir: configTempDir,
          label: `warmup-a-${String(sampleSeq++)}`,
        });
        await runWorkerSubprocess({
          workerScriptPath,
          host: hostB,
          corpus,
          scenario,
          observeMode: "off",
          collectAttribution: false,
          configDir: configTempDir,
          label: `warmup-b-${String(sampleSeq++)}`,
        });
      }

      const baselineSamples: WorkerRunSample[] = [];
      const candidateSamples: WorkerRunSample[] = [];
      for (const [firstSide, secondSide] of schedule) {
        for (const side of [firstSide, secondSide]) {
          const sample = await runWorkerSubprocess({
            workerScriptPath,
            host: side === "baseline" ? hostA : hostB,
            corpus,
            scenario,
            observeMode: "off",
            collectAttribution: false,
            configDir: configTempDir,
            label: `sample-${scenario}-${side}-${String(sampleSeq++)}`,
          });
          if (side === "baseline") {
            baselineSamples.push(sample);
          } else {
            candidateSamples.push(sample);
          }
        }
      }

      const baselineStats = summarizeScenarioSamples(baselineSamples);
      const candidateStats = summarizeScenarioSamples(candidateSamples);
      const decision = evaluateScenarioBudget({
        baselineP50Ms: baselineStats.p50Ms,
        candidateP50Ms: candidateStats.p50Ms,
        baselinePeakMemoryBytes: baselineStats.peakEngineRssBytes,
        candidatePeakMemoryBytes: candidateStats.peakEngineRssBytes,
        budget,
      });

      let attribution;
      if (!decision.passed) {
        log(
          `[perf] Budget exceeded on "${scenario}"; running observe="summary" attribution...`,
        );
        const baseAttrSample = await runWorkerSubprocess({
          workerScriptPath,
          host: hostA,
          corpus,
          scenario,
          observeMode: "summary",
          collectAttribution: true,
          configDir: configTempDir,
          label: `attr-a-${scenario}-${String(sampleSeq++)}`,
        });
        const candAttrSample = await runWorkerSubprocess({
          workerScriptPath,
          host: hostB,
          corpus,
          scenario,
          observeMode: "summary",
          collectAttribution: true,
          configDir: configTempDir,
          label: `attr-b-${scenario}-${String(sampleSeq++)}`,
        });
        attribution = buildAttributionReport(
          scenario,
          baseAttrSample,
          candAttrSample,
        );
      }

      comparisons.push({
        scenario,
        label: SCENARIO_LABELS[scenario],
        baseline: baselineStats,
        candidate: candidateStats,
        latencyDeltaRatio: decision.latencyDeltaRatio,
        memoryDeltaRatio: decision.memoryDeltaRatio,
        passed: decision.passed,
        violations: decision.violations,
        ...(attribution === undefined ? {} : { attribution }),
      });
    }

    const observeOverhead: ObserveOverheadEntry[] = [];
    if (options.includeObserveOverhead !== false) {
      log("[perf] Measuring observe overhead (off / summary / detail)...");
      const observeRounds = Math.min(rounds, 5);
      const modes: readonly ObserveModeName[] = ["off", "summary", "detail"];
      const byMode = new Map<ObserveModeName, WorkerRunSample[]>();

      for (const mode of modes) {
        if (warmup > 0) {
          await runWorkerSubprocess({
            workerScriptPath,
            host: hostB,
            corpus,
            scenario: "coldQuery",
            observeMode: mode,
            collectAttribution: false,
            configDir: configTempDir,
            label: `obs-warmup-${mode}-${String(sampleSeq++)}`,
          });
        }
        const samples: WorkerRunSample[] = [];
        for (let r = 0; r < observeRounds; r += 1) {
          samples.push(
            await runWorkerSubprocess({
              workerScriptPath,
              host: hostB,
              corpus,
              scenario: "coldQuery",
              observeMode: mode,
              collectAttribution: false,
              configDir: configTempDir,
              label: `obs-${mode}-${String(sampleSeq++)}`,
            }),
          );
        }
        byMode.set(mode, samples);
      }

      const offStats = summarizeScenarioSamples(byMode.get("off")!);
      for (const mode of modes) {
        const samples = byMode.get(mode)!;
        const stats = summarizeScenarioSamples(samples);
        const rpcBytesTotal = samples.reduce(
          (sum, sample) => sum + sample.rpcBytesSent + sample.rpcBytesReceived,
          0,
        );
        observeOverhead.push({
          mode,
          p50Ms: stats.p50Ms,
          p95Ms: stats.p95Ms,
          peakEngineRssBytes: stats.peakEngineRssBytes,
          peakTotalRssBytes: stats.peakTotalRssBytes,
          rpcBytesTotal: Math.round(rpcBytesTotal / samples.length),
          latencyOverheadMs: stats.p50Ms - offStats.p50Ms,
          latencyOverheadRatio: computeRelativeDelta(
            offStats.p50Ms,
            stats.p50Ms,
          ),
          memoryOverheadBytes:
            stats.peakEngineRssBytes - offStats.peakEngineRssBytes,
          samples,
        });
      }
    }

    const cpus = os.cpus();
    return {
      passed: comparisons.every((item) => item.passed),
      profile: options.profile,
      warmupRounds: warmup,
      samplingRounds: rounds,
      budget,
      corpus: {
        repoUrl: corpus.repoUrl,
        repoName: corpus.repoName,
        commitSha: corpus.commitSha,
        subsets: corpus.subsets,
        fileCount: corpus.fileCount,
        sourceOrigin: corpus.sourceOrigin,
        cacheBypassed: corpus.cacheBypassed,
      },
      environment: {
        timestampUtc: new Date().toISOString(),
        platform: process.platform,
        osRelease: os.release(),
        arch: process.arch,
        cpuModel: cpus[0]?.model ?? "unknown",
        cpuCores: cpus.length,
        totalMemoryBytes: os.totalmem(),
        nodeVersion: process.version,
        pnpmVersion: readPnpmVersion(),
        baselineRev: baselineArtifact.rev,
        baselineCommitSha: baselineArtifact.commitSha,
        baselineVersion: baselineArtifact.version,
        baselineTarballSha256: baselineArtifact.sha256,
        baselineBinarySha256: baselineArtifact.binarySha256,
        baselineCacheHit: baselineArtifact.cacheHit,
        candidateCommitSha: headState.commitSha,
        candidateDirty: headState.dirty,
        candidateVersion: candidateArtifact.version,
        candidateTarballSha256: candidateArtifact.sha256,
        candidateBinarySha256: candidateArtifact.binarySha256,
      },
      scenarios: comparisons,
      observeOverhead,
      totalDurationMs: performance.now() - totalStart,
    };
  } finally {
    await hostA?.dispose();
    await hostB?.dispose();
    baselineBundle.dispose();
    candidateBundle.dispose();
    rmSync(configTempDir, { recursive: true, force: true });
  }
}
