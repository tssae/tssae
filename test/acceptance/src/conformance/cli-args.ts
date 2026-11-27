import { CONFORMANCE_CORPORA, DEFAULT_CONFORMANCE_CORPORA } from "./corpora.ts";
import {
  DEFAULT_CONFORMANCE_BATCH_SIZE,
  DEFAULT_CONFORMANCE_BATCH_TIMEOUT_MS,
  DEFAULT_CONFORMANCE_CONCURRENCY,
} from "./runner.ts";
import type { ConformanceCorpusId } from "./types.ts";

export type ConformanceCliOptions = {
  readonly corpora: readonly ConformanceCorpusId[];
  readonly refreshCorpus: boolean;
  readonly noCorpusCache: boolean;
  readonly batchSize: number;
  readonly concurrency: number;
  readonly batchTimeoutMs: number;
  readonly jsonOut?: string;
  readonly markdownOut?: string;
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

/** Formats CLI usage instructions for the static conformance corpus sweep runner. */
export function formatConformanceCliUsage(): string {
  return [
    "Usage: pnpm conformance -- [options]",
    "",
    "Options:",
    `  --corpora <list>            Comma-separated corpora (${DEFAULT_CONFORMANCE_CORPORA.join(",")}; default: all)`,
    "  --refresh-corpus            Rebuild cached corpus checkouts from source",
    "  --no-corpus-cache           Bypass corpus cache and fetch/extract fresh",
    `  --batch-size <n>            Files per openProject batch (default: ${String(DEFAULT_CONFORMANCE_BATCH_SIZE)})`,
    `  --concurrency <n>           Concurrent worker subprocesses (default: ${String(DEFAULT_CONFORMANCE_CONCURRENCY)})`,
    `  --batch-timeout-ms <ms>     Per-batch watchdog timeout in ms (default: ${String(DEFAULT_CONFORMANCE_BATCH_TIMEOUT_MS)})`,
    "  --json-out <path>           Write full structured JSON report",
    "  --markdown-out <path>       Write Markdown summary report",
    "  -h, --help                  Show help",
  ].join("\n");
}

/** Parses CLI arguments for the conformance corpus sweep runner. */
export function parseConformanceCliArgs(
  argv: readonly string[],
): ConformanceCliOptions {
  let corpora: readonly ConformanceCorpusId[] = DEFAULT_CONFORMANCE_CORPORA;
  let refreshCorpus = false;
  let noCorpusCache = false;
  let batchSize = DEFAULT_CONFORMANCE_BATCH_SIZE;
  let concurrency = DEFAULT_CONFORMANCE_CONCURRENCY;
  let batchTimeoutMs = DEFAULT_CONFORMANCE_BATCH_TIMEOUT_MS;
  let jsonOut: string | undefined;
  let markdownOut: string | undefined;
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

    const eqIdx = arg.indexOf("=");
    const flag = eqIdx === -1 ? arg : arg.slice(0, eqIdx);
    const inlineValue = eqIdx === -1 ? undefined : arg.slice(eqIdx + 1);
    const step = inlineValue === undefined ? 1 : 0;

    switch (flag) {
      case "--corpora":
      case "--corpus": {
        const raw = consumeValue(i, flag, inlineValue);
        i += step;
        const items = raw
          .split(",")
          .map((item) => item.trim())
          .filter((item) => item.length > 0);
        if (items.length === 0) {
          throw new Error(`Expected at least one corpus name in ${flag}.`);
        }
        for (const item of items) {
          if (!Object.hasOwn(CONFORMANCE_CORPORA, item)) {
            throw new Error(
              `Unknown corpus "${item}". Supported: ${DEFAULT_CONFORMANCE_CORPORA.join(", ")}.`,
            );
          }
        }
        corpora = [...new Set(items)] as ConformanceCorpusId[];
        break;
      }
      case "--batch-size": {
        batchSize = parsePositiveInt(consumeValue(i, flag, inlineValue), flag);
        i += step;
        break;
      }
      case "--concurrency": {
        concurrency = parsePositiveInt(
          consumeValue(i, flag, inlineValue),
          flag,
        );
        i += step;
        break;
      }
      case "--batch-timeout-ms": {
        batchTimeoutMs = parsePositiveInt(
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
    corpora,
    refreshCorpus,
    noCorpusCache,
    batchSize,
    concurrency,
    batchTimeoutMs,
    ...(jsonOut === undefined ? {} : { jsonOut }),
    ...(markdownOut === undefined ? {} : { markdownOut }),
    help,
  };
}
