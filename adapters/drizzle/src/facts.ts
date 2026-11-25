import {
  defineFactType,
  type Evidence,
  type FactMergeOutcome,
  type FactProviderContribution,
  type Gap,
} from "tssae";

export type QueryRowSubjectKind = "syntax" | "call" | "declaration";

export type QueryRowCardinality = "single" | "many";

export type QueryRowKind = "select" | "relational" | "returning";

export type QueryRowColumnKind = "tableColumn" | "computed";

export type QueryRowProjectionCondition = {
  readonly library?: string;
};

export type QueryRowColumnInfo = {
  /** TypeScript property name present on the returned row object. */
  readonly name: string;
  /** Underlying SQL column name when statically known from the table definition. */
  readonly sqlName?: string;
  /** Source table name when statically known. */
  readonly tableName?: string;
  readonly kind: QueryRowColumnKind;
  readonly nullable?: boolean;
  readonly evidence?: readonly Evidence[];
};

export type QueryRowNestedRelationInfo = {
  /** Property name on the parent row object (e.g. `workspace`). */
  readonly name: string;
  readonly cardinality: QueryRowCardinality;
  readonly tableName?: string;
  /** Sorted list of statically known property names on this nested relation row. */
  readonly columns: readonly string[];
  readonly columnDetails: Readonly<Record<string, QueryRowColumnInfo>>;
  readonly relations: Readonly<Record<string, QueryRowNestedRelationInfo>>;
  readonly hasDynamicColumns: boolean;
  readonly evidence?: readonly Evidence[];
};

export type QueryRowProjectionFactValue = {
  readonly library: string;
  readonly queryKind: QueryRowKind;
  readonly cardinality: QueryRowCardinality;
  readonly tableName?: string;
  /** Sorted statically known top-level property names on the returned row (columns + relation keys). */
  readonly projectedColumns: readonly string[];
  /** Detailed metadata per top-level scalar or computed column. */
  readonly columns: Readonly<Record<string, QueryRowColumnInfo>>;
  /** Nested relation sub-object projections (e.g. relational `with: { workspace: true }`). */
  readonly relations: Readonly<Record<string, QueryRowNestedRelationInfo>>;
  /** True when spreads, computed keys, or unresolved `columns`/`with` prevent enumerating all fields. */
  readonly hasDynamicColumns: boolean;
};

function compareCodepoint(a: string, b: string): number {
  if (a < b) return -1;
  if (a > b) return 1;
  return 0;
}

function combineEvidence(
  first: readonly Evidence[] | undefined,
  second: readonly Evidence[] | undefined,
): readonly Evidence[] | undefined {
  const combined = [...(first ?? []), ...(second ?? [])];
  if (combined.length === 0) return undefined;
  const seen = new Set<string>();
  const out: Evidence[] = [];
  for (const item of combined) {
    const key = JSON.stringify(item);
    if (!seen.has(key)) {
      seen.add(key);
      out.push(item);
    }
  }
  return out;
}

/** Normalizes and sorts projected column/relation maps into a deterministic fact value. */
export function buildQueryRowProjectionValue(params: {
  readonly library: string;
  readonly queryKind: QueryRowKind;
  readonly cardinality: QueryRowCardinality;
  readonly tableName?: string;
  readonly columns: Readonly<Record<string, QueryRowColumnInfo>>;
  readonly relations?: Readonly<Record<string, QueryRowNestedRelationInfo>>;
  readonly hasDynamicColumns: boolean;
}): QueryRowProjectionFactValue {
  const sortedColumns: Record<string, QueryRowColumnInfo> = {};
  for (const key of Object.keys(params.columns).toSorted(compareCodepoint)) {
    sortedColumns[key] = params.columns[key]!;
  }

  const rawRelations = params.relations ?? {};
  const sortedRelations: Record<string, QueryRowNestedRelationInfo> = {};
  for (const key of Object.keys(rawRelations).toSorted(compareCodepoint)) {
    sortedRelations[key] = rawRelations[key]!;
  }

  const projectedColumns = [
    ...new Set([
      ...Object.keys(sortedColumns),
      ...Object.keys(sortedRelations),
    ]),
  ].toSorted(compareCodepoint);

  return {
    library: params.library,
    queryKind: params.queryKind,
    cardinality: params.cardinality,
    ...(params.tableName !== undefined ? { tableName: params.tableName } : {}),
    projectedColumns,
    columns: sortedColumns,
    relations: sortedRelations,
    hasDynamicColumns: params.hasDynamicColumns,
  };
}

