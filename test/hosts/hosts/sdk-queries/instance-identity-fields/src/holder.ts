import { createResource, Store } from "./resource.js";

export interface Holder {
  item?: Store;
  other?: Store;
}

export function createHolder(): Holder {
  return {};
}

export function setItem(holder: Holder, value: Store): void {
  holder.item = value;
}

export function getItem(holder: Holder): Store | undefined {
  return holder.item;
}

export class Manager {
  private _store: Store;
  private _other: Store;
  constructor(initial?: Store) {
    this._store = initial ?? createResource();
    this._other = createResource();
  }
  getStore(): Store {
    return this._store;
  }
  getOther(): Store {
    return this._other;
  }
  setStore(next: Store): void {
    this._store = next;
  }
  inspectOwn(): [Store, Store] {
    return [this._store, this._other];
  }
}

export function makeGetter(): () => Store {
  const captured = createResource();
  return () => captured;
}
