import { isAbsolute, relative, resolve } from "node:path";
import { performance } from "node:perf_hooks";

import {
  createAdditionalInputRegistry,
  createScopedAdditionalInputRegistrar,
  validateAdditionalInputPaths,
  type AdditionalInputEntry,
  type AdditionalInputRegistrar,
  type AdditionalInputSource,
} from "./additional-inputs.ts";
import {
  assertValidProgramRef,
  toBranchCoverageItem,
  toFunctionBranchesResult,
  type BranchCoverageItem,
  type BranchCoverageOptions,
  type EngineBranchCoverageItem,
  type EngineFunctionBranchesItem,
  type FunctionBranchesResult,
} from "./branch-coverage.ts";
import {
  createCallEffectsCache,
  createCallEffectsQueries,
} from "./call-effects.ts";
import {
  createCallsValuesCache,
  createCallsValuesQueries,
} from "./calls-values.ts";
import { EngineClient } from "./engine-client.ts";
import {
  recordSyntaxLocation,
  registerProvenance,
  summarizeSyntaxSnippet,
} from "./evidence.ts";
import {
  FactCache,
  executeFactQuery,
  validateAdapters,
  type AdapterContext,
  type EngineFactExtractionOptions,
  type EngineNativeSubQueryTrace,
  type FactContribution,
  type FactExplainTrace,
  type FactQueryOptions,
  type FactType,
  type LibraryAdapter,
} from "./facts.ts";
import {
  createInstanceIdentityCache,
  createInstanceIdentityQueries,
} from "./instance-identity.ts";
import { resolveNativeAdapterPackages } from "./native-adapters.ts";
import {
  ObservationCollector,
  buildRpcRequestOptions,
  locationMatchesTarget,
  normalizeObserveConfig,
} from "./observation.ts";
import {
  createPackageImplCache,
  createPackageImplQueries,
} from "./package-impl.ts";
import { createProjectionQueries, validateProgramRefs } from "./projection.ts";
import { buildFileOrigins, resolveProjectEntries } from "./projects.ts";
import {
  discoverAndLoadSummaries,
  normalizeSummaryOptions,
  type NormalizedSummaryOptions,
  type ResolvedPackageSummary,
  type SummaryConfigPayload,
} from "./summaries.ts";
import {
  TssaeError,
  makeProgramRef,
  type CommentKind,
  type DiagnosticLocation,
  type Evidence,
  type FindSyntaxOptions,
  type Gap,
  type MemoryStats,
  type NativeAdapterObservation,
  type OpenProjectOptions,
  type ProgramObjectKind,
  type ProgramRef,
  type ProjectContext,
  type ProjectContextOptions,
  type ProjectSession,
  type ProjectSummary,
  type RecomputationScope,
  type ScanCommentsOptions,
  type ScannedComment,
  type SessionUpdateOptions,
  type SyntaxKind,
  type SyntaxNodeResult,
  type SyntaxRef,
  type TypeAtResult,
} from "./types.ts";

type EngineOpenResult = {
  readonly snapshotId: number;
  readonly snapshotDigest: string;
  readonly projects: readonly ProjectSummary[];
  readonly workspaceSourceFiles: readonly string[];
  readonly nativeAdapters: readonly NativeAdapterObservation[];
  readonly resolvedPackages?: readonly ResolvedPackageSummary[];
  readonly engineRssBytes?: number;
};

type EngineUpdateResult = EngineOpenResult & {
  readonly recomputedProjects: readonly string[];
  readonly reusedProjects: readonly string[];
  readonly recomputedFiles: readonly string[];
  readonly reusedFiles: readonly string[];
  readonly removedFiles: readonly string[];
};

type NormalizedSessionConfig = {
  readonly tsconfig?: string;
  readonly projects?: readonly string[];
  readonly adapters: readonly LibraryAdapter[];
  readonly adapterPackages: readonly string[];
  readonly summaries: NormalizedSummaryOptions;
  readonly additionalInputs: readonly string[];
};

type EngineSyntaxItem = {
  readonly ref: string;
  readonly kind: SyntaxKind;
  readonly text: string;
  readonly location: DiagnosticLocation;
  readonly expression?: string;
  readonly arguments?: readonly string[];
  readonly name?: string;
  readonly initializer?: string;
  readonly body?: string;
  readonly parameters?: readonly string[];
};

type EngineTypeItem = {
  readonly target: string;
  readonly typeRef: string;
  readonly typeText: string;
  readonly isUnion: boolean;
  readonly isIntersection: boolean;
  readonly isLiteral: boolean;
  readonly literalValue?: string | number | boolean;
  readonly unionMembers?: readonly string[];
  readonly gaps?: readonly Gap[];
};

type EngineBatchResult = {
  readonly results: readonly {
    readonly items: readonly unknown[];
    readonly additionalInputs?: readonly string[];
    readonly adapterInputs?: readonly {
      readonly adapterId: string;
      readonly path: string;
    }[];
    readonly subQueries?: readonly EngineNativeSubQueryTrace[];
  }[];
};

function validateAdapterPackageNames(
  packageNames: readonly string[],
): readonly string[] {
  const seen = new Set<string>();
  const normalized: string[] = [];
  for (const rawName of packageNames) {
    if (typeof rawName !== "string" || rawName.trim() === "") {
      throw new TssaeError(
        "INVALID_CONFIG",
        "Adapter package names must be non-empty strings.",
      );
    }
    const packageName = rawName.trim();
    if (seen.has(packageName)) {
      throw new TssaeError(
        "INVALID_CONFIG",
        `Duplicate adapter package name: "${packageName}".`,
      );
    }
    seen.add(packageName);
    normalized.push(packageName);
  }
  return normalized;
}

function normalizeOpenOptions(
  options: OpenProjectOptions,
): NormalizedSessionConfig {
  resolveProjectEntries(options);
  const configuredAdapters = options.adapters ?? [];
  const adapters = validateAdapters(
    configuredAdapters.filter(
      (adapter): adapter is LibraryAdapter => typeof adapter !== "string",
    ),
  );
  const adapterPackages = validateAdapterPackageNames(
    configuredAdapters.filter(
      (adapter): adapter is string => typeof adapter === "string",
    ),
  );
  const summaries = normalizeSummaryOptions(options.summaries);
  const additionalInputs = validateAdditionalInputPaths(
    options.additionalInputs,
  );
  return {
    ...(options.tsconfig !== undefined ? { tsconfig: options.tsconfig } : {}),
    ...(options.projects !== undefined
      ? { projects: [...options.projects] }
      : {}),
    adapters,
    adapterPackages,
    summaries,
    additionalInputs,
  };
}

