import { barrelSelect } from "@acme/drizzle-helpers";
import { pgTable, serial, text } from "drizzle-orm/pg-core";
import {
  createSchemaFactory,
  createSelectSchema,
  createSelectSchema as aliasedSelect,
} from "drizzle-zod";
import * as dz from "drizzle-zod";

import { createSelectSchema as fakeSelect, fakeFactory } from "./local-fake.js";

export const accounts = pgTable("accounts", {
  id: serial("id").primaryKey(),
  email: text("email").notNull(),
  bio: text("bio"),
});

const factory = createSchemaFactory();
const { createSelectSchema: destructuredSelect } = createSchemaFactory();

export const directSchema = createSelectSchema(accounts);
export const aliasedSchema = aliasedSelect(accounts);
export const namespaceSchema = dz.createSelectSchema(accounts);
export const barrelSchema = barrelSelect(accounts);
export const chainedFactorySchema =
  createSchemaFactory().createSelectSchema(accounts);
export const methodFactorySchema = factory.createSelectSchema(accounts);
export const destructuredFactorySchema = destructuredSelect(accounts);
export const fakeDirectSchema = fakeSelect(accounts);
export const fakeFactorySchema = fakeFactory.createSelectSchema(accounts);
