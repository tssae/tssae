declare function bad(): void;
export function run(): void {
  // verule-ignore-next-code-line rule-a -- Valid suppression when rule-a runs.
  const x = 1;
  void x;
  // verule-ignore-next-code-line unknown-in-partial -- Cannot confirm unknown under partial ruleIds.
  const y = 2;
  void y;
}
