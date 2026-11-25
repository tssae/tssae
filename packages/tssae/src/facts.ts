import { performance } from "node:perf_hooks";

import {
  dedupeEvidence,
  extractBasisEvidence,
  registerProvenance,
  type DiagnosticBasis,
} from "./evidence.ts";
import type { ObservationCollector } from "./observation.ts";
import { locationMatchesTarget } from "./observation.ts";
import { validateProgramRefs } from "./projection.ts";
import {
  TssaeError,
  type DiagnosticLocation,
  type Evidence,
  type ExplainLocalGap,
  type FactConflictDetails,
  type FactConflictParty,
  type Gap,
  type Model,
  type ProgramObjectKind,
  type ProgramRef,
  type ProjectSummary,
  type ProjectContext,
  type SyntaxRef,
} from "./types.ts";

declare const factSubjectBrand: unique symbol;
declare const factConditionBrand: unique symbol;
declare const factValueBrand: unique symbol;

/** Contribution passed to `FactType.merge` for combining multi-provider facts. */
export type FactProviderContribution<Value = unknown> = {
  readonly providerId: string;
  readonly providers: readonly string[];
  readonly value: Value;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

/** Outcome of merging two provider contributions for the same `(snapshot, project, subject, condition)`. */
export type FactMergeOutcome<Value = unknown> =
  | {
      readonly ok: true;
      readonly value: Value;
    }
  | {
      readonly ok: false;
      readonly conflict: {
        readonly message: string;
        readonly existingConclusion?: string;
        readonly incomingConclusion?: string;
        readonly existingEvidence?: readonly Evidence[];
        readonly incomingEvidence?: readonly Evidence[];
      };
    };

/** Specification passed to `defineFactType`. */
export type FactTypeSpec<
  SubjectKind extends ProgramObjectKind = ProgramObjectKind,
  Condition = unknown,
  Value = unknown,
> = {
  readonly id: string;
  readonly description?: string;
  readonly subjectKind: SubjectKind | readonly SubjectKind[];
  readonly [factConditionBrand]?: Condition;
  merge?(
    existing: FactProviderContribution<Value>,
    incoming: FactProviderContribution<Value>,
  ): FactMergeOutcome<Value>;
};

/** Typed definition of a queryable program fact shared between adapters and rules. */
export type FactType<
  SubjectKind extends ProgramObjectKind = ProgramObjectKind,
  Condition = unknown,
  Value = unknown,
> = {
  readonly id: string;
  readonly description?: string;
  readonly subjectKind: SubjectKind | readonly SubjectKind[];
  readonly [factSubjectBrand]?: SubjectKind;
  readonly [factConditionBrand]?: Condition;
  readonly [factValueBrand]?: Value;
  merge?(
    existing: FactProviderContribution<Value>,
    incoming: FactProviderContribution<Value>,
  ): FactMergeOutcome<Value>;
};

/** Options passed to `ProjectContext.facts`. */
export type FactQueryOptions<
  SubjectKind extends ProgramObjectKind = ProgramObjectKind,
  Condition = unknown,
> = {
  readonly targets?: readonly ProgramRef<SubjectKind>[];
  readonly providerId?: string;
  readonly condition?: Condition;
  readonly explain?: boolean;
};

/** Snapshot-bound fact result returned by `ProjectContext.facts`. */
export type FactResult<
  SubjectKind extends ProgramObjectKind = ProgramObjectKind,
  Value = unknown,
> = {
  readonly factTypeId: string;
  readonly projectId: string;
  readonly snapshotId: number;
  readonly subject: ProgramRef<SubjectKind>;
  readonly value: Value;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
  readonly providers: readonly string[];
  readonly explainTrace?: FactExplainTrace;
};

/** Options passed to `AdapterContext.extractEngineFacts`. */
export type EngineFactExtractionOptions<
  SubjectKind extends ProgramObjectKind = ProgramObjectKind,
  Condition = unknown,
> = {
  readonly factTypeId: string;
  readonly providerId: string;
  readonly reportProviderId?: string;
  readonly targets?: readonly ProgramRef<SubjectKind>[];
  readonly condition?: Condition;
  readonly explain?: boolean;
};

/** Read-only query context exposed to TypeScript `LibraryAdapter` providers. */
export type AdapterContext = {
  readonly project: ProjectSummary;
  readonly models: readonly Model[];
  getModel: (id: string) => Model | undefined;
  findSyntax: ProjectContext["findSyntax"];
  typeAt: ProjectContext["typeAt"];
  listBranches: ProjectContext["listBranches"];
  branchCoverage: ProjectContext["branchCoverage"];
  resolveDeclarations: ProjectContext["resolveDeclarations"];
  resolveCalls: ProjectContext["resolveCalls"];
  projectCalls: ProjectContext["projectCalls"];
  projectSignatures: ProjectContext["projectSignatures"];
  projectTypes: ProjectContext["projectTypes"];
  isAssignableTo: ProjectContext["isAssignableTo"];
  syntaxOf: ProjectContext["syntaxOf"];
  callersOf: ProjectContext["callersOf"];
  calleesOf: ProjectContext["calleesOf"];
  resolveImplementations: ProjectContext["resolveImplementations"];
  valuePropagation: ProjectContext["valuePropagation"];
  callEffects: ProjectContext["callEffects"];
  instanceCleanup: ProjectContext["instanceCleanup"];
  /** Queries facts from other enabled providers on the active project snapshot. */
  facts: ProjectContext["facts"];
  /** Invokes an engine-internal built-in fact provider on the active project snapshot. */
  extractEngineFacts: <
    SubjectKind extends ProgramObjectKind,
    Condition = unknown,
    Value = unknown,
  >(
    options: EngineFactExtractionOptions<SubjectKind, Condition>,
  ) => Promise<readonly FactContribution<SubjectKind, Value>[]>;
  /** Registers a local non-source file read by this adapter as an additional input. */
  registerAdditionalInput: ProjectContext["registerAdditionalInput"];
  /** Registers a local non-source file as an additional input and reads its UTF-8 content. */
  readAdditionalInput: ProjectContext["readAdditionalInput"];
};

/** Optional explain trace populated only when `request.explain` is true. */
export type FactExplainTrace = {
  readonly localGaps?: readonly ExplainLocalGap[];
};

/** Request passed to a `FactProvider.extract` implementation. */
export type FactExtractionRequest<
  SubjectKind extends ProgramObjectKind = ProgramObjectKind,
  Condition = unknown,
> = {
  readonly factType: FactType<SubjectKind, Condition>;
  readonly targets?: readonly ProgramRef<SubjectKind>[];
  readonly condition?: Condition;
  readonly explain?: boolean;
};

/** Single subject contribution returned by a `FactProvider.extract` call. */
export type FactContribution<
  SubjectKind extends ProgramObjectKind = ProgramObjectKind,
  Value = unknown,
> = {
  readonly subject: ProgramRef<SubjectKind>;
  readonly value: Value;
  readonly basis?: readonly DiagnosticBasis[];
  /** Evidence supplied directly by the provider, appended after any selected basis. */
  readonly evidence?: readonly Evidence[];
  readonly gaps?: readonly Gap[];
  readonly explainTrace?: FactExplainTrace;
};

/** Provider implementation for one `FactType` inside a `LibraryAdapter`. */
export interface FactProvider<
  SubjectKind extends ProgramObjectKind = ProgramObjectKind,
  Condition = unknown,
  Value = unknown,
> {
  readonly factType: FactType<SubjectKind, Condition, Value>;
  extract(
    ctx: AdapterContext,
    request: FactExtractionRequest<SubjectKind, Condition>,
  ):
    | readonly FactContribution<SubjectKind, Value>[]
    | Promise<readonly FactContribution<SubjectKind, Value>[]>;
}

/** Optional library adapter registered in `OpenProjectOptions.adapters`. */
export type LibraryAdapter = {
  readonly id: string;
  readonly version?: string;
  readonly description?: string;
  readonly provides: readonly FactProvider[];
};

/** Library adapter object or installed package name accepted by project configuration. */
export type ConfiguredAdapter = LibraryAdapter | string;

/** Defines a typed `FactType` contract that multiple adapters can implement. */
export function defineFactType<
  SubjectKind extends ProgramObjectKind,
  Condition = undefined,
  Value = unknown,
>(
  spec: FactTypeSpec<SubjectKind, Condition, Value>,
): FactType<SubjectKind, Condition, Value> {
  if (typeof spec.id !== "string" || spec.id.trim() === "") {
    throw new TssaeError(
      "INVALID_CONFIG",
      "FactType id must be a non-empty string.",
    );
  }
  const kinds = Array.isArray(spec.subjectKind)
    ? spec.subjectKind
    : [spec.subjectKind];
  if (kinds.length === 0) {
    throw new TssaeError(
      "INVALID_CONFIG",
      `FactType "${spec.id}" must specify at least one subjectKind.`,
    );
  }
  return {
    id: spec.id,
    ...(spec.description !== undefined
      ? { description: spec.description }
      : {}),
    subjectKind: Array.isArray(spec.subjectKind)
      ? [...spec.subjectKind]
      : spec.subjectKind,
    ...(spec.merge !== undefined
      ? {
          merge: (
            existing: FactProviderContribution<Value>,
            incoming: FactProviderContribution<Value>,
          ) => spec.merge!(existing, incoming),
        }
      : {}),
  };
}

/** Helper for defining a strongly typed `FactProvider` entry for `defineAdapter`. */
export function defineFactProvider<
  SubjectKind extends ProgramObjectKind,
  Condition,
  Value,
>(
  factType: FactType<SubjectKind, Condition, Value>,
  extract: (
    ctx: AdapterContext,
    request: FactExtractionRequest<NoInfer<SubjectKind>, NoInfer<Condition>>,
  ) =>
    | readonly FactContribution<NoInfer<SubjectKind>, NoInfer<Value>>[]
    | Promise<
        readonly FactContribution<NoInfer<SubjectKind>, NoInfer<Value>>[]
      >,
): FactProvider<SubjectKind, Condition, Value> {
  return {
    factType,
    extract,
  };
}

/** Validates and normalizes a `LibraryAdapter` definition. */
export function defineAdapter(adapter: LibraryAdapter): LibraryAdapter {
  if (typeof adapter.id !== "string" || adapter.id.trim() === "") {
    throw new TssaeError(
      "INVALID_CONFIG",
      "LibraryAdapter id must be a non-empty string.",
    );
  }
  if (adapter.version !== undefined && typeof adapter.version !== "string") {
    throw new TssaeError(
      "INVALID_CONFIG",
      `LibraryAdapter "${adapter.id}" version must be a string when provided.`,
    );
  }
  if (!Array.isArray(adapter.provides)) {
    throw new TssaeError(
      "INVALID_CONFIG",
      `LibraryAdapter "${adapter.id}" must specify a provides array.`,
    );
  }
  const seenFactIds = new Set<string>();
  for (const provider of adapter.provides) {
    if (
      typeof provider.factType?.id !== "string" ||
      provider.factType.id.trim() === ""
    ) {
      throw new TssaeError(
        "INVALID_CONFIG",
        `LibraryAdapter "${adapter.id}" contains a provider with an invalid factType.`,
      );
    }
    if (seenFactIds.has(provider.factType.id)) {
      throw new TssaeError(
        "INVALID_CONFIG",
        `LibraryAdapter "${adapter.id}" declares duplicate provider for factType "${provider.factType.id}".`,
      );
    }
    if (typeof provider.extract !== "function") {
      throw new TssaeError(
        "INVALID_CONFIG",
        `LibraryAdapter "${adapter.id}" provider for "${provider.factType.id}" must provide an extract function.`,
      );
    }
    seenFactIds.add(provider.factType.id);
  }
  return {
    id: adapter.id,
    ...(adapter.version !== undefined ? { version: adapter.version } : {}),
    ...(adapter.description !== undefined
      ? { description: adapter.description }
      : {}),
    provides: [...adapter.provides],
  };
}

/** Validates configured adapters and rejects duplicate adapter IDs. */
export function validateAdapters(
  adapters: readonly LibraryAdapter[],
): readonly LibraryAdapter[] {
  const seen = new Set<string>();
  const normalized: LibraryAdapter[] = [];
  for (const raw of adapters) {
    const adapter = defineAdapter(raw);
    if (seen.has(adapter.id)) {
      throw new TssaeError(
        "INVALID_CONFIG",
        `Duplicate adapter id: "${adapter.id}".`,
      );
    }
    seen.add(adapter.id);
    normalized.push(adapter);
  }
  return normalized;
}

function compareCodepoint(a: string, b: string): number {
  if (a < b) return -1;
  if (a > b) return 1;
  return 0;
}

function stableSerializeValue(value: unknown): string {
  if (value === null || typeof value !== "object") {
    return JSON.stringify(value) ?? "undefined";
  }
  if (Array.isArray(value)) {
    return `[${value.map((item) => stableSerializeValue(item)).join(",")}]`;
  }
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record).toSorted(compareCodepoint);
  return `{${keys
    .filter((key) => record[key] !== undefined)
    .map((key) => `${JSON.stringify(key)}:${stableSerializeValue(record[key])}`)
    .join(",")}}`;
}

