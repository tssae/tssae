import { relayJs } from "@ws/js-relay";
import { relayTs } from "@ws/ts-relay";
import { adapt } from "cond-custom";
import { passPureJs } from "pure-js-dep";

export function runEsm(): void {
  const base = { kind: "a" };
  const viaWs = relayTs(base);
  const viaWsJs = relayJs(viaWs);
  const viaJs = passPureJs(viaWsJs);
  const viaCond = adapt(viaJs);
  void viaCond;
}
