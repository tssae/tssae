import { z } from "./zod-stub.js";

export const userSchema = z.object({
  id: z.string(),
  age: z.number(),
  bio: z.string().optional(),
});

export async function handleAll(raw: string, input: unknown) {
  const json = JSON.parse(raw);
  const a = userSchema.parse(input);
  const b = userSchema.safeParse(input);
  const c = await userSchema.parseAsync(input);
  const d = await userSchema.safeParseAsync(input);
  return { json, a, b, c, d };
}
