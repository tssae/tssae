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

/** Subpath specifier for enabling the official Drizzle adapter from `@tssae/adapters`. */
export const drizzleAdapter = "@tssae/adapters/drizzle";

export { default } from "./native-adapter.ts";
