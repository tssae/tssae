import { createHash } from "node:crypto";
import { existsSync, readFileSync, realpathSync, statSync } from "node:fs";
import { basename, dirname, relative, resolve } from "node:path";

import { TssaeError } from "./types.ts";

/** Origin of a declared additional input file tracked by an analysis session. */
export type AdditionalInputSource =
  | { readonly kind: "config" }
  | { readonly kind: "rule"; readonly id: string }
  | { readonly kind: "summary"; readonly id: string };

/** Deduplicated additional input entry with its normalized relative path, digest, and all declaration sources. */
export type AdditionalInputEntry = {
  readonly file: string;
  readonly exists: boolean;
  readonly digest?: string;
  readonly sources: readonly AdditionalInputSource[];
};

/** Shared registration hook used by config, rules, and summaries to declare local non-source inputs. */
export type AdditionalInputRegistrar = {
  readonly cwd: string;
  registerAdditionalInput: (
    filePath: string,
    source: AdditionalInputSource,
  ) => AdditionalInputEntry;
  readAdditionalInput: (
    filePath: string,
    source: AdditionalInputSource,
  ) => string;
  listAdditionalInputs: () => readonly AdditionalInputEntry[];
  getTrackedAbsolutePaths: () => readonly string[];
};

/** Source-bound registrar passed to individual rules or summary loaders. */
export type ScopedAdditionalInputRegistrar = {
  registerAdditionalInput: (filePath: string) => AdditionalInputEntry;
  readAdditionalInput: (filePath: string) => string;
};

/** Stateful registry backing an analysis session's additional input tracking. */
export type AdditionalInputRegistry = AdditionalInputRegistrar & {
  refresh: () => readonly string[];
  resolveAbsolutePath: (filePath: string) => string;
};

type MutableAdditionalInputRecord = {
  readonly canonicalAbs: string;
  readonly file: string;
  exists: boolean;
  digest: string | undefined;
  readonly sourcesByKey: Map<string, AdditionalInputSource>;
};

function compareCodepoint(a: string, b: string): number {
  if (a < b) return -1;
  if (a > b) return 1;
  return 0;
}

function normalizeSlashes(value: string): string {
  return value.replaceAll("\\", "/");
}

function canonicalizeAbsPath(absPath: string): string {
  const resolved = resolve(absPath);
  if (existsSync(resolved)) {
    try {
      return normalizeSlashes(realpathSync.native(resolved));
    } catch {
      return normalizeSlashes(resolved);
    }
  }

  const missingSegments: string[] = [];
  let current = resolved;
  while (true) {
    const parent = dirname(current);
    if (parent === current) {
      return normalizeSlashes(resolved);
    }
    missingSegments.unshift(basename(current));
    current = parent;
    if (existsSync(current)) {
      try {
        const realAncestor = realpathSync.native(current);
        return normalizeSlashes(resolve(realAncestor, ...missingSegments));
      } catch {
        return normalizeSlashes(resolved);
      }
    }
  }
}

function sourceKey(source: AdditionalInputSource): string {
  return source.kind === "config" ? "config" : `${source.kind}:${source.id}`;
}

function sourceKindRank(kind: AdditionalInputSource["kind"]): number {
  if (kind === "config") return 0;
  if (kind === "summary") return 1;
  return 2;
}

function compareSources(
  a: AdditionalInputSource,
  b: AdditionalInputSource,
): number {
  const rankDiff = sourceKindRank(a.kind) - sourceKindRank(b.kind);
  if (rankDiff !== 0) return rankDiff;
  if (a.kind === "config" || b.kind === "config") return 0;
  return compareCodepoint(a.id, b.id);
}

function validateInputPath(filePath: string): string {
  if (typeof filePath !== "string" || filePath.trim() === "") {
    throw new TssaeError(
      "INVALID_CONFIG",
      "Additional input path must be a non-empty string.",
    );
  }
  return filePath;
}

function validateSource(source: AdditionalInputSource): void {
  if (
    source.kind !== "config" &&
    (typeof source.id !== "string" || source.id.trim() === "")
  ) {
    throw new TssaeError(
      "INVALID_CONFIG",
      `Additional input source "${source.kind}" must have a non-empty id.`,
    );
  }
}

function inspectFileDigest(absPath: string): {
  readonly exists: boolean;
  readonly digest: string | undefined;
} {
  try {
    const stat = statSync(absPath);
    if (!stat.isFile()) {
      return { exists: false, digest: undefined };
    }
    const digest = createHash("sha256")
      .update(readFileSync(absPath))
      .digest("hex");
    return { exists: true, digest };
  } catch {
    return { exists: false, digest: undefined };
  }
}

