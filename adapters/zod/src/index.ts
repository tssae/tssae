export {
  ValidatorFieldFact,
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
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

/** Package-name alias retained for configs migrating to native package loading. */
export const zodAdapter = "@verule/adapter-zod";

export { default } from "./native-adapter.ts";
