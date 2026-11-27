import { createSelectSchema } from "drizzle-zod";
import { z } from "zod";

import { page } from "./page.js";

export const selectPageSchema = createSelectSchema(page).extend({
  password: z.string().nullable().prefault(""),
  accessType: z.enum(["public", "private"]).prefault("public"),
});