function formatEvidenceList(evidence: readonly Evidence[]): string {
  if (evidence.length === 0) return "none";
  return evidence
    .map((item) => {
      const loc = item.at
        ? ` (${item.at.file}:${item.at.line}:${item.at.column})`
        : "";
      return `${item.message}${loc}`;
    })
    .join("; ");
}

function dedupeGaps(items: readonly Gap[]): readonly Gap[] {
  const seen = new Set<string>();
  const out: Gap[] = [];
  for (const item of items) {
    const key = stableSerializeValue(item);
    if (!seen.has(key)) {
      seen.add(key);
      out.push(item);
    }
  }
  return out;
}

const factExplainLocalGaps = new WeakMap<object, readonly ExplainLocalGap[]>();

function localGapSuperseded(
  upstream: ExplainLocalGap,
  explicit: ExplainLocalGap,
): boolean {
  if (
    stableSerializeValue(explicit.gap) === stableSerializeValue(upstream.gap) &&
    (explicit.field === undefined ||
      upstream.field === undefined ||
      explicit.field === upstream.field)
  ) {
    return true;
  }
  return (
    explicit.status === "resolved_by_override" &&
    explicit.field !== undefined &&
    explicit.field === upstream.field &&
    explicit.factTypeId === upstream.factTypeId &&
    explicit.providerId === upstream.providerId
  );
}

