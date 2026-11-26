import { resolve } from "node:path";

import { formatPerfCliUsage, parsePerfCliArgs } from "./cli-args.ts";
import { formatPerfTableReport, writePerfReportFiles } from "./report.ts";
import { runPerfAbBenchmark } from "./runner.ts";
import { DEFAULT_PERF_BUDGET } from "./stats.ts";
import type { CorpusProfileId } from "./types.ts";

/** Executes the shared A/B performance CLI with the given default corpus profile. */
export async function runPerfCli(
  argv: readonly string[],
  defaultProfile: CorpusProfileId = "medium",
): Promise<number> {
  let options;
  try {
    options = parsePerfCliArgs(argv, defaultProfile);
  } catch (error) {
    process.stderr.write(
      `${error instanceof Error ? error.message : String(error)}\n\n${formatPerfCliUsage()}\n`,
    );
    return 2;
  }

  if (options.help) {
    process.stdout.write(`${formatPerfCliUsage()}\n`);
    return 0;
  }

  const outputCwd = resolve(process.env.TSSAE_PERF_CWD ?? process.cwd());
  const report = await runPerfAbBenchmark({
    profile: options.profile,
    ...(options.baselineRev === undefined
      ? {}
      : { baselineRev: options.baselineRev }),
    rounds: options.rounds,
    warmup: options.warmup,
    scenarios: options.scenarios,
    includeObserveOverhead: options.includeObserveOverhead,
    refreshCorpus: options.refreshCorpus,
    noCorpusCache: options.noCorpusCache,
    refreshBaseline: options.refreshBaseline,
    budget: {
      maxQueryLatencyRegressionRatio:
        options.maxLatencyRegressionRatio ??
        DEFAULT_PERF_BUDGET.maxQueryLatencyRegressionRatio,
      maxPeakMemoryGrowthRatio:
        options.maxMemoryGrowthRatio ??
        DEFAULT_PERF_BUDGET.maxPeakMemoryGrowthRatio,
    },
  });

  process.stdout.write(`${formatPerfTableReport(report)}\n`);
  writePerfReportFiles(report, {
    cwd: outputCwd,
    ...(options.jsonOut === undefined ? {} : { jsonOut: options.jsonOut }),
    ...(options.markdownOut === undefined
      ? {}
      : { markdownOut: options.markdownOut }),
  });

  return report.passed ? 0 : 1;
}
