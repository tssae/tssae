import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import {
  parseConformanceCliArgs,
  runConformanceCli,
  runConformanceSweep,
  type ConformanceReport,
  type ExternalCorpusSpec,
} from "@tssae/acceptance";

function createGitCorpusRepo(
  rootDir: string,
  files: Readonly<Record<string, string>>,
): { readonly bareOriginDir: string; readonly commitSha: string } {
  const workDir = join(rootDir, "upstream-work");
  const bareOriginDir = join(rootDir, "upstream-origin.git");
  for (const [relPath, content] of Object.entries(files)) {
    const fullPath = join(workDir, relPath);
    mkdirSync(join(fullPath, ".."), { recursive: true });
    writeFileSync(fullPath, content, "utf8");
  }
  execFileSync("git", ["init", "--quiet"], { cwd: workDir });
  execFileSync("git", ["config", "user.email", "test@example.com"], {
    cwd: workDir,
  });
  execFileSync("git", ["config", "user.name", "Test User"], { cwd: workDir });
  execFileSync("git", ["add", "."], { cwd: workDir });
  execFileSync("git", ["commit", "--quiet", "-m", "fixture commit"], {
    cwd: workDir,
  });
  const commitSha = execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: workDir,
    encoding: "utf8",
  }).trim();
  execFileSync("git", ["clone", "--bare", "--quiet", workDir, bareOriginDir]);
  execFileSync("git", [
    "-C",
    bareOriginDir,
    "config",
    "uploadpack.allowAnySHA1InWant",
    "true",
  ]);
  return { bareOriginDir, commitSha };
}

