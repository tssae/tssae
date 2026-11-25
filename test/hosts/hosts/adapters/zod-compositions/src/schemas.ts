import { z } from "./zod-stub.js";

export const basePageSchema = z.object({
  id: z.string(),
  slug: z.string(),
  title: z.string(),
  password: z.string().nullable(),
});

export const extraMetaSchema = z.object({
  locale: z.string().prefault("en"),
  secret: z.string(),
});
