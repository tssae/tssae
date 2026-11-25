export function ping(): string {
  return "other";
}
export function runUnrelated(): string {
  return ping();
}
