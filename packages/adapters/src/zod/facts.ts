import {
  defineFactType,
  type Evidence,
  type FactMergeOutcome,
  type FactProviderContribution,
} from "tssae";

export type ValidatorSubjectKind = "syntax" | "call" | "declaration";

export type ValidatorSiteKind = "call" | "schema";

export type ValidatorFieldMode = "replace" | "callback";

export type ValidatorFieldFactValue = {
  readonly resolved: boolean;
  readonly mode: ValidatorFieldMode;
  readonly requirement?: ValidatorInputFieldRequirement;
  readonly presence?: ValidatorOutputFieldPresence;
  readonly nullable?: boolean;
  readonly nullabilityExplicit?: boolean;
  readonly fromDefault?: boolean;
  readonly evidence?: readonly Evidence[];
};

export type ValidatorMethodName =
  | "parse"
  | "safeParse"
  | "parseAsync"
  | "safeParseAsync"
  | (string & {});

export type ValidatorFieldsCondition = {
  readonly scope?: "all" | "calls" | "schemas";
};

export type ValidatorInputFieldRequirement =
  | "required"
  | "optional"
  | "defaulted"
  | "forbidden";

export type ValidatorInputFieldInfo = {
  readonly name: string;
  readonly requirement: ValidatorInputFieldRequirement;
  readonly nullable: boolean;
  readonly evidence?: readonly Evidence[];
};

export type ValidatorInputFieldsFactValue = {
  readonly siteKind: ValidatorSiteKind;
  readonly validatorMethod?: ValidatorMethodName;
  readonly schemaName?: string;
  readonly requiredFields: readonly string[];
  readonly optionalFields: readonly string[];
  readonly defaultedFields: readonly string[];
  readonly fields: Readonly<Record<string, ValidatorInputFieldInfo>>;
  readonly hasDynamicFields: boolean;
};

export type ValidatorOutputFieldPresence = "required" | "optional" | "absent";

export type ValidatorOutputFieldInfo = {
  readonly name: string;
  readonly presence: ValidatorOutputFieldPresence;
  readonly nullable: boolean;
  readonly fromDefault: boolean;
  readonly evidence?: readonly Evidence[];
};

