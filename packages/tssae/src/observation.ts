import type { RpcRequestOptions } from "./engine-client.ts";
import {
  TssaeError,
  type DiagnosticLocation,
  type EngineProviderExtractionStats,
  type ExplainCleanupPath,
  type ExplainLocalGap,
  type ExplainProviderEvaluation,
  type Gap,
  type ObserveLimit,
  type ObserveMode,
  type ObserveOptions,
  type SourceTarget,
} from "./types.ts";

export type EngineStepMetrics = {
  readonly op: string;
  readonly provider: string;
  readonly cacheStatus: "computed" | "reused";
  readonly durationMs: number;
  readonly candidateCount: number;
  readonly details?: readonly {
    readonly kind: string;
    readonly file?: string;
    readonly line?: number;
    readonly column?: number;
    readonly summary: string;
  }[];
};

export type EngineRpcMetrics = {
  readonly durationMs: number;
  readonly encodeMs: number;
  readonly truncated: boolean;
  readonly droppedCount: number;
  readonly steps?: readonly EngineStepMetrics[];
  readonly providerExtractions?: Readonly<
    Record<string, EngineProviderExtractionStats>
  >;
};

export type EngineCallIoMetrics = {
  readonly isQueryBatch: boolean;
  readonly bytesSent: number;
  readonly bytesReceived: number;
  readonly encodeMs: number;
  readonly writeMs: number;
  readonly engineMetrics?: EngineRpcMetrics;
};

export type QueryCompletedEvent = {
  readonly op: string;
  readonly projectId: string;
  readonly provider: string;
  readonly providers?: readonly string[];
  readonly cacheStatus: "computed" | "reused";
  readonly candidateCount: number;
  readonly durationMs: number;
  readonly summary: string;
  readonly matchedLocations?: readonly DiagnosticLocation[];
  readonly detail?: unknown;
};

export type FactExplainEvent = {
  readonly subjectLocation?: DiagnosticLocation;
  readonly gaps: readonly Gap[];
  readonly localGaps: readonly ExplainLocalGap[];
};

export type CleanupCoverageEvent = {
  readonly targetLocations: readonly DiagnosticLocation[];
  readonly paths: readonly ExplainCleanupPath[];
};

/** Observer hooks attached to a `ProjectContext` to collect metrics, gaps, and explain traces. */
export type QueryObserver = {
  readonly observe?: ObserveMode | ObserveOptions;
  readonly explainTarget?: SourceTarget;
  readonly onRpcMetrics?: (metrics: EngineCallIoMetrics) => void;
  readonly onQueryCompleted?: (event: QueryCompletedEvent) => void;
  readonly onGaps?: (gaps: readonly Gap[]) => void;
  readonly onFactExplain?: (event: FactExplainEvent) => void;
  readonly onProviderEvaluation?: (
    evaluation: ExplainProviderEvaluation,
  ) => void;
  readonly onCleanupCovered?: (event: CleanupCoverageEvent) => void;
};

export type NormalizedObserveConfig = {
  readonly mode: ObserveMode;
  readonly limit?: ObserveLimit;
};

function validateNonNegativeInt(value: number, label: string): number {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TssaeError(
      "INVALID_CONFIG",
      `Invalid ${label} ${String(value)}: must be a non-negative integer.`,
    );
  }
  return value;
}

/** Normalizes `ObserveMode | ObserveOptions` into a validated mode and optional byte/item limit. */
export function normalizeObserveConfig(
  observe: ObserveMode | ObserveOptions | undefined,
): NormalizedObserveConfig {
  if (observe === undefined) {
    return { mode: "off" };
  }
  if (typeof observe === "string") {
    return { mode: observe };
  }
  let limit: ObserveLimit | undefined;
  if (observe.limit !== undefined) {
    if (typeof observe.limit === "number") {
      limit = {
        maxItems: validateNonNegativeInt(observe.limit, "observe limit"),
      };
    } else {
      limit = {
        ...(observe.limit.maxItems !== undefined
          ? {
              maxItems: validateNonNegativeInt(
                observe.limit.maxItems,
                "maxItems",
              ),
            }
          : {}),
        ...(observe.limit.maxBytes !== undefined
          ? {
              maxBytes: validateNonNegativeInt(
                observe.limit.maxBytes,
                "maxBytes",
              ),
            }
          : {}),
      };
    }
  }
  return {
    mode: observe.mode,
    ...(limit !== undefined ? { limit } : {}),
  };
}