function collectContributionLocalGaps<
  SubjectKind extends ProgramObjectKind,
  Value,
>(contribution: FactContribution<SubjectKind, Value>): ExplainLocalGap[] {
  const explicit = contribution.explainTrace?.localGaps ?? [];
  const combined: ExplainLocalGap[] = [...explicit];
  for (const basisItem of contribution.basis ?? []) {
    if (typeof basisItem !== "object" || basisItem === null) {
      continue;
    }
    const upstreamLocalGaps = factExplainLocalGaps.get(basisItem);
    if (!upstreamLocalGaps) {
      continue;
    }
    for (const upstream of upstreamLocalGaps) {
      if (!combined.some((item) => localGapSuperseded(upstream, item))) {
        combined.push(upstream);
      }
    }
  }
  return combined;
}

type ProviderEntry = {
  readonly adapterId: string;
  readonly provider: FactProvider;
};

type SubjectAccumulator<SubjectKind extends ProgramObjectKind, Value> = {
  readonly subject: ProgramRef<SubjectKind>;
  providerId: string;
  providers: string[];
  value: Value;
  evidence: Evidence[];
  gaps: Gap[];
  localGaps: ExplainLocalGap[];
};

function buildFactConflictError<
  SubjectKind extends ProgramObjectKind,
  Value,
>(params: {
  readonly factTypeId: string;
  readonly projectId: string;
  readonly snapshotId: number;
  readonly subject: ProgramRef<SubjectKind>;
  readonly condition: unknown;
  readonly syntaxLocations: WeakMap<SyntaxRef, DiagnosticLocation>;
  readonly reason: string;
  readonly existingProviders: readonly string[];
  readonly existingValue: Value;
  readonly existingConclusionSummary?: string;
  readonly existingEvidence: readonly Evidence[];
  readonly incomingProviderId: string;
  readonly incomingValue: Value;
  readonly incomingConclusionSummary?: string;
  readonly incomingEvidence: readonly Evidence[];
}): TssaeError {
  const existingProviders = [...new Set(params.existingProviders)].toSorted(
    compareCodepoint,
  );
  const partyA: FactConflictParty = {
    providerId: existingProviders.join(","),
    providers: existingProviders,
    conclusion: params.existingValue,
    conclusionSummary:
      params.existingConclusionSummary ??
      stableSerializeValue(params.existingValue),
    evidence: dedupeEvidence(params.existingEvidence),
  };
  const partyB: FactConflictParty = {
    providerId: params.incomingProviderId,
    providers: [params.incomingProviderId],
    conclusion: params.incomingValue,
    conclusionSummary:
      params.incomingConclusionSummary ??
      stableSerializeValue(params.incomingValue),
    evidence: dedupeEvidence(params.incomingEvidence),
  };
  const parties: readonly [FactConflictParty, FactConflictParty] =
    compareCodepoint(partyA.providerId, partyB.providerId) <= 0
      ? [partyA, partyB]
      : [partyB, partyA];

  const subjectLocation =
    params.syntaxLocations.get(params.subject as SyntaxRef) ??
    parties[0].evidence.find((item) => item.at !== undefined)?.at ??
    parties[1].evidence.find((item) => item.at !== undefined)?.at;

  const conflict: FactConflictDetails = {
    factTypeId: params.factTypeId,
    projectId: params.projectId,
    snapshotId: params.snapshotId,
    subjectHandleId: params.subject.handleId,
    ...(subjectLocation !== undefined ? { subjectLocation } : {}),
    ...(params.condition !== undefined ? { condition: params.condition } : {}),
    reason: params.reason,
    parties,
  };

  const subjectLocSuffix = subjectLocation
    ? ` at ${subjectLocation.file}:${subjectLocation.line}:${subjectLocation.column}`
    : "";
  const message =
    `Fact conflict for "${params.factTypeId}" on "${params.subject.handleId}"${subjectLocSuffix} ` +
    `between [${parties[0].providerId}] and [${parties[1].providerId}]: ${params.reason}. ` +
    `[${parties[0].providerId}] concluded ${parties[0].conclusionSummary} (evidence: ${formatEvidenceList(parties[0].evidence)}). ` +
    `[${parties[1].providerId}] concluded ${parties[1].conclusionSummary} (evidence: ${formatEvidenceList(parties[1].evidence)}).`;

  return new TssaeError("FACT_CONFLICT", message, { conflict });
}

