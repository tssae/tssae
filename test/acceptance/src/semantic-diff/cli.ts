import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

import {
  formatSemanticDiffCliUsage,
  parseSemanticDiffCliArgs,
} from "./cli-args.ts";
import { formatSemanticDiffReport } from "./diff.ts";
import { runSemanticDiffGate } from "./runner.ts";

export type SemanticDiffCliIo = {
  readonly stdout?: (chunk: string) => void;
  readonly stderr?: (chunk: string) => void;
};

/** Executes the semantic A/B diff CLI and returns exit code `0` (pass), `1` (regression), or `2` (usage error). */
export async function runSemanticDiffCli(
  argv: readonly string[],
  io: SemanticDiffCliIo = {},
): Promise<number> {
  const writeOut =
    io.stdout ?? ((chunk: string) => process.stdout.write(chunk));
  const writeErr =
    io.stderr ?? ((chunk: string) => process.stderr.write(chunk));

  let options;
  try {
    options = parseSemanticDiffCliArgs(argv);
  } catch (error) {
    writeErr(
      `${error instanceof Error ? error.message : String(error)}\n\n${formatSemanticDiffCliUsage()}\n`,
    );
    return 2;
  }

  if (options.help) {
    writeOut(`${formatSemanticDiffCliUsage()}\n`);
    return 0;
  }

  const outputCwd = resolve(
    process.env.TSSAE_SEMANTIC_DIFF_CWD ?? process.cwd(),
  );
  const run = await runSemanticDiffGate({
    profile: options.profile,
    ...(options.projectDir === undefined
      ? {}
      : { projectDir: options.projectDir }),
    ...(options.tsconfig === undefined ? {} : { tsconfig: options.tsconfig }),
    ...(options.baselineRev === undefined
      ? {}
      : { baselineRev: options.baselineRev }),
    ...(options.baselineCommit === undefined
      ? {}
      : { baselineCommit: options.baselineCommit }),
    ...(options.waiversPath === undefined
      ? {}
      : { waiversPath: options.waiversPath }),
    ...(options.repoRoot === undefined ? {} : { repoRoot: options.repoRoot }),
    ...(options.baselineEntryPath === undefined
      ? {}
      : { baselineEntryPath: options.baselineEntryPath }),
    ...(options.candidateEntryPath === undefined
      ? {}
      : { candidateEntryPath: options.candidateEntryPath }),
    forceDualRun: options.forceDualRun,
    refreshCorpus: options.refreshCorpus,
    noCorpusCache: options.noCorpusCache,
    refreshBaseline: options.refreshBaseline,
    logProgress: (msg: string) => writeErr(`${msg}\n`),
  });

  const summaryHeader = [
    formatSemanticDiffReport(run.diff),
    `Timing: baseline=${(run.baselineDurationMs / 1000).toFixed(2)}s, candidate=${(run.candidateDurationMs / 1000).toFixed(2)}s, total=${(run.totalDurationMs / 1000).toFixed(2)}s (fastPath=${String(run.fastPath)})`,
  ].join("\n");
  writeOut(`${summaryHeader}\n`);

  if (options.jsonOut !== undefined) {
    const outPath = resolve(outputCwd, options.jsonOut);
    mkdirSync(dirname(outPath), { recursive: true });
    writeFileSync(outPath, `${JSON.stringify(run, null, 2)}\n`, "utf8");
  }

  return run.diff.passed ? 0 : 1;
}
