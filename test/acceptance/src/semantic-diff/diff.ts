import type {
  DigestSample,
  QueryDigest,
  RegressionFinding,
  SemanticDiffResult,
  SemanticDiffWaiver,
  SemanticOutcomeChannel,
  SemanticOutcomeStatus,
  SemanticQueryName,
  StatusTransitionCount,
  UnknownCountDelta,
} from "./types.ts";

export type DiffDigestsOptions = {
  readonly baselineCommit: string;
  readonly waivers?: readonly SemanticDiffWaiver[];
};

type SampleBucketKey = `${SemanticQueryName}\0${SemanticOutcomeChannel}`;

function compareStrings(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function sampleKey(sample: DigestSample): string {
  return `${sample.query}\0${sample.channel}\0${sample.file}\0${sample.targetId}`;
}

function bucketKey(
  query: SemanticQueryName,
  channel: SemanticOutcomeChannel,
): SampleBucketKey {
  return `${query}\0${channel}`;
}

function unknownStatusForQuery(
  query: SemanticQueryName,
): "inconclusive" | "incomplete" {
  if (
    query === "calleesOf" ||
    query === "callersOf" ||
    query === "callEffects"
  ) {
    return "incomplete";
  }
  return "inconclusive";
}

function isDeterministicInversion(
  fromStatus: SemanticOutcomeStatus,
  toStatus: SemanticOutcomeStatus,
): boolean {
  return (
    (fromStatus === "proved" && toStatus === "disproved") ||
    (fromStatus === "disproved" && toStatus === "proved") ||
    (fromStatus === "resolved" && toStatus === "external") ||
    (fromStatus === "external" && toStatus === "resolved")
  );
}

function isUnknownStatus(status: SemanticOutcomeStatus): boolean {
  return status === "inconclusive" || status === "incomplete";
}

function findMatchingWaiver(
  activeWaivers: readonly SemanticDiffWaiver[],
  sample: DigestSample,
  fromStatus: SemanticOutcomeStatus,
  toStatus: SemanticOutcomeStatus,
): SemanticDiffWaiver | undefined {
  return activeWaivers.find(
    (w) =>
      w.query === sample.query &&
      (w.channel ?? "primary") === sample.channel &&
      w.file === sample.file &&
      w.target === sample.targetId &&
      w.fromStatus === fromStatus &&
      w.toStatus === toStatus,
  );
}

/**
 * Purely compares baseline and candidate query digests, applying only waivers
 * whose `baselineCommit` matches the resolved baseline commit SHA.
 */
export function diffDigests(
  baseline: QueryDigest,
  candidate: QueryDigest,
  options: DiffDigestsOptions,
): SemanticDiffResult {
  const normalizedBaselineCommit = options.baselineCommit.trim().toLowerCase();
  const allWaivers = options.waivers ?? [];
  const activeWaivers = allWaivers.filter(
    (w) => w.baselineCommit.trim().toLowerCase() === normalizedBaselineCommit,
  );
  const ignoredWaiverCount = allWaivers.length - activeWaivers.length;

  const baselineMap = new Map<string, DigestSample>();
  for (const sample of baseline.samples) {
    baselineMap.set(sampleKey(sample), sample);
  }

  const candidateMap = new Map<string, DigestSample>();
  for (const sample of candidate.samples) {
    candidateMap.set(sampleKey(sample), sample);
  }

  const transitionMap = new Map<string, StatusTransitionCount>();
  const appliedWaivers: SemanticDiffWaiver[] = [];
  const appliedWaiverSet = new Set<SemanticDiffWaiver>();
  const inversions: RegressionFinding[] = [];
  const waivedIntoUnknownByBucket = new Map<SampleBucketKey, number>();
  const unwaivedIntoUnknownByBucket = new Map<
    SampleBucketKey,
    RegressionFinding[]
  >();
  const buckets = new Map<
    SampleBucketKey,
    {
      readonly query: SemanticQueryName;
      readonly channel: SemanticOutcomeChannel;
    }
  >();

  const registerBucket = (
    query: SemanticQueryName,
    channel: SemanticOutcomeChannel,
  ): SampleBucketKey => {
    const key = bucketKey(query, channel);
    if (!buckets.has(key)) {
      buckets.set(key, { query, channel });
    }
    return key;
  };

  let matchedSampleCount = 0;

  for (const [key, cand] of candidateMap.entries()) {
    const bKey = registerBucket(cand.query, cand.channel);
    const base = baselineMap.get(key);
    if (base === undefined) {
      if (isUnknownStatus(cand.status)) {
        const list = unwaivedIntoUnknownByBucket.get(bKey) ?? [];
        list.push({
          kind: "unknown-net-increase",
          query: cand.query,
          channel: cand.channel,
          file: cand.file,
          line: cand.location.line,
          column: cand.location.column,
          endLine: cand.location.endLine,
          endColumn: cand.location.endColumn,
          target: cand.targetId,
          fromStatus: "absent",
          toStatus: cand.status,
          message: `New ${cand.status} sample in candidate (${cand.query}/${cand.channel})`,
        });
        unwaivedIntoUnknownByBucket.set(bKey, list);
      }
      continue;
    }

    matchedSampleCount += 1;
    const trKey = `${cand.query}\0${cand.channel}\0${base.status}\0${cand.status}`;
    const existingTr = transitionMap.get(trKey);
    if (existingTr !== undefined) {
      transitionMap.set(trKey, {
        ...existingTr,
        count: existingTr.count + 1,
      });
    } else {
      transitionMap.set(trKey, {
        query: cand.query,
        channel: cand.channel,
        fromStatus: base.status,
        toStatus: cand.status,
        count: 1,
      });
    }

    if (base.status === cand.status) {
      continue;
    }

    const waiver = findMatchingWaiver(
      activeWaivers,
      cand,
      base.status,
      cand.status,
    );
    if (waiver !== undefined && !appliedWaiverSet.has(waiver)) {
      appliedWaiverSet.add(waiver);
      appliedWaivers.push(waiver);
    }

    if (isDeterministicInversion(base.status, cand.status)) {
      if (waiver === undefined) {
        inversions.push({
          kind: "deterministic-inversion",
          query: cand.query,
          channel: cand.channel,
          file: cand.file,
          line: cand.location.line,
          column: cand.location.column,
          endLine: cand.location.endLine,
          endColumn: cand.location.endColumn,
          target: cand.targetId,
          fromStatus: base.status,
          toStatus: cand.status,
          message: `Deterministic conclusion inverted from ${base.status} to ${cand.status}`,
        });
      }
    }

    if (isUnknownStatus(cand.status)) {
      if (waiver !== undefined) {
        waivedIntoUnknownByBucket.set(
          bKey,
          (waivedIntoUnknownByBucket.get(bKey) ?? 0) + 1,
        );
      } else {
        const list = unwaivedIntoUnknownByBucket.get(bKey) ?? [];
        list.push({
          kind: "unknown-net-increase",
          query: cand.query,
          channel: cand.channel,
          file: cand.file,
          line: cand.location.line,
          column: cand.location.column,
          endLine: cand.location.endLine,
          endColumn: cand.location.endColumn,
          target: cand.targetId,
          fromStatus: base.status,
          toStatus: cand.status,
          message: `Precision regressed from ${base.status} to ${cand.status}`,
        });
        unwaivedIntoUnknownByBucket.set(bKey, list);
      }
    }
  }

  for (const base of baselineMap.values()) {
    registerBucket(base.query, base.channel);
  }

  const baselineUnknownCounts = new Map<SampleBucketKey, number>();
  for (const sample of baseline.samples) {
    const unknownStatus = unknownStatusForQuery(sample.query);
    if (sample.status === unknownStatus) {
      const bKey = bucketKey(sample.query, sample.channel);
      baselineUnknownCounts.set(
        bKey,
        (baselineUnknownCounts.get(bKey) ?? 0) + 1,
      );
    }
  }

  const candidateUnknownCounts = new Map<SampleBucketKey, number>();
  for (const sample of candidate.samples) {
    const unknownStatus = unknownStatusForQuery(sample.query);
    if (sample.status === unknownStatus) {
      const bKey = bucketKey(sample.query, sample.channel);
      candidateUnknownCounts.set(
        bKey,
        (candidateUnknownCounts.get(bKey) ?? 0) + 1,
      );
    }
  }

  const unknownDeltas: UnknownCountDelta[] = [];
  const unknownIncreases: RegressionFinding[] = [];

  const sortedBuckets = [...buckets.entries()].sort(([a], [b]) =>
    compareStrings(a, b),
  );
  for (const [bKey, meta] of sortedBuckets) {
    const unknownStatus = unknownStatusForQuery(meta.query);
    const baselineCount = baselineUnknownCounts.get(bKey) ?? 0;
    const candidateCount = candidateUnknownCounts.get(bKey) ?? 0;
    const waivedIntoUnknownCount = waivedIntoUnknownByBucket.get(bKey) ?? 0;
    const unwaivedCandidateCount = candidateCount - waivedIntoUnknownCount;
    const netDelta = unwaivedCandidateCount - baselineCount;

    unknownDeltas.push({
      query: meta.query,
      channel: meta.channel,
      unknownStatus,
      baselineCount,
      candidateCount,
      waivedIntoUnknownCount,
      unwaivedCandidateCount,
      netDelta,
    });

    if (netDelta > 0) {
      const findings = unwaivedIntoUnknownByBucket.get(bKey) ?? [];
      if (findings.length > 0) {
        unknownIncreases.push(...findings);
      } else {
        unknownIncreases.push({
          kind: "unknown-net-increase",
          query: meta.query,
          channel: meta.channel,
          file: "<aggregate>",
          line: 1,
          column: 1,
          endLine: 1,
          endColumn: 1,
          target: `${meta.query}:${meta.channel}`,
          fromStatus: "absent",
          toStatus: unknownStatus,
          message: `${meta.query} (${meta.channel}) ${unknownStatus} count increased by ${String(netDelta)}`,
        });
      }
    }
  }

  const transitions = [...transitionMap.values()].sort((a, b) => {
    const q = compareStrings(a.query, b.query);
    if (q !== 0) return q;
    const c = compareStrings(a.channel, b.channel);
    if (c !== 0) return c;
    const f = compareStrings(a.fromStatus, b.fromStatus);
    return f !== 0 ? f : compareStrings(a.toStatus, b.toStatus);
  });

  inversions.sort((a, b) => {
    const f = compareStrings(a.file, b.file);
    if (f !== 0) return f;
    if (a.line !== b.line) return a.line - b.line;
    return compareStrings(a.target, b.target);
  });

  unknownIncreases.sort((a, b) => {
    const f = compareStrings(a.file, b.file);
    if (f !== 0) return f;
    if (a.line !== b.line) return a.line - b.line;
    return compareStrings(a.target, b.target);
  });

  return {
    passed: inversions.length === 0 && unknownIncreases.length === 0,
    baselineCommit: options.baselineCommit,
    baselineSampleCount: baseline.sampleCount,
    candidateSampleCount: candidate.sampleCount,
    matchedSampleCount,
    transitions,
    unknownDeltas,
    inversions,
    unknownIncreases,
    appliedWaivers,
    ignoredWaiverCount,
  };
}

function renderAlignedTable(
  headers: readonly string[],
  rows: readonly (readonly string[])[],
  numericCols: ReadonlySet<number>,
): readonly string[] {
  const widths = headers.map((h, i) =>
    Math.max(h.length, ...rows.map((r) => r[i]!.length)),
  );
  const formatRow = (cells: readonly string[]): string =>
    cells
      .map((cell, i) =>
        numericCols.has(i)
          ? cell.padStart(widths[i]!)
          : cell.padEnd(widths[i]!),
      )
      .join(" | ");
  const separator = widths.map((w) => "-".repeat(w)).join("-+-");
  return [formatRow(headers), separator, ...rows.map(formatRow)];
}

/** Formats a human-readable semantic diff gate report with transition counts and degraded sample locations. */
export function formatSemanticDiffReport(result: SemanticDiffResult): string {
  const lines: string[] = [
    `TSSAE Semantic A/B Diff Report | Status: ${result.passed ? "PASS" : "FAIL"} | Baseline: ${result.baselineCommit.slice(0, 12)}`,
    `Samples: baseline=${String(result.baselineSampleCount)}, candidate=${String(result.candidateSampleCount)}, matched=${String(result.matchedSampleCount)}, appliedWaivers=${String(result.appliedWaivers.length)}, ignoredWaivers=${String(result.ignoredWaiverCount)}`,
  ];

  if (result.unknownDeltas.length > 0) {
    lines.push(
      "",
      "Unknown Status Deltas:",
      ...renderAlignedTable(
        [
          "Query",
          "Channel",
          "Unknown Status",
          "Base",
          "Cand",
          "Waived",
          "Net Delta",
        ],
        result.unknownDeltas.map((d) => [
          d.query,
          d.channel,
          d.unknownStatus,
          String(d.baselineCount),
          String(d.candidateCount),
          String(d.waivedIntoUnknownCount),
          `${d.netDelta > 0 ? "+" : ""}${String(d.netDelta)}`,
        ]),
        new Set([3, 4, 5, 6]),
      ),
    );
  }

  const changedTransitions = result.transitions.filter(
    (t) => t.fromStatus !== t.toStatus,
  );
  if (changedTransitions.length > 0) {
    lines.push(
      "",
      "Status Transitions (Changed):",
      ...renderAlignedTable(
        ["Query", "Channel", "From", "To", "Count"],
        changedTransitions.map((t) => [
          t.query,
          t.channel,
          t.fromStatus,
          t.toStatus,
          String(t.count),
        ]),
        new Set([4]),
      ),
    );
  } else if (result.transitions.length > 0) {
    lines.push(
      "",
      "Status Transitions (Unchanged Summary):",
      ...renderAlignedTable(
        ["Query", "Channel", "Status", "Count"],
        result.transitions.map((t) => [
          t.query,
          t.channel,
          t.toStatus,
          String(t.count),
        ]),
        new Set([3]),
      ),
    );
  }

  if (result.appliedWaivers.length > 0) {
    lines.push("", "Applied Waivers:");
    for (const w of result.appliedWaivers) {
      lines.push(
        `- ${w.query} (${w.channel ?? "primary"}) ${w.file} [${w.target}]: ${w.fromStatus} -> ${w.toStatus} (${w.reason}; test=${w.regressionTest})`,
      );
    }
  }

  if (result.inversions.length > 0) {
    lines.push("", "Blocked Deterministic Inversions:");
    for (const item of result.inversions.slice(0, 50)) {
      lines.push(
        `- ${item.file}:${String(item.line)}:${String(item.column)}-${String(item.endLine)}:${String(item.endColumn)} | ${item.query} (${item.channel}) | ${item.fromStatus} -> ${item.toStatus} | target=${item.target}`,
      );
    }
  }

  if (result.unknownIncreases.length > 0) {
    lines.push("", "Blocked Unknown Status Net Increases:");
    for (const item of result.unknownIncreases.slice(0, 50)) {
      lines.push(
        `- ${item.file}:${String(item.line)}:${String(item.column)}-${String(item.endLine)}:${String(item.endColumn)} | ${item.query} (${item.channel}) | ${item.fromStatus} -> ${item.toStatus} | target=${item.target}`,
      );
    }
  }

  return lines.join("\n");
}
