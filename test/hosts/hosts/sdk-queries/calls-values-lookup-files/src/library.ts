export function getFactory() {
  return function factoryResult(value: number): number {
    return value > 0 ? factoryResult(value - 1) : value;
  };
}
export function genericTarget(value: number): number {
  return value > 0 ? genericTarget(value - 1) : value;
}
export function keepIdentity<T>(value: T): T {
  return value;
}
export function forward(value: number): number {
  return value;
}
