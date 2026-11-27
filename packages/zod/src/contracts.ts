import {
  dedupeEvidence,
  registerProvenance,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type ProjectContext,
  type ProvenanceCarrier,
  type SyntaxRef,
} from "tssae";

import { sortStringKeys } from "./ast.ts";

/** Input or output presence/default requirement of a validated field. */
export type ValidatorFieldRequirement = "required" | "optional" | "defaulted";

/** Extracted field metadata reported by a validator library. */
export type ValidatorFieldInfo = {
  readonly name: string;
  readonly requirement: ValidatorFieldRequirement;
  readonly nullable: boolean;
  readonly evidence?: readonly Evidence[];
};

/** Whether a validator result describes a validation call site or a schema definition. */
export type ValidatorSiteKind = "call" | "schema";

/** Input-side field requirements at a validation call or schema definition. */
export type ValidatorInputFieldsResult = ProvenanceCarrier<{
  readonly siteKind: ValidatorSiteKind;
  readonly validatorMethod?: string;
  readonly schemaName?: string;
  readonly target: SyntaxRef;
  readonly schemaSyntax?: SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly providerId: string;
  readonly requiredFields: readonly string[];
  readonly optionalFields: readonly string[];
  readonly defaultedFields: readonly string[];
  readonly fields: Readonly<Record<string, ValidatorFieldInfo>>;
  readonly hasDynamicFields: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
}>;

/** Output-side field presence and defaults after validation completes. */
export type ValidatorOutputFieldsResult = ProvenanceCarrier<{
  readonly siteKind: ValidatorSiteKind;
  readonly validatorMethod?: string;
  readonly schemaName?: string;
  readonly target: SyntaxRef;
  readonly schemaSyntax?: SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly providerId: string;
  readonly presentFields: readonly string[];
  readonly optionalFields: readonly string[];
  readonly defaultedFields: readonly string[];
  readonly fields: Readonly<Record<string, ValidatorFieldInfo>>;
  readonly hasDynamicFields: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
}>;

/** Single-field modifier evaluation result for direct or callback refinements. */
export type ValidatorFieldModifierResult = ProvenanceCarrier<{
  readonly target: SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly providerId: string;
  readonly resolved: boolean;
  readonly mode: "direct" | "callback";
  readonly requirement?: ValidatorFieldRequirement;
  readonly nullable?: boolean;
  readonly fromDefault: boolean;
  readonly nullabilityExplicit: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
}>;

/** Resolved schema shape returned by an external schema resolver hook (for example `drizzle-zod`). */
export type ExternalSchemaResolution = {
  readonly fields: Readonly<Record<string, ValidatorFieldInfo>>;
  readonly hasDynamicFields?: boolean;
  readonly unresolvedFields?: Readonly<Record<string, readonly Gap[]>>;
  readonly evidence?: readonly Evidence[];
  readonly gaps?: readonly Gap[];
};

/** Hook invoked when Zod encounters a foreign schema call (such as `createSelectSchema`). */
export type ExternalSchemaResolver = {
  readonly id: string;
  readonly resolveSchemaCall: (
    ctx: ProjectContext,
    callSyntax: SyntaxRef,
    options?: ValidatorQueryOptions,
  ) => Promise<ExternalSchemaResolution | undefined>;
};

/** Scope filter for validator extraction queries. */
export type ValidatorQueryScope = "all" | "calls" | "schemas";

/** Options accepted by validator extraction functions. */
export type ValidatorQueryOptions = {
  readonly scope?: ValidatorQueryScope;
  readonly targets?: readonly SyntaxRef[];
  readonly externalSchemaResolvers?: readonly ExternalSchemaResolver[];
};

/**
 * Library-agnostic TypeScript contract for validator domain extractors.
 * Rules written against this interface work identically across `@tssae/zod`,
 * `@tssae/drizzle`, or custom validators.
 */
export type ValidatorFieldExtractor = {
  readonly id: string;
  readonly extractInputFields: (
    ctx: ProjectContext,
    options?: ValidatorQueryOptions,
  ) => Promise<readonly ValidatorInputFieldsResult[]>;
  readonly extractOutputFields: (
    ctx: ProjectContext,
    options?: ValidatorQueryOptions,
  ) => Promise<readonly ValidatorOutputFieldsResult[]>;
};

