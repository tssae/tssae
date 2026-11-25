export class Store {
  readonly tag = "store";
}

export function createResource(): Store {
  return new Store();
}

export function makeWrapper(): Store {
  return createResource();
}
