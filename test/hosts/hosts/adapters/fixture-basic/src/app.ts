export function run() {
  return target(42);
}

declare function target(n: number): number;
