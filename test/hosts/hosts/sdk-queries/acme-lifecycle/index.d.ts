export interface IDisposable {
  dispose(): void;
}
export declare class DisposableStore implements IDisposable {
  add<T extends IDisposable>(o: T): T;
  delete<T extends IDisposable>(o: T): void;
  clear(): void;
  dispose(): void;
}
export declare class MutableDisposable<
  T extends IDisposable = IDisposable,
> implements IDisposable {
  value: T | undefined;
  clear(): void;
  dispose(): void;
}
export declare class Disposable implements IDisposable {
  protected _register<T extends IDisposable>(o: T): T;
  dispose(): void;
}
export declare function createExternalResource(name: string): IDisposable;
export declare function onDidDisconnect(cb: () => void): IDisposable;
