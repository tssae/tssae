import { createHash } from "node:crypto";
import {
  existsSync,
  readdirSync,
  readFileSync,
  statSync,
  watch,
  type FSWatcher,
} from "node:fs";
import { dirname, relative, resolve } from "node:path";

import {
  defineConfig,
  getSessionWorkspaceFiles,
  openSession,
} from "./session.ts";
import {
  VeruleExecutionError,
  type AnalysisSession,
  type CheckReport,
  type FactConflictDetails,
  type MemoryStats,
  type NormalizedVeruleConfig,
  type ObserveLimitInput,
  type ObserveMode,
  type ObserveOptions,
  type RecomputationScope,
  type SessionCheckOptions,
  type SessionUpdateResult,
  type SourceTarget,
  type VeruleConfig,
  type VeruleErrorCode,
  type WatchConfigLoadResult,
  type WatchEvent,
  type WatchProjectOptions,
} from "./types.ts";

export type WorkspaceFailureInfo = {
  readonly code: VeruleErrorCode;
  readonly message: string;
  readonly conflict?: FactConflictDetails;
};

export type WorkspaceControllerOptions = {
  readonly cwd: string;
  readonly config?: VeruleConfig;
  readonly adapterResolutionRoot?: string;
  readonly loadConfig?: () => Promise<WatchConfigLoadResult>;
  readonly watchFiles?: readonly string[];
  readonly observe?: ObserveMode;
  readonly observeLimit?: ObserveLimitInput;
  readonly debounceMs?: number;
  readonly onBatchReady?: () => void;
};

export type WorkspaceCheckParams = {
  readonly ruleIds?: readonly string[];
  readonly at?: string | SourceTarget;
  readonly observe?: ObserveMode | ObserveOptions;
  readonly observeLimit?: ObserveLimitInput;
};

function normalizeRelPath(absCwd: string, absPath: string): string {
  return relative(absCwd, absPath).replace(/\\/gu, "/");
}

function normalizeAbsPath(path: string): string {
  return resolve(path).replace(/\\/gu, "/");
}

function isIgnoredRelativePath(relPath: string): boolean {
  const parts = relPath.split("/");
  return parts.includes(".git") || parts.includes("node_modules");
}

function hashFileContent(absPath: string): string | undefined {
  try {
    const stat = statSync(absPath);
    if (!stat.isFile()) return undefined;
    return createHash("sha256").update(readFileSync(absPath)).digest("hex");
  } catch {
    return undefined;
  }
}

export function toExecutionErrorInfo(err: unknown): WorkspaceFailureInfo {
  if (err instanceof VeruleExecutionError) {
    return {
      code: err.code,
      message: err.message,
      ...(err.conflict !== undefined ? { conflict: err.conflict } : {}),
    };
  }
  return {
    code: "ENGINE_ERROR",
    message: err instanceof Error ? err.message : String(err),
  };
}

export function initialRecomputationScope(
  session: AnalysisSession,
): RecomputationScope {
  const files = new Set<string>();
  for (const project of session.projects) {
    for (const file of project.sourceFiles) {
      files.add(file);
    }
  }
  return {
    recomputedProjects: session.projects.map((p) => p.projectId),
    reusedProjects: [],
    recomputedFiles: [...files].toSorted(),
    reusedFiles: [],
    removedFiles: [],
    invalidatedBy: ["initial"],
  };
}

