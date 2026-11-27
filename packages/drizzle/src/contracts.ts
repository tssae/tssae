import type { ValidatorFieldRequirement } from "@tssae/zod";
import {
  dedupeEvidence,
  registerProvenance,
  type DeclarationRef,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type ProjectContext,
  type ProvenanceCarrier,
  type SyntaxRef,
} from "tssae";

import { sortStringKeys } from "./ast.ts";

/** Static Drizzle column metadata extracted from a table definition. */
export type DrizzleColumnInfo = {
  readonly name: string;
  readonly sqlName: string;
  readonly nullable: boolean;
  readonly primaryKey: boolean;
  readonly hasDefault: boolean;
  readonly evidence?: readonly Evidence[];
};

/** Drizzle table columns result with registered per-item provenance. */
export type DrizzleTableColumnsResult = ProvenanceCarrier<{
  readonly library: "drizzle";
  readonly target: DeclarationRef | SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly tableName: string;
  readonly sqlTableName: string;
  readonly columnNames: readonly string[];
  readonly columns: Readonly<Record<string, DrizzleColumnInfo>>;
  readonly hasDynamicColumns: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
}>;

/** Options for `extractDrizzleTableColumns`. */
export type DrizzleTableColumnsOptions = {
  readonly tableName?: string;
  readonly targets?: readonly (DeclarationRef | SyntaxRef)[];
};

/** Classification of a projected column in a database query result. */
export type QueryRowColumnKind = "tableColumn" | "computed" | "unknown";

/** Metadata for a single projected column returned by a database query. */
export type QueryRowColumnInfo = {
  readonly name: string;
  readonly kind: QueryRowColumnKind;
  readonly tableName?: string;
  readonly columnName?: string;
  readonly sqlName?: string;
  readonly nullable?: boolean;
  readonly hasDefault?: boolean;
  readonly evidence?: readonly Evidence[];
};

/** Multiplicity of a nested relational query field. */
export type QueryRowRelationCardinality = "single" | "many";

/** Nested relational projection metadata returned by a relational query (`with`). */
export type QueryRowRelationInfo = {
  readonly name: string;
  readonly targetTable?: string;
  readonly cardinality: QueryRowRelationCardinality;
  readonly columns: readonly string[];
  readonly hasDynamicColumns: boolean;
  readonly relations: Readonly<Record<string, QueryRowRelationInfo>>;
  readonly evidence?: readonly Evidence[];
};

/** Kind of database query producing a row projection. */
export type QueryRowKind = "select" | "relational" | "returning";

/** Row shape projected by a database query (`select`, relational `findFirst`/`findMany`, or `.returning()`). */
export type QueryRowProjectionResult = ProvenanceCarrier<{
  readonly library: string;
  readonly queryKind: QueryRowKind;
  readonly target: SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly tableName?: string;
  readonly projectedColumns: readonly string[];
  readonly columns: Readonly<Record<string, QueryRowColumnInfo>>;
  readonly relations: Readonly<Record<string, QueryRowRelationInfo>>;
  readonly hasDynamicColumns: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
}>;

/** Options accepted by query row projection extractors. */
export type QueryRowProjectionOptions = {
  readonly library?: string;
  readonly targets?: readonly SyntaxRef[];
};

/**
 * Library-agnostic TypeScript contract for database query row projection extractors.
 * Rules written against this interface can inspect row shapes independently of ORM.
 */
export type QueryRowProjectionExtractor = {
  readonly id: string;
  readonly extractRowProjections: (
    ctx: ProjectContext,
    options?: QueryRowProjectionOptions,
  ) => Promise<readonly QueryRowProjectionResult[]>;
};

/** Field state extracted from a `drizzle-zod` schema conversion call. */
export type DrizzleZodFieldInfo = {
  readonly name: string;
  readonly resolved: boolean;
  readonly requirement?: ValidatorFieldRequirement;
  readonly nullable?: boolean;
  readonly evidence?: readonly Evidence[];
  readonly gaps?: readonly Gap[];
};

/** Schema extracted from a verified `drizzle-zod` call (`createSelectSchema` / `createSchemaFactory`). */
export type DrizzleZodSchemaResult = ProvenanceCarrier<{
  readonly library: "drizzle-zod";
  readonly target: SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly schemaName?: string;
  readonly tableName?: string;
  readonly fields: Readonly<Record<string, DrizzleZodFieldInfo>>;
  readonly hasDynamicFields: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
}>;

/** Options for `extractDrizzleZodSchemas`. */
export type DrizzleZodQueryOptions = {
  readonly targets?: readonly SyntaxRef[];
  /** Whether to resolve refinement callbacks and direct Zod overrides in the second argument (default `true`). */
  readonly refineFields?: boolean;
};

