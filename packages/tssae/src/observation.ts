import type {
  DiagnosticLocation,
  EngineProviderExtractionStats,
  ExplainLocalGap,
  ExplainProviderEvaluation,
  Gap,
  ObserveLimit,
  ObserveMode,
  SourceTarget,
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

  constructor(
    options: {
      readonly mode?: ObserveMode;
      readonly limit?: ObserveLimit;
      readonly explainTarget?: SourceTarget;
    } = {},
  ) {
    this.mode = options.mode ?? "off";
    this.limit = options.limit;
    this.explainTarget = options.explainTarget;
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

  recordQuery(_params: {
    readonly ruleId: string;
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
  }): void {}

  recordFactExplain(_params: {
    readonly ruleId: string;
    readonly subjectLocation?: DiagnosticLocation;
    readonly gaps: readonly Gap[];
    readonly localGaps: readonly ExplainLocalGap[];
  }): void {}

  recordProviderEvaluation(_params: {
    readonly ruleId: string;
    readonly evaluation: ExplainProviderEvaluation;
  }): void {}
}
