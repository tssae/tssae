import type {
  AdditionalInputEntry,
  AdditionalInputSource,
} from "./additional-inputs.ts";
import type {
  BranchCoverageItem,
  BranchCoverageOptions,
  FunctionBranchesResult,
} from "./branch-coverage.ts";
import type {
  CallEffectsOptions,
  CallEffectsResult,
  CleanupModelOperation,
  InstanceCleanupOptions,
  InstanceCleanupScopeResult,
  LifecycleEventBehavior,
} from "./call-effects.ts";
import type {
  CallTargetsResult,
  DeclarationCallersResult,
  ValuePropagationOptions,
  ValuePropagationResult,
} from "./calls-values.ts";
import type { DiagnosticBasis } from "./evidence.ts";
import type {
  ConfiguredAdapter,
  FactQueryOptions,
  FactResult,
  FactType,
  LibraryAdapter,
} from "./facts.ts";
import type {
  InstanceModelBehavior,
  InstanceProjectionSelection,
  InstanceResolutionResult,
  InstanceTargetRef,
  ProjectedInstanceRow,
} from "./instance-identity.ts";
import type {
  ImplementationResolutionResult,
  ImplementationTargetRef,
} from "./package-impl.ts";
import type {
  AssignabilityPair,
  AssignabilityResult,
  CallProjectionSelection,
  DeclarationProjectionSelection,
  ProjectedCallResolutionRow,
  ProjectedCallRow,
  ProjectedDeclarationRow,
  ProjectedSignatureRow,
  ProjectedSyntaxLookupRow,
  ProjectedTypeRow,
  SignatureProjectionSelection,
  SyntaxLookupSelection,
  TypeProjectionSelection,
} from "./projection.ts";

export const REPORT_SCHEMA_VERSION = "1" as const;

export type VeruleErrorCode =
  | "INVALID_CONFIG"
  | "PROJECT_LOAD_ERROR"
  | "RULE_EXECUTION_ERROR"
  | "FACT_CONFLICT"
  | "STALE_REFERENCE"
  | "CROSS_PROJECT_REFERENCE"
  | "STALE_SNAPSHOT"
  | "SESSION_CLOSED"
  | "NATIVE_ADAPTER_LOAD"
  | "NATIVE_ADAPTER_EXTRACT"
  | "ENGINE_ERROR"
  | "INVALID_QUERY";

export type FactConflictParty = {
  readonly providerId: string;
  readonly providers: readonly string[];
  readonly conclusion: unknown;
  readonly conclusionSummary: string;
  readonly evidence: readonly Evidence[];
};

export type FactConflictDetails = {
  readonly factTypeId: string;
  readonly projectId: string;
  readonly snapshotId: number;
  readonly subjectHandleId: string;
  readonly subjectLocation?: DiagnosticLocation;
  readonly condition?: unknown;
  readonly reason: string;
  readonly parties: readonly [FactConflictParty, FactConflictParty];
};

/** Execution failure raised when analysis configuration, loading, queries, or rules fail. */
export class VeruleExecutionError extends Error {
  readonly code: VeruleErrorCode;
  readonly conflict?: FactConflictDetails;

  constructor(
    code: VeruleErrorCode,
    message: string,
    options?: ErrorOptions & { readonly conflict?: FactConflictDetails },
  ) {
    super(message, options);
    this.name = "VeruleExecutionError";
    this.code = code;
    if (options?.conflict !== undefined) {
      this.conflict = options.conflict;
    }
  }
}

export type ObserveMode = "off" | "summary" | "detail";

export type ObserveLimit = {
  readonly maxItems?: number;
  readonly maxBytes?: number;
};

export type ObserveLimitInput = number | ObserveLimit;

export type ObserveOptions = {
  readonly mode: ObserveMode;
  readonly limit?: ObserveLimitInput;
};

export type SourceTarget = {
  readonly file: string;
  readonly line?: number;
  readonly column?: number;
};

export type CheckpointOutcome = "pass" | "exclude" | "info";

