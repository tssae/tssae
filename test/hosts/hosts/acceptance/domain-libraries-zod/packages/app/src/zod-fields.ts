import { userTable } from "@acceptance-host/db";
import { createSelectSchema } from "drizzle-zod";
import { z } from "zod";

declare function generatedAccountFields(): Record<string, z.ZodType>;

const dynamicFieldName: string = Math.random() > 0.5 ? "first" : "second";

export const accountSchema = z
  .object({
    id: z.number(),
    nullableName: z.string().nullable(),
    nickname: z.string().optional(),
    state: z.string().prefault("active"),
    fallback: z.string().catch("unknown"),
    ...generatedAccountFields(),
    [dynamicFieldName]: z.string(),
  })
  .transform((account) => ({ ...account }));

export const userSchema = createSelectSchema(userTable).extend({
  age: z.number().default(18),
});

export function parseZodFields(row: unknown) {
  return {
    account: accountSchema.safeParse(row),
    user: userSchema.parse(row),
  };
}

z.object({ standaloneOnly: z.string() });
