type MissingType = { readonly missing: true };
function ping(): void {}
const anon = () => 1;
declare const dyn: any;
declare const broken: MissingType;
export function testAll() {
  ping();
  dyn(123);
  ping();
  return [anon, broken];
}
