import type {
  AttributionReport,
  EngineStepAttribution,
  PerfBudgetConfig,
  PerfScenarioId,
  QueryAttribution,
  SampleEngineStepRecord,
  SampleQueryRecord,
  ScenarioStats,
  WorkerRunSample,
} from "./types.ts";

/** Default performance gate budget: <= 5% query p50 regression and <= 10% peak memory growth. */
export const DEFAULT_PERF_BUDGET: PerfBudgetConfig = {
  maxQueryLatencyRegressionRatio: 0.05,
  maxPeakMemoryGrowthRatio: 0.1,
};

export type BudgetEvaluationInput = {
  readonly baselineP50Ms: number;
  readonly candidateP50Ms: number;
  readonly baselinePeakMemoryBytes: number;
  readonly candidatePeakMemoryBytes: number;
  readonly budget?: PerfBudgetConfig;
};

export type BudgetEvaluationResult = {
  readonly latencyDeltaRatio: number;
  readonly memoryDeltaRatio: number;
  readonly passed: boolean;
  readonly violations: readonly string[];
};

/** Computes the 50th percentile (median) of non-empty numeric samples. */
export function computeP50(values: readonly number[]): number {
  if (values.length === 0) {
    throw new Error("Cannot compute p50 of an empty sample list.");
  }
  const sorted = [...values].toSorted((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  if (sorted.length % 2 === 1) {
    return sorted[mid]!;
  }
  return (sorted[mid - 1]! + sorted[mid]!) / 2;
}

/** Computes the 95th percentile using nearest-rank indexing on non-empty numeric samples. */
export function computeP95(values: readonly number[]): number {
  if (values.length === 0) {
    throw new Error("Cannot compute p95 of an empty sample list.");
  }
  const sorted = [...values].toSorted((a, b) => a - b);
  const index = Math.min(
    sorted.length - 1,
    Math.ceil(sorted.length * 0.95) - 1,
  );
  return sorted[index]!;
}

/** Computes relative change `(candidate - baseline) / baseline`. */
export function computeRelativeDelta(
  baseline: number,
  candidate: number,
): number {
  if (baseline === 0) {
    return candidate === 0 ? 0 : Number.POSITIVE_INFINITY;
  }
  return (candidate - baseline) / baseline;
}

function formatPercent(ratio: number): string {
  if (!Number.isFinite(ratio)) {
    return "+inf%";
  }
  const pct = ratio * 100;
  return `${pct >= 0 ? "+" : ""}${pct.toFixed(2)}%`;
}

/** Evaluates a scenario against latency and peak memory budgets. */
export function evaluateScenarioBudget(
  input: BudgetEvaluationInput,
): BudgetEvaluationResult {
  const budget = input.budget ?? DEFAULT_PERF_BUDGET;
  const latencyDeltaRatio = computeRelativeDelta(
    input.baselineP50Ms,
    input.candidateP50Ms,
  );
  const memoryDeltaRatio = computeRelativeDelta(
    input.baselinePeakMemoryBytes,
    input.candidatePeakMemoryBytes,
  );
  const violations: string[] = [];

  if (latencyDeltaRatio > budget.maxQueryLatencyRegressionRatio) {
    violations.push(
      `p50 latency regressed by ${formatPercent(latencyDeltaRatio)} (budget <= ${formatPercent(budget.maxQueryLatencyRegressionRatio)})`,
    );
  }
  if (memoryDeltaRatio > budget.maxPeakMemoryGrowthRatio) {
    violations.push(
      `peak memory grew by ${formatPercent(memoryDeltaRatio)} (budget <= ${formatPercent(budget.maxPeakMemoryGrowthRatio)})`,
    );
  }

  return {
    latencyDeltaRatio,
    memoryDeltaRatio,
    passed: violations.length === 0,
    violations,
  };
}

/** Aggregates per-run subprocess samples into p50/p95 latency and median peak memory stats. */
export function summarizeScenarioSamples(
  samples: readonly WorkerRunSample[],
): ScenarioStats {
  const walls = samples.map((sample) => sample.wallMs);
  const peakHost = samples.map((sample) => sample.peakHostRssBytes);
  const peakEngine = samples.map((sample) => sample.peakEngineRssBytes);
  const peakTotal = samples.map((sample) => sample.peakTotalRssBytes);
  const sessionHost = samples.map((sample) => sample.sessionHostRssBytes);
  const sessionEngine = samples
    .map((sample) => sample.sessionEngineRssBytes)
    .filter((value): value is number => value !== undefined);

  return {
    p50Ms: computeP50(walls),
    p95Ms: computeP95(walls),
    peakHostRssBytes: computeP50(peakHost),
    peakEngineRssBytes: computeP50(peakEngine),
    peakTotalRssBytes: computeP50(peakTotal),
    sessionHostRssBytes: computeP50(sessionHost),
    ...(sessionEngine.length > 0
      ? { sessionEngineRssBytes: computeP50(sessionEngine) }
      : {}),
    samples,
  };
}

function aggregateStepMetrics(
  steps: readonly SampleEngineStepRecord[],
): ReadonlyMap<
  string,
  { durationMs: number; count: number; op: string; provider: string }
> {
  const map = new Map<
    string,
    { durationMs: number; count: number; op: string; provider: string }
  >();
  for (const step of steps) {
    const key = `${step.op}\0${step.provider}`;
    const existing = map.get(key);
    if (existing === undefined) {
      map.set(key, {
        op: step.op,
        provider: step.provider,
        durationMs: step.durationMs,
        count: 1,
      });
    } else {
      existing.durationMs += step.durationMs;
      existing.count += 1;
    }
  }
  return map;
}

function aggregateQueryMetrics(
  queries: readonly SampleQueryRecord[],
): ReadonlyMap<string, { durationMs: number; count: number }> {
  const map = new Map<string, { durationMs: number; count: number }>();
  for (const query of queries) {
    const existing = map.get(query.op);
    if (existing === undefined) {
      map.set(query.op, { durationMs: query.durationMs, count: 1 });
    } else {
      existing.durationMs += query.durationMs;
      existing.count += 1;
    }
  }
  return map;
}

/**
 * Compares `observe: "summary"` samples between baseline and candidate and ranks
 * engine steps and public queries by latency regression delta.
 */
export function buildAttributionReport(
  scenario: PerfScenarioId,
  baselineSample: WorkerRunSample,
  candidateSample: WorkerRunSample,
): AttributionReport {
  const baseSteps = aggregateStepMetrics(baselineSample.steps);
  const candSteps = aggregateStepMetrics(candidateSample.steps);
  const allStepKeys = new Set([...baseSteps.keys(), ...candSteps.keys()]);
  const steps: EngineStepAttribution[] = [];
  for (const key of allStepKeys) {
    const base = baseSteps.get(key);
    const cand = candSteps.get(key);
    const [op, provider] = key.split("\0") as [string, string];
    const baselineDurationMs = base?.durationMs ?? 0;
    const candidateDurationMs = cand?.durationMs ?? 0;
    steps.push({
      op,
      provider,
      baselineDurationMs,
      candidateDurationMs,
      deltaMs: candidateDurationMs - baselineDurationMs,
      baselineCount: base?.count ?? 0,
      candidateCount: cand?.count ?? 0,
    });
  }
  steps.sort((a, b) => b.deltaMs - a.deltaMs);

  const baseQueries = aggregateQueryMetrics(baselineSample.queries);
  const candQueries = aggregateQueryMetrics(candidateSample.queries);
  const allQueryOps = new Set([...baseQueries.keys(), ...candQueries.keys()]);
  const queries: QueryAttribution[] = [];
  for (const op of allQueryOps) {
    const base = baseQueries.get(op);
    const cand = candQueries.get(op);
    const baselineDurationMs = base?.durationMs ?? 0;
    const candidateDurationMs = cand?.durationMs ?? 0;
    queries.push({
      op,
      baselineDurationMs,
      candidateDurationMs,
      deltaMs: candidateDurationMs - baselineDurationMs,
      baselineCount: base?.count ?? 0,
      candidateCount: cand?.count ?? 0,
    });
  }
  queries.sort((a, b) => b.deltaMs - a.deltaMs);

  return {
    scenario,
    observeMode: "summary",
    baselineRpcBytes:
      baselineSample.rpcBytesSent + baselineSample.rpcBytesReceived,
    candidateRpcBytes:
      candidateSample.rpcBytesSent + candidateSample.rpcBytesReceived,
    steps,
    queries,
  };
}
