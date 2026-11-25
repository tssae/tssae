declare function sink(v: string): void;
/** verule-ignore-next-code-line r1 -- JSDoc example must not match */
export function demo(): void {
  // verule-ignore-next-code-line r1 -- Line reason
  sink("ok");
  /* verule-ignore-next-code-line r2 --
   * Block reason
   */
  // leading note
  sink("ok");
  // verule-ignore-next-code-line r3 -- Blank line truncates

  sink("ok");
  // verule-ignore-next-code-line r1 r2 -- malformed multiple IDs
  const s = "// verule-ignore-next-code-line r1 -- inside string";
  void s;
}
