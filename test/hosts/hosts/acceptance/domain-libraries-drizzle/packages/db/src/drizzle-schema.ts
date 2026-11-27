import { relations } from "drizzle-orm";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";
import { createSelectSchema } from "drizzle-zod";

export const workspaces = sqliteTable("workspaces", {
  id: integer("id").primaryKey(),
  slug: text("slug").notNull(),
  displayName: text("display_name").notNull(),
});

export const users = sqliteTable("users", {
  id: integer("id").primaryKey(),
  workspaceId: integer("workspace_id")
    .notNull()
    .references(() => workspaces.id),
  fullName: text("full_name").notNull(),
});

export const usersRelations = relations(users, ({ one }) => ({
  workspace: one(workspaces, {
    fields: [users.workspaceId],
    references: [workspaces.id],
  }),
}));

export const workspacesRelations = relations(workspaces, ({ many }) => ({
  users: many(users),
}));

export const strictWorkspaceSchema = createSelectSchema(workspaces);
export const refinedWorkspaceSchema = createSelectSchema(workspaces, {
  slug: (s) => s.optional(),
});
