export function callWithErrorHandling(
  fn: (...args: unknown[]) => unknown,
  args?: unknown[],
): unknown {
  return args ? fn(...args) : fn();
}
