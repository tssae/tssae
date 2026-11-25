export function helper<T>(val: T): T {
  return val;
}
export function checkFlow(flag: boolean, s: string): string {
  if (!flag) {
    return "";
  }
  return helper(s);
}
