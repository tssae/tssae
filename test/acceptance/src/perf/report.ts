import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

import type { PerfReport, PerfScenarioId } from "./types.ts";

function formatMs(value: number): string {
  return value.toFixed(1);
}

function formatMb(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function formatDeltaPct(ratio: number): string {
  if (!Number.isFinite(ratio)) {
    return "+inf%";
  }
  const pct = ratio * 100;
  return `${pct >= 0 ? "+" : ""}${pct.toFixed(2)}%`;
}

function padRight(text: string, width: number): string {
  return text.length >= width
    ? text
    : `${text}${" ".repeat(width - text.length)}`;
}

function padLeft(text: string, width: number): string {
  return text.length >= width
    ? text
    : `${" ".repeat(width - text.length)}${text}`;
}

function renderAlignedTable(
  headers: readonly string[],
  rows: readonly (readonly string[])[],
  numericColumns: ReadonlySet<number>,
): readonly string[] {
  const widths = headers.map((header, col) =>
    Math.max(header.length, ...rows.map((row) => row[col]!.length)),
  );
  const formatRow = (cells: readonly string[]): string =>
    cells
      .map((cell, col) =>
        numericColumns.has(col)
          ? padLeft(cell, widths[col]!)
          : padRight(cell, widths[col]!),
      )
      .join(" | ");
  const separator = widths.map((width) => "-".repeat(width)).join("-+-");
  return [formatRow(headers), separator, ...rows.map(formatRow)];
}

/** Formats the aligned terminal table report, including observe overhead and attribution on failure. */
export function formatPerfTableReport(report: PerfReport): string {
  const { corpus, environment } = report;
  const lines: string[] = [
    `TSSAE Performance A/B Report | Profile: ${report.profile} | Status: ${report.passed ? "PASS" : "FAIL"} | Duration: ${(report.totalDurationMs / 1000).toFixed(1)}s`,
    `Corpus: ${corpus.repoName}@${corpus.commitSha} (${corpus.subsets.join(", ")}; ${String(corpus.fileCount)} files; source=${corpus.sourceOrigin})`,
    `Baseline: ${environment.baselineRev} (${environment.baselineCommitSha.slice(0, 12)}, tssae@${environment.baselineVersion}, cacheHit=${String(environment.baselineCacheHit)})`,
    `Candidate: ${environment.candidateCommitSha.slice(0, 12)}${environment.candidateDirty ? "+dirty" : ""} (tssae@${environment.candidateVersion})`,
    `Budget: query p50 <= ${formatDeltaPct(report.budget.maxQueryLatencyRegressionRatio)}, peak memory <= ${formatDeltaPct(report.budget.maxPeakMemoryGrowthRatio)} | Warmup: ${String(report.warmupRounds)}, Rounds: ${String(report.samplingRounds)}`,
    "",
    ...renderAlignedTable(
      [
        "Scenario",
        "Base p50 (ms)",
        "Cand p50 (ms)",
        "p50 Delta",
        "Base p95 (ms)",
        "Cand p95 (ms)",
        "Base Peak RSS",
        "Cand Peak RSS",
        "Mem Delta",
        "Result",
      ],
      report.scenarios.map((item) => [
        item.label,
        formatMs(item.baseline.p50Ms),
        formatMs(item.candidate.p50Ms),
        formatDeltaPct(item.latencyDeltaRatio),
        formatMs(item.baseline.p95Ms),
        formatMs(item.candidate.p95Ms),
        formatMb(item.baseline.peakEngineRssBytes),
        formatMb(item.candidate.peakEngineRssBytes),
        formatDeltaPct(item.memoryDeltaRatio),
        item.passed ? "PASS" : "FAIL",
      ]),
      new Set([1, 2, 3, 4, 5, 6, 7, 8]),
    ),
  ];

  if (report.observeOverhead.length > 0) {
    lines.push(
      "",
      "Observe Overhead (Candidate):",
      ...renderAlignedTable(
        [
          "Mode",
          "p50 (ms)",
          "p95 (ms)",
          "Overhead (ms)",
          "Overhead %",
          "Peak RSS",
          "Mem Delta",
          "RPC Bytes",
        ],
        report.observeOverhead.map((entry) => [
          entry.mode,
          formatMs(entry.p50Ms),
          formatMs(entry.p95Ms),
          formatMs(entry.latencyOverheadMs),
          formatDeltaPct(entry.latencyOverheadRatio),
          formatMb(entry.peakEngineRssBytes),
          formatMb(entry.memoryOverheadBytes),
          String(entry.rpcBytesTotal),
        ]),
        new Set([1, 2, 3, 4, 5, 6, 7]),
      ),
    );
  }

  const failing = report.scenarios.filter((item) => !item.passed);
  if (failing.length > 0) {
    lines.push("", "Budget Violations & Attribution (observe=summary):");
    for (const item of failing) {
      lines.push(`- ${item.label}: ${item.violations.join("; ")}`);
      if (item.attribution !== undefined) {
        lines.push(
          `  RPC bytes: baseline=${String(item.attribution.baselineRpcBytes)}, candidate=${String(item.attribution.candidateRpcBytes)}`,
        );
        if (item.attribution.steps.length > 0) {
          lines.push(
            "  Engine Steps:",
            ...renderAlignedTable(
              ["Step Op", "Provider", "Base (ms)", "Cand (ms)", "Delta (ms)"],
              item.attribution.steps
                .slice(0, 10)
                .map((step) => [
                  step.op,
                  step.provider,
                  formatMs(step.baselineDurationMs),
                  formatMs(step.candidateDurationMs),
                  `${step.deltaMs >= 0 ? "+" : ""}${formatMs(step.deltaMs)}`,
                ]),
              new Set([2, 3, 4]),
            ).map((row) => `  ${row}`),
          );
        }
        if (item.attribution.queries.length > 0) {
          lines.push(
            "  Public Queries:",
            ...renderAlignedTable(
              ["Query Op", "Base (ms)", "Cand (ms)", "Delta (ms)"],
              item.attribution.queries
                .slice(0, 10)
                .map((query) => [
                  query.op,
                  formatMs(query.baselineDurationMs),
                  formatMs(query.candidateDurationMs),
                  `${query.deltaMs >= 0 ? "+" : ""}${formatMs(query.deltaMs)}`,
                ]),
              new Set([1, 2, 3]),
            ).map((row) => `  ${row}`),
          );
        }
      }
    }
  }

  return lines.join("\n");
}

function renderMdTable(
  headers: readonly string[],
  rows: readonly (readonly string[])[],
  numericColumns: ReadonlySet<number>,
): readonly string[] {
  const headerLine = `| ${headers.join(" | ")} |`;
  const alignLine = `| ${headers
    .map((_, col) => (numericColumns.has(col) ? "---:" : "---"))
    .join(" | ")} |`;
  const bodyLines = rows.map((row) => `| ${row.join(" | ")} |`);
  return [headerLine, alignLine, ...bodyLines];
}

/** Formats a structured Markdown report capturing corpus scope, A/B metrics, observe overhead, and attribution. */
export function formatPerfMarkdownReport(report: PerfReport): string {
  const { corpus, environment } = report;
  const totalGib = (
    environment.totalMemoryBytes /
    (1024 * 1024 * 1024)
  ).toFixed(1);

  const lines: string[] = [
    `# TSSAE Performance A/B Report (${report.profile})`,
    "",
    `- **Status**: \`${report.passed ? "PASS" : "FAIL"}\` (${(report.totalDurationMs / 1000).toFixed(1)} s)`,
    `- **Timestamp (UTC)**: \`${environment.timestampUtc}\``,
    `- **Corpus**: \`${corpus.repoName}\` (\`${corpus.repoUrl}\`) @ \`${corpus.commitSha}\``,
    `- **Corpus Scope**: ${corpus.subsets.map((s) => `\`${s}\``).join(", ")} (${String(corpus.fileCount)} files; source=\`${corpus.sourceOrigin}\`; cacheBypassed=\`${String(corpus.cacheBypassed)}\`)`,
    `- **Baseline**: \`${environment.baselineRev}\` -> \`${environment.baselineCommitSha}\` (\`tssae@${environment.baselineVersion}\`, tgz sha256=\`${environment.baselineTarballSha256.slice(0, 12)}\`, bin sha256=\`${environment.baselineBinarySha256.slice(0, 12)}\`, cacheHit=\`${String(environment.baselineCacheHit)}\`)`,
    `- **Candidate**: \`${environment.candidateCommitSha}\`${environment.candidateDirty ? " (dirty)" : ""} (\`tssae@${environment.candidateVersion}\`, tgz sha256=\`${environment.candidateTarballSha256.slice(0, 12)}\`, bin sha256=\`${environment.candidateBinarySha256.slice(0, 12)}\`)`,
    `- **Environment**: \`${environment.platform}/${environment.arch}\` (${environment.osRelease}), CPU \`${environment.cpuModel}\` x ${String(environment.cpuCores)}, RAM ${totalGib} GiB, Node \`${environment.nodeVersion}\`, pnpm \`${environment.pnpmVersion}\``,
    `- **Sampling & Budget**: ${String(report.warmupRounds)} warmup + ${String(report.samplingRounds)} paired ABBA rounds (independent Node subprocesses, \`observe: "off"\`); gate: query p50 <= ${formatDeltaPct(report.budget.maxQueryLatencyRegressionRatio)}, peak memory <= ${formatDeltaPct(report.budget.maxPeakMemoryGrowthRatio)}`,
    "",
    "## Scenario Scope & A/B Comparison",
    "",
    ...renderMdTable(
      [
        "Scenario",
        "Base p50 (ms)",
        "Cand p50 (ms)",
        "p50 Delta",
        "Base p95 (ms)",
        "Cand p95 (ms)",
        "Base Peak RSS",
        "Cand Peak RSS",
        "Mem Delta",
        "Result",
      ],
      report.scenarios.map((item) => [
        item.label,
        formatMs(item.baseline.p50Ms),
        formatMs(item.candidate.p50Ms),
        formatDeltaPct(item.latencyDeltaRatio),
        formatMs(item.baseline.p95Ms),
        formatMs(item.candidate.p95Ms),
        formatMb(item.baseline.peakEngineRssBytes),
        formatMb(item.candidate.peakEngineRssBytes),
        formatDeltaPct(item.memoryDeltaRatio),
        item.passed ? "PASS" : "FAIL",
      ]),
      new Set([1, 2, 3, 4, 5, 6, 7, 8]),
    ),
  ];

  if (report.observeOverhead.length > 0) {
    lines.push(
      "",
      "## Observe Overhead (`off` / `summary` / `detail`)",
      "",
      ...renderMdTable(
        [
          "Mode",
          "p50 (ms)",
          "p95 (ms)",
          "Overhead (ms)",
          "Overhead %",
          "Peak RSS",
          "Mem Delta",
          "RPC Bytes",
        ],
        report.observeOverhead.map((entry) => [
          `\`${entry.mode}\``,
          formatMs(entry.p50Ms),
          formatMs(entry.p95Ms),
          formatMs(entry.latencyOverheadMs),
          formatDeltaPct(entry.latencyOverheadRatio),
          formatMb(entry.peakEngineRssBytes),
          formatMb(entry.memoryOverheadBytes),
          String(entry.rpcBytesTotal),
        ]),
        new Set([1, 2, 3, 4, 5, 6, 7]),
      ),
    );
  }

  const failing = report.scenarios.filter((item) => !item.passed);
  if (failing.length > 0) {
    lines.push("", "## Budget Failure Attribution (`observe: summary`)");
    for (const item of failing) {
      lines.push("", `### ${item.label}`);
      for (const violation of item.violations) {
        lines.push(`- ${violation}`);
      }
      if (item.attribution !== undefined) {
        lines.push(
          `- RPC Bytes: baseline=\`${String(item.attribution.baselineRpcBytes)}\`, candidate=\`${String(item.attribution.candidateRpcBytes)}\``,
        );
        if (item.attribution.steps.length > 0) {
          lines.push(
            "",
            ...renderMdTable(
              ["Step Op", "Provider", "Base (ms)", "Cand (ms)", "Delta (ms)"],
              item.attribution.steps
                .slice(0, 10)
                .map((step) => [
                  `\`${step.op}\``,
                  `\`${step.provider}\``,
                  formatMs(step.baselineDurationMs),
                  formatMs(step.candidateDurationMs),
                  `${step.deltaMs >= 0 ? "+" : ""}${formatMs(step.deltaMs)}`,
                ]),
              new Set([2, 3, 4]),
            ),
          );
        }
        if (item.attribution.queries.length > 0) {
          lines.push(
            "",
            ...renderMdTable(
              ["Query Op", "Base (ms)", "Cand (ms)", "Delta (ms)"],
              item.attribution.queries
                .slice(0, 10)
                .map((query) => [
                  `\`${query.op}\``,
                  formatMs(query.baselineDurationMs),
                  formatMs(query.candidateDurationMs),
                  `${query.deltaMs >= 0 ? "+" : ""}${formatMs(query.deltaMs)}`,
                ]),
              new Set([1, 2, 3]),
            ),
          );
        }
      }
    }
  }

  lines.push("");
  return lines.join("\n");
}

export function writePerfReportFiles(
  report: PerfReport,
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
    writeFileSync(outPath, formatPerfMarkdownReport(report), "utf8");
  }
}

export const SCENARIO_LABELS: Readonly<Record<PerfScenarioId, string>> = {
  openProject: "Open Project",
  coldQuery: "Cold Query",
  cachedQuery: "Cached Query",
  updateQuery: "Update + Query",
};
