import assert from "node:assert/strict";
import { dirname, resolve } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { runSoundnessReconciliationSuite } from "@tssae/acceptance";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURES_DIR = resolve(HERE, "../../fixtures/soundness");

void describe("zero-rewrite runtime soundness reconciliation suite", () => {
  void it("reconciles branchCoverage, valuePropagation, calleesOf, callEffects, and instanceCleanup in one project snapshot", async () => {
    const result = await runSoundnessReconciliationSuite({
      projectDir: FIXTURES_DIR,
      tsconfig: "tsconfig.json",
    });

    assert.equal(
      result.report.passed,
      true,
      `Soundness violations found:\n${result.report.violations
        .map(
          (v) => `- [${v.query}] ${v.id} (${v.location ?? "?"}): ${v.message}`,
        )
        .join("\n")}`,
    );
    assert.ok(
      result.report.checkedCount >= 14,
      `Expected at least 14 checked cases across all 5 queries, got ${result.report.checkedCount}`,
    );
    assert.equal(result.snapshotCount, 1);
    assert.ok(result.queriesEvaluated.branchCoverage >= 4);
    assert.ok(result.queriesEvaluated.valuePropagation >= 5);
    assert.ok(result.queriesEvaluated.calleesOf >= 3);
    assert.ok(result.queriesEvaluated.callEffects >= 3);
    assert.ok(result.queriesEvaluated.instanceCleanup >= 4);

    for (const b of result.input.branchCoverage ?? []) {
      assert.ok(
        b.targetExecutedCount > 0,
        `Expected target to execute at runtime for ${b.id}`,
      );
      assert.ok(
        b.selectedBranchExecutedCounts.every((c) => c === 0),
        `Expected guard branch to be bypassed at runtime for ${b.id}`,
      );
    }
    for (const v of result.input.valuePropagation ?? []) {
      assert.equal(
        v.runtimeArrived,
        true,
        `Expected runtime value arrival for ${v.id}`,
      );
    }
    for (const c of result.input.calleesOf ?? []) {
      assert.ok(
        c.runtimeCalledTargets.length > 0,
        `Expected runtime callee invocation for ${c.id}`,
      );
    }
    for (const e of result.input.callEffects ?? []) {
      assert.ok(
        e.runtimeWrittenProperties.length > 0,
        `Expected runtime property write for ${e.id}`,
      );
    }
    const uncleanedIds = new Set([
      "cleanup-unprotected-throw",
      "cleanup-uncleaned-branch",
    ]);
    for (const ic of result.input.instanceCleanup ?? []) {
      assert.equal(
        ic.runtimeCleanedUp,
        !uncleanedIds.has(ic.id),
        `Unexpected runtime cleanup state for ${ic.id}`,
      );
    }
  });
});
