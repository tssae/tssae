import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { checkSoundness } from "@tssae/acceptance";

void describe("checkSoundness unilateral soundness oracle", () => {
  void it("passes when runtime observations match proved/disproved/complete static verdicts", () => {
    const report = checkSoundness({
      branchCoverage: [
        {
          id: "branch-consistent-disproved",
          targetExecutedCount: 1,
          selectedBranchExecutedCounts: [0],
          reachedViaCaughtException: false,
          staticStatus: "disproved",
          implicitExceptionsStatus: "proved",
        },
        {
          id: "branch-consistent-caught-exception",
          targetExecutedCount: 1,
          selectedBranchExecutedCounts: [0],
          reachedViaCaughtException: true,
          staticStatus: "proved",
          implicitExceptionsStatus: "disproved",
        },
        {
          id: "branch-consistent-guarded-execution",
          targetExecutedCount: 1,
          selectedBranchExecutedCounts: [1],
          reachedViaCaughtException: false,
          staticStatus: "proved",
          implicitExceptionsStatus: "proved",
        },
      ],
      valuePropagation: [
        {
          id: "val-consistent-proved",
          runtimeArrived: true,
          staticStatus: "proved",
        },
        {
          id: "val-consistent-unobserved-disproved",
          runtimeArrived: false,
          staticStatus: "disproved",
        },
      ],
      calleesOf: [
        {
          id: "callees-consistent-complete",
          runtimeCalledTargets: ["fnA", "fnB"],
          staticCompleteness: "complete",
          staticTargets: ["fnB", "fnA", "fnC"],
        },
      ],
      callEffects: [
        {
          id: "effects-consistent-complete",
          runtimeWrittenProperties: ["flags", "count"],
          staticCompleteness: "complete",
          staticFieldWrites: ["count", "flags"],
        },
      ],
      instanceCleanup: [
        {
          id: "cleanup-consistent-disproved",
          runtimeCleanedUp: false,
          staticStatus: "disproved",
          implicitExceptionsStatus: "disproved",
        },
        {
          id: "cleanup-consistent-cleaned-proved",
          runtimeCleanedUp: true,
          staticStatus: "proved",
          implicitExceptionsStatus: "proved",
        },
      ],
    });

    assert.equal(report.passed, true);
    assert.equal(report.checkedCount, 9);
    assert.deepEqual(report.violations, []);
  });

  void it("treats inconclusive and incomplete static declarations as honest and passes", () => {
    const report = checkSoundness({
      branchCoverage: [
        {
          id: "branch-honest-normal-inconclusive",
          targetExecutedCount: 1,
          selectedBranchExecutedCounts: [0],
          reachedViaCaughtException: false,
          staticStatus: "inconclusive",
          implicitExceptionsStatus: "proved",
        },
        {
          id: "branch-honest-exception-inconclusive",
          targetExecutedCount: 1,
          selectedBranchExecutedCounts: [0],
          reachedViaCaughtException: true,
          staticStatus: "proved",
          implicitExceptionsStatus: "inconclusive",
        },
      ],
      valuePropagation: [
        {
          id: "val-honest-inconclusive",
          runtimeArrived: true,
          staticStatus: "inconclusive",
        },
      ],
      calleesOf: [
        {
          id: "callees-honest-incomplete",
          runtimeCalledTargets: ["dynamicTarget"],
          staticCompleteness: "incomplete",
          staticTargets: [],
        },
      ],
      callEffects: [
        {
          id: "effects-honest-incomplete",
          runtimeWrittenProperties: ["dynamicProp"],
          staticCompleteness: "incomplete",
          staticFieldWrites: [],
        },
      ],
      instanceCleanup: [
        {
          id: "cleanup-honest-inconclusive",
          runtimeCleanedUp: false,
          staticStatus: "inconclusive",
          implicitExceptionsStatus: "proved",
        },
        {
          id: "cleanup-honest-exception-inconclusive",
          runtimeCleanedUp: false,
          reachedViaCaughtException: true,
          staticStatus: "proved",
          implicitExceptionsStatus: "inconclusive",
        },
      ],
    });

    assert.equal(report.passed, true);
    assert.equal(report.checkedCount, 7);
    assert.deepEqual(report.violations, []);
  });

  void it("flags branchCoverage contradiction when normal path bypasses guard and static status is proved", () => {
    const report = checkSoundness({
      branchCoverage: [
        {
          id: "branch-normal-false-proved",
          location: "sample.ts:10:3",
          targetExecutedCount: 1,
          selectedBranchExecutedCounts: [0],
          reachedViaCaughtException: false,
          staticStatus: "proved",
          implicitExceptionsStatus: "disproved",
        },
      ],
    });

    assert.equal(report.passed, false);
    assert.equal(report.violations.length, 1);
    assert.equal(report.violations[0]!.query, "branchCoverage");
    assert.equal(report.violations[0]!.id, "branch-normal-false-proved");
    assert.equal(report.violations[0]!.location, "sample.ts:10:3");
  });

  void it("flags branchCoverage contradiction when caught exception bypasses guard and implicitExceptions is proved", () => {
    const report = checkSoundness({
      branchCoverage: [
        {
          id: "branch-exception-false-proved",
          location: "sample.ts:22:3",
          targetExecutedCount: 1,
          selectedBranchExecutedCounts: [0, 0],
          reachedViaCaughtException: true,
          staticStatus: "proved",
          implicitExceptionsStatus: "proved",
        },
      ],
    });

    assert.equal(report.passed, false);
    assert.equal(report.violations.length, 1);
    assert.equal(report.violations[0]!.query, "branchCoverage");
    assert.equal(report.violations[0]!.id, "branch-exception-false-proved");
  });

  void it("flags valuePropagation contradiction when value arrives at runtime but static status is disproved", () => {
    const report = checkSoundness({
      valuePropagation: [
        {
          id: "val-false-disproved",
          location: "sample.ts:30:5",
          runtimeArrived: true,
          staticStatus: "disproved",
        },
      ],
    });

    assert.equal(report.passed, false);
    assert.equal(report.violations.length, 1);
    assert.equal(report.violations[0]!.query, "valuePropagation");
    assert.equal(report.violations[0]!.id, "val-false-disproved");
  });

  void it("flags calleesOf contradiction when runtime calls an omitted target under completeness complete", () => {
    const report = checkSoundness({
      calleesOf: [
        {
          id: "callees-missing-target",
          location: "sample.ts:40:3",
          runtimeCalledTargets: ["Derived.run"],
          staticCompleteness: "complete",
          staticTargets: ["Base.run"],
        },
      ],
    });

    assert.equal(report.passed, false);
    assert.equal(report.violations.length, 1);
    assert.equal(report.violations[0]!.query, "calleesOf");
    assert.equal(report.violations[0]!.id, "callees-missing-target");
    assert.match(report.violations[0]!.message, /Derived\.run/);
  });

  void it("flags callEffects contradiction when runtime writes an omitted property under completeness complete", () => {
    const report = checkSoundness({
      callEffects: [
        {
          id: "effects-missing-write",
          location: "sample.ts:50:3",
          runtimeWrittenProperties: ["flags"],
          staticCompleteness: "complete",
          staticFieldWrites: ["otherProp"],
        },
      ],
    });

    assert.equal(report.passed, false);
    assert.equal(report.violations.length, 1);
    assert.equal(report.violations[0]!.query, "callEffects");
    assert.equal(report.violations[0]!.id, "effects-missing-write");
    assert.match(report.violations[0]!.message, /flags/);
  });

  void it("flags instanceCleanup contradictions on normal and caught-exception uncleaned exits", () => {
    const report = checkSoundness({
      instanceCleanup: [
        {
          id: "cleanup-normal-false-proved",
          location: "sample.ts:60:3",
          runtimeCleanedUp: false,
          staticStatus: "proved",
          implicitExceptionsStatus: "disproved",
        },
        {
          id: "cleanup-exception-false-proved",
          location: "sample.ts:70:3",
          runtimeCleanedUp: false,
          reachedViaCaughtException: true,
          staticStatus: "proved",
          implicitExceptionsStatus: "proved",
        },
      ],
    });

    assert.equal(report.passed, false);
    assert.equal(report.violations.length, 2);
    assert.equal(report.violations[0]!.query, "instanceCleanup");
    assert.equal(report.violations[0]!.id, "cleanup-normal-false-proved");
    assert.equal(report.violations[1]!.query, "instanceCleanup");
    assert.equal(report.violations[1]!.id, "cleanup-exception-false-proved");
  });
});
