import { orderSpec } from "./source.js";
export function useSpec(spec: unknown, overrides?: Record<string, string>) {
  return { spec, overrides };
}
export function unrelatedHelper(x: string): string {
  return x;
}
export const rawConsumer = useSpec(orderSpec);
export const overriddenConsumer = useSpec(orderSpec, {
  couponCode: "defaulted",
});
export const ignoredValue = unrelatedHelper("ok");
