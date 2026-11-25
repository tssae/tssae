import { v } from "./valibot-stub.js";

export const createUserSchema = v.schema({
  email: v.string(),
  age: v.number(),
});

export function submit(payload: unknown) {
  return v.parse(createUserSchema, payload);
}