function combineConfiguredAdapters(
  configuredAdapters: readonly LibraryAdapter[],
  packageAdapters: readonly LibraryAdapter[],
  packageNames: readonly string[],
): readonly LibraryAdapter[] {
  try {
    return validateAdapters([...configuredAdapters, ...packageAdapters]);
  } catch (error) {
    if (packageNames.length === 0) throw error;
    const detail = error instanceof Error ? error.message : String(error);
    throw new TssaeError(
      "INVALID_CONFIG",
      `Configured adapter package(s) ${packageNames.map((name) => `"${name}"`).join(", ")} could not be registered: ${detail}`,
      { cause: error },
    );
  }
}

function stableSerialize(value: unknown): string {
  if (value === null || typeof value !== "object") {
    return JSON.stringify(value) ?? "undefined";
  }
  if (Array.isArray(value)) {
    return `[${value.map((item) => stableSerialize(item)).join(",")}]`;
  }
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record).toSorted();
  return `{${keys
    .filter((key) => record[key] !== undefined)
    .map((key) => `${JSON.stringify(key)}:${stableSerialize(record[key])}`)
    .join(",")}}`;
}

function summarizeAdapter(adapter: LibraryAdapter) {
  return {
    id: adapter.id,
    version: adapter.version ?? null,
    provides: adapter.provides
      .map((provider) => provider.factType.id)
      .toSorted(),
  };
}

function toSyntaxNodeResult(
  item: EngineSyntaxItem,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
  syntaxLocations: WeakMap<SyntaxRef, DiagnosticLocation>,
): SyntaxNodeResult {
  const ref = makeProgramRef(
    "syntax",
    sessionToken,
    projectId,
    snapshotId,
    item.ref,
  );
  syntaxLocations.set(ref, item.location);
  recordSyntaxLocation(ref, item.location);

  return registerProvenance(
    {
      ref,
      kind: item.kind,
      text: item.text,
      location: item.location,
      ...(item.expression !== undefined
        ? {
            expression: makeProgramRef(
              "syntax",
              sessionToken,
              projectId,
              snapshotId,
              item.expression,
            ),
          }
        : {}),
      ...(item.arguments !== undefined
        ? {
            arguments: item.arguments.map((argId) =>
              makeProgramRef(
                "syntax",
                sessionToken,
                projectId,
                snapshotId,
                argId,
              ),
            ),
          }
        : {}),
      ...(item.name !== undefined
        ? {
            name: makeProgramRef(
              "syntax",
              sessionToken,
              projectId,
              snapshotId,
              item.name,
            ),
          }
        : {}),
      ...(item.initializer !== undefined
        ? {
            initializer: makeProgramRef(
              "syntax",
              sessionToken,
              projectId,
              snapshotId,
              item.initializer,
            ),
          }
        : {}),
      ...(item.body !== undefined
        ? {
            body: makeProgramRef(
              "syntax",
              sessionToken,
              projectId,
              snapshotId,
              item.body,
            ),
          }
        : {}),
      ...(item.parameters !== undefined
        ? {
            parameters: item.parameters.map((paramId) =>
              makeProgramRef(
                "syntax",
                sessionToken,
                projectId,
                snapshotId,
                paramId,
              ),
            ),
          }
        : {}),
    },
    [
      {
        kind: "source",
        message: `${item.kind}: ${summarizeSyntaxSnippet(item.text)}`,
        at: item.location,
      },
    ],
  );
}

function toTypeAtResult(
  item: EngineTypeItem,
  target: SyntaxRef,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
): TypeAtResult {
  return {
    target,
    ref: makeProgramRef(
      "type",
      sessionToken,
      projectId,
      snapshotId,
      item.typeRef,
    ),
    typeText: item.typeText,
    isUnion: item.isUnion,
    isIntersection: item.isIntersection,
    isLiteral: item.isLiteral,
    ...(item.literalValue !== undefined
      ? { literalValue: item.literalValue }
      : {}),
    ...(item.unionMembers !== undefined
      ? { unionMembers: item.unionMembers }
      : {}),
  };
}

function resolveNextConfig(
  current: NormalizedSessionConfig,
  options?: SessionUpdateOptions,
): NormalizedSessionConfig {
  const adapters = options?.adapters
    ? validateAdapters(
        options.adapters.filter(
          (adapter): adapter is LibraryAdapter => typeof adapter !== "string",
        ),
      )
    : current.adapters;
  const adapterPackages = options?.adapters
    ? validateAdapterPackageNames(
        options.adapters.filter(
          (adapter): adapter is string => typeof adapter === "string",
        ),
      )
    : current.adapterPackages;
  const summaries =
    options?.summaries !== undefined
      ? normalizeSummaryOptions(options.summaries)
      : current.summaries;
  const additionalInputs =
    options?.additionalInputs !== undefined
      ? validateAdditionalInputPaths(options.additionalInputs)
      : current.additionalInputs;
  if (options?.entries !== undefined) {
    if (options.entries.length === 0) {
      throw new TssaeError(
        "INVALID_CONFIG",
        '"entries" must contain at least one tsconfig path when specified.',
      );
    }
    return {
      projects: [...options.entries],
      adapters,
      adapterPackages,
      summaries,
      additionalInputs,
    };
  }
  return {
    ...(current.tsconfig !== undefined ? { tsconfig: current.tsconfig } : {}),
    ...(current.projects !== undefined ? { projects: current.projects } : {}),
    adapters,
    adapterPackages,
    summaries,
    additionalInputs,
  };
}

function collectInvalidatedBy(
  prevConfig: NormalizedSessionConfig,
  nextConfig: NormalizedSessionConfig,
  options?: SessionUpdateOptions,
  changedAdditionalFiles: readonly string[] = [],
): readonly string[] {
  const reasons: string[] = [];
  const changedSet = new Set<string>(options?.changed ?? []);
  for (const file of options?.changed ?? []) reasons.push(`changed:${file}`);
  for (const file of options?.created ?? []) reasons.push(`created:${file}`);
  for (const file of options?.deleted ?? []) reasons.push(`deleted:${file}`);
  for (const file of changedAdditionalFiles) {
    if (!changedSet.has(file)) {
      reasons.push(`additional-input:${file}`);
    }
  }

  const prevEntries = resolveProjectEntries(prevConfig);
  const nextEntries = resolveProjectEntries(nextConfig);
  if (stableSerialize(prevEntries) !== stableSerialize(nextEntries)) {
    reasons.push("entries");
  }
  if (
    stableSerialize(prevConfig.additionalInputs) !==
    stableSerialize(nextConfig.additionalInputs)
  ) {
    reasons.push("additionalInputs");
  }
  if (
    stableSerialize(
      prevConfig.adapters
        .map(summarizeAdapter)
        .toSorted((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)),
    ) !==
    stableSerialize(
      nextConfig.adapters
        .map(summarizeAdapter)
        .toSorted((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)),
    )
  ) {
    reasons.push("adapters");
  }
  if (
    stableSerialize(prevConfig.adapterPackages) !==
    stableSerialize(nextConfig.adapterPackages)
  ) {
    reasons.push("adapterPackages");
  }
  if (
    stableSerialize(prevConfig.summaries) !==
    stableSerialize(nextConfig.summaries)
  ) {
    reasons.push("summaries");
  }
  return reasons;
}