function writeFakeTssaeModule(dir: string): string {
  const modPath = join(dir, "fake-tssae.mjs");
  const source = `
import { readFileSync } from "node:fs";
import { relative, resolve } from "node:path";

export async function openProject(options) {
  const tsconfig = JSON.parse(readFileSync(options.tsconfig, "utf8"));
  const files = (tsconfig.files ?? []).map((abs) => relative(options.cwd, abs).replaceAll("\\\\", "/"));
  let closed = false;
  let nextHandle = 1;
  const fileByHandle = new Map();
  const makeRef = (handleId, file) => {
    if (file !== undefined) {
      fileByHandle.set(handleId, file);
    }
    return { projectId: "p1", snapshotId: 1, handleId };
  };

  const ctx = {
    project: {
      projectId: "p1",
      configFile: options.tsconfig,
      entryConfig: options.tsconfig,
      entryChain: [options.tsconfig],
      referencedBy: [],
      projectReferences: [],
      compilerOptions: {
        strict: false,
        allowJs: true,
        checkJs: false,
        composite: false,
        paths: {},
        customConditions: [],
      },
      sourceFiles: files,
    },
    async findSyntax(opts) {
      const file = opts?.range?.file ?? files[0];
      const text = readFileSync(resolve(options.cwd, file), "utf8");
      if (text.includes("TRIGGER_HANG")) {
        await new Promise((resolveHang) => setTimeout(resolveHang, 60_000));
      }
      if (text.includes("TRIGGER_CRASH")) {
        process.exit(134);
      }
      const fnRef = makeRef("syn_" + (nextHandle++), file);
      const stmtRef = makeRef("syn_" + (nextHandle++), file);
      const callRef = makeRef("syn_" + (nextHandle++), file);
      const varInitRef = makeRef("syn_" + (nextHandle++), file);
      const condRef = makeRef("syn_" + (nextHandle++), file);
      const loc = { file, line: 1, column: 1, endLine: 1, endColumn: 10, start: 0, end: 9 };
      return [
        { ref: fnRef, kind: "FunctionDeclaration", text: "function f() {}", location: loc, body: stmtRef },
        { ref: stmtRef, kind: "ExpressionStatement", text: "f();", location: loc },
        { ref: callRef, kind: "CallExpression", text: "f()", location: loc, expression: condRef, arguments: [] },
        { ref: varInitRef, kind: "VariableDeclaration", text: "x = 1", location: loc, initializer: condRef },
      ];
    },
    async listBranches(targets) {
      return targets.map((target, idx) => {
        const file = fileByHandle.get(target.handleId) ?? files[Math.min(idx, files.length - 1)] ?? files[0];
        const text = readFileSync(resolve(options.cwd, file), "utf8");
        const loc = { file, line: 2, column: 3, endLine: 2, endColumn: 12, start: 10, end: 19 };
        if (text.includes("TRIGGER_LIST_BRANCHES_VIOLATION")) {
          return {
            target,
            branches: [],
            gaps: [{ kind: "unmodeled-construct", message: "Scope analysis validation failed: vertex 2 has no explicit edges", at: loc }],
          };
        }
        return {
          target,
          branches: [
            {
              ref: makeRef("brn_" + (nextHandle++)),
              kind: "if",
              direction: "true",
              condition: makeRef("syn_" + (nextHandle++)),
              conditionText: "cond",
              location: loc,
              statementLocation: loc,
            },
          ],
          gaps: [],
        };
      });
    },
    async branchCoverage(opts) {
      const file = files[0];
      const text = files.map((f) => readFileSync(resolve(options.cwd, f), "utf8")).join("\\n");
      const loc = { file, line: 3, column: 1, endLine: 3, endColumn: 5, start: 20, end: 24 };
      const gaps = [];
      if (text.includes("TRIGGER_BENIGN_UNKNOWN")) {
        gaps.push({ kind: "unmodeled-construct", message: "Control flow for Unknown node", at: loc });
      }
      if (text.includes("TRIGGER_QUERY_VIOLATION")) {
        gaps.push({ kind: "unmodeled-construct", message: "Scope analysis validation failed: target vertex out of bounds", at: loc });
      }
      return opts.targets.map((target) => ({
        target,
        targetReachable: true,
        implicitExceptions: { status: "proved", evidence: [] },
        ...(gaps.length > 0
          ? { status: "inconclusive", gaps, evidence: [] }
          : { status: "proved", evidence: [] }),
      }));
    },
    async resolveCalls(targets) {
      return targets.map((target) => ({ target, call: makeRef("cal_" + (nextHandle++)) }));
    },
    async calleesOf(calls) {
      return calls.map((call) => ({
        call,
        targets: [makeRef("dcl_" + (nextHandle++))],
        resolvedTargets: [],
        returnTarget: makeRef("syn_" + (nextHandle++)),
        evidence: [],
        completeness: "complete",
      }));
    },
    async callersOf(decls) {
      return decls.map((declaration) => ({
        declaration,
        calls: [],
        callSites: [],
        evidence: [],
        completeness: "complete",
      }));
    },
    async resolveImplementations(targets) {
      return targets.map((target) => ({
        target,
        status: "external",
        packageName: null,
        declarationFile: null,
        evidence: [],
      }));
    },
    async callEffects(opts) {
      return opts.targets.map((target) => ({
        target,
        fieldReads: [],
        fieldWrites: [],
        effects: [],
        callbackInvocations: [],
        calls: [],
        evidence: [],
        completeness: "complete",
      }));
    },
    async resolveInstances(targets) {
      return targets.map((target) => ({
        target,
        status: "disproved",
        evidence: [],
      }));
    },
    async instanceCleanup(opts) {
      return opts.scopes.map((scope) => ({
        scope,
        resolvedFunctions: [],
        targetResults: [],
        disposedInstances: [],
        registeredInstances: [],
        detachedInstances: [],
        evidence: [],
        gaps: [],
      }));
    },
    async valuePropagation() {
      return { propagation: "value", results: [], reachableTargets: [] };
    },
  };

  return {
    snapshotId: 1,
    snapshotDigest: "fake",
    projects: [ctx.project],
    fileOrigins: [],
    workspaceSourceFiles: files,
    additionalInputs: [],
    memory: { hostRssBytes: 1024 },
    projectContexts: [ctx],
    project() { return ctx; },
    async close() { closed = true; },
  };
}
`;
  writeFileSync(modPath, source, "utf8");
  return modPath;
}

