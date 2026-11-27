import type { DiagnosticLocation } from "tssae";

import type { CorpusSourceOrigin, ExternalCorpusSpec } from "../corpus.ts";

export type ConformanceCorpusId = "test262" | "typescript-conformance";

export type ConformanceFailureKind =
  | "invariant-violation"
  | "timeout"
  | "crash"
  | "error";

export type ConformanceViolation = {
  readonly corpusId: string;
  readonly kind: ConformanceFailureKind;
  readonly query: string;
  readonly file: string;
  readonly message: string;
  readonly at?: DiagnosticLocation;
};

export type ConformanceFailureRecord = {
  readonly corpusId: string;
  readonly kind: ConformanceFailureKind;
  readonly message: string;
  readonly files: readonly string[];
  readonly file?: string;
  readonly query?: string;
  readonly at?: DiagnosticLocation;
  readonly batchIndex?: number;
};

export type ConformanceBatchWorkerConfig = {
  readonly tssaeEntryPath: string;
  readonly corpusDir: string;
  readonly corpusId: string;
  readonly files: readonly string[];
};

export type ConformanceBatchWorkerResult = {
  readonly filesScanned: number;
  readonly scopesChecked: number;
  readonly queriesExecuted: number;
  readonly violations: readonly ConformanceViolation[];
};

export type ConformanceCorpusResult = {
  readonly id: string;
  readonly repoName: string;
  readonly commitSha: string;
  readonly corpusDir: string;
  readonly sourceOrigin: CorpusSourceOrigin;
  readonly cacheHit: boolean;
  readonly cacheBypassed: boolean;
  readonly matchedFiles: number;
  readonly scannedFiles: number;
  readonly scopesChecked: number;
  readonly queriesExecuted: number;
  readonly batchesTotal: number;
  readonly durationMs: number;
  readonly passed: boolean;
  readonly failures: readonly ConformanceFailureRecord[];
};

export type ConformanceReport = {
  readonly passed: boolean;
  readonly totalDurationMs: number;
  readonly totalFiles: number;
  readonly totalScopes: number;
  readonly totalQueries: number;
  readonly batchSize: number;
  readonly concurrency: number;
  readonly batchTimeoutMs: number;
  readonly corpora: readonly ConformanceCorpusResult[];
  readonly failures: readonly ConformanceFailureRecord[];
};

export type ConformanceCorpusSpecMap = Readonly<
  Record<ConformanceCorpusId, ExternalCorpusSpec>
>;
