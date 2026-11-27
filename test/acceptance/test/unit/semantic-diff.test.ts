import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  collectQueryDigest,
  diffDigests,
  formatSemanticDiffReport,
  loadSemanticDiffWaivers,
  resolveDefaultWaiversPath,
  runSemanticDiffCli,
  runSemanticDiffGate,
  type DigestSample,
  type QueryDigest,
  type SemanticDiffWaiver,
} from "@tssae/acceptance";
import { openProject } from "tssae";

function createControlledFixtureProject(rootDir: string): void {
  mkdirSync(join(rootDir, "src"), { recursive: true });
  writeFileSync(
    join(rootDir, "tsconfig.json"),
    JSON.stringify({
      compilerOptions: {
        target: "esnext",
        module: "nodenext",
        moduleResolution: "nodenext",
        strict: true,
        noEmit: true,
      },
      include: ["src/**/*.ts"],
    }),
  );
  writeFileSync(
    join(rootDir, "src/helper.ts"),
    [
      "export class Resource {",
      "  value = 0;",
      "  dispose(): void {",
      "    this.value = -1;",
      "  }",
      "}",
      "",
      "export function compute(x: number): number {",
      "  if (x > 0) {",
      "    return x + 1;",
      "  }",
      "  return 0;",
      "}",
      "",
      "export function mutateBox(box: { count: number }): void {",
      "  box.count = 1;",
      "}",
      "",
    ].join("\n"),
  );
  writeFileSync(
    join(rootDir, "src/main.ts"),
    [
      'import { compute, mutateBox, Resource } from "./helper.ts";',
      "",
      "export function runWorkflow(flag: boolean, x: number): number {",
      "  const box = { count: 0 };",
      "  const res = new Resource();",
      "  try {",
      "    const result = compute(x);",
      "    if (flag) {",
      "      mutateBox(box);",
      "      res.dispose();",
      "      return result;",
      "    }",
      "  } catch {",
      "    res.dispose();",
      "    return -1;",
      "  }",
      "  res.dispose();",
      "  return box.count;",
      "}",
      "",
    ].join("\n"),
  );
}

void describe("semantic diff query digest", () => {
  void it("collects all 8 query categories deterministically without temporary handle IDs", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-digest-fixture-"));
    try {
      createControlledFixtureProject(tempDir);

      const session1 = await openProject({ cwd: tempDir });
      let digest1a: string;
      let digest1b: string;
      let parsedDigest;
      try {
        const ctx1 = session1.project();
        const d1a = await collectQueryDigest(ctx1);
        const d1b = await collectQueryDigest(ctx1);
        digest1a = JSON.stringify(d1a);
        digest1b = JSON.stringify(d1b);
        parsedDigest = d1a;
      } finally {
        await session1.close();
      }

      assert.equal(digest1a, digest1b);

      const session2 = await openProject({ cwd: tempDir });
      let digest2: string;
      try {
        const ctx2 = session2.project();
        digest2 = JSON.stringify(await collectQueryDigest(ctx2));
      } finally {
        await session2.close();
      }

      assert.equal(digest1a, digest2);

      const seenQueries = new Set(parsedDigest.samples.map((s) => s.query));
      for (const expectedQuery of [
        "branchCoverage",
        "calleesOf",
        "callersOf",
        "valuePropagation",
        "resolveInstances",
        "instanceCleanup",
        "callEffects",
        "resolveImplementations",
      ] as const) {
        assert.ok(
          seenQueries.has(expectedQuery),
          `Expected query category "${expectedQuery}" in digest`,
        );
      }

      const branchChannels = new Set(
        parsedDigest.samples
          .filter((s) => s.query === "branchCoverage")
          .map((s) => s.channel),
      );
      assert.ok(branchChannels.has("primary"));
      assert.ok(branchChannels.has("implicitExceptions"));

      const cleanupChannels = new Set(
        parsedDigest.samples
          .filter((s) => s.query === "instanceCleanup")
          .map((s) => s.channel),
      );
      assert.ok(cleanupChannels.has("primary"));
      assert.ok(cleanupChannels.has("implicitExceptions"));

      assert.doesNotMatch(digest1a, /"(?:syn|cal|dcl|brn|ins|sig|typ)_\d+"/u);
      assert.doesNotMatch(digest1a, /"snapshotId"/u);
      assert.doesNotMatch(digest1a, /"handleId"/u);
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });
});

