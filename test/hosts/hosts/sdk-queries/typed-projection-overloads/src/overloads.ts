export function parseInput(value: string): { kind: "text"; value: string };
export function parseInput(value: number): { kind: "num"; value: number };
export function parseInput(value: string | number): {
  kind: string;
  value: string | number;
} {
  return typeof value === "string"
    ? { kind: "text", value }
    : { kind: "num", value };
}

export function wrap<T>(item: T, tag: string): { item: T; tag: string } {
  return { item, tag };
}

export function run(
  input: string | number,
  wider: { item: string | number; tag: string },
) {
  const textParsed = parseInput("hello");
  const numParsed = parseInput(42);
  if (typeof input === "string") {
    const wrappedStr = wrap(input, "str");
    return { textParsed, numParsed, wrappedStr, wider };
  }
  const wrappedNum = wrap(input, "num");
  return { textParsed, numParsed, wrappedNum, wider };
}
