import { processItem as otherProcess } from "./other.js";
import { forwardedProcess as aliasedProcess } from "./reexport.js";

interface Handler {
  run: (value: string) => string;
}

export function runAll(
  input: string,
  flag: boolean,
  cb: (v: string) => string,
  handler: Handler,
): string {
  function processItem(localVal: string): string {
    return localVal;
  }
  const a = aliasedProcess(input);
  const b = otherProcess(input);
  const c = processItem(input);
  const mixed = flag ? aliasedProcess : cb;
  const d = mixed(a);
  const e = handler.run(b);
  return `${c}:${d}:${e}`;
}
