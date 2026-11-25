import type { createSchemaFactory } from "drizzle-zod";
import { z } from "zod";

export function createSelectSchema(_table: unknown) {
  return z.object({ localOnly: z.string() });
}
export const fakeFactory = {} as ReturnType<typeof createSchemaFactory>;