void describe("conformance sweep runner", () => {
  void it("lowers all root top-level statements after a leading directive or declaration and exercises class field initializers", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-conformance-toplevel-"));
    try {
      const { bareOriginDir, commitSha } = createGitCorpusRepo(tempDir, {
        "test/strict-script.js": [
          '"use strict";',
          "var seed = 1;",
          "for (const item of [seed, 2]) {",
          "  if (item > 1) {",
          "    void item;",
          "  }",
          "}",
          "try {",
          "  void seed;",
          "} finally {",
          "  void 0;",
          "}",
          "class Box {",
          "  value = seed > 0 ? 10 : 20;",
          "}",
          "void Box;",
          "",
        ].join("\n"),
      });
      const tssaeEntryPath = createRequire(import.meta.url).resolve("tssae");
      const spec: ExternalCorpusSpec = {
        id: "mini-toplevel",
        repoUrl: bareOriginDir,
        repoName: "mini",
        commitSha,
        sparsePaths: ["test"],
        fileMatch: { scanPaths: ["test"], extensions: [".js"] },
        minFileCount: 1,
      };

      const report = await runConformanceSweep({
        corpora: [spec],
        corpusCacheDir: join(tempDir, "cache"),
        hostMirrorPaths: [bareOriginDir],
        batchSize: 1,
        concurrency: 1,
        batchTimeoutMs: 10_000,
        tssaeEntryPath,
        logProgress: () => {},
      });

      assert.equal(report.passed, true);
      assert.equal(report.totalFiles, 1);
      // 6 root top-level statements: "use strict";, var seed = 1;, for-of, try-finally, class Box, void Box;
      assert.equal(report.totalScopes, 6);
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });

  void it("passes valid batches including benign Unknown syntax-recovery gaps without writing tsconfig into corpus dir", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-conformance-pass-"));
    try {
      const { bareOriginDir, commitSha } = createGitCorpusRepo(tempDir, {
        "test/a.js": "function a() { return 1; }\n",
        "test/b.js": "// TRIGGER_BENIGN_UNKNOWN\nfunction b( { }\n",
        "test/c.js": "export const c = 3;\n",
      });
      const fakeTssaePath = writeFakeTssaeModule(tempDir);
      const cacheDir = join(tempDir, "cache");
      const spec: ExternalCorpusSpec = {
        id: "mini-pass",
        repoUrl: bareOriginDir,
        repoName: "mini",
        commitSha,
        sparsePaths: ["test"],
        fileMatch: { scanPaths: ["test"], extensions: [".js"] },
        minFileCount: 3,
      };

      const report = await runConformanceSweep({
        corpora: [spec],
        corpusCacheDir: cacheDir,
        hostMirrorPaths: [bareOriginDir],
        batchSize: 2,
        concurrency: 2,
        batchTimeoutMs: 5_000,
        tssaeEntryPath: fakeTssaePath,
        logProgress: () => {},
      });

      assert.equal(report.passed, true);
      assert.equal(report.totalFiles, 3);
      assert.ok(report.totalScopes >= 3);
      assert.ok(report.totalQueries > 0);
      assert.equal(report.failures.length, 0);
      const corpusDir = report.corpora[0]!.corpusDir;
      assert.equal(existsSync(join(corpusDir, "tsconfig.batch.json")), false);
      assert.deepEqual(readdirSync(corpusDir).sort(), [
        ".corpus-ready.json",
        "test",
      ]);
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });

  void it("fails and reports invariant violations from listBranches and query validation", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-conformance-inv-"));
    try {
      const { bareOriginDir, commitSha } = createGitCorpusRepo(tempDir, {
        "test/ok.js": "function ok() { return 1; }\n",
        "test/bad-ir.js": "// TRIGGER_LIST_BRANCHES_VIOLATION\n",
        "test/bad-query.js": "// TRIGGER_QUERY_VIOLATION\n",
      });
      const fakeTssaePath = writeFakeTssaeModule(tempDir);
      const spec: ExternalCorpusSpec = {
        id: "mini-inv",
        repoUrl: bareOriginDir,
        repoName: "mini",
        commitSha,
        sparsePaths: ["test"],
        fileMatch: { scanPaths: ["test"], extensions: [".js"] },
        minFileCount: 3,
      };

      const report = await runConformanceSweep({
        corpora: [spec],
        corpusCacheDir: join(tempDir, "cache"),
        hostMirrorPaths: [bareOriginDir],
        batchSize: 3,
        concurrency: 1,
        batchTimeoutMs: 5_000,
        tssaeEntryPath: fakeTssaePath,
        logProgress: () => {},
      });

      assert.equal(report.passed, false);
      assert.ok(
        report.failures.some(
          (f) =>
            f.kind === "invariant-violation" &&
            f.file === "test/bad-ir.js" &&
            f.message.includes("Scope analysis validation failed:"),
        ),
      );
      assert.ok(
        report.failures.some(
          (f) =>
            f.kind === "invariant-violation" &&
            f.message.includes("target vertex out of bounds"),
        ),
      );
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });

  void it("kills hanging batches on timeout and bisects to pinpoint the offending file", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-conformance-timeout-"));
    try {
      const { bareOriginDir, commitSha } = createGitCorpusRepo(tempDir, {
        "test/01-ok.js": "export const a = 1;\n",
        "test/02-hang.js": "// TRIGGER_HANG\n",
        "test/03-ok.js": "export const c = 3;\n",
      });
      const fakeTssaePath = writeFakeTssaeModule(tempDir);
      const spec: ExternalCorpusSpec = {
        id: "mini-timeout",
        repoUrl: bareOriginDir,
        repoName: "mini",
        commitSha,
        sparsePaths: ["test"],
        fileMatch: { scanPaths: ["test"], extensions: [".js"] },
        minFileCount: 3,
      };

      const report = await runConformanceSweep({
        corpora: [spec],
        corpusCacheDir: join(tempDir, "cache"),
        hostMirrorPaths: [bareOriginDir],
        batchSize: 3,
        concurrency: 1,
        batchTimeoutMs: 500,
        tssaeEntryPath: fakeTssaePath,
        logProgress: () => {},
      });

      assert.equal(report.passed, false);
      assert.equal(report.failures.length, 1);
      assert.equal(report.failures[0]!.kind, "timeout");
      assert.equal(report.failures[0]!.file, "test/02-hang.js");
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });

  void it("catches worker crashes and bisects to pinpoint the crashing file", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-conformance-crash-"));
    try {
      const { bareOriginDir, commitSha } = createGitCorpusRepo(tempDir, {
        "test/01-ok.js": "export const a = 1;\n",
        "test/02-ok.js": "export const b = 2;\n",
        "test/03-crash.js": "// TRIGGER_CRASH\n",
      });
      const fakeTssaePath = writeFakeTssaeModule(tempDir);
      const spec: ExternalCorpusSpec = {
        id: "mini-crash",
        repoUrl: bareOriginDir,
        repoName: "mini",
        commitSha,
        sparsePaths: ["test"],
        fileMatch: { scanPaths: ["test"], extensions: [".js"] },
        minFileCount: 3,
      };

      const report = await runConformanceSweep({
        corpora: [spec],
        corpusCacheDir: join(tempDir, "cache"),
        hostMirrorPaths: [bareOriginDir],
        batchSize: 3,
        concurrency: 1,
        batchTimeoutMs: 5_000,
        tssaeEntryPath: fakeTssaePath,
        logProgress: () => {},
      });

      assert.equal(report.passed, false);
      assert.equal(report.failures.length, 1);
      assert.equal(report.failures[0]!.kind, "crash");
      assert.equal(report.failures[0]!.file, "test/03-crash.js");
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });
});

