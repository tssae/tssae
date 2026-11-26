import { createHash } from "node:crypto";
import { existsSync, readFileSync, realpathSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import {
  dirname,
  extname,
  isAbsolute,
  join,
  relative,
  resolve,
} from "node:path";
import { pathToFileURL } from "node:url";

import type { AdditionalInputRegistrar } from "./additional-inputs.ts";
import { registerProvenance } from "./evidence.ts";
import { TssaeError } from "./types.ts";

/** Completeness declaration for a single behavioral aspect (defaults to `"partial"`). */
export type SummaryCompleteness = "complete" | "partial";

/** Discovery tier for behavior summaries, ordered from highest to lowest priority. */
export type SummaryDiscoverySource =
  | "local"
  | "package"
  | "community"
  | "builtin";

/** The four behavioral aspects represented in a behavior summary entry. */
export type SummaryAspectName = "calls" | "values" | "instances" | "effects";

/** Declarative identity matcher for an external package symbol, local module symbol, or global. */
export type SummaryTargetMatcher = {
  /** External package name (e.g. `"@acme/lifecycle"`); omit for project-local or global declarations. */
  readonly package?: string;
  /** Optional npm semver range (e.g. `"^1.2.0"`). */
  readonly version?: string;
  /** Optional package export subpath (e.g. `"."`, `"./subpath"`). */
  readonly exportPath?: string;
  /** Optional project-relative module path when matching project-local declarations without `package`. */
  readonly module?: string;
  /** Symbol/member path such as `["createResource"]`, `["DisposableStore", "add"]`, or `["Array", "prototype", "map"]`. */
  readonly memberPath: readonly [string, ...string[]];
};

/** Pure-data value flow endpoint within a summarized call signature. */
export type SummaryValueRef =
  | "receiver"
  | "receiverElement"
  | "return"
  | "returnElement"
  | { readonly kind: "arg"; readonly index: number }
  | { readonly kind: "argElement"; readonly index: number }
  | {
      readonly kind: "callbackParam";
      readonly callbackArgIndex: number;
      readonly paramIndex: number;
    }
  | { readonly kind: "callbackReturn"; readonly callbackArgIndex: number }
  | {
      readonly kind: "callbackParamArg";
      readonly callbackArgIndex: number;
      readonly callbackParamIndex: number;
      readonly argIndex: number;
    };

/** Call-graph and callback-invocation aspect of a behavior summary. */
export type CallAspectSummary = {
  readonly completeness?: SummaryCompleteness;
  readonly invokesCallbacks?: readonly {
    readonly callbackArgIndex: number;
    readonly timing: "synchronous" | "deferred" | "lifecycle-end";
    readonly cardinality?: "at-most-once" | "zero-or-more" | "once";
    readonly thisArg?: { readonly fromArgIndex: number } | "undefined";
    readonly eventName?: string;
  }[];
  readonly forwardsCall?: {
    readonly target: "receiver" | { readonly argIndex: number };
    readonly mode: "call" | "apply" | "bind";
    readonly thisArg?: { readonly fromArgIndex: number } | "undefined";
    readonly argsFromIndex?: number;
    readonly argsArrayIndex?: number;
  };
};

/** Value-propagation aspect of a behavior summary. */
export type ValueAspectSummary = {
  readonly completeness?: SummaryCompleteness;
  readonly flows?: readonly {
    readonly from: SummaryValueRef;
    readonly to: SummaryValueRef;
    readonly preserveObjectPath?: boolean;
  }[];
};

/** Instance-identity aspect of a behavior summary. */
export type InstanceAspectSummary = {
  readonly completeness?: SummaryCompleteness;
  readonly outcome:
    | { readonly kind: "allocates-fresh-instance" }
    | {
        readonly kind: "forwards-instance";
        readonly from: "receiver" | { readonly argIndex: number };
      }
    | { readonly kind: "none" };
};

/** Side-effect and resource-cleanup aspect of a behavior summary. */
export type EffectAspectSummary = {
  readonly completeness?: SummaryCompleteness;
  readonly resourceOps?: readonly (
    | {
        readonly kind: "disposes-target";
        readonly target: "receiver" | { readonly argIndex: number };
      }
    | {
        readonly kind: "replaces-previous-value";
        readonly target: "receiver" | { readonly argIndex: number };
      }
    | {
        readonly kind: "registers-into-container";
        readonly item: "receiver" | { readonly argIndex: number };
        readonly container: "receiver" | { readonly argIndex: number };
      }
    | {
        readonly kind: "detaches-from-container";
        readonly item: "receiver" | { readonly argIndex: number };
        readonly container: "receiver" | { readonly argIndex: number };
      }
  )[];
  readonly mutatesReceiver?: boolean;
};

/** Single behavioral summary entry targeting one declaration matcher. */
export type BehaviorSummaryEntry = {
  readonly id?: string;
  readonly match: SummaryTargetMatcher;
  readonly description?: string;
  readonly calls?: CallAspectSummary;
  readonly values?: ValueAspectSummary;
  readonly instances?: InstanceAspectSummary;
  readonly effects?: EffectAspectSummary;
};

/** Pure-data behavior summary pack exported by `defineSummaries(...)` or summary files. */
export type BehaviorSummaryPack = {
  readonly id: string;
  readonly version?: string;
  readonly description?: string;
  readonly entries: readonly BehaviorSummaryEntry[];
};

/** Configuration options controlling behavior summary loading and automatic discovery. */
export type SummaryOptions = {
  readonly packs?: readonly BehaviorSummaryPack[];
  readonly files?: readonly string[];
  /** Enabled discovery tiers; `false` or `[]` disables automatic discovery. */
  readonly discovery?: boolean | readonly SummaryDiscoverySource[];
};

/** One conflicting party in a same-tier `SUMMARY_CONFLICT`. */
export type SummaryConflictParty = {
  readonly source: SummaryDiscoverySource;
  readonly packId: string;
  readonly entryId?: string;
  readonly originFile?: string;
  readonly matcher: SummaryTargetMatcher;
  readonly aspect: SummaryAspectName;
  readonly completeness: SummaryCompleteness;
  readonly summary: unknown;
};

/** Structured error details attached to `TssaeError("SUMMARY_CONFLICT", ...)`. */
export type SummaryConflictDetails = {
  readonly source: SummaryDiscoverySource;
  readonly aspect: SummaryAspectName;
  readonly targetDescription: string;
  readonly reason: string;
  readonly parties: readonly [SummaryConflictParty, SummaryConflictParty];
};

/** Resolved external package metadata returned by the engine for on-demand summary discovery. */
export type ResolvedPackageSummary = {
  readonly name: string;
  readonly version?: string;
  readonly packageRoot?: string;
  readonly projectDirs: readonly string[];
};

export type TaggedSummaryPack = {
  readonly source: SummaryDiscoverySource;
  readonly originFile?: string;
  readonly pack: BehaviorSummaryPack;
};

export type SummaryConfigPayload = {
  readonly packs: readonly TaggedSummaryPack[];
  readonly enableBuiltin: boolean;
};

export type NormalizedSummaryOptions = {
  readonly packs: readonly BehaviorSummaryPack[];
  readonly files: readonly string[];
  readonly discoverySources: readonly SummaryDiscoverySource[];
};

const ALL_DISCOVERY_SOURCES: readonly SummaryDiscoverySource[] = [
  "local",
  "package",
  "community",
  "builtin",
];

const LOCAL_CONVENTION_FILES: readonly string[] = [
  "tssae.summaries.ts",
  "tssae.summaries.mts",
  "tssae.summaries.js",
  "tssae.summaries.mjs",
  "tssae.summaries.json",
];

const SUPPORTED_SUMMARY_EXTENSIONS = new Set([
  ".ts",
  ".mts",
  ".js",
  ".mjs",
  ".json",
]);

function invalidConfig(message: string, cause?: unknown): TssaeError {
  return new TssaeError(
    "INVALID_CONFIG",
    message,
    cause !== undefined ? { cause } : undefined,
  );
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
}

function assertPureData(value: unknown, path: string): void {
  if (
    value === null ||
    value === undefined ||
    typeof value === "string" ||
    typeof value === "boolean"
  ) {
    return;
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      throw invalidConfig(
        `Summary field "${path}" must be a finite number, got ${String(value)}.`,
      );
    }
    return;
  }
  if (typeof value === "function") {
    throw invalidConfig(
      `Summary field "${path}" must be pure data; functions are not allowed.`,
    );
  }
  if (typeof value !== "object") {
    throw invalidConfig(
      `Summary field "${path}" must be pure JSON-serializable data, got ${typeof value}.`,
    );
  }
  if (Array.isArray(value)) {
    for (let i = 0; i < value.length; i += 1) {
      if (value[i] === undefined) {
        throw invalidConfig(
          `Summary array element "${path}[${i}]" cannot be undefined.`,
        );
      }
      assertPureData(value[i], `${path}[${i}]`);
    }
    return;
  }
  if (!isPlainRecord(value)) {
    throw invalidConfig(
      `Summary field "${path}" must be a plain object with no custom prototype.`,
    );
  }
  for (const [key, child] of Object.entries(value)) {
    assertPureData(child, `${path}.${key}`);
  }
}