function mergeContributions<
  SubjectKind extends ProgramObjectKind,
  Condition,
  Value,
>(
  factType: FactType<SubjectKind, Condition, Value>,
  projectId: string,
  snapshotId: number,
  condition: Condition | undefined,
  syntaxLocations: WeakMap<SyntaxRef, DiagnosticLocation>,
  recordExplain: boolean,
  perProvider: readonly {
    readonly adapterId: string;
    readonly contributions: readonly FactContribution<SubjectKind, Value>[];
  }[],
): readonly FactResult<SubjectKind, Value>[] {
  const orderedIds: string[] = [];
  const bySubject = new Map<string, SubjectAccumulator<SubjectKind, Value>>();

  for (const entry of perProvider) {
    // Collapse multiple entries for the same subject within a single provider if any.
    for (const contribution of entry.contributions) {
      const handleId = contribution.subject.handleId;
      const incomingEvidence = [
        ...extractBasisEvidence(contribution.basis, contribution.evidence),
      ];
      const incomingGaps = [...(contribution.gaps ?? [])];
      const incomingLocalGaps = recordExplain
        ? collectContributionLocalGaps(contribution)
        : [];
      const existing = bySubject.get(handleId);

      if (!existing) {
        orderedIds.push(handleId);
        bySubject.set(handleId, {
          subject: contribution.subject,
          providerId: entry.adapterId,
          providers: [entry.adapterId],
          value: contribution.value,
          evidence: incomingEvidence,
          gaps: incomingGaps,
          localGaps: incomingLocalGaps,
        });
        continue;
      }

      if (factType.merge) {
        const outcome = factType.merge(
          {
            providerId: existing.providerId,
            providers: existing.providers,
            value: existing.value,
            evidence: existing.evidence,
            gaps: existing.gaps,
          },
          {
            providerId: entry.adapterId,
            providers: [entry.adapterId],
            value: contribution.value,
            evidence: incomingEvidence,
            gaps: incomingGaps,
          },
        );
        if (!outcome.ok) {
          const existingEv =
            outcome.conflict.existingEvidence ?? existing.evidence;
          const incomingEv =
            outcome.conflict.incomingEvidence ?? incomingEvidence;
          throw buildFactConflictError({
            factTypeId: factType.id,
            projectId,
            snapshotId,
            subject: contribution.subject,
            condition,
            syntaxLocations,
            reason: outcome.conflict.message,
            existingProviders: existing.providers,
            existingValue: existing.value,
            ...(outcome.conflict.existingConclusion !== undefined
              ? {
                  existingConclusionSummary:
                    outcome.conflict.existingConclusion,
                }
              : {}),
            existingEvidence: existingEv,
            incomingProviderId: entry.adapterId,
            incomingValue: contribution.value,
            ...(outcome.conflict.incomingConclusion !== undefined
              ? {
                  incomingConclusionSummary:
                    outcome.conflict.incomingConclusion,
                }
              : {}),
            incomingEvidence: incomingEv,
          });
        }
        existing.value = outcome.value;
        existing.providers = [
          ...new Set([...existing.providers, entry.adapterId]),
        ].toSorted(compareCodepoint);
        existing.providerId = existing.providers.join(",");
        existing.evidence = [
          ...dedupeEvidence([...existing.evidence, ...incomingEvidence]),
        ];
        existing.gaps = [...dedupeGaps([...existing.gaps, ...incomingGaps])];
        if (recordExplain && incomingLocalGaps.length > 0) {
          existing.localGaps.push(...incomingLocalGaps);
        }
      } else {
        const existingSerialized = stableSerializeValue(existing.value);
        const incomingSerialized = stableSerializeValue(contribution.value);
        if (existingSerialized !== incomingSerialized) {
          throw buildFactConflictError({
            factTypeId: factType.id,
            projectId,
            snapshotId,
            subject: contribution.subject,
            condition,
            syntaxLocations,
            reason: `providers produced differing definite conclusions (${existingSerialized} vs ${incomingSerialized})`,
            existingProviders: existing.providers,
            existingValue: existing.value,
            existingConclusionSummary: existingSerialized,
            existingEvidence: existing.evidence,
            incomingProviderId: entry.adapterId,
            incomingValue: contribution.value,
            incomingConclusionSummary: incomingSerialized,
            incomingEvidence,
          });
        }
        existing.providers = [
          ...new Set([...existing.providers, entry.adapterId]),
        ].toSorted(compareCodepoint);
        existing.providerId = existing.providers.join(",");
        existing.evidence = [
          ...dedupeEvidence([...existing.evidence, ...incomingEvidence]),
        ];
        existing.gaps = [...dedupeGaps([...existing.gaps, ...incomingGaps])];
        if (recordExplain && incomingLocalGaps.length > 0) {
          existing.localGaps.push(...incomingLocalGaps);
        }
      }
    }
  }

  return orderedIds.map((handleId) => {
    const acc = bySubject.get(handleId)!;
    let mergedLocalGaps: ExplainLocalGap[] = [];
    if (recordExplain) {
      const subjectLocation = syntaxLocations.get(acc.subject as SyntaxRef);
      const resolvedEntries = acc.localGaps.filter(
        (item) => item.status === "resolved_by_override",
      );
      const seen = new Set<string>();
      for (const item of acc.localGaps) {
        if (
          item.status === "unresolved" &&
          resolvedEntries.some((resolved) => localGapSuperseded(item, resolved))
        ) {
          continue;
        }
        const key = stableSerializeValue(item);
        if (!seen.has(key)) {
          seen.add(key);
          mergedLocalGaps.push(item);
        }
      }
      for (const gap of acc.gaps) {
        const alreadyTracked = mergedLocalGaps.some(
          (item) =>
            item.status === "unresolved" &&
            stableSerializeValue(item.gap) === stableSerializeValue(gap),
        );
        if (!alreadyTracked) {
          mergedLocalGaps.push({
            status: "unresolved",
            factTypeId: factType.id,
            providerId: acc.providers.join(","),
            gap,
            ...(subjectLocation !== undefined ? { subjectLocation } : {}),
            impact: "Remained unresolved during fact evaluation.",
          });
        }
      }
    }
    const fact: FactResult<SubjectKind, Value> = {
      factTypeId: factType.id,
      projectId,
      snapshotId,
      subject: acc.subject,
      value: acc.value,
      evidence: acc.evidence,
      gaps: acc.gaps,
      providers: acc.providers,
      ...(recordExplain && mergedLocalGaps.length > 0
        ? { explainTrace: { localGaps: mergedLocalGaps } }
        : {}),
    };
    if (recordExplain && mergedLocalGaps.length > 0) {
      factExplainLocalGaps.set(fact, mergedLocalGaps);
    }
    return registerProvenance(fact, acc.evidence);
  });
}

