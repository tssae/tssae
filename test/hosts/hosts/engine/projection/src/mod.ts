export function id<T>(x: T): T {
  return x;
}

export function empty(): void {}

// oxlint-disable-next-line no-explicit-any
declare const dyn: any;

export function run(s: string, u: string | number) {
  const a = id(s);
  const b = empty();
  const c = dyn(u);
  return { a, b, c };
}