/** Input data for `buildValidatorInputFields`. */
export type ValidatorInputFieldsInput = {
  readonly siteKind: ValidatorSiteKind;
  readonly validatorMethod?: string;
  readonly schemaName?: string;
  readonly target: SyntaxRef;
  readonly schemaSyntax?: SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly providerId: string;
  readonly fields: Readonly<Record<string, ValidatorFieldInfo>>;
  readonly hasDynamicFields: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

/** Input data for `buildValidatorOutputFields`. */
export type ValidatorOutputFieldsInput = ValidatorInputFieldsInput;

/** Input data for `buildValidatorFieldModifier`. */
export type ValidatorFieldModifierInput = {
  readonly target: SyntaxRef;
  readonly at: DiagnosticLocation;
  readonly providerId: string;
  readonly resolved: boolean;
  readonly mode: "direct" | "callback";
  readonly requirement?: ValidatorFieldRequirement;
  readonly nullable?: boolean;
  readonly fromDefault: boolean;
  readonly nullabilityExplicit: boolean;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

function dedupeGaps(gaps: readonly Gap[]): readonly Gap[] {
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
 * Constructs a `ValidatorInputFieldsResult` with sorted field partitions and
 * registered per-item provenance for `extractBasisEvidence`.
 */
export function buildValidatorInputFields(
  input: ValidatorInputFieldsInput,
): ValidatorInputFieldsResult {
  const sortedKeys = sortStringKeys(Object.keys(input.fields));
  const normalizedFields: Record<string, ValidatorFieldInfo> = {};
  const requiredFields: string[] = [];
  const optionalFields: string[] = [];
  const defaultedFields: string[] = [];

  for (const key of sortedKeys) {
    const field = input.fields[key]!;
    normalizedFields[key] = field;
    if (field.requirement === "required") {
      requiredFields.push(key);
    } else {
      optionalFields.push(key);
      if (field.requirement === "defaulted") {
        defaultedFields.push(key);
      }
    }
  }

  const evidence = dedupeEvidence(input.evidence);
  const gaps = dedupeGaps(input.gaps);
  const entry = {
    siteKind: input.siteKind,
    ...(input.validatorMethod !== undefined
      ? { validatorMethod: input.validatorMethod }
      : {}),
    ...(input.schemaName !== undefined ? { schemaName: input.schemaName } : {}),
    target: input.target,
    ...(input.schemaSyntax !== undefined
      ? { schemaSyntax: input.schemaSyntax }
      : {}),
    at: input.at,
    providerId: input.providerId,
    requiredFields,
    optionalFields,
    defaultedFields,
    fields: normalizedFields,
    hasDynamicFields: input.hasDynamicFields,
    evidence,
    gaps,
  };
  return registerProvenance(entry, evidence);
}

/**
 * Constructs a `ValidatorOutputFieldsResult` with sorted present/optional/defaulted
 * partitions and registered per-item provenance for `extractBasisEvidence`.
 */
export function buildValidatorOutputFields(
  input: ValidatorOutputFieldsInput,
): ValidatorOutputFieldsResult {
  const sortedKeys = sortStringKeys(Object.keys(input.fields));
  const normalizedFields: Record<string, ValidatorFieldInfo> = {};
  const presentFields: string[] = [];
  const optionalFields: string[] = [];
  const defaultedFields: string[] = [];

  for (const key of sortedKeys) {
    const field = input.fields[key]!;
    normalizedFields[key] = field;
    if (field.requirement === "optional") {
      optionalFields.push(key);
    } else {
      presentFields.push(key);
      if (field.requirement === "defaulted") {
        defaultedFields.push(key);
      }
    }
  }

  const evidence = dedupeEvidence(input.evidence);
  const gaps = dedupeGaps(input.gaps);
  const entry = {
    siteKind: input.siteKind,
    ...(input.validatorMethod !== undefined
      ? { validatorMethod: input.validatorMethod }
      : {}),
    ...(input.schemaName !== undefined ? { schemaName: input.schemaName } : {}),
    target: input.target,
    ...(input.schemaSyntax !== undefined
      ? { schemaSyntax: input.schemaSyntax }
      : {}),
    at: input.at,
    providerId: input.providerId,
    presentFields,
    optionalFields,
    defaultedFields,
    fields: normalizedFields,
    hasDynamicFields: input.hasDynamicFields,
    evidence,
    gaps,
  };
  return registerProvenance(entry, evidence);
}

/**
 * Constructs a `ValidatorFieldModifierResult` with registered per-item provenance
 * for `extractBasisEvidence`.
 */
export function buildValidatorFieldModifier(
  input: ValidatorFieldModifierInput,
): ValidatorFieldModifierResult {
  const evidence = dedupeEvidence(input.evidence);
  const gaps = dedupeGaps(input.gaps);
  const entry = {
    target: input.target,
    at: input.at,
    providerId: input.providerId,
    resolved: input.resolved,
    mode: input.mode,
    ...(input.requirement !== undefined
      ? { requirement: input.requirement }
      : {}),
    ...(input.nullable !== undefined ? { nullable: input.nullable } : {}),
    fromDefault: input.fromDefault,
    nullabilityExplicit: input.nullabilityExplicit,
    evidence,
    gaps,
  };
  return registerProvenance(entry, evidence);
}
