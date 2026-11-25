import { defineAdapter, defineFactProvider, type LibraryAdapter } from "tssae";

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
    defineFactProvider(QueryRowProjectionFact, (context, request) =>
      context.extractEngineFacts({
        factTypeId: request.factType.id,
        providerId: "drizzle",
        ...(request.targets !== undefined ? { targets: request.targets } : {}),
        ...(request.condition !== undefined
          ? { condition: request.condition }
          : {}),
        ...(request.explain !== undefined ? { explain: request.explain } : {}),
      }),
    ),
    defineFactProvider(DrizzleTableColumnsFact, (context, request) =>
      context.extractEngineFacts({
        factTypeId: request.factType.id,
        providerId: "drizzle",
        ...(request.targets !== undefined ? { targets: request.targets } : {}),
        ...(request.condition !== undefined
          ? { condition: request.condition }
          : {}),
        ...(request.explain !== undefined ? { explain: request.explain } : {}),
      }),
    ),
    defineFactProvider(ValidatorSchemaFieldsFact, (context, request) =>
      context.extractEngineFacts({
        factTypeId: request.factType.id,
        providerId: "drizzle",
        ...(request.targets !== undefined ? { targets: request.targets } : {}),
        ...(request.explain !== undefined ? { explain: request.explain } : {}),
      }),
    ),
  ],
});

export default drizzleAdapter;
