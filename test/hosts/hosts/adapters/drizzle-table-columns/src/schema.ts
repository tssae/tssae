import { int, mysqlTable, varchar } from "drizzle-orm/mysql-core";
import {
  pgTable,
  serial,
  text,
  timestamp,
  type PgDatabase,
} from "drizzle-orm/pg-core";
import { sqliteTable } from "drizzle-orm/sqlite-core";

export const users = pgTable("users", {
  id: serial("id").primaryKey(),
  email: text("email").notNull(),
  displayName: text("display_name"),
  role: text("role").notNull().default("member"),
  createdAt: timestamp("created_at").defaultNow().notNull(),
  externalId: text("external_id").$defaultFn(() => "gen"),
});

export const sessions = sqliteTable("sessions", (t) => ({
  id: t.integer("id").primaryKey(),
  token: t.text("token").notNull(),
}));

export const auditLogs = mysqlTable("audit_logs", {
  id: int("id").primaryKey(),
  note: varchar("note", { length: 255 }),
});

declare const db: PgDatabase<never>;
export function listUserEmails() {
  return db.select({ id: users.id, email: users.email }).from(users);
}