export type ValidatorOutputFieldsFactValue = {
  readonly siteKind: ValidatorSiteKind;
  readonly validatorMethod?: ValidatorMethodName;
  readonly schemaName?: string;
  readonly presentFields: readonly string[];
  readonly optionalFields: readonly string[];
  readonly defaultedFields: readonly string[];
  readonly fields: Readonly<Record<string, ValidatorOutputFieldInfo>>;
  readonly hasDynamicFields: boolean;
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

export function buildInputFieldsValue(params: {
  readonly siteKind: ValidatorSiteKind;
  readonly validatorMethod?: ValidatorMethodName;
  readonly schemaName?: string;
  readonly fields: Readonly<Record<string, ValidatorInputFieldInfo>>;
  readonly hasDynamicFields: boolean;
}): ValidatorInputFieldsFactValue {
  const keys = Object.keys(params.fields).toSorted(compareCodepoint);
  const sortedFields: Record<string, ValidatorInputFieldInfo> = {};
  const requiredFields: string[] = [];
  const optionalFields: string[] = [];
  const defaultedFields: string[] = [];

  for (const key of keys) {
    const info = params.fields[key]!;
    sortedFields[key] = info;
    if (info.requirement === "required") {
      requiredFields.push(key);
    } else if (info.requirement === "optional") {
      optionalFields.push(key);
    } else if (info.requirement === "defaulted") {
      optionalFields.push(key);
      defaultedFields.push(key);
    }
  }

  return {
    siteKind: params.siteKind,
    ...(params.validatorMethod !== undefined
      ? { validatorMethod: params.validatorMethod }
      : {}),
    ...(params.schemaName !== undefined
      ? { schemaName: params.schemaName }
      : {}),
    requiredFields,
    optionalFields,
    defaultedFields,
    fields: sortedFields,
    hasDynamicFields: params.hasDynamicFields,
  };
}

export function buildOutputFieldsValue(params: {
  readonly siteKind: ValidatorSiteKind;
  readonly validatorMethod?: ValidatorMethodName;
  readonly schemaName?: string;
  readonly fields: Readonly<Record<string, ValidatorOutputFieldInfo>>;
  readonly hasDynamicFields: boolean;
}): ValidatorOutputFieldsFactValue {
  const keys = Object.keys(params.fields).toSorted(compareCodepoint);
  const sortedFields: Record<string, ValidatorOutputFieldInfo> = {};
  const presentFields: string[] = [];
  const optionalFields: string[] = [];
  const defaultedFields: string[] = [];

  for (const key of keys) {
    const info = params.fields[key]!;
    sortedFields[key] = info;
    if (info.presence === "required") {
      presentFields.push(key);
    } else if (info.presence === "optional") {
      optionalFields.push(key);
    }
    if (info.fromDefault) {
      defaultedFields.push(key);
    }
  }

  return {
    siteKind: params.siteKind,
    ...(params.validatorMethod !== undefined
      ? { validatorMethod: params.validatorMethod }
      : {}),
    ...(params.schemaName !== undefined
      ? { schemaName: params.schemaName }
      : {}),
    presentFields,
    optionalFields,
    defaultedFields,
    fields: sortedFields,
    hasDynamicFields: params.hasDynamicFields,
  };
}

function pickConflictEvidence(
  fieldEvidence: readonly Evidence[] | undefined,
  contributionEvidence: readonly Evidence[],
): readonly Evidence[] {
  return fieldEvidence && fieldEvidence.length > 0
    ? fieldEvidence
    : contributionEvidence;
}

function mergeInputFields(
  existing: FactProviderContribution<ValidatorInputFieldsFactValue>,
  incoming: FactProviderContribution<ValidatorInputFieldsFactValue>,
): FactMergeOutcome<ValidatorInputFieldsFactValue> {
  if (existing.value.siteKind !== incoming.value.siteKind) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting validator siteKind "${existing.value.siteKind}" vs "${incoming.value.siteKind}"`,
        existingConclusion: `siteKind=${existing.value.siteKind}`,
        incomingConclusion: `siteKind=${incoming.value.siteKind}`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }
  if (
    existing.value.validatorMethod !== undefined &&
    incoming.value.validatorMethod !== undefined &&
    existing.value.validatorMethod !== incoming.value.validatorMethod
  ) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting validator method "${existing.value.validatorMethod}" vs "${incoming.value.validatorMethod}"`,
        existingConclusion: `validatorMethod=${existing.value.validatorMethod}`,
        incomingConclusion: `validatorMethod=${incoming.value.validatorMethod}`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }

  const merged: Record<string, ValidatorInputFieldInfo> = {
    ...existing.value.fields,
  };
  for (const [name, incomingField] of Object.entries(incoming.value.fields)) {
    const existingField = merged[name];
    if (!existingField) {
      merged[name] = incomingField;
      continue;
    }
    if (
      existingField.requirement !== incomingField.requirement ||
      existingField.nullable !== incomingField.nullable
    ) {
      return {
        ok: false,
        conflict: {
          message: `Conflicting input field requirement for "${name}": ${existingField.requirement} (nullable=${String(existingField.nullable)}) vs ${incomingField.requirement} (nullable=${String(incomingField.nullable)})`,
          existingConclusion: `field "${name}": ${existingField.requirement} (nullable=${String(existingField.nullable)})`,
          incomingConclusion: `field "${name}": ${incomingField.requirement} (nullable=${String(incomingField.nullable)})`,
          existingEvidence: pickConflictEvidence(
            existingField.evidence,
            existing.evidence,
          ),
          incomingEvidence: pickConflictEvidence(
            incomingField.evidence,
            incoming.evidence,
          ),
        },
      };
    }
    const evidence = combineEvidence(
      existingField.evidence,
      incomingField.evidence,
    );
    merged[name] = {
      name,
      requirement: existingField.requirement,
      nullable: existingField.nullable,
      ...(evidence !== undefined ? { evidence } : {}),
    };
  }

  const validatorMethod =
    existing.value.validatorMethod ?? incoming.value.validatorMethod;
  const schemaName = existing.value.schemaName ?? incoming.value.schemaName;
  return {
    ok: true,
    value: buildInputFieldsValue({
      siteKind: existing.value.siteKind,
      ...(validatorMethod !== undefined ? { validatorMethod } : {}),
      ...(schemaName !== undefined ? { schemaName } : {}),
      fields: merged,
      hasDynamicFields:
        existing.value.hasDynamicFields || incoming.value.hasDynamicFields,
    }),
  };
}

