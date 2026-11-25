import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";
import { z } from "zod";

const localUsers = sqliteTable("local_users", {
  id: integer("id").primaryKey(),
  secret: text("secret").notNull(),
});

function createSelectSchema(_table: unknown) {
  return z.object({ localOnly: z.string() });
}

export const fakeLocalSelectSchema = createSelectSchema(localUsers);
