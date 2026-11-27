export {
  CORE_TSSAE_PACKAGES,
  packWorkspaceArtifacts,
  type PackedArtifact,
  type PackedArtifactBundle,
  type PackWorkspaceArtifactsOptions,
} from "./artifacts.ts";
export {
  DEFAULT_DIRECT_TSSAE_PACKAGES,
  installArtifactPackages,
  type CommandOutcome,
  type InstallArtifactPackagesOptions,
  type InstallArtifactPackagesResult,
} from "./host-install.ts";
export {
  prepareAcceptanceHost,
  withAcceptanceHost,
  type HostRunOptions,
  type PrepareAcceptanceHostOptions,
  type PreparedAcceptanceHost,
} from "./host-lifecycle.ts";
export {
  resolveDefaultBlockedPaths,
  runInIsolatedHost,
  type IsolatedRunResult,
  type IsolationEvidence,
  type IsolationMode,
  type RunInIsolatedHostOptions,
} from "./isolation.ts";
export {
  extractPackageScope,
  startArtifactRegistry,
  type ArtifactRegistryServer,
  type StartArtifactRegistryOptions,
} from "./registry.ts";
export type { PackageManager } from "./types.ts";
export {
  verifyHostArtifactResolution,
  type HostResolutionAudit,
  type VerifyHostResolutionOptions,
} from "./verify.ts";
export {
  prepareBaselineArtifact,
  readRepoHeadState,
  resolveBaselineRevision,
  resolvePerfCacheDir,
  resolveRepoRoot,
  type ResolvedRevision,
} from "./perf/baseline.ts";
export { runPerfCli } from "./perf/cli.ts";
export {
  DEFAULT_SCENARIOS,
  formatPerfCliUsage,
  parsePerfCliArgs,
  type PerfCliOptions,
} from "./perf/cli-args.ts";
export {
  CORPUS_PROFILES,
  materializeCorpus,
  resolveCorpusCacheDir,
  type MaterializeCorpusOptions,
} from "./perf/corpus.ts";
export {
  SCENARIO_LABELS,
  formatPerfMarkdownReport,
  formatPerfTableReport,
  writePerfReportFiles,
} from "./perf/report.ts";
export {
  buildPairedSamplingSchedule,
  runPerfAbBenchmark,
  type RunPerfAbOptions,
  type VersionSide,
} from "./perf/runner.ts";
export {
  DEFAULT_PERF_BUDGET,
  buildAttributionReport,
  computeP50,
  computeP95,
  computeRelativeDelta,
  evaluateScenarioBudget,
  summarizeScenarioSamples,
  type BudgetEvaluationInput,
  type BudgetEvaluationResult,
} from "./perf/stats.ts";
export type {
  AttributionReport,
  BaselineArtifact,
  CorpusProfileId,
  CorpusProfileSpec,
  EngineStepAttribution,
  MaterializedCorpus,
  ObserveModeName,
  ObserveOverheadEntry,
  PerfBudgetConfig,
  PerfEnvironmentInfo,
  PerfReport,
  PerfScenarioId,
  QueryAttribution,
  SampleEngineStepRecord,
  SampleQueryRecord,
  ScenarioComparison,
  ScenarioStats,
  WorkerRunSample,
} from "./perf/types.ts";
export {
  checkSoundness,
  collectPreciseCoverage,
  runSoundnessReconciliationSuite,
  type BranchCoverageSoundnessCase,
  type CallEffectsSoundnessCase,
  type CalleesOfSoundnessCase,
  type ExecutionCountOptions,
  type InstanceCleanupSoundnessCase,
  type PreciseCoverageSnapshot,
  type SoundnessCheckInput,
  type SoundnessCheckReport,
  type SoundnessCompleteness,
  type SoundnessQueryKind,
  type SoundnessReconciliationOptions,
  type SoundnessReconciliationResult,
  type SoundnessTriState,
  type SoundnessViolation,
  type ValuePropagationSoundnessCase,
} from "./soundness/index.ts";