/** Builds per-request RPC observation options when observation or RPC metrics collection is enabled. */
export function buildRpcRequestOptions(
  observe: ObserveMode | ObserveOptions | undefined,
  onRpcMetrics: ((metrics: EngineCallIoMetrics) => void) | undefined,
): RpcRequestOptions | undefined {
  const normalized = normalizeObserveConfig(observe);
  if (normalized.mode === "off" && onRpcMetrics === undefined) {
    return undefined;
  }
  return {
    ...(normalized.mode !== "off"
      ? {
          observe: {
            mode: normalized.mode,
            ...(normalized.limit?.maxItems !== undefined
              ? { maxItems: normalized.limit.maxItems }
              : {}),
            ...(normalized.limit?.maxBytes !== undefined
              ? { maxBytes: normalized.limit.maxBytes }
              : {}),
          },
        }
      : {}),
    ...(onRpcMetrics !== undefined ? { onMetrics: onRpcMetrics } : {}),
  };
}

/** Returns whether a diagnostic or syntax span contains the requested `SourceTarget`. */
export function locationMatchesTarget(
  loc: DiagnosticLocation,
  target: SourceTarget,
): boolean {
  if (loc.file !== target.file) {
    return false;
  }
  if (target.line === undefined) {
    return true;
  }
  if (target.line < loc.line || target.line > loc.endLine) {
    return false;
  }
  if (target.column === undefined) {
    return true;
  }
  if (target.line === loc.line && target.column < loc.column) {
    return false;
  }
  if (target.line === loc.endLine) {
    if (loc.line === loc.endLine && loc.endColumn <= loc.column) {
      return target.column === loc.column;
    }
    return target.column < loc.endColumn;
  }
  return true;
}

/** Returns whether two spans in the same file overlap. */
export function locationsOverlap(
  a: DiagnosticLocation,
  b: DiagnosticLocation,
): boolean {
  if (a.file !== b.file) return false;
  return a.start <= b.end && b.start <= a.end;
}

/** Lightweight query trace collector used during adapter and engine fact execution. */
export class ObservationCollector {
  readonly mode: ObserveMode;
  readonly limit: ObserveLimit | undefined;
  readonly explainTarget: SourceTarget | undefined;
  readonly #onQueryCompleted:
    | ((event: QueryCompletedEvent) => void)
    | undefined;
  readonly #onFactExplain: ((event: FactExplainEvent) => void) | undefined;
  readonly #onProviderEvaluation:
    | ((evaluation: ExplainProviderEvaluation) => void)
    | undefined;

  constructor(
    options: {
      readonly mode?: ObserveMode;
      readonly limit?: ObserveLimit;
      readonly explainTarget?: SourceTarget;
      readonly onQueryCompleted?: (event: QueryCompletedEvent) => void;
      readonly onFactExplain?: (event: FactExplainEvent) => void;
      readonly onProviderEvaluation?: (
        evaluation: ExplainProviderEvaluation,
      ) => void;
    } = {},
  ) {
    this.mode = options.mode ?? "off";
    this.limit = options.limit;
    this.explainTarget = options.explainTarget;
    this.#onQueryCompleted = options.onQueryCompleted;
    this.#onFactExplain = options.onFactExplain;
    this.#onProviderEvaluation = options.onProviderEvaluation;
  }

  isObserveEnabled(): boolean {
    return this.mode !== "off";
  }

  isExplainEnabled(): boolean {
    return this.explainTarget !== undefined;
  }

  isActive(): boolean {
    return this.isObserveEnabled() || this.isExplainEnabled();
  }

  recordQuery(params: {
    readonly op: string;
    readonly projectId: string;
    readonly provider: string;
    readonly providers?: readonly string[];
    readonly cacheStatus: "computed" | "reused";
    readonly candidateCount: number;
    readonly durationMs: number;
    readonly summary: string;
    readonly matchedLocations?: readonly DiagnosticLocation[];
    readonly detail?: unknown;
  }): void {
    this.#onQueryCompleted?.({
      op: params.op,
      projectId: params.projectId,
      provider: params.provider,
      ...(params.providers !== undefined
        ? { providers: params.providers }
        : {}),
      cacheStatus: params.cacheStatus,
      candidateCount: params.candidateCount,
      durationMs: params.durationMs,
      summary: params.summary,
      ...(params.matchedLocations !== undefined
        ? { matchedLocations: params.matchedLocations }
        : {}),
      ...(params.detail !== undefined ? { detail: params.detail } : {}),
    });
  }

  recordFactExplain(params: {
    readonly subjectLocation?: DiagnosticLocation;
    readonly gaps: readonly Gap[];
    readonly localGaps: readonly ExplainLocalGap[];
  }): void {
    this.#onFactExplain?.({
      ...(params.subjectLocation !== undefined
        ? { subjectLocation: params.subjectLocation }
        : {}),
      gaps: params.gaps,
      localGaps: params.localGaps,
    });
  }

  recordProviderEvaluation(evaluation: ExplainProviderEvaluation): void {
    this.#onProviderEvaluation?.(evaluation);
  }
}