function mergeColumnMaps(
  first: Readonly<Record<string, QueryRowColumnInfo>>,
  second: Readonly<Record<string, QueryRowColumnInfo>>,
  existingEvidence: readonly Evidence[],
  incomingEvidence: readonly Evidence[],
):
  | {
      readonly ok: true;
      readonly columns: Readonly<Record<string, QueryRowColumnInfo>>;
    }
  | {
      readonly ok: false;
      readonly message: string;
      readonly existingEvidence: readonly Evidence[];
      readonly incomingEvidence: readonly Evidence[];
    } {
  const merged: Record<string, QueryRowColumnInfo> = { ...first };
  for (const [name, incomingCol] of Object.entries(second)) {
    const existingCol = merged[name];
    if (!existingCol) {
      merged[name] = incomingCol;
      continue;
    }
    if (
      existingCol.kind !== incomingCol.kind ||
      (existingCol.sqlName !== undefined &&
        incomingCol.sqlName !== undefined &&
        existingCol.sqlName !== incomingCol.sqlName) ||
      (existingCol.nullable !== undefined &&
        incomingCol.nullable !== undefined &&
        existingCol.nullable !== incomingCol.nullable)
    ) {
      return {
        ok: false,
        message: `Conflicting projected column "${name}"`,
        existingEvidence: existingCol.evidence ?? existingEvidence,
        incomingEvidence: incomingCol.evidence ?? incomingEvidence,
      };
    }
    const evidence = combineEvidence(
      existingCol.evidence,
      incomingCol.evidence,
    );
    const sqlName = existingCol.sqlName ?? incomingCol.sqlName;
    const tableName = existingCol.tableName ?? incomingCol.tableName;
    const nullable = existingCol.nullable ?? incomingCol.nullable;
    merged[name] = {
      name,
      ...(sqlName !== undefined ? { sqlName } : {}),
      ...(tableName !== undefined ? { tableName } : {}),
      kind: existingCol.kind,
      ...(nullable !== undefined ? { nullable } : {}),
      ...(evidence !== undefined ? { evidence } : {}),
    };
  }
  return { ok: true, columns: merged };
}

function mergeRelationMaps(
  first: Readonly<Record<string, QueryRowNestedRelationInfo>>,
  second: Readonly<Record<string, QueryRowNestedRelationInfo>>,
  existingEvidence: readonly Evidence[],
  incomingEvidence: readonly Evidence[],
):
  | {
      readonly ok: true;
      readonly relations: Readonly<Record<string, QueryRowNestedRelationInfo>>;
    }
  | {
      readonly ok: false;
      readonly message: string;
      readonly existingEvidence: readonly Evidence[];
      readonly incomingEvidence: readonly Evidence[];
    } {
  const merged: Record<string, QueryRowNestedRelationInfo> = { ...first };
  for (const [name, incomingRel] of Object.entries(second)) {
    const existingRel = merged[name];
    if (!existingRel) {
      merged[name] = incomingRel;
      continue;
    }
    if (existingRel.cardinality !== incomingRel.cardinality) {
      return {
        ok: false,
        message: `Conflicting relation cardinality for "${name}": ${existingRel.cardinality} vs ${incomingRel.cardinality}`,
        existingEvidence: existingRel.evidence ?? existingEvidence,
        incomingEvidence: incomingRel.evidence ?? incomingEvidence,
      };
    }
    const colResult = mergeColumnMaps(
      existingRel.columnDetails,
      incomingRel.columnDetails,
      existingRel.evidence ?? existingEvidence,
      incomingRel.evidence ?? incomingEvidence,
    );
    if (!colResult.ok) {
      return colResult;
    }
    const nestedResult = mergeRelationMaps(
      existingRel.relations,
      incomingRel.relations,
      existingRel.evidence ?? existingEvidence,
      incomingRel.evidence ?? incomingEvidence,
    );
    if (!nestedResult.ok) {
      return nestedResult;
    }
    const evidence = combineEvidence(
      existingRel.evidence,
      incomingRel.evidence,
    );
    const tableName = existingRel.tableName ?? incomingRel.tableName;
    const columns = [
      ...new Set([
        ...Object.keys(colResult.columns),
        ...Object.keys(nestedResult.relations),
      ]),
    ].toSorted(compareCodepoint);
    merged[name] = {
      name,
      cardinality: existingRel.cardinality,
      ...(tableName !== undefined ? { tableName } : {}),
      columns,
      columnDetails: colResult.columns,
      relations: nestedResult.relations,
      hasDynamicColumns:
        existingRel.hasDynamicColumns || incomingRel.hasDynamicColumns,
      ...(evidence !== undefined ? { evidence } : {}),
    };
  }
  return { ok: true, relations: merged };
}

