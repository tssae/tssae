import { checkoutInput } from "./source.js";
export function wrapInput(input: unknown, overrides?: Record<string, string>) {
  return { input, overrides };
}
export const forwardedConsumer = wrapInput(checkoutInput);
export const overriddenConsumer = wrapInput(checkoutInput, {
  promoCode: "defaulted",
});
