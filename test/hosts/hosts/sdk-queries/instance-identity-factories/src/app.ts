import { passthrough } from "./forward.js";
import { createResource as makeOther } from "./other.js";
import { createResource as makeRes, makeWrapper, Store } from "./resource.js";

export function runScenario(): void {
  function createResource(): Store {
    return new Store();
  }
  const direct1 = new Store();
  const direct2 = new Store();
  const fac1 = makeRes();
  const fac2 = makeRes();
  const aliasFac1 = fac1;
  const forwardedFac1 = passthrough(aliasFac1);
  const forwardedFresh = passthrough(makeRes());
  const wrap1 = makeWrapper();
  const wrap2 = makeWrapper();
  const otherFac = makeOther();
  const localFac = createResource();
  void [
    direct1,
    direct2,
    fac2,
    forwardedFac1,
    forwardedFresh,
    wrap1,
    wrap2,
    otherFac,
    localFac,
  ];
}
