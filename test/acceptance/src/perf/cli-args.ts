import type { CorpusProfileId, PerfScenarioId } from "./types.ts";

export const DEFAULT_SCENARIOS: readonly PerfScenarioId[] = [
  "openProject",
  "coldQuery",
  "cachedQuery",
  "updateQuery",
];

export type PerfCliOptions = {
  readonly profile: CorpusProfileId;
  readonly baselineRev?: string;
  readonly rounds: number;
  readonly warmup: number;
  readonly scenarios: readonly PerfScenarioId[];
  readonly includeObserveOverhead: boolean;
  readonly refreshCorpus: boolean;
  readonly noCorpusCache: boolean;
  readonly refreshBaseline: boolean;
  readonly jsonOut?: string;
  readonly markdownOut?: string;
  readonly maxLatencyRegressionRatio?: number;
  readonly maxMemoryGrowthRatio?: number;
  readonly help: boolean;
};

function parsePositiveInt(raw: string, flag: string): number {
  if (!/^\d+$/u.test(raw.trim())) {
    throw new Error(
      `Invalid ${flag} value "${raw}": expected positive integer.`,
    );
  }
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new Error(
      `Invalid ${flag} value "${raw}": expected positive integer.`,
    );
  }
  return value;
}

function parseNonNegativeInt(raw: string, flag: string): number {
  if (!/^\d+$/u.test(raw.trim())) {
    throw new Error(
      `Invalid ${flag} value "${raw}": expected non-negative integer.`,
    );
  }
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(
      `Invalid ${flag} value "${raw}": expected non-negative integer.`,
    );
  }
  return value;
}

function parseNonNegativeRatio(raw: string, flag: string): number {
  const value = Number(raw);
  if (!Number.isFinite(value) || value < 0) {
    throw new Error(
      `Invalid ${flag} value "${raw}": expected non-negative number.`,
    );
  }
  return value;
}

/** Formats CLI usage instructions for the TSSAE A/B performance runner. */
export function formatPerfCliUsage(): string {
  return [
    "Usage: pnpm perf -- [options]",
    "",
    "Options:",
    '  --profile <medium|large>          Corpus profile (default: "medium" for perf:gate, "large" for perf)',
    "  --baseline <rev>                  Git revision to compare against (default: merge-base HEAD main / origin/main)",
    "  --rounds <n>                      Paired ABBA sampling rounds per scenario (default: 7)",
    "  --warmup <n>                      Untimed warmup rounds per version before sampling (default: 1)",
    "  --scenarios <list>                Comma-separated scenarios (openProject,coldQuery,cachedQuery,updateQuery)",
    "  --skip-observe-overhead           Skip standalone off/summary/detail observe overhead measurement",
    "  --refresh-corpus                  Rebuild cached corpus checkout from source",
    "  --no-corpus-cache                 Bypass corpus cache and fetch/extract fresh",
    "  --refresh-baseline                Rebuild cached merge-base tssae tarball",
    "  --max-latency-regression <ratio>  Override query p50 budget ratio (default: 0.05)",
    "  --max-memory-growth <ratio>       Override peak memory budget ratio (default: 0.10)",
    "  --json-out <path>                 Write full structured JSON report",
    "  --markdown-out <path>             Write Markdown report",
    "  -h, --help                        Show help",
  ].join("\n");
}

