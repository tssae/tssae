import {
  DrizzleTableColumnsFact,
  QueryRowProjectionFact,
  ValidatorSchemaFieldsFact,
  drizzleAdapter,
} from "@tssae/adapters/drizzle";
import {
  ValidatorFieldFact,
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
  zodAdapter,
} from "@tssae/adapters/zod";
import { openProject } from "tssae";

export async function runExternalOfficialAdaptersConsumer(
  cwd: string,
): Promise<void> {
  const session = await openProject({
    cwd,
    tsconfig: "tsconfig.json",
    adapters: [zodAdapter, drizzleAdapter],
  });
  try {
    const ctx = session.project();
    const inputs = await ctx.facts(ValidatorInputFieldsFact, {
      providerId: "zod",
    });
    const outputs = await ctx.facts(ValidatorOutputFieldsFact);
    const fields = await ctx.facts(ValidatorFieldFact);
    const tables = await ctx.facts(DrizzleTableColumnsFact);
    const projections = await ctx.facts(QueryRowProjectionFact);
    const schemaFields = await ctx.facts(ValidatorSchemaFieldsFact);
    for (const item of inputs) {
      void item.value.requiredFields;
      void item.value.defaultedFields;
    }
    for (const item of outputs) {
      void item.value.presentFields;
    }
    for (const item of fields) {
      void item.value.resolved;
    }
    for (const item of tables) {
      void item.value.columnNames;
    }
    for (const item of projections) {
      void item.value.projectedColumns;
    }
    for (const item of schemaFields) {
      void item.value.fields;
    }
  } finally {
    await session.close();
  }
}