function toEntry(record: MutableAdditionalInputRecord): AdditionalInputEntry {
  const sources = [...record.sourcesByKey.values()].toSorted(compareSources);
  return {
    file: record.file,
    exists: record.exists,
    ...(record.digest !== undefined ? { digest: record.digest } : {}),
    sources,
  };
}

/** Validates config-level `additionalInputs` paths without performing filesystem I/O. */
export function validateAdditionalInputPaths(
  paths: readonly string[] | undefined,
): readonly string[] {
  if (paths === undefined) {
    return [];
  }
  const validated: string[] = [];
  for (const rawPath of paths) {
    validated.push(validateInputPath(rawPath));
  }
  return validated;
}

/**
 * Creates a registry that normalizes local additional input paths, deduplicates physical files
 * across config, rule, and summary declarations, tracks content digests, and preserves all
 * declaration sources without expanding compiler source file sets.
 */
export function createAdditionalInputRegistry(options: {
  readonly cwd: string;
}): AdditionalInputRegistry {
  const absCwd = resolve(options.cwd);
  const canonicalCwd = canonicalizeAbsPath(absCwd);
  const recordsByCanonicalPath = new Map<
    string,
    MutableAdditionalInputRecord
  >();

  function resolveCanonicalPath(filePath: string): string {
    const validated = validateInputPath(filePath);
    return canonicalizeAbsPath(resolve(absCwd, validated));
  }

  function registerAdditionalInput(
    filePath: string,
    source: AdditionalInputSource,
  ): AdditionalInputEntry {
    validateSource(source);
    const canonicalAbs = resolveCanonicalPath(filePath);
    const existing = recordsByCanonicalPath.get(canonicalAbs);
    if (existing) {
      existing.sourcesByKey.set(sourceKey(source), source);
      const inspected = inspectFileDigest(canonicalAbs);
      existing.exists = inspected.exists;
      existing.digest = inspected.digest;
      return toEntry(existing);
    }

    const rel = normalizeSlashes(relative(canonicalCwd, canonicalAbs));
    const inspected = inspectFileDigest(canonicalAbs);
    const record: MutableAdditionalInputRecord = {
      canonicalAbs,
      file: rel,
      exists: inspected.exists,
      digest: inspected.digest,
      sourcesByKey: new Map([[sourceKey(source), source]]),
    };
    recordsByCanonicalPath.set(canonicalAbs, record);
    return toEntry(record);
  }

  function readAdditionalInput(
    filePath: string,
    source: AdditionalInputSource,
  ): string {
    validateSource(source);
    const canonicalAbs = resolveCanonicalPath(filePath);
    registerAdditionalInput(filePath, source);
    try {
      return readFileSync(canonicalAbs, "utf8");
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      throw new TssaeError(
        "INVALID_CONFIG",
        `Failed to read additional input "${filePath}": ${detail}`,
        { cause: err },
      );
    }
  }

  function listAdditionalInputs(): readonly AdditionalInputEntry[] {
    return [...recordsByCanonicalPath.values()]
      .map(toEntry)
      .toSorted((a, b) => compareCodepoint(a.file, b.file));
  }

  function getTrackedAbsolutePaths(): readonly string[] {
    return [...recordsByCanonicalPath.keys()].toSorted(compareCodepoint);
  }

  function refresh(): readonly string[] {
    const changedFiles: string[] = [];
    for (const record of recordsByCanonicalPath.values()) {
      const prevExists = record.exists;
      const prevDigest = record.digest;
      const next = inspectFileDigest(record.canonicalAbs);
      record.exists = next.exists;
      record.digest = next.digest;
      if (prevExists !== next.exists || prevDigest !== next.digest) {
        changedFiles.push(record.file);
      }
    }
    return changedFiles.toSorted(compareCodepoint);
  }

  return {
    cwd: absCwd,
    registerAdditionalInput,
    readAdditionalInput,
    listAdditionalInputs,
    getTrackedAbsolutePaths,
    refresh,
    resolveAbsolutePath: resolveCanonicalPath,
  };
}

/** Binds an `AdditionalInputRegistrar` to a fixed rule or summary source tag. */
export function createScopedAdditionalInputRegistrar(
  registrar: AdditionalInputRegistrar,
  source: AdditionalInputSource,
): ScopedAdditionalInputRegistrar {
  validateSource(source);
  return {
    registerAdditionalInput: (filePath: string) =>
      registrar.registerAdditionalInput(filePath, source),
    readAdditionalInput: (filePath: string) =>
      registrar.readAdditionalInput(filePath, source),
  };
}
