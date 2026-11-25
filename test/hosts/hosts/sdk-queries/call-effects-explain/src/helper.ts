import type { IDisposable } from "@acme/lifecycle";
export function releaseResource(res: IDisposable): void {
  res.dispose();
}