function mergeRowProjections(
  existing: FactProviderContribution<QueryRowProjectionFactValue>,
  incoming: FactProviderContribution<QueryRowProjectionFactValue>,
): FactMergeOutcome<QueryRowProjectionFactValue> {
  if (existing.value.library !== incoming.value.library) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting query library "${existing.value.library}" vs "${incoming.value.library}"`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }
  if (existing.value.queryKind !== incoming.value.queryKind) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting queryKind "${existing.value.queryKind}" vs "${incoming.value.queryKind}"`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }
  if (existing.value.cardinality !== incoming.value.cardinality) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting row cardinality "${existing.value.cardinality}" vs "${incoming.value.cardinality}"`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }

  const colResult = mergeColumnMaps(
    existing.value.columns,
    incoming.value.columns,
    existing.evidence,
    incoming.evidence,
  );
  if (!colResult.ok) {
    return {
      ok: false,
      conflict: {
        message: colResult.message,
        existingEvidence: colResult.existingEvidence,
        incomingEvidence: colResult.incomingEvidence,
      },
    };
  }

  const relResult = mergeRelationMaps(
    existing.value.relations,
    incoming.value.relations,
    existing.evidence,
    incoming.evidence,
  );
  if (!relResult.ok) {
    return {
      ok: false,
      conflict: {
        message: relResult.message,
        existingEvidence: relResult.existingEvidence,
        incomingEvidence: relResult.incomingEvidence,
      },
    };
  }

  const tableName = existing.value.tableName ?? incoming.value.tableName;
  return {
    ok: true,
    value: buildQueryRowProjectionValue({
      library: existing.value.library,
      queryKind: existing.value.queryKind,
      cardinality: existing.value.cardinality,
      ...(tableName !== undefined ? { tableName } : {}),
      columns: colResult.columns,
      relations: relResult.relations,
      hasDynamicColumns:
        existing.value.hasDynamicColumns || incoming.value.hasDynamicColumns,
    }),
  };
}

/**
 * Shared fact type describing the statically projected properties and nested relations
 * returned by a database query call. Compatible with both TS and Rust Drizzle providers.
 */
export const QueryRowProjectionFact = defineFactType<
  QueryRowSubjectKind,
  QueryRowProjectionCondition | undefined,
  QueryRowProjectionFactValue
>({
  id: "verule.query.row-projection",
  description:
    "Statically extracted row property projection (table columns, computed fields, and nested relations) for a query call.",
  subjectKind: ["syntax", "call", "declaration"],
  merge: mergeRowProjections,
});

export type DrizzleTableSubjectKind = "syntax" | "call" | "declaration";

export type DrizzleTableColumnsCondition = {
  readonly tableName?: string;
};

export type DrizzleTableColumnInfo = {
  /** TypeScript property name declared on the Drizzle table object. */
  readonly name: string;
  /** Underlying SQL column name declared in Drizzle (defaults to `name`). */
  readonly sqlName: string;
  readonly nullable: boolean;
  readonly primaryKey: boolean;
  readonly hasDefault: boolean;
  readonly evidence?: readonly Evidence[];
};

export type DrizzleTableColumnsFactValue = {
  readonly library: "drizzle";
  /** TypeScript declaration name when bound to a variable, otherwise SQL table name. */
  readonly tableName: string;
  /** SQL table name literal passed to the Drizzle table builder when statically known. */
  readonly sqlTableName?: string;
  /** Sorted statically known TypeScript column property names on this table. */
  readonly columnNames: readonly string[];
  readonly columns: Readonly<Record<string, DrizzleTableColumnInfo>>;
  /** True when spreads or computed keys prevent enumerating every column statically. */
  readonly hasDynamicColumns: boolean;
};

/** Normalizes and sorts Drizzle table column metadata into a deterministic fact value. */
export function buildDrizzleTableColumnsValue(params: {
  readonly tableName: string;
  readonly sqlTableName?: string;
  readonly columns: Readonly<Record<string, DrizzleTableColumnInfo>>;
  readonly hasDynamicColumns: boolean;
}): DrizzleTableColumnsFactValue {
  const sortedColumns: Record<string, DrizzleTableColumnInfo> = {};
  for (const key of Object.keys(params.columns).toSorted(compareCodepoint)) {
    sortedColumns[key] = params.columns[key]!;
  }
  const columnNames = Object.keys(sortedColumns);

  return {
    library: "drizzle",
    tableName: params.tableName,
    ...(params.sqlTableName !== undefined
      ? { sqlTableName: params.sqlTableName }
      : {}),
    columnNames,
    columns: sortedColumns,
    hasDynamicColumns: params.hasDynamicColumns,
  };
}

function mergeDrizzleTableColumns(
  existing: FactProviderContribution<DrizzleTableColumnsFactValue>,
  incoming: FactProviderContribution<DrizzleTableColumnsFactValue>,
): FactMergeOutcome<DrizzleTableColumnsFactValue> {
  const existingLibrary: string = existing.value.library;
  const incomingLibrary: string = incoming.value.library;
  if (existingLibrary !== incomingLibrary) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting table library "${existingLibrary}" vs "${incomingLibrary}"`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }
  if (existing.value.tableName !== incoming.value.tableName) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting Drizzle table name "${existing.value.tableName}" vs "${incoming.value.tableName}"`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }
  if (
    existing.value.sqlTableName !== undefined &&
    incoming.value.sqlTableName !== undefined &&
    existing.value.sqlTableName !== incoming.value.sqlTableName
  ) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting SQL table name "${existing.value.sqlTableName}" vs "${incoming.value.sqlTableName}"`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }

  const mergedColumns: Record<string, DrizzleTableColumnInfo> = {
    ...existing.value.columns,
  };
  for (const [name, incomingCol] of Object.entries(incoming.value.columns)) {
    const existingCol = mergedColumns[name];
    if (!existingCol) {
      mergedColumns[name] = incomingCol;
      continue;
    }
    if (
      existingCol.sqlName !== incomingCol.sqlName ||
      existingCol.nullable !== incomingCol.nullable ||
      existingCol.primaryKey !== incomingCol.primaryKey ||
      existingCol.hasDefault !== incomingCol.hasDefault
    ) {
      return {
        ok: false,
        conflict: {
          message: `Conflicting Drizzle table column "${name}"`,
          existingEvidence: existingCol.evidence ?? existing.evidence,
          incomingEvidence: incomingCol.evidence ?? incoming.evidence,
        },
      };
    }
    const evidence = combineEvidence(
      existingCol.evidence,
      incomingCol.evidence,
    );
    mergedColumns[name] = {
      name,
      sqlName: existingCol.sqlName,
      nullable: existingCol.nullable,
      primaryKey: existingCol.primaryKey,
      hasDefault: existingCol.hasDefault,
      ...(evidence !== undefined ? { evidence } : {}),
    };
  }

  const sqlTableName =
    existing.value.sqlTableName ?? incoming.value.sqlTableName;
  return {
    ok: true,
    value: buildDrizzleTableColumnsValue({
      tableName: existing.value.tableName,
      ...(sqlTableName !== undefined ? { sqlTableName } : {}),
      columns: mergedColumns,
      hasDynamicColumns:
        existing.value.hasDynamicColumns || incoming.value.hasDynamicColumns,
    }),
  };
}

