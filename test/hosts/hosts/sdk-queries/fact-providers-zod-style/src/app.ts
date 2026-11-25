const userSchema = { fields: { name: "required" } };
declare function zParse(schema: unknown, data: unknown): unknown;
export function submit(data: unknown) {
  return zParse(userSchema, data);
}
