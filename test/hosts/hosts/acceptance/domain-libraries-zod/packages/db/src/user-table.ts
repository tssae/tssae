import { integer, pgTable, text } from "drizzle-orm/pg-core";

export const userTable = pgTable("users", {
  id: integer("id").primaryKey(),
  name: text("name").notNull(),
  age: integer("age").notNull(),
  nickname: text("nickname"),
});
