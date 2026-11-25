function sink(value: string | number): string {
  return String(value);
}

export function classify(input: string | number): string {
  if (typeof input === "string") {
    return `中文😀:${sink(input)}`;
  }
  return sink(input);
}
