export class Store {
  dispose(): void {}
}

export function createResource(): Store {
  return new Store();
}

const moduleSingleton = createResource();
export function getSingletonA(): Store {
  return moduleSingleton;
}
export function getSingletonB(): Store {
  return moduleSingleton;
}