function assertAllowedKeys(
  obj: Record<string, unknown>,
  allowed: readonly string[],
  path: string,
): void {
  const allowedSet = new Set(allowed);
  for (const key of Object.keys(obj)) {
    if (!allowedSet.has(key)) {
      throw invalidConfig(
        `Unknown property "${key}" in behavior summary at "${path}".`,
      );
    }
  }
}

function assertNonEmptyString(value: unknown, path: string): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw invalidConfig(`Summary field "${path}" must be a non-empty string.`);
  }
  return value;
}

function assertOptionalNonEmptyString(
  value: unknown,
  path: string,
): string | undefined {
  if (value === undefined) return undefined;
  return assertNonEmptyString(value, path);
}

function assertNonNegativeInt(value: unknown, path: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw invalidConfig(
      `Summary field "${path}" must be a non-negative integer.`,
    );
  }
  return value;
}

function assertOptionalCompleteness(
  value: unknown,
  path: string,
): SummaryCompleteness | undefined {
  if (value === undefined) return undefined;
  if (value !== "complete" && value !== "partial") {
    throw invalidConfig(
      `Summary field "${path}" must be "complete" or "partial".`,
    );
  }
  return value;
}

type PrereleasePart =
  | { readonly kind: "num"; readonly value: number }
  | { readonly kind: "alpha"; readonly value: string };

type SemverVersion = {
  readonly major: number;
  readonly minor: number;
  readonly patch: number;
  readonly prerelease: readonly PrereleasePart[];
};

type PartialVersion = {
  readonly major: number | undefined;
  readonly minor: number | undefined;
  readonly patch: number | undefined;
  readonly prerelease: readonly PrereleasePart[];
};

type CompOp = "=" | "<" | "<=" | ">" | ">=";

type Comparator = {
  readonly op: CompOp;
  readonly ver: SemverVersion;
};

function parseStrictUInt(raw: string): number | undefined {
  if (!/^(?:0|[1-9]\d*)$/u.test(raw)) return undefined;
  const num = Number(raw);
  return Number.isSafeInteger(num) ? num : undefined;
}

function parsePrereleasePart(raw: string): PrereleasePart | undefined {
  if (!/^[0-9A-Za-z-]+$/u.test(raw)) return undefined;
  if (/^\d+$/u.test(raw)) {
    const num = parseStrictUInt(raw);
    return num !== undefined ? { kind: "num", value: num } : undefined;
  }
  return { kind: "alpha", value: raw };
}

function isWildcard(part: string): boolean {
  return part === "*" || part === "x" || part === "X";
}

function parseSemverVersion(raw: string): SemverVersion | undefined {
  const trimmed = raw.trim().replace(/^v/u, "");
  const withoutBuild = trimmed.split("+")[0];
  if (withoutBuild === undefined || withoutBuild === "") return undefined;
  const dashIdx = withoutBuild.indexOf("-");
  const core = dashIdx >= 0 ? withoutBuild.slice(0, dashIdx) : withoutBuild;
  const preRaw = dashIdx >= 0 ? withoutBuild.slice(dashIdx + 1) : undefined;
  if (preRaw === "") return undefined;
  const parts = core.split(".");
  if (parts.length !== 3) return undefined;
  const major = parseStrictUInt(parts[0]!);
  const minor = parseStrictUInt(parts[1]!);
  const patch = parseStrictUInt(parts[2]!);
  if (major === undefined || minor === undefined || patch === undefined) {
    return undefined;
  }
  const prerelease: PrereleasePart[] = [];
  if (preRaw !== undefined) {
    for (const seg of preRaw.split(".")) {
      const parsed = parsePrereleasePart(seg);
      if (!parsed) return undefined;
      prerelease.push(parsed);
    }
  }
  return { major, minor, patch, prerelease };
}

function parsePartialVersion(raw: string): PartialVersion | undefined {
  const trimmed = raw.trim().replace(/^v/u, "");
  if (trimmed === "") return undefined;
  const plusIdx = trimmed.indexOf("+");
  const withoutBuild = plusIdx >= 0 ? trimmed.slice(0, plusIdx) : trimmed;
  if (withoutBuild === "") return undefined;
  const dashIdx = withoutBuild.indexOf("-");
  const core = dashIdx >= 0 ? withoutBuild.slice(0, dashIdx) : withoutBuild;
  const preRaw = dashIdx >= 0 ? withoutBuild.slice(dashIdx + 1) : undefined;
  if (preRaw === "") return undefined;
  const parts = core.split(".");
  if (parts.length === 0 || parts.length > 3) return undefined;

  const p0 = parts[0]!;
  const major = isWildcard(p0) ? undefined : parseStrictUInt(p0);
  if (!isWildcard(p0) && major === undefined) return undefined;

  let minor: number | undefined;
  if (parts.length >= 2) {
    const p1 = parts[1]!;
    if (!isWildcard(p1)) {
      if (major === undefined) return undefined;
      minor = parseStrictUInt(p1);
      if (minor === undefined) return undefined;
    }
  }

  let patch: number | undefined;
  if (parts.length >= 3) {
    const p2 = parts[2]!;
    if (!isWildcard(p2)) {
      if (major === undefined || minor === undefined) return undefined;
      patch = parseStrictUInt(p2);
      if (patch === undefined) return undefined;
    }
  }

  const prerelease: PrereleasePart[] = [];
  if (preRaw !== undefined) {
    if (major === undefined || minor === undefined || patch === undefined) {
      return undefined;
    }
    for (const seg of preRaw.split(".")) {
      const parsed = parsePrereleasePart(seg);
      if (!parsed) return undefined;
      prerelease.push(parsed);
    }
  }
  return { major, minor, patch, prerelease };
}

