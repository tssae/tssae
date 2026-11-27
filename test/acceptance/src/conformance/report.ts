import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

import type { ConformanceFailureRecord, ConformanceReport } from "./types.ts";

function formatFailureLocation(failure: ConformanceFailureRecord): string {
  if (failure.at !== undefined) {
    return `${failure.at.file}:${String(failure.at.line)}:${String(failure.at.column)}`;
  }
  if (failure.file !== undefined) {
    return failure.file;
  }
  if (failure.files.length > 0) {
    return failure.files.join(", ");
  }
  return "<batch>";
}

/** Formats a concise console summary table and failure breakdown for a conformance sweep. */
export function formatConformanceConsoleReport(
  report: ConformanceReport,
): string {
  const lines: string[] = [
    `TSSAE Conformance Corpus Sweep | Status: ${report.passed ? "PASS" : "FAIL"} | Files: ${String(report.totalFiles)} | Scopes: ${String(report.totalScopes)} | Queries: ${String(report.totalQueries)} | Duration: ${(report.totalDurationMs / 1000).toFixed(1)}s`,
    "",
  ];

  for (const corpus of report.corpora) {
    lines.push(
      `- ${corpus.id} (${corpus.repoName}@${corpus.commitSha.slice(0, 12)}, origin=${corpus.sourceOrigin}, cacheHit=${String(corpus.cacheHit)}): ${corpus.passed ? "PASS" : "FAIL"} | files=${String(corpus.scannedFiles)}/${String(corpus.matchedFiles)} | scopes=${String(corpus.scopesChecked)} | queries=${String(corpus.queriesExecuted)} | batches=${String(corpus.batchesTotal)} | ${(corpus.durationMs / 1000).toFixed(1)}s`,
    );
  }

  if (report.failures.length > 0) {
    lines.push("", `Failures (${String(report.failures.length)}):`);
    for (const failure of report.failures.slice(0, 50)) {
      const queryPart =
        failure.query !== undefined ? ` [${failure.query}]` : "";
      lines.push(
        `- [${failure.corpusId}] (${failure.kind})${queryPart} ${formatFailureLocation(failure)}: ${failure.message}`,
      );
    }
    if (report.failures.length > 50) {
      lines.push(
        `- ... and ${String(report.failures.length - 50)} more failure(s).`,
      );
    }
  }

  return lines.join("\n");
}

/** Formats a Markdown report for a conformance corpus sweep. */
export function formatConformanceMarkdownReport(
  report: ConformanceReport,
): string {
  const lines: string[] = [
    "# TSSAE Conformance Corpus Sweep Report",
    "",
    `- **Status**: \`${report.passed ? "PASS" : "FAIL"}\``,
    `- **Total Files Scanned**: \`${String(report.totalFiles)}\``,
    `- **Total Scopes Checked**: \`${String(report.totalScopes)}\``,
    `- **Total Queries Executed**: \`${String(report.totalQueries)}\``,
    `- **Total Duration**: \`${(report.totalDurationMs / 1000).toFixed(1)} s\``,
    `- **Batch Config**: \`batchSize=${String(report.batchSize)}\`, \`concurrency=${String(report.concurrency)}\`, \`batchTimeoutMs=${String(report.batchTimeoutMs)}\``,
    "",
    "## Corpora",
    "",
    "| Corpus | Commit | Origin | Matched | Scanned | Scopes | Queries | Duration (s) | Result |",
    "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- |",
    ...report.corpora.map(
      (corpus) =>
        `| \`${corpus.id}\` | \`${corpus.commitSha.slice(0, 12)}\` | \`${corpus.sourceOrigin}\` | ${String(corpus.matchedFiles)} | ${String(corpus.scannedFiles)} | ${String(corpus.scopesChecked)} | ${String(corpus.queriesExecuted)} | ${(corpus.durationMs / 1000).toFixed(1)} | ${corpus.passed ? "PASS" : "FAIL"} |`,
    ),
  ];

  if (report.failures.length > 0) {
    lines.push("", `## Failures (${String(report.failures.length)})`, "");
    for (const failure of report.failures) {
      const queryPart =
        failure.query !== undefined ? ` \`${failure.query}\`` : "";
      lines.push(
        `- \`${failure.corpusId}\` **${failure.kind}**${queryPart} at \`${formatFailureLocation(failure)}\`: ${failure.message}`,
      );
    }
  }

  lines.push("");
  return lines.join("\n");
}

/** Writes optional JSON and Markdown conformance reports relative to `options.cwd`. */
export function writeConformanceReportFiles(
  report: ConformanceReport,
  options: {
    readonly cwd: string;
    readonly jsonOut?: string;
    readonly markdownOut?: string;
  },
): void {
  if (options.jsonOut !== undefined) {
    const outPath = resolve(options.cwd, options.jsonOut);
    mkdirSync(dirname(outPath), { recursive: true });
    writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`, "utf8");
  }
  if (options.markdownOut !== undefined) {
    const outPath = resolve(options.cwd, options.markdownOut);
    mkdirSync(dirname(outPath), { recursive: true });
    writeFileSync(outPath, formatConformanceMarkdownReport(report), "utf8");
  }
}
