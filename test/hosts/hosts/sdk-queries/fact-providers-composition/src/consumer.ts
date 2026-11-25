import { orderSpec } from "./source.js";
export function useSpec(spec: unknown, overrides: Record<string, string>) {
  return { spec, overrides };
}
export const orderConsumer = useSpec(orderSpec, { coupon: "defaulted" });