export type CachedAdapterAdditionalInput = {
  readonly adapterId: string;
  readonly filePath: string;
};

export type EngineNativeSubQueryTrace = {
  readonly factTypeId: string;
  readonly providers: readonly string[];
  readonly cacheStatus: "computed" | "reused";
  readonly candidateCount: number;
  readonly durationMs: number;
  readonly subjectLocations: readonly DiagnosticLocation[];
};

export type ActiveFactQueryFrame = {
  readonly adapterId: string;
  readonly factTypeId: string;
  readonly targetHandleIds: readonly string[] | undefined;
};

type CachedSubProviderTrace = {
  readonly providerId: string;
  readonly factTypeId: string;
};

type CachedFullEntry = {
  readonly results: readonly FactResult[];
  readonly additionalInputs: readonly CachedAdapterAdditionalInput[];
  readonly engineSubQueries: readonly EngineNativeSubQueryTrace[];
  readonly subProviders: readonly CachedSubProviderTrace[];
};

type CachedTargetEntry = {
  readonly item: FactResult | null;
  readonly additionalInputs: readonly CachedAdapterAdditionalInput[];
  readonly engineSubQueries: readonly EngineNativeSubQueryTrace[];
  readonly subProviders: readonly CachedSubProviderTrace[];
};

/** Per-session cache storing computed fact results across rules for the active snapshot. */
export class FactCache {
  readonly #fullResults = new Map<string, Promise<CachedFullEntry>>();
  readonly #fullLoadedKeys = new Set<string>();
  readonly #targetResults = new Map<string, Promise<CachedTargetEntry>>();

  clear(): void {
    this.#fullResults.clear();
    this.#fullLoadedKeys.clear();
    this.#targetResults.clear();
  }

  hasFull(key: string): boolean {
    return this.#fullResults.has(key);
  }

  getFull(key: string): Promise<CachedFullEntry> | undefined {
    return this.#fullResults.get(key);
  }

  setFull(key: string, promise: Promise<CachedFullEntry>): void {
    this.#fullResults.set(key, promise);
  }

  deleteFull(key: string): void {
    this.#fullResults.delete(key);
    this.#fullLoadedKeys.delete(key);
  }

  markFullLoaded(key: string): void {
    this.#fullLoadedKeys.add(key);
  }

  isFullLoaded(key: string): boolean {
    return this.#fullLoadedKeys.has(key);
  }

  getTarget(
    baseKey: string,
    handleId: string,
  ): Promise<CachedTargetEntry> | undefined {
    return this.#targetResults.get(`${baseKey}::${handleId}`);
  }

  setTarget(
    baseKey: string,
    handleId: string,
    promise: Promise<CachedTargetEntry>,
  ): void {
    this.#targetResults.set(`${baseKey}::${handleId}`, promise);
  }

  deleteTarget(baseKey: string, handleId: string): void {
    this.#targetResults.delete(`${baseKey}::${handleId}`);
  }
}

export async function executeFactQuery<
  SubjectKind extends ProgramObjectKind,
  Condition,
  Value,
