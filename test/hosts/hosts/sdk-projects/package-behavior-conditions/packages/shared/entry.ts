import { adapt } from "cond-custom";

export function runShared(): void {
  const seed = { tag: 1 };
  const out = adapt(seed);
  void out;
}
