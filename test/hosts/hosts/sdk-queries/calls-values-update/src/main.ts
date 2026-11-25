import { relay } from "./helper.js";
function make(): string {
  return "secret";
}
export function run(): string {
  const origin = make();
  const out = relay(origin);
  return out;
}
