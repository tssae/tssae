import defFn, { BaseBox, publicMake as renamedMake } from "./defs.js";
export class ChildBox extends BaseBox {
  constructor(label: string) {
    super(label);
  }
}
export function exercise(flag: boolean): string {
  const pick = flag ? renamedMake : renamedMake;
  const made = pick("ok");
  const box = new BaseBox(made.tag);
  const child = new ChildBox(defFn("hi"));
  return box["ping"]() + child.ping();
}