function recordSessionFiles(
  absCwd: string,
  session: AnalysisSession,
  knownFileHashes: Map<string, string>,
  knownWorkspaceFiles: Set<string>,
  loadedWorkspaceFiles: Set<string>,
  preservedFiles: ReadonlySet<string>,
): void {
  const projectFiles = new Set<string>();
  for (const project of session.projects) {
    const configAbs = normalizeAbsPath(resolve(absCwd, project.configFile));
    projectFiles.add(configAbs);
    const configHash = hashFileContent(configAbs);
    if (configHash !== undefined) {
      knownFileHashes.set(configAbs, configHash);
      knownWorkspaceFiles.add(configAbs);
    }
    for (const relFile of project.sourceFiles) {
      const fileAbs = normalizeAbsPath(resolve(absCwd, relFile));
      projectFiles.add(fileAbs);
      const fileHash = hashFileContent(fileAbs);
      if (fileHash !== undefined) {
        knownFileHashes.set(fileAbs, fileHash);
        knownWorkspaceFiles.add(fileAbs);
      }
    }
  }

  const nextWorkspaceFiles = new Set<string>();
  for (const relFile of getSessionWorkspaceFiles(session)) {
    const fileAbs = normalizeAbsPath(resolve(absCwd, relFile));
    nextWorkspaceFiles.add(fileAbs);
    const fileHash = hashFileContent(fileAbs);
    if (fileHash !== undefined) {
      knownFileHashes.set(fileAbs, fileHash);
      knownWorkspaceFiles.add(fileAbs);
    }
  }

  for (const oldFile of loadedWorkspaceFiles) {
    if (
      !nextWorkspaceFiles.has(oldFile) &&
      !projectFiles.has(oldFile) &&
      !preservedFiles.has(oldFile)
    ) {
      knownFileHashes.delete(oldFile);
    }
  }
  loadedWorkspaceFiles.clear();
  for (const file of nextWorkspaceFiles) loadedWorkspaceFiles.add(file);
}

function collectWorkspaceFiles(
  absCwd: string,
  dir: string,
  out: Set<string>,
): void {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const absChild = normalizeAbsPath(resolve(dir, entry.name));
    const rel = normalizeRelPath(absCwd, absChild);
    if (isIgnoredRelativePath(rel)) {
      continue;
    }
    if (entry.isDirectory()) {
      collectWorkspaceFiles(absCwd, absChild, out);
    } else if (entry.isFile()) {
      out.add(absChild);
    }
  }
}

function createAbortPromise(signal: AbortSignal | undefined): {
  readonly promise: Promise<void>;
  readonly dispose: () => void;
} {
  if (!signal) {
    return {
      promise: new Promise<void>(() => {}),
      dispose: () => {},
    };
  }
  if (signal.aborted) {
    return {
      promise: Promise.resolve(),
      dispose: () => {},
    };
  }
  let onAbort: (() => void) | undefined;
  const promise = new Promise<void>((resolvePromise) => {
    onAbort = () => resolvePromise();
    signal.addEventListener("abort", onAbort, { once: true });
  });
  return {
    promise,
    dispose: () => {
      if (onAbort) {
        signal.removeEventListener("abort", onAbort);
      }
    },
  };
}

export type WorkspaceApplyResult =
  | { readonly applied: false }
  | {
      readonly applied: true;
      readonly trigger: "initial" | "change";
      readonly session: AnalysisSession;
      readonly recomputation: RecomputationScope;
      readonly memory: MemoryStats;
    };

/**
 * Shared incremental workspace controller used by both `watchProject` and `startSharedSession`.
 * Serializes filesystem watcher updates, synchronous disk checks, config reloads, and checks
 * on a single underlying `AnalysisSession`.
 */
