import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  buildAttributionReport,
  DEFAULT_PERF_BUDGET,
  formatPerfMarkdownReport,
  formatPerfTableReport,
  parsePerfCliArgs,
  type PerfReport,
  type WorkerRunSample,
} from "@tssae/acceptance";

function makeSample(overrides: {
  readonly wallMs: number;
  readonly peakEngineRssBytes?: number;
  readonly steps?: WorkerRunSample["steps"];
  readonly queries?: WorkerRunSample["queries"];
  readonly rpcBytesSent?: number;
  readonly rpcBytesReceived?: number;
}): WorkerRunSample {
  return {
    wallMs: overrides.wallMs,
    peakHostRssBytes: 40_000_000,
    peakEngineRssBytes: overrides.peakEngineRssBytes ?? 120_000_000,
    peakTotalRssBytes:
      40_000_000 + (overrides.peakEngineRssBytes ?? 120_000_000),
    sessionHostRssBytes: 38_000_000,
    sessionEngineRssBytes: 115_000_000,
    observeMode: "summary",
    rpcBytesSent: overrides.rpcBytesSent ?? 1000,
    rpcBytesReceived: overrides.rpcBytesReceived ?? 2000,
    steps: overrides.steps ?? [],
    queries: overrides.queries ?? [],
  };
}

