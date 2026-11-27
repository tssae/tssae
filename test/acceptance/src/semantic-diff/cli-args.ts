import type { CorpusProfileId } from "../perf/types.ts";

export type SemanticDiffCliOptions = {
  readonly profile: CorpusProfileId;
  readonly projectDir?: string;
  readonly tsconfig?: string;
  readonly baselineRev?: string;
  readonly baselineCommit?: string;
  readonly waiversPath?: string;
  readonly repoRoot?: string;
  readonly baselineEntryPath?: string;
  readonly candidateEntryPath?: string;
  readonly jsonOut?: string;
  readonly forceDualRun: boolean;
  readonly refreshCorpus: boolean;
  readonly noCorpusCache: boolean;
  readonly refreshBaseline: boolean;
  readonly help: boolean;
};

/** Formats CLI usage instructions for the TSSAE A/B semantic diff gate. */
export function formatSemanticDiffCliUsage(): string {
  return [
    "Usage: pnpm semantic-diff -- [options]",
    "",
    "Options:",
    '  --profile <medium|large>    VS Code corpus profile (default: "medium")',
    "  --project-dir <path>        Analyze a local project directory instead of cached vscode corpus",
    "  --tsconfig <path>           Tsconfig file relative to project root",
    "  --baseline <rev>            Git revision to compare against (default: merge-base HEAD main / origin/main)",
    "  --baseline-commit <sha>     Explicit 40-char baseline SHA (with custom entry overrides)",
    "  --waivers <path>            Path to declarative waivers JSON manifest",
    "  --repo-root <path>          Override Git repository root",
    "  --baseline-entry <path>     Override baseline tssae entry module path",
    "  --candidate-entry <path>    Override candidate tssae entry module path",
    "  --force-dual-run            Force full A/B dual-side run even when HEAD matches clean baseline",
    "  --refresh-corpus            Rebuild cached corpus checkout from source",
    "  --no-corpus-cache           Bypass corpus cache and fetch/extract fresh",
    "  --refresh-baseline          Rebuild cached merge-base tssae tarball",
    "  --json-out <path>           Write full structured JSON report",
    "  -h, --help                  Show help",
  ].join("\n");
}

/** Parses CLI arguments for the TSSAE A/B semantic diff gate. */
export function parseSemanticDiffCliArgs(
  argv: readonly string[],
): SemanticDiffCliOptions {
  let profile: CorpusProfileId = "medium";
  let projectDir: string | undefined;
  let tsconfig: string | undefined;
  let baselineRev: string | undefined;
  let baselineCommit: string | undefined;
  let waiversPath: string | undefined;
  let repoRoot: string | undefined;
  let baselineEntryPath: string | undefined;
  let candidateEntryPath: string | undefined;
  let jsonOut: string | undefined;
  let forceDualRun = false;
  let refreshCorpus = false;
  let noCorpusCache = false;
  let refreshBaseline = false;
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
    if (arg === "--force-dual-run") {
      forceDualRun = true;
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
      case "--project-dir": {
        projectDir = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--tsconfig": {
        tsconfig = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--baseline": {
        baselineRev = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--baseline-commit": {
        baselineCommit = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--waivers": {
        waiversPath = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--repo-root": {
        repoRoot = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--baseline-entry": {
        baselineEntryPath = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--candidate-entry": {
        candidateEntryPath = consumeValue(i, flag, inlineValue);
        i += step;
        break;
      }
      case "--json-out": {
        jsonOut = consumeValue(i, flag, inlineValue);
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
    ...(projectDir === undefined ? {} : { projectDir }),
    ...(tsconfig === undefined ? {} : { tsconfig }),
    ...(baselineRev === undefined ? {} : { baselineRev }),
    ...(baselineCommit === undefined ? {} : { baselineCommit }),
    ...(waiversPath === undefined ? {} : { waiversPath }),
    ...(repoRoot === undefined ? {} : { repoRoot }),
    ...(baselineEntryPath === undefined ? {} : { baselineEntryPath }),
    ...(candidateEntryPath === undefined ? {} : { candidateEntryPath }),
    ...(jsonOut === undefined ? {} : { jsonOut }),
    forceDualRun,
    refreshCorpus,
    noCorpusCache,
    refreshBaseline,
    help,
  };
}
