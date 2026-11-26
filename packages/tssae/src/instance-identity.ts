import { performance } from "node:perf_hooks";

import type { EngineClient, RpcRequestOptions } from "./engine-client.ts";
import { registerProvenance } from "./evidence.ts";
import { normalizeLookupFiles } from "./lookup-files.ts";
import { validateProgramRefs, type SelectedKeys } from "./projection.ts";
import type { SummaryDiscoverySource } from "./summaries.ts";
import {
  TssaeError,
  makeProgramRef,
  type CallRef,
  type DeclarationRef,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type LookupFilesOptions,
  type InstanceRef,
  type SyntaxRef,
} from "./types.ts";

/**
 * Matching predicate for associating an external `.d.ts` callable declaration
 * with a project `Model` instance behavior rule.
 */
export type InstanceModelMatch = {
  /** Exact symbol or qualified member name (e.g. `"createExternal"`, `"Event.once"`, `"IMenuService.createMenu"`). */
  readonly symbol?: string;
  /** Enclosing class, interface, or namespace name (e.g. `"IMenuService"`, `"DisposableStore"`). */
  readonly container?: string;
  /** Method or function name (e.g. `"createMenu"`, `"_register"`, `"add"`, `"once"`). */
  readonly member?: string;
  /** Optional package name or normalized path segment scoping matching declarations. */
  readonly module?: string;
};

/**
 * Declares how an external declaration without a source body creates or forwards resource instances.
 */
export type InstanceModelBehavior =
  | {
      readonly kind: "allocates-fresh-instance";
      readonly match: InstanceModelMatch;
      readonly description?: string;
    }
  | {
      readonly kind: "forwards-argument";
      readonly match: InstanceModelMatch;
      /** 0-based argument index whose instance identity is returned. Defaults to `0`. */
      readonly argumentIndex?: number;
      readonly description?: string;
    };

export type InstanceTargetRef = SyntaxRef | DeclarationRef | CallRef;

export type InstanceOriginKind =
  | "allocation"
  | "summary-allocation"
  | "parameter";

export type ResolvedInstanceInfo = {
  readonly ref: InstanceRef;
  readonly instanceKey: string;
  readonly originKind: InstanceOriginKind;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly originSyntax: SyntaxRef | null;
  readonly summaryId: string | null;
  readonly evidence: readonly Evidence[];
};

export type InstanceResolutionResult = {
  readonly target: InstanceTargetRef;
} & (
  | {
      readonly status: "proved";
      readonly instance: InstanceRef | null;
      readonly instanceKey: string | null;
      readonly allocationLocation: DiagnosticLocation | null;
      readonly originKind: InstanceOriginKind | null;
      readonly instances: readonly [
        ResolvedInstanceInfo,
        ...ResolvedInstanceInfo[],
      ];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "disproved";
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "inconclusive";
      readonly instances: readonly ResolvedInstanceInfo[];
      readonly gaps: readonly Gap[];
      readonly evidence: readonly Evidence[];
    }
);

export type InstanceProjectionSelection = {
  readonly instanceKey?: true;
  readonly originKind?: true;
  readonly allocationLocation?: true;
  readonly originSyntax?: true;
  readonly summaryId?: true;
  readonly evidence?: true;
};

export type FullInstanceProjectionFields = {
  readonly instanceKey: string;
  readonly originKind: InstanceOriginKind;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly originSyntax: SyntaxRef | null;
  readonly summaryId: string | null;
  readonly evidence: readonly Evidence[];
};

export type ProjectedInstanceRow<
  S extends InstanceProjectionSelection = Record<never, never>,
> = {
  readonly instance: InstanceRef;
  readonly gaps: readonly Gap[];
} & Pick<
  FullInstanceProjectionFields,
  Extract<SelectedKeys<S>, keyof FullInstanceProjectionFields>
>;

type RawResolvedInstanceInfo = {
  readonly ref: string;
  readonly instanceKey: string;
  readonly originKind: InstanceOriginKind;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly originSyntax: string | null;
  readonly summaryId: string | null;
  readonly summarySource?: SummaryDiscoverySource;
  readonly evidence: readonly Evidence[];
};

type RawInstanceResolutionRow =
  | {
      readonly status: "proved";
      readonly target: string;
      readonly instance: string | null;
      readonly instanceKey: string | null;
      readonly allocationLocation: DiagnosticLocation | null;
      readonly originKind: InstanceOriginKind | null;
      readonly instances: readonly RawResolvedInstanceInfo[];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "disproved";
      readonly target: string;
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "inconclusive";
      readonly target: string;
      readonly instances: readonly RawResolvedInstanceInfo[];
      readonly gaps: readonly Gap[];
      readonly evidence: readonly Evidence[];
    };