export type SyntaxKind =
  | "Identifier"
  | "CallExpression"
  | "NewExpression"
  | "ReturnStatement"
  | "VariableDeclaration"
  | "BinaryExpression"
  | "PropertyAccessExpression"
  | "IfStatement"
  | "FunctionDeclaration"
  | "ArrowFunction"
  | "FunctionExpression"
  | "MethodDeclaration"
  | "MethodSignature"
  | "CallSignature"
  | "Constructor"
  | "FunctionType"
  | "StringLiteral"
  | "NumericLiteral"
  | "Parameter"
  | "PropertyAssignment"
  | "ShorthandPropertyAssignment"
  | "PropertyDeclaration"
  | "PropertySignature"
  | "ClassDeclaration"
  | "InterfaceDeclaration"
  | "TypeAliasDeclaration"
  | "ImportDeclaration"
  | "ImportSpecifier"
  | "ExportSpecifier"
  | "Block"
  | "TypeReference"
  | "SwitchStatement"
  | "CaseClause"
  | "DefaultClause"
  | "ConditionalExpression"
  | "ThrowStatement"
  | "TryStatement"
  | "ExpressionStatement"
  | "VariableStatement";

declare const objectKindBrand: unique symbol;
declare const sessionBrand: unique symbol;

export type ProgramObjectKind =
  | "syntax"
  | "declaration"
  | "call"
  | "signature"
  | "type"
  | "branch"
  | "instance";

export interface ProgramRef<K extends ProgramObjectKind = ProgramObjectKind> {
  readonly [objectKindBrand]: K;
  readonly [sessionBrand]: symbol;
  readonly projectId: string;
  readonly snapshotId: number;
  readonly handleId: string;
}

export type SyntaxRef = ProgramRef<"syntax">;
export type DeclarationRef = ProgramRef<"declaration">;
export type CallRef = ProgramRef<"call">;
export type SignatureRef = ProgramRef<"signature">;
export type TypeRef = ProgramRef<"type">;
export type BranchRef = ProgramRef<"branch">;
export type InstanceRef = ProgramRef<"instance">;

const refSessionTokens = new WeakMap<object, symbol>();

export function makeProgramRef<K extends ProgramObjectKind>(
  _kind: K,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
  handleId: string,
): ProgramRef<K> {
  const ref = {
    projectId,
    snapshotId,
    handleId,
  } as ProgramRef<K>;
  refSessionTokens.set(ref, sessionToken);
  return ref;
}

export function getRefSessionToken(ref: ProgramRef): symbol | undefined {
  return refSessionTokens.get(ref);
}

/** 1-based UTF-16 line/column and 0-based UTF-8 byte offsets in a normalized relative file. */
export type DiagnosticLocation = {
  readonly file: string;
  readonly line: number;
  readonly column: number;
  readonly endLine: number;
  readonly endColumn: number;
  readonly start: number;
  readonly end: number;
};

export type CheckpointTarget =
  | DiagnosticLocation
  | SyntaxRef
  | { readonly location: DiagnosticLocation };

export type Evidence = {
  readonly kind: "source" | "type" | "note" | "model";
  readonly message: string;
  readonly at?: DiagnosticLocation;
  readonly modelId?: string;
  readonly declaration?: string;
};

export type Gap = {
  readonly kind: string;
  readonly message: string;
  readonly at?: DiagnosticLocation;
};

export type DiagnosticInput = {
  readonly at: DiagnosticLocation;
  readonly message: string;
  readonly findingKey?: string;
  /** Selected query results and explicit evidence, in diagnostic display order. */
  readonly basis?: readonly DiagnosticBasis[];
};

export type Diagnostic = {
  readonly id: string;
  readonly ruleId: string;
  readonly at: DiagnosticLocation;
  readonly message: string;
  readonly evidence: readonly Evidence[];
  readonly projects: readonly string[];
};

export type CompilerOptionsSummary = {
  readonly strict: boolean;
  readonly allowJs: boolean;
  readonly checkJs: boolean;
  readonly composite: boolean;
  readonly target?: string;
  readonly module?: string;
  readonly moduleResolution?: string;
  readonly jsx?: string;
  readonly paths: Readonly<Record<string, readonly string[]>>;
  readonly customConditions: readonly string[];
};

