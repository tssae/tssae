function sink(mode: string): string {
  return mode;
}
export function runAlt(): string {
  return sink("legacy");
}