type RawInstanceProjectionRow = {
  readonly instance: string;
  readonly gaps: readonly Gap[];
  readonly instanceKey?: string;
  readonly originKind?: InstanceOriginKind;
  readonly allocationLocation?: DiagnosticLocation | null;
  readonly originSyntax?: string | null;
  readonly summaryId?: string | null;
  readonly summarySource?: SummaryDiscoverySource;
  readonly evidence?: readonly Evidence[];
};

type EngineBatchResult = {
  readonly results: readonly {
    readonly items: readonly unknown[];
  }[];
};

function assertNonEmptyOptionalString(
  value: unknown,
  field: string,
  modelId: string,
): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || value.trim() === "") {
    throw new TssaeError(
      "INVALID_CONFIG",
      `Model "${modelId}" match.${field} must be a non-empty string when provided.`,
    );
  }
  return value;
}

/** Validates `instanceBehaviors` entries declared on a `Model`. */
export function validateInstanceBehaviors(
  modelId: string,
  behaviors: readonly InstanceModelBehavior[] | undefined,
): void {
  if (behaviors === undefined) return;
  if (!Array.isArray(behaviors)) {
    throw new TssaeError(
      "INVALID_CONFIG",
      `Model "${modelId}" instanceBehaviors must be an array when provided.`,
    );
  }

  for (const [index, behavior] of behaviors.entries()) {
    if (
      typeof behavior !== "object" ||
      behavior === null ||
      (behavior.kind !== "allocates-fresh-instance" &&
        behavior.kind !== "forwards-argument")
    ) {
      throw new TssaeError(
        "INVALID_CONFIG",
        `Model "${modelId}" instanceBehaviors[${String(index)}] has invalid kind.`,
      );
    }

    const match = behavior.match;
    if (typeof match !== "object" || match === null) {
      throw new TssaeError(
        "INVALID_CONFIG",
        `Model "${modelId}" instanceBehaviors[${String(index)}].match must be an object.`,
      );
    }

    const symbol = assertNonEmptyOptionalString(
      match.symbol,
      "symbol",
      modelId,
    );
    const container = assertNonEmptyOptionalString(
      match.container,
      "container",
      modelId,
    );
    const member = assertNonEmptyOptionalString(
      match.member,
      "member",
      modelId,
    );
    assertNonEmptyOptionalString(match.module, "module", modelId);

    if (
      symbol === undefined &&
      container === undefined &&
      member === undefined
    ) {
      throw new TssaeError(
        "INVALID_CONFIG",
        `Model "${modelId}" instanceBehaviors[${String(index)}].match must specify at least one of symbol, container, or member.`,
      );
    }

    if (
      behavior.description !== undefined &&
      typeof behavior.description !== "string"
    ) {
      throw new TssaeError(
        "INVALID_CONFIG",
        `Model "${modelId}" instanceBehaviors[${String(index)}].description must be a string.`,
      );
    }

    if (behavior.kind === "forwards-argument") {
      if (
        behavior.argumentIndex !== undefined &&
        (!Number.isInteger(behavior.argumentIndex) ||
          behavior.argumentIndex < 0)
      ) {
        throw new TssaeError(
          "INVALID_CONFIG",
          `Model "${modelId}" instanceBehaviors[${String(index)}].argumentIndex must be a non-negative integer.`,
        );
      }
    }
  }
}

/** Returns true when two `InstanceRef` handles refer to the same canonical resource instance in the same snapshot and project. */
export function isSameInstance(
  a: InstanceRef | null | undefined,
  b: InstanceRef | null | undefined,
): boolean {
  if (a == null || b == null) return false;
  return (
    a.projectId === b.projectId &&
    a.snapshotId === b.snapshotId &&
    a.handleId === b.handleId
  );
}

export type InstanceIdentityCache = {
  readonly resolutionCache: Map<string, Promise<InstanceResolutionResult>>;
  clear: () => void;
};

export function createInstanceIdentityCache(): InstanceIdentityCache {
  const resolutionCache = new Map<string, Promise<InstanceResolutionResult>>();
  return {
    resolutionCache,
    clear: () => {
      resolutionCache.clear();
    },
  };
}