export type ProjectSummary = {
  readonly projectId: string;
  readonly configFile: string;
  readonly entryConfig: string;
  readonly entryChain: readonly string[];
  readonly referencedBy: readonly string[];
  readonly projectReferences: readonly string[];
  readonly compilerOptions: CompilerOptionsSummary;
  readonly sourceFiles: readonly string[];
};

export type SourceFileProjectOrigin = {
  readonly projectId: string;
  readonly configFile: string;
  readonly entryConfig: string;
  readonly entryChain: readonly string[];
};

export type SourceFileOrigin = {
  readonly file: string;
  readonly projects: readonly SourceFileProjectOrigin[];
};

export type Model = {
  readonly id: string;
  readonly version?: string;
  readonly description?: string;
  readonly facts?: Readonly<Record<string, unknown>>;
  readonly instanceBehaviors?: readonly InstanceModelBehavior[];
  readonly cleanupOperations?: readonly CleanupModelOperation[];
  readonly lifecycleEvents?: readonly LifecycleEventBehavior[];
};

export type RuleIdentitySummary = {
  readonly id: string;
  readonly digest: string;
};

export type ModelIdentitySummary = {
  readonly id: string;
  readonly version?: string;
  readonly digest: string;
};

export type ReportIdentity = {
  readonly snapshotDigest: string;
  readonly configDigest: string;
  readonly rules: readonly RuleIdentitySummary[];
  readonly models: readonly ModelIdentitySummary[];
};

export type ObservationPhases = {
  readonly configAndOpenMs: number;
  readonly engineQueryMs: number;
  readonly ruleExecutionMs: number;
  readonly totalMs: number;
};

export type ObservationRpcMetrics = {
  readonly batches: number;
  readonly queryBatches: number;
  readonly bytesSent: number;
  readonly bytesReceived: number;
};

export type ObservationOverhead = {
  readonly checkpointMs: number;
  readonly encodeMs: number;
  readonly writeMs: number;
};

export type EngineProviderExtractionStats = {
  readonly extractions: number;
  readonly candidates: number;
};

/** Native adapter state returned by the active engine snapshot registry. */
export type NativeAdapterObservation = {
  /** The package name configured by the project. */
  readonly packageName: string;
  /** Provider ID declared by the loaded adapter manifest. */
  readonly providerId: string;
  /** `reused` means the snapshot update retained the existing loaded library. */
  readonly status: "loaded" | "reused";
};

export type ObservationDetailEntry = {
  readonly kind: "query" | "checkpoint" | "engineStep";
  readonly ruleId?: string;
  readonly projectId?: string;
  readonly provider?: string;
  readonly name?: string;
  readonly outcome?: CheckpointOutcome;
  readonly cacheStatus?: "computed" | "reused";
  readonly candidateCount?: number;
  readonly durationMs?: number;
  readonly at?: DiagnosticLocation;
  readonly summary: string;
  readonly detail?: unknown;
};

export type ObservationReport = {
  readonly mode: Exclude<ObserveMode, "off">;
  readonly phases: ObservationPhases;
  readonly rpc: ObservationRpcMetrics;
  readonly overhead: ObservationOverhead;
  readonly queryCount: number;
  readonly computedQueryCount: number;
  readonly reusedQueryCount: number;
  readonly candidateCount: number;
  readonly checkpointCount: number;
  readonly providers: readonly string[];
  readonly nativeAdapters: readonly NativeAdapterObservation[];
  readonly limit?: ObserveLimit;
  readonly truncated: boolean;
  readonly droppedCount: number;
  readonly engineExtractions: Readonly<
    Record<string, EngineProviderExtractionStats>
  >;
  readonly details?: readonly ObservationDetailEntry[];
};

export type ExplainStatus =
  | "reported"
  | "excluded_by_checkpoint"
  | "excluded_by_query"
  | "not_in_candidates"
  | "target_not_in_project"
  | "suppressed_by_ignore"
  | "blocked_by_gap"
  | "covered_by_cleanup"
  | "rule_filtered";

export type ExplainQueryStep = {
  readonly queryIndex: number;
  readonly op: string;
  readonly projectId: string;
  readonly provider: string;
  readonly cacheStatus: "computed" | "reused";
  readonly candidateCount: number;
  readonly matchedTarget: boolean;
  readonly durationMs: number;
  readonly summary?: string;
  readonly detail?: unknown;
};

