export function defineInput(name: string, fields: Record<string, string>) {
  return { name, fields };
}
export function dynamicPromo(): string {
  return "dyn";
}
export const checkoutInput = defineInput("checkout", {
  id: "required",
  promoCode: dynamicPromo(),
});
