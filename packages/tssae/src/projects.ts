import { normalizeLocation } from "./evidence.ts";
import {
  TssaeError,
  type DiagnosticLocation,
  type Gap,
  type ProjectSummary,
  type SourceFileOrigin,
  type SourceFileProjectOrigin,
} from "./types.ts";

function compareCodepoint(a: string, b: string): number {
  if (a < b) return -1;
  if (a > b) return 1;
  return 0;
}

function canonicalLocationKey(loc: DiagnosticLocation): string {
  return `${loc.file}:${loc.start}:${loc.end}:${loc.line}:${loc.column}:${loc.endLine}:${loc.endColumn}`;
}

function compareLocations(
  a: DiagnosticLocation,
  b: DiagnosticLocation,
): number {
  const fileCmp = compareCodepoint(a.file, b.file);
  if (fileCmp !== 0) return fileCmp;
  if (a.start !== b.start) return a.start - b.start;
  if (a.end !== b.end) return a.end - b.end;
  if (a.line !== b.line) return a.line - b.line;
  return a.column - b.column;
}

/** Resolves the configured tsconfig/jsconfig entry paths for an analysis session. */
export function resolveProjectEntries(
  config:
    | {
        readonly tsconfig?: string;
        readonly projects?: readonly string[];
      }
    | undefined,
): readonly string[] {
  if (!config) {
    return ["tsconfig.json"];
  }
  if (config.tsconfig !== undefined && config.projects !== undefined) {
    throw new TssaeError(
      "INVALID_CONFIG",
      'Specify either "tsconfig" or "projects", not both.',
    );
  }
  if (config.projects !== undefined) {
    if (config.projects.length === 0) {
      throw new TssaeError(
        "INVALID_CONFIG",
        '"projects" must contain at least one tsconfig path when specified.',
      );
    }
    return config.projects;
  }
  if (config.tsconfig !== undefined) {
    return [config.tsconfig];
  }
  return ["tsconfig.json"];
}

/** Summarizes which opened projects and entry chains include each inspected source file. */
export function buildFileOrigins(
  projects: readonly ProjectSummary[],
): readonly SourceFileOrigin[] {
  const byFile = new Map<string, SourceFileProjectOrigin[]>();

  for (const project of projects) {
    const origin: SourceFileProjectOrigin = {
      projectId: project.projectId,
      configFile: project.configFile,
      entryConfig: project.entryConfig,
      entryChain: [...project.entryChain],
    };

    for (const file of project.sourceFiles) {
      const list = byFile.get(file);
      if (list) {
        if (!list.some((item) => item.projectId === origin.projectId)) {
          list.push(origin);
        }
      } else {
        byFile.set(file, [origin]);
      }
    }
  }

  return [...byFile.entries()]
    .toSorted(([a], [b]) => compareCodepoint(a, b))
    .map(([file, origins]) => ({
      file,
      projects: origins.toSorted((a, b) =>
        compareCodepoint(a.projectId, b.projectId),
      ),
    }));
}

/** Deduplicates and deterministically orders analysis gaps across projects and queries. */
export function buildGaps(collected: readonly Gap[]): readonly Gap[] {
  const unique = new Map<string, Gap>();
  for (const rawGap of collected) {
    const normAt =
      rawGap.at !== undefined ? normalizeLocation(rawGap.at) : undefined;
    const gap: Gap =
      normAt !== rawGap.at && normAt !== undefined
        ? { ...rawGap, at: normAt }
        : rawGap;
    const key = JSON.stringify([
      gap.kind,
      gap.message,
      gap.at !== undefined ? canonicalLocationKey(gap.at) : null,
    ]);
    if (!unique.has(key)) {
      unique.set(key, gap);
    }
  }
  return [...unique.values()].toSorted((a, b) => {
    if (a.at && b.at) {
      const locCmp = compareLocations(a.at, b.at);
      if (locCmp !== 0) return locCmp;
    } else if (a.at || b.at) {
      return a.at ? -1 : 1;
    }
    const kindCmp = compareCodepoint(a.kind, b.kind);
    if (kindCmp !== 0) return kindCmp;
    return compareCodepoint(a.message, b.message);
  });
}
