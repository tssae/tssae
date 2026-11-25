import { getMode } from "./shared.js";
function sink(mode: string): string {
  return mode;
}
export function runConsumer(): string {
  return sink(getMode());
}
