import { pgTable, serial, text, timestamp } from "drizzle-orm/pg-core";
import {
  integer,
  sqliteTable,
  text as sqliteText,
} from "drizzle-orm/sqlite-core";

declare function dynamicColumns(): Record<
  string,
  ReturnType<typeof sqliteText>
>;
const dynamicEventKey: string = "payload";

export const accounts = pgTable("accounts", {
  id: serial("id").primaryKey(),
  email: text("email").notNull(),
  displayName: text("display_name"),
  role: text("role").notNull().default("member"),
  createdAt: timestamp("created_at").defaultNow().notNull(),
});

export const dynamicAuditEvents = sqliteTable("dynamic_audit_events", {
  id: integer("id").primaryKey(),
  ...dynamicColumns(),
  [dynamicEventKey]: sqliteText("payload"),
});
