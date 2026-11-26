import { performance } from "node:perf_hooks";

import type { EngineClient, RpcRequestOptions } from "./engine-client.ts";
import { registerProvenance } from "./evidence.ts";
import { validateProgramRefs } from "./projection.ts";
import {
  makeProgramRef,
  type CallRef,
  type DeclarationRef,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type SyntaxRef,
} from "./types.ts";

export type FirstPartyResolutionSource =
  | "current-project"
  | "project-reference"
  | "custom-conditions"
  | "declaration-map";

export type ImplementationTargetRef = CallRef | DeclarationRef | SyntaxRef;

export type ResolvedImplementationTarget = {
  readonly file: string;
  readonly location: DiagnosticLocation;
  readonly declaration: DeclarationRef;
  readonly exportName: string | null;
  readonly packageName: string | null;
  readonly resolutionSource: FirstPartyResolutionSource;
  readonly declarationFile: string | null;
};

export type ImplementationResolutionResult = {
  readonly target: ImplementationTargetRef;
} & (
  | {
      readonly status: "resolved";
      readonly implementation: ResolvedImplementationTarget;
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "external";
      readonly packageName: string | null;
      readonly declarationFile: string | null;
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly status: "inconclusive";
      readonly declarationFile: string | null;
      readonly gaps: readonly Gap[];
      readonly evidence: readonly Evidence[];
    }
);

export type RawResolvedImplementationTarget = {
  readonly file: string;
  readonly location: DiagnosticLocation;
  readonly declaration: string;
  readonly exportName: string | null;
  readonly packageName: string | null;
  readonly resolutionSource: FirstPartyResolutionSource;
  readonly declarationFile: string | null;
};

type RawImplementationResolutionRow =
  | {
      readonly target: string;
      readonly status: "resolved";
      readonly implementation: RawResolvedImplementationTarget;
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly target: string;
      readonly status: "external";
      readonly packageName: string | null;
      readonly declarationFile: string | null;
      readonly evidence: readonly Evidence[];
    }
  | {
      readonly target: string;
      readonly status: "inconclusive";
      readonly declarationFile: string | null;
      readonly gaps: readonly Gap[];
      readonly evidence: readonly Evidence[];
    };

type EngineBatchResult = {
  readonly results: readonly {
    readonly items: readonly unknown[];
  }[];
};

export type PackageImplCache = {
  readonly implCache: Map<string, Promise<ImplementationResolutionResult>>;
  clear: () => void;
};

export function createPackageImplCache(): PackageImplCache {
  const implCache = new Map<string, Promise<ImplementationResolutionResult>>();
  return {
    implCache,
    clear: () => {
      implCache.clear();
    },
  };
}

/** Converts an engine-serialized implementation target into a typed SDK object. */
export function mapRawImplementationTarget(
  raw: RawResolvedImplementationTarget,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
): ResolvedImplementationTarget {
  return {
    file: raw.file,
    location: raw.location,
    declaration: makeProgramRef(
      "declaration",
      sessionToken,
      projectId,
      snapshotId,
      raw.declaration,
    ),
    exportName: raw.exportName,
    packageName: raw.packageName,
    resolutionSource: raw.resolutionSource,
    declarationFile: raw.declarationFile,
  };
}

/** Creates package implementation resolution queries bound to an active snapshot and project. */
export function createPackageImplQueries(deps: {
  readonly client: EngineClient;
  readonly cache: PackageImplCache;
  readonly sessionToken: symbol;
  readonly snapshotId: number;
  readonly projectId: string;
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
    assertOpen,
    recordGaps,
    getRpcOptions,
    onQueryCompleted,
  } = deps;

  return {
    resolveImplementations: async (
      targets: readonly ImplementationTargetRef[],
    ): Promise<readonly ImplementationResolutionResult[]> => {
      assertOpen();
      if (targets.length === 0) return [];
      validateProgramRefs(targets, sessionToken, snapshotId, projectId);

      const missing: ImplementationTargetRef[] = [];
      for (const target of targets) {
        const key = `${snapshotId}:${projectId}:${target.handleId}`;
        if (!cache.implCache.has(key)) {
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
                op: "resolveImplementations",
                targets: missing.map((t) => t.handleId),
              },
            ],
          },
          getRpcOptions?.(),
        );

        for (const [index, target] of missing.entries()) {
          const key = `${snapshotId}:${projectId}:${target.handleId}`;
          const rowPromise = batchPromise.then((batch) => {
            const items = (batch.results[0]?.items ??
              []) as readonly RawImplementationResolutionRow[];
            const raw = items[index];
            if (!raw) {
              throw new Error(
                `Engine omitted resolveImplementations result for "${target.handleId}".`,
              );
            }
            const result: ImplementationResolutionResult =
              raw.status === "resolved"
                ? {
                    target,
                    status: "resolved",
                    implementation: mapRawImplementationTarget(
                      raw.implementation,
                      sessionToken,
                      projectId,
                      snapshotId,
                    ),
                    evidence: raw.evidence,
                  }
                : raw.status === "external"
                  ? {
                      target,
                      status: "external",
                      packageName: raw.packageName,
                      declarationFile: raw.declarationFile,
                      evidence: raw.evidence,
                    }
                  : {
                      target,
                      status: "inconclusive",
                      declarationFile: raw.declarationFile,
                      gaps: raw.gaps,
                      evidence: raw.evidence,
                    };
            return registerProvenance(result, raw.evidence);
          });
          cache.implCache.set(key, rowPromise);
        }
      }

      const results = await Promise.all(
        targets.map((target) =>
          cache.implCache.get(`${snapshotId}:${projectId}:${target.handleId}`)!,
        ),
      );
      for (const row of results) {
        if (row.status === "inconclusive") {
          recordGaps(row.gaps);
        }
      }

      if (onQueryCompleted) {
        const count = results.filter((r) => r.status === "resolved").length;
        onQueryCompleted(
          "resolveImplementations",
          cacheStatus,
          count,
          performance.now() - start,
        );
      }

      return results;
    },
  };
}
