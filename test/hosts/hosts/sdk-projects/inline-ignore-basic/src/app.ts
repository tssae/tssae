declare function dangerous(label: string): void;
declare function helper(flag: boolean): void;

export function run(flag: boolean): void {
  // verule-ignore-next-code-line no-dangerous -- Legacy path kept for compatibility.
  dangerous("legacy");
  try {
    if (flag) {
      helper(flag);
    }
  } catch {
    // ignore
  }
}