const ZERO_VER: SemverVersion = {
  major: 0,
  minor: 0,
  patch: 0,
  prerelease: [],
};

function expandSingleComparator(
  token: string,
): readonly Comparator[] | undefined {
  const t = token.trim();
  if (t === "") return undefined;

  if (t.startsWith("^")) {
    const pv = parsePartialVersion(t.slice(1));
    if (!pv) return undefined;
    if (pv.major === undefined) {
      return [{ op: ">=", ver: ZERO_VER }];
    }
    const lower: SemverVersion = {
      major: pv.major,
      minor: pv.minor ?? 0,
      patch: pv.patch ?? 0,
      prerelease: pv.prerelease,
    };
    let upper: SemverVersion;
    if (pv.major > 0) {
      upper = { major: pv.major + 1, minor: 0, patch: 0, prerelease: [] };
    } else if (pv.minor !== undefined) {
      if (pv.minor > 0) {
        upper = { major: 0, minor: pv.minor + 1, patch: 0, prerelease: [] };
      } else if (pv.patch !== undefined) {
        upper = { major: 0, minor: 0, patch: pv.patch + 1, prerelease: [] };
      } else {
        upper = { major: 0, minor: 1, patch: 0, prerelease: [] };
      }
    } else {
      upper = { major: 1, minor: 0, patch: 0, prerelease: [] };
    }
    return [
      { op: ">=", ver: lower },
      { op: "<", ver: upper },
    ];
  }

  if (t.startsWith("~")) {
    const pv = parsePartialVersion(t.slice(1));
    if (!pv) return undefined;
    if (pv.major === undefined) {
      return [{ op: ">=", ver: ZERO_VER }];
    }
    const lower: SemverVersion = {
      major: pv.major,
      minor: pv.minor ?? 0,
      patch: pv.patch ?? 0,
      prerelease: pv.prerelease,
    };
    const upper: SemverVersion =
      pv.minor !== undefined
        ? { major: pv.major, minor: pv.minor + 1, patch: 0, prerelease: [] }
        : { major: pv.major + 1, minor: 0, patch: 0, prerelease: [] };
    return [
      { op: ">=", ver: lower },
      { op: "<", ver: upper },
    ];
  }

  let explicitOp: CompOp | undefined;
  let rest = t;
  if (t.startsWith(">=")) {
    explicitOp = ">=";
    rest = t.slice(2);
  } else if (t.startsWith("<=")) {
    explicitOp = "<=";
    rest = t.slice(2);
  } else if (t.startsWith(">")) {
    explicitOp = ">";
    rest = t.slice(1);
  } else if (t.startsWith("<")) {
    explicitOp = "<";
    rest = t.slice(1);
  } else if (t.startsWith("=")) {
    explicitOp = "=";
    rest = t.slice(1);
  }

  const pv = parsePartialVersion(rest);
  if (!pv) return undefined;

  if (explicitOp !== undefined) {
    if (pv.major === undefined && explicitOp === "=") {
      return [{ op: ">=", ver: ZERO_VER }];
    }
    return [
      {
        op: explicitOp,
        ver: {
          major: pv.major ?? 0,
          minor: pv.minor ?? 0,
          patch: pv.patch ?? 0,
          prerelease: pv.prerelease,
        },
      },
    ];
  }

  if (pv.major === undefined) {
    return [{ op: ">=", ver: ZERO_VER }];
  }
  if (pv.minor === undefined) {
    return [
      {
        op: ">=",
        ver: { major: pv.major, minor: 0, patch: 0, prerelease: [] },
      },
      {
        op: "<",
        ver: { major: pv.major + 1, minor: 0, patch: 0, prerelease: [] },
      },
    ];
  }
  if (pv.patch === undefined) {
    return [
      {
        op: ">=",
        ver: { major: pv.major, minor: pv.minor, patch: 0, prerelease: [] },
      },
      {
        op: "<",
        ver: { major: pv.major, minor: pv.minor + 1, patch: 0, prerelease: [] },
      },
    ];
  }
  return [
    {
      op: "=",
      ver: {
        major: pv.major,
        minor: pv.minor,
        patch: pv.patch,
        prerelease: pv.prerelease,
      },
    },
  ];
}

function parseSemverRange(
  range: string,
): readonly (readonly Comparator[])[] | undefined {
  const rawClauses = range.split("||");
  const parsedClauses: Comparator[][] = [];
  for (const rawClause of rawClauses) {
    const trimmed = rawClause.trim();
    if (trimmed === "") return undefined;
    const hyphenIdx = trimmed.indexOf(" - ");
    if (hyphenIdx >= 0) {
      const leftRaw = trimmed.slice(0, hyphenIdx);
      const rightRaw = trimmed.slice(hyphenIdx + 3);
      if (rightRaw.includes(" - ")) return undefined;
      const left = parsePartialVersion(leftRaw);
      const right = parsePartialVersion(rightRaw);
      if (!left || !right) return undefined;
      const comps: Comparator[] = [
        {
          op: ">=",
          ver: {
            major: left.major ?? 0,
            minor: left.minor ?? 0,
            patch: left.patch ?? 0,
            prerelease: left.prerelease,
          },
        },
      ];
      if (right.major !== undefined) {
        if (right.minor === undefined) {
          comps.push({
            op: "<",
            ver: {
              major: right.major + 1,
              minor: 0,
              patch: 0,
              prerelease: [],
            },
          });
        } else if (right.patch === undefined) {
          comps.push({
            op: "<",
            ver: {
              major: right.major,
              minor: right.minor + 1,
              patch: 0,
              prerelease: [],
            },
          });
        } else {
          comps.push({
            op: "<=",
            ver: {
              major: right.major,
              minor: right.minor,
              patch: right.patch,
              prerelease: right.prerelease,
            },
          });
        }
      }
      parsedClauses.push(comps);
      continue;
    }

    const comps: Comparator[] = [];
    for (const token of trimmed.split(/\s+/u)) {
      const expanded = expandSingleComparator(token);
      if (!expanded) return undefined;
      comps.push(...expanded);
    }
    if (comps.length === 0) return undefined;
    parsedClauses.push(comps);
  }
  return parsedClauses.length > 0 ? parsedClauses : undefined;
}

function compareSemver(a: SemverVersion, b: SemverVersion): number {
  if (a.major !== b.major) return a.major - b.major;
  if (a.minor !== b.minor) return a.minor - b.minor;
  if (a.patch !== b.patch) return a.patch - b.patch;
  if (a.prerelease.length === 0 && b.prerelease.length === 0) return 0;
  if (a.prerelease.length === 0) return 1;
  if (b.prerelease.length === 0) return -1;
  const len = Math.min(a.prerelease.length, b.prerelease.length);
  for (let i = 0; i < len; i += 1) {
    const pa = a.prerelease[i]!;
    const pb = b.prerelease[i]!;
    if (pa.kind === "num" && pb.kind === "num") {
      if (pa.value !== pb.value) return pa.value - pb.value;
    } else if (pa.kind === "num") {
      return -1;
    } else if (pb.kind === "num") {
      return 1;
    } else if (pa.value !== pb.value) {
      return pa.value < pb.value ? -1 : 1;
    }
  }
  return a.prerelease.length - b.prerelease.length;
}

function comparatorMatches(comp: Comparator, ver: SemverVersion): boolean {
  const cmp = compareSemver(ver, comp.ver);
  switch (comp.op) {
    case "=":
      return cmp === 0;
    case "<":
      return cmp < 0;
    case "<=":
      return cmp <= 0;
    case ">":
      return cmp > 0;
    case ">=":
      return cmp >= 0;
  }
}

