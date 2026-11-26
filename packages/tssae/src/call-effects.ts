import { performance } from "node:perf_hooks";

import type { ControlFlowPathStep } from "./branch-coverage.ts";
import type { EngineClient, RpcRequestOptions } from "./engine-client.ts";
import { recordSyntaxLocation, registerProvenance } from "./evidence.ts";
import { normalizeLookupFiles } from "./lookup-files.ts";
import { validateProgramRefs } from "./projection.ts";
import type { SummaryDiscoverySource } from "./summaries.ts";
import {
  TssaeError,
  makeProgramRef,
  type CallRef,
  type DeclarationRef,
  type DiagnosticLocation,
  type Evidence,
  type ExplainCleanupPath,
  type Gap,
  type InstanceRef,
  type LookupFilesOptions,
  type SyntaxRef,
} from "./types.ts";

export type CleanupScopeRef = SyntaxRef | CallRef | DeclarationRef;

export type InstanceCleanupOptions = LookupFilesOptions & {
  /** Functions, methods, or lifecycle-event calls to analyze for cleanup paths. */
  readonly scopes: readonly CleanupScopeRef[];
  /** Target instances to check whether every modeled path in each scope disposes them. */
  readonly targets?: readonly InstanceRef[];
};

export type CleanupCoverageStatus = "proved" | "disproved" | "inconclusive";

export type CleanupSiteInfo = {
  readonly instance: InstanceRef;
  readonly instanceKey: string;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly operationKind:
    | "disposes-target"
    | "replaces-previous-value"
    | "registers-into-container"
    | "detaches-from-container"
    | (string & {});
  readonly syntax: SyntaxRef;
  readonly location: DiagnosticLocation;
  readonly viaCall: CallRef | null;
  readonly viaCallLocation: DiagnosticLocation | null;
  readonly summaryId: string | null;
  readonly summarySource?: SummaryDiscoverySource;
  readonly evidence: readonly Evidence[];
};

/** Tri-state outcome evaluating implicit `mayThrow` exception paths during cleanup. */
export type ImplicitExceptionCleanupOutcome =
  | {
      readonly status: "proved";
      readonly cleanupSites: readonly CleanupSiteInfo[];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "disproved";
      readonly mayThrowSites: readonly DiagnosticLocation[];
      readonly counterexamplePath: readonly ControlFlowPathStep[];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "inconclusive";
      readonly mayThrowSites: readonly DiagnosticLocation[];
      readonly gaps: readonly Gap[];
      readonly evidence: readonly Evidence[];
    };

export type TargetInstanceCleanupResult = {
  readonly instance: InstanceRef;
  readonly instanceKey: string;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly implicitExceptions: ImplicitExceptionCleanupOutcome;
} & (
  | {
      readonly status: "proved";
      readonly cleanupSites: readonly [CleanupSiteInfo, ...CleanupSiteInfo[]];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "disproved";
      readonly cleanedOnAnyPath: boolean;
      readonly cleanupSites: readonly CleanupSiteInfo[];
      readonly uncleanedPath: readonly ControlFlowPathStep[];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "inconclusive";
      readonly cleanupSites: readonly CleanupSiteInfo[];
      readonly gaps: readonly Gap[];
      readonly evidence: readonly Evidence[];
    }
);

