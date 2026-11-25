export type CleanupFn = () => Promise<void> | void;

/**
 * Runs registered cleanup callbacks in LIFO order, returning the first failure
 * while still executing all remaining cleanups.
 */
export async function runCleanupsLifo(cleanups: CleanupFn[]): Promise<unknown> {
  let firstError: unknown;
  while (cleanups.length > 0) {
    const cleanup = cleanups.pop()!;
    try {
      await cleanup();
    } catch (error) {
      if (firstError === undefined) {
        firstError = error;
      }
    }
  }
  return firstError;
}

/**
 * Runs registered cleanup callbacks synchronously in LIFO order for
 * `Symbol.dispose`, throwing the first synchronous failure if any occur.
 */
export function runCleanupsLifoSync(cleanups: CleanupFn[]): void {
  let firstError: unknown;
  while (cleanups.length > 0) {
    const cleanup = cleanups.pop()!;
    try {
      const result = cleanup();
      if (result !== undefined && typeof result.then === "function") {
        void result.catch(() => {});
      }
    } catch (error) {
      if (firstError === undefined) {
        firstError = error;
      }
    }
  }
  if (firstError !== undefined) {
    throw firstError;
  }
}
