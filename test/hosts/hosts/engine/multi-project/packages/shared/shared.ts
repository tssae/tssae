import { mode } from "@runtime";
import { extFlag } from "ext-pkg";

function inspectRuntime(value: string): boolean {
  return value.length > 0 && extFlag;
}

export const active = inspectRuntime(mode);