/** Input data for `buildDrizzleTableColumns`. */
export type DrizzleTableColumnsInput = {
  readonly target: DeclarationRef | SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly tableName: string;
  readonly sqlTableName: string;
  readonly columns: Readonly<Record<string, DrizzleColumnInfo>>;
  readonly hasDynamicColumns: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

/** Input data for `buildQueryRowProjection`. */
export type QueryRowProjectionInput = {
  readonly library: string;
  readonly queryKind: QueryRowKind;
  readonly target: SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly tableName?: string;
  readonly columns: Readonly<Record<string, QueryRowColumnInfo>>;
  readonly relations: Readonly<Record<string, QueryRowRelationInfo>>;
  readonly hasDynamicColumns: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

/** Input data for `buildDrizzleZodSchema`. */
export type DrizzleZodSchemaInput = {
  readonly target: SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly schemaName?: string;
  readonly tableName?: string;
  readonly fields: Readonly<Record<string, DrizzleZodFieldInfo>>;
  readonly hasDynamicFields: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

export function dedupeGaps(gaps: readonly Gap[]): readonly Gap[] {
  const seen = new Set<string>();
  const out: Gap[] = [];
  for (const gap of gaps) {
    const loc = gap.at;
    const key = `${gap.kind}|${gap.message}|${loc?.file ?? ""}:${String(loc?.start ?? -1)}:${String(loc?.end ?? -1)}`;
    if (!seen.has(key)) {
      seen.add(key);
      out.push(gap);
    }
  }
  return out;
}

/**
 * Constructs a `DrizzleTableColumnsResult` with sorted column names and registered provenance.
 */
export function buildDrizzleTableColumns(
  input: DrizzleTableColumnsInput,
): DrizzleTableColumnsResult {
  const sortedNames = sortStringKeys(Object.keys(input.columns));
  const columns: Record<string, DrizzleColumnInfo> = {};
  for (const name of sortedNames) {
    columns[name] = input.columns[name]!;
  }
  const evidence = dedupeEvidence(input.evidence);
  const gaps = dedupeGaps(input.gaps);
  return registerProvenance(
    {
      library: "drizzle" as const,
      target: input.target,
      at: input.at,
      tableName: input.tableName,
      sqlTableName: input.sqlTableName,
      columnNames: sortedNames,
      columns,
      hasDynamicColumns: input.hasDynamicColumns,
      evidence,
      gaps,
    },
    evidence,
  );
}

/**
 * Constructs a `QueryRowProjectionResult` with sorted projected columns and registered provenance.
 */
export function buildQueryRowProjection(
  input: QueryRowProjectionInput,
): QueryRowProjectionResult {
  const colNames = Object.keys(input.columns);
  const relNames = Object.keys(input.relations);
  const projectedColumns = sortStringKeys([...colNames, ...relNames]);
  const columns: Record<string, QueryRowColumnInfo> = {};
  for (const name of sortStringKeys(colNames)) {
    columns[name] = input.columns[name]!;
  }
  const relations: Record<string, QueryRowRelationInfo> = {};
  for (const name of sortStringKeys(relNames)) {
    relations[name] = input.relations[name]!;
  }
  const evidence = dedupeEvidence(input.evidence);
  const gaps = dedupeGaps(input.gaps);
  return registerProvenance(
    {
      library: input.library,
      queryKind: input.queryKind,
      target: input.target,
      at: input.at,
      ...(input.tableName !== undefined ? { tableName: input.tableName } : {}),
      projectedColumns,
      columns,
      relations,
      hasDynamicColumns: input.hasDynamicColumns,
      evidence,
      gaps,
    },
    evidence,
  );
}

/**
 * Constructs a `DrizzleZodSchemaResult` with sorted fields and registered provenance.
 */
export function buildDrizzleZodSchema(
  input: DrizzleZodSchemaInput,
): DrizzleZodSchemaResult {
  const fields: Record<string, DrizzleZodFieldInfo> = {};
  for (const name of sortStringKeys(Object.keys(input.fields))) {
    fields[name] = input.fields[name]!;
  }
  const evidence = dedupeEvidence(input.evidence);
  const gaps = dedupeGaps(input.gaps);
  return registerProvenance(
    {
      library: "drizzle-zod" as const,
      target: input.target,
      at: input.at,
      ...(input.schemaName !== undefined
        ? { schemaName: input.schemaName }
        : {}),
      ...(input.tableName !== undefined ? { tableName: input.tableName } : {}),
      fields,
      hasDynamicFields: input.hasDynamicFields,
      evidence,
      gaps,
    },
    evidence,
  );
}
