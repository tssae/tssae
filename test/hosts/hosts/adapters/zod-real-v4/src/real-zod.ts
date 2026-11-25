import { z } from "zod";

export const realSchema = z
  .object({
    id: z.string(),
    nullableKey: z.string().nullable(),
    optKey: z.string().optional(),
    prefaulted: z.string().nullable().prefault(""),
    caught: z.string().catch("fallback"),
    defaulted: z.string().default("v3"),
  })
  .transform((row) => ({ ...row }));

export function validateReal(payload: unknown) {
  return realSchema.safeParse(payload);
}
