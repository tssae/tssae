import type { CorpusSourceOrigin } from "../corpus.ts";

export type CorpusProfileId = "medium" | "large";

export type CorpusProfileSpec = {
  readonly id: CorpusProfileId;
  readonly repoUrl: string;
  readonly repoName: string;
  readonly commitSha: string;
  readonly subsets: readonly string[];
  readonly includeGlobs: readonly string[];
  readonly primaryFile: string;
  readonly secondaryFile: string;
  readonly targetCalleeName: string;
};

export type MaterializedCorpus = {
  readonly profile: CorpusProfileId;
  readonly repoUrl: string;
  readonly repoName: string;
  readonly commitSha: string;
  readonly subsets: readonly string[];
  readonly corpusDir: string;
  readonly bareRepoDir: string;
  readonly tsconfigPath: string;
  readonly fileCount: number;
  readonly primaryFile: string;
  readonly secondaryFile: string;
  readonly targetCalleeName: string;
  readonly cacheHit: boolean;
  readonly cacheBypassed: boolean;
  readonly sourceOrigin: CorpusSourceOrigin;
};

export type PerfScenarioId =
  | "openProject"
  | "coldQuery"
  | "cachedQuery"
  | "updateQuery";

export type ObserveModeName = "off" | "summary" | "detail";

export type PerfBudgetConfig = {
  readonly maxQueryLatencyRegressionRatio: number;
  readonly maxPeakMemoryGrowthRatio: number;
};

export type EngineStepAttribution = {
  readonly op: string;
  readonly provider: string;
  readonly baselineDurationMs: number;
  readonly candidateDurationMs: number;
  readonly deltaMs: number;
  readonly baselineCount: number;
  readonly candidateCount: number;
};

export type QueryAttribution = {
  readonly op: string;
  readonly baselineDurationMs: number;
  readonly candidateDurationMs: number;
  readonly deltaMs: number;
  readonly baselineCount: number;
  readonly candidateCount: number;
};

export type AttributionReport = {
  readonly scenario: PerfScenarioId;
  readonly observeMode: "summary";
  readonly baselineRpcBytes: number;
  readonly candidateRpcBytes: number;
  readonly steps: readonly EngineStepAttribution[];
  readonly queries: readonly QueryAttribution[];
};

export type SampleEngineStepRecord = {
  readonly op: string;
  readonly provider: string;
  readonly cacheStatus: "computed" | "reused";
  readonly durationMs: number;
  readonly candidateCount: number;
};

export type SampleQueryRecord = {
  readonly op: string;
  readonly cacheStatus: "computed" | "reused";
  readonly durationMs: number;
  readonly candidateCount: number;
};

export type WorkerRunSample = {
  readonly wallMs: number;
  readonly peakHostRssBytes: number;
  readonly peakEngineRssBytes: number;
  readonly peakTotalRssBytes: number;
  readonly sessionHostRssBytes: number;
  readonly sessionEngineRssBytes?: number;
  readonly observeMode: ObserveModeName;
  readonly rpcBytesSent: number;
  readonly rpcBytesReceived: number;
  readonly steps: readonly SampleEngineStepRecord[];
  readonly queries: readonly SampleQueryRecord[];
};

export type ScenarioStats = {
  readonly p50Ms: number;
  readonly p95Ms: number;
  readonly peakHostRssBytes: number;
  readonly peakEngineRssBytes: number;
  readonly peakTotalRssBytes: number;
  readonly sessionHostRssBytes: number;
  readonly sessionEngineRssBytes?: number;
  readonly samples: readonly WorkerRunSample[];
};

export type ScenarioComparison = {
  readonly scenario: PerfScenarioId;
  readonly label: string;
  readonly baseline: ScenarioStats;
  readonly candidate: ScenarioStats;
  readonly latencyDeltaRatio: number;
  readonly memoryDeltaRatio: number;
  readonly passed: boolean;
  readonly violations: readonly string[];
  readonly attribution?: AttributionReport;
};

export type ObserveOverheadEntry = {
  readonly mode: ObserveModeName;
  readonly p50Ms: number;
  readonly p95Ms: number;
  readonly peakEngineRssBytes: number;
  readonly peakTotalRssBytes: number;
  readonly rpcBytesTotal: number;
  readonly latencyOverheadMs: number;
  readonly latencyOverheadRatio: number;
  readonly memoryOverheadBytes: number;
  readonly samples: readonly WorkerRunSample[];
};

export type PerfEnvironmentInfo = {
  readonly timestampUtc: string;
  readonly platform: NodeJS.Platform;
  readonly osRelease: string;
  readonly arch: string;
  readonly cpuModel: string;
  readonly cpuCores: number;
  readonly totalMemoryBytes: number;
  readonly nodeVersion: string;
  readonly pnpmVersion: string;
  readonly baselineRev: string;
  readonly baselineCommitSha: string;
  readonly baselineVersion: string;
  readonly baselineTarballSha256: string;
  readonly baselineBinarySha256: string;
  readonly baselineCacheHit: boolean;
  readonly candidateCommitSha: string;
  readonly candidateDirty: boolean;
  readonly candidateVersion: string;
  readonly candidateTarballSha256: string;
  readonly candidateBinarySha256: string;
};

export type PerfReport = {
  readonly passed: boolean;
  readonly profile: CorpusProfileId;
  readonly warmupRounds: number;
  readonly samplingRounds: number;
  readonly budget: PerfBudgetConfig;
  readonly corpus: {
    readonly repoUrl: string;
    readonly repoName: string;
    readonly commitSha: string;
    readonly subsets: readonly string[];
    readonly fileCount: number;
    readonly sourceOrigin: "cache" | "host-mirror" | "remote";
    readonly cacheBypassed: boolean;
  };
  readonly environment: PerfEnvironmentInfo;
  readonly scenarios: readonly ScenarioComparison[];
  readonly observeOverhead: readonly ObserveOverheadEntry[];
  readonly totalDurationMs: number;
};