export type ExplainCheckpointTrace = {
  readonly name: string;
  readonly ruleId: string;
  readonly projectId: string;
  readonly outcome: CheckpointOutcome;
  readonly at?: DiagnosticLocation;
  readonly detail?: unknown;
};

export type ExplainCleanupPath = {
  readonly instanceKey: string;
  readonly allocationLocation?: DiagnosticLocation;
  readonly scopeLocation: DiagnosticLocation;
  readonly cleanupLocations: readonly DiagnosticLocation[];
  readonly modelIds: readonly string[];
  readonly summary: string;
  readonly evidence: readonly Evidence[];
};

export type ExplainGapResolutionStatus = "unresolved" | "resolved_by_override";

export type ExplainGapResolution = {
  readonly providerId: string;
  readonly operation: string;
  readonly conclusion: string;
  readonly at?: DiagnosticLocation;
};

export type ExplainLocalGap = {
  readonly status: ExplainGapResolutionStatus;
  readonly factTypeId: string;
  readonly providerId: string;
  readonly field?: string;
  readonly gap: Gap;
  readonly subjectLocation?: DiagnosticLocation;
  readonly resolvedBy?: ExplainGapResolution;
  readonly impact?: string;
};

export type ExplainProviderEvaluationStatus =
  | "evaluated"
  | "reused"
  | "skipped";

export type ExplainProviderEvaluation = {
  readonly providerId: string;
  readonly factTypeId?: string;
  readonly status: ExplainProviderEvaluationStatus;
  readonly reason?: string;
  readonly subjectLocation?: DiagnosticLocation;
};

export type ExplainResult = {
  readonly ruleId: string;
  readonly target: SourceTarget;
  readonly status: ExplainStatus;
  readonly inCandidates: boolean;
  readonly summary: string;
  readonly projectsChecked: readonly string[];
  readonly matchingCandidates: readonly DiagnosticLocation[];
  readonly excludedByQuery?: ExplainQueryStep;
  readonly excludedByCheckpoint?: ExplainCheckpointTrace;
  readonly queries: readonly ExplainQueryStep[];
  readonly checkpoints: readonly ExplainCheckpointTrace[];
  readonly gaps: readonly Gap[];
  readonly localGaps: readonly ExplainLocalGap[];
  readonly providerEvaluations: readonly ExplainProviderEvaluation[];
  readonly providers: readonly string[];
  readonly cleanupPaths: readonly ExplainCleanupPath[];
  readonly inlineIgnore?: {
    readonly matched: boolean;
    readonly ruleId?: string;
    readonly reason?: string;
    readonly commentAt?: DiagnosticLocation;
  };
};

export type InlineIgnoreStatus =
  | "suppressed"
  | "unused"
  | "ambiguous"
  | "malformed"
  | "unknown_rule"
  | "undetermined";

export type InlineIgnoreEntry = {
  readonly status: InlineIgnoreStatus;
  readonly at: DiagnosticLocation;
  readonly targetLine?: number;
  readonly ruleId?: string;
  readonly reason?: string;
  readonly matchedLocations?: readonly DiagnosticLocation[];
  readonly message?: string;
};

export type CheckReport = {
  readonly reportVersion: typeof REPORT_SCHEMA_VERSION;
  readonly status: "clean" | "findings";
  readonly projects: readonly ProjectSummary[];
  readonly fileOrigins: readonly SourceFileOrigin[];
  readonly additionalInputs: readonly AdditionalInputEntry[];
  readonly diagnostics: readonly Diagnostic[];
  readonly gaps: readonly Gap[];
  readonly identity: ReportIdentity;
  readonly inlineIgnores?: readonly InlineIgnoreEntry[];
  readonly observation?: ObservationReport;
  readonly explanation?: ExplainResult;
  readonly explanations?: readonly ExplainResult[];
};

export type ExplainReport = CheckReport & {
  readonly explanation: ExplainResult;
};

export type SourceRange = {
  readonly file: string;
  readonly startLine?: number;
  readonly startColumn?: number;
  readonly endLine?: number;
  readonly endColumn?: number;
  readonly start?: number;
  readonly end?: number;
  readonly match?: "within" | "intersects";
};