function makeSyntheticDigest(samples: readonly DigestSample[]): QueryDigest {
  return {
    schemaVersion: 1,
    projectCount: 1,
    fileCount: 1,
    sampleCount: samples.length,
    counts: [],
    samples,
  };
}

function makeSample(
  overrides: Partial<DigestSample> &
    Pick<DigestSample, "query" | "status" | "targetId">,
): DigestSample {
  return {
    channel: "primary",
    file: "src/vs/base/common/event.ts",
    location: {
      file: "src/vs/base/common/event.ts",
      line: 42,
      column: 3,
      endLine: 42,
      endColumn: 20,
      start: 1000,
      end: 1017,
    },
    ...overrides,
  };
}

const BASELINE_SHA = "cdfa2ef2aad2675131b7235fada6788a449f80e2";

void describe("diffDigests pure gate evaluation", () => {
  void it("passes on zero diff and on precision improvements", () => {
    const baseline = makeSyntheticDigest([
      makeSample({
        query: "branchCoverage",
        targetId: "bc-1",
        status: "inconclusive",
      }),
      makeSample({
        query: "calleesOf",
        targetId: "cal-1",
        status: "incomplete",
      }),
      makeSample({
        query: "resolveImplementations",
        targetId: "impl-1",
        status: "inconclusive",
      }),
      makeSample({
        query: "valuePropagation",
        targetId: "vp-1",
        status: "proved",
      }),
    ]);

    const sameResult = diffDigests(baseline, baseline, {
      baselineCommit: BASELINE_SHA,
    });
    assert.equal(sameResult.passed, true);
    assert.equal(sameResult.inversions.length, 0);
    assert.equal(sameResult.unknownIncreases.length, 0);

    const improved = makeSyntheticDigest([
      makeSample({
        query: "branchCoverage",
        targetId: "bc-1",
        status: "proved",
      }),
      makeSample({
        query: "calleesOf",
        targetId: "cal-1",
        status: "complete",
      }),
      makeSample({
        query: "resolveImplementations",
        targetId: "impl-1",
        status: "resolved",
      }),
      makeSample({
        query: "valuePropagation",
        targetId: "vp-1",
        status: "proved",
      }),
    ]);

    const improvedResult = diffDigests(baseline, improved, {
      baselineCommit: BASELINE_SHA,
    });
    assert.equal(improvedResult.passed, true);
    assert.equal(improvedResult.inversions.length, 0);
    assert.equal(improvedResult.unknownIncreases.length, 0);
  });

  void it("blocks proved <-> disproved and resolved <-> external inversions with location details", () => {
    const baseline = makeSyntheticDigest([
      makeSample({
        query: "branchCoverage",
        channel: "implicitExceptions",
        targetId: "bc-exc-1",
        status: "proved",
      }),
      makeSample({
        query: "resolveImplementations",
        targetId: "impl-1",
        status: "resolved",
      }),
    ]);
    const candidate = makeSyntheticDigest([
      makeSample({
        query: "branchCoverage",
        channel: "implicitExceptions",
        targetId: "bc-exc-1",
        status: "disproved",
      }),
      makeSample({
        query: "resolveImplementations",
        targetId: "impl-1",
        status: "external",
      }),
    ]);

    const result = diffDigests(baseline, candidate, {
      baselineCommit: BASELINE_SHA,
    });
    assert.equal(result.passed, false);
    assert.equal(result.inversions.length, 2);
    assert.equal(result.inversions[0]?.file, "src/vs/base/common/event.ts");
    assert.equal(result.inversions[0]?.line, 42);
    assert.equal(result.inversions[0]?.fromStatus, "proved");
    assert.equal(result.inversions[0]?.toStatus, "disproved");

    const report = formatSemanticDiffReport(result);
    assert.match(report, /FAIL/u);
    assert.match(report, /src\/vs\/base\/common\/event\.ts:42:3/u);
    assert.match(report, /branchCoverage/u);
    assert.match(report, /proved -> disproved/u);
  });

  void it("blocks net increases in inconclusive or incomplete counts while allowing net-neutral shifts", () => {
    const baseline = makeSyntheticDigest([
      makeSample({
        query: "valuePropagation",
        targetId: "vp-1",
        status: "proved",
      }),
      makeSample({
        query: "valuePropagation",
        targetId: "vp-2",
        status: "inconclusive",
      }),
      makeSample({
        query: "callEffects",
        targetId: "fx-1",
        status: "complete",
      }),
    ]);

    const regressed = makeSyntheticDigest([
      makeSample({
        query: "valuePropagation",
        targetId: "vp-1",
        status: "inconclusive",
      }),
      makeSample({
        query: "valuePropagation",
        targetId: "vp-2",
        status: "inconclusive",
      }),
      makeSample({
        query: "callEffects",
        targetId: "fx-1",
        status: "incomplete",
      }),
    ]);

    const failResult = diffDigests(baseline, regressed, {
      baselineCommit: BASELINE_SHA,
    });
    assert.equal(failResult.passed, false);
    assert.equal(failResult.unknownIncreases.length, 2);

    const balanced = makeSyntheticDigest([
      makeSample({
        query: "valuePropagation",
        targetId: "vp-1",
        status: "inconclusive",
      }),
      makeSample({
        query: "valuePropagation",
        targetId: "vp-2",
        status: "proved",
      }),
      makeSample({
        query: "callEffects",
        targetId: "fx-1",
        status: "complete",
      }),
    ]);

    const passResult = diffDigests(baseline, balanced, {
      baselineCommit: BASELINE_SHA,
    });
    assert.equal(passResult.passed, true);
    assert.equal(passResult.unknownIncreases.length, 0);
  });
});

