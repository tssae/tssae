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
