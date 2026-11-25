export function computeUnique(input: number): number {
  return input > 10 ? computeUnique(input - 1) : input + 1;
}
export function keepIdentity<T>(value: T): T {
  return value;
}
