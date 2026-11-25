export {
  DrizzleTableColumnsFact,
  QueryRowProjectionFact,
  ValidatorSchemaFieldsFact,
  buildDrizzleTableColumnsValue,
  buildQueryRowProjectionValue,
  type DrizzleTableColumnInfo,
  type DrizzleTableColumnsCondition,
  type DrizzleTableColumnsFactValue,
  type DrizzleTableSubjectKind,
  type QueryRowCardinality,
  type QueryRowColumnInfo,
  type QueryRowColumnKind,
  type QueryRowKind,
  type QueryRowNestedRelationInfo,
  type QueryRowProjectionCondition,
  type QueryRowProjectionFactValue,
  type QueryRowSubjectKind,
  type ValidatorSchemaFieldPresence,
  type ValidatorSchemaFieldRequirement,
  type ValidatorSchemaFieldStatus,
  type ValidatorSchemaFieldsFactValue,
  type ValidatorSchemaSubjectKind,
} from "./facts.ts";

/** Package-name alias retained for configs migrating to native package loading. */
export const drizzleAdapter = "@verule/adapter-drizzle";