export type InstanceCleanupScopeResult = {
  readonly scope: CleanupScopeRef;
  readonly resolvedFunctions: readonly SyntaxRef[];
  readonly targetResults: readonly TargetInstanceCleanupResult[];
  /** All distinct resource instances actually disposed or replaced on paths through this scope. */
  readonly disposedInstances: readonly CleanupSiteInfo[];
  /** Instances registered into a container inside this scope. */
  readonly registeredInstances: readonly CleanupSiteInfo[];
  /** Instances detached from a container without disposal (e.g. `.delete()`). */
  readonly detachedInstances: readonly CleanupSiteInfo[];
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

export type CallEffectsTargetRef = SyntaxRef | CallRef | DeclarationRef;

export type CallEffectsOptions = {
  readonly targets: readonly CallEffectsTargetRef[];
  readonly followCallbacks?: boolean;
  readonly includeReads?: boolean;
  readonly includeWrites?: boolean;
  readonly maxDepth?: number;
};

export type FieldEffectKind = "read" | "write";

export type FieldWriteOperator =
  | "="
  | "|="
  | "&="
  | "^="
  | "+="
  | "-="
  | "*="
  | "/="
  | "%="
  | "**="
  | "<<="
  | ">>="
  | ">>>="
  | "&&="
  | "||="
  | "??="
  | "++"
  | "--";

export type GuardConditionInfo = {
  readonly branchSyntax: SyntaxRef;
  readonly condition: SyntaxRef;
  readonly conditionText: string;
  readonly direction: "true" | "false";
  readonly location: DiagnosticLocation;
};

export type FieldEffect = {
  readonly kind: FieldEffectKind;
  readonly propertyName: string;
  readonly operator: FieldWriteOperator | (string & {}) | null;
  /** Full read/write syntax node (passable to `branchCoverage`, `typeAt`, `findSyntax`). */
  readonly syntax: SyntaxRef;
  /** Target member expression (e.g. `job.flags` in `job.flags |= QUEUED`). */
  readonly targetSyntax: SyntaxRef;
  /** Receiver object expression (passable to `resolveInstances`, `valuePropagation`, `typeAt`). */
  readonly receiverSyntax: SyntaxRef;
  /** Canonical declaration of the receiver expression when statically resolved. */
  readonly receiverDeclaration: DeclarationRef | null;
  /** Assigned RHS syntax for writes (passable to `valuePropagation`, `typeAt`), or `null` for reads/unary updates. */
  readonly valueSyntax: SyntaxRef | null;
  readonly valueText: string | null;
  readonly location: DiagnosticLocation;
  readonly viaCall: CallRef | null;
  readonly enclosingFunction: SyntaxRef | null;
  readonly enclosingBlock: SyntaxRef | null;
  /** Loop/try-free enclosing block or function suitable as `scope` for `listBranches` and `branchCoverage`. */
  readonly cfgScope: SyntaxRef | null;
  readonly guards: readonly GuardConditionInfo[];
  readonly evidence: readonly Evidence[];
};

export type CallbackInvocationInfo = {
  readonly kind: "direct" | "indirect";
  readonly call: CallRef;
  readonly outerCall: CallRef;
  readonly syntax: SyntaxRef;
  readonly callbackArgument: SyntaxRef;
  readonly parameterIndex: number | null;
  readonly targetDeclaration: DeclarationRef | null;
  readonly helperDeclaration: DeclarationRef | null;
  readonly resolvedTargets: readonly DeclarationRef[];
  readonly enclosingFunction: SyntaxRef | null;
  readonly enclosingBlock: SyntaxRef | null;
  readonly cfgScope: SyntaxRef | null;
  readonly location: DiagnosticLocation;
  readonly outerCallLocation: DiagnosticLocation;
  readonly evidence: readonly Evidence[];
};

export type CallEffectsResult = {
  readonly target: CallEffectsTargetRef;
  readonly fieldReads: readonly FieldEffect[];
  readonly fieldWrites: readonly FieldEffect[];
  readonly effects: readonly FieldEffect[];
  readonly callbackInvocations: readonly CallbackInvocationInfo[];
  readonly calls: readonly CallRef[];
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

type RawCleanupSite = {
  readonly instance: string;
  readonly instanceKey: string;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly operationKind: string;
  readonly syntax: string;
  readonly location: DiagnosticLocation;
  readonly viaCall: string | null;
  readonly viaCallLocation: DiagnosticLocation | null;
  readonly summaryId: string | null;
  readonly summarySource?: SummaryDiscoverySource;
  readonly evidence: readonly Evidence[];
};

type RawImplicitExceptionCleanupOutcome =
  | {
      readonly status: "proved";
      readonly cleanupSites: readonly RawCleanupSite[];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "disproved";
      readonly mayThrowSites: readonly DiagnosticLocation[];
      readonly counterexamplePath: readonly ControlFlowPathStep[];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "inconclusive";
      readonly mayThrowSites: readonly DiagnosticLocation[];
      readonly gaps: readonly Gap[];
      readonly evidence: readonly Evidence[];
    };

type RawTargetCleanupRow = {
  readonly instance: string;
  readonly instanceKey: string;
  readonly allocationLocation: DiagnosticLocation | null;
  readonly implicitExceptions: RawImplicitExceptionCleanupOutcome;
} & (
  | {
      readonly status: "proved";
      readonly cleanupSites: readonly RawCleanupSite[];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "disproved";
      readonly cleanedOnAnyPath: boolean;
      readonly cleanupSites: readonly RawCleanupSite[];
      readonly uncleanedPath: readonly ControlFlowPathStep[];
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "inconclusive";
      readonly cleanupSites: readonly RawCleanupSite[];
      readonly gaps: readonly Gap[];
      readonly evidence: readonly Evidence[];
    }
);

type RawInstanceCleanupRow = {
  readonly scope: string;
  readonly resolvedFunctions: readonly string[];
  readonly targetResults: readonly RawTargetCleanupRow[];
  readonly disposedInstances: readonly RawCleanupSite[];
  readonly registeredInstances: readonly RawCleanupSite[];
  readonly detachedInstances: readonly RawCleanupSite[];
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

type RawGuardCondition = {
  readonly branchSyntax: string;
  readonly condition: string;
  readonly conditionText: string;
  readonly direction: "true" | "false";
  readonly location: DiagnosticLocation;
};

type RawFieldEffect = {
  readonly kind: FieldEffectKind;
  readonly propertyName: string;
  readonly operator: string | null;
  readonly syntax: string;
  readonly targetSyntax: string;
  readonly receiverSyntax: string;
  readonly receiverDeclaration: string | null;
  readonly valueSyntax: string | null;
  readonly valueText: string | null;
  readonly location: DiagnosticLocation;
  readonly viaCall: string | null;
  readonly enclosingFunction: string | null;
  readonly enclosingBlock: string | null;
  readonly cfgScope: string | null;
  readonly guards: readonly RawGuardCondition[];
  readonly evidence: readonly Evidence[];
};

type RawCallbackInvocation = {
  readonly kind: "direct" | "indirect";
  readonly call: string;
  readonly outerCall: string;
  readonly syntax: string;
  readonly callbackArgument: string;
  readonly parameterIndex: number | null;
  readonly targetDeclaration: string | null;
  readonly helperDeclaration: string | null;
  readonly resolvedTargets: readonly string[];
  readonly enclosingFunction: string | null;
  readonly enclosingBlock: string | null;
  readonly cfgScope: string | null;
  readonly location: DiagnosticLocation;
  readonly outerCallLocation: DiagnosticLocation;
  readonly evidence: readonly Evidence[];
};

type RawCallEffectsRow = {
  readonly target: string;
  readonly fieldReads: readonly RawFieldEffect[];
  readonly fieldWrites: readonly RawFieldEffect[];
  readonly effects: readonly RawFieldEffect[];
  readonly callbackInvocations: readonly RawCallbackInvocation[];
  readonly calls: readonly string[];
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

type EngineBatchResult = {
  readonly results: readonly {
    readonly items: readonly unknown[];
  }[];
};

function mapCleanupSite(
  raw: RawCleanupSite,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
  syntaxLocations?: WeakMap<SyntaxRef, DiagnosticLocation>,
): CleanupSiteInfo {
  const syntax = makeProgramRef(
    "syntax",
    sessionToken,
    projectId,
    snapshotId,
    raw.syntax,
  );
  syntaxLocations?.set(syntax, raw.location);
  recordSyntaxLocation(syntax, raw.location);
  return registerProvenance(
    {
      instance: makeProgramRef(
        "instance",
        sessionToken,
        projectId,
        snapshotId,
        raw.instance,
      ),
      instanceKey: raw.instanceKey,
      allocationLocation: raw.allocationLocation,
      operationKind: raw.operationKind,
      syntax,
      location: raw.location,
      viaCall:
        raw.viaCall !== null
          ? makeProgramRef(
              "call",
              sessionToken,
              projectId,
              snapshotId,
              raw.viaCall,
            )
          : null,
      viaCallLocation: raw.viaCallLocation,
      summaryId: raw.summaryId,
      ...(raw.summarySource !== undefined
        ? { summarySource: raw.summarySource }
        : {}),
      evidence: raw.evidence,
    },
    raw.evidence,
  );
}

function mapImplicitExceptionCleanup(
  raw: RawImplicitExceptionCleanupOutcome,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
  syntaxLocations?: WeakMap<SyntaxRef, DiagnosticLocation>,
): ImplicitExceptionCleanupOutcome {
  if (raw.status === "proved") {
    return registerProvenance(
      {
        status: "proved",
        cleanupSites: raw.cleanupSites.map((site) =>
          mapCleanupSite(
            site,
            sessionToken,
            projectId,
            snapshotId,
            syntaxLocations,
          ),
        ),
        evidence: raw.evidence,
      },
      raw.evidence,
    );
  }
  if (raw.status === "disproved") {
    return registerProvenance(
      {
        status: "disproved",
        mayThrowSites: raw.mayThrowSites,
        counterexamplePath: raw.counterexamplePath,
        evidence: raw.evidence,
      },
      raw.evidence,
    );
  }
  return registerProvenance(
    {
      status: "inconclusive",
      mayThrowSites: raw.mayThrowSites,
      gaps: raw.gaps,
      evidence: raw.evidence,
    },
    raw.evidence,
  );
}

function optionalRef<K extends "syntax" | "call" | "declaration">(
  kind: K,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
  handleId: string | null,
) {
  return handleId !== null
    ? makeProgramRef(kind, sessionToken, projectId, snapshotId, handleId)
    : null;
}

function mapFieldEffect(
  raw: RawFieldEffect,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
  syntaxLocations?: WeakMap<SyntaxRef, DiagnosticLocation>,
): FieldEffect {
  const syntax = makeProgramRef(
    "syntax",
    sessionToken,
    projectId,
    snapshotId,
    raw.syntax,
  );
  syntaxLocations?.set(syntax, raw.location);
  recordSyntaxLocation(syntax, raw.location);
  const guards = raw.guards.map((g) => {
    const condition = makeProgramRef(
      "syntax",
      sessionToken,
      projectId,
      snapshotId,
      g.condition,
    );
    syntaxLocations?.set(condition, g.location);
    recordSyntaxLocation(condition, g.location);
    return {
      branchSyntax: makeProgramRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        g.branchSyntax,
      ),
      condition,
      conditionText: g.conditionText,
      direction: g.direction,
      location: g.location,
    };
  });
  return registerProvenance(
    {
      kind: raw.kind,
      propertyName: raw.propertyName,
      operator: raw.operator,
      syntax,
      targetSyntax: makeProgramRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        raw.targetSyntax,
      ),
      receiverSyntax: makeProgramRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        raw.receiverSyntax,
      ),
      receiverDeclaration: optionalRef(
        "declaration",
        sessionToken,
        projectId,
        snapshotId,
        raw.receiverDeclaration,
      ),
      valueSyntax: optionalRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        raw.valueSyntax,
      ),
      valueText: raw.valueText,
      location: raw.location,
      viaCall: optionalRef(
        "call",
        sessionToken,
        projectId,
        snapshotId,
        raw.viaCall,
      ),
      enclosingFunction: optionalRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        raw.enclosingFunction,
      ),
      enclosingBlock: optionalRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        raw.enclosingBlock,
      ),
      cfgScope: optionalRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        raw.cfgScope,
      ),
      guards,
      evidence: raw.evidence,
    },
    raw.evidence,
  );
}