const STALE_SHA = "1111111111111111111111111111111111111111";

void describe("semantic diff waivers and baseline hash matching", () => {
  void it("applies waivers matching active baselineCommit and blocks unwaived regressions when commit mismatches", () => {
    const baseline = makeSyntheticDigest([
      makeSample({
        query: "branchCoverage",
        targetId: "bc-1",
        status: "proved",
      }),
      makeSample({
        query: "valuePropagation",
        targetId: "vp-1",
        status: "proved",
      }),
    ]);
    const candidate = makeSyntheticDigest([
      makeSample({
        query: "branchCoverage",
        targetId: "bc-1",
        status: "disproved",
      }),
      makeSample({
        query: "valuePropagation",
        targetId: "vp-1",
        status: "inconclusive",
      }),
    ]);

    const matchingWaivers: readonly SemanticDiffWaiver[] = [
      {
        baselineCommit: BASELINE_SHA,
        query: "branchCoverage",
        file: "src/vs/base/common/event.ts",
        target: "bc-1",
        fromStatus: "proved",
        toStatus: "disproved",
        reason: "Baseline missed bypass path in catch block",
        regressionTest: "packages/tssae/test/branch-coverage.test.ts",
      },
      {
        baselineCommit: BASELINE_SHA,
        query: "valuePropagation",
        file: "src/vs/base/common/event.ts",
        target: "vp-1",
        fromStatus: "proved",
        toStatus: "inconclusive",
        reason: "Soundness fix on dynamic getter propagation",
        regressionTest: "packages/tssae/test/value-propagation.test.ts",
      },
    ];

    const waivedResult = diffDigests(baseline, candidate, {
      baselineCommit: BASELINE_SHA,
      waivers: matchingWaivers,
    });
    assert.equal(waivedResult.passed, true);
    assert.equal(waivedResult.appliedWaivers.length, 2);
    assert.equal(waivedResult.ignoredWaiverCount, 0);

    const staleWaivers = matchingWaivers.map((w) => ({
      ...w,
      baselineCommit: STALE_SHA,
    }));

    const blockedOnStale = diffDigests(baseline, candidate, {
      baselineCommit: BASELINE_SHA,
      waivers: staleWaivers,
    });
    assert.equal(blockedOnStale.passed, false);
    assert.equal(blockedOnStale.appliedWaivers.length, 0);
    assert.equal(blockedOnStale.ignoredWaiverCount, 2);
    assert.equal(blockedOnStale.inversions.length, 1);
    assert.equal(blockedOnStale.unknownIncreases.length, 1);

    const cleanAfterMerge = diffDigests(candidate, candidate, {
      baselineCommit: BASELINE_SHA,
      waivers: staleWaivers,
    });
    assert.equal(cleanAfterMerge.passed, true);
    assert.equal(cleanAfterMerge.ignoredWaiverCount, 2);
  });

  void it("loads default and temporary waiver files and validates waiver schema", () => {
    const defaultWaivers = loadSemanticDiffWaivers(resolveDefaultWaiversPath());
    assert.ok(Array.isArray(defaultWaivers));

    const tempDir = mkdtempSync(join(tmpdir(), "tssae-waivers-test-"));
    try {
      const validFile = join(tempDir, "waivers.json");
      writeFileSync(
        validFile,
        JSON.stringify({
          waivers: [
            {
              baselineCommit: BASELINE_SHA,
              query: "valuePropagation",
              file: "src/a.ts",
              target: "vp-1",
              fromStatus: "proved",
              toStatus: "inconclusive",
              reason: "Known fix",
              regressionTest: "test/vp.test.ts",
            },
          ],
        }),
      );
      const loaded = loadSemanticDiffWaivers(validFile);
      assert.equal(loaded.length, 1);
      assert.equal(loaded[0]?.baselineCommit, BASELINE_SHA);

      const invalidFile = join(tempDir, "bad-waivers.json");
      writeFileSync(
        invalidFile,
        JSON.stringify({
          waivers: [
            {
              baselineCommit: "short-sha",
              query: "valuePropagation",
              file: "src/a.ts",
              target: "vp-1",
              fromStatus: "proved",
              toStatus: "inconclusive",
              reason: "Known fix",
              regressionTest: "test/vp.test.ts",
            },
          ],
        }),
      );
      assert.throws(
        () => loadSemanticDiffWaivers(invalidFile),
        /baselineCommit/u,
      );
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });
});

void describe("semantic diff runner and CLI on controlled fixtures", () => {
  void it("returns exit code 0 on --help and 2 on invalid arguments", async () => {
    const outChunks: string[] = [];
    const errChunks: string[] = [];
    const helpCode = await runSemanticDiffCli(["--help"], {
      stdout: (chunk) => outChunks.push(chunk),
      stderr: (chunk) => errChunks.push(chunk),
    });
    assert.equal(helpCode, 0);
    assert.match(outChunks.join(""), /Usage:/u);
    assert.equal(errChunks.length, 0);

    const errChunks2: string[] = [];
    const badCode = await runSemanticDiffCli(["--unknown-flag"], {
      stdout: () => {},
      stderr: (chunk) => errChunks2.push(chunk),
    });
    assert.equal(badCode, 2);
    assert.match(errChunks2.join(""), /Unknown option/u);
  });

  void it("exits 0 when controlled fixture passes and 1 when candidate regresses", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-cli-fixture-"));
    try {
      const fixtureDir = join(tempDir, "fixture");
      createControlledFixtureProject(fixtureDir);

      const tssaeDistEntry = resolve(
        dirname(fileURLToPath(import.meta.url)),
        "../../../../packages/tssae/dist/index.js",
      );

      // Wrapper candidate module that flips branchCoverage primary status to trigger regression
      const regressingEntry = join(tempDir, "regressing-tssae.mjs");
      writeFileSync(
        regressingEntry,
        [
          `import { openProject as realOpenProject } from ${JSON.stringify(pathToFileURL(tssaeDistEntry).href)};`,
          "export async function openProject(options) {",
          "  const session = await realOpenProject(options);",
          "  const origProject = session.project.bind(session);",
          "  const wrapCtx = (ctx) => ({",
          "    ...ctx,",
          "    branchCoverage: async (opts) => {",
          "      const rows = await ctx.branchCoverage(opts);",
          "      return rows.map((row) => ({",
          "        ...row,",
          '        status: row.status === "proved" ? "disproved" : row.status === "disproved" ? "proved" : "inconclusive",',
          "      }));",
          "    },",
          "  });",
          "  return {",
          "    ...session,",
          "    projectContexts: session.projectContexts.map(wrapCtx),",
          "    project: (...args) => wrapCtx(origProject(...args)),",
          "  };",
          "}",
          "",
        ].join("\n"),
      );

      const jsonOut = join(tempDir, "diff-report.json");
      const passStdout: string[] = [];
      const passCode = await runSemanticDiffCli(
        [
          "--project-dir",
          fixtureDir,
          "--tsconfig",
          "tsconfig.json",
          "--baseline-entry",
          tssaeDistEntry,
          "--candidate-entry",
          tssaeDistEntry,
          "--baseline-commit",
          BASELINE_SHA,
          "--json-out",
          jsonOut,
        ],
        {
          stdout: (chunk) => passStdout.push(chunk),
          stderr: () => {},
        },
      );
      assert.equal(passCode, 0);
      assert.match(passStdout.join(""), /Status: PASS/u);
      const writtenJson = JSON.parse(readFileSync(jsonOut, "utf8")) as {
        readonly diff: { readonly passed: boolean };
      };
      assert.equal(writtenJson.diff.passed, true);

      const failStdout: string[] = [];
      const failCode = await runSemanticDiffCli(
        [
          "--project-dir",
          fixtureDir,
          "--tsconfig",
          "tsconfig.json",
          "--baseline-entry",
          tssaeDistEntry,
          "--candidate-entry",
          regressingEntry,
          "--baseline-commit",
          BASELINE_SHA,
        ],
        {
          stdout: (chunk) => failStdout.push(chunk),
          stderr: () => {},
        },
      );
      assert.equal(failCode, 1);
      const failOutput = failStdout.join("");
      assert.match(failOutput, /Status: FAIL/u);
      assert.match(failOutput, /branchCoverage/u);
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });

  void it("supports same-commit fast path when HEAD matches clean main", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-fastpath-"));
    try {
      const repoDir = join(tempDir, "repo");
      const fixtureDir = join(tempDir, "fixture");
      mkdirSync(repoDir, { recursive: true });
      createControlledFixtureProject(fixtureDir);

      writeFileSync(join(repoDir, "README.md"), "repo\n");
      execFileSync("git", ["init", "--quiet", "-b", "main"], { cwd: repoDir });
      execFileSync("git", ["config", "user.email", "test@example.com"], {
        cwd: repoDir,
      });
      execFileSync("git", ["config", "user.name", "Test User"], {
        cwd: repoDir,
      });
      execFileSync("git", ["add", "."], { cwd: repoDir });
      execFileSync("git", ["commit", "--quiet", "-m", "initial"], {
        cwd: repoDir,
      });

      const tssaeDistEntry = resolve(
        dirname(fileURLToPath(import.meta.url)),
        "../../../../packages/tssae/dist/index.js",
      );

      const result = await runSemanticDiffGate({
        repoRoot: repoDir,
        projectDir: fixtureDir,
        tsconfig: "tsconfig.json",
        candidateEntryPath: tssaeDistEntry,
        logProgress: () => {},
      });

      assert.equal(result.fastPath, true);
      assert.equal(result.diff.passed, true);
      assert.ok(result.diff.baselineSampleCount > 0);
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });
});
