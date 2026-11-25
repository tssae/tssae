import { mode } from "@runtime";
import { externalId } from "ext-lib";

function checkMode(value: string): string {
  return `${value}:${externalId}`;
}

export const current = checkMode(mode);
