import { z } from "./zod-stub.js";

export const createUserSchema = z.object({
  email: z.string(),
  age: z.number(),
});

export function submit(payload: unknown) {
  return createUserSchema.parse(payload);
}
