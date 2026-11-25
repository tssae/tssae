declare function bad(): void;

export function demo(): void {
  // verule-ignore-next-code-line
  bad();
  // verule-ignore-next-code-line rule-a rule-b -- multiple IDs
  bad();
  // verule-ignore-next-code-line rule-a missing separator
  bad();
  // verule-ignore-next-code-line rule-a --
  bad();
  // verule-ignore-next-code-line unknown-rule -- not configured
  bad();
  // verule-ignore-next-code-line rule-other -- valid for rule-other
  const ok = 1;
  void ok;
}
