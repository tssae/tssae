export function getCallable() {
  return function computeUnique(value: number): number {
    return value > 10 ? computeUnique(value - 1) : value + 1;
  };
}