export type FindSyntaxOptions = {
  readonly kinds?: readonly SyntaxKind[];
  readonly files?: readonly string[];
  readonly range?: SourceRange;
  /** Optional substring needles; only AST nodes whose source text contains at least one needle are returned. */
  readonly textIncludes?: readonly string[];
  /** Optional exact name filter for identifier, property, or static string-index callees. */
  readonly calleeNames?: readonly string[];
};

/**
 * Restricts unanchored reverse searches to these files in the current project.
 * An empty list supplies no reverse candidates; traversal from a known call
 * still follows source in the complete compiler project.
 */
export type LookupFilesOptions = {
  readonly lookupFiles?: readonly string[];
};

export type SyntaxNodeResult = {
  readonly ref: SyntaxRef;
  readonly kind: SyntaxKind | (string & {});
  readonly text: string;
  readonly location: DiagnosticLocation;
  readonly expression?: SyntaxRef;
  readonly arguments?: readonly SyntaxRef[];
  readonly name?: SyntaxRef;
  readonly initializer?: SyntaxRef;
  readonly body?: SyntaxRef;
  readonly parameters?: readonly SyntaxRef[];
};

export type TypeAtResult = {
  readonly target: SyntaxRef;
  readonly ref: TypeRef;
  readonly typeText: string;
  readonly isUnion: boolean;
  readonly isIntersection: boolean;
  readonly isLiteral: boolean;
  readonly literalValue?: string | number | boolean;
  readonly unionMembers?: readonly string[];
};

export type RuleContext = {
  readonly project: ProjectSummary;
  readonly models: readonly Model[];
  getModel: (id: string) => Model | undefined;
  findSyntax: (
    options: FindSyntaxOptions,
  ) => Promise<readonly SyntaxNodeResult[]>;
  typeAt: (targets: readonly SyntaxRef[]) => Promise<readonly TypeAtResult[]>;
  listBranches: (
    functions: readonly SyntaxRef[],
  ) => Promise<readonly FunctionBranchesResult[]>;
  branchCoverage: (
    options: BranchCoverageOptions,
  ) => Promise<readonly BranchCoverageItem[]>;
  resolveDeclarations: <
    const S extends DeclarationProjectionSelection = Record<never, never>,
  >(
    targets: readonly (SyntaxRef | CallRef | SignatureRef)[],
    options?: { readonly select?: S },
  ) => Promise<readonly ProjectedDeclarationRow<S>[]>;
  resolveCalls: <
    const S extends CallProjectionSelection = Record<never, never>,
  >(
    targets: readonly SyntaxRef[],
    options?: { readonly select?: S },
  ) => Promise<readonly ProjectedCallResolutionRow<S>[]>;
  projectCalls: <const S extends CallProjectionSelection>(
    calls: readonly CallRef[],
    options: { readonly select: S },
  ) => Promise<readonly ProjectedCallRow<S>[]>;
  projectSignatures: <const S extends SignatureProjectionSelection>(
    signatures: readonly SignatureRef[],
    options: { readonly select: S },
  ) => Promise<readonly ProjectedSignatureRow<S>[]>;
  projectTypes: <const S extends TypeProjectionSelection>(
    types: readonly TypeRef[],
    options: { readonly select: S },
  ) => Promise<readonly ProjectedTypeRow<S>[]>;
  isAssignableTo: (
    pairs: readonly AssignabilityPair[],
  ) => Promise<readonly AssignabilityResult[]>;
  syntaxOf: <const S extends SyntaxLookupSelection = Record<never, never>>(
    targets: readonly (SyntaxRef | DeclarationRef | CallRef | SignatureRef)[],
    options?: { readonly select?: S },
  ) => Promise<readonly ProjectedSyntaxLookupRow<S>[]>;
  /** Registers a local non-source file read by this rule as an additional input. */
  registerAdditionalInput: (filePath: string) => AdditionalInputEntry;
  /** Registers a local non-source file as an additional input and reads its UTF-8 content. */
  readAdditionalInput: (filePath: string) => string;
  callersOf: (
    declarations: readonly DeclarationRef[],
    options?: LookupFilesOptions,
  ) => Promise<readonly DeclarationCallersResult[]>;
  calleesOf: (
    calls: readonly CallRef[],
  ) => Promise<readonly CallTargetsResult[]>;
  resolveImplementations: (
    targets: readonly ImplementationTargetRef[],
  ) => Promise<readonly ImplementationResolutionResult[]>;
  valuePropagation: (
    options: ValuePropagationOptions,
  ) => Promise<ValuePropagationResult>;
  resolveInstances: (
    targets: readonly InstanceTargetRef[],
    options?: LookupFilesOptions,
  ) => Promise<readonly InstanceResolutionResult[]>;
  projectInstances: <
    const S extends InstanceProjectionSelection = Record<never, never>,
  >(
    instances: readonly InstanceRef[],
    options?: { readonly select?: S },
  ) => Promise<readonly ProjectedInstanceRow<S>[]>;
  callEffects: (
    options: CallEffectsOptions,
  ) => Promise<readonly CallEffectsResult[]>;
  instanceCleanup: (
    options: InstanceCleanupOptions,
  ) => Promise<readonly InstanceCleanupScopeResult[]>;
  facts: <SubjectKind extends ProgramObjectKind, Condition, Value>(
    factType: FactType<SubjectKind, Condition, Value>,
    options?: FactQueryOptions<NoInfer<SubjectKind>, NoInfer<Condition>>,
  ) => Promise<readonly FactResult<SubjectKind, Value>[]>;
  report: (diagnostic: DiagnosticInput) => void;
  /**
   * Records a named TS judgment checkpoint for explainability and detail observation.
   * When observation and explanation are disabled, `detail` is never invoked.
   */
  checkpoint: (
    name: string,
    target: CheckpointTarget,
    outcome: CheckpointOutcome,
    detail?: () => unknown,
  ) => void;
};

