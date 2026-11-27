export type SoundnessTriState = "proved" | "disproved" | "inconclusive";
export type SoundnessCompleteness = "complete" | "incomplete";

export type SoundnessQueryKind =
  | "branchCoverage"
  | "valuePropagation"
  | "calleesOf"
  | "callEffects"
  | "instanceCleanup";

export type BranchCoverageSoundnessCase = {
  readonly id: string;
  readonly location?: string | undefined;
  readonly targetExecutedCount: number;
  readonly selectedBranchExecutedCounts: readonly number[];
  readonly reachedViaCaughtException: boolean;
  readonly staticStatus: SoundnessTriState;
  readonly implicitExceptionsStatus: SoundnessTriState;
};

export type ValuePropagationSoundnessCase = {
  readonly id: string;
  readonly location?: string | undefined;
  readonly runtimeArrived: boolean;
  readonly staticStatus: SoundnessTriState;
};

export type CalleesOfSoundnessCase = {
  readonly id: string;
  readonly location?: string | undefined;
  readonly runtimeCalledTargets: readonly string[];
  readonly staticCompleteness: SoundnessCompleteness;
  readonly staticTargets: readonly string[];
};

export type CallEffectsSoundnessCase = {
  readonly id: string;
  readonly location?: string | undefined;
  readonly runtimeWrittenProperties: readonly string[];
  readonly staticCompleteness: SoundnessCompleteness;
  readonly staticFieldWrites: readonly string[];
};

export type InstanceCleanupSoundnessCase = {
  readonly id: string;
  readonly location?: string | undefined;
  readonly runtimeCleanedUp: boolean;
  readonly reachedViaCaughtException?: boolean | undefined;
  readonly staticStatus: SoundnessTriState;
  readonly implicitExceptionsStatus?: SoundnessTriState | undefined;
};

export type SoundnessCheckInput = {
  readonly branchCoverage?: readonly BranchCoverageSoundnessCase[] | undefined;
  readonly valuePropagation?:
    | readonly ValuePropagationSoundnessCase[]
    | undefined;
  readonly calleesOf?: readonly CalleesOfSoundnessCase[] | undefined;
  readonly callEffects?: readonly CallEffectsSoundnessCase[] | undefined;
  readonly instanceCleanup?:
    | readonly InstanceCleanupSoundnessCase[]
    | undefined;
};

export type SoundnessViolation = {
  readonly query: SoundnessQueryKind;
  readonly id: string;
  readonly location?: string | undefined;
  readonly message: string;
};

export type SoundnessCheckReport = {
  readonly passed: boolean;
  readonly checkedCount: number;
  readonly violations: readonly SoundnessViolation[];
};

function makeViolation(
  query: SoundnessQueryKind,
  item: { readonly id: string; readonly location?: string | undefined },
  message: string,
): SoundnessViolation {
  return {
    query,
    id: item.id,
    ...(item.location !== undefined ? { location: item.location } : {}),
    message,
  };
}

/**
 * Verifies the unilateral soundness invariants between runtime observations and
 * static `tssae` query verdicts. Honest `inconclusive` or `incomplete` results
 * never violate soundness; only definite static verdicts contradicting actual
 * runtime execution are flagged.
 */
export function checkSoundness(
  input: SoundnessCheckInput,
): SoundnessCheckReport {
  const violations: SoundnessViolation[] = [];
  let checkedCount = 0;

  for (const item of input.branchCoverage ?? []) {
    checkedCount += 1;
    const bypassedAllGuards =
      item.targetExecutedCount > 0 &&
      item.selectedBranchExecutedCounts.length > 0 &&
      item.selectedBranchExecutedCounts.every((count) => count === 0);

    if (!bypassedAllGuards) {
      continue;
    }

    const relevantStatus = item.reachedViaCaughtException
      ? item.implicitExceptionsStatus
      : item.staticStatus;

    if (relevantStatus === "proved") {
      const channel = item.reachedViaCaughtException
        ? "implicitExceptions.status"
        : "status";
      violations.push(
        makeViolation(
          "branchCoverage",
          item,
          `Runtime executed target (${item.targetExecutedCount}x) without executing any selected guard branch, yet static ${channel} returned "proved".`,
        ),
      );
    }
  }

  for (const item of input.valuePropagation ?? []) {
    checkedCount += 1;
    if (item.runtimeArrived && item.staticStatus === "disproved") {
      violations.push(
        makeViolation(
          "valuePropagation",
          item,
          'Runtime observed exact source value reaching sink, yet static valuePropagation returned "disproved".',
        ),
      );
    }
  }

  for (const item of input.calleesOf ?? []) {
    checkedCount += 1;
    if (item.staticCompleteness !== "complete") {
      continue;
    }
    const staticSet = new Set(item.staticTargets);
    for (const target of item.runtimeCalledTargets) {
      if (!staticSet.has(target)) {
        violations.push(
          makeViolation(
            "calleesOf",
            item,
            `Runtime invoked callee "${target}", but static calleesOf claimed completeness="complete" without including it.`,
          ),
        );
      }
    }
  }

  for (const item of input.callEffects ?? []) {
    checkedCount += 1;
    if (item.staticCompleteness !== "complete") {
      continue;
    }
    const staticSet = new Set(item.staticFieldWrites);
    for (const prop of item.runtimeWrittenProperties) {
      if (!staticSet.has(prop)) {
        violations.push(
          makeViolation(
            "callEffects",
            item,
            `Runtime wrote property "${prop}", but static callEffects claimed completeness="complete" without including it in fieldWrites.`,
          ),
        );
      }
    }
  }

  for (const item of input.instanceCleanup ?? []) {
    checkedCount += 1;
    if (item.runtimeCleanedUp) {
      continue;
    }
    const viaException = item.reachedViaCaughtException === true;
    const relevantStatus = viaException
      ? (item.implicitExceptionsStatus ?? "inconclusive")
      : item.staticStatus;

    if (relevantStatus === "proved") {
      const channel = viaException ? "implicitExceptions.status" : "status";
      violations.push(
        makeViolation(
          "instanceCleanup",
          item,
          `Runtime exited scope without cleaning up tracked instance, yet static instanceCleanup ${channel} returned "proved".`,
        ),
      );
    }
  }

  return {
    passed: violations.length === 0,
    checkedCount,
    violations,
  };
}
