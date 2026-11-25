import { createHash } from "node:crypto";

import { normalizeEvidence, normalizeLocation } from "./evidence.ts";
import {
  VeruleExecutionError,
  type Diagnostic,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type ProjectSummary,
  type SourceFileOrigin,
  type SourceFileProjectOrigin,
  type VeruleConfig,
} from "./types.ts";

export type CollectedDiagnostic = {
  readonly ruleId: string;
  readonly projectId: string;
  readonly at: DiagnosticLocation;
  readonly message: string;
  readonly findingKey?: string;
  readonly evidence: readonly Evidence[];
};

function compareCodepoint(a: string, b: string): number {
  if (a < b) return -1;
  if (a > b) return 1;
  return 0;
}

function canonicalLocationKey(loc: DiagnosticLocation): string {
  return `${loc.file}:${loc.start}:${loc.end}:${loc.line}:${loc.column}:${loc.endLine}:${loc.endColumn}`;
}

function canonicalEvidenceKey(evidence: readonly Evidence[]): string {
  return JSON.stringify(
    evidence.map((item) => ({
      kind: item.kind,
      message: item.message,
      ...(item.at !== undefined ? { at: canonicalLocationKey(item.at) } : {}),
      ...(item.modelId !== undefined ? { modelId: item.modelId } : {}),
      ...(item.declaration !== undefined
        ? { declaration: item.declaration }
        : {}),
    })),
  );
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
  config: VeruleConfig | undefined,
): readonly string[] {
  if (!config) {
    return ["tsconfig.json"];
  }
  if (config.projects !== undefined) {
    if (config.projects.length === 0) {
      throw new VeruleExecutionError(
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

/**
 * Normalizes, deduplicates, and merges multi-project diagnostics.
 *
 * Diagnostics are merged only when rule id, primary source location, message, `findingKey`,
 * and normalized evidence all match identically; differing conclusions or evidence stay separate.
 */
export function buildDiagnostics(
  collected: readonly CollectedDiagnostic[],
): readonly Diagnostic[] {
  const normalized = collected.map((item) => ({
    ...item,
    at: normalizeLocation(item.at),
    evidence: normalizeEvidence(item.evidence),
  }));

  const sorted = normalized.toSorted((a, b) => {
    const locCmp = compareLocations(a.at, b.at);
    if (locCmp !== 0) return locCmp;
    const ruleCmp = compareCodepoint(a.ruleId, b.ruleId);
    if (ruleCmp !== 0) return ruleCmp;
    const keyCmp = compareCodepoint(a.findingKey ?? "", b.findingKey ?? "");
    if (keyCmp !== 0) return keyCmp;
    const msgCmp = compareCodepoint(a.message, b.message);
    if (msgCmp !== 0) return msgCmp;
    return compareCodepoint(a.projectId, b.projectId);
  });

  type MergedEntry = {
    readonly ruleId: string;
    readonly at: DiagnosticLocation;
    readonly message: string;
    readonly findingKey?: string;
    readonly evidence: readonly Evidence[];
    readonly projects: string[];
  };

  const mergedMap = new Map<string, MergedEntry>();
  const mergedList: MergedEntry[] = [];

  for (const item of sorted) {
    const mergeKey = JSON.stringify([
      item.ruleId,
      canonicalLocationKey(item.at),
      item.message,
      item.findingKey ?? null,
      canonicalEvidenceKey(item.evidence),
    ]);
    const existing = mergedMap.get(mergeKey);
    if (existing) {
      if (!existing.projects.includes(item.projectId)) {
        existing.projects.push(item.projectId);
      }
    } else {
      const entry: MergedEntry = {
        ruleId: item.ruleId,
        at: item.at,
        message: item.message,
        ...(item.findingKey !== undefined
          ? { findingKey: item.findingKey }
          : {}),
        evidence: item.evidence,
        projects: [item.projectId],
      };
      mergedMap.set(mergeKey, entry);
      mergedList.push(entry);
    }
  }

  const occurrenceCounts = new Map<string, number>();
  return mergedList.map((entry) => {
    const anchorKey = entry.findingKey ?? entry.message;
    const sigKey = JSON.stringify([entry.ruleId, entry.at.file, anchorKey]);
    const occurrence = occurrenceCounts.get(sigKey) ?? 0;
    occurrenceCounts.set(sigKey, occurrence + 1);

    const id = createHash("sha256")
      .update(
        JSON.stringify([
          entry.ruleId,
          entry.at.file,
          anchorKey,
          `${entry.at.start}:${entry.at.end}`,
          occurrence,
        ]),
      )
      .digest("hex")
      .slice(0, 16);

    return {
      id,
      ruleId: entry.ruleId,
      at: entry.at,
      message: entry.message,
      evidence: entry.evidence,
      projects: entry.projects.toSorted(compareCodepoint),
    };
  });
}

/** Deduplicates and deterministically orders analysis gaps across projects and rules. */
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