export type Rule = {
  readonly id: string;
  readonly description?: string;
  readonly run: (ctx: RuleContext) => void | Promise<void>;
};

export type VeruleConfig = {
  readonly tsconfig?: string;
  readonly projects?: readonly string[];
  readonly rules?: readonly Rule[];
  readonly models?: readonly Model[];
  readonly adapters?: readonly ConfiguredAdapter[];
  readonly additionalInputs?: readonly string[];
};

export type NormalizedVeruleConfig = {
  readonly tsconfig?: string;
  readonly projects?: readonly string[];
  readonly rules: readonly Rule[];
  readonly models: readonly Model[];
  readonly adapters: readonly LibraryAdapter[];
  readonly adapterPackages: readonly string[];
  readonly additionalInputs: readonly string[];
};

export type SessionCheckOptions = {
  readonly rulesOverride?: readonly Rule[];
  readonly ruleIds?: readonly string[];
  readonly at?: string | SourceTarget;
  readonly observe?: ObserveMode | ObserveOptions;
  readonly observeLimit?: ObserveLimitInput;
  readonly configLoadMs?: number;
};

export type ExplainOptions = {
  readonly ruleId?: string;
  readonly at: string | SourceTarget;
  readonly observe?: ObserveMode | ObserveOptions;
  readonly observeLimit?: ObserveLimitInput;
  readonly rulesOverride?: readonly Rule[];
};

export type OpenSessionOptions = {
  readonly cwd: string;
  readonly config?: VeruleConfig | NormalizedVeruleConfig;
  /** Directory used to resolve adapter package names; defaults to `cwd`. */
  readonly adapterResolutionRoot?: string;
  readonly observe?: ObserveMode | ObserveOptions;
  readonly observeLimit?: ObserveLimitInput;
  readonly configLoadMs?: number;
};

export type RunCheckOptions = OpenSessionOptions & {
  readonly ruleIds?: readonly string[];
  readonly at?: string | SourceTarget;
};

export type RecomputationScope = {
  readonly recomputedProjects: readonly string[];
  readonly reusedProjects: readonly string[];
  readonly recomputedFiles: readonly string[];
  readonly reusedFiles: readonly string[];
  readonly removedFiles: readonly string[];
  readonly invalidatedBy: readonly string[];
};