function mergeOutputFields(
  existing: FactProviderContribution<ValidatorOutputFieldsFactValue>,
  incoming: FactProviderContribution<ValidatorOutputFieldsFactValue>,
): FactMergeOutcome<ValidatorOutputFieldsFactValue> {
  if (existing.value.siteKind !== incoming.value.siteKind) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting validator siteKind "${existing.value.siteKind}" vs "${incoming.value.siteKind}"`,
        existingConclusion: `siteKind=${existing.value.siteKind}`,
        incomingConclusion: `siteKind=${incoming.value.siteKind}`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }
  if (
    existing.value.validatorMethod !== undefined &&
    incoming.value.validatorMethod !== undefined &&
    existing.value.validatorMethod !== incoming.value.validatorMethod
  ) {
    return {
      ok: false,
      conflict: {
        message: `Conflicting validator method "${existing.value.validatorMethod}" vs "${incoming.value.validatorMethod}"`,
        existingConclusion: `validatorMethod=${existing.value.validatorMethod}`,
        incomingConclusion: `validatorMethod=${incoming.value.validatorMethod}`,
        existingEvidence: existing.evidence,
        incomingEvidence: incoming.evidence,
      },
    };
  }

  const merged: Record<string, ValidatorOutputFieldInfo> = {
    ...existing.value.fields,
  };
  for (const [name, incomingField] of Object.entries(incoming.value.fields)) {
    const existingField = merged[name];
    if (!existingField) {
      merged[name] = incomingField;
      continue;
    }
    if (
      existingField.presence !== incomingField.presence ||
      existingField.nullable !== incomingField.nullable ||
      existingField.fromDefault !== incomingField.fromDefault
    ) {
      return {
        ok: false,
        conflict: {
          message: `Conflicting output field presence for "${name}": ${existingField.presence} (nullable=${String(existingField.nullable)}, fromDefault=${String(existingField.fromDefault)}) vs ${incomingField.presence} (nullable=${String(incomingField.nullable)}, fromDefault=${String(incomingField.fromDefault)})`,
          existingConclusion: `field "${name}": ${existingField.presence} (nullable=${String(existingField.nullable)}, fromDefault=${String(existingField.fromDefault)})`,
          incomingConclusion: `field "${name}": ${incomingField.presence} (nullable=${String(incomingField.nullable)}, fromDefault=${String(incomingField.fromDefault)})`,
          existingEvidence: pickConflictEvidence(
            existingField.evidence,
            existing.evidence,
          ),
          incomingEvidence: pickConflictEvidence(
            incomingField.evidence,
            incoming.evidence,
          ),
        },
      };
    }
    const evidence = combineEvidence(
      existingField.evidence,
      incomingField.evidence,
    );
    merged[name] = {
      name,
      presence: existingField.presence,
      nullable: existingField.nullable,
      fromDefault: existingField.fromDefault,
      ...(evidence !== undefined ? { evidence } : {}),
    };
  }

  const validatorMethod =
    existing.value.validatorMethod ?? incoming.value.validatorMethod;
  const schemaName = existing.value.schemaName ?? incoming.value.schemaName;
  return {
    ok: true,
    value: buildOutputFieldsValue({
      siteKind: existing.value.siteKind,
      ...(validatorMethod !== undefined ? { validatorMethod } : {}),
      ...(schemaName !== undefined ? { schemaName } : {}),
      fields: merged,
      hasDynamicFields:
        existing.value.hasDynamicFields || incoming.value.hasDynamicFields,
    }),
  };
}

/**
 * Shared fact type describing input field requirements for a validator schema or validator call.
 * Other validator adapters can implement this same contract without changing consuming rules.
 */
export const ValidatorInputFieldsFact = defineFactType<
  ValidatorSubjectKind,
  ValidatorFieldsCondition | undefined,
  ValidatorInputFieldsFactValue
>({
  id: "tssae.validator.input-fields",
  description:
    "Statically extracted input field requirements (required, optional, defaulted) for a validator schema or call.",
  subjectKind: ["syntax", "call", "declaration"],
  merge: mergeInputFields,
});

/**
 * Shared fact type describing output object fields produced by a validator schema or validator call.
 * Defaulted/prefaulted/caught fields are optional on input but present on output.
 */
export const ValidatorOutputFieldsFact = defineFactType<
  ValidatorSubjectKind,
  ValidatorFieldsCondition | undefined,
  ValidatorOutputFieldsFactValue
>({
  id: "tssae.validator.output-fields",
  description:
    "Statically extracted output object field presence (including defaulted fields) for a validator schema or call.",
  subjectKind: ["syntax", "call", "declaration"],
  merge: mergeOutputFields,
});

/**
 * Shared fact type describing a single field validator expression (direct or refinement callback)
 * so external schema converters can compose field-level validator semantics.
 */
export const ValidatorFieldFact = defineFactType<
  ValidatorSubjectKind,
  undefined,
  ValidatorFieldFactValue
>({
  id: "tssae.validator.field",
  description:
    "Statically extracted validator field modifier and nullability semantics for a field expression.",
  subjectKind: ["syntax", "call", "declaration"],
});
