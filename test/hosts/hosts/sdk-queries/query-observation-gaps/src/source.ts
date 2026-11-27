export function defineSpec(name: string, fields: Record<string, string>) {
  return { name, fields };
}
export function dynamicSpec(): string {
  return "dynamic";
}
export const orderSpec = defineSpec("order", {
  orderId: "required",
  couponCode: dynamicSpec(),
});
