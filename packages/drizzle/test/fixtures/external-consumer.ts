import {
  buildDrizzleTableColumns,
  buildDrizzleZodSchema,
  buildQueryRowProjection,
  drizzleQueryProjection,
  drizzleZodSchemaResolver,
  drizzleZodValidator,
  extractDrizzleRowProjections,
  extractDrizzleTableColumns,
  extractDrizzleZodSchemas,
  type DrizzleTableColumnsResult,
  type DrizzleZodSchemaResult,
  type QueryRowProjectionExtractor,
  type QueryRowProjectionResult,
} from "@tssae/drizzle";
import {
  extractZodInputFields,
  extractZodOutputFields,
  type ValidatorInputFieldsResult,
  type ValidatorOutputFieldsResult,
} from "@tssae/zod";
import {
  evidenceOf,
  extractBasisEvidence,
  openProject,
  type Gap,
  type ProjectContext,
} from "tssae";

export async function inspectDrizzle(
  cwd: string,
  projectionExtractor: QueryRowProjectionExtractor = drizzleQueryProjection,
): Promise<{
  readonly tables: readonly DrizzleTableColumnsResult[];
  readonly projections: readonly QueryRowProjectionResult[];
  readonly schemas: readonly DrizzleZodSchemaResult[];
  readonly validatorInputs: readonly ValidatorInputFieldsResult[];
  readonly validatorOutputs: readonly ValidatorOutputFieldsResult[];
  readonly gaps: readonly Gap[];
}> {
  const gaps: Gap[] = [];
  const session = await openProject({ cwd });
  try {
    const ctx: ProjectContext = session.project(undefined, {
      observer: {
        onGaps: (reported) => gaps.push(...reported),
      },
    });
    const tables = await extractDrizzleTableColumns(ctx);
    const directProjections = await extractDrizzleRowProjections(ctx);
    const projections = await projectionExtractor.extractRowProjections(ctx);
    if (directProjections.length !== projections.length) {
      throw new Error("Expected matching row projection counts");
    }
    const schemas = await extractDrizzleZodSchemas(ctx);
    const validatorInputs = await drizzleZodValidator.extractInputFields(ctx);
    const validatorOutputs = await drizzleZodValidator.extractOutputFields(ctx);

    const directInputs = await extractZodInputFields(ctx, {
      externalSchemaResolvers: [drizzleZodSchemaResolver],
    });
    const directOutputs = await extractZodOutputFields(ctx, {
      externalSchemaResolvers: [drizzleZodSchemaResolver],
    });

    const syntheticTable = buildDrizzleTableColumns({
      target: tables[0]!.target,
      at: tables[0]!.at,
      tableName: "synthetic",
      sqlTableName: "synthetic",
      columns: {},
      hasDynamicColumns: false,
      evidence: [],
      gaps: [],
    });
    const syntheticProj = buildQueryRowProjection({
      library: "drizzle",
      queryKind: "select",
      target: projections[0]!.target,
      at: projections[0]!.at,
      columns: {},
      relations: {},
      hasDynamicColumns: false,
      evidence: [],
      gaps: [],
    });
    const syntheticSchema = buildDrizzleZodSchema({
      target: schemas[0]!.target,
      at: schemas[0]!.at,
      fields: {},
      hasDynamicFields: false,
      evidence: [],
      gaps: [],
    });

    const basisEvidence = extractBasisEvidence([
      ...tables,
      ...projections,
      ...schemas,
      ...directInputs,
      ...directOutputs,
      syntheticTable,
      syntheticProj,
      syntheticSchema,
    ]);
    const tableEvidence = tables[0] ? evidenceOf(tables[0]) : [];
    if (basisEvidence.length < tableEvidence.length) {
      throw new Error(
        "Expected extractBasisEvidence to include table provenance",
      );
    }

    return {
      tables,
      projections,
      schemas,
      validatorInputs,
      validatorOutputs,
      gaps,
    };
  } finally {
    await session.close();
  }
}
