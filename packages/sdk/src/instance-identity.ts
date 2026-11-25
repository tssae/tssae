import { createHash } from "node:crypto";
import { performance } from "node:perf_hooks";

import type { EngineClient, RpcRequestOptions } from "./engine-client.ts";
import { registerProvenance } from "./evidence.ts";
import { normalizeLookupFiles } from "./lookup-files.ts";
import { validateProgramRefs, type SelectedKeys } from "./projection.ts";
import {
  VeruleExecutionError,
  makeProgramRef,
  type CallRef,
  type DeclarationRef,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type LookupFilesOptions,
  type InstanceRef,
  type Model,
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
  | "model-allocation"
  | "parameter"
  | "unresolved";

export type ResolvedInstanceInfo = {
  readonly ref: InstanceRef;
  readonly instanceKey: string;
  readonly originKind: InstanceOriginKind;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly originSyntax: SyntaxRef | null;
  readonly modelId: string | null;
  readonly evidence: readonly Evidence[];
};

export type InstanceResolutionResult = {
  readonly target: InstanceTargetRef;
  readonly instance: InstanceRef | null;
  readonly instanceKey: string | null;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly originKind: InstanceOriginKind | null;
  readonly instances: readonly ResolvedInstanceInfo[];
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

export type InstanceProjectionSelection = {
  readonly instanceKey?: true;
  readonly originKind?: true;
  readonly allocationLocation?: true;
  readonly originSyntax?: true;
  readonly modelId?: true;
  readonly evidence?: true;
};

export type FullInstanceProjectionFields = {
  readonly instanceKey: string;
  readonly originKind: InstanceOriginKind;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly originSyntax: SyntaxRef | null;
  readonly modelId: string | null;
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
  readonly modelId: string | null;
  readonly evidence: readonly Evidence[];
};

type RawInstanceResolutionRow = {
  readonly target: string;
  readonly instance: string | null;
  readonly instanceKey: string | null;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly originKind: InstanceOriginKind | null;
  readonly instances: readonly RawResolvedInstanceInfo[];
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

type RawInstanceProjectionRow = {
  readonly instance: string;
  readonly gaps: readonly Gap[];
  readonly instanceKey?: string;
  readonly originKind?: InstanceOriginKind;
  readonly allocationLocation?: DiagnosticLocation | null;
  readonly originSyntax?: string | null;
  readonly modelId?: string | null;
  readonly evidence?: readonly Evidence[];
};

type EngineBatchResult = {
  readonly results: readonly {
    readonly items: readonly unknown[];
  }[];
};

type EngineModelPayload = {
  readonly id: string;
  readonly instanceBehaviors: readonly {
    readonly kind: "allocates-fresh-instance" | "forwards-argument";
    readonly match: {
      readonly symbol?: string;
      readonly container?: string;
      readonly member?: string;
      readonly module?: string;
    };
    readonly argumentIndex?: number;
    readonly description?: string;
  }[];
};

function assertNonEmptyOptionalString(
  value: unknown,
  field: string,
  modelId: string,
): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || value.trim() === "") {
    throw new VeruleExecutionError(
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
    throw new VeruleExecutionError(
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
      throw new VeruleExecutionError(
        "INVALID_CONFIG",
        `Model "${modelId}" instanceBehaviors[${String(index)}] has invalid kind.`,
      );
    }

    const match = behavior.match;
    if (typeof match !== "object" || match === null) {
      throw new VeruleExecutionError(
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
      throw new VeruleExecutionError(
        "INVALID_CONFIG",
        `Model "${modelId}" instanceBehaviors[${String(index)}].match must specify at least one of symbol, container, or member.`,
      );
    }

    if (
      behavior.description !== undefined &&
      typeof behavior.description !== "string"
    ) {
      throw new VeruleExecutionError(
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
        throw new VeruleExecutionError(
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

function serializeModelsForEngine(
  models: readonly Model[],
): readonly EngineModelPayload[] {
  const payload: EngineModelPayload[] = [];
  for (const model of models) {
    if (!model.instanceBehaviors || model.instanceBehaviors.length === 0) {
      continue;
    }
    payload.push({
      id: model.id,
      instanceBehaviors: model.instanceBehaviors.map((behavior) => ({
        kind: behavior.kind,
        match: {
          ...(behavior.match.symbol !== undefined
            ? { symbol: behavior.match.symbol }
            : {}),
          ...(behavior.match.container !== undefined
            ? { container: behavior.match.container }
            : {}),
          ...(behavior.match.member !== undefined
            ? { member: behavior.match.member }
            : {}),
          ...(behavior.match.module !== undefined
            ? { module: behavior.match.module }
            : {}),
        },
        ...(behavior.kind === "forwards-argument" &&
        behavior.argumentIndex !== undefined
          ? { argumentIndex: behavior.argumentIndex }
          : {}),
        ...(behavior.description !== undefined
          ? { description: behavior.description }
          : {}),
      })),
    });
  }
  return payload;
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

/** Creates instance-identity resolution and projection query methods bound to an active snapshot, project, and models. */
export function createInstanceIdentityQueries(deps: {
  readonly client: EngineClient;
  readonly cache: InstanceIdentityCache;
  readonly models: readonly Model[];
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
    models,
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

  const engineModels = serializeModelsForEngine(models);
  const modelsDigest = createHash("sha256")
    .update(JSON.stringify(engineModels))
    .digest("hex")
    .slice(0, 16);

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
        const key = `${snapshotId}:${projectId}:${modelsDigest}:${lookupScopeKey}:${target.handleId}`;
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
                models: engineModels,
                ...(lookupFiles !== undefined ? { lookupFiles } : {}),
              },
            ],
          },
          getRpcOptions?.(),
        );

        for (const [index, target] of missing.entries()) {
          const key = `${snapshotId}:${projectId}:${modelsDigest}:${lookupScopeKey}:${target.handleId}`;
          const rowPromise = batchPromise.then((batch) => {
            const items = (batch.results[0]?.items ??
              []) as readonly RawInstanceResolutionRow[];
            const raw = items[index];
            if (!raw) {
              throw new VeruleExecutionError(
                "ENGINE_ERROR",
                `Engine omitted resolveInstances result for "${target.handleId}".`,
              );
            }
            const instances: ResolvedInstanceInfo[] = raw.instances.map(
              (entry) =>
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
                    modelId: entry.modelId,
                    evidence: entry.evidence,
                  },
                  entry.evidence,
                ),
            );
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
                instance: primaryRef,
                instanceKey: raw.instanceKey,
                allocationLocation: raw.allocationLocation,
                originKind: raw.originKind,
                instances,
                evidence: raw.evidence,
                gaps: raw.gaps,
              },
              raw.evidence,
            );
          });
          cache.resolutionCache.set(key, rowPromise);
        }
      }

      const results = await Promise.all(
        targets.map((target) => {
          const key = `${snapshotId}:${projectId}:${modelsDigest}:${lookupScopeKey}:${target.handleId}`;
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
        recordGaps(row.gaps);
      }

      if (onQueryCompleted) {
        const count = results.reduce(
          (sum, row) => sum + row.instances.length,
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
          row.originKind = raw.originKind ?? "unresolved";
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
        if (select?.modelId === true) {
          row.modelId = raw.modelId ?? null;
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
          if (select.modelId === true && raw.modelId != null) {
            projectedEvidence.push({
              kind: "model",
              modelId: raw.modelId,
              message: `Instance allocated by model "${raw.modelId}"`,
              at: raw.allocationLocation,
            });
          } else {
            projectedEvidence.push({
              kind: "source",
              message: "Instance allocation",
              at: raw.allocationLocation,
            });
          }
        } else if (select?.modelId === true && raw.modelId != null) {
          projectedEvidence.push({
            kind: "model",
            modelId: raw.modelId,
            message: `Instance modeled by "${raw.modelId}"`,
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
