import type {
  BranchCoverageItem,
  BranchEdgeInfo,
  ImplicitExceptionCoverageOutcome,
} from "./branch-coverage.ts";
import type {
  CallbackInvocationInfo,
  CleanupSiteInfo,
  FieldEffect,
  TargetInstanceCleanupResult,
} from "./call-effects.ts";
import type {
  CallTargetInfo,
  CallTargetsResult,
  DeclarationCallersResult,
  DirectCallInfo,
  ValuePropagationPath,
  ValuePropagationTargetResult,
} from "./calls-values.ts";
import type { FactResult } from "./facts.ts";
import type {
  InstanceProjectionSelection,
  InstanceResolutionResult,
  ProjectedInstanceRow,
  ResolvedInstanceInfo,
} from "./instance-identity.ts";
import type { ImplementationResolutionResult } from "./package-impl.ts";
import type {
  CallProjectionSelection,
  DeclarationProjectionSelection,
  ProjectedCallResolutionRow,
  ProjectedCallRow,
  ProjectedDeclarationRow,
  ProjectedSyntaxLookupRow,
  SyntaxLookupSelection,
} from "./projection.ts";
import type { BehaviorSummaryPack } from "./summaries.ts";
import type {
  DiagnosticLocation,
  Evidence,
  Model,
  SyntaxNodeResult,
  SyntaxRef,
} from "./types.ts";

/** Any individual query result, model, or fact item that carries per-item provenance. */
export type SelectableQueryResult =
  | ValuePropagationPath
  | ValuePropagationTargetResult
  | BranchCoverageItem
  | ImplicitExceptionCoverageOutcome
  | BranchEdgeInfo
  | CallTargetsResult
  | CallTargetInfo
  | DeclarationCallersResult
  | DirectCallInfo
  | FieldEffect
  | CallbackInvocationInfo
  | ResolvedInstanceInfo
  | InstanceResolutionResult
  | TargetInstanceCleanupResult
  | CleanupSiteInfo
  | ImplementationResolutionResult
  | SyntaxNodeResult
  | ProjectedSyntaxLookupRow<SyntaxLookupSelection>
  | ProjectedDeclarationRow<DeclarationProjectionSelection>
  | ProjectedCallRow<CallProjectionSelection>
  | ProjectedCallResolutionRow<CallProjectionSelection>
  | ProjectedInstanceRow<InstanceProjectionSelection>
  | Model
  | BehaviorSummaryPack
  | FactResult;

/** Item accepted in `basis` lists to supply selected provenance or explicit evidence. */
export type DiagnosticBasis = SelectableQueryResult | Evidence;

type ProvenanceRecord = {
  readonly evidence: readonly Evidence[];
};

const provenanceRegistry = new WeakMap<object, ProvenanceRecord>();
const syntaxLocationRegistry = new WeakMap<SyntaxRef, DiagnosticLocation>();

/** Records the source location of a `SyntaxRef` for synchronous lookup. */
export function recordSyntaxLocation(
  ref: SyntaxRef,
  location: DiagnosticLocation,
): void {
  syntaxLocationRegistry.set(ref, location);
}

/** Returns the cached source location for a materialized `SyntaxRef`, if known. */
export function syntaxLocationOf(
  ref: SyntaxRef,
): DiagnosticLocation | undefined {
  return syntaxLocationRegistry.get(ref);
}

/** Normalizes file separators on a source location. */
export function normalizeLocation(loc: DiagnosticLocation): DiagnosticLocation {
  const normalizedFile = loc.file.replaceAll("\\", "/");
  return normalizedFile === loc.file ? loc : { ...loc, file: normalizedFile };
}

/** Normalizes a single `Evidence` entry so optional fields and location paths follow canonical form. */
export function normalizeEvidenceItem(item: Evidence): Evidence {
  return {
    kind: item.kind,
    message: item.message,
    ...(item.at !== undefined ? { at: normalizeLocation(item.at) } : {}),
    ...(item.modelId !== undefined ? { modelId: item.modelId } : {}),
    ...(item.summaryId !== undefined ? { summaryId: item.summaryId } : {}),
    ...(item.summarySource !== undefined
      ? { summarySource: item.summarySource }
      : {}),
    ...(item.declaration !== undefined
      ? { declaration: item.declaration }
      : {}),
  };
}

/** Normalizes an evidence list without removing duplicates. */
export function normalizeEvidence(
  evidence: readonly Evidence[],
): readonly Evidence[] {
  return evidence.map(normalizeEvidenceItem);
}

function evidenceKey(item: Evidence): string {
  const loc = item.at;
  return JSON.stringify([
    item.kind,
    item.message,
    loc !== undefined
      ? `${loc.file}:${loc.start}:${loc.end}:${loc.line}:${loc.column}:${loc.endLine}:${loc.endColumn}`
      : null,
    item.modelId ?? null,
    item.summaryId ?? null,
    item.summarySource ?? null,
    item.declaration ?? null,
  ]);
}

/** Normalizes and deduplicates evidence items while preserving first-seen author order. */
export function dedupeEvidence(
  items: readonly Evidence[],
): readonly Evidence[] {
  const seen = new Set<string>();
  const out: Evidence[] = [];
  for (const rawItem of items) {
    const item = normalizeEvidenceItem(rawItem);
    const key = evidenceKey(item);
    if (!seen.has(key)) {
      seen.add(key);
      out.push(item);
    }
  }
  return out;
}

/** Associates per-item provenance evidence with a materialized query result, model, or fact object. */
export function registerProvenance<T extends object>(
  item: T,
  evidence: readonly Evidence[],
): T {
  provenanceRegistry.set(item, {
    evidence: dedupeEvidence(evidence),
  });
  return item;
}

/** Returns the normalized provenance evidence recorded on a query result, model, or fact item. */
export function evidenceOf(item: SelectableQueryResult): readonly Evidence[] {
  if (typeof item !== "object" || item === null) {
    return [];
  }
  return provenanceRegistry.get(item)?.evidence ?? [];
}

/** Formats a single-line source snippet for concise syntax provenance messages. */
export function summarizeSyntaxSnippet(text: string): string {
  const singleLine = text.replaceAll(/\s+/g, " ").trim();
  if (singleLine.length <= 60) {
    return singleLine;
  }
  return `${singleLine.slice(0, 57)}...`;
}

function isEvidenceItem(value: unknown): value is Evidence {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const candidate = value as Partial<Evidence>;
  return (
    (candidate.kind === "source" ||
      candidate.kind === "type" ||
      candidate.kind === "note" ||
      candidate.kind === "model" ||
      candidate.kind === "summary") &&
    typeof candidate.message === "string"
  );
}

/** Extracts, combines, and deduplicates evidence from selected `basis` items and explicit `evidence` notes in author order. */
export function extractBasisEvidence(
  basis?: readonly DiagnosticBasis[],
  explicitEvidence?: readonly Evidence[],
): readonly Evidence[] {
  const combined: Evidence[] = [];
  if (basis) {
    for (const item of basis) {
      if (typeof item !== "object" || item === null) {
        continue;
      }
      const record = provenanceRegistry.get(item);
      if (record) {
        combined.push(...record.evidence);
      } else if (isEvidenceItem(item)) {
        combined.push(item);
      }
    }
  }
  if (explicitEvidence) {
    combined.push(...explicitEvidence);
  }
  return dedupeEvidence(combined);
}
