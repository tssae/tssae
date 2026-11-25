import { Disposable, Event, type IMenuService } from "./contracts.js";
export class Widget extends Disposable {
  run(menu: IMenuService): void {
    menu.createMenu("main");
    Event.once();
    this._register();
  }
}
