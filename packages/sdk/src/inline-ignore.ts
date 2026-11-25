import type { CollectedDiagnostic } from "./projects.ts";
import type {
  DiagnosticLocation,
  Evidence,
  InlineIgnoreEntry,
  ProjectSummary,
} from "./types.ts";

export const INLINE_IGNORE_RULE_ID = "verule/inline-ignore" as const;

export type EngineDirectiveItem =
  | {
      readonly kind: "valid";
      readonly ruleId: string;
      readonly reason: string;
      readonly at: DiagnosticLocation;
      readonly targetLine: number | null;
    }
  | {
      readonly kind: "malformed";
      readonly message: string;
      readonly at: DiagnosticLocation;
    };

export type EngineDirectivesScanResult = {
  readonly directives: readonly EngineDirectiveItem[];
};

export type InlineIgnoreSuppression = {
  readonly ruleId: string;
  readonly at: DiagnosticLocation;
  readonly directiveAt: DiagnosticLocation;
  readonly reason: string;
};

export type InlineIgnoreSettlement = {
  readonly diagnostics: readonly CollectedDiagnostic[];
  readonly suppressions: readonly InlineIgnoreSuppression[];
  readonly entries: readonly InlineIgnoreEntry[];
};

function normalizeFilePath(file: string): string {
  return file.replaceAll("\\", "/");
}

function normalizeLocation(loc: DiagnosticLocation): DiagnosticLocation {
  const file = normalizeFilePath(loc.file);
  return file === loc.file ? loc : { ...loc, file };
}

function primaryRangeKey(loc: DiagnosticLocation): string {
  return `${normalizeFilePath(loc.file)}\0${loc.start}\0${loc.end}`;
}

function compareLocations(
  a: DiagnosticLocation,
  b: DiagnosticLocation,
): number {
  if (a.file < b.file) return -1;
  if (a.file > b.file) return 1;
  if (a.start !== b.start) return a.start - b.start;
  if (a.end !== b.end) return a.end - b.end;
  if (a.line !== b.line) return a.line - b.line;
  return a.column - b.column;
}

/**
 * Resolves `verule-ignore-next-code-line` directives against collected rule diagnostics.
 *
 * Rules run normally and keep all gaps and evidence; suppression only hides diagnostics whose
 * `(ruleId, file, start, end)` primary location uniquely matches the target code line.
 */
