export interface IMenuService {
  createMenu(id: string): void;
}
export namespace Event {
  export function once(): void {}
}
export abstract class Disposable {
  protected _register(): void {}
}
