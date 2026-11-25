import { targetHelper as aliasedTarget, ignoredHelper } from "./defs.js";

export function runWorkflow(_input: string): string {
  const first = ignoredHelper(true);
  const second = aliasedTarget("ok", first ? 1 : 2);
  return second;
}
