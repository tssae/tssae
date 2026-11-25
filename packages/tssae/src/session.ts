import { resolve } from "node:path";

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
  validateCleanupAndLifecycleModels,
} from "./call-effects.ts";
import {
  createCallsValuesCache,
  createCallsValuesQueries,
} from "./calls-values.ts";
import { EngineClient } from "./engine-client.ts";
import { registerProvenance, summarizeSyntaxSnippet } from "./evidence.ts";
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
  validateInstanceBehaviors,
} from "./instance-identity.ts";
import { resolveNativeAdapterPackages } from "./native-adapters.ts";
import { ObservationCollector } from "./observation.ts";
import {
  createPackageImplCache,
  createPackageImplQueries,
} from "./package-impl.ts";
import { createProjectionQueries, validateProgramRefs } from "./projection.ts";
import { buildFileOrigins, resolveProjectEntries } from "./projects.ts";
import {
  TssaeError,
  makeProgramRef,
  type DiagnosticLocation,
  type Evidence,
  type FindSyntaxOptions,
  type Gap,
  type MemoryStats,
  type Model,
  type NativeAdapterObservation,
  type OpenProjectOptions,
  type ProgramObjectKind,
  type ProgramRef,
  type ProjectContext,
  type ProjectSession,
  type ProjectSummary,
  type RecomputationScope,
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
  readonly models: readonly Model[];
  readonly adapters: readonly LibraryAdapter[];
  readonly adapterPackages: readonly string[];
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

function registerModelProvenance(model: Model): Model {
  const message =
    typeof model.description === "string" && model.description.trim() !== ""
      ? model.description
      : `Project model "${model.id}"`;
  return registerProvenance(model, [
    {
      kind: "model",
      modelId: model.id,
      message,
    },
  ]);
}

function validateModels(models: readonly Model[]): readonly Model[] {
  const seen = new Set<string>();
  for (const model of models) {
    defineModel(model);
    if (seen.has(model.id)) {
      throw new TssaeError(
        "INVALID_CONFIG",
        `Duplicate model id: "${model.id}".`,
      );
    }
    seen.add(model.id);
  }
  return [...models];
}

/** Validates a project model definition and attaches provenance metadata. */
export function defineModel(model: Model): Model {
  if (typeof model.id !== "string" || model.id.trim() === "") {
    throw new TssaeError(
      "INVALID_CONFIG",
      "Model id must be a non-empty string.",
    );
  }
  if (model.version !== undefined && typeof model.version !== "string") {
    throw new TssaeError(
      "INVALID_CONFIG",
      `Model "${model.id}" version must be a string when provided.`,
    );
  }
  validateInstanceBehaviors(model.id, model.instanceBehaviors);
  validateCleanupAndLifecycleModels(
    model.id,
    model.cleanupOperations,
    model.lifecycleEvents,
  );
  return registerModelProvenance(model);
}

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
  const models = validateModels(options.models ?? []);
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
  const additionalInputs = validateAdditionalInputPaths(
    options.additionalInputs,
  );
  return {
    ...(options.tsconfig !== undefined ? { tsconfig: options.tsconfig } : {}),
    ...(options.projects !== undefined
      ? { projects: [...options.projects] }
      : {}),
    models,
    adapters,
    adapterPackages,
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

function summarizeModel(model: Model) {
  return {
    id: model.id,
    version: model.version ?? null,
    description: model.description ?? null,
    facts: model.facts ?? null,
    ...(model.instanceBehaviors !== undefined
      ? { instanceBehaviors: model.instanceBehaviors }
      : {}),
    ...(model.cleanupOperations !== undefined
      ? { cleanupOperations: model.cleanupOperations }
      : {}),
    ...(model.lifecycleEvents !== undefined
      ? { lifecycleEvents: model.lifecycleEvents }
      : {}),
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
  const models = options?.models
    ? validateModels(options.models)
    : current.models;
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
      models,
      adapters,
      adapterPackages,
      additionalInputs,
    };
  }
  return {
    ...(current.tsconfig !== undefined ? { tsconfig: current.tsconfig } : {}),
    ...(current.projects !== undefined ? { projects: current.projects } : {}),
    models,
    adapters,
    adapterPackages,
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
    stableSerialize(prevConfig.models.map(summarizeModel)) !==
    stableSerialize(nextConfig.models.map(summarizeModel))
  ) {
    reasons.push("models");
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
  return reasons;
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
  try {
    openResult = await client.request<EngineOpenResult>("snapshot.open", {
      cwd: absCwd,
      entries: initialEntries,
      nativeAdapters: packageResolution.nativeAdapters,
    });
  } catch (err) {
    await client.close();
    throw err;
  }

  const sessionToken = Symbol("tssae-session");
  let currentSnapshotId = openResult.snapshotId;
  let currentSnapshotDigest = openResult.snapshotDigest;
  let currentProjects = openResult.projects;
  let currentNativeAdapters = openResult.nativeAdapters;
  let currentFileOrigins = buildFileOrigins(openResult.projects);
  let lastEngineRssBytes = openResult.engineRssBytes;
  let closed = false;

  const syntaxCache = new Map<string, Promise<readonly SyntaxNodeResult[]>>();
  const typeCache = new Map<string, Promise<TypeAtResult>>();
  const branchListCache = new Map<string, Promise<FunctionBranchesResult>>();
  const callsValuesCache = createCallsValuesCache();
  const callEffectsCache = createCallEffectsCache();
  const instanceCache = createInstanceIdentityCache();
  const packageImplCache = createPackageImplCache();
  const factCache = new FactCache();
  const syntaxLocations = new WeakMap<SyntaxRef, DiagnosticLocation>();
  const collector = new ObservationCollector();

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
    return registry;
  }

  let sessionRegistry = buildSessionRegistry(
    normalizedConfig.additionalInputs,
    explicitRegistrations,
    dynamicRegistrations,
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
    models: readonly Model[],
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

    const noopRecordGaps = (_gaps: readonly Gap[]): void => {};
    const scopedInputs = createScopedAdditionalInputRegistrar(
      dynamicRegistrar,
      { kind: "config" },
    );

    const projectionQueries = createProjectionQueries({
      client,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      assertOpen: assertActiveSnapshot,
      recordGaps: noopRecordGaps,
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
      recordGaps: noopRecordGaps,
    });

    const instanceQueries = createInstanceIdentityQueries({
      client,
      cache: instanceCache,
      models,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      cwd: absCwd,
      sourceFiles: project.sourceFiles,
      assertOpen: assertActiveSnapshot,
      recordGaps: noopRecordGaps,
    });

    const callEffectsQueries = createCallEffectsQueries({
      client,
      cache: callEffectsCache,
      models,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      cwd: absCwd,
      sourceFiles: project.sourceFiles,
      assertOpen: assertActiveSnapshot,
      recordGaps: noopRecordGaps,
      resolveSyntaxLocation: (ref) => syntaxLocations.get(ref as SyntaxRef),
      syntaxLocations,
    });

    const packageImplQueries = createPackageImplQueries({
      client,
      cache: packageImplCache,
      sessionToken,
      snapshotId: activeSnapshotId,
      projectId: project.projectId,
      assertOpen: assertActiveSnapshot,
      recordGaps: noopRecordGaps,
    });

    const runFindSyntax = async (
      query: FindSyntaxOptions,
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
      const cached = syntaxCache.get(cacheKey);
      if (cached) {
        return cached;
      }
      const promise = client
        .request<EngineBatchResult>("query.batch", {
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
        })
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
      return promise;
    };

    const runTypeAt = async (
      targets: readonly SyntaxRef[],
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

      if (missingTargets.length > 0) {
        const batchPromise = client.request<EngineBatchResult>("query.batch", {
          snapshotId: activeSnapshotId,
          projectId: project.projectId,
          steps: [
            {
              op: "typeAtLocation",
              targets: missingTargets.map((target) => target.handleId),
            },
          ],
        });

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
            return toTypeAtResult(
              raw,
              target,
              sessionToken,
              project.projectId,
              activeSnapshotId,
            );
          });
          typeCache.set(key, itemPromise);
        }
      }

      return Promise.all(
        targets.map((target) =>
          typeCache.get(
            `${activeSnapshotId}:${project.projectId}:${target.handleId}`,
          )!,
        ),
      );
    };

    const runListBranches = async (
      functions: readonly SyntaxRef[],
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

      const missingFunctions: SyntaxRef[] = [];
      for (const fnRef of functions) {
        const key = `${activeSnapshotId}:${project.projectId}:${fnRef.handleId}`;
        if (!branchListCache.has(key)) {
          missingFunctions.push(fnRef);
        }
      }

      if (missingFunctions.length > 0) {
        const batchPromise = client.request<EngineBatchResult>("query.batch", {
          snapshotId: activeSnapshotId,
          projectId: project.projectId,
          steps: [
            {
              op: "listBranches",
              targets: missingFunctions.map((fnRef) => fnRef.handleId),
            },
          ],
        });

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
            }
            return mapped;
          });
          branchListCache.set(key, itemPromise);
        }
      }

      return Promise.all(
        functions.map((fnRef) =>
          branchListCache.get(
            `${activeSnapshotId}:${project.projectId}:${fnRef.handleId}`,
          )!,
        ),
      );
    };

    const runBranchCoverage = async (
      query: BranchCoverageOptions,
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

      const batch = await client.request<EngineBatchResult>("query.batch", {
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
      });

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
        results.push(toBranchCoverageItem(raw, targetRef));
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
        models,
        getModel: (id: string) => models.find((model) => model.id === id),
        ...projectionQueries,
        ...callsValuesQueries,
        ...packageImplQueries,
        ...callEffectsQueries,
        facts,
        findSyntax: runFindSyntax,
        typeAt: runTypeAt,
        listBranches: runListBranches,
        branchCoverage: runBranchCoverage,
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

          const explainMode = extractOptions.explain ?? false;
          const batch = await client.request<EngineBatchResult>("query.batch", {
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
          });

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
      models,
      getModel: (id: string) => models.find((model) => model.id === id),
      ...projectionQueries,
      ...callsValuesQueries,
      ...callEffectsQueries,
      ...instanceQueries,
      ...packageImplQueries,
      findSyntax: runFindSyntax,
      typeAt: runTypeAt,
      listBranches: runListBranches,
      branchCoverage: runBranchCoverage,
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
          ruleId: "tssae-query",
          assertActiveSnapshot,
          createAdapterContext,
          replayAdapterAdditionalInput,
          recordGaps: noopRecordGaps,
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
        createProjectContext(
          project,
          currentSnapshotId,
          normalizedConfig.models,
        ),
      );
    },
    project: (target?: string | ProjectSummary): ProjectContext => {
      const resolved = resolveProjectTarget(target);
      return createProjectContext(
        resolved,
        currentSnapshotId,
        normalizedConfig.models,
      );
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
      );

      const invalidatedBy = collectInvalidatedBy(
        normalizedConfig,
        nextConfig,
        updateOptions,
        changedAdditionalFiles,
      );
      const updatedFileOrigins = buildFileOrigins(updated.projects);

      normalizedConfig = nextConfig;
      adapterResolutionRoot = nextAdapterResolutionRoot;
      packageResolution = nextPackageResolution;
      activeAdapters = nextActiveAdapters;
      currentSnapshotId = updated.snapshotId;
      currentSnapshotDigest = updated.snapshotDigest;
      currentProjects = updated.projects;
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
      sessionRegistry = buildSessionRegistry(
        nextConfig.additionalInputs,
        explicitRegistrations,
        dynamicRegistrations,
      );
      const updatedAdditionalInputs = sessionRegistry.listAdditionalInputs();

      const recomputation: RecomputationScope = {
        recomputedProjects: updated.recomputedProjects,
        reusedProjects: updated.reusedProjects,
        recomputedFiles: updated.recomputedFiles,
        reusedFiles: updated.reusedFiles,
        removedFiles: updated.removedFiles,
        invalidatedBy,
      };

      return {
        previousSnapshotId,
        snapshotId: updated.snapshotId,
        projects: updated.projects,
        fileOrigins: updatedFileOrigins,
        additionalInputs: updatedAdditionalInputs,
        recomputation,
        memory: readMemoryStats(updated.engineRssBytes),
      };
    },
    close: closeSession,
  };
}