export function createWorkspaceController(options: WorkspaceControllerOptions) {
  const absCwd = resolve(options.cwd);
  let adapterResolutionRoot = resolve(options.adapterResolutionRoot ?? absCwd);
  const debounceMs = options.debounceMs ?? 50;

  let session: AnalysisSession | undefined;
  let lastConfig: NormalizedVeruleConfig | undefined;
  const configWatchFiles = new Set<string>(
    (options.watchFiles ?? []).map((file) =>
      normalizeAbsPath(resolve(absCwd, file)),
    ),
  );
  const additionalInputFiles = new Set<string>();
  const loadedWorkspaceFiles = new Set<string>();
  const knownFileHashes = new Map<string, string>();
  const knownWorkspaceFiles = new Set<string>();
  for (const file of configWatchFiles) {
    const hash = hashFileContent(file);
    if (hash !== undefined) {
      knownFileHashes.set(file, hash);
      knownWorkspaceFiles.add(file);
    }
  }
  if (existsSync(absCwd)) {
    collectWorkspaceFiles(absCwd, absCwd, knownWorkspaceFiles);
  }

  const externalWatchers = new Map<string, FSWatcher>();
  let rootWatcher: FSWatcher | undefined;

  const pendingPaths = new Set<string>();
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;
  let closed = false;

  let lastFailure: WorkspaceFailureInfo | undefined;
  let pendingRetryConfig = false;
  const pendingRetryChanged = new Set<string>();
  const pendingRetryCreated = new Set<string>();
  const pendingRetryDeleted = new Set<string>();
  let cachedDefaultReport:
    | {
        readonly snapshotId: number;
        readonly configDigest: string;
        readonly report: CheckReport;
      }
    | undefined;

  let queue: Promise<void> = Promise.resolve();

  function runSerialized<T>(fn: () => Promise<T>): Promise<T> {
    const next = queue.then(fn, fn);
    queue = next.then(
      () => undefined,
      () => undefined,
    );
    return next;
  }

  function clearDebounce(): void {
    if (debounceTimer !== undefined) {
      clearTimeout(debounceTimer);
      debounceTimer = undefined;
    }
  }

  function schedulePath(absPath: string): void {
    if (closed) return;
    const normalized = normalizeAbsPath(absPath);
    const rel = normalizeRelPath(absCwd, normalized);
    const isExplicitWatchFile =
      configWatchFiles.has(normalized) ||
      additionalInputFiles.has(normalized) ||
      loadedWorkspaceFiles.has(normalized);
    if (!isExplicitWatchFile && isIgnoredRelativePath(rel)) {
      return;
    }

    if (existsSync(normalized)) {
      try {
        if (statSync(normalized).isDirectory()) {
          for (const entry of readdirSync(normalized)) {
            const child = normalizeAbsPath(resolve(normalized, entry));
            const childRel = normalizeRelPath(absCwd, child);
            if (!isIgnoredRelativePath(childRel) && existsSync(child)) {
              try {
                if (statSync(child).isFile()) {
                  pendingPaths.add(child);
                }
              } catch {
                // Ignore transient entries during directory scans.
              }
            }
          }
        } else {
          pendingPaths.add(normalized);
        }
      } catch {
        pendingPaths.add(normalized);
      }
    } else {
      pendingPaths.add(normalized);
    }

    clearDebounce();
    debounceTimer = setTimeout(() => {
      debounceTimer = undefined;
      options.onBatchReady?.();
    }, debounceMs);
  }

  function syncWatchers(): void {
    if (closed) return;
    if (!rootWatcher && existsSync(absCwd)) {
      rootWatcher = watch(absCwd, { recursive: true }, (_event, filename) => {
        if (typeof filename === "string" && filename.length > 0) {
          schedulePath(resolve(absCwd, filename));
        } else {
          for (const knownPath of knownFileHashes.keys()) {
            schedulePath(knownPath);
          }
        }
      });
    }

    const absCwdNorm = normalizeAbsPath(absCwd);
    const neededDirs = new Set<string>();
    for (const filePath of [
      ...configWatchFiles,
      ...additionalInputFiles,
      ...loadedWorkspaceFiles,
    ]) {
      const rel = normalizeRelPath(absCwd, filePath);
      const isRootWatchedFile =
        filePath.startsWith(`${absCwdNorm}/`) && !isIgnoredRelativePath(rel);
      if (filePath !== absCwdNorm && !isRootWatchedFile) {
        neededDirs.add(normalizeAbsPath(dirname(filePath)));
      }
    }

    for (const [dir, watcher] of externalWatchers) {
      if (!neededDirs.has(dir)) {
        watcher.close();
        externalWatchers.delete(dir);
      }
    }

    for (const dir of neededDirs) {
      if (!externalWatchers.has(dir) && existsSync(dir)) {
        const watcher = watch(dir, (_event, filename) => {
          if (typeof filename === "string" && filename.length > 0) {
            const candidate = normalizeAbsPath(resolve(dir, filename));
            if (
              configWatchFiles.has(candidate) ||
              additionalInputFiles.has(candidate) ||
              loadedWorkspaceFiles.has(candidate)
            ) {
              schedulePath(candidate);
            }
          } else {
            for (const filePath of [
              ...configWatchFiles,
              ...additionalInputFiles,
              ...loadedWorkspaceFiles,
            ]) {
              if (normalizeAbsPath(dirname(filePath)) === dir) {
                schedulePath(filePath);
              }
            }
          }
        });
        externalWatchers.set(dir, watcher);
      }
    }
  }

  function syncSessionAdditionalInputs(): void {
    if (!session || closed) return;
    const nextAdditionalFiles = new Set<string>();
    for (const entry of session.additionalInputs) {
      const absFile = normalizeAbsPath(resolve(absCwd, entry.file));
      nextAdditionalFiles.add(absFile);
      if (!knownFileHashes.has(absFile)) {
        const hash =
          entry.exists && entry.digest !== undefined
            ? entry.digest
            : hashFileContent(absFile);
        if (hash !== undefined) {
          knownFileHashes.set(absFile, hash);
          knownWorkspaceFiles.add(absFile);
        }
      }
    }
    additionalInputFiles.clear();
    for (const file of nextAdditionalFiles) {
      additionalInputFiles.add(file);
    }
    syncWatchers();
  }

  async function resolveCurrentConfig(): Promise<NormalizedVeruleConfig> {
    if (options.loadConfig) {
      const loaded = await options.loadConfig();
      adapterResolutionRoot = resolve(
        absCwd,
        loaded.adapterResolutionRoot ?? adapterResolutionRoot,
      );
      if (loaded.watchFiles) {
        configWatchFiles.clear();
        for (const file of loaded.watchFiles) {
          const absFile = normalizeAbsPath(resolve(absCwd, file));
          configWatchFiles.add(absFile);
          const hash = hashFileContent(absFile);
          if (hash !== undefined) {
            knownFileHashes.set(absFile, hash);
            knownWorkspaceFiles.add(absFile);
          }
        }
      }
      return defineConfig(loaded.config);
    }
    return options.config ? defineConfig(options.config) : defineConfig({});
  }

  function classifyPaths(paths: Iterable<string>): {
    readonly changed: string[];
    readonly created: string[];
    readonly deleted: string[];
    readonly touchedConfig: boolean;
  } {
    const changed: string[] = [];
    const created: string[] = [];
    const deleted: string[] = [];
    let touchedConfig = false;

    const uniquePaths = new Set(paths);
    for (const absPath of uniquePaths) {
      const rel = normalizeRelPath(absCwd, absPath);
      const prevHash = knownFileHashes.get(absPath);
      const nextHash = hashFileContent(absPath);

      if (nextHash !== undefined) {
        knownWorkspaceFiles.add(absPath);
        if (prevHash === undefined) {
          if (configWatchFiles.has(absPath)) {
            touchedConfig = true;
          }
          knownFileHashes.set(absPath, nextHash);
          created.push(rel);
        } else if (nextHash !== prevHash) {
          if (configWatchFiles.has(absPath)) {
            touchedConfig = true;
          }
          knownFileHashes.set(absPath, nextHash);
          changed.push(rel);
        }
      } else {
        knownWorkspaceFiles.delete(absPath);
        if (prevHash !== undefined) {
          if (configWatchFiles.has(absPath)) {
            touchedConfig = true;
          }
          knownFileHashes.delete(absPath);
          deleted.push(rel);
        }
      }
    }

    return { changed, created, deleted, touchedConfig };
  }

  function drainPendingPaths(): readonly string[] {
    if (pendingPaths.size === 0) return [];
    const paths = [...pendingPaths];
    pendingPaths.clear();
    return paths;
  }

  function recordPendingFailure(
    err: unknown,
    changed: readonly string[],
    created: readonly string[],
    deleted: readonly string[],
    reloadConfigNeeded: boolean,
  ): never {
    for (const file of changed) pendingRetryChanged.add(file);
    for (const file of created) pendingRetryCreated.add(file);
    for (const file of deleted) pendingRetryDeleted.add(file);
    if (reloadConfigNeeded) {
      pendingRetryConfig = true;
    }
    lastFailure = toExecutionErrorInfo(err);
    cachedDefaultReport = undefined;
    throw err;
  }

  function clearFailureState(): void {
    lastFailure = undefined;
    pendingRetryConfig = false;
    pendingRetryChanged.clear();
    pendingRetryCreated.clear();
    pendingRetryDeleted.clear();
  }

  async function applyPathsInternal(
    paths: readonly string[],
  ): Promise<WorkspaceApplyResult> {
    const classified = classifyPaths(paths);
    const mergedChanged = [
      ...new Set([...pendingRetryChanged, ...classified.changed]),
    ];
    const mergedCreated = [
      ...new Set([...pendingRetryCreated, ...classified.created]),
    ];
    const mergedDeleted = [
      ...new Set([...pendingRetryDeleted, ...classified.deleted]),
    ];
    const needConfigReload =
      classified.touchedConfig || pendingRetryConfig || !lastConfig;

    const hasFileChanges =
      mergedChanged.length > 0 ||
      mergedCreated.length > 0 ||
      mergedDeleted.length > 0;

    if (!hasFileChanges && !classified.touchedConfig) {
      return { applied: false };
    }

    let reloadedConfig: NormalizedVeruleConfig | undefined;
    if (needConfigReload) {
      try {
        reloadedConfig = await resolveCurrentConfig();
        lastConfig = reloadedConfig;
        syncWatchers();
      } catch (err) {
        syncWatchers();
        recordPendingFailure(
          err,
          mergedChanged,
          mergedCreated,
          mergedDeleted,
          true,
        );
      }
    }

    if (!session) {
      try {
        session = await openSession({
          cwd: absCwd,
          config: lastConfig!,
          adapterResolutionRoot,
          ...(options.observe !== undefined
            ? { observe: options.observe }
            : {}),
          ...(options.observeLimit !== undefined
            ? { observeLimit: options.observeLimit }
            : {}),
        });
        recordSessionFiles(
          absCwd,
          session,
          knownFileHashes,
          knownWorkspaceFiles,
          loadedWorkspaceFiles,
          new Set([...configWatchFiles, ...additionalInputFiles]),
        );
        syncSessionAdditionalInputs();
        clearFailureState();
        cachedDefaultReport = undefined;
        return {
          applied: true,
          trigger: "initial",
          session,
          recomputation: initialRecomputationScope(session),
          memory: session.memory,
        };
      } catch (err) {
        syncWatchers();
        recordPendingFailure(
          err,
          mergedChanged,
          mergedCreated,
          mergedDeleted,
          reloadedConfig !== undefined,
        );
      }
    }

    let updateResult: SessionUpdateResult;
    try {
      updateResult = await session.update({
        changed: mergedChanged,
        created: mergedCreated,
        deleted: mergedDeleted,
        adapterResolutionRoot,
        ...(reloadedConfig !== undefined ? { config: reloadedConfig } : {}),
      });
    } catch (err) {
      syncWatchers();
      recordPendingFailure(
        err,
        mergedChanged,
        mergedCreated,
        mergedDeleted,
        reloadedConfig !== undefined,
      );
    }

    recordSessionFiles(
      absCwd,
      session,
      knownFileHashes,
      knownWorkspaceFiles,
      loadedWorkspaceFiles,
      new Set([...configWatchFiles, ...additionalInputFiles]),
    );
    syncSessionAdditionalInputs();
    clearFailureState();
    cachedDefaultReport = undefined;
    return {
      applied: true,
      trigger: "change",
      session,
      recomputation: updateResult.recomputation,
      memory: updateResult.memory,
    };
  }

  /**
   * Synchronously inspects all tracked files and scans for newly added workspace files
   * so an immediate query after saving never races with OS watcher debounce.
   */
  function collectDiskSyncCandidates(): readonly string[] {
    clearDebounce();
    const candidates = new Set<string>(drainPendingPaths());
    for (const file of knownFileHashes.keys()) {
      candidates.add(file);
    }
    for (const file of configWatchFiles) {
      candidates.add(file);
    }
    for (const file of additionalInputFiles) {
      candidates.add(file);
    }
    if (existsSync(absCwd)) {
      const currentDiskFiles = new Set<string>();
      collectWorkspaceFiles(absCwd, absCwd, currentDiskFiles);
      for (const diskFile of currentDiskFiles) {
        if (!knownWorkspaceFiles.has(diskFile)) {
          candidates.add(diskFile);
        }
      }
    }
    return [...candidates];
  }

  return {
    absCwd,
    get session() {
      return session;
    },
    syncWatchers,
    syncAdditionalInputs: syncSessionAdditionalInputs,
    openInitial: () =>
      runSerialized(async () => {
        syncWatchers();
        try {
          lastConfig = await resolveCurrentConfig();
          syncWatchers();
          session = await openSession({
            cwd: absCwd,
            config: lastConfig,
            adapterResolutionRoot,
            ...(options.observe !== undefined
              ? { observe: options.observe }
              : {}),
            ...(options.observeLimit !== undefined
              ? { observeLimit: options.observeLimit }
              : {}),
          });
          recordSessionFiles(
            absCwd,
            session,
            knownFileHashes,
            knownWorkspaceFiles,
            loadedWorkspaceFiles,
            new Set([...configWatchFiles, ...additionalInputFiles]),
          );
          syncSessionAdditionalInputs();
          clearFailureState();
          cachedDefaultReport = undefined;
          return {
            session,
            recomputation: initialRecomputationScope(session),
            memory: session.memory,
          };
        } catch (err) {
          syncWatchers();
          lastFailure = toExecutionErrorInfo(err);
          throw err;
        }
      }),
    applyPendingBatch: () =>
      runSerialized(async () => {
        const paths = drainPendingPaths();
        if (paths.length === 0) {
          return { applied: false } satisfies WorkspaceApplyResult;
        }
        return applyPathsInternal(paths);
      }),
    checkLatest: (params?: WorkspaceCheckParams) =>
      runSerialized(async (): Promise<CheckReport> => {
        const candidates = collectDiskSyncCandidates();
        if (candidates.length > 0) {
          await applyPathsInternal(candidates);
        }
        if (lastFailure !== undefined) {
          throw new VeruleExecutionError(
            lastFailure.code,
            lastFailure.message,
            lastFailure.conflict !== undefined
              ? { conflict: lastFailure.conflict }
              : undefined,
          );
        }
        if (!session) {
          throw new VeruleExecutionError(
            "SESSION_CLOSED",
            "Analysis session is not open.",
          );
        }

        const isDefaultQuery =
          !params?.ruleIds?.length &&
          params?.at === undefined &&
          (params?.observe === undefined || params.observe === "off") &&
          params?.observeLimit === undefined;

        if (
          isDefaultQuery &&
          cachedDefaultReport !== undefined &&
          cachedDefaultReport.snapshotId === session.snapshotId
        ) {
          const checkReport = cachedDefaultReport.report;
          return checkReport;
        }

        const checkOptions: SessionCheckOptions | undefined =
          params !== undefined
            ? {
                ...(params.ruleIds !== undefined
                  ? { ruleIds: params.ruleIds }
                  : {}),
                ...(params.at !== undefined ? { at: params.at } : {}),
                ...(params.observe !== undefined
                  ? { observe: params.observe }
                  : {}),
                ...(params.observeLimit !== undefined
                  ? { observeLimit: params.observeLimit }
                  : {}),
              }
            : undefined;
        let report: CheckReport;
        try {
          report = await session.check(checkOptions);
        } finally {
          syncSessionAdditionalInputs();
        }
        if (isDefaultQuery) {
          cachedDefaultReport = {
            snapshotId: session.snapshotId,
            configDigest: report.identity.configDigest,
            report,
          };
        }
        return report;
      }),
    close: async () => {
      closed = true;
      clearDebounce();
      rootWatcher?.close();
      rootWatcher = undefined;
      for (const watcher of externalWatchers.values()) {
        watcher.close();
      }
      externalWatchers.clear();
      await runSerialized(async () => {
        if (session) {
          const toClose = session;
          session = undefined;
          await toClose.close();
        }
      });
    },
  };
}

