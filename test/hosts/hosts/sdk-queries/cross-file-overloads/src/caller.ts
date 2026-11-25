import { parseInput, wrap } from "./overloads.js";

export function runCrossFile(
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
