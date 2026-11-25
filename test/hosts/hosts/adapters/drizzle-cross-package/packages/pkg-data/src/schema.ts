import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const accounts = pgTable("accounts", {
  id: serial("id").primaryKey(),
  ownerEmail: text("owner_email").notNull(),
  nickname: text("nickname").default("anon"),
});
