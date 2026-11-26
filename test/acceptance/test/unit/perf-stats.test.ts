import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  computeP50,
  computeP95,
  DEFAULT_PERF_BUDGET,
  evaluateScenarioBudget,
} from "@tssae/acceptance";

void describe("perf statistics and budget gate", () => {
  void it("computes p50 and p95 percentiles accurately", () => {
    assert.equal(computeP50([10]), 10);
    assert.equal(computeP50([30, 10, 20]), 20);
    assert.equal(computeP50([40, 10, 30, 20]), 25);

    const twentySamples = Array.from({ length: 20 }, (_, i) => (20 - i) * 10);
    assert.equal(computeP95(twentySamples), 190);
    assert.equal(computeP95([100, 200, 150]), 200);
    assert.equal(computeP95([10, 20, 30, 40, 50, 60, 70]), 70);
  });

  void it("passes when latency and peak memory stay within 5% and 10% budgets", () => {
    const decision = evaluateScenarioBudget({
      baselineP50Ms: 100,
      candidateP50Ms: 105,
      baselinePeakMemoryBytes: 1_000_000,
      candidatePeakMemoryBytes: 1_100_000,
      budget: DEFAULT_PERF_BUDGET,
    });

    assert.equal(decision.passed, true);
    assert.deepEqual(decision.violations, []);
    assert.ok(Math.abs(decision.latencyDeltaRatio - 0.05) < 1e-9);
    assert.ok(Math.abs(decision.memoryDeltaRatio - 0.1) < 1e-9);
  });

  void it("fails when query p50 regresses by more than 5% or peak memory grows by more than 10%", () => {
    const latencyFail = evaluateScenarioBudget({
      baselineP50Ms: 100,
      candidateP50Ms: 105.1,
      baselinePeakMemoryBytes: 1_000_000,
      candidatePeakMemoryBytes: 1_000_000,
      budget: DEFAULT_PERF_BUDGET,
    });
    assert.equal(latencyFail.passed, false);
    assert.equal(latencyFail.violations.length, 1);
    assert.match(latencyFail.violations[0]!, /p50 latency/iu);

    const memoryFail = evaluateScenarioBudget({
      baselineP50Ms: 100,
      candidateP50Ms: 100,
      baselinePeakMemoryBytes: 1_000_000,
      candidatePeakMemoryBytes: 1_100_001,
      budget: DEFAULT_PERF_BUDGET,
    });
    assert.equal(memoryFail.passed, false);
    assert.equal(memoryFail.violations.length, 1);
    assert.match(memoryFail.violations[0]!, /peak memory/iu);

    const bothFail = evaluateScenarioBudget({
      baselineP50Ms: 100,
      candidateP50Ms: 120,
      baselinePeakMemoryBytes: 1_000_000,
      candidatePeakMemoryBytes: 1_200_000,
      budget: DEFAULT_PERF_BUDGET,
    });
    assert.equal(bothFail.passed, false);
    assert.equal(bothFail.violations.length, 2);
  });
});