>(params: {
  readonly factType: FactType<SubjectKind, Condition, Value>;
  readonly options: FactQueryOptions<SubjectKind, Condition> | undefined;
  readonly adapters: readonly LibraryAdapter[];
  readonly factCache: FactCache;
  readonly sessionToken: symbol;
  readonly snapshotId: number;
  readonly projectId: string;
  readonly ruleId: string;
  readonly assertActiveSnapshot: () => void;
  readonly createAdapterContext: (
    adapterId: string,
    onAdditionalInput: (filePath: string, sourceAdapterId?: string) => void,
    facts: ProjectContext["facts"],
    onEngineSubQuery: (trace: EngineNativeSubQueryTrace) => void,
  ) => AdapterContext;
  readonly replayAdapterAdditionalInput: (
    adapterId: string,
    filePath: string,
  ) => void;
  readonly recordGaps: (gaps: readonly Gap[]) => void;
  readonly collector: ObservationCollector;
  readonly syntaxLocations: WeakMap<SyntaxRef, DiagnosticLocation>;
  readonly activeQueryStack?: readonly ActiveFactQueryFrame[];
  readonly onTransitiveAdditionalInput?: (
    adapterId: string,
    filePath: string,
  ) => void;
  readonly onTransitiveSubProvider?: (trace: CachedSubProviderTrace) => void;
}): Promise<readonly FactResult<SubjectKind, Value>[]> {
  const {
    factType,
    options,
    adapters,
    factCache,
    sessionToken,
    snapshotId,
    projectId,
    ruleId,
    assertActiveSnapshot,
    createAdapterContext,
    replayAdapterAdditionalInput,
    recordGaps,
    collector,
    syntaxLocations,
    activeQueryStack = [],
    onTransitiveAdditionalInput,
    onTransitiveSubProvider,
  } = params;

  assertActiveSnapshot();

  if (options?.providerId !== undefined && options.providerId.trim() === "") {
    throw new TssaeError(
      "INVALID_CONFIG",
      "Fact query providerId must be a non-empty string when specified.",
    );
  }

  const targets = options?.targets;
  if (targets !== undefined) {
    if (targets.length === 0) {
      return [];
    }
    validateProgramRefs(targets, sessionToken, snapshotId, projectId);
  }

  const matchedProviders: ProviderEntry[] = [];
  let filteredAdapterFound = false;
  for (const adapter of adapters) {
    if (options?.providerId !== undefined) {
      if (adapter.id !== options.providerId) {
        continue;
      }
      filteredAdapterFound = true;
    }
    const found = adapter.provides.find(
      (provider) => provider.factType.id === factType.id,
    );
    if (found) {
      matchedProviders.push({ adapterId: adapter.id, provider: found });
    }
  }
  matchedProviders.sort((a, b) => compareCodepoint(a.adapterId, b.adapterId));

  const requestedHandles = targets?.map((target) => target.handleId);
  for (const entry of matchedProviders) {
    const hasCycle = activeQueryStack.some(
      (frame) =>
        frame.adapterId === entry.adapterId &&
        frame.factTypeId === factType.id &&
        (frame.targetHandleIds === undefined ||
          requestedHandles === undefined ||
          requestedHandles.some((handleId) =>
            frame.targetHandleIds!.includes(handleId),
          )),
    );
    if (hasCycle) {
      const chain = [
        ...activeQueryStack.map((f) => `${f.adapterId}(${f.factTypeId})`),
        `${entry.adapterId}(${factType.id})`,
      ].join(" -> ");
      throw new TssaeError(
        "INVALID_QUERY",
        `Cyclic fact query detected: ${chain}`,
      );
    }
  }

  const providerFilterStatus:
    | "matched"
    | "not_enabled"
    | "unsupported"
    | undefined =
    options?.providerId !== undefined
      ? matchedProviders.length > 0
        ? "matched"
        : filteredAdapterFound
          ? "unsupported"
          : "not_enabled"
      : undefined;

  const providersUsed = matchedProviders.map((entry) => entry.adapterId);
  const providerLabel =
    providersUsed.length === 1
      ? providersUsed[0]!
      : providersUsed.length > 1
        ? providersUsed.join(",")
        : "none";

  const explainEnabled = options?.explain ?? collector.isExplainEnabled();
  const providerFilterKey = options?.providerId ?? "*";
  const conditionKey = stableSerializeValue(options?.condition ?? null);
  const baseKey = JSON.stringify([
    snapshotId,
    projectId,
    factType.id,
    providerFilterKey,
    conditionKey,
    explainEnabled ? "explain" : "check",
  ]);

  const queryStart = collector.isActive() ? performance.now() : 0;
  let cacheStatus: "computed" | "reused";
  let results: readonly FactResult<SubjectKind, Value>[];
  let engineSubQueries: readonly EngineNativeSubQueryTrace[] = [];
  let replayedSubProviders: readonly CachedSubProviderTrace[] = [];

  const replayInputs = (
    inputs: readonly CachedAdapterAdditionalInput[],
  ): void => {
    const seen = new Set<string>();
    for (const input of inputs) {
      const key = `${input.adapterId}\0${input.filePath}`;
      if (!seen.has(key)) {
        seen.add(key);
        replayAdapterAdditionalInput(input.adapterId, input.filePath);
        onTransitiveAdditionalInput?.(input.adapterId, input.filePath);
      }
    }
  };

  const runExtraction = async (
    requestedTargets: readonly ProgramRef<SubjectKind>[] | undefined,
  ): Promise<{
    readonly results: readonly FactResult<SubjectKind, Value>[];
    readonly additionalInputs: readonly CachedAdapterAdditionalInput[];
    readonly engineSubQueries: readonly EngineNativeSubQueryTrace[];
    readonly subProviders: readonly CachedSubProviderTrace[];
  }> => {
    if (matchedProviders.length === 0) {
      return {
        results: [],
        additionalInputs: [],
        engineSubQueries: [],
        subProviders: [],
      };
    }
    const recordedInputs: CachedAdapterAdditionalInput[] = [];
    const seenRecorded = new Set<string>();
    const recordedEngineSubQueries: EngineNativeSubQueryTrace[] = [];
    const recordedSubProviders: CachedSubProviderTrace[] = [];
    const seenSubProviders = new Set<string>();
    const recordSubProvider = (trace: CachedSubProviderTrace): void => {
      const key = `${trace.providerId}\0${trace.factTypeId}`;
      if (!seenSubProviders.has(key)) {
        seenSubProviders.add(key);
        recordedSubProviders.push(trace);
      }
      onTransitiveSubProvider?.(trace);
    };
    const request: FactExtractionRequest<SubjectKind, Condition> = {
      factType: factType,
      explain: explainEnabled,
      ...(requestedTargets !== undefined ? { targets: requestedTargets } : {}),
      ...(options?.condition !== undefined
        ? { condition: options.condition }
        : {}),
    };
    const perProvider: {
      readonly adapterId: string;
      readonly contributions: readonly FactContribution<SubjectKind, Value>[];
    }[] = [];

    for (const entry of matchedProviders) {
      const recordInput = (
        filePath: string,
        sourceAdapterId?: string,
      ): void => {
        const attributedId = sourceAdapterId ?? entry.adapterId;
        const key = `${attributedId}\0${filePath}`;
        if (!seenRecorded.has(key)) {
          seenRecorded.add(key);
          recordedInputs.push({ adapterId: attributedId, filePath });
        }
        onTransitiveAdditionalInput?.(attributedId, filePath);
      };
      const nextStack: readonly ActiveFactQueryFrame[] = [
        ...activeQueryStack,
        {
          adapterId: entry.adapterId,
          factTypeId: factType.id,
          targetHandleIds: requestedTargets?.map((target) => target.handleId),
        },
      ];
      const nestedFacts: ProjectContext["facts"] = <
        SubSubjectKind extends ProgramObjectKind,
        SubCondition,
        SubValue,
      >(
        subFactType: FactType<SubSubjectKind, SubCondition, SubValue>,
        subOptions?: FactQueryOptions<SubSubjectKind, SubCondition>,
      ) =>
        executeFactQuery({
          factType: subFactType,
          options:
            subOptions?.explain !== undefined || !explainEnabled
              ? subOptions
              : { ...subOptions, explain: true },
          adapters,
          factCache,
          sessionToken,
          snapshotId,
          projectId,
          ruleId,
          assertActiveSnapshot,
          createAdapterContext,
          replayAdapterAdditionalInput,
          recordGaps: () => {},
          collector,
          syntaxLocations,
          activeQueryStack: nextStack,
          onTransitiveAdditionalInput: (srcAdapterId, filePath) => {
            recordInput(filePath, srcAdapterId);
          },
          onTransitiveSubProvider: (trace) => {
            recordSubProvider(trace);
          },
        });
      const adapterCtx = createAdapterContext(
        entry.adapterId,
        recordInput,
        nestedFacts,
        (trace) => {
          recordedEngineSubQueries.push(trace);
        },
      );
      const rawContributions = (await entry.provider.extract(
        adapterCtx,
        request,
      )) as readonly FactContribution<SubjectKind, Value>[];
      validateProgramRefs(
        rawContributions.map((item) => item.subject),
        sessionToken,
        snapshotId,
        projectId,
      );
      perProvider.push({
        adapterId: entry.adapterId,
        contributions: rawContributions,
      });
    }

    return {
      results: mergeContributions(
        factType,
        projectId,
        snapshotId,
        options?.condition,
        syntaxLocations,
        explainEnabled,
        perProvider,
      ),
      additionalInputs: recordedInputs,
      engineSubQueries: recordedEngineSubQueries,
      subProviders: recordedSubProviders,
    };
  };

  const extractMissingTargets = async (
    missingTargets: readonly ProgramRef<SubjectKind>[],
  ): Promise<void> => {
    const batchPromise = runExtraction(missingTargets);
    const itemPromises = [];
    for (const target of missingTargets) {
      const itemPromise = batchPromise.then(
        (computed) => ({
          item:
            computed.results.find(
              (item) => item.subject.handleId === target.handleId,
            ) ?? null,
          additionalInputs: computed.additionalInputs,
          engineSubQueries: computed.engineSubQueries,
          subProviders: computed.subProviders,
        }),
        (err: unknown) => {
          factCache.deleteTarget(baseKey, target.handleId);
          throw err;
        },
      );
      factCache.setTarget(baseKey, target.handleId, itemPromise);
      itemPromises.push(itemPromise);
    }
    await Promise.all(itemPromises);
  };

  if (targets === undefined) {
    const cachedFull = factCache.getFull(baseKey);
    if (cachedFull) {
      cacheStatus = "reused";
      const loaded = await cachedFull;
      replayInputs(loaded.additionalInputs);
      results = loaded.results as readonly FactResult<SubjectKind, Value>[];
      engineSubQueries = loaded.engineSubQueries.map((sub) => ({
        ...sub,
        cacheStatus: "reused",
      }));
      replayedSubProviders = loaded.subProviders;
    } else {
      cacheStatus = "computed";
      const fullPromise = runExtraction(undefined).then(
        (computed) => {
          factCache.markFullLoaded(baseKey);
          for (const item of computed.results) {
            factCache.setTarget(
              baseKey,
              item.subject.handleId,
              Promise.resolve({
                item,
                additionalInputs: computed.additionalInputs,
                engineSubQueries: computed.engineSubQueries,
                subProviders: computed.subProviders,
              }),
            );
          }
          return computed;
        },
        (err: unknown) => {
          factCache.deleteFull(baseKey);
          throw err;
        },
      );
      factCache.setFull(baseKey, fullPromise);
      const computed = await fullPromise;
      results = computed.results;
      engineSubQueries = computed.engineSubQueries;
    }
  } else if (factCache.isFullLoaded(baseKey)) {
    const loaded = await factCache.getFull(baseKey)!;
    const allResults = loaded.results as readonly FactResult<
      SubjectKind,
      Value
    >[];
    const byHandle = new Map(
      allResults.map((item) => [item.subject.handleId, item]),
    );
    const missingTargets = targets.filter(
      (target) =>
        !byHandle.has(target.handleId) &&
        !factCache.getTarget(baseKey, target.handleId),
    );
    cacheStatus = missingTargets.length === 0 ? "reused" : "computed";
    if (missingTargets.length > 0) {
      await extractMissingTargets(missingTargets);
    }
    const resolved = await Promise.all(
      targets.map(async (target) => {
        const found = byHandle.get(target.handleId);
        if (found !== undefined) {
          return {
            item: found,
            additionalInputs: loaded.additionalInputs,
            engineSubQueries: loaded.engineSubQueries.map((sub) => ({
              ...sub,
              cacheStatus: "reused" as const,
            })),
            subProviders: loaded.subProviders,
          };
        }
        return factCache.getTarget(baseKey, target.handleId)!;
      }),
    );
    replayInputs(resolved.flatMap((entry) => entry.additionalInputs));
    engineSubQueries = resolved.flatMap((entry) => entry.engineSubQueries);
    if (cacheStatus === "reused") {
      replayedSubProviders = resolved.flatMap((entry) => entry.subProviders);
    }
    results = resolved
      .map((entry) => entry.item)
      .filter((item): item is FactResult<SubjectKind, Value> => item !== null);
  } else {
    const missingTargets: ProgramRef<SubjectKind>[] = [];
    const seenMissing = new Set<string>();
    for (const target of targets) {
      if (
        !factCache.getTarget(baseKey, target.handleId) &&
        !seenMissing.has(target.handleId)
      ) {
        seenMissing.add(target.handleId);
        missingTargets.push(target);
      }
    }

    if (missingTargets.length === 0) {
      cacheStatus = "reused";
    } else {
      cacheStatus = "computed";
      await extractMissingTargets(missingTargets);
    }

    const resolved = await Promise.all(
      targets.map((target) => factCache.getTarget(baseKey, target.handleId)!),
    );
    replayInputs(resolved.flatMap((entry) => entry.additionalInputs));
    engineSubQueries = resolved.flatMap((entry) => entry.engineSubQueries);
    if (cacheStatus === "reused") {
      replayedSubProviders = resolved.flatMap((entry) => entry.subProviders);
    }
    results = resolved
      .map((entry) => entry.item)
      .filter((item): item is FactResult<SubjectKind, Value> => item !== null);
  }

  if (activeQueryStack.length === 0) {
    for (const item of results) {
      if (item.gaps.length > 0) {
        recordGaps(item.gaps);
      }
    }
  }

  if (collector.isActive()) {
    const durationMs = performance.now() - queryStart;
    const explainTarget = collector.explainTarget;
    for (const sub of engineSubQueries) {
      const subMatchedLocations = explainTarget
        ? sub.subjectLocations.filter((loc) =>
            locationMatchesTarget(loc, explainTarget),
          )
        : undefined;
      const subProviderLabel =
        sub.providers.length > 0 ? sub.providers.join(",") : "none";
      if (explainEnabled) {
        for (const pid of sub.providers) {
          collector.recordProviderEvaluation({
            ruleId,
            evaluation: {
              providerId: pid,
              factTypeId: sub.factTypeId,
              status: sub.cacheStatus === "reused" ? "reused" : "evaluated",
              ...(sub.subjectLocations[0] !== undefined
                ? { subjectLocation: sub.subjectLocations[0] }
                : {}),
            },
          });
        }
      }
      collector.recordQuery({
        ruleId,
        op: `facts:${sub.factTypeId}`,
        projectId,
        provider: subProviderLabel,
        providers: sub.providers,
        cacheStatus: sub.cacheStatus,
        candidateCount: sub.candidateCount,
        durationMs: sub.durationMs,
        summary: `facts(${sub.factTypeId}) [${subProviderLabel}] -> ${sub.candidateCount} result(s)`,
        ...(subMatchedLocations !== undefined
          ? { matchedLocations: subMatchedLocations }
          : {}),
        ...(collector.mode === "detail" || collector.isExplainEnabled()
          ? {
              detail: {
                factTypeId: sub.factTypeId,
                providers: sub.providers,
                candidateCount: sub.candidateCount,
              },
            }
          : {}),
      });
    }

    if (explainEnabled) {
      for (const sub of replayedSubProviders) {
        onTransitiveSubProvider?.(sub);
        collector.recordProviderEvaluation({
          ruleId,
          evaluation: {
            providerId: sub.providerId,
            factTypeId: sub.factTypeId,
            status: "reused",
          },
        });
      }
      for (const entry of matchedProviders) {
        onTransitiveSubProvider?.({
          providerId: entry.adapterId,
          factTypeId: factType.id,
        });
        collector.recordProviderEvaluation({
          ruleId,
          evaluation: {
            providerId: entry.adapterId,
            factTypeId: factType.id,
            status: cacheStatus === "reused" ? "reused" : "evaluated",
          },
        });
      }
      if (options?.providerId !== undefined) {
        for (const adapter of adapters) {
          if (
            adapter.id !== options.providerId &&
            adapter.provides.some((p) => p.factType.id === factType.id)
          ) {
            collector.recordProviderEvaluation({
              ruleId,
              evaluation: {
                providerId: adapter.id,
                factTypeId: factType.id,
                status: "skipped",
                reason: `Skipped due to providerId filter ("${options.providerId}").`,
              },
            });
          }
        }
      }
      if (activeQueryStack.length === 0) {
        for (const item of results) {
          const subjectLocation = syntaxLocations.get(
            item.subject as SyntaxRef,
          );
          const localGaps = factExplainLocalGaps.get(item) ?? [];
          if (item.gaps.length > 0 || localGaps.length > 0) {
            collector.recordFactExplain({
              ruleId,
              ...(subjectLocation !== undefined ? { subjectLocation } : {}),
              gaps: item.gaps,
              localGaps,
            });
          }
        }
      }
    }

    const matchedLocations: DiagnosticLocation[] | undefined = explainTarget
      ? collectMatchedFactLocations(results, explainTarget, syntaxLocations)
      : undefined;

    const filterSummarySuffix =
      providerFilterStatus === "not_enabled"
        ? ` (providerFilter=${options!.providerId}: not enabled)`
        : providerFilterStatus === "unsupported"
          ? ` (providerFilter=${options!.providerId}: factType unsupported)`
          : "";

    collector.recordQuery({
      ruleId,
      op: `facts:${factType.id}`,
      projectId,
      provider: providerLabel,
      providers: providersUsed,
      cacheStatus,
      candidateCount: results.length,
      durationMs,
      summary: `facts(${factType.id}) [${providerLabel}]${filterSummarySuffix} -> ${results.length} result(s)`,
      ...(matchedLocations !== undefined ? { matchedLocations } : {}),
      ...(collector.mode === "detail" || collector.isExplainEnabled()
        ? {
            detail: {
              factTypeId: factType.id,
              providers: providersUsed,
              ...(options?.providerId !== undefined
                ? {
                    providerFilter: options.providerId,
                    providerFilterStatus,
                  }
                : {}),
              candidateCount: results.length,
            },
          }
        : {}),
    });
  }

  return results;
}

