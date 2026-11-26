import { performance } from "node:perf_hooks";

import type { EngineClient, RpcRequestOptions } from "./engine-client.ts";
import { registerProvenance } from "./evidence.ts";
import { normalizeLookupFiles } from "./lookup-files.ts";
import {
  mapRawImplementationTarget,
  type RawResolvedImplementationTarget,
  type ResolvedImplementationTarget,
} from "./package-impl.ts";
import { validateProgramRefs } from "./projection.ts";
import {
  makeProgramRef,
  type CallRef,
  type DeclarationRef,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type LookupFilesOptions,
  type SyntaxRef,
} from "./types.ts";

export type CallSiteParameterBinding = {
  readonly argumentIndex: number;
  readonly argument: SyntaxRef;
  readonly parameterName: string | null;
  readonly parameterDeclaration: DeclarationRef | null;
  readonly isRest: boolean;
};

export type DirectCallInfo = {
  readonly call: CallRef;
  readonly syntax: SyntaxRef;
  readonly location: DiagnosticLocation;
  readonly callerDeclaration: DeclarationRef | null;
  readonly parameterBindings: readonly CallSiteParameterBinding[];
  readonly returnTarget: SyntaxRef;
};

export type DeclarationCallersResult = {
  readonly declaration: DeclarationRef;
  readonly calls: readonly CallRef[];
  readonly callSites: readonly DirectCallInfo[];
  readonly evidence: readonly Evidence[];
} & (
  | {
      readonly completeness: "complete";
    }
  | {
      readonly completeness: "incomplete";
      readonly gaps: readonly Gap[];
    }
);

export type CallTargetInfo = {
  readonly declaration: DeclarationRef;
  readonly name: string | null;
  readonly location: DiagnosticLocation | null;
  readonly isExternal: boolean;
  readonly implementation?: ResolvedImplementationTarget | null;
  readonly parameterBindings: readonly CallSiteParameterBinding[];
  readonly returnExpressions: readonly SyntaxRef[];
};

export type ValuePropagationMode = "value";

export type ValueEndpoint = SyntaxRef | DeclarationRef | CallRef;

export type ValuePropagationStepKind =
  | "variableInit"
  | "assignment"
  | "callArgument"
  | "returnValue"
  | "awaitUnwrap"
  | "typeAssertion"
  | "parenthesized"
  | "propertyWrite"
  | "propertyRead"
  | "destructure"
  | "restDestructure"
  | "conditionalBranch"
  | "alias";

export type ValuePropagationStep = {
  readonly kind: ValuePropagationStepKind;
  readonly description: string;
  readonly fromLocation: DiagnosticLocation;
  readonly toLocation: DiagnosticLocation;
  readonly property?: string;
  readonly omittedProperties?: readonly string[];
};

export type ValuePropagationPath = {
  readonly from: ValueEndpoint;
  readonly to: ValueEndpoint;
  readonly steps: readonly ValuePropagationStep[];
  readonly evidence: readonly Evidence[];
  readonly sourcePath: readonly string[];
  readonly omittedProperties: readonly string[];
};