void describe("conformance CLI", () => {
  void it("parses flags and validates corpus and numeric options", () => {
    const defaults = parseConformanceCliArgs([]);
    assert.deepEqual(defaults.corpora, ["test262", "typescript-conformance"]);
    assert.equal(defaults.refreshCorpus, false);
    assert.equal(defaults.noCorpusCache, false);

    const parsed = parseConformanceCliArgs([
      "--corpora",
      "test262",
      "--refresh-corpus",
      "--no-corpus-cache",
      "--batch-size",
      "120",
      "--concurrency",
      "2",
      "--batch-timeout-ms",
      "30000",
      "--json-out",
      "out/conformance.json",
      "--markdown-out",
      "out/conformance.md",
    ]);
    assert.deepEqual(parsed.corpora, ["test262"]);
    assert.equal(parsed.refreshCorpus, true);
    assert.equal(parsed.noCorpusCache, true);
    assert.equal(parsed.batchSize, 120);
    assert.equal(parsed.concurrency, 2);
    assert.equal(parsed.batchTimeoutMs, 30_000);
    assert.equal(parsed.jsonOut, "out/conformance.json");
    assert.equal(parsed.markdownOut, "out/conformance.md");

    assert.throws(() => parseConformanceCliArgs(["--corpora", "unknown"]));
    assert.throws(() => parseConformanceCliArgs(["--batch-size", "0"]));
  });

  void it("writes JSON/Markdown reports and returns 0 on pass, 1 on failure, 2 on usage error", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-conformance-cli-"));
    try {
      const { bareOriginDir, commitSha } = createGitCorpusRepo(tempDir, {
        "test/ok.js": "function ok() { return 1; }\n",
        "test/bad.js": "// TRIGGER_LIST_BRANCHES_VIOLATION\n",
      });
      const fakeTssaePath = writeFakeTssaeModule(tempDir);
      const outDir = join(tempDir, "reports");
      const okSpec: ExternalCorpusSpec = {
        id: "ok-only",
        repoUrl: bareOriginDir,
        repoName: "mini",
        commitSha,
        sparsePaths: ["test"],
        fileMatch: { scanPaths: ["test"], extensions: [".js"] },
        minFileCount: 2,
      };

      const stdoutChunks: string[] = [];
      const stderrChunks: string[] = [];

      const invalidCode = await runConformanceCli(["--bogus"], {
        stdout: (s) => stdoutChunks.push(s),
        stderr: (s) => stderrChunks.push(s),
      });
      assert.equal(invalidCode, 2);

      const helpCode = await runConformanceCli(["--help"], {
        stdout: (s) => stdoutChunks.push(s),
        stderr: (s) => stderrChunks.push(s),
      });
      assert.equal(helpCode, 0);

      const failCode = await runConformanceCli(
        ["--json-out", "fail.json", "--markdown-out", "fail.md"],
        {
          cwd: outDir,
          stdout: (s) => stdoutChunks.push(s),
          stderr: (s) => stderrChunks.push(s),
          sweepOverrides: {
            corpora: [okSpec],
            corpusCacheDir: join(tempDir, "cache"),
            hostMirrorPaths: [bareOriginDir],
            tssaeEntryPath: fakeTssaePath,
            logProgress: () => {},
          },
        },
      );
      assert.equal(failCode, 1);

      const parsedJson = JSON.parse(
        readFileSync(join(outDir, "fail.json"), "utf8"),
      ) as ConformanceReport;
      assert.equal(parsedJson.passed, false);
      assert.equal(parsedJson.failures[0]!.file, "test/bad.js");
      const markdownText = readFileSync(join(outDir, "fail.md"), "utf8");
      assert.match(markdownText, /test\/bad\.js/u);

      // Now run on a clean repo -> exitCode === 0
      const cleanRepo = createGitCorpusRepo(join(tempDir, "clean"), {
        "test/good.js": "export const x = 1;\n",
      });
      const cleanCode = await runConformanceCli(["--json-out", "pass.json"], {
        cwd: outDir,
        stdout: (s) => stdoutChunks.push(s),
        stderr: (s) => stderrChunks.push(s),
        sweepOverrides: {
          corpora: [
            {
              id: "clean-only",
              repoUrl: cleanRepo.bareOriginDir,
              repoName: "clean",
              commitSha: cleanRepo.commitSha,
              sparsePaths: ["test"],
              fileMatch: { scanPaths: ["test"], extensions: [".js"] },
              minFileCount: 1,
            },
          ],
          corpusCacheDir: join(tempDir, "cache-clean"),
          hostMirrorPaths: [cleanRepo.bareOriginDir],
          tssaeEntryPath: fakeTssaePath,
          logProgress: () => {},
        },
      });
      assert.equal(cleanCode, 0);
      const passJson = JSON.parse(
        readFileSync(join(outDir, "pass.json"), "utf8"),
      ) as ConformanceReport;
      assert.equal(passJson.passed, true);
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });
});