/**
 * Watches a project directory (and any external config dependency files), updating an
 * {@link AnalysisSession} incrementally and yielding a complete report or failure record
 * after each initial check and subsequent save.
 */
export async function* watchProject(
  options: WatchProjectOptions,
): AsyncGenerator<WatchEvent, void, void> {
  const signal = options.signal;
  let sequence = 0;
  let wakeQueue: (() => void) | undefined;
  let pendingBatchReady = false;

  function notifyQueue(): void {
    if (wakeQueue) {
      const wake = wakeQueue;
      wakeQueue = undefined;
      wake();
    }
  }

  const controller = createWorkspaceController({
    cwd: options.cwd,
    ...(options.config !== undefined ? { config: options.config } : {}),
    ...(options.adapterResolutionRoot !== undefined
      ? { adapterResolutionRoot: options.adapterResolutionRoot }
      : {}),
    ...(options.loadConfig !== undefined
      ? { loadConfig: options.loadConfig }
      : {}),
    ...(options.watchFiles !== undefined
      ? { watchFiles: options.watchFiles }
      : {}),
    ...(options.observe !== undefined ? { observe: options.observe } : {}),
    ...(options.debounceMs !== undefined
      ? { debounceMs: options.debounceMs }
      : {}),
    onBatchReady: () => {
      pendingBatchReady = true;
      notifyQueue();
    },
  });

  async function waitForBatchReady(): Promise<boolean> {
    if (signal?.aborted) return false;
    if (pendingBatchReady) {
      pendingBatchReady = false;
      return true;
    }

    const abortHandle = createAbortPromise(signal);
    try {
      await Promise.race([
        new Promise<void>((resolveWait) => {
          wakeQueue = resolveWait;
        }),
        abortHandle.promise,
      ]);
    } finally {
      wakeQueue = undefined;
      abortHandle.dispose();
    }

    if (signal?.aborted) return false;
    const ready = pendingBatchReady;
    pendingBatchReady = false;
    return ready;
  }

  const abortSignalListener = () => notifyQueue();
  signal?.addEventListener("abort", abortSignalListener, { once: true });

  try {
    if (signal?.aborted) return;

    sequence += 1;
    try {
      const initial = await controller.openInitial();
      let report: CheckReport;
      try {
        report = await initial.session.check();
      } finally {
        controller.syncAdditionalInputs();
      }
      yield {
        kind: "report",
        sequence,
        trigger: "initial",
        snapshotId: initial.session.snapshotId,
        report,
        recomputation: initial.recomputation,
        memory: initial.memory,
      };
    } catch (err) {
      const info = toExecutionErrorInfo(err);
      yield {
        kind: "failed",
        sequence,
        trigger: "initial",
        code: info.code,
        message: info.message,
        ...(info.conflict !== undefined ? { conflict: info.conflict } : {}),
      };
    }

    while (true) {
      if (signal?.aborted) break;
      const ready = await waitForBatchReady();
      if (!ready) break;

      let applied: WorkspaceApplyResult;
      try {
        applied = await controller.applyPendingBatch();
      } catch (err) {
        sequence += 1;
        const info = toExecutionErrorInfo(err);
        yield {
          kind: "failed",
          sequence,
          trigger: "change",
          code: info.code,
          message: info.message,
          ...(info.conflict !== undefined ? { conflict: info.conflict } : {}),
        };
        continue;
      }

      if (!applied.applied) {
        continue;
      }

      sequence += 1;
      try {
        let report: CheckReport;
        try {
          report = await applied.session.check();
        } finally {
          controller.syncAdditionalInputs();
        }
        yield {
          kind: "report",
          sequence,
          trigger: "change",
          snapshotId: applied.session.snapshotId,
          report,
          recomputation: applied.recomputation,
          memory: applied.memory,
        };
      } catch (err) {
        const info = toExecutionErrorInfo(err);
        yield {
          kind: "failed",
          sequence,
          trigger: "change",
          code: info.code,
          message: info.message,
          ...(info.conflict !== undefined ? { conflict: info.conflict } : {}),
        };
      }
    }
  } finally {
    signal?.removeEventListener("abort", abortSignalListener);
    await controller.close();
  }
}