/** Parses CLI arguments for the TSSAE A/B performance runner. */
export function parsePerfCliArgs(
  argv: readonly string[],
  defaultProfile: CorpusProfileId = "medium",
): PerfCliOptions {
  let profile: CorpusProfileId = defaultProfile;
  let baselineRev: string | undefined;
  let rounds = 7;
  let warmup = 1;
  let scenarios: readonly PerfScenarioId[] = DEFAULT_SCENARIOS;
  let includeObserveOverhead = true;
  let refreshCorpus = false;
  let noCorpusCache = false;
  let refreshBaseline = false;
  let jsonOut: string | undefined;
  let markdownOut: string | undefined;
  let maxLatencyRegressionRatio: number | undefined;
  let maxMemoryGrowthRatio: number | undefined;
  let help = false;

  const consumeValue = (
    index: number,
    flag: string,
    inlineValue?: string,
  ): string => {
    if (inlineValue !== undefined) {
      if (inlineValue.length === 0) {
        throw new Error(`Missing value for ${flag}`);
      }
      return inlineValue;
    }
    const next = argv[index + 1];
    if (next === undefined || next.startsWith("--")) {
      throw new Error(`Missing value for ${flag}`);
    }
    return next;
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i]!;
    if (arg === "--") {
      continue;
    }
    if (arg === "--help" || arg === "-h") {
      help = true;
      continue;
    }
    if (arg === "--refresh-corpus") {
      refreshCorpus = true;
      continue;
    }
    if (arg === "--no-corpus-cache") {
      noCorpusCache = true;
      continue;
    }
    if (arg === "--refresh-baseline") {
      refreshBaseline = true;
      continue;
    }
    if (arg === "--skip-observe-overhead") {
      includeObserveOverhead = false;
      continue;
    }

    const eqIdx = arg.indexOf("=");
    const flag = eqIdx === -1 ? arg : arg.slice(0, eqIdx);
    const inlineValue = eqIdx === -1 ? undefined : arg.slice(eqIdx + 1);
    const step = inlineValue === undefined ? 1 : 0;

    switch (flag) {
      case "--profile": {
        const val = consumeValue(i, flag, inlineValue);
        i += step;
        if (val !== "medium" && val !== "large") {
          throw new Error(
            `Unknown profile "${val}": expected "medium" or "large".`,
          );
        }
        profile = val;
        break;
      }
      case "--baseline": {
        baselineRev = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--rounds": {
        rounds = parsePositiveInt(consumeValue(i, flag, inlineValue), flag);
        i += step;
        break;
      }
      case "--warmup": {
        warmup = parseNonNegativeInt(consumeValue(i, flag, inlineValue), flag);
        i += step;
        break;
      }
      case "--scenarios": {
        const raw = consumeValue(i, flag, inlineValue);
        i += step;
        const parsed = raw
          .split(",")
          .map((item) => item.trim())
          .filter((item) => item.length > 0);
        if (parsed.length === 0) {
          throw new Error("Expected at least one scenario in --scenarios.");
        }
        for (const item of parsed) {
          if (!(DEFAULT_SCENARIOS as readonly string[]).includes(item)) {
            throw new Error(
              `Unknown scenario "${item}". Supported: ${DEFAULT_SCENARIOS.join(", ")}.`,
            );
          }
        }
        scenarios = [...new Set(parsed)] as PerfScenarioId[];
        break;
      }
      case "--max-latency-regression": {
        maxLatencyRegressionRatio = parseNonNegativeRatio(
          consumeValue(i, flag, inlineValue),
          flag,
        );
        i += step;
        break;
      }
      case "--max-memory-growth": {
        maxMemoryGrowthRatio = parseNonNegativeRatio(
          consumeValue(i, flag, inlineValue),
          flag,
        );
        i += step;
        break;
      }
      case "--json-out": {
        jsonOut = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--markdown-out": {
        markdownOut = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      default: {
        throw new Error(
          arg.startsWith("-")
            ? `Unknown option "${arg}". Run with --help to inspect valid flags.`
            : `Unexpected positional argument "${arg}". Run with --help to inspect valid flags.`,
        );
      }
    }
  }

  return {
    profile,
    ...(baselineRev === undefined ? {} : { baselineRev }),
    rounds,
    warmup,
    scenarios,
    includeObserveOverhead,
    refreshCorpus,
    noCorpusCache,
    refreshBaseline,
    ...(jsonOut === undefined ? {} : { jsonOut }),
    ...(markdownOut === undefined ? {} : { markdownOut }),
    ...(maxLatencyRegressionRatio === undefined
      ? {}
      : { maxLatencyRegressionRatio }),
    ...(maxMemoryGrowthRatio === undefined ? {} : { maxMemoryGrowthRatio }),
    help,
  };
}
