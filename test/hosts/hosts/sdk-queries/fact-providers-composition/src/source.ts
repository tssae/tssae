export function defineSpec(name: string, fields: Record<string, string>) {
  return { name, fields };
}
export const orderSpec = defineSpec("order", {
  orderId: "required",
  coupon: "optional",
});
