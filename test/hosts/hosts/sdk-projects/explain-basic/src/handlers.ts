function sink(_mode: string, value: string | number): string {
  return String(value);
}

export function processValue(
  input: string | number,
  mode: "fast" | "safe",
): string {
  if (typeof input === "string") {
    return sink(mode, input);
  }
  if (mode === "fast") {
    return sink(mode, input);
  }
  return String(input);
}