/** Returns whether `range` is a syntactically valid npm semver range. */
export function isValidSemverRange(range: string): boolean {
  return parseSemverRange(range) !== undefined;
}

/** Evaluates whether a concrete `version` satisfies a semver `range`. */
export function semverMatches(version: string, range: string): boolean {
  const ver = parseSemverVersion(version);
  const clauses = parseSemverRange(range);
  if (!ver || !clauses) return false;
  return clauses.some((set) => {
    if (!set.every((c) => comparatorMatches(c, ver))) return false;
    if (ver.prerelease.length === 0) return true;
    return set.some(
      (c) =>
        c.ver.prerelease.length > 0 &&
        c.ver.major === ver.major &&
        c.ver.minor === ver.minor &&
        c.ver.patch === ver.patch,
    );
  });
}

function validateThisArg(value: unknown, path: string): void {
  if (value === undefined || value === "undefined") return;
  if (!isPlainRecord(value)) {
    throw invalidConfig(
      `Summary field "${path}" must be "undefined" or { fromArgIndex: number }.`,
    );
  }
  assertAllowedKeys(value, ["fromArgIndex"], path);
  assertNonNegativeInt(value.fromArgIndex, `${path}.fromArgIndex`);
}

function validateSlotRef(value: unknown, path: string): void {
  if (value === "receiver") return;
  if (!isPlainRecord(value)) {
    throw invalidConfig(
      `Summary field "${path}" must be "receiver" or { argIndex: number }.`,
    );
  }
  assertAllowedKeys(value, ["argIndex"], path);
  assertNonNegativeInt(value.argIndex, `${path}.argIndex`);
}

function validateValueRef(value: unknown, path: string): void {
  if (
    value === "receiver" ||
    value === "receiverElement" ||
    value === "return" ||
    value === "returnElement"
  ) {
    return;
  }
  if (!isPlainRecord(value)) {
    throw invalidConfig(`Invalid SummaryValueRef at "${path}".`);
  }
  switch (value.kind) {
    case "arg":
    case "argElement":
      assertAllowedKeys(value, ["kind", "index"], path);
      assertNonNegativeInt(value.index, `${path}.index`);
      return;
    case "callbackParam":
      assertAllowedKeys(
        value,
        ["kind", "callbackArgIndex", "paramIndex"],
        path,
      );
      assertNonNegativeInt(value.callbackArgIndex, `${path}.callbackArgIndex`);
      assertNonNegativeInt(value.paramIndex, `${path}.paramIndex`);
      return;
    case "callbackReturn":
      assertAllowedKeys(value, ["kind", "callbackArgIndex"], path);
      assertNonNegativeInt(value.callbackArgIndex, `${path}.callbackArgIndex`);
      return;
    case "callbackParamArg":
      assertAllowedKeys(
        value,
        ["kind", "callbackArgIndex", "callbackParamIndex", "argIndex"],
        path,
      );
      assertNonNegativeInt(value.callbackArgIndex, `${path}.callbackArgIndex`);
      assertNonNegativeInt(
        value.callbackParamIndex,
        `${path}.callbackParamIndex`,
      );
      assertNonNegativeInt(value.argIndex, `${path}.argIndex`);
      return;
    default:
      throw invalidConfig(
        `Unknown SummaryValueRef kind ${JSON.stringify(value.kind)} at "${path}".`,
      );
  }
}

function validateTargetMatcher(value: unknown, path: string): void {
  if (!isPlainRecord(value)) {
    throw invalidConfig(`Summary matcher at "${path}" must be an object.`);
  }
  assertAllowedKeys(
    value,
    ["package", "version", "exportPath", "module", "memberPath"],
    path,
  );
  const pkg = assertOptionalNonEmptyString(value.package, `${path}.package`);
  const version = assertOptionalNonEmptyString(
    value.version,
    `${path}.version`,
  );
  const exportPath = assertOptionalNonEmptyString(
    value.exportPath,
    `${path}.exportPath`,
  );
  const modulePath = assertOptionalNonEmptyString(
    value.module,
    `${path}.module`,
  );

  if (pkg !== undefined && modulePath !== undefined) {
    throw invalidConfig(
      `Summary matcher at "${path}" cannot specify both "package" and "module".`,
    );
  }
  if (
    pkg === undefined &&
    (version !== undefined || exportPath !== undefined)
  ) {
    throw invalidConfig(
      `Summary matcher at "${path}" requires "package" when "version" or "exportPath" is specified.`,
    );
  }
  if (version !== undefined && !isValidSemverRange(version)) {
    throw invalidConfig(
      `Summary matcher at "${path}.version" has invalid semver range "${version}".`,
    );
  }
  if (!Array.isArray(value.memberPath) || value.memberPath.length === 0) {
    throw invalidConfig(
      `Summary matcher at "${path}.memberPath" must be a non-empty string array.`,
    );
  }
  for (let i = 0; i < value.memberPath.length; i += 1) {
    assertNonEmptyString(value.memberPath[i], `${path}.memberPath[${i}]`);
  }
}

function validateCallAspect(value: unknown, path: string): void {
  if (!isPlainRecord(value)) {
    throw invalidConfig(
      `Summary "calls" aspect at "${path}" must be an object.`,
    );
  }
  assertAllowedKeys(
    value,
    ["completeness", "invokesCallbacks", "forwardsCall"],
    path,
  );
  assertOptionalCompleteness(value.completeness, `${path}.completeness`);

  if (value.invokesCallbacks !== undefined) {
    if (!Array.isArray(value.invokesCallbacks)) {
      throw invalidConfig(
        `Summary field "${path}.invokesCallbacks" must be an array.`,
      );
    }
    for (let i = 0; i < value.invokesCallbacks.length; i += 1) {
      const cb = value.invokesCallbacks[i];
      const cbPath = `${path}.invokesCallbacks[${i}]`;
      if (!isPlainRecord(cb)) {
        throw invalidConfig(`Summary item "${cbPath}" must be an object.`);
      }
      assertAllowedKeys(
        cb,
        ["callbackArgIndex", "timing", "cardinality", "thisArg", "eventName"],
        cbPath,
      );
      assertNonNegativeInt(cb.callbackArgIndex, `${cbPath}.callbackArgIndex`);
      if (
        cb.timing !== "synchronous" &&
        cb.timing !== "deferred" &&
        cb.timing !== "lifecycle-end"
      ) {
        throw invalidConfig(
          `Summary field "${cbPath}.timing" must be "synchronous", "deferred", or "lifecycle-end".`,
        );
      }
      if (
        cb.cardinality !== undefined &&
        cb.cardinality !== "at-most-once" &&
        cb.cardinality !== "zero-or-more" &&
        cb.cardinality !== "once"
      ) {
        throw invalidConfig(
          `Summary field "${cbPath}.cardinality" must be "at-most-once", "zero-or-more", or "once".`,
        );
      }
      validateThisArg(cb.thisArg, `${cbPath}.thisArg`);
      assertOptionalNonEmptyString(cb.eventName, `${cbPath}.eventName`);
    }
  }

  if (value.forwardsCall !== undefined) {
    const fc = value.forwardsCall;
    const fcPath = `${path}.forwardsCall`;
    if (!isPlainRecord(fc)) {
      throw invalidConfig(`Summary field "${fcPath}" must be an object.`);
    }
    assertAllowedKeys(
      fc,
      ["target", "mode", "thisArg", "argsFromIndex", "argsArrayIndex"],
      fcPath,
    );
    validateSlotRef(fc.target, `${fcPath}.target`);
    if (fc.mode !== "call" && fc.mode !== "apply" && fc.mode !== "bind") {
      throw invalidConfig(
        `Summary field "${fcPath}.mode" must be "call", "apply", or "bind".`,
      );
    }
    validateThisArg(fc.thisArg, `${fcPath}.thisArg`);
    if (fc.argsFromIndex !== undefined) {
      assertNonNegativeInt(fc.argsFromIndex, `${fcPath}.argsFromIndex`);
    }
    if (fc.argsArrayIndex !== undefined) {
      assertNonNegativeInt(fc.argsArrayIndex, `${fcPath}.argsArrayIndex`);
    }
  }
}

