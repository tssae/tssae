import { DisposableStore, type IDisposable } from "@acme/lifecycle";

import { finalRelease } from "./leaf.js";

export function cleanupHelper(item: IDisposable): void {
  finalRelease(item);
}

export function makeStoreWrapper(item: IDisposable): IDisposable {
  const store = new DisposableStore();
  store.add(item);
  return { dispose: () => store.dispose() };
}
