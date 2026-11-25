export class Resource {
  dispose(): void {}
}
export function makeResource(): Resource {
  return new Resource();
}
export function run(flag: boolean): void {
  const res = makeResource();
  if (flag) {
    res.dispose();
  }
}