export function resolveInlineIgnores(params: {
  readonly directives: readonly EngineDirectiveItem[];
  readonly collected: readonly CollectedDiagnostic[];
  readonly projects: readonly ProjectSummary[];
  readonly configuredRuleIds: ReadonlySet<string>;
  readonly executedRuleIds: ReadonlySet<string>;
  readonly isPartialRuleSelection: boolean;
  readonly incompleteRuleIds?: ReadonlySet<string>;
}): InlineIgnoreSettlement {
  if (params.directives.length === 0) {
    return {
      diagnostics: params.collected,
      suppressions: [],
      entries: [],
    };
  }

  const fileProjects = new Map<string, string[]>();
  for (const project of params.projects) {
    for (const file of project.sourceFiles) {
      const normalized = normalizeFilePath(file);
      const list = fileProjects.get(normalized);
      if (list) {
        if (!list.includes(project.projectId)) {
          list.push(project.projectId);
        }
      } else {
        fileProjects.set(normalized, [project.projectId]);
      }
    }
  }

  const byRuleFileLine = new Map<string, CollectedDiagnostic[]>();
  for (const diag of params.collected) {
    const file = normalizeFilePath(diag.at.file);
    const key = `${diag.ruleId}\0${file}\0${diag.at.line}`;
    const bucket = byRuleFileLine.get(key);
    if (bucket) {
      bucket.push(diag);
    } else {
      byRuleFileLine.set(key, [diag]);
    }
  }

  const suppressedKeys = new Set<string>();
  const suppressions: InlineIgnoreSuppression[] = [];
  const directiveDiagnostics: CollectedDiagnostic[] = [];
  const entries: InlineIgnoreEntry[] = [];

  const emitDirectiveDiagnostic = (
    directiveAt: DiagnosticLocation,
    findingKey: string,
    message: string,
    evidence: readonly Evidence[] = [],
  ): void => {
    const projectIds = fileProjects.get(directiveAt.file) ?? [
      params.projects[0]?.projectId ?? "default",
    ];
    for (const projectId of projectIds) {
      directiveDiagnostics.push({
        ruleId: INLINE_IGNORE_RULE_ID,
        projectId,
        at: directiveAt,
        message,
        findingKey,
        evidence,
      });
    }
  };

  for (const rawDirective of params.directives) {
    const directiveAt = normalizeLocation(rawDirective.at);

    if (rawDirective.kind === "malformed") {
      const message = `Malformed inline ignore directive: ${rawDirective.message}`;
      emitDirectiveDiagnostic(directiveAt, "inline-ignore-malformed", message);
      entries.push({
        status: "malformed",
        at: directiveAt,
        message,
      });
      continue;
    }

    if (
      !params.isPartialRuleSelection &&
      !params.configuredRuleIds.has(rawDirective.ruleId)
    ) {
      const message = `Unknown rule ID "${rawDirective.ruleId}" in inline ignore directive.`;
      emitDirectiveDiagnostic(
        directiveAt,
        "inline-ignore-unknown-rule",
        message,
      );
      entries.push({
        status: "unknown_rule",
        at: directiveAt,
        ...(rawDirective.targetLine !== null
          ? { targetLine: rawDirective.targetLine }
          : {}),
        ruleId: rawDirective.ruleId,
        reason: rawDirective.reason,
        message,
      });
      continue;
    }

    const matches =
      rawDirective.targetLine !== null
        ? (byRuleFileLine.get(
            `${rawDirective.ruleId}\0${directiveAt.file}\0${rawDirective.targetLine}`,
          ) ?? [])
        : [];

    const uniqueByRange = new Map<string, DiagnosticLocation>();
    for (const match of matches) {
      const key = primaryRangeKey(match.at);
      if (!uniqueByRange.has(key)) {
        uniqueByRange.set(key, normalizeLocation(match.at));
      }
    }
    const uniqueLocations = [...uniqueByRange.values()].toSorted(
      compareLocations,
    );

    const isRuleComplete =
      params.executedRuleIds.has(rawDirective.ruleId) &&
      !(params.incompleteRuleIds?.has(rawDirective.ruleId) ?? false);

    if (uniqueLocations.length >= 2) {
      const locLabels = uniqueLocations
        .map((loc) => `${loc.file}:${loc.line}:${loc.column}`)
        .join(", ");
      const message = `Ambiguous inline ignore directive for rule "${rawDirective.ruleId}": matched ${uniqueLocations.length} distinct primary locations on line ${rawDirective.targetLine} (${locLabels}); all original diagnostics are retained.`;
      const evidence: Evidence[] = uniqueLocations.map((loc) => ({
        kind: "source",
        message: `Matched primary location ${loc.file}:${loc.line}:${loc.column} (${loc.start}..${loc.end})`,
        at: loc,
      }));
      emitDirectiveDiagnostic(
        directiveAt,
        "inline-ignore-ambiguous",
        message,
        evidence,
      );
      entries.push({
        status: "ambiguous",
        at: directiveAt,
        ...(rawDirective.targetLine !== null
          ? { targetLine: rawDirective.targetLine }
          : {}),
        ruleId: rawDirective.ruleId,
        reason: rawDirective.reason,
        matchedLocations: uniqueLocations,
        message,
      });
      continue;
    }

    if (!isRuleComplete) {
      entries.push({
        status: "undetermined",
        at: directiveAt,
        ...(rawDirective.targetLine !== null
          ? { targetLine: rawDirective.targetLine }
          : {}),
        ruleId: rawDirective.ruleId,
        reason: rawDirective.reason,
      });
      continue;
    }

    if (uniqueLocations.length === 0) {
      const message =
        rawDirective.targetLine === null
          ? `Unused inline ignore directive for rule "${rawDirective.ruleId}": no code line immediately follows the comment (blank line or end of file).`
          : `Unused inline ignore directive for rule "${rawDirective.ruleId}": no matching diagnostic on line ${rawDirective.targetLine}.`;
      emitDirectiveDiagnostic(directiveAt, "inline-ignore-unused", message);
      entries.push({
        status: "unused",
        at: directiveAt,
        ...(rawDirective.targetLine !== null
          ? { targetLine: rawDirective.targetLine }
          : {}),
        ruleId: rawDirective.ruleId,
        reason: rawDirective.reason,
        message,
      });
      continue;
    }

    const matchedLoc = uniqueLocations[0]!;
    suppressedKeys.add(
      `${rawDirective.ruleId}\0${primaryRangeKey(matchedLoc)}`,
    );
    suppressions.push({
      ruleId: rawDirective.ruleId,
      at: matchedLoc,
      directiveAt,
      reason: rawDirective.reason,
    });
    entries.push({
      status: "suppressed",
      at: directiveAt,
      ...(rawDirective.targetLine !== null
        ? { targetLine: rawDirective.targetLine }
        : {}),
      ruleId: rawDirective.ruleId,
      reason: rawDirective.reason,
      matchedLocations: [matchedLoc],
    });
  }

  const surviving = params.collected.filter(
    (diag) =>
      !suppressedKeys.has(`${diag.ruleId}\0${primaryRangeKey(diag.at)}`),
  );

  return {
    diagnostics: [...surviving, ...directiveDiagnostics],
    suppressions,
    entries,
  };
}
