// Multi-byte emoji 🚀🎉 verifies exact alignment between UTF-8 byte offsets and UTF-16 code-unit offsets.
type EmojiMarker = "🚀🎉";

const marker: EmojiMarker = "🚀🎉";

function guardStep(): number {
  return 1;
}

function targetWork(label: string): number {
  void label;
  return marker.length;
}

function mightThrow(shouldThrow: boolean): void {
  if (shouldThrow) {
    throw new Error("simulated failure");
  }
}

export function normalIfBypass(flag: boolean): number {
  if (flag) {
    guardStep();
  }
  return targetWork("normal-if-bypass");
}

export function shortCircuitAndBypass(flag: boolean): number {
  const guarded = flag && guardStep();
  void guarded;
  return targetWork("short-circuit-and");
}

export function optionalChainBypass(helper?: { check: () => number }): number {
  const checked = helper?.check();
  void checked;
  return targetWork("optional-chain");
}

export function caughtExceptionBypass(
  shouldThrow: boolean,
  flag: boolean,
): number {
  try {
    mightThrow(shouldThrow);
    if (flag) {
      guardStep();
    } else {
      return 0;
    }
  } catch {
    // Intentionally swallowed to exercise exception jump past guard.
  }
  return targetWork("caught-exception");
}

export function runControlFlowScenarios(): void {
  normalIfBypass(false);
  shortCircuitAndBypass(false);
  optionalChainBypass(undefined);
  caughtExceptionBypass(true, true);
}
