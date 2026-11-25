import { defineAdapter, type LibraryAdapter } from "tssae";

import { defineNativeFactProvider } from "../native-provider.ts";
import {
  DrizzleTableColumnsFact,
  QueryRowProjectionFact,
  ValidatorSchemaFieldsFact,
} from "./facts.ts";

/** Bridges public Drizzle table and query facts to the Rust adapter loaded from this package. */
export const drizzleAdapter: LibraryAdapter = defineAdapter({
  id: "drizzle",
  version: "0.1.0",
  description:
    "Rust Drizzle ORM table column, query row projection, and drizzle-zod schema extractor.",
  provides: [
    defineNativeFactProvider(QueryRowProjectionFact, {
      providerId: "drizzle",
    }),
    defineNativeFactProvider(DrizzleTableColumnsFact, {
      providerId: "drizzle",
    }),
    defineNativeFactProvider(ValidatorSchemaFieldsFact, {
      providerId: "drizzle",
    }),
  ],
});

export default drizzleAdapter;
