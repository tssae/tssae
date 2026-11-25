declare function ruleACall(): void;
declare function ruleBCall(): void;

export function stackedPartial(): void {
  // verule-ignore-next-code-line rule-a -- Suppress rule A on next code line.
  /* verule-ignore-next-code-line rule-b --
   * Reason spanning multiple lines
   *
   * with an internal blank comment line.
   */
  // leading non-directive comment
  ruleACall();
}

export function stackedBothUnused(): void {
  // verule-ignore-next-code-line rule-a -- Unused A.
  // verule-ignore-next-code-line rule-b -- Unused B.
  const ok = 1;
  void ok;
}

export function blankLineBreaksMatch(): void {
  // verule-ignore-next-code-line rule-a -- Separated by blank line.

  ruleACall();
  ruleACall();
}