function validateValueAspect(value: unknown, path: string): void {
  if (!isPlainRecord(value)) {
    throw invalidConfig(
      `Summary "values" aspect at "${path}" must be an object.`,
    );
  }
  assertAllowedKeys(value, ["completeness", "flows"], path);
  assertOptionalCompleteness(value.completeness, `${path}.completeness`);
  if (value.flows !== undefined) {
    if (!Array.isArray(value.flows)) {
      throw invalidConfig(`Summary field "${path}.flows" must be an array.`);
    }
    for (let i = 0; i < value.flows.length; i += 1) {
      const flow = value.flows[i];
      const flowPath = `${path}.flows[${i}]`;
      if (!isPlainRecord(flow)) {
        throw invalidConfig(`Summary item "${flowPath}" must be an object.`);
      }
      assertAllowedKeys(flow, ["from", "to", "preserveObjectPath"], flowPath);
      validateValueRef(flow.from, `${flowPath}.from`);
      validateValueRef(flow.to, `${flowPath}.to`);
      if (
        flow.preserveObjectPath !== undefined &&
        typeof flow.preserveObjectPath !== "boolean"
      ) {
        throw invalidConfig(
          `Summary field "${flowPath}.preserveObjectPath" must be a boolean.`,
        );
      }
    }
  }
}

function validateInstanceAspect(value: unknown, path: string): void {
  if (!isPlainRecord(value)) {
    throw invalidConfig(
      `Summary "instances" aspect at "${path}" must be an object.`,
    );
  }
  assertAllowedKeys(value, ["completeness", "outcome"], path);
  assertOptionalCompleteness(value.completeness, `${path}.completeness`);
  const outcome = value.outcome;
  const outPath = `${path}.outcome`;
  if (!isPlainRecord(outcome)) {
    throw invalidConfig(`Summary field "${outPath}" must be an object.`);
  }
  switch (outcome.kind) {
    case "allocates-fresh-instance":
    case "none":
      assertAllowedKeys(outcome, ["kind"], outPath);
      return;
    case "forwards-instance":
      assertAllowedKeys(outcome, ["kind", "from"], outPath);
      validateSlotRef(outcome.from, `${outPath}.from`);
      return;
    default:
      throw invalidConfig(
        `Summary field "${outPath}.kind" has unsupported value ${JSON.stringify(outcome.kind)}.`,
      );
  }
}

function validateEffectAspect(value: unknown, path: string): void {
  if (!isPlainRecord(value)) {
    throw invalidConfig(
      `Summary "effects" aspect at "${path}" must be an object.`,
    );
  }
  assertAllowedKeys(
    value,
    ["completeness", "resourceOps", "mutatesReceiver"],
    path,
  );
  assertOptionalCompleteness(value.completeness, `${path}.completeness`);
  if (
    value.mutatesReceiver !== undefined &&
    typeof value.mutatesReceiver !== "boolean"
  ) {
    throw invalidConfig(
      `Summary field "${path}.mutatesReceiver" must be a boolean.`,
    );
  }
  if (value.resourceOps !== undefined) {
    if (!Array.isArray(value.resourceOps)) {
      throw invalidConfig(
        `Summary field "${path}.resourceOps" must be an array.`,
      );
    }
    for (let i = 0; i < value.resourceOps.length; i += 1) {
      const op = value.resourceOps[i];
      const opPath = `${path}.resourceOps[${i}]`;
      if (!isPlainRecord(op)) {
        throw invalidConfig(`Summary item "${opPath}" must be an object.`);
      }
      switch (op.kind) {
        case "disposes-target":
        case "replaces-previous-value":
          assertAllowedKeys(op, ["kind", "target"], opPath);
          validateSlotRef(op.target, `${opPath}.target`);
          break;
        case "registers-into-container":
        case "detaches-from-container":
          assertAllowedKeys(op, ["kind", "item", "container"], opPath);
          validateSlotRef(op.item, `${opPath}.item`);
          validateSlotRef(op.container, `${opPath}.container`);
          break;
        default:
          throw invalidConfig(
            `Summary field "${opPath}.kind" has unsupported value ${JSON.stringify(op.kind)}.`,
          );
      }
    }
  }
}

/** Validates a raw pure-data summary pack and returns it typed as `BehaviorSummaryPack`. */
export function validateBehaviorSummaryPack(
  rawPack: unknown,
  originLabel = "<inline>",
): BehaviorSummaryPack {
  assertPureData(rawPack, `summaryPack(${originLabel})`);
  if (!isPlainRecord(rawPack)) {
    throw invalidConfig(
      `Behavior summary pack from "${originLabel}" must be a plain object.`,
    );
  }
  assertAllowedKeys(
    rawPack,
    ["id", "version", "description", "entries"],
    `summaryPack(${originLabel})`,
  );
  const packId = assertNonEmptyString(
    rawPack.id,
    `summaryPack(${originLabel}).id`,
  );
  assertOptionalNonEmptyString(
    rawPack.version,
    `summaryPack(${originLabel}).version`,
  );
  if (
    rawPack.description !== undefined &&
    typeof rawPack.description !== "string"
  ) {
    throw invalidConfig(
      `Summary field "summaryPack(${originLabel}).description" must be a string.`,
    );
  }
  if (!Array.isArray(rawPack.entries)) {
    throw invalidConfig(
      `Summary field "summaryPack(${originLabel}).entries" must be an array.`,
    );
  }

  for (let i = 0; i < rawPack.entries.length; i += 1) {
    const entry = rawPack.entries[i];
    const entryPath = `summaryPack(${packId}).entries[${i}]`;
    if (!isPlainRecord(entry)) {
      throw invalidConfig(`Summary entry "${entryPath}" must be an object.`);
    }
    assertAllowedKeys(
      entry,
      ["id", "match", "description", "calls", "values", "instances", "effects"],
      entryPath,
    );
    assertOptionalNonEmptyString(entry.id, `${entryPath}.id`);
    if (
      entry.description !== undefined &&
      typeof entry.description !== "string"
    ) {
      throw invalidConfig(
        `Summary field "${entryPath}.description" must be a string.`,
      );
    }
    validateTargetMatcher(entry.match, `${entryPath}.match`);
    if (
      entry.calls === undefined &&
      entry.values === undefined &&
      entry.instances === undefined &&
      entry.effects === undefined
    ) {
      throw invalidConfig(
        `Summary entry "${entryPath}" must declare at least one aspect ("calls", "values", "instances", or "effects").`,
      );
    }
    if (entry.calls !== undefined) {
      validateCallAspect(entry.calls, `${entryPath}.calls`);
    }
    if (entry.values !== undefined) {
      validateValueAspect(entry.values, `${entryPath}.values`);
    }
    if (entry.instances !== undefined) {
      validateInstanceAspect(entry.instances, `${entryPath}.instances`);
    }
    if (entry.effects !== undefined) {
      validateEffectAspect(entry.effects, `${entryPath}.effects`);
    }
  }

  return rawPack as BehaviorSummaryPack;
}