function collectMatchedFactLocations<
  SubjectKind extends ProgramObjectKind,
  Value,
>(
  results: readonly FactResult<SubjectKind, Value>[],
  explainTarget: NonNullable<ObservationCollector["explainTarget"]>,
  syntaxLocations: WeakMap<SyntaxRef, DiagnosticLocation>,
): DiagnosticLocation[] {
  const matched: DiagnosticLocation[] = [];
  for (const result of results) {
    const subjectLoc = syntaxLocations.get(result.subject as SyntaxRef);
    if (subjectLoc && locationMatchesTarget(subjectLoc, explainTarget)) {
      matched.push(subjectLoc);
      continue;
    }
    const evidenceLoc = result.evidence.find(
      (ev) => ev.at && locationMatchesTarget(ev.at, explainTarget),
    )?.at;
    if (evidenceLoc) {
      matched.push(evidenceLoc);
      continue;
    }
    const gapLoc = result.gaps.find(
      (gap) => gap.at && locationMatchesTarget(gap.at, explainTarget),
    )?.at;
    if (gapLoc) {
      matched.push(gapLoc);
      continue;
    }
    const localGaps = factExplainLocalGaps.get(result) ?? [];
    const localGapMatch = localGaps.find(
      (lg) =>
        (lg.gap.at && locationMatchesTarget(lg.gap.at, explainTarget)) ||
        (lg.subjectLocation &&
          locationMatchesTarget(lg.subjectLocation, explainTarget)) ||
        (lg.resolvedBy?.at &&
          locationMatchesTarget(lg.resolvedBy.at, explainTarget)),
    );
    if (localGapMatch) {
      const loc =
        (localGapMatch.resolvedBy?.at &&
        locationMatchesTarget(localGapMatch.resolvedBy.at, explainTarget)
          ? localGapMatch.resolvedBy.at
          : undefined) ??
        (localGapMatch.subjectLocation &&
        locationMatchesTarget(localGapMatch.subjectLocation, explainTarget)
          ? localGapMatch.subjectLocation
          : undefined) ??
        localGapMatch.gap.at;
      if (loc) {
        matched.push(loc);
      }
    }
  }
  return matched;
}