export type ValuePropagationTargetResult = {
  readonly from: ValueEndpoint;
  readonly to: ValueEndpoint;
  readonly reachable: boolean;
  readonly paths: readonly ValuePropagationPath[];
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

export type ValuePropagationOptions = {
  readonly propagation?: ValuePropagationMode;
  /**
   * When true, property accesses (`row.prop`, `row["prop"]`) and non-rest destructuring
   * (`const { prop } = row`) on an unwrapped object advance `ValuePropagationPath.sourcePath`
   * instead of stopping propagation. Defaults to false.
   */
  readonly includeMemberAccess?: boolean;
  readonly from: ValueEndpoint | readonly ValueEndpoint[];
  readonly to: ValueEndpoint | readonly ValueEndpoint[];
} & LookupFilesOptions;

export type ObjectFieldShape = {
  readonly fields: readonly string[];
  readonly nested?: Readonly<Record<string, ObjectFieldShape>>;
  readonly hasDynamicFields?: boolean;
};

export type RetainedObjectFields = {
  readonly fields: readonly string[];
  readonly omittedFields: readonly string[];
  readonly hasDynamicFields: boolean;
  readonly sourcePath: readonly string[];
  readonly matchedNested: boolean;
};

/**
 * Resolves the object fields retained along a `ValuePropagationPath` from an object shape,
 * using only structured `sourcePath` and `omittedProperties` metadata (independent of step prose).
 * Returns `null` when a nested segment in `sourcePath` is absent from `shape.nested`.
 */
export function resolveRetainedObjectFields(
  shape: ObjectFieldShape,
  path: Pick<ValuePropagationPath, "sourcePath" | "omittedProperties">,
): RetainedObjectFields | null {
  let current = shape;
  for (const segment of path.sourcePath) {
    const next = current.nested?.[segment];
    if (!next) {
      return null;
    }
    current = next;
  }
  const omittedSet = new Set(path.omittedProperties);
  return {
    fields: current.fields.filter((field) => !omittedSet.has(field)),
    omittedFields: [...path.omittedProperties],
    hasDynamicFields: current.hasDynamicFields === true,
    sourcePath: [...path.sourcePath],
    matchedNested: path.sourcePath.length > 0,
  };
}

export type ValuePropagationResult = {
  readonly propagation: ValuePropagationMode;
  readonly results: readonly ValuePropagationTargetResult[];
  readonly reachableTargets: readonly ValueEndpoint[];
  readonly gaps: readonly Gap[];
};

export type CallTargetsResult = {
  readonly call: CallRef;
  readonly targets: readonly DeclarationRef[];
  readonly resolvedTargets: readonly CallTargetInfo[];
  readonly returnTarget: SyntaxRef;
  readonly evidence: readonly Evidence[];
} & (
  | {
      readonly completeness: "complete";
    }
  | {
      readonly completeness: "incomplete";
      readonly gaps: readonly Gap[];
    }
);

type RawParameterBinding = {
  readonly argumentIndex: number;
  readonly argument: string;
  readonly parameterName: string | null;
  readonly parameterDeclaration: string | null;
  readonly isRest: boolean;
};

type RawDirectCallInfo = {
  readonly call: string;
  readonly syntax: string;
  readonly location: DiagnosticLocation;
  readonly callerDeclaration: string | null;
  readonly parameterBindings: readonly RawParameterBinding[];
  readonly returnTarget: string;
};

type RawDeclarationCallersRow = {
  readonly declaration: string;
  readonly calls: readonly string[];
  readonly callSites: readonly RawDirectCallInfo[];
  readonly evidence: readonly Evidence[];
} & (
  | {
      readonly completeness: "complete";
    }
  | {
      readonly completeness: "incomplete";
      readonly gaps: readonly Gap[];
    }
);

type RawCallTargetInfo = {
  readonly declaration: string;
  readonly name: string | null;
  readonly location: DiagnosticLocation | null;
  readonly isExternal: boolean;
  readonly implementation?: RawResolvedImplementationTarget | null;
  readonly parameterBindings: readonly RawParameterBinding[];
  readonly returnExpressions: readonly string[];
};

type RawCallTargetsRow = {
  readonly call: string;
  readonly targets: readonly string[];
  readonly resolvedTargets: readonly RawCallTargetInfo[];
  readonly returnTarget: string;
  readonly evidence: readonly Evidence[];
} & (
  | {
      readonly completeness: "complete";
    }
  | {
      readonly completeness: "incomplete";
      readonly gaps: readonly Gap[];
    }
);

type RawValuePropagationPath = {
  readonly from: string;
  readonly to: string;
  readonly steps: readonly ValuePropagationStep[];
  readonly evidence: readonly Evidence[];
  readonly sourcePath: readonly string[];
  readonly omittedProperties: readonly string[];
};

type RawValuePropagationTargetRow = {
  readonly from: string;
  readonly to: string;
  readonly reachable: boolean;
  readonly paths: readonly RawValuePropagationPath[];
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

type RawValuePropagationResult = {
  readonly propagation: ValuePropagationMode;
  readonly results: readonly RawValuePropagationTargetRow[];
  readonly gaps: readonly Gap[];
};

type EngineBatchResult = {
  readonly results: readonly {
    readonly items: readonly unknown[];
  }[];
};

function mapBindings(
  rawBindings: readonly RawParameterBinding[],
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
): readonly CallSiteParameterBinding[] {
  return rawBindings.map((binding) => ({
    argumentIndex: binding.argumentIndex,
    argument: makeProgramRef(
      "syntax",
      sessionToken,
      projectId,
      snapshotId,
      binding.argument,
    ),
    parameterName: binding.parameterName,
    parameterDeclaration:
      binding.parameterDeclaration !== null
        ? makeProgramRef(
            "declaration",
            sessionToken,
            projectId,
            snapshotId,
            binding.parameterDeclaration,
          )
        : null,
    isRest: binding.isRest,
  }));
}

export type CallsValuesCache = {
  readonly callersCache: Map<string, Promise<DeclarationCallersResult>>;
  readonly calleesCache: Map<string, Promise<CallTargetsResult>>;
  readonly valueCache: Map<string, Promise<ValuePropagationResult>>;
  clear: () => void;
};

export function createCallsValuesCache(): CallsValuesCache {
  const callersCache = new Map<string, Promise<DeclarationCallersResult>>();
  const calleesCache = new Map<string, Promise<CallTargetsResult>>();
  const valueCache = new Map<string, Promise<ValuePropagationResult>>();
  return {
    callersCache,
    calleesCache,
    valueCache,
    clear: () => {
      callersCache.clear();
      calleesCache.clear();
      valueCache.clear();
    },
  };
}

/** Creates call-graph and value-propagation query methods bound to an active snapshot and project. */
export function createCallsValuesQueries(deps: {
  readonly client: EngineClient;
  readonly cache: CallsValuesCache;
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
    callersOf: async (
      declarations: readonly DeclarationRef[],
      options?: LookupFilesOptions,
    ): Promise<readonly DeclarationCallersResult[]> => {
      assertOpen();
      const lookupFiles = normalizeLookupFiles(
        options?.lookupFiles,
        cwd,
        sourceFiles,
      );
      const lookupScopeKey = JSON.stringify(lookupFiles ?? null);
      if (declarations.length === 0) return [];
      validateProgramRefs(declarations, sessionToken, snapshotId, projectId);

      const cacheKeyFor = (decl: DeclarationRef) =>
        `${snapshotId}:${projectId}:${lookupScopeKey}:${decl.handleId}`;

      const missing: DeclarationRef[] = [];
      for (const decl of declarations) {
        const key = cacheKeyFor(decl);
        if (!cache.callersCache.has(key)) {
          missing.push(decl);
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
                op: "callersOf",
                targets: missing.map((d) => d.handleId),
                ...(lookupFiles !== undefined ? { lookupFiles } : {}),
              },
            ],
          },
          getRpcOptions?.(),
        );

        for (const [index, decl] of missing.entries()) {
          const key = cacheKeyFor(decl);
          const rowPromise = batchPromise.then((batch) => {
            const items = (batch.results[0]?.items ??
              []) as readonly RawDeclarationCallersRow[];
            const raw = items[index];
            if (!raw) {
              throw new Error(
                `Engine omitted callersOf result for "${decl.handleId}".`,
              );
            }
            const common = {
              declaration: decl,
              calls: raw.calls.map((id) =>
                makeProgramRef("call", sessionToken, projectId, snapshotId, id),
              ),
              callSites: raw.callSites.map((site) =>
                registerProvenance(
                  {
                    call: makeProgramRef(
                      "call",
                      sessionToken,
                      projectId,
                      snapshotId,
                      site.call,
                    ),
                    syntax: makeProgramRef(
                      "syntax",
                      sessionToken,
                      projectId,
                      snapshotId,
                      site.syntax,
                    ),
                    location: site.location,
                    callerDeclaration:
                      site.callerDeclaration !== null
                        ? makeProgramRef(
                            "declaration",
                            sessionToken,
                            projectId,
                            snapshotId,
                            site.callerDeclaration,
                          )
                        : null,
                    parameterBindings: mapBindings(
                      site.parameterBindings,
                      sessionToken,
                      projectId,
                      snapshotId,
                    ),
                    returnTarget: makeProgramRef(
                      "syntax",
                      sessionToken,
                      projectId,
                      snapshotId,
                      site.returnTarget,
                    ),
                  },
                  [
                    {
                      kind: "source",
                      message: "Direct call site",
                      at: site.location,
                    },
                  ],
                ),
              ),
              evidence: raw.evidence,
            };
            const row: DeclarationCallersResult =
              raw.completeness === "incomplete"
                ? { ...common, completeness: "incomplete", gaps: raw.gaps }
                : { ...common, completeness: "complete" };
            return registerProvenance(row, raw.evidence);
          });
          cache.callersCache.set(key, rowPromise);
        }
      }

      const results = await Promise.all(
        declarations.map((decl) => cache.callersCache.get(cacheKeyFor(decl))!),
      );
      for (const row of results) {
        if (row.completeness === "incomplete") {
          recordGaps(row.gaps);
        }
      }

      if (onQueryCompleted) {
        const count = results.reduce((sum, r) => sum + r.calls.length, 0);
        onQueryCompleted(
          "callersOf",
          cacheStatus,
          count,
          performance.now() - start,
        );
      }

      return results;
    },

    calleesOf: async (
      calls: readonly CallRef[],
    ): Promise<readonly CallTargetsResult[]> => {
      assertOpen();
      if (calls.length === 0) return [];
      validateProgramRefs(calls, sessionToken, snapshotId, projectId);

      const missing: CallRef[] = [];
      for (const call of calls) {
        const key = `${snapshotId}:${projectId}:${call.handleId}`;
        if (!cache.calleesCache.has(key)) {
          missing.push(call);
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
                op: "calleesOf",
                targets: missing.map((c) => c.handleId),
              },
            ],
          },
          getRpcOptions?.(),
        );

        for (const [index, call] of missing.entries()) {
          const key = `${snapshotId}:${projectId}:${call.handleId}`;
          const rowPromise = batchPromise.then((batch) => {
            const items = (batch.results[0]?.items ??
              []) as readonly RawCallTargetsRow[];
            const raw = items[index];
            if (!raw) {
              throw new Error(
                `Engine omitted calleesOf result for "${call.handleId}".`,
              );
            }
            const common = {
              call,
              targets: raw.targets.map((id) =>
                makeProgramRef(
                  "declaration",
                  sessionToken,
                  projectId,
                  snapshotId,
                  id,
                ),
              ),
              resolvedTargets: raw.resolvedTargets.map((target) => {
                const mappedTarget: CallTargetInfo = {
                  declaration: makeProgramRef(
                    "declaration",
                    sessionToken,
                    projectId,
                    snapshotId,
                    target.declaration,
                  ),
                  name: target.name,
                  location: target.location,
                  isExternal: target.isExternal,
                  ...(target.implementation !== undefined
                    ? {
                        implementation:
                          target.implementation !== null
                            ? mapRawImplementationTarget(
                                target.implementation,
                                sessionToken,
                                projectId,
                                snapshotId,
                              )
                            : null,
                      }
                    : {}),
                  parameterBindings: mapBindings(
                    target.parameterBindings,
                    sessionToken,
                    projectId,
                    snapshotId,
                  ),
                  returnExpressions: target.returnExpressions.map((id) =>
                    makeProgramRef(
                      "syntax",
                      sessionToken,
                      projectId,
                      snapshotId,
                      id,
                    ),
                  ),
                };
                const targetEvidence: Evidence[] = [];
                if (mappedTarget.location !== null) {
                  targetEvidence.push({
                    kind: "source",
                    message: `Call target: ${mappedTarget.name ?? "<anonymous>"}`,
                    at: mappedTarget.location,
                    ...(mappedTarget.name !== null
                      ? { declaration: mappedTarget.name }
                      : {}),
                  });
                }
                if (
                  mappedTarget.implementation?.location != null &&
                  (mappedTarget.isExternal ||
                    mappedTarget.location === null ||
                    mappedTarget.implementation.location.file !==
                      mappedTarget.location.file ||
                    mappedTarget.implementation.location.start !==
                      mappedTarget.location.start)
                ) {
                  targetEvidence.push({
                    kind: "source",
                    message: `Resolved ${mappedTarget.implementation.resolutionSource} implementation '${mappedTarget.implementation.file}' for '${mappedTarget.name ?? "<anonymous>"}'`,
                    at: mappedTarget.implementation.location,
                    ...(mappedTarget.name !== null
                      ? { declaration: mappedTarget.name }
                      : {}),
                  });
                } else if (
                  targetEvidence.length === 0 &&
                  mappedTarget.name !== null
                ) {
                  targetEvidence.push({
                    kind: "source",
                    message: `External call target: ${mappedTarget.name}`,
                    declaration: mappedTarget.name,
                  });
                }
                return registerProvenance(mappedTarget, targetEvidence);
              }),
              returnTarget: makeProgramRef(
                "syntax",
                sessionToken,
                projectId,
                snapshotId,
                raw.returnTarget,
              ),
              evidence: raw.evidence,
            };
            const result: CallTargetsResult =
              raw.completeness === "incomplete"
                ? { ...common, completeness: "incomplete", gaps: raw.gaps }
                : { ...common, completeness: "complete" };
            return registerProvenance(result, raw.evidence);
          });
          cache.calleesCache.set(key, rowPromise);
        }
      }

      const results = await Promise.all(
        calls.map((call) =>
          cache.calleesCache.get(
            `${snapshotId}:${projectId}:${call.handleId}`,
          )!,
        ),
      );
      for (const row of results) {
        if (row.completeness === "incomplete") {
          recordGaps(row.gaps);
        }
      }

      if (onQueryCompleted) {
        const count = results.reduce((sum, r) => sum + r.targets.length, 0);
        onQueryCompleted(
          "calleesOf",
          cacheStatus,
          count,
          performance.now() - start,
        );
      }

      return results;
    },

    valuePropagation: async (
      options: ValuePropagationOptions,
    ): Promise<ValuePropagationResult> => {
      assertOpen();
      const propagation: ValuePropagationMode = options.propagation ?? "value";
      const includeMemberAccess = options.includeMemberAccess === true;
      const lookupFiles = normalizeLookupFiles(
        options.lookupFiles,
        cwd,
        sourceFiles,
      );
      const fromList = Array.isArray(options.from)
        ? options.from
        : [options.from];
      const toList = Array.isArray(options.to) ? options.to : [options.to];
      const allEndpoints = [...fromList, ...toList];
      validateProgramRefs(allEndpoints, sessionToken, snapshotId, projectId);
      if (fromList.length === 0 || toList.length === 0) {
        return {
          propagation,
          results: [],
          reachableTargets: [],
          gaps: [],
        };
      }

      const endpointByHandle = new Map<string, ValueEndpoint>();
      for (const endpoint of allEndpoints) {
        endpointByHandle.set(endpoint.handleId, endpoint);
      }

      const cacheKey = JSON.stringify([
        snapshotId,
        projectId,
        lookupFiles ?? null,
        propagation,
        includeMemberAccess,
        fromList.map((e) => e.handleId),
        toList.map((e) => e.handleId),
      ]);

      const start = onQueryCompleted ? performance.now() : 0;
      const existing = cache.valueCache.get(cacheKey);
      const cacheStatus: "computed" | "reused" = existing
        ? "reused"
        : "computed";

      let promise: Promise<ValuePropagationResult>;
      if (existing) {
        promise = existing;
      } else {
        promise = client
          .request<EngineBatchResult>(
            "query.batch",
            {
              snapshotId,
              projectId,
              steps: [
                {
                  op: "valuePropagation",
                  propagation,
                  ...(includeMemberAccess ? { includeMemberAccess: true } : {}),
                  from: fromList.map((e) => e.handleId),
                  to: toList.map((e) => e.handleId),
                  ...(lookupFiles !== undefined ? { lookupFiles } : {}),
                },
              ],
            },
            getRpcOptions?.(),
          )
          .then((batch) => {
            const raw = (batch.results[0]?.items?.[0] ?? undefined) as
              | RawValuePropagationResult
              | undefined;
            if (!raw) {
              throw new Error("Engine omitted valuePropagation result.");
            }
            const results: ValuePropagationTargetResult[] = raw.results.map(
              (row) => {
                const fromRef = endpointByHandle.get(row.from)!;
                const toRef = endpointByHandle.get(row.to)!;
                const targetResult: ValuePropagationTargetResult = {
                  from: fromRef,
                  to: toRef,
                  reachable: row.reachable,
                  paths: row.paths.map((p) =>
                    registerProvenance(
                      {
                        from: endpointByHandle.get(p.from) ?? fromRef,
                        to: endpointByHandle.get(p.to) ?? toRef,
                        steps: p.steps,
                        evidence: p.evidence,
                        sourcePath: p.sourcePath,
                        omittedProperties: p.omittedProperties,
                      },
                      p.evidence,
                    ),
                  ),
                  evidence: row.evidence,
                  gaps: row.gaps,
                };
                return registerProvenance(targetResult, row.evidence);
              },
            );
            const reachableSet = new Set<ValueEndpoint>();
            for (const row of results) {
              if (row.reachable) {
                reachableSet.add(row.to);
              }
            }
            return {
              propagation: raw.propagation,
              results,
              reachableTargets: [...reachableSet],
              gaps: raw.gaps,
            };
          });
        cache.valueCache.set(cacheKey, promise);
      }

      const result = await promise;
      recordGaps(result.gaps);
      if (onQueryCompleted) {
        onQueryCompleted(
          "valuePropagation",
          cacheStatus,
          result.results.length,
          performance.now() - start,
        );
      }
      return result;
    },
  };
}
