export type SemanticQueryName =
  | "branchCoverage"
  | "calleesOf"
  | "callersOf"
  | "valuePropagation"
  | "resolveInstances"
  | "instanceCleanup"
  | "callEffects"
  | "resolveImplementations";

export type SemanticOutcomeChannel = "primary" | "implicitExceptions";

export type SemanticOutcomeStatus =
  | "proved"
  | "disproved"
  | "inconclusive"
  | "complete"
  | "incomplete"
  | "resolved"
  | "external";

export type NormalizedLocation = {
  readonly file: string;
  readonly line: number;
  readonly column: number;
  readonly endLine: number;
  readonly endColumn: number;
  readonly start: number;
  readonly end: number;
};

export type DigestSample = {
  readonly query: SemanticQueryName;
  readonly channel: SemanticOutcomeChannel;
  readonly file: string;
  readonly location: NormalizedLocation;
  readonly targetId: string;
  readonly status: SemanticOutcomeStatus;
};

export type QueryChannelCounts = {
  readonly query: SemanticQueryName;
  readonly channel: SemanticOutcomeChannel;
  readonly total: number;
  readonly statusCounts: Readonly<Record<string, number>>;
};

export type QueryDigest = {
  readonly schemaVersion: 1;
  readonly projectCount: number;
  readonly fileCount: number;
  readonly sampleCount: number;
  readonly counts: readonly QueryChannelCounts[];
  readonly samples: readonly DigestSample[];
};

export type CollectQueryDigestOptions = {
  readonly maxFiles?: number;
  readonly maxFunctionsPerFile?: number;
  readonly maxCallsPerFile?: number;
  readonly maxBranchesPerFunction?: number;
  readonly maxBranchTargetsPerFunction?: number;
  readonly maxValueSourcesPerFile?: number;
  readonly maxValueTargetsPerFile?: number;
  readonly maxInstanceTargetsPerFile?: number;
  readonly maxCleanupScopesPerFile?: number;
  readonly maxCleanupTargetsPerFile?: number;
};

export type SemanticDiffWaiver = {
  /** Full 40-character commit SHA of the resolved baseline commit this waiver applies to. */
  readonly baselineCommit: string;
  /** Public TSSAE query method name. */
  readonly query: SemanticQueryName;
  /** Query outcome channel (defaults to `"primary"`). */
  readonly channel?: SemanticOutcomeChannel;
  /** POSIX-normalized relative file path inside the analyzed project. */
  readonly file: string;
  /** Deterministic target identifier (`targetId`) reported by semantic diff. */
  readonly target: string;
  /** Expected baseline status prior to the fix. */
  readonly fromStatus: SemanticOutcomeStatus;
  /** Expected candidate status after the fix. */
  readonly toStatus: SemanticOutcomeStatus;
  /** Reason explaining why the baseline outcome was incorrect or incomplete. */
  readonly reason: string;
  /** In-repo regression test reference locking in the fixed behavior. */
  readonly regressionTest: string;
};

export type StatusTransitionCount = {
  readonly query: SemanticQueryName;
  readonly channel: SemanticOutcomeChannel;
  readonly fromStatus: SemanticOutcomeStatus;
  readonly toStatus: SemanticOutcomeStatus;
  readonly count: number;
};

export type UnknownCountDelta = {
  readonly query: SemanticQueryName;
  readonly channel: SemanticOutcomeChannel;
  readonly unknownStatus: "inconclusive" | "incomplete";
  readonly baselineCount: number;
  readonly candidateCount: number;
  readonly waivedIntoUnknownCount: number;
  readonly unwaivedCandidateCount: number;
  readonly netDelta: number;
};

export type RegressionFinding = {
  readonly kind: "deterministic-inversion" | "unknown-net-increase";
  readonly query: SemanticQueryName;
  readonly channel: SemanticOutcomeChannel;
  readonly file: string;
  readonly line: number;
  readonly column: number;
  readonly endLine: number;
  readonly endColumn: number;
  readonly target: string;
  readonly fromStatus: SemanticOutcomeStatus | "absent";
  readonly toStatus: SemanticOutcomeStatus;
  readonly message: string;
};

export type SemanticDiffResult = {
  readonly passed: boolean;
  readonly baselineCommit: string;
  readonly baselineSampleCount: number;
  readonly candidateSampleCount: number;
  readonly matchedSampleCount: number;
  readonly transitions: readonly StatusTransitionCount[];
  readonly unknownDeltas: readonly UnknownCountDelta[];
  readonly inversions: readonly RegressionFinding[];
  readonly unknownIncreases: readonly RegressionFinding[];
  readonly appliedWaivers: readonly SemanticDiffWaiver[];
  readonly ignoredWaiverCount: number;
};
