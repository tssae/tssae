import { ambigOp } from "ambiguous-pkg";
// @ts-ignore testing uninstalled package behavior
import { missingFn } from "not-installed-pkg";
import { modeledOp, unmodeledOp, type Item } from "removable-pkg";

export function run(): void {
  const item: Item = { name: "x" };
  const m = modeledOp(item);
  const u = unmodeledOp(item);
  const a = ambigOp(item);
  const z = missingFn(item);
  void [m, u, a, z];
}
