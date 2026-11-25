import type { IDisposable } from "@acme/lifecycle";

export function finalRelease(target: IDisposable): void {
  target.dispose();
}