function mapCallbackInvocation(
  cb: RawCallbackInvocation,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
  syntaxLocations?: WeakMap<SyntaxRef, DiagnosticLocation>,
): CallbackInvocationInfo {
  const syntax = makeProgramRef(
    "syntax",
    sessionToken,
    projectId,
    snapshotId,
    cb.syntax,
  );
  syntaxLocations?.set(syntax, cb.location);
  return registerProvenance(
    {
      kind: cb.kind,
      call: makeProgramRef(
        "call",
        sessionToken,
        projectId,
        snapshotId,
        cb.call,
      ),
      outerCall: makeProgramRef(
        "call",
        sessionToken,
        projectId,
        snapshotId,
        cb.outerCall,
      ),
      syntax,
      callbackArgument: makeProgramRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        cb.callbackArgument,
      ),
      parameterIndex: cb.parameterIndex,
      targetDeclaration: optionalRef(
        "declaration",
        sessionToken,
        projectId,
        snapshotId,
        cb.targetDeclaration,
      ),
      helperDeclaration: optionalRef(
        "declaration",
        sessionToken,
        projectId,
        snapshotId,
        cb.helperDeclaration,
      ),
      resolvedTargets: cb.resolvedTargets.map((id) =>
        makeProgramRef("declaration", sessionToken, projectId, snapshotId, id),
      ),
      enclosingFunction: optionalRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        cb.enclosingFunction,
      ),
      enclosingBlock: optionalRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        cb.enclosingBlock,
      ),
      cfgScope: optionalRef(
        "syntax",
        sessionToken,
        projectId,
        snapshotId,
        cb.cfgScope,
      ),
      location: cb.location,
      outerCallLocation: cb.outerCallLocation,
      evidence: cb.evidence,
    },
    cb.evidence,
  );
}

