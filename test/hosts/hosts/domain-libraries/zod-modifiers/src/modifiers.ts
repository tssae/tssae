import { z } from "./zod-stub.js";

const workspacePlans = ["free", "pro"] as const;

export const accountSchema = z
  .object({
    id: z.string(),
    homepageUrl: z.string().nullable(),
    nickname: z.string().optional(),
    customTheme: z.string().nullish(),
    plan: z
      .enum(workspacePlans)
      .nullable()
      .default("free")
      .transform((p) => p)
      .openapi({ description: "plan" }),
    password: z.string().nullable().prefault(""),
    timezone: z.string().catch("UTC"),
    fallbackTheme: z.string().nullish().catch(null),
  })
  .refine((v) => Boolean(v.id))
  .superRefine(() => {})
  .transform((v) => ({ ...v }))
  .openapi("Account");

export const pipedSchema = z
  .object({
    rawId: z.string(),
    role: z.string().default("member"),
  })
  .pipe(
    z.object({
      rawId: z.string(),
      role: z.string(),
    }),
  );

export function validate(input: unknown) {
  return {
    account: accountSchema.parse(input),
    piped: pipedSchema.safeParse(input),
  };
}