/**
 * Shared fact type describing the statically extracted columns, nullability,
 * default modifiers, and source evidence of a Drizzle table declaration.
 */
export const DrizzleTableColumnsFact = defineFactType<
  DrizzleTableSubjectKind,
  DrizzleTableColumnsCondition | undefined,
  DrizzleTableColumnsFactValue
>({
  id: "verule.drizzle.table-columns",
  description:
    "Statically extracted Drizzle table column declarations, nullability, defaults, and source evidence.",
  subjectKind: ["syntax", "declaration", "call"],
  merge: mergeDrizzleTableColumns,
});

export type ValidatorSchemaSubjectKind = "syntax" | "declaration" | "call";

export type ValidatorSchemaFieldRequirement =
  | "required"
  | "optional"
  | "defaulted"
  | "forbidden";

export type ValidatorSchemaFieldPresence = "required" | "optional" | "absent";

export type ValidatorSchemaFieldStatus = {
  readonly name: string;
  readonly resolved: boolean;
  readonly requirement?: ValidatorSchemaFieldRequirement;
  readonly presence?: ValidatorSchemaFieldPresence;
  readonly nullable?: boolean;
  readonly fromDefault?: boolean;
  readonly evidence?: readonly Evidence[];
  readonly gaps?: readonly Gap[];
};

export type ValidatorSchemaFieldsFactValue = {
  readonly library: string;
  readonly fields: Readonly<Record<string, ValidatorSchemaFieldStatus>>;
  readonly hasDynamicFields: boolean;
  readonly shapeGaps?: readonly Gap[];
};

/**
 * Shared fact type describing object schema fields derived by a schema converter
 * (such as `drizzle-zod`) before downstream validator chaining (`.extend()`, `.pick()`, etc.).
 */
export const ValidatorSchemaFieldsFact = defineFactType<
  ValidatorSchemaSubjectKind,
  undefined,
  ValidatorSchemaFieldsFactValue
>({
  id: "verule.validator.schema-fields",
  description:
    "Statically extracted schema fields and per-field gaps produced by schema converters such as drizzle-zod.",
  subjectKind: ["syntax", "declaration", "call"],
});