/**
 * Defines a typed pure-data behavior summary pack, validates its structure and
 * pure-data invariant immediately, and attaches provenance metadata.
 */
export function defineSummaries(
  pack: BehaviorSummaryPack,
): BehaviorSummaryPack {
  const validated = validateBehaviorSummaryPack(pack, "defineSummaries");
  const message =
    typeof validated.description === "string" &&
    validated.description.trim() !== ""
      ? validated.description
      : `Behavior summary pack "${validated.id}"`;
  return registerProvenance(validated, [
    {
      kind: "summary",
      summaryId: validated.id,
      summarySource: "local",
      message,
    },
  ]);
}

/** Normalizes and validates `OpenProjectOptions.summaries` / `SessionUpdateOptions.summaries`. */
export function normalizeSummaryOptions(
  options: SummaryOptions | undefined,
): NormalizedSummaryOptions {
  if (options === undefined) {
    return {
      packs: [],
      files: [],
      discoverySources: [...ALL_DISCOVERY_SOURCES],
    };
  }
  if (!isPlainRecord(options)) {
    throw invalidConfig('Option "summaries" must be a plain object.');
  }
  assertAllowedKeys(options, ["packs", "files", "discovery"], "summaries");

  const packs: BehaviorSummaryPack[] = [];
  if (options.packs !== undefined) {
    if (!Array.isArray(options.packs)) {
      throw invalidConfig('Option "summaries.packs" must be an array.');
    }
    for (let i = 0; i < options.packs.length; i += 1) {
      packs.push(
        validateBehaviorSummaryPack(options.packs[i], `summaries.packs[${i}]`),
      );
    }
  }

  const files: string[] = [];
  if (options.files !== undefined) {
    if (!Array.isArray(options.files)) {
      throw invalidConfig('Option "summaries.files" must be an array.');
    }
    for (let i = 0; i < options.files.length; i += 1) {
      files.push(
        assertNonEmptyString(options.files[i], `summaries.files[${i}]`),
      );
    }
  }

  let discoverySources: readonly SummaryDiscoverySource[];
  if (options.discovery === undefined || options.discovery === true) {
    discoverySources = [...ALL_DISCOVERY_SOURCES];
  } else if (options.discovery === false) {
    discoverySources = [];
  } else if (Array.isArray(options.discovery)) {
    const deduped: SummaryDiscoverySource[] = [];
    const seen = new Set<SummaryDiscoverySource>();
    for (const rawSource of options.discovery) {
      if (
        rawSource !== "local" &&
        rawSource !== "package" &&
        rawSource !== "community" &&
        rawSource !== "builtin"
      ) {
        throw invalidConfig(
          `Invalid summary discovery source ${JSON.stringify(rawSource)}; expected "local", "package", "community", or "builtin".`,
        );
      }
      if (!seen.has(rawSource)) {
        seen.add(rawSource);
        deduped.push(rawSource);
      }
    }
    discoverySources = deduped;
  } else {
    throw invalidConfig(
      'Option "summaries.discovery" must be a boolean or an array of discovery source names.',
    );
  }

  return { packs, files, discoverySources };
}

function canonicalRealPath(filePath: string): string {
  const abs = resolve(filePath);
  try {
    return realpathSync.native(abs).replaceAll("\\", "/");
  } catch {
    return abs.replaceAll("\\", "/");
  }
}

async function loadSummaryFile(
  absPath: string,
  cwd: string,
): Promise<{
  readonly canonicalAbs: string;
  readonly relPath: string;
  readonly pack: BehaviorSummaryPack;
}> {
  let bytes: Buffer;
  try {
    const st = statSync(absPath);
    if (!st.isFile()) {
      throw new Error("Path is not a regular file");
    }
    bytes = readFileSync(absPath);
  } catch (err) {
    const detail = err instanceof Error ? err.message : String(err);
    throw invalidConfig(
      `Failed to read behavior summary file "${absPath}": ${detail}`,
      err,
    );
  }

  const canonicalAbs = canonicalRealPath(absPath);
  const relPath = relative(cwd, canonicalAbs).replaceAll("\\", "/");
  const ext = extname(canonicalAbs).toLowerCase();
  if (!SUPPORTED_SUMMARY_EXTENSIONS.has(ext)) {
    throw invalidConfig(
      `Unsupported behavior summary file extension "${ext}" for "${relPath}". Expected .ts, .mts, .js, .mjs, or .json.`,
    );
  }

  if (ext === ".json") {
    let parsed: unknown;
    try {
      parsed = JSON.parse(bytes.toString("utf8"));
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      throw invalidConfig(
        `Invalid JSON in behavior summary file "${relPath}": ${detail}`,
        err,
      );
    }
    return {
      canonicalAbs,
      relPath,
      pack: validateBehaviorSummaryPack(parsed, relPath),
    };
  }

  const sha256 = createHash("sha256").update(bytes).digest("hex");
  const fileUrl = new URL(pathToFileURL(canonicalAbs).href);
  fileUrl.searchParams.set("v", sha256);

  let mod: unknown;
  try {
    mod = await import(fileUrl.href);
  } catch (err) {
    const detail = err instanceof Error ? err.message : String(err);
    throw invalidConfig(
      `Failed to load behavior summary module "${relPath}": ${detail}`,
      err,
    );
  }
  if (typeof mod !== "object" || mod === null || !("default" in mod)) {
    throw invalidConfig(
      `Behavior summary module "${relPath}" must have a default export (e.g. export default defineSummaries(...)).`,
    );
  }
  const exported = (mod as { readonly default: unknown }).default;
  return {
    canonicalAbs,
    relPath,
    pack: validateBehaviorSummaryPack(exported, relPath),
  };
}

function communitySummaryPackageName(pkgName: string): string {
  if (pkgName.startsWith("@")) {
    const normalized = pkgName.slice(1).replace("/", "__");
    return `@tssae-summaries/${normalized}`;
  }
  return `@tssae-summaries/${pkgName}`;
}

function isPathInsideDir(parentDir: string, candidatePath: string): boolean {
  const rel = relative(parentDir, candidatePath);
  return rel !== "" && !rel.startsWith("..") && !isAbsolute(rel);
}

function readPackageJsonObject(
  pkgJsonPath: string,
): Record<string, unknown> | undefined {
  if (!existsSync(pkgJsonPath)) return undefined;
  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(pkgJsonPath, "utf8"));
  } catch (err) {
    const detail = err instanceof Error ? err.message : String(err);
    throw invalidConfig(
      `Failed to parse package.json at "${pkgJsonPath}": ${detail}`,
      err,
    );
  }
  if (!isPlainRecord(parsed)) return undefined;
  return parsed;
}

function canonicalStableValue(value: unknown): unknown {
  if (value === null || typeof value !== "object") return value;
  if (Array.isArray(value)) return value.map(canonicalStableValue);
  const rec = value as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(rec).toSorted()) {
    const v = rec[key];
    if (v !== undefined) {
      out[key] = canonicalStableValue(v);
    }
  }
  return out;
}

