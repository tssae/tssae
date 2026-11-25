import { selectPageSchema } from "@acme/db";

const gateFieldsSchema = selectPageSchema.pick({
  slug: true,
  customDomain: true,
  password: true,
  accessType: true,
});

export function parseGate(input: unknown) {
  return gateFieldsSchema.parse(input);
}
