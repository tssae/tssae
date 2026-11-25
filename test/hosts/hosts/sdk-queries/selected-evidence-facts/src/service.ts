export function fetchSecret(): string {
  return "secret";
}
export function publish(): void {
  const v = fetchSecret();
  void v;
}