type EngineCommentsScanResult = {
  readonly comments: readonly {
    readonly kind: CommentKind;
    readonly text: string;
    readonly at: DiagnosticLocation;
    readonly nextCodeLine: number | null;
    readonly isJsx: boolean;
  }[];
};

function normalizeScanFilePaths(
  cwd: string,
  files: readonly string[] | undefined,
): readonly string[] | undefined {
  if (files === undefined) return undefined;
  const seen = new Set<string>();
  const out: string[] = [];
  for (const rawFile of files) {
    const slashed = rawFile.replaceAll("\\", "/").trim();
    let rel =
      isAbsolute(slashed) || /^[A-Za-z]:\//u.test(slashed)
        ? relative(cwd, slashed).replaceAll("\\", "/")
        : slashed;
    while (rel.startsWith("./")) {
      rel = rel.slice(2);
    }
    if (rel !== "" && !seen.has(rel)) {
      seen.add(rel);
      out.push(rel);
    }
  }
  return out.toSorted();
}

function readMemoryStats(engineRssBytes?: number): MemoryStats {
  return {
    hostRssBytes: process.memoryUsage().rss,
    ...(engineRssBytes !== undefined ? { engineRssBytes } : {}),
  };
}

/**
 * Opens a TSSAE static analysis session for the TypeScript/JavaScript project(s) under `options.cwd`.
 *
 * Call `session.close()` when finished to release
 * the snapshot and terminate the bundled Rust engine subprocess.
 */
