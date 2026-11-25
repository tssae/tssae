export interface Event<T> {
  (listener: (e: T) => void): void;
}
export declare namespace Event {
  function once<T>(event: Event<T>): Event<T>;
}
export interface IMenu {
  dispose(): void;
}
export interface IMenuService {
  createMenu(id: number): IMenu;
}
export interface IInstantiationService {
  createInstance<T>(ctor: new (...args: unknown[]) => T): T;
}
export declare class DisposableStore {
  add<T>(o: T): T;
  dispose(): void;
}
export declare class Disposable {
  protected _register<T>(o: T): T;
}
export declare function unmodeledExternal(): IMenu;
