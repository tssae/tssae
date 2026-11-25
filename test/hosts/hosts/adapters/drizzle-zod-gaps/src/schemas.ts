import { createSelectSchema } from "drizzle-zod";
import { z } from "zod";

import { users } from "./table.js";

declare function extraRefines(): Record<string, z.ZodType>;
const dynKey: string = "extra";

export const refinedWithoutZodProvider = createSelectSchema(users, {
  age: z.number().default(18),
  ...extraRefines(),
  [dynKey]: z.string(),
});

declare const externalTable: typeof users;
export const unresolvedTableSchema = createSelectSchema(externalTable);
