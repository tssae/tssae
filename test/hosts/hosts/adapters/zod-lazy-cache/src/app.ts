import { z } from "./zod-stub.js";

export const itemSchema = z.object({
  id: z.string(),
  count: z.number().default(1),
});

export function submit(input: unknown) {
  return itemSchema.parse(input);
}
