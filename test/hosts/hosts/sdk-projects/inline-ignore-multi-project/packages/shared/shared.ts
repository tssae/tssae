import { mode } from "@runtime";
declare function register(name: string, runtimeMode?: string): void;

export function init(): void {
  // verule-ignore-next-code-line no-register -- Single call evaluated across two projects and two findings.
  register("single", mode);

  // verule-ignore-next-code-line no-register -- Two distinct calls on one line should be ambiguous.
  const pair = [register("first"), register("second")];
  void pair;
}
