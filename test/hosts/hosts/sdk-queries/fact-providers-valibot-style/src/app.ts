const userValibot = { fields: { name: "required" } };
declare function vParse(schema: unknown, data: unknown): unknown;
export function submit(data: unknown) {
  return vParse(userValibot, data);
}
