import {
  Disposable,
  DisposableStore,
  Event,
  type IInstantiationService,
  type IMenu,
  type IMenuService,
  unmodeledExternal,
} from "@acme/ui";

class Widget {
  dispose(): void {}
}

export class Panel extends Disposable {
  private readonly _store = new DisposableStore();
  private _widget: Widget;
  constructor(
    event: Event<void>,
    menuService: IMenuService,
    instantiationService: IInstantiationService,
    flag: boolean,
    dynamicFactory: () => IMenu,
  ) {
    super();
    const w1 = new Widget();
    const registered = this._register(w1);
    const added = this._store.add(w1);
    this._widget = registered;
    const evOnce1 = Event.once(event);
    const evOnce2 = Event.once(event);
    const menu1 = menuService.createMenu(1);
    const menu2 = menuService.createMenu(2);
    const menu1Alias = menu1;
    const inst1 = instantiationService.createInstance(Widget);
    const inst2 = instantiationService.createInstance(Widget);
    const unknownExt = unmodeledExternal();
    const partial = flag ? menu1 : unmodeledExternal();
    const dyn = dynamicFactory();
    void [
      added,
      this._widget,
      evOnce1,
      evOnce2,
      menu1Alias,
      menu2,
      inst1,
      inst2,
      unknownExt,
      partial,
      dyn,
    ];
  }
}