function materializeResolvedInstances(
  rawInstances: readonly RawResolvedInstanceInfo[],
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
): ResolvedInstanceInfo[] {
  return rawInstances.map((entry) =>
    registerProvenance(
      {
        ref: makeProgramRef(
          "instance",
          sessionToken,
          projectId,
          snapshotId,
          entry.ref,
        ),
        instanceKey: entry.instanceKey,
        originKind: entry.originKind,
        allocationLocation: entry.allocationLocation,
        originSyntax:
          entry.originSyntax !== null
            ? makeProgramRef(
                "syntax",
                sessionToken,
                projectId,
                snapshotId,
                entry.originSyntax,
              )
            : null,
        summaryId: entry.summaryId,
        evidence: entry.evidence,
      },
      entry.evidence,
    ),
  );
}

/** Creates instance-identity resolution and projection query methods bound to an active snapshot and project. */
export function createInstanceIdentityQueries(deps: {
  readonly client: EngineClient;
  readonly cache: InstanceIdentityCache;
  readonly sessionToken: symbol;
  readonly snapshotId: number;
  readonly projectId: string;
  readonly cwd: string;
  readonly sourceFiles: readonly string[];
  readonly assertOpen: () => void;
  readonly recordGaps: (gaps: readonly Gap[]) => void;
  readonly getRpcOptions?: () => RpcRequestOptions | undefined;
  readonly onQueryCompleted?: (
    op: string,
    cacheStatus: "computed" | "reused",
    candidateCount: number,
    durationMs: number,
  ) => void;
}) {
  const {
    client,
    cache,
    sessionToken,
    snapshotId,
    projectId,
    cwd,
    sourceFiles,
    assertOpen,
    recordGaps,
    getRpcOptions,
    onQueryCompleted,
  } = deps;

  return {
    resolveInstances: async (
      targets: readonly InstanceTargetRef[],
      options?: LookupFilesOptions,
    ): Promise<readonly InstanceResolutionResult[]> => {
      assertOpen();
      const lookupFiles = normalizeLookupFiles(
        options?.lookupFiles,
        cwd,
        sourceFiles,
      );
      if (targets.length === 0) return [];
      validateProgramRefs(targets, sessionToken, snapshotId, projectId);

      const lookupScopeKey = JSON.stringify(lookupFiles ?? null);

      const missing: InstanceTargetRef[] = [];
      const seenMissing = new Set<string>();
      for (const target of targets) {
        const key = `${snapshotId}:${projectId}:${lookupScopeKey}:${target.handleId}`;
        if (!cache.resolutionCache.has(key) && !seenMissing.has(key)) {
          seenMissing.add(key);
          missing.push(target);
        }
      }

      const start = onQueryCompleted ? performance.now() : 0;
      const cacheStatus: "computed" | "reused" =
        missing.length > 0 ? "computed" : "reused";

      if (missing.length > 0) {
        const batchPromise = client.request<EngineBatchResult>(
          "query.batch",
          {
            snapshotId,
            projectId,
            steps: [
              {
                op: "resolveInstances",
                targets: missing.map((t) => t.handleId),
                ...(lookupFiles !== undefined ? { lookupFiles } : {}),
              },
            ],
          },
          getRpcOptions?.(),
        );

        for (const [index, target] of missing.entries()) {
          const key = `${snapshotId}:${projectId}:${lookupScopeKey}:${target.handleId}`;
          const rowPromise = batchPromise.then((batch) => {
            const items = (batch.results[0]?.items ??
              []) as readonly RawInstanceResolutionRow[];
            const raw = items[index];
            if (!raw) {
              throw new TssaeError(
                "ENGINE_ERROR",
                `Engine omitted resolveInstances result for "${target.handleId}".`,
              );
            }
            if (raw.status === "proved") {
              const instances = materializeResolvedInstances(
                raw.instances,
                sessionToken,
                projectId,
                snapshotId,
              );
              const [firstInst, ...restInst] = instances;
              if (!firstInst) {
                throw new TssaeError(
                  "ENGINE_ERROR",
                  `Engine returned proved resolveInstances result with no instances for "${target.handleId}".`,
                );
              }
              const primaryRef =
                raw.instance !== null
                  ? makeProgramRef(
                      "instance",
                      sessionToken,
                      projectId,
                      snapshotId,
                      raw.instance,
                    )
                  : null;
              return registerProvenance(
                {
                  target,
                  status: "proved" as const,
                  instance: primaryRef,
                  instanceKey: raw.instanceKey,
                  allocationLocation: raw.allocationLocation,
                  originKind: raw.originKind,
                  instances: [firstInst, ...restInst] as readonly [
                    ResolvedInstanceInfo,
                    ...ResolvedInstanceInfo[],
                  ],
                  evidence: raw.evidence,
                },
                raw.evidence,
              );
            }
            if (raw.status === "disproved") {
              return registerProvenance(
                {
                  target,
                  status: "disproved" as const,
                  evidence: raw.evidence,
                },
                raw.evidence,
              );
            }
            const instances = materializeResolvedInstances(
              raw.instances,
              sessionToken,
              projectId,
              snapshotId,
            );
            return registerProvenance(
              {
                target,
                status: "inconclusive" as const,
                instances,
                gaps: raw.gaps,
                evidence: raw.evidence,
              },
              raw.evidence,
            );
          });
          cache.resolutionCache.set(key, rowPromise);
        }
      }

      const results = await Promise.all(
        targets.map((target) => {
          const key = `${snapshotId}:${projectId}:${lookupScopeKey}:${target.handleId}`;
          return cache.resolutionCache.get(key)!.then((cached) =>
            registerProvenance(
              {
                ...cached,
                target,
              },
              cached.evidence,
            ),
          );
        }),
      );
      for (const row of results) {
        if (row.status === "inconclusive") {
          recordGaps(row.gaps);
        }
      }

      if (onQueryCompleted) {
        const count = results.reduce(
          (sum, row) =>
            sum + (row.status === "disproved" ? 0 : row.instances.length),
          0,
        );
        onQueryCompleted(
          "resolveInstances",
          cacheStatus,
          count,
          performance.now() - start,
        );
      }

      return results;
    },

    projectInstances: async <
      const S extends InstanceProjectionSelection = Record<never, never>,
    >(
      instances: readonly InstanceRef[],
      options?: { readonly select?: S },
    ): Promise<readonly ProjectedInstanceRow<S>[]> => {
      assertOpen();
      if (instances.length === 0) return [];
      validateProgramRefs(instances, sessionToken, snapshotId, projectId);
      const select = options?.select;
      const start = onQueryCompleted ? performance.now() : 0;

      const batch = await client.request<EngineBatchResult>(
        "query.batch",
        {
          snapshotId,
          projectId,
          steps: [
            {
              op: "projectInstances",
              targets: instances.map((inst) => inst.handleId),
              ...(select !== undefined ? { select } : {}),
            },
          ],
        },
        getRpcOptions?.(),
      );

      const items = (batch.results[0]?.items ??
        []) as readonly RawInstanceProjectionRow[];
      const rows = items.map((raw, index) => {
        recordGaps(raw.gaps);
        const row: Record<string, unknown> = {
          instance: instances[index]!,
          gaps: raw.gaps,
        };
        if (select?.instanceKey === true) {
          row.instanceKey = raw.instanceKey ?? "";
        }
        if (select?.originKind === true) {
          row.originKind = raw.originKind ?? "allocation";
        }
        if (select?.allocationLocation === true) {
          row.allocationLocation = raw.allocationLocation ?? null;
        }
        if (select?.originSyntax === true) {
          row.originSyntax =
            raw.originSyntax != null
              ? makeProgramRef(
                  "syntax",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.originSyntax,
                )
              : null;
        }
        if (select?.summaryId === true) {
          row.summaryId = raw.summaryId ?? null;
        }
        if (select?.evidence === true) {
          row.evidence = raw.evidence ?? [];
        }
        const projectedEvidence: Evidence[] = [];
        if (
          select?.evidence === true &&
          raw.evidence &&
          raw.evidence.length > 0
        ) {
          projectedEvidence.push(...raw.evidence);
        } else if (
          select?.allocationLocation === true &&
          raw.allocationLocation != null
        ) {
          if (select.summaryId === true && raw.summaryId != null) {
            projectedEvidence.push({
              kind: "summary",
              summaryId: raw.summaryId,
              ...(raw.summarySource !== undefined
                ? { summarySource: raw.summarySource }
                : {}),
              message: `Instance allocated by summary "${raw.summaryId}"`,
              at: raw.allocationLocation,
            });
          } else {
            projectedEvidence.push({
              kind: "source",
              message: "Instance allocation",
              at: raw.allocationLocation,
            });
          }
        } else if (select?.summaryId === true && raw.summaryId != null) {
          projectedEvidence.push({
            kind: "summary",
            summaryId: raw.summaryId,
            ...(raw.summarySource !== undefined
              ? { summarySource: raw.summarySource }
              : {}),
            message: `Instance summarized by "${raw.summaryId}"`,
          });
        }
        return registerProvenance(
          row as ProjectedInstanceRow<S>,
          projectedEvidence,
        );
      });

      if (onQueryCompleted) {
        onQueryCompleted(
          "projectInstances",
          "computed",
          rows.length,
          performance.now() - start,
        );
      }

      return rows;
    },
  };
}
