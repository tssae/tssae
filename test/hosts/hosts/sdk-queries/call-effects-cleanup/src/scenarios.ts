import {
  DisposableStore,
  MutableDisposable,
  createExternalResource,
  onDidDisconnect,
  type IDisposable,
} from "@acme/lifecycle";

import { cleanupHelper, makeStoreWrapper } from "./helpers.js";

export function closeAButCleanB(): void {
  const resourceA = createExternalResource("A");
  const resourceB = createExternalResource("B");
  onDidDisconnect(() => {
    resourceB.dispose();
  });
  void resourceA;
}

export function extractedHelperCleanup(): void {
  const resourceA = createExternalResource("A-helper");
  onDidDisconnect(() => {
    cleanupHelper(resourceA);
  });
}

export function containerWrapperCleanup(): void {
  const resourceA = createExternalResource("A-wrap");
  const resourceB = createExternalResource("B-wrap");
  const wrapperA = makeStoreWrapper(resourceA);
  const wrapperB = makeStoreWrapper(resourceB);
  onDidDisconnect(() => {
    wrapperA.dispose();
  });
  void wrapperB;
}

export function mutableReplaceAndClear(): void {
  const slot = new MutableDisposable<IDisposable>();
  const first = createExternalResource("first");
  const second = createExternalResource("second");
  slot.value = first;
  const replaceWrite = () => {
    slot.value = second;
  };
  const clearSlot = () => {
    slot.clear();
  };
  void [replaceWrite, clearSlot];
}

export function detachWithoutDispose(): void {
  const store = new DisposableStore();
  const conn = createExternalResource("conn");
  store.add(conn);
  onDidDisconnect(() => {
    store.delete(conn);
  });
}

export function conditionalAndDeadCleanup(
  flag: boolean,
  kind: "a" | "b",
): void {
  const resPartial = createExternalResource("partial");
  const resCovered = createExternalResource("covered");
  const resDead = createExternalResource("dead");
  const partialCb = () => {
    if (flag) {
      resPartial.dispose();
    }
  };
  const multiBranchCb = () => {
    if (kind === "a") {
      cleanupHelper(resCovered);
    } else {
      resCovered.dispose();
    }
  };
  const deadCb = () => {
    // oxlint-disable-next-line no-constant-condition
    if (false) {
      resDead.dispose();
    }
  };
  void [partialCb, multiBranchCb, deadCb];
}

export function dynamicCallbackMix(extraCb?: () => void): void {
  const resA = createExternalResource("dyn-A");
  const resB = createExternalResource("dyn-B");
  const knownPlusDyn = () => {
    cleanupHelper(resA);
    extraCb?.();
  };
  const wrongPlusDyn = () => {
    resB.dispose();
    extraCb?.();
  };
  void [knownPlusDyn, wrongPlusDyn];
}
