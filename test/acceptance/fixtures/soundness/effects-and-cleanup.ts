import { TrackedHandle, watchWrites } from "./probe.ts";

function markQueued(job: { flags: number }): void {
  job.flags = 1;
}

export function runDirectFieldWrite(): void {
  const job = watchWrites("effect-direct-write", { flags: 0 });
  markQueued(job);
}

export class BaseWriter {
  apply(job: { flags: number; mode: string }): void {
    job.flags = 0;
  }
}

export class CustomWriter extends BaseWriter {
  override apply(job: { flags: number; mode: string }): void {
    job.flags = 2;
    job.mode = "custom";
  }
}

export function invokeWriter(
  writer: BaseWriter,
  job: { flags: number; mode: string },
): void {
  writer.apply(job);
}

export class FlaggedRecord {
  backing = 0;

  set level(next: number) {
    this.backing = next;
  }
}

function assignLevel(rec: FlaggedRecord): void {
  rec.level = 7;
}

export function runSetterWrite(): void {
  const rec = watchWrites("effect-setter-write", new FlaggedRecord());
  assignLevel(rec);
}

function mightFail(shouldThrow: boolean): void {
  if (shouldThrow) {
    throw new Error("cleanup test throw");
  }
}

export function cleanupWithUsing(shouldThrow: boolean): void {
  using resource = new TrackedHandle("cleanup-using-throw");
  void resource;
  mightFail(shouldThrow);
}

export function cleanupWithTryFinally(shouldThrow: boolean): void {
  const resource = new TrackedHandle("cleanup-try-finally-throw");
  try {
    mightFail(shouldThrow);
  } finally {
    resource.dispose();
  }
}

export function cleanupUnprotectedThrow(shouldThrow: boolean): void {
  const resource = new TrackedHandle("cleanup-unprotected-throw");
  mightFail(shouldThrow);
  resource.dispose();
}

export function cleanupUncleanedBranch(skipCleanup: boolean): void {
  const resource = new TrackedHandle("cleanup-uncleaned-branch");
  if (skipCleanup) {
    return;
  }
  resource.dispose();
}

export function runEffectsAndCleanupScenarios(): void {
  runDirectFieldWrite();
  invokeWriter(
    new CustomWriter(),
    watchWrites("effect-polymorphic-write", { flags: 0, mode: "init" }),
  );
  runSetterWrite();

  try {
    cleanupWithUsing(true);
  } catch {
    // Caught so reconciliation observes disposed state after unwinding.
  }

  try {
    cleanupWithTryFinally(true);
  } catch {
    // Caught so reconciliation observes disposed state after finally.
  }

  try {
    cleanupUnprotectedThrow(true);
  } catch {
    // Caught so reconciliation observes uncleaned instance after exception.
  }

  cleanupUncleanedBranch(true);
}