function normalizeAspectForCompare(
  aspect: SummaryAspectName,
  raw: Record<string, unknown>,
): Record<string, unknown> {
  const copy = { ...raw };
  if (aspect === "calls") {
    if (copy.invokesCallbacks === undefined) copy.invokesCallbacks = [];
  } else if (aspect === "values") {
    if (copy.flows === undefined) copy.flows = [];
  } else if (aspect === "effects") {
    if (copy.resourceOps === undefined) copy.resourceOps = [];
  }
  return canonicalStableValue(copy) as Record<string, unknown>;
}

function stableJson(value: unknown): string {
  return JSON.stringify(canonicalStableValue(value));
}

function matcherTargetKey(matcher: SummaryTargetMatcher): string {
  const normModule =
    matcher.module !== undefined
      ? matcher.module.replace(/^\.\//u, "")
      : undefined;
  const normExport =
    matcher.exportPath !== undefined
      ? matcher.exportPath === "./"
        ? "."
        : matcher.exportPath
      : undefined;
  return JSON.stringify([
    matcher.package ?? null,
    normExport ?? null,
    normModule ?? null,
    matcher.memberPath,
  ]);
}

function describeMatcherTarget(matcher: SummaryTargetMatcher): string {
  const member = matcher.memberPath.join(".");
  if (matcher.package !== undefined) {
    const exp =
      matcher.exportPath !== undefined && matcher.exportPath !== "."
        ? ` (${matcher.exportPath})`
        : "";
    return `${matcher.package}${exp}:${member}`;
  }
  if (matcher.module !== undefined) {
    return `${matcher.module}:${member}`;
  }
  return member;
}

function versionConstraintsCompatible(
  vA: string | undefined,
  vB: string | undefined,
  resolvedPkgVersion: string | undefined,
): boolean {
  if (vA === undefined || vB === undefined) return true;
  if (vA === vB) return true;
  if (resolvedPkgVersion !== undefined) {
    return (
      semverMatches(resolvedPkgVersion, vA) &&
      semverMatches(resolvedPkgVersion, vB)
    );
  }
  return true;
}

function checkStaticSameTierConflicts(
  taggedPacks: readonly TaggedSummaryPack[],
  resolvedVersionsByPackage: ReadonlyMap<string, string>,
): void {
  const byTier = new Map<SummaryDiscoverySource, TaggedSummaryPack[]>();
  for (const tagged of taggedPacks) {
    const list = byTier.get(tagged.source);
    if (list) list.push(tagged);
    else byTier.set(tagged.source, [tagged]);
  }

  const aspects: readonly SummaryAspectName[] = [
    "calls",
    "values",
    "instances",
    "effects",
  ];

  for (const [source, tierPacks] of byTier.entries()) {
    type Candidate = {
      readonly packId: string;
      readonly originFile: string | undefined;
      readonly entry: BehaviorSummaryEntry;
      readonly aspectName: SummaryAspectName;
      readonly aspectPayload: Record<string, unknown>;
    };
    const buckets = new Map<string, Candidate[]>();

    for (const tagged of tierPacks) {
      for (const entry of tagged.pack.entries) {
        const key = matcherTargetKey(entry.match);
        for (const aspectName of aspects) {
          const aspectVal = entry[aspectName];
          if (
            aspectVal === undefined ||
            aspectVal.completeness !== "complete"
          ) {
            continue;
          }
          const candidate: Candidate = {
            packId: tagged.pack.id,
            originFile: tagged.originFile,
            entry,
            aspectName,
            aspectPayload: normalizeAspectForCompare(
              aspectName,
              aspectVal as unknown as Record<string, unknown>,
            ),
          };
          const bucketKey = `${aspectName}:${key}`;
          const existing = buckets.get(bucketKey);
          if (existing) existing.push(candidate);
          else buckets.set(bucketKey, [candidate]);
        }
      }
    }

    for (const candidates of buckets.values()) {
      if (candidates.length < 2) continue;
      for (let i = 0; i < candidates.length; i += 1) {
        for (let j = i + 1; j < candidates.length; j += 1) {
          const a = candidates[i]!;
          const b = candidates[j]!;
          const pkgVer =
            a.entry.match.package !== undefined
              ? resolvedVersionsByPackage.get(a.entry.match.package)
              : undefined;
          if (
            !versionConstraintsCompatible(
              a.entry.match.version,
              b.entry.match.version,
              pkgVer,
            )
          ) {
            continue;
          }
          if (stableJson(a.aspectPayload) !== stableJson(b.aspectPayload)) {
            const targetDescription = describeMatcherTarget(a.entry.match);
            const partyA: SummaryConflictParty = {
              source,
              packId: a.packId,
              ...(a.entry.id !== undefined ? { entryId: a.entry.id } : {}),
              ...(a.originFile !== undefined
                ? { originFile: a.originFile }
                : {}),
              matcher: a.entry.match,
              aspect: a.aspectName,
              completeness: "complete",
              summary: a.aspectPayload,
            };
            const partyB: SummaryConflictParty = {
              source,
              packId: b.packId,
              ...(b.entry.id !== undefined ? { entryId: b.entry.id } : {}),
              ...(b.originFile !== undefined
                ? { originFile: b.originFile }
                : {}),
              matcher: b.entry.match,
              aspect: b.aspectName,
              completeness: "complete",
              summary: b.aspectPayload,
            };
            const details: SummaryConflictDetails = {
              source,
              aspect: a.aspectName,
              targetDescription,
              reason: `two complete summaries in the "${source}" tier specify conflicting "${a.aspectName}" behaviors`,
              parties: [partyA, partyB],
            };
            const originA = partyA.originFile ?? "<in-memory>";
            const originB = partyB.originFile ?? "<in-memory>";
            throw new TssaeError(
              "SUMMARY_CONFLICT",
              `Behavior summary conflict in "${source}" tier for "${targetDescription}" on aspect "${a.aspectName}": ${details.reason}. ` +
                `Party 1: pack "${partyA.packId}" (${originA}, matcher ${JSON.stringify(partyA.matcher)}) declared ${JSON.stringify(partyA.summary)}. ` +
                `Party 2: pack "${partyB.packId}" (${originB}, matcher ${JSON.stringify(partyB.matcher)}) declared ${JSON.stringify(partyB.summary)}.`,
              { summaryConflict: details },
            );
          }
        }
      }
    }
  }
}

export type DiscoveredSummariesResult = {
  readonly payload: SummaryConfigPayload;
  readonly registeredFiles: readonly {
    readonly filePath: string;
    readonly packId: string;
  }[];
};

/**
 * Discovers, loads, validates, and conflict-checks behavior summaries across all enabled tiers
 * (`local`, `package`, `community`, `builtin`), triggering package/community discovery strictly
 * for packages resolved by the analyzed programs.
 */
export async function discoverAndLoadSummaries(params: {
  readonly cwd: string;
  readonly projectConfigFiles: readonly string[];
  readonly resolvedPackages: readonly ResolvedPackageSummary[];
  readonly options: NormalizedSummaryOptions;
  readonly registrar?: AdditionalInputRegistrar;
}): Promise<DiscoveredSummariesResult> {
  const { cwd, projectConfigFiles, resolvedPackages, options, registrar } =
    params;
  const discoverySet = new Set(options.discoverySources);
  const taggedPacks: TaggedSummaryPack[] = [];
  const registeredFiles: {
    readonly filePath: string;
    readonly packId: string;
  }[] = [];
  const loadedCanonicalFiles = new Set<string>();

  const registerLoadedFile = (canonicalAbs: string, packId: string): void => {
    registeredFiles.push({ filePath: canonicalAbs, packId });
    registrar?.registerAdditionalInput(canonicalAbs, {
      kind: "summary",
      id: packId,
    });
  };

  // 1. Explicit in-memory local packs
  for (const pack of options.packs) {
    taggedPacks.push({ source: "local", pack });
  }

  // 2. Explicit local summary files
  for (const fileSpec of options.files) {
    const absPath = resolve(cwd, fileSpec);
    const loaded = await loadSummaryFile(absPath, cwd);
    if (!loadedCanonicalFiles.has(loaded.canonicalAbs)) {
      loadedCanonicalFiles.add(loaded.canonicalAbs);
      registerLoadedFile(loaded.canonicalAbs, loaded.pack.id);
      taggedPacks.push({
        source: "local",
        originFile: loaded.relPath,
        pack: loaded.pack,
      });
    }
  }

  // 3. Local convention files next to each opened tsconfig.json / jsconfig.json
  if (discoverySet.has("local")) {
    const tsconfigDirs = [
      ...new Set(projectConfigFiles.map((cfg) => dirname(resolve(cwd, cfg)))),
    ].toSorted();
    for (const dir of tsconfigDirs) {
      for (const fileName of LOCAL_CONVENTION_FILES) {
        const candidate = join(dir, fileName);
        if (!existsSync(candidate)) continue;
        const loaded = await loadSummaryFile(candidate, cwd);
        if (!loadedCanonicalFiles.has(loaded.canonicalAbs)) {
          loadedCanonicalFiles.add(loaded.canonicalAbs);
          registerLoadedFile(loaded.canonicalAbs, loaded.pack.id);
          taggedPacks.push({
            source: "local",
            originFile: loaded.relPath,
            pack: loaded.pack,
          });
        }
      }
    }
  }

  // 4. Package-bundled summaries (`package.json` `"tssaeSummaries"` field) for resolved packages only
  if (discoverySet.has("package")) {
    const seenPkgRoots = new Set<string>();
    for (const pkg of resolvedPackages) {
      if (pkg.packageRoot === undefined) continue;
      const canonicalRoot = canonicalRealPath(pkg.packageRoot);
      if (seenPkgRoots.has(canonicalRoot)) continue;
      seenPkgRoots.add(canonicalRoot);

      const pkgJsonPath = join(canonicalRoot, "package.json");
      const pkgJson = readPackageJsonObject(pkgJsonPath);
      if (!pkgJson || pkgJson.tssaeSummaries === undefined) continue;

      const summaryField = pkgJson.tssaeSummaries;
      if (typeof summaryField !== "string" || summaryField.trim() === "") {
        throw invalidConfig(
          `Package "${pkg.name}" declares an invalid "tssaeSummaries" field in "${pkgJsonPath}": expected a non-empty relative path string.`,
        );
      }
      const targetPath = resolve(canonicalRoot, summaryField);
      if (!isPathInsideDir(canonicalRoot, targetPath)) {
        throw invalidConfig(
          `Package "${pkg.name}" "tssaeSummaries" path "${summaryField}" escapes package directory "${canonicalRoot}".`,
        );
      }
      const loaded = await loadSummaryFile(targetPath, cwd);
      if (!loadedCanonicalFiles.has(loaded.canonicalAbs)) {
        loadedCanonicalFiles.add(loaded.canonicalAbs);
        registerLoadedFile(loaded.canonicalAbs, loaded.pack.id);
        taggedPacks.push({
          source: "package",
          originFile: loaded.relPath,
          pack: loaded.pack,
        });
      }
    }
  }

  // 5. Community summary packages `@tssae-summaries/<pkg>` for resolved packages only
  if (discoverySet.has("community")) {
    const seenCommunityDirs = new Set<string>();
    for (const pkg of resolvedPackages) {
      const communityName = communitySummaryPackageName(pkg.name);
      const searchRoots = [
        ...new Set([...pkg.projectDirs.map((d) => resolve(cwd, d)), cwd]),
      ];

      for (const searchRoot of searchRoots) {
        const req = createRequire(
          join(searchRoot, "__tssae_summary_loader__.cjs"),
        );
        let communityPkgJsonPath: string | undefined;
        try {
          communityPkgJsonPath = req.resolve(`${communityName}/package.json`);
        } catch {
          // Try resolving main entry directly when package.json is not exported
          try {
            const entry = req.resolve(communityName);
            communityPkgJsonPath = findNearestPackageJson(entry);
            if (!communityPkgJsonPath) {
              const loaded = await loadSummaryFile(entry, cwd);
              if (!loadedCanonicalFiles.has(loaded.canonicalAbs)) {
                loadedCanonicalFiles.add(loaded.canonicalAbs);
                registerLoadedFile(loaded.canonicalAbs, loaded.pack.id);
                taggedPacks.push({
                  source: "community",
                  originFile: loaded.relPath,
                  pack: loaded.pack,
                });
              }
              continue;
            }
          } catch {
            continue;
          }
        }

        if (!communityPkgJsonPath) continue;
        const communityRoot = canonicalRealPath(dirname(communityPkgJsonPath));
        if (seenCommunityDirs.has(communityRoot)) continue;
        seenCommunityDirs.add(communityRoot);

        const commPkgJson = readPackageJsonObject(communityPkgJsonPath);
        let entryFilePath: string | undefined;
        if (commPkgJson && commPkgJson.tssaeSummaries !== undefined) {
          if (
            typeof commPkgJson.tssaeSummaries !== "string" ||
            commPkgJson.tssaeSummaries.trim() === ""
          ) {
            throw invalidConfig(
              `Community summary package "${communityName}" has invalid "tssaeSummaries" field in "${communityPkgJsonPath}".`,
            );
          }
          entryFilePath = resolve(communityRoot, commPkgJson.tssaeSummaries);
          if (!isPathInsideDir(communityRoot, entryFilePath)) {
            throw invalidConfig(
              `Community summary package "${communityName}" "tssaeSummaries" path escapes package directory.`,
            );
          }
        } else {
          try {
            entryFilePath = req.resolve(communityName);
          } catch (err) {
            const detail = err instanceof Error ? err.message : String(err);
            throw invalidConfig(
              `Community summary package "${communityName}" has no resolvable entry point: ${detail}`,
              err,
            );
          }
        }

        const loaded = await loadSummaryFile(entryFilePath, cwd);
        if (!loadedCanonicalFiles.has(loaded.canonicalAbs)) {
          loadedCanonicalFiles.add(loaded.canonicalAbs);
          registerLoadedFile(loaded.canonicalAbs, loaded.pack.id);
          taggedPacks.push({
            source: "community",
            originFile: loaded.relPath,
            pack: loaded.pack,
          });
        }
      }
    }
  }

  const resolvedVersions = new Map<string, string>();
  for (const pkg of resolvedPackages) {
    if (pkg.version !== undefined) {
      resolvedVersions.set(pkg.name, pkg.version);
    }
  }
  checkStaticSameTierConflicts(taggedPacks, resolvedVersions);

  return {
    payload: {
      packs: taggedPacks,
      enableBuiltin: discoverySet.has("builtin"),
    },
    registeredFiles,
  };
}

function findNearestPackageJson(startFile: string): string | undefined {
  let cur = dirname(resolve(startFile));
  for (let i = 0; i < 8; i += 1) {
    const candidate = join(cur, "package.json");
    if (existsSync(candidate)) return candidate;
    const parent = dirname(cur);
    if (parent === cur) break;
    cur = parent;
  }
  return undefined;
}