export type CallEffectsCache = {
  readonly effectsCache: Map<string, Promise<readonly CallEffectsResult[]>>;
  readonly cleanupCache: Map<
    string,
    Promise<readonly InstanceCleanupScopeResult[]>
  >;
  clear: () => void;
};

export function createCallEffectsCache(): CallEffectsCache {
  const effectsCache = new Map<string, Promise<readonly CallEffectsResult[]>>();
  const cleanupCache = new Map<
    string,
    Promise<readonly InstanceCleanupScopeResult[]>
  >();
  return {
    effectsCache,
    cleanupCache,
    clear: () => {
      effectsCache.clear();
      cleanupCache.clear();
    },
  };
}

/** Creates `callEffects` and `instanceCleanup` query methods bound to the active snapshot and project. */
export function createCallEffectsQueries(deps: {
  readonly client: EngineClient;
  readonly cache: CallEffectsCache;
  readonly sessionToken: symbol;
  readonly snapshotId: number;
  readonly projectId: string;
  readonly cwd: string;
  readonly sourceFiles: readonly string[];
  readonly assertOpen: () => void;
  readonly recordGaps: (gaps: readonly Gap[]) => void;
  readonly resolveSyntaxLocation?: (
    ref: CleanupScopeRef,
  ) => DiagnosticLocation | undefined;
  readonly syntaxLocations?: WeakMap<SyntaxRef, DiagnosticLocation>;
  readonly onCleanupCovered?: (params: {
    readonly targetLocations: readonly DiagnosticLocation[];
    readonly paths: readonly ExplainCleanupPath[];
  }) => void;
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
    resolveSyntaxLocation,
    syntaxLocations,
    onCleanupCovered,
    getRpcOptions,
    onQueryCompleted,
  } = deps;

  return {
    instanceCleanup: async (
      options: InstanceCleanupOptions,
    ): Promise<readonly InstanceCleanupScopeResult[]> => {
      assertOpen();
      const lookupFiles = normalizeLookupFiles(
        options.lookupFiles,
        cwd,
        sourceFiles,
      );
      const targets = options.targets ?? [];
      validateProgramRefs(options.scopes, sessionToken, snapshotId, projectId);
      validateProgramRefs(targets, sessionToken, snapshotId, projectId);
      if (options.scopes.length === 0) return [];

      const targetByHandle = new Map<string, InstanceRef>();
      for (const target of targets) {
        targetByHandle.set(target.handleId, target);
      }

      const cacheKey = JSON.stringify([
        snapshotId,
        projectId,
        lookupFiles ?? null,
        options.scopes.map((s) => s.handleId),
        targets.map((t) => t.handleId),
      ]);

      const start = onQueryCompleted ? performance.now() : 0;
      const existing = cache.cleanupCache.get(cacheKey);
      const cacheStatus: "computed" | "reused" = existing
        ? "reused"
        : "computed";

      let promise: Promise<readonly InstanceCleanupScopeResult[]>;
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
                  op: "instanceCleanup",
                  scopes: options.scopes.map((s) => s.handleId),
                  targets: targets.map((t) => t.handleId),
                  ...(lookupFiles !== undefined ? { lookupFiles } : {}),
                },
              ],
            },
            getRpcOptions?.(),
          )
          .then((batch) => {
            const items = (batch.results[0]?.items ??
              []) as readonly RawInstanceCleanupRow[];
            return options.scopes.map((scopeRef, idx) => {
              const raw = items[idx];
              if (!raw) {
                throw new TssaeError(
                  "ENGINE_ERROR",
                  `Engine omitted instanceCleanup result for "${scopeRef.handleId}".`,
                );
              }
              const targetResults: TargetInstanceCleanupResult[] =
                raw.targetResults.map((tr) => {
                  const instance =
                    targetByHandle.get(tr.instance) ??
                    makeProgramRef(
                      "instance",
                      sessionToken,
                      projectId,
                      snapshotId,
                      tr.instance,
                    );
                  const implicitExceptions = mapImplicitExceptionCleanup(
                    tr.implicitExceptions,
                    sessionToken,
                    projectId,
                    snapshotId,
                    syntaxLocations,
                  );
                  const cleanupSites = tr.cleanupSites.map((site) =>
                    mapCleanupSite(
                      site,
                      sessionToken,
                      projectId,
                      snapshotId,
                      syntaxLocations,
                    ),
                  );
                  if (tr.status === "proved") {
                    const provedSites = cleanupSites as [
                      CleanupSiteInfo,
                      ...CleanupSiteInfo[],
                    ];
                    return registerProvenance(
                      {
                        instance,
                        instanceKey: tr.instanceKey,
                        allocationLocation: tr.allocationLocation,
                        implicitExceptions,
                        status: "proved",
                        cleanupSites: provedSites,
                        evidence: tr.evidence,
                      },
                      tr.evidence,
                    );
                  }
                  if (tr.status === "disproved") {
                    return registerProvenance(
                      {
                        instance,
                        instanceKey: tr.instanceKey,
                        allocationLocation: tr.allocationLocation,
                        implicitExceptions,
                        status: "disproved",
                        cleanedOnAnyPath: tr.cleanedOnAnyPath,
                        cleanupSites,
                        uncleanedPath: tr.uncleanedPath,
                        evidence: tr.evidence,
                      },
                      tr.evidence,
                    );
                  }
                  return registerProvenance(
                    {
                      instance,
                      instanceKey: tr.instanceKey,
                      allocationLocation: tr.allocationLocation,
                      implicitExceptions,
                      status: "inconclusive",
                      cleanupSites,
                      gaps: tr.gaps,
                      evidence: tr.evidence,
                    },
                    tr.evidence,
                  );
                });

              return {
                scope: scopeRef,
                resolvedFunctions: raw.resolvedFunctions.map((id) =>
                  makeProgramRef(
                    "syntax",
                    sessionToken,
                    projectId,
                    snapshotId,
                    id,
                  ),
                ),
                targetResults,
                disposedInstances: raw.disposedInstances.map((site) =>
                  mapCleanupSite(
                    site,
                    sessionToken,
                    projectId,
                    snapshotId,
                    syntaxLocations,
                  ),
                ),
                registeredInstances: raw.registeredInstances.map((site) =>
                  mapCleanupSite(
                    site,
                    sessionToken,
                    projectId,
                    snapshotId,
                    syntaxLocations,
                  ),
                ),
                detachedInstances: raw.detachedInstances.map((site) =>
                  mapCleanupSite(
                    site,
                    sessionToken,
                    projectId,
                    snapshotId,
                    syntaxLocations,
                  ),
                ),
                evidence: raw.evidence,
                gaps: raw.gaps,
              };
            });
          });
        cache.cleanupCache.set(cacheKey, promise);
      }

      const results = await promise;
      for (const scopeResult of results) {
        recordGaps(scopeResult.gaps);
        for (const tr of scopeResult.targetResults) {
          if (tr.status === "inconclusive") {
            recordGaps(tr.gaps);
          }
          if (tr.implicitExceptions.status === "inconclusive") {
            recordGaps(tr.implicitExceptions.gaps);
          }
        }
      }
      if (onCleanupCovered) {
        for (const scopeResult of results) {
          const scopeLoc = resolveSyntaxLocation?.(scopeResult.scope);
          for (const tr of scopeResult.targetResults) {
            if (tr.status !== "proved" || tr.cleanupSites.length === 0) {
              continue;
            }
            const cleanupLocations: DiagnosticLocation[] = [];
            const seenLocKeys = new Set<string>();
            for (const s of tr.cleanupSites) {
              for (const loc of [
                s.location,
                ...(s.viaCallLocation !== null ? [s.viaCallLocation] : []),
              ]) {
                const key = `${loc.file}:${loc.start}:${loc.end}`;
                if (!seenLocKeys.has(key)) {
                  seenLocKeys.add(key);
                  cleanupLocations.push(loc);
                }
              }
            }
            const resolvedScopeLoc = scopeLoc ?? cleanupLocations[0]!;
            const summaryIds = [
              ...new Set(
                tr.cleanupSites
                  .map((s) => s.summaryId)
                  .filter((id): id is string => id !== null),
              ),
            ].toSorted();
            const targetLocations: DiagnosticLocation[] = [
              resolvedScopeLoc,
              ...(tr.allocationLocation !== null
                ? [tr.allocationLocation]
                : []),
              ...cleanupLocations,
            ];
            const pathEntry: ExplainCleanupPath = {
              instanceKey: tr.instanceKey,
              ...(tr.allocationLocation !== null
                ? { allocationLocation: tr.allocationLocation }
                : {}),
              scopeLocation: resolvedScopeLoc,
              cleanupLocations,
              summaryIds,
              summary: `Instance ${tr.instanceKey} is cleaned on all paths (${cleanupLocations.length} site(s))`,
              evidence: tr.evidence,
            };
            onCleanupCovered({ targetLocations, paths: [pathEntry] });
          }
        }
      }

      if (onQueryCompleted) {
        const count = results.reduce(
          (sum, row) => sum + row.disposedInstances.length,
          0,
        );
        onQueryCompleted(
          "instanceCleanup",
          cacheStatus,
          count,
          performance.now() - start,
        );
      }

      return results;
    },

    callEffects: async (
      options: CallEffectsOptions,
    ): Promise<readonly CallEffectsResult[]> => {
      assertOpen();
      if (options.targets.length === 0) return [];
      validateProgramRefs(options.targets, sessionToken, snapshotId, projectId);

      const followCallbacks = options.followCallbacks ?? true;
      const includeReads = options.includeReads ?? true;
      const includeWrites = options.includeWrites ?? true;

      const cacheKey = JSON.stringify([
        snapshotId,
        projectId,
        followCallbacks,
        includeReads,
        includeWrites,
        options.maxDepth ?? null,
        options.targets.map((t) => t.handleId),
      ]);

      const start = onQueryCompleted ? performance.now() : 0;
      const existing = cache.effectsCache.get(cacheKey);
      const cacheStatus: "computed" | "reused" = existing
        ? "reused"
        : "computed";

      let promise: Promise<readonly CallEffectsResult[]>;
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
                  op: "callEffects",
                  targets: options.targets.map((t) => t.handleId),
                  followCallbacks,
                  includeReads,
                  includeWrites,
                  ...(options.maxDepth !== undefined
                    ? { maxDepth: options.maxDepth }
                    : {}),
                },
              ],
            },
            getRpcOptions?.(),
          )
          .then((batch) => {
            const items = (batch.results[0]?.items ??
              []) as readonly RawCallEffectsRow[];
            return options.targets.map((targetRef, idx) => {
              const raw = items[idx];
              if (!raw) {
                throw new TssaeError(
                  "ENGINE_ERROR",
                  `Engine omitted callEffects result for "${targetRef.handleId}".`,
                );
              }
              const base = {
                target: targetRef,
                fieldReads: raw.fieldReads.map((e) =>
                  mapFieldEffect(
                    e,
                    sessionToken,
                    projectId,
                    snapshotId,
                    syntaxLocations,
                  ),
                ),
                fieldWrites: raw.fieldWrites.map((e) =>
                  mapFieldEffect(
                    e,
                    sessionToken,
                    projectId,
                    snapshotId,
                    syntaxLocations,
                  ),
                ),
                effects: raw.effects.map((e) =>
                  mapFieldEffect(
                    e,
                    sessionToken,
                    projectId,
                    snapshotId,
                    syntaxLocations,
                  ),
                ),
                callbackInvocations: raw.callbackInvocations.map((cb) =>
                  mapCallbackInvocation(
                    cb,
                    sessionToken,
                    projectId,
                    snapshotId,
                    syntaxLocations,
                  ),
                ),
                calls: raw.calls.map((id) =>
                  makeProgramRef(
                    "call",
                    sessionToken,
                    projectId,
                    snapshotId,
                    id,
                  ),
                ),
                evidence: raw.evidence,
              };
              return raw.completeness === "incomplete"
                ? {
                    ...base,
                    completeness: "incomplete" as const,
                    gaps: raw.gaps,
                  }
                : {
                    ...base,
                    completeness: "complete" as const,
                  };
            });
          });
        cache.effectsCache.set(cacheKey, promise);
      }

      const results = await promise;
      for (const row of results) {
        if (row.completeness === "incomplete") {
          recordGaps(row.gaps);
        }
      }
      if (onQueryCompleted) {
        const count = results.reduce((sum, row) => sum + row.effects.length, 0);
        onQueryCompleted(
          "callEffects",
          cacheStatus,
          count,
          performance.now() - start,
        );
      }
      return results;
    },
  };
}
