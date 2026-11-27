import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";

export const page = sqliteTable("page", {
  id: integer("id").primaryKey(),
  slug: text("slug").notNull(),
  customDomain: text("custom_domain"),
  password: text("password"),
  accessType: text("access_type").notNull().default("public"),
});
