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
import type {
  ConfiguredAdapter,
  FactQueryOptions,
  FactResult,
  FactType,
} from "./facts.ts";
import type {
  InstanceModelBehavior,
  InstanceProjectionSelection,
  InstanceResolutionResult,
  InstanceTargetRef,
  ProjectedInstanceRow,
} from "./instance-identity.ts";
import type { EngineCallIoMetrics, QueryObserver } from "./observation.ts";
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

export type TssaeErrorCode =
  | "INVALID_CONFIG"
  | "PROJECT_LOAD_ERROR"
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

/** Execution failure raised when project configuration, loading, queries, or adapters fail. */
export class TssaeError extends Error {
  readonly code: TssaeErrorCode;
  readonly conflict?: FactConflictDetails;

  constructor(
    code: TssaeErrorCode,
    message: string,
    options?: ErrorOptions & { readonly conflict?: FactConflictDetails },
  ) {
    super(message, options);
    this.name = "TssaeError";
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

export type SyntaxKind =
  | "Identifier"
  | "CallExpression"
  | "NewExpression"
  | "TaggedTemplateExpression"
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

/**
 * Snapshot-bound typed handle representing an analyzed TypeScript/JavaScript program object.
 * Valid only within the originating project and snapshot.
 */
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

/** Typed query context bound to one opened project and active snapshot. */
export type ProjectContext = {
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
  /** Registers a local non-source file as an additional input. */
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
};

export type OpenProjectOptions = {
  readonly cwd: string;
  readonly tsconfig?: string;
  readonly projects?: readonly string[];
  readonly models?: readonly Model[];
  readonly adapters?: readonly ConfiguredAdapter[];
  readonly additionalInputs?: readonly string[];
  /** Directory used to resolve adapter package names; defaults to `cwd`. */
  readonly adapterResolutionRoot?: string;
  readonly observe?: ObserveMode | ObserveOptions;
  readonly onRpcMetrics?: (metrics: EngineCallIoMetrics) => void;
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
  readonly adapterResolutionRoot?: string;
  readonly models?: readonly Model[];
  readonly adapters?: readonly ConfiguredAdapter[];
  readonly entries?: readonly string[];
  readonly additionalInputs?: readonly string[];
  readonly observe?: ObserveMode | ObserveOptions;
  readonly onRpcMetrics?: (metrics: EngineCallIoMetrics) => void;
};

export type SessionUpdateResult = {
  readonly previousSnapshotId: number;
  readonly snapshotId: number;
  readonly projects: readonly ProjectSummary[];
  readonly fileOrigins: readonly SourceFileOrigin[];
  readonly workspaceSourceFiles: readonly string[];
  readonly additionalInputs: readonly AdditionalInputEntry[];
  readonly recomputation: RecomputationScope;
  readonly memory: MemoryStats;
};

/** Comment syntax kind reported by `ProjectSession.scanComments`. */
export type CommentKind = "line" | "block";

/** Options passed to `ProjectSession.scanComments`. */
export type ScanCommentsOptions = {
  /** Optional relative or absolute file paths to restrict scanning to. */
  readonly files?: readonly string[];
  readonly observe?: ObserveMode | ObserveOptions;
  readonly onRpcMetrics?: (metrics: EngineCallIoMetrics) => void;
};

/** Options passed to `ProjectSession.project`. */
export type ProjectContextOptions = {
  readonly observer?: QueryObserver;
};

/** Standalone comment and immediately following code line returned by `ProjectSession.scanComments`. */
export type ScannedComment = {
  readonly kind: CommentKind;
  /** Normalized comment body text with delimiters and multiline `*` prefixes stripped. */
  readonly text: string;
  /** Exact source range of the standalone comment (or JSX empty-expression comment wrapper). */
  readonly at: DiagnosticLocation;
  /** 1-based line number of the first code line after this comment block, or `null` when interrupted by a blank line or EOF. */
  readonly nextCodeLine: number | null;
  /** Whether the comment appeared inside a JSX empty expression wrapper. */
  readonly isJsx: boolean;
};

/**
 * Snapshot-managed TSSAE project session backed by a bundled Rust engine process.
 * Call `close()` to release snapshots and terminate the child process.
 */
export type ProjectSession = {
  readonly snapshotId: number;
  readonly snapshotDigest: string;
  readonly projects: readonly ProjectSummary[];
  readonly fileOrigins: readonly SourceFileOrigin[];
  readonly workspaceSourceFiles: readonly string[];
  readonly additionalInputs: readonly AdditionalInputEntry[];
  readonly nativeAdapters: readonly NativeAdapterObservation[];
  readonly memory: MemoryStats;
  readonly projectContexts: readonly ProjectContext[];
  project: (
    target?: string | ProjectSummary,
    options?: ProjectContextOptions,
  ) => ProjectContext;
  scanComments: (
    options?: ScanCommentsOptions,
  ) => Promise<readonly ScannedComment[]>;
  registerAdditionalInput: (
    filePath: string,
    source: AdditionalInputSource,
  ) => AdditionalInputEntry;
  update: (options?: SessionUpdateOptions) => Promise<SessionUpdateResult>;
  close: () => Promise<void>;
};