export async function openProject(
  options: OpenProjectOptions,
): Promise<ProjectSession> {
  let normalizedConfig = normalizeOpenOptions(options);
  const absCwd = resolve(options.cwd);
  let adapterResolutionRoot = resolve(options.adapterResolutionRoot ?? absCwd);
  let packageResolution = await resolveNativeAdapterPackages(
    normalizedConfig.adapterPackages,
    adapterResolutionRoot,
  );
  let activeAdapters = combineConfiguredAdapters(
    normalizedConfig.adapters,
    packageResolution.adapters,
    normalizedConfig.adapterPackages,
  );
  const initialEntries = resolveProjectEntries(normalizedConfig);

  const client = new EngineClient();
  let openResult: EngineOpenResult;
  let currentSummaryPayload: SummaryConfigPayload;
  let currentSummaryFiles: readonly {
    readonly filePath: string;
    readonly packId: string;
  }[];
  try {
    openResult = await client.request<EngineOpenResult>(
      "snapshot.open",
      {
        cwd: absCwd,
        entries: initialEntries,
        nativeAdapters: packageResolution.nativeAdapters,
      },
      buildRpcRequestOptions(options.observe, options.onRpcMetrics),
    );
    const discovered = await discoverAndLoadSummaries({
      cwd: absCwd,
      projectConfigFiles: openResult.projects.map((p) => p.configFile),
      resolvedPackages: openResult.resolvedPackages ?? [],
      options: normalizedConfig.summaries,
    });
    currentSummaryPayload = discovered.payload;
    currentSummaryFiles = discovered.registeredFiles;
    if (
      currentSummaryPayload.packs.length > 0 ||
      !currentSummaryPayload.enableBuiltin
    ) {
      await client.request("snapshot.setSummaries", {
        snapshotId: openResult.snapshotId,
        summaries: currentSummaryPayload,
      });
    }
  } catch (err) {
    await client.close();
    throw err;
  }

  const sessionToken = Symbol("tssae-session");
  let currentSnapshotId = openResult.snapshotId;
  let currentSnapshotDigest = openResult.snapshotDigest;
  let currentProjects = openResult.projects;
  let currentWorkspaceSourceFiles = openResult.workspaceSourceFiles;
  let currentNativeAdapters = openResult.nativeAdapters;
  let currentFileOrigins = buildFileOrigins(openResult.projects);
  let lastEngineRssBytes = openResult.engineRssBytes;
  let closed = false;

  const syntaxCache = new Map<string, Promise<readonly SyntaxNodeResult[]>>();
  const typeCache = new Map<
    string,
    Promise<{ readonly result: TypeAtResult; readonly gaps: readonly Gap[] }>
  >();
  const branchListCache = new Map<string, Promise<FunctionBranchesResult>>();
  const callsValuesCache = createCallsValuesCache();
  const callEffectsCache = createCallEffectsCache();
  const instanceCache = createInstanceIdentityCache();
  const packageImplCache = createPackageImplCache();
  const factCache = new FactCache();
  const commentScanCache = new Map<
    string,
    Promise<readonly ScannedComment[]>
  >();
  const syntaxLocations = new WeakMap<SyntaxRef, DiagnosticLocation>();

  const explicitRegistrations: {
    readonly filePath: string;
    readonly source: AdditionalInputSource;
  }[] = [];
  const dynamicRegistrations: {
    readonly filePath: string;
    readonly source: AdditionalInputSource;
  }[] = [];

  function buildSessionRegistry(
    configInputs: readonly string[],
    explicit: readonly {
      readonly filePath: string;
      readonly source: AdditionalInputSource;
    }[],
    dynamic: readonly {
      readonly filePath: string;
      readonly source: AdditionalInputSource;
    }[],
    summaryFiles: readonly {
      readonly filePath: string;
      readonly packId: string;
    }[],
  ) {
    const registry = createAdditionalInputRegistry({ cwd: absCwd });
    for (const filePath of configInputs) {
      registry.registerAdditionalInput(filePath, { kind: "config" });
    }
    for (const item of explicit) {
      registry.registerAdditionalInput(item.filePath, item.source);
    }
    for (const item of dynamic) {
      registry.registerAdditionalInput(item.filePath, item.source);
    }
    for (const item of summaryFiles) {
      registry.registerAdditionalInput(item.filePath, {
        kind: "summary",
        id: item.packId,
      });
    }
    return registry;
  }

  let sessionRegistry = buildSessionRegistry(
    normalizedConfig.additionalInputs,
    explicitRegistrations,
    dynamicRegistrations,
    currentSummaryFiles,
  );

  const dynamicRegistrar: AdditionalInputRegistrar = {
    cwd: absCwd,
    registerAdditionalInput: (filePath, source) => {
      dynamicRegistrations.push({ filePath, source });
      return sessionRegistry.registerAdditionalInput(filePath, source);
    },
    readAdditionalInput: (filePath, source) => {
      dynamicRegistrations.push({ filePath, source });
      return sessionRegistry.readAdditionalInput(filePath, source);
    },
    listAdditionalInputs: () => sessionRegistry.listAdditionalInputs(),
    getTrackedAbsolutePaths: () => sessionRegistry.getTrackedAbsolutePaths(),
  };

  function assertOpen(): void {
    if (closed) {
      throw new TssaeError(
        "SESSION_CLOSED",
        "Analysis session is already closed.",
      );
    }
  }

  function createProjectContext(
    project: ProjectSummary,
    activeSnapshotId: number,
    contextOptions?: ProjectContextOptions,
  ): ProjectContext {
    const assertActiveSnapshot = (): void => {
      assertOpen();
      if (activeSnapshotId !== currentSnapshotId) {
        throw new TssaeError(
          "STALE_SNAPSHOT",
          `Snapshot ${String(activeSnapshotId)} has been superseded by snapshot ${String(currentSnapshotId)}.`,
        );
      }
    };

    const observer = contextOptions?.observer;
    const normalizedObserve = normalizeObserveConfig(observer?.observe);
    const collector = new ObservationCollector({
      mode: normalizedObserve.mode,
      ...(normalizedObserve.limit !== undefined
        ? { limit: normalizedObserve.limit }
        : {}),
      ...(observer?.explainTarget !== undefined
        ? { explainTarget: observer.explainTarget }
        : {}),
      ...(observer?.onQueryCompleted !== undefined
        ? { onQueryCompleted: observer.onQueryCompleted }
        : {}),
      ...(observer?.onFactExplain !== undefined
        ? { onFactExplain: observer.onFactExplain }
        : {}),
      ...(observer?.onProviderEvaluation !== undefined
        ? { onProviderEvaluation: observer.onProviderEvaluation }
        : {}),
    });
    const getRpcOptions = () =>
      buildRpcRequestOptions(observer?.observe, observer?.onRpcMetrics);
    const recordGaps = (gaps: readonly Gap[]): void => {
      if (gaps.length > 0) {
        observer?.onGaps?.(gaps);
      }
    };
    const scopedInputs = createScopedAdditionalInputRegistrar(
      dynamicRegistrar,
      { kind: "config" },
    );

    const recordQueryEvent = (
      op: string,
      cacheStatus: "computed" | "reused",
      candidateCount: number,
      durationMs: number,
    ): void => {
      if (!collector.isActive()) return;
      collector.recordQuery({
        op,
        projectId: project.projectId,
        provider: "engine",
        cacheStatus,
        candidateCount,
        durationMs,
        summary: `${op} -> ${candidateCount} item(s)`,
      });
    };

    const projectionQueries = createProjectionQueries({
      client,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      assertOpen: assertActiveSnapshot,
      recordGaps,
      getRpcOptions,
      ...(collector.isActive()
        ? {
            onQueryCompleted: (op, candidateCount, durationMs) => {
              recordQueryEvent(op, "computed", candidateCount, durationMs);
            },
          }
        : {}),
    });

    const callsValuesQueries = createCallsValuesQueries({
      client,
      cache: callsValuesCache,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      cwd: absCwd,
      sourceFiles: project.sourceFiles,
      assertOpen: assertActiveSnapshot,
      recordGaps,
      getRpcOptions,
      ...(collector.isActive() ? { onQueryCompleted: recordQueryEvent } : {}),
    });

    const instanceQueries = createInstanceIdentityQueries({
      client,
      cache: instanceCache,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      cwd: absCwd,
      sourceFiles: project.sourceFiles,
      assertOpen: assertActiveSnapshot,
      recordGaps,
      getRpcOptions,
      ...(collector.isActive() ? { onQueryCompleted: recordQueryEvent } : {}),
    });

    const callEffectsQueries = createCallEffectsQueries({
      client,
      cache: callEffectsCache,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      cwd: absCwd,
      sourceFiles: project.sourceFiles,
      assertOpen: assertActiveSnapshot,
      recordGaps,
      resolveSyntaxLocation: (ref) => syntaxLocations.get(ref as SyntaxRef),
      syntaxLocations,
      ...(observer?.onCleanupCovered !== undefined
        ? { onCleanupCovered: observer.onCleanupCovered }
        : {}),
      getRpcOptions,
      ...(collector.isActive() ? { onQueryCompleted: recordQueryEvent } : {}),
    });

    const packageImplQueries = createPackageImplQueries({
      client,
      cache: packageImplCache,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      assertOpen: assertActiveSnapshot,
      recordGaps,
      getRpcOptions,
      ...(collector.isActive() ? { onQueryCompleted: recordQueryEvent } : {}),
    });

    const adapterProjectionQueries = createProjectionQueries({
      client,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      assertOpen: assertActiveSnapshot,
      recordGaps: () => {},
      getRpcOptions,
    });

    const adapterCallsValuesQueries = createCallsValuesQueries({
      client,
      cache: callsValuesCache,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      cwd: absCwd,
      sourceFiles: project.sourceFiles,
      assertOpen: assertActiveSnapshot,
      recordGaps: () => {},
      getRpcOptions,
    });

    const adapterCallEffectsQueries = createCallEffectsQueries({
      client,
      cache: callEffectsCache,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      cwd: absCwd,
      sourceFiles: project.sourceFiles,
      assertOpen: assertActiveSnapshot,
      recordGaps: () => {},
      getRpcOptions,
    });

    const adapterPackageImplQueries = createPackageImplQueries({
      client,
      cache: packageImplCache,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      assertOpen: assertActiveSnapshot,
      recordGaps: () => {},
      getRpcOptions,
    });

    const runFindSyntax = async (
      query: FindSyntaxOptions,
      recordRuleQuery = true,
    ): Promise<readonly SyntaxNodeResult[]> => {
      assertActiveSnapshot();
      const cacheKey = JSON.stringify([
        activeSnapshotId,
        project.projectId,
        query.kinds ? query.kinds.toSorted() : null,
        query.files ? query.files.toSorted() : null,
        query.range ?? null,
        query.textIncludes ? query.textIncludes.toSorted() : null,
        query.calleeNames ? query.calleeNames.toSorted() : null,
      ]);
      const queryStart = collector.isActive() ? performance.now() : 0;
      const cached = syntaxCache.get(cacheKey);
      let items: readonly SyntaxNodeResult[];
      let cacheStatus: "computed" | "reused";
      if (cached) {
        cacheStatus = "reused";
        items = await cached;
      } else {
        cacheStatus = "computed";
        const promise = client
          .request<EngineBatchResult>(
            "query.batch",
            {
              snapshotId: activeSnapshotId,
              projectId: project.projectId,
              steps: [
                {
                  op: "findSyntax",
                  ...(query.kinds !== undefined ? { kinds: query.kinds } : {}),
                  ...(query.files !== undefined ? { files: query.files } : {}),
                  ...(query.range !== undefined ? { range: query.range } : {}),
                  ...(query.textIncludes !== undefined
                    ? { textIncludes: query.textIncludes }
                    : {}),
                  ...(query.calleeNames !== undefined
                    ? { calleeNames: query.calleeNames }
                    : {}),
                },
              ],
            },
            getRpcOptions(),
          )
          .then((batch) => {
            const rawItems = (batch.results[0]?.items ??
              []) as readonly EngineSyntaxItem[];
            return rawItems.map((item) =>
              toSyntaxNodeResult(
                item,
                sessionToken,
                project.projectId,
                activeSnapshotId,
                syntaxLocations,
              ),
            );
          });
        syntaxCache.set(cacheKey, promise);
        items = await promise;
      }

      if (collector.isActive() && recordRuleQuery) {
        const durationMs = performance.now() - queryStart;
        const matchedLocations = collector.explainTarget
          ? items
              .filter((item) =>
                locationMatchesTarget(item.location, collector.explainTarget!),
              )
              .map((item) => item.location)
          : undefined;
        const kindsLabel = query.kinds ? query.kinds.join(", ") : "range";
        collector.recordQuery({
          op: "findSyntax",
          projectId: project.projectId,
          provider: "engine",
          cacheStatus,
          candidateCount: items.length,
          durationMs,
          summary: `findSyntax(${kindsLabel}) -> ${items.length} candidate(s)`,
          ...(matchedLocations !== undefined ? { matchedLocations } : {}),
          ...(collector.mode === "detail" || collector.isExplainEnabled()
            ? {
                detail: {
                  ...(query.kinds !== undefined ? { kinds: query.kinds } : {}),
                  ...(query.files !== undefined ? { files: query.files } : {}),
                  ...(query.range !== undefined ? { range: query.range } : {}),
                  ...(query.textIncludes !== undefined
                    ? { textIncludes: query.textIncludes }
                    : {}),
                  ...(query.calleeNames !== undefined
                    ? { calleeNames: query.calleeNames }
                    : {}),
                },
              }
            : {}),
        });
      }

      return items;
    };

    const runTypeAt = async (
      targets: readonly SyntaxRef[],
      recordRuleQuery = true,
    ): Promise<readonly TypeAtResult[]> => {
      assertActiveSnapshot();
      if (targets.length === 0) return [];
      validateProgramRefs(
        targets,
        sessionToken,
        activeSnapshotId,
        project.projectId,
      );

      const missingTargets: SyntaxRef[] = [];
      for (const target of targets) {
        const key = `${activeSnapshotId}:${project.projectId}:${target.handleId}`;
        if (!typeCache.has(key)) {
          missingTargets.push(target);
        }
      }

      const queryStart = collector.isActive() ? performance.now() : 0;
      const cacheStatus: "computed" | "reused" =
        missingTargets.length > 0 ? "computed" : "reused";

      if (missingTargets.length > 0) {
        const batchPromise = client.request<EngineBatchResult>(
          "query.batch",
          {
            snapshotId: activeSnapshotId,
            projectId: project.projectId,
            steps: [
              {
                op: "typeAtLocation",
                targets: missingTargets.map((target) => target.handleId),
              },
            ],
          },
          getRpcOptions(),
        );

        for (const [index, target] of missingTargets.entries()) {
          const key = `${activeSnapshotId}:${project.projectId}:${target.handleId}`;
          const itemPromise = batchPromise.then((batch) => {
            const items = (batch.results[0]?.items ??
              []) as readonly EngineTypeItem[];
            const raw = items[index];
            if (!raw) {
              throw new TssaeError(
                "ENGINE_ERROR",
                `Engine omitted type result for target "${target.handleId}".`,
              );
            }
            return {
              result: toTypeAtResult(
                raw,
                target,
                sessionToken,
                project.projectId,
                activeSnapshotId,
              ),
              gaps: raw.gaps ?? [],
            };
          });
          typeCache.set(key, itemPromise);
        }
      }

      const cachedEntries = await Promise.all(
        targets.map((target) =>
          typeCache.get(
            `${activeSnapshotId}:${project.projectId}:${target.handleId}`,
          )!,
        ),
      );
      if (recordRuleQuery) {
        for (const entry of cachedEntries) {
          recordGaps(entry.gaps);
        }
      }
      const results = cachedEntries.map((entry) => entry.result);

      if (collector.isActive() && recordRuleQuery) {
        const durationMs = performance.now() - queryStart;
        const matchedLocations: DiagnosticLocation[] = [];
        const matchedTypeTexts: string[] = [];
        if (collector.explainTarget) {
          for (const item of results) {
            const loc = syntaxLocations.get(item.target);
            if (loc && locationMatchesTarget(loc, collector.explainTarget)) {
              matchedLocations.push(loc);
              matchedTypeTexts.push(item.typeText);
            }
          }
        }
        collector.recordQuery({
          op: "typeAtLocation",
          projectId: project.projectId,
          provider: "engine",
          cacheStatus,
          candidateCount: results.length,
          durationMs,
          summary: `typeAtLocation(${targets.length} target(s))`,
          ...(collector.explainTarget !== undefined
            ? { matchedLocations }
            : {}),
          ...(matchedTypeTexts.length > 0
            ? { detail: { matchedTypes: matchedTypeTexts } }
            : {}),
        });
      }

      return results;
    };

    const runListBranches = async (
      functions: readonly SyntaxRef[],
      recordRuleQuery = true,
    ): Promise<readonly FunctionBranchesResult[]> => {
      assertActiveSnapshot();
      if (functions.length === 0) return [];

      for (const fnRef of functions) {
        assertValidProgramRef(
          fnRef,
          sessionToken,
          activeSnapshotId,
          project.projectId,
        );
      }

      const queryStart = collector.isActive() ? performance.now() : 0;
      const missingFunctions: SyntaxRef[] = [];
      for (const fnRef of functions) {
        const key = `${activeSnapshotId}:${project.projectId}:${fnRef.handleId}`;
        if (!branchListCache.has(key)) {
          missingFunctions.push(fnRef);
        }
      }
      const cacheStatus: "computed" | "reused" =
        missingFunctions.length > 0 ? "computed" : "reused";

      if (missingFunctions.length > 0) {
        const batchPromise = client.request<EngineBatchResult>(
          "query.batch",
          {
            snapshotId: activeSnapshotId,
            projectId: project.projectId,
            steps: [
              {
                op: "listBranches",
                targets: missingFunctions.map((fnRef) => fnRef.handleId),
              },
            ],
          },
          getRpcOptions(),
        );

        for (const [index, fnRef] of missingFunctions.entries()) {
          const key = `${activeSnapshotId}:${project.projectId}:${fnRef.handleId}`;
          const itemPromise = batchPromise.then((batch) => {
            const items = (batch.results[0]?.items ??
              []) as readonly EngineFunctionBranchesItem[];
            const raw = items[index];
            if (!raw) {
              throw new TssaeError(
                "ENGINE_ERROR",
                `Engine omitted branch list for function "${fnRef.handleId}".`,
              );
            }
            const mapped = toFunctionBranchesResult(
              raw,
              fnRef,
              sessionToken,
              project.projectId,
              activeSnapshotId,
            );
            for (const branch of mapped.branches) {
              syntaxLocations.set(branch.condition, branch.location);
              recordSyntaxLocation(branch.condition, branch.location);
            }
            return mapped;
          });
          branchListCache.set(key, itemPromise);
        }
      }

      const results = await Promise.all(
        functions.map((fnRef) =>
          branchListCache.get(
            `${activeSnapshotId}:${project.projectId}:${fnRef.handleId}`,
          )!,
        ),
      );
      if (recordRuleQuery) {
        for (const item of results) {
          recordGaps(item.gaps);
        }
      }

      if (collector.isActive() && recordRuleQuery) {
        collector.recordQuery({
          op: "listBranches",
          projectId: project.projectId,
          provider: "engine",
          cacheStatus,
          candidateCount: results.reduce(
            (total, item) => total + item.branches.length,
            0,
          ),
          durationMs: performance.now() - queryStart,
          summary: `listBranches(${functions.length} function(s))`,
        });
      }

      return results;
    };

    const runBranchCoverage = async (
      query: BranchCoverageOptions,
      recordRuleQuery = true,
    ): Promise<readonly BranchCoverageItem[]> => {
      assertActiveSnapshot();
      if (query.function !== undefined && query.scope !== undefined) {
        throw new TssaeError(
          "INVALID_CONFIG",
          'branchCoverage accepts either "function" or "scope", not both.',
        );
      }
      if (query.function !== undefined) {
        assertValidProgramRef(
          query.function,
          sessionToken,
          activeSnapshotId,
          project.projectId,
        );
      }
      if (query.scope !== undefined) {
        assertValidProgramRef(
          query.scope,
          sessionToken,
          activeSnapshotId,
          project.projectId,
        );
      }
      for (const branchRef of query.branches) {
        assertValidProgramRef(
          branchRef,
          sessionToken,
          activeSnapshotId,
          project.projectId,
        );
      }
      for (const targetRef of query.targets) {
        assertValidProgramRef(
          targetRef,
          sessionToken,
          activeSnapshotId,
          project.projectId,
        );
      }
      if (query.targets.length === 0) return [];

      const queryStart = collector.isActive() ? performance.now() : 0;
      const batch = await client.request<EngineBatchResult>(
        "query.batch",
        {
          snapshotId: activeSnapshotId,
          projectId: project.projectId,
          steps: [
            {
              op: "branchCoverage",
              ...(query.function !== undefined
                ? { function: query.function.handleId }
                : {}),
              ...(query.scope !== undefined
                ? { scope: query.scope.handleId }
                : {}),
              branches: query.branches.map((branch) => branch.handleId),
              targets: query.targets.map((target) => target.handleId),
            },
          ],
        },
        getRpcOptions(),
      );

      const rawItems = (batch.results[0]?.items ??
        []) as readonly EngineBranchCoverageItem[];
      const results: BranchCoverageItem[] = [];
      for (const [index, targetRef] of query.targets.entries()) {
        const raw = rawItems[index];
        if (!raw) {
          throw new TssaeError(
            "ENGINE_ERROR",
            `Engine omitted branchCoverage result for target "${targetRef.handleId}".`,
          );
        }
        const mapped = toBranchCoverageItem(raw, targetRef);
        if (recordRuleQuery) {
          if (mapped.status === "inconclusive") {
            recordGaps(mapped.gaps);
          }
          if (mapped.implicitExceptions.status === "inconclusive") {
            recordGaps(mapped.implicitExceptions.gaps);
          }
        }
        results.push(mapped);
      }

      if (collector.isActive() && recordRuleQuery) {
        collector.recordQuery({
          op: "branchCoverage",
          projectId: project.projectId,
          provider: "engine",
          cacheStatus: "computed",
          candidateCount: results.length,
          durationMs: performance.now() - queryStart,
          summary: `branchCoverage(${query.branches.length} branch(es), ${query.targets.length} target(s))`,
        });
      }

      return results;
    };

    const createAdapterContext = (
      adapterId: string,
      onAdditionalInput: (filePath: string, sourceAdapterId?: string) => void,
      facts: ProjectContext["facts"],
      onEngineSubQuery: (trace: EngineNativeSubQueryTrace) => void,
    ): AdapterContext => {
      const scopedAdapterInputs = createScopedAdditionalInputRegistrar(
        dynamicRegistrar,
        { kind: "adapter", id: adapterId },
      );
      return {
        project,
        ...adapterProjectionQueries,
        ...adapterCallsValuesQueries,
        ...adapterPackageImplQueries,
        ...adapterCallEffectsQueries,
        facts,
        findSyntax: (query: FindSyntaxOptions) => runFindSyntax(query, false),
        typeAt: (targets: readonly SyntaxRef[]) => runTypeAt(targets, false),
        listBranches: (functions: readonly SyntaxRef[]) =>
          runListBranches(functions, false),
        branchCoverage: (query: BranchCoverageOptions) =>
          runBranchCoverage(query, false),
        extractEngineFacts: async <
          SubjectKind extends ProgramObjectKind,
          Condition = unknown,
          Value = unknown,
        >(
          extractOptions: EngineFactExtractionOptions<SubjectKind, Condition>,
        ): Promise<readonly FactContribution<SubjectKind, Value>[]> => {
          assertActiveSnapshot();
          if (extractOptions.targets !== undefined) {
            if (extractOptions.targets.length === 0) {
              return [];
            }
            validateProgramRefs(
              extractOptions.targets,
              sessionToken,
              activeSnapshotId,
              project.projectId,
            );
          }

          const explainMode =
            extractOptions.explain ?? collector.isExplainEnabled();
          const batch = await client.request<EngineBatchResult>(
            "query.batch",
            {
              snapshotId: activeSnapshotId,
              projectId: project.projectId,
              steps: [
                {
                  op: "extractFacts",
                  factTypeId: extractOptions.factTypeId,
                  providerId: extractOptions.providerId,
                  ...(extractOptions.reportProviderId !== undefined
                    ? { reportProviderId: extractOptions.reportProviderId }
                    : {}),
                  ...(extractOptions.targets !== undefined
                    ? {
                        targets: extractOptions.targets.map(
                          (target) => target.handleId,
                        ),
                      }
                    : {}),
                  ...(extractOptions.condition !== undefined
                    ? { condition: extractOptions.condition }
                    : {}),
                  ...(explainMode ? { explain: true } : {}),
                },
              ],
            },
            getRpcOptions(),
          );

          const rawItems = (batch.results[0]?.items ?? []) as readonly {
            readonly subject: string;
            readonly subjectLocation?: DiagnosticLocation;
            readonly value: Value;
            readonly evidence?: readonly Evidence[];
            readonly gaps?: readonly Gap[];
            readonly explainTrace?: FactExplainTrace;
          }[];
          const additionalInputs = batch.results[0]?.additionalInputs ?? [];
          const adapterInputs = batch.results[0]?.adapterInputs ?? [];
          const attributedPaths = new Set<string>();
          for (const entry of adapterInputs) {
            const attributedId =
              entry.adapterId === extractOptions.providerId
                ? (extractOptions.reportProviderId ?? entry.adapterId)
                : entry.adapterId;
            attributedPaths.add(entry.path);
            onAdditionalInput(entry.path, attributedId);
            dynamicRegistrar.registerAdditionalInput(entry.path, {
              kind: "adapter",
              id: attributedId,
            });
          }
          for (const filePath of additionalInputs) {
            if (!attributedPaths.has(filePath)) {
              onAdditionalInput(filePath);
              scopedAdapterInputs.registerAdditionalInput(filePath);
            }
          }
          for (const subQuery of batch.results[0]?.subQueries ?? []) {
            onEngineSubQuery(subQuery);
          }

          return rawItems.map((item) => {
            const ref = makeProgramRef(
              "syntax",
              sessionToken,
              project.projectId,
              activeSnapshotId,
              item.subject,
            );
            if (item.subjectLocation !== undefined) {
              syntaxLocations.set(ref, item.subjectLocation);
              recordSyntaxLocation(ref, item.subjectLocation);
            }
            return {
              subject: ref as unknown as ProgramRef<SubjectKind>,
              value: item.value,
              ...(item.evidence !== undefined
                ? { evidence: item.evidence }
                : {}),
              ...(item.gaps !== undefined ? { gaps: item.gaps } : {}),
              ...(item.explainTrace !== undefined
                ? { explainTrace: item.explainTrace }
                : {}),
            };
          });
        },
        registerAdditionalInput: (filePath: string) => {
          assertActiveSnapshot();
          onAdditionalInput(filePath);
          return scopedAdapterInputs.registerAdditionalInput(filePath);
        },
        readAdditionalInput: (filePath: string) => {
          assertActiveSnapshot();
          onAdditionalInput(filePath);
          return scopedAdapterInputs.readAdditionalInput(filePath);
        },
      };
    };

    const replayAdapterAdditionalInput = (
      adapterId: string,
      filePath: string,
    ): void => {
      assertActiveSnapshot();
      dynamicRegistrar.registerAdditionalInput(filePath, {
        kind: "adapter",
        id: adapterId,
      });
    };

    return {
      project,
      ...projectionQueries,
      ...callsValuesQueries,
      ...callEffectsQueries,
      ...instanceQueries,
      ...packageImplQueries,
      findSyntax: (query: FindSyntaxOptions) => runFindSyntax(query, true),
      typeAt: (targets: readonly SyntaxRef[]) => runTypeAt(targets, true),
      listBranches: (functions: readonly SyntaxRef[]) =>
        runListBranches(functions, true),
      branchCoverage: (query: BranchCoverageOptions) =>
        runBranchCoverage(query, true),
      facts: <SubjectKind extends ProgramObjectKind, Condition, Value>(
        factType: FactType<SubjectKind, Condition, Value>,
        factOptions?: FactQueryOptions<SubjectKind, Condition>,
      ) =>
        executeFactQuery({
          factType,
          options: factOptions,
          adapters: activeAdapters,
          factCache,
          sessionToken,
          snapshotId: activeSnapshotId,
          projectId: project.projectId,
          assertActiveSnapshot,
          createAdapterContext,
          replayAdapterAdditionalInput,
          recordGaps,
          collector,
          syntaxLocations,
        }),
      registerAdditionalInput: (filePath: string) => {
        assertActiveSnapshot();
        return scopedInputs.registerAdditionalInput(filePath);
      },
      readAdditionalInput: (filePath: string) => {
        assertActiveSnapshot();
        return scopedInputs.readAdditionalInput(filePath);
      },
    };
  }

  function resolveProjectTarget(
    target?: string | ProjectSummary,
  ): ProjectSummary {
    assertOpen();
    if (currentProjects.length === 0) {
      throw new TssaeError(
        "PROJECT_LOAD_ERROR",
        "No projects are open in this session.",
      );
    }
    if (target === undefined) {
      if (currentProjects.length === 1) {
        return currentProjects[0]!;
      }
      throw new TssaeError(
        "INVALID_QUERY",
        `Multiple projects (${String(currentProjects.length)}) are open; pass a projectId or configFile to session.project(...).`,
      );
    }
    if (typeof target === "string") {
      const byId = currentProjects.find((p) => p.projectId === target);
      if (byId) return byId;
      const byConfig = currentProjects.find((p) => p.configFile === target);
      if (byConfig) return byConfig;
      throw new TssaeError(
        "INVALID_QUERY",
        `Unknown project "${target}". Opened projects: ${currentProjects.map((p) => p.projectId).join(", ")}`,
      );
    }
    const found = currentProjects.find((p) => p.projectId === target.projectId);
    if (!found) {
      throw new TssaeError(
        "INVALID_QUERY",
        `Project "${target.projectId}" is not part of the active snapshot.`,
      );
    }
    return found;
  }

  const closeSession = async (): Promise<void> => {
    if (closed) return;
    closed = true;
    syntaxCache.clear();
    typeCache.clear();
    branchListCache.clear();
    callsValuesCache.clear();
    callEffectsCache.clear();
    instanceCache.clear();
    packageImplCache.clear();
    factCache.clear();
    commentScanCache.clear();
    try {
      await client.request("snapshot.release", {
        snapshotId: currentSnapshotId,
      });
    } catch {
      // Best-effort release before terminating the child process.
    } finally {
      await client.close();
    }
  };

  return {
    get snapshotId() {
      return currentSnapshotId;
    },
    get snapshotDigest() {
      return currentSnapshotDigest;
    },
    get projects() {
      return currentProjects;
    },
    get fileOrigins() {
      return currentFileOrigins;
    },
    get workspaceSourceFiles() {
      return currentWorkspaceSourceFiles;
    },
    get additionalInputs(): readonly AdditionalInputEntry[] {
      return sessionRegistry.listAdditionalInputs();
    },
    get nativeAdapters(): readonly NativeAdapterObservation[] {
      return currentNativeAdapters;
    },
    get memory() {
      return readMemoryStats(lastEngineRssBytes);
    },
    get projectContexts(): readonly ProjectContext[] {
      assertOpen();
      return currentProjects.map((project) =>
        createProjectContext(project, currentSnapshotId),
      );
    },
    project: (
      target?: string | ProjectSummary,
      contextOptions?: ProjectContextOptions,
    ): ProjectContext => {
      const resolved = resolveProjectTarget(target);
      return createProjectContext(resolved, currentSnapshotId, contextOptions);
    },
    scanComments: async (
      scanOptions?: ScanCommentsOptions,
    ): Promise<readonly ScannedComment[]> => {
      assertOpen();
      const files = normalizeScanFilePaths(absCwd, scanOptions?.files);
      if (files !== undefined && files.length === 0) {
        return [];
      }
      const rpcOptions = buildRpcRequestOptions(
        scanOptions?.observe,
        scanOptions?.onRpcMetrics,
      );
      const snapshotId = currentSnapshotId;
      const cacheKey = `${snapshotId}:${files ? JSON.stringify(files) : "*"}`;
      const cached =
        rpcOptions === undefined ? commentScanCache.get(cacheKey) : undefined;
      if (cached) {
        return cached;
      }
      const promise = client
        .request<EngineCommentsScanResult>(
          "comments.scan",
          {
            snapshotId,
            ...(files !== undefined ? { files } : {}),
          },
          rpcOptions,
        )
        .then((res) => res.comments);
      if (rpcOptions === undefined) {
        commentScanCache.set(cacheKey, promise);
      }
      return promise;
    },
    registerAdditionalInput: (
      filePath: string,
      source: AdditionalInputSource,
    ) => {
      assertOpen();
      explicitRegistrations.push({ filePath, source });
      return sessionRegistry.registerAdditionalInput(filePath, source);
    },
    update: async (updateOptions?: SessionUpdateOptions) => {
      assertOpen();
      const changedAdditionalFiles = sessionRegistry.refresh();
      const nextConfig = resolveNextConfig(normalizedConfig, updateOptions);
      const nextAdapterResolutionRoot = resolve(
        updateOptions?.adapterResolutionRoot ?? adapterResolutionRoot,
      );
      const adapterPackagesChanged =
        nextAdapterResolutionRoot !== adapterResolutionRoot ||
        stableSerialize(nextConfig.adapterPackages) !==
          stableSerialize(normalizedConfig.adapterPackages);
      const nextPackageResolution = adapterPackagesChanged
        ? await resolveNativeAdapterPackages(
            nextConfig.adapterPackages,
            nextAdapterResolutionRoot,
          )
        : packageResolution;
      const nextActiveAdapters = combineConfiguredAdapters(
        nextConfig.adapters,
        nextPackageResolution.adapters,
        nextConfig.adapterPackages,
      );
      const nativeAdapterSpecsChanged =
        stableSerialize(nextPackageResolution.nativeAdapters) !==
        stableSerialize(packageResolution.nativeAdapters);
      const nextEntries = resolveProjectEntries(nextConfig);
      const previousSnapshotId = currentSnapshotId;

      const updated = await client.request<EngineUpdateResult>(
        "snapshot.update",
        {
          snapshotId: previousSnapshotId,
          entries: nextEntries,
          changed: updateOptions?.changed ?? [],
          created: updateOptions?.created ?? [],
          deleted: updateOptions?.deleted ?? [],
          ...(nativeAdapterSpecsChanged
            ? { nativeAdapters: nextPackageResolution.nativeAdapters }
            : {}),
        },
        buildRpcRequestOptions(
          updateOptions?.observe,
          updateOptions?.onRpcMetrics,
        ),
      );

      const discovered = await discoverAndLoadSummaries({
        cwd: absCwd,
        projectConfigFiles: updated.projects.map((p) => p.configFile),
        resolvedPackages: updated.resolvedPackages ?? [],
        options: nextConfig.summaries,
      });
      const summariesChanged =
        stableSerialize(discovered.payload) !==
        stableSerialize(currentSummaryPayload);
      if (summariesChanged) {
        await client.request("snapshot.setSummaries", {
          snapshotId: updated.snapshotId,
          summaries: discovered.payload,
        });
      }

      const invalidatedBy = collectInvalidatedBy(
        normalizedConfig,
        nextConfig,
        updateOptions,
        changedAdditionalFiles,
      );
      const effectiveInvalidatedBy =
        summariesChanged && !invalidatedBy.includes("summaries")
          ? [...invalidatedBy, "summaries"]
          : invalidatedBy;
      const updatedFileOrigins = buildFileOrigins(updated.projects);

      normalizedConfig = nextConfig;
      currentSummaryPayload = discovered.payload;
      currentSummaryFiles = discovered.registeredFiles;
      adapterResolutionRoot = nextAdapterResolutionRoot;
      packageResolution = nextPackageResolution;
      activeAdapters = nextActiveAdapters;
      currentSnapshotId = updated.snapshotId;
      currentSnapshotDigest = updated.snapshotDigest;
      currentProjects = updated.projects;
      currentWorkspaceSourceFiles = updated.workspaceSourceFiles;
      currentNativeAdapters = updated.nativeAdapters;
      currentFileOrigins = updatedFileOrigins;
      lastEngineRssBytes = updated.engineRssBytes;
      syntaxCache.clear();
      typeCache.clear();
      branchListCache.clear();
      callsValuesCache.clear();
      callEffectsCache.clear();
      instanceCache.clear();
      packageImplCache.clear();
      factCache.clear();
      commentScanCache.clear();
      sessionRegistry = buildSessionRegistry(
        nextConfig.additionalInputs,
        explicitRegistrations,
        dynamicRegistrations,
        currentSummaryFiles,
      );
      const updatedAdditionalInputs = sessionRegistry.listAdditionalInputs();

      const recomputation: RecomputationScope = {
        recomputedProjects: updated.recomputedProjects,
        reusedProjects: updated.reusedProjects,
        recomputedFiles: updated.recomputedFiles,
        reusedFiles: updated.reusedFiles,
        removedFiles: updated.removedFiles,
        invalidatedBy: effectiveInvalidatedBy,
      };

      return {
        previousSnapshotId,
        snapshotId: updated.snapshotId,
        projects: updated.projects,
        fileOrigins: updatedFileOrigins,
        workspaceSourceFiles: updated.workspaceSourceFiles,
        additionalInputs: updatedAdditionalInputs,
        recomputation,
        memory: readMemoryStats(updated.engineRssBytes),
      };
    },
    close: closeSession,
  };
}
