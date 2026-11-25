export {
  ValidatorFieldFact,
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
  buildInputFieldsValue,
  buildOutputFieldsValue,
  type ValidatorFieldFactValue,
  type ValidatorFieldMode,
  type ValidatorFieldsCondition,
  type ValidatorInputFieldInfo,
  type ValidatorInputFieldRequirement,
  type ValidatorInputFieldsFactValue,
  type ValidatorMethodName,
  type ValidatorOutputFieldInfo,
  type ValidatorOutputFieldPresence,
  type ValidatorOutputFieldsFactValue,
  type ValidatorSiteKind,
  type ValidatorSubjectKind,
} from "./facts.ts";

/** Subpath specifier for enabling the official Zod adapter from `@tssae/adapters`. */
export const zodAdapter = "@tssae/adapters/zod";

export { default } from "./native-adapter.ts";
