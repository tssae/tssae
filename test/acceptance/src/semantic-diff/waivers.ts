import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import type {
  SemanticDiffWaiver,
  SemanticOutcomeChannel,
  SemanticOutcomeStatus,
  SemanticQueryName,
} from "./types.ts";

const COMMIT_SHA_PATTERN = /^[0-9a-f]{40}$/iu;

const VALID_QUERIES = new Set<SemanticQueryName>([
  "branchCoverage",
  "calleesOf",
  "callersOf",
  "valuePropagation",
  "resolveInstances",
  "instanceCleanup",
  "callEffects",
  "resolveImplementations",
]);

const VALID_CHANNELS = new Set<SemanticOutcomeChannel>([
  "primary",
  "implicitExceptions",
]);

const VALID_STATUSES = new Set<SemanticOutcomeStatus>([
  "proved",
  "disproved",
  "inconclusive",
  "complete",
  "incomplete",
  "resolved",
  "external",
]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function assertNonEmptyString(
  raw: Record<string, unknown>,
  field: string,
  index: number,
): string {
  const value = raw[field];
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(
      `Invalid semantic diff waiver at index ${String(index)}: "${field}" must be a non-empty string.`,
    );
  }
  return value.trim();
}

function validateWaiverEntry(raw: unknown, index: number): SemanticDiffWaiver {
  if (!isRecord(raw)) {
    throw new Error(
      `Invalid semantic diff waiver at index ${String(index)}: expected JSON object.`,
    );
  }

  const baselineCommit = assertNonEmptyString(raw, "baselineCommit", index);
  if (!COMMIT_SHA_PATTERN.test(baselineCommit)) {
    throw new Error(
      `Invalid semantic diff waiver at index ${String(index)}: "baselineCommit" must be a 40-character hexadecimal SHA, got "${baselineCommit}".`,
    );
  }

  const query = assertNonEmptyString(raw, "query", index) as SemanticQueryName;
  if (!VALID_QUERIES.has(query)) {
    throw new Error(
      `Invalid semantic diff waiver at index ${String(index)}: unknown query "${query}".`,
    );
  }

  let channel: SemanticOutcomeChannel | undefined;
  if (raw.channel !== undefined) {
    if (
      typeof raw.channel !== "string" ||
      !VALID_CHANNELS.has(raw.channel as SemanticOutcomeChannel)
    ) {
      throw new Error(
        `Invalid semantic diff waiver at index ${String(index)}: invalid channel ${JSON.stringify(raw.channel)}.`,
      );
    }
    channel = raw.channel as SemanticOutcomeChannel;
  }

  const file = assertNonEmptyString(raw, "file", index);
  const target = assertNonEmptyString(raw, "target", index);

  const fromStatus = assertNonEmptyString(
    raw,
    "fromStatus",
    index,
  ) as SemanticOutcomeStatus;
  if (!VALID_STATUSES.has(fromStatus)) {
    throw new Error(
      `Invalid semantic diff waiver at index ${String(index)}: invalid fromStatus "${fromStatus}".`,
    );
  }

  const toStatus = assertNonEmptyString(
    raw,
    "toStatus",
    index,
  ) as SemanticOutcomeStatus;
  if (!VALID_STATUSES.has(toStatus)) {
    throw new Error(
      `Invalid semantic diff waiver at index ${String(index)}: invalid toStatus "${toStatus}".`,
    );
  }

  const reason = assertNonEmptyString(raw, "reason", index);
  const regressionTest = assertNonEmptyString(raw, "regressionTest", index);

  return {
    baselineCommit: baselineCommit.toLowerCase(),
    query,
    ...(channel === undefined ? {} : { channel }),
    file,
    target,
    fromStatus,
    toStatus,
    reason,
    regressionTest,
  };
}

/**
 * Resolves the package's version-controlled `semantic-diff-waivers.json` path
 * reliably whether executed from `src/` (`--conditions=source`) or compiled `dist/`.
 */
export function resolveDefaultWaiversPath(): string {
  const currentDir = dirname(fileURLToPath(import.meta.url));
  const candidate = resolve(currentDir, "../../semantic-diff-waivers.json");
  if (existsSync(candidate)) {
    return candidate;
  }
  throw new Error(`Cannot locate semantic-diff-waivers.json at ${candidate}.`);
}

/** Loads and validates declarative semantic diff waivers from a JSON manifest file. */
export function loadSemanticDiffWaivers(
  filePath: string = resolveDefaultWaiversPath(),
): readonly SemanticDiffWaiver[] {
  const resolved = resolve(filePath);
  const parsed = JSON.parse(readFileSync(resolved, "utf8")) as unknown;
  const rawList = Array.isArray(parsed)
    ? parsed
    : isRecord(parsed) && Array.isArray(parsed.waivers)
      ? parsed.waivers
      : undefined;

  if (rawList === undefined) {
    throw new Error(
      `Invalid semantic diff waivers manifest at "${resolved}": expected an array or { "waivers": [...] }.`,
    );
  }

  return rawList.map((entry, index) => validateWaiverEntry(entry, index));
}
