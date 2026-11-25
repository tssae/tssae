import type { Store } from "./resource.js";

export function passthrough(value: Store): Store {
  const forwarded = value;
  return forwarded;
}