void describe("attribution and report formatting", () => {
  void it("aggregates observe=summary attribution by engine step and query ordered by regression", () => {
    const baseline = makeSample({
      wallMs: 100,
      rpcBytesSent: 500,
      rpcBytesReceived: 1200,
      steps: [
        {
          op: "findSyntax",
          provider: "engine",
          cacheStatus: "computed",
          durationMs: 12,
          candidateCount: 50,
        },
        {
          op: "calleesOf",
          provider: "engine",
          cacheStatus: "computed",
          durationMs: 25,
          candidateCount: 10,
        },
      ],
      queries: [
        {
          op: "findSyntax",
          cacheStatus: "computed",
          durationMs: 15,
          candidateCount: 50,
        },
        {
          op: "calleesOf",
          cacheStatus: "computed",
          durationMs: 30,
          candidateCount: 10,
        },
      ],
    });
    const candidate = makeSample({
      wallMs: 140,
      rpcBytesSent: 600,
      rpcBytesReceived: 1800,
      steps: [
        {
          op: "findSyntax",
          provider: "engine",
          cacheStatus: "computed",
          durationMs: 14,
          candidateCount: 50,
        },
        {
          op: "calleesOf",
          provider: "engine",
          cacheStatus: "computed",
          durationMs: 65,
          candidateCount: 10,
        },
      ],
      queries: [
        {
          op: "findSyntax",
          cacheStatus: "computed",
          durationMs: 17,
          candidateCount: 50,
        },
        {
          op: "calleesOf",
          cacheStatus: "computed",
          durationMs: 70,
          candidateCount: 10,
        },
      ],
    });

    const attribution = buildAttributionReport(
      "coldQuery",
      baseline,
      candidate,
    );
    assert.equal(attribution.scenario, "coldQuery");
    assert.equal(attribution.observeMode, "summary");
    assert.equal(attribution.baselineRpcBytes, 1700);
    assert.equal(attribution.candidateRpcBytes, 2400);
    assert.equal(attribution.steps[0]?.op, "calleesOf");
    assert.equal(attribution.steps[0]?.deltaMs, 40);
    assert.equal(attribution.queries[0]?.op, "calleesOf");
    assert.equal(attribution.queries[0]?.deltaMs, 40);
  });

  void it("renders table, JSON-serializable object, and Markdown reports with corpus and scenario scope", () => {
    const baselineSample = makeSample({ wallMs: 100 });
    const candidateSample = makeSample({
      wallMs: 120,
      peakEngineRssBytes: 150_000_000,
    });
    const attribution = buildAttributionReport(
      "coldQuery",
      makeSample({
        wallMs: 100,
        steps: [
          {
            op: "calleesOf",
            provider: "engine",
            cacheStatus: "computed",
            durationMs: 20,
            candidateCount: 5,
          },
        ],
        queries: [
          {
            op: "calleesOf",
            cacheStatus: "computed",
            durationMs: 22,
            candidateCount: 5,
          },
        ],
      }),
      makeSample({
        wallMs: 120,
        steps: [
          {
            op: "calleesOf",
            provider: "engine",
            cacheStatus: "computed",
            durationMs: 42,
            candidateCount: 5,
          },
        ],
        queries: [
          {
            op: "calleesOf",
            cacheStatus: "computed",
            durationMs: 44,
            candidateCount: 5,
          },
        ],
      }),
    );

    const report: PerfReport = {
      passed: false,
      profile: "medium",
      warmupRounds: 1,
      samplingRounds: 7,
      budget: DEFAULT_PERF_BUDGET,
      corpus: {
        repoUrl: "https://github.com/microsoft/vscode.git",
        repoName: "vscode",
        commitSha: "3fc62371e017d5e6d3d742b7aa79832c9fc9e71d",
        subsets: ["src/vs/base", "src/typings", "src/tsconfig.base.json"],
        fileCount: 483,
        sourceOrigin: "host-mirror",
        cacheBypassed: false,
      },
      environment: {
        timestampUtc: "2026-11-26T17:00:00.000Z",
        platform: "linux",
        osRelease: "6.1.0",
        arch: "x64",
        cpuModel: "Test CPU",
        cpuCores: 8,
        totalMemoryBytes: 16 * 1024 * 1024 * 1024,
        nodeVersion: "v26.13.1",
        pnpmVersion: "12.21.0",
        baselineRev: "main",
        baselineCommitSha: "474154c000000000000000000000000000000000",
        baselineVersion: "0.1.0",
        baselineTarballSha256: "aaa",
        baselineBinarySha256: "bbb",
        baselineCacheHit: true,
        candidateCommitSha: "1111111000000000000000000000000000000000",
        candidateDirty: false,
        candidateVersion: "0.1.0",
        candidateTarballSha256: "ccc",
        candidateBinarySha256: "ddd",
      },
      scenarios: [
        {
          scenario: "coldQuery",
          label: "Cold Query",
          baseline: {
            p50Ms: 100,
            p95Ms: 110,
            peakHostRssBytes: 40_000_000,
            peakEngineRssBytes: 120_000_000,
            peakTotalRssBytes: 160_000_000,
            sessionHostRssBytes: 38_000_000,
            sessionEngineRssBytes: 115_000_000,
            samples: [baselineSample],
          },
          candidate: {
            p50Ms: 120,
            p95Ms: 130,
            peakHostRssBytes: 40_000_000,
            peakEngineRssBytes: 150_000_000,
            peakTotalRssBytes: 190_000_000,
            sessionHostRssBytes: 38_000_000,
            sessionEngineRssBytes: 145_000_000,
            samples: [candidateSample],
          },
          latencyDeltaRatio: 0.2,
          memoryDeltaRatio: 0.25,
          passed: false,
          violations: [
            "p50 latency regressed by +20.00% (budget <= +5.00%)",
            "peak memory grew by +25.00% (budget <= +10.00%)",
          ],
          attribution,
        },
      ],
      observeOverhead: [
        {
          mode: "off",
          p50Ms: 100,
          p95Ms: 110,
          peakEngineRssBytes: 120_000_000,
          peakTotalRssBytes: 160_000_000,
          rpcBytesTotal: 0,
          latencyOverheadMs: 0,
          latencyOverheadRatio: 0,
          memoryOverheadBytes: 0,
          samples: [baselineSample],
        },
        {
          mode: "summary",
          p50Ms: 103,
          p95Ms: 112,
          peakEngineRssBytes: 121_000_000,
          peakTotalRssBytes: 161_000_000,
          rpcBytesTotal: 2048,
          latencyOverheadMs: 3,
          latencyOverheadRatio: 0.03,
          memoryOverheadBytes: 1_000_000,
          samples: [baselineSample],
        },
        {
          mode: "detail",
          p50Ms: 109,
          p95Ms: 119,
          peakEngineRssBytes: 123_000_000,
          peakTotalRssBytes: 163_000_000,
          rpcBytesTotal: 16_384,
          latencyOverheadMs: 9,
          latencyOverheadRatio: 0.09,
          memoryOverheadBytes: 3_000_000,
          samples: [baselineSample],
        },
      ],
      totalDurationMs: 5_000,
    };

    const table = formatPerfTableReport(report);
    assert.match(table, /3fc62371e017d5e6d3d742b7aa79832c9fc9e71d/u);
    assert.match(table, /src\/vs\/base/u);
    assert.match(table, /Cold Query/u);
    assert.match(table, /FAIL/u);
    assert.match(table, /Attribution/u);
    assert.match(table, /calleesOf/u);
    assert.match(table, /Observe Overhead/u);

    const markdown = formatPerfMarkdownReport(report);
    assert.match(markdown, /3fc62371e017d5e6d3d742b7aa79832c9fc9e71d/u);
    assert.match(markdown, /483/u);
    assert.match(markdown, /Cold Query/u);
    assert.match(markdown, /calleesOf/u);
    assert.match(markdown, /summary/u);
    assert.match(markdown, /detail/u);

    const parsedJson = JSON.parse(JSON.stringify(report)) as PerfReport;
    assert.equal(parsedJson.corpus.fileCount, 483);
    assert.equal(
      parsedJson.scenarios[0]?.attribution?.steps[0]?.op,
      "calleesOf",
    );
  });

  void it("parses CLI flags including --profile, --baseline, --refresh-corpus, --no-corpus-cache, and output paths", () => {
    const parsed = parsePerfCliArgs([
      "--",
      "--profile",
      "large",
      "--baseline",
      "origin/main",
      "--rounds",
      "9",
      "--warmup",
      "2",
      "--refresh-corpus",
      "--no-corpus-cache",
      "--json-out",
      "out/perf.json",
      "--markdown-out",
      "out/perf.md",
    ]);
    assert.equal(parsed.profile, "large");
    assert.equal(parsed.baselineRev, "origin/main");
    assert.equal(parsed.rounds, 9);
    assert.equal(parsed.warmup, 2);
    assert.equal(parsed.refreshCorpus, true);
    assert.equal(parsed.noCorpusCache, true);
    assert.equal(parsed.jsonOut, "out/perf.json");
    assert.equal(parsed.markdownOut, "out/perf.md");
  });
});
