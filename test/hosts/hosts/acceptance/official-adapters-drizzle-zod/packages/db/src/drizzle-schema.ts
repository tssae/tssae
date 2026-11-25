import { relations } from "drizzle-orm";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";
import { createSelectSchema } from "drizzle-zod";
import { z } from "zod";

export const users = sqliteTable("users", {
  id: integer("id").primaryKey(),
  fullName: text("full_name").notNull(),
  age: integer("age").notNull(),
  status: text("status").notNull(),
});

export const workspace = sqliteTable("workspace", {
  id: integer("id").primaryKey(),
  slug: text("slug").notNull(),
  name: text("name"),
  plan: text("plan").notNull().default("free"),
});

export const page = sqliteTable("page", {
  id: integer("id").primaryKey(),
  workspaceId: integer("workspace_id").notNull(),
  slug: text("slug").notNull(),
  customDomain: text("custom_domain"),
  password: text("password"),
});

export const pageRelations = relations(page, ({ one }) => ({
  workspace: one(workspace),
}));

export const strictUserSchema = createSelectSchema(users);
export const userSchema = strictUserSchema.extend({
  age: z.number().default(18),
});

export const selectWorkspaceSchema = createSelectSchema(workspace).extend({
  plan: z.enum(["free", "pro"]).nullable().prefault("free"),
});

export const selectPageSchema = createSelectSchema(page).extend({
  password: z.string().nullable().prefault(""),
});

export const gateFieldsSchema = selectPageSchema.pick({
  slug: true,
  customDomain: true,
  password: true,
});

export const refinedWorkspaceSchema = createSelectSchema(workspace, {
  slug: (s) => s.optional(),
  name: (s) => s.default("Workspace"),
}).extend({
  plan: z.string().default("enterprise"),
});