export type MemoryStats = {
  readonly hostRssBytes: number;
  readonly engineRssBytes?: number;
};

export type SessionUpdateOptions = {
  readonly changed?: readonly string[];
  readonly created?: readonly string[];
  readonly deleted?: readonly string[];
  readonly config?: VeruleConfig | NormalizedVeruleConfig;
  readonly adapterResolutionRoot?: string;
  readonly rules?: readonly Rule[];
  readonly models?: readonly Model[];
  readonly adapters?: readonly ConfiguredAdapter[];
  readonly entries?: readonly string[];
  readonly additionalInputs?: readonly string[];
};

export type SessionUpdateResult = {
  readonly previousSnapshotId: number;
  readonly snapshotId: number;
  readonly projects: readonly ProjectSummary[];
  readonly fileOrigins: readonly SourceFileOrigin[];
  readonly additionalInputs: readonly AdditionalInputEntry[];
  readonly recomputation: RecomputationScope;
  readonly memory: MemoryStats;
};

export type AnalysisSession = {
  readonly snapshotId: number;
  readonly projects: readonly ProjectSummary[];
  readonly fileOrigins: readonly SourceFileOrigin[];
  readonly additionalInputs: readonly AdditionalInputEntry[];
  readonly memory: MemoryStats;
  registerAdditionalInput: (
    filePath: string,
    source: AdditionalInputSource,
  ) => AdditionalInputEntry;
  check: (
    optionsOrRules?: readonly Rule[] | SessionCheckOptions,
  ) => Promise<CheckReport>;
  update: (options?: SessionUpdateOptions) => Promise<SessionUpdateResult>;
  explain: (options: ExplainOptions) => Promise<ExplainReport>;
  close: () => Promise<void>;
};

export type WatchTrigger = "initial" | "change";

export type WatchConfigLoadResult = {
  readonly config: VeruleConfig;
  readonly watchFiles?: readonly string[];
  readonly adapterResolutionRoot?: string;
};

export type WatchProjectOptions = {
  readonly cwd: string;
  readonly config?: VeruleConfig;
  /** Directory used to resolve adapter package names; defaults to `cwd`. */
  readonly adapterResolutionRoot?: string;
  readonly loadConfig?: () => Promise<WatchConfigLoadResult>;
  readonly watchFiles?: readonly string[];
  readonly observe?: ObserveMode;
  readonly debounceMs?: number;
  readonly signal?: AbortSignal;
};

export type WatchReportEvent = {
  readonly kind: "report";
  readonly sequence: number;
  readonly trigger: WatchTrigger;
  readonly snapshotId: number;
  readonly report: CheckReport;
  readonly recomputation: RecomputationScope;
  readonly memory: MemoryStats;
};

export type WatchFailureEvent = {
  readonly kind: "failed";
  readonly sequence: number;
  readonly trigger: WatchTrigger;
  readonly code: VeruleErrorCode;
  readonly message: string;
  readonly conflict?: FactConflictDetails;
};

export type WatchEvent = WatchReportEvent | WatchFailureEvent;

export type SharedSessionOptions = {
  readonly cwd: string;
  readonly endpoint: string;
  readonly config?: VeruleConfig;
  /** Directory used to resolve adapter package names; defaults to `cwd`. */
  readonly adapterResolutionRoot?: string;
  readonly loadConfig?: () => Promise<WatchConfigLoadResult>;
  readonly watchFiles?: readonly string[];
  readonly observe?: ObserveMode;
  readonly observeLimit?: ObserveLimitInput;
  readonly debounceMs?: number;
  readonly signal?: AbortSignal;
};

export type SharedSession = {
  readonly endpoint: string;
  readonly snapshotId: number;
  readonly projects: readonly ProjectSummary[];
  readonly closed: boolean;
  waitUntilClosed: () => Promise<void>;
  close: () => Promise<void>;
};

export type RequestSharedReportOptions = {
  readonly endpoint: string;
  readonly cwd?: string;
  readonly ruleIds?: readonly string[];
  readonly at?: string | SourceTarget;
  readonly observe?: ObserveMode | ObserveOptions;
  readonly observeLimit?: ObserveLimitInput;
  readonly timeoutMs?: number;
};
