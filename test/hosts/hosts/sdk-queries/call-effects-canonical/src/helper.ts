export function invokeSafely(fn: () => void): void {
  try {
    fn();
  } catch {
    // handled
  }
}
