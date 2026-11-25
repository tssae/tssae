import { opaqueBox } from "@acme/decl-only";
import { relayValue } from "@acme/ws-bridge";

function makeOrigin(): string {
  return "origin";
}

export function main(): string {
  const origin = makeOrigin();
  const relayed = relayValue(origin);
  const boxed = opaqueBox(origin);
  return `${relayed}:${boxed}`;
}
