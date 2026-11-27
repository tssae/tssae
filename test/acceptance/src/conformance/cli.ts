import { resolve } from "node:path";

import {
  formatConformanceCliUsage,
  parseConformanceCliArgs,
} from "./cli-args.ts";
import {
  formatConformanceConsoleReport,
  writeConformanceReportFiles,
} from "./report.ts";
import {
  runConformanceSweep,
  type RunConformanceSweepOptions,
} from "./runner.ts";

export type RunConformanceCliHost = {
  readonly cwd?: string;
  readonly stdout?: (chunk: string) => void;
  readonly stderr?: (chunk: string) => void;
  readonly sweepOverrides?: Partial<RunConformanceSweepOptions>;
};

/** Executes the conformance corpus sweep CLI and returns the process exit code. */
export async function runConformanceCli(
  argv: readonly string[],
  host: RunConformanceCliHost = {},
): Promise<number> {
  const stdout =
    host.stdout ?? ((chunk: string) => process.stdout.write(chunk));
  const stderr =
    host.stderr ?? ((chunk: string) => process.stderr.write(chunk));

  let options;
  try {
    options = parseConformanceCliArgs(argv);
  } catch (error) {
    stderr(
      `${error instanceof Error ? error.message : String(error)}\n\n${formatConformanceCliUsage()}\n`,
    );
    return 2;
  }

  if (options.help) {
    stdout(`${formatConformanceCliUsage()}\n`);
    return 0;
  }

  const outputCwd = resolve(
    host.cwd ?? process.env.TSSAE_CONFORMANCE_CWD ?? process.cwd(),
  );
  const report = await runConformanceSweep({
    corpora: options.corpora,
    refreshCorpus: options.refreshCorpus,
    noCorpusCache: options.noCorpusCache,
    batchSize: options.batchSize,
    concurrency: options.concurrency,
    batchTimeoutMs: options.batchTimeoutMs,
    ...host.sweepOverrides,
  });

  stdout(`${formatConformanceConsoleReport(report)}\n`);
  writeConformanceReportFiles(report, {
    cwd: outputCwd,
    ...(options.jsonOut === undefined ? {} : { jsonOut: options.jsonOut }),
    ...(options.markdownOut === undefined
      ? {}
      : { markdownOut: options.markdownOut }),
  });

  return report.passed ? 0 : 1;
}
