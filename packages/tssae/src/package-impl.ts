import { performance } from "node:perf_hooks";

import type { EngineClient, RpcRequestOptions } from "./engine-client.ts";
import { registerProvenance } from "./evidence.ts";
import { validateProgramRefs } from "./projection.ts";
import type {
  CallRef,
  DeclarationRef,
  DiagnosticLocation,
  Evidence,
  Gap,
  SyntaxRef,
} from "./types.ts";

export type PackageResolutionMode = "import" | "require" | "unknown";

export type ImplementationStatus =
  | "source"
  | "resolved"
  | "declaration-only"
  | "missing-package"
  | "ambiguous"
  | "unimplemented";

export type ImplementationTargetRef = CallRef | DeclarationRef | SyntaxRef;

export type ResolvedImplementationTarget = {
  readonly file: string;
  readonly location: DiagnosticLocation | null;
  readonly exportName: string | null;
  readonly isExternal: boolean;
  readonly packageName: string | null;
  readonly packageVersion: string | null;
  readonly subpath: string | null;
  readonly resolutionMode: PackageResolutionMode;
  readonly conditions: readonly string[];
  readonly declarationFile: string | null;
};

export type ImplementationResolutionResult = {
  readonly target: ImplementationTargetRef;
  readonly status: ImplementationStatus;
  readonly implementation: ResolvedImplementationTarget | null;
  readonly candidates: readonly ResolvedImplementationTarget[];
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

type RawImplementationResolutionRow = {
  readonly target: string;
  readonly status: ImplementationStatus;
  readonly implementation: ResolvedImplementationTarget | null;
  readonly candidates: readonly ResolvedImplementationTarget[];
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
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
            return registerProvenance(
              {
                target,
                status: raw.status,
                implementation: raw.implementation,
                candidates: raw.candidates,
                evidence: raw.evidence,
                gaps: raw.gaps,
              },
              raw.evidence,
            );
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
        recordGaps(row.gaps);
      }

      if (onQueryCompleted) {
        const count = results.filter((r) => r.implementation !== null).length;
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
