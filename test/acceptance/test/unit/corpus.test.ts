import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import {
  materializeExternalCorpus,
  resolveCorpusCacheDir,
  resolveCorpusCheckoutDirName,
  type ExternalCorpusSpec,
} from "@tssae/acceptance";

function createFixtureGitRepo(rootDir: string): {
  readonly workDir: string;
  readonly bareOriginDir: string;
  readonly commitSha: string;
} {
  const workDir = join(rootDir, "upstream-work");
  const bareOriginDir = join(rootDir, "upstream-origin.git");
  mkdirSync(join(workDir, "tests/language"), { recursive: true });
  mkdirSync(join(workDir, "tests/built-ins"), { recursive: true });
  mkdirSync(join(workDir, "docs"), { recursive: true });

  writeFileSync(join(workDir, "tests/language/a.js"), "export const a = 1;\n");
  writeFileSync(join(workDir, "tests/language/b.ts"), "export const b = 2;\n");
  writeFileSync(
    join(workDir, "tests/language/fixture.json"),
    '{"ignore": true}\n',
  );
  writeFileSync(join(workDir, "tests/built-ins/c.js"), "export const c = 3;\n");
  writeFileSync(join(workDir, "docs/readme.md"), "# docs\n");

  execFileSync("git", ["init", "--quiet"], { cwd: workDir });
  execFileSync("git", ["config", "user.email", "test@example.com"], {
    cwd: workDir,
  });
  execFileSync("git", ["config", "user.name", "Test User"], { cwd: workDir });
  execFileSync("git", ["add", "."], { cwd: workDir });
  execFileSync("git", ["commit", "--quiet", "-m", "initial commit"], {
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
  return { workDir, bareOriginDir, commitSha };
}

void describe("shared external corpus foundation", () => {
  void it("derives checkout directory names and materializes sparse subsets with ready metadata", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-shared-corpus-"));
    try {
      const { bareOriginDir, commitSha } = createFixtureGitRepo(tempDir);
      const cacheDir = resolveCorpusCacheDir(join(tempDir, "cache"));

      const spec: ExternalCorpusSpec = {
        id: "sample",
        repoUrl: bareOriginDir,
        repoName: "sample",
        commitSha,
        sparsePaths: ["tests/language", "tests/built-ins"],
        minFileCount: 3,
      };
      assert.equal(resolveCorpusCheckoutDirName(spec), `sample-${commitSha}`);

      let afterExtractCalled = 0;
      const first = await materializeExternalCorpus({
        spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
        onAfterExtract: ({ stagingDir, fileCount }) => {
          afterExtractCalled += 1;
          assert.equal(fileCount, 3);
          writeFileSync(join(stagingDir, "custom-marker.txt"), "ok\n");
        },
      });
      assert.equal(afterExtractCalled, 1);
      assert.equal(first.cacheHit, false);
      assert.equal(first.cacheBypassed, false);
      assert.equal(first.sourceOrigin, "host-mirror");
      assert.equal(first.fileCount, 3);
      assert.equal(
        first.corpusDir,
        join(cacheDir, "checkouts", `sample-${commitSha}`),
      );
      assert.ok(existsSync(join(first.corpusDir, "custom-marker.txt")));
      assert.ok(!existsSync(join(first.corpusDir, "docs")));

      const readyJson = JSON.parse(
        readFileSync(join(first.corpusDir, ".corpus-ready.json"), "utf8"),
      ) as {
        readonly id?: string;
        readonly commitSha?: string;
        readonly fileCount?: number;
      };
      assert.equal(readyJson.id, "sample");
      assert.equal(readyJson.commitSha, commitSha);
      assert.equal(readyJson.fileCount, 3);

      const second = await materializeExternalCorpus({
        spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      assert.equal(second.cacheHit, true);
      assert.equal(second.sourceOrigin, "cache");
      assert.equal(second.fileCount, 3);
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });

  void it("adopts existing legacy .corpus-ready.json and .git sparse worktrees offline without fetching", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-corpus-offline-"));
    try {
      const { bareOriginDir, commitSha } = createFixtureGitRepo(tempDir);
      const cacheDir = join(tempDir, "cache");
      const checkoutsDir = join(cacheDir, "checkouts");
      mkdirSync(checkoutsDir, { recursive: true });

      // 1. Legacy .corpus-ready.json using "profile" instead of "id"
      const legacyDirName = `sample-${commitSha.slice(0, 12)}-medium`;
      const legacyDir = join(checkoutsDir, legacyDirName);
      mkdirSync(join(legacyDir, "tests/language"), { recursive: true });
      writeFileSync(
        join(legacyDir, "tests/language/a.js"),
        "export const a = 1;\n",
      );
      writeFileSync(
        join(legacyDir, ".corpus-ready.json"),
        JSON.stringify({
          profile: "medium",
          repoName: "sample",
          commitSha,
          subsets: ["tests/language"],
          fileCount: 1,
        }),
      );
      const legacyHit = await materializeExternalCorpus({
        spec: {
          id: "medium",
          repoUrl: "https://invalid.example.invalid/unreachable.git",
          repoName: "sample",
          commitSha,
          sparsePaths: ["tests/language"],
          checkoutDirName: legacyDirName,
          minFileCount: 1,
        },
        cacheDir,
        hostMirrorPaths: [],
      });
      assert.equal(legacyHit.cacheHit, true);
      assert.equal(legacyHit.sourceOrigin, "cache");
      assert.equal(legacyHit.fileCount, 1);

      // 2. Existing .git sparse checkout without .corpus-ready.json
      const gitCheckoutDir = join(checkoutsDir, `sample-${commitSha}`);
      execFileSync("git", [
        "clone",
        "--quiet",
        "--no-checkout",
        "--filter=blob:none",
        bareOriginDir,
        gitCheckoutDir,
      ]);
      execFileSync("git", [
        "-C",
        gitCheckoutDir,
        "sparse-checkout",
        "set",
        "--no-cone",
        "tests/language",
        "tests/built-ins",
      ]);
      execFileSync("git", [
        "-C",
        gitCheckoutDir,
        "checkout",
        "--quiet",
        commitSha,
      ]);
      assert.equal(
        existsSync(join(gitCheckoutDir, ".corpus-ready.json")),
        false,
      );

      const adopted = await materializeExternalCorpus({
        spec: {
          id: "sample",
          repoUrl: "https://invalid.example.invalid/unreachable.git",
          repoName: "sample",
          commitSha,
          sparsePaths: ["tests/language", "tests/built-ins"],
          minFileCount: 3,
        },
        cacheDir,
        hostMirrorPaths: [],
      });
      assert.equal(adopted.cacheHit, true);
      assert.equal(adopted.sourceOrigin, "cache");
      assert.equal(adopted.fileCount, 3);
      assert.ok(existsSync(join(gitCheckoutDir, ".corpus-ready.json")));
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });

  void it("applies fileMatch rules and rejects checkouts below minFileCount", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-corpus-filter-"));
    try {
      const { workDir, bareOriginDir, commitSha } =
        createFixtureGitRepo(tempDir);
      writeFileSync(
        join(workDir, "tests/language/helper_FIXTURE.js"),
        "export const fixture = 1;\n",
      );
      execFileSync("git", ["add", "."], { cwd: workDir });
      execFileSync("git", ["commit", "--quiet", "-m", "add fixture"], {
        cwd: workDir,
      });
      const secondSha = execFileSync("git", ["rev-parse", "HEAD"], {
        cwd: workDir,
        encoding: "utf8",
      }).trim();
      execFileSync(
        "git",
        ["push", "--quiet", bareOriginDir, "HEAD:refs/heads/main"],
        {
          cwd: workDir,
        },
      );

      const cacheDir = join(tempDir, "cache");
      const matched = await materializeExternalCorpus({
        spec: {
          id: "js-only",
          repoUrl: bareOriginDir,
          repoName: "sample",
          commitSha: secondSha,
          sparsePaths: ["tests/language", "tests/built-ins"],
          fileMatch: {
            extensions: [".js"],
            scanPaths: ["tests/language"],
            excludeFileSuffixes: ["_FIXTURE.js"],
          },
          minFileCount: 1,
        },
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      // Only tests/language/a.js matches (.ts, .json, _FIXTURE.js, and tests/built-ins excluded from scanPaths).
      assert.equal(matched.fileCount, 1);

      await assert.rejects(
        () =>
          materializeExternalCorpus({
            spec: {
              id: "too-few",
              repoUrl: bareOriginDir,
              repoName: "sample",
              commitSha,
              sparsePaths: ["tests/language"],
              fileMatch: { extensions: [".js"] },
              minFileCount: 5,
            },
            cacheDir,
            hostMirrorPaths: [bareOriginDir],
          }),
        /minFileCount=5/u,
      );
      assert.equal(
        existsSync(join(cacheDir, "checkouts", `sample-${commitSha}-too-few`)),
        false,
      );
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });

  void it("rebuilds corrupted or mismatched caches and bypasses cache on refresh", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-corpus-rebuild-"));
    try {
      const { workDir, bareOriginDir, commitSha } =
        createFixtureGitRepo(tempDir);
      writeFileSync(
        join(workDir, "tests/language/d.js"),
        "export const d = 4;\n",
      );
      execFileSync("git", ["add", "."], { cwd: workDir });
      execFileSync("git", ["commit", "--quiet", "-m", "second commit"], {
        cwd: workDir,
      });
      const secondSha = execFileSync("git", ["rev-parse", "HEAD"], {
        cwd: workDir,
        encoding: "utf8",
      }).trim();
      execFileSync(
        "git",
        ["push", "--quiet", bareOriginDir, "HEAD:refs/heads/main"],
        {
          cwd: workDir,
        },
      );

      const cacheDir = join(tempDir, "cache");
      const spec: ExternalCorpusSpec = {
        id: "sample",
        repoUrl: bareOriginDir,
        repoName: "sample",
        commitSha: secondSha,
        sparsePaths: ["tests/language", "tests/built-ins"],
        minFileCount: 4,
      };

      const initial = await materializeExternalCorpus({
        spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      assert.equal(initial.cacheHit, false);
      assert.equal(initial.fileCount, 4);

      // 1. Corrupt .corpus-ready.json -> automatic rebuild
      writeFileSync(join(initial.corpusDir, ".corpus-ready.json"), "{broken");
      const afterCorruptMeta = await materializeExternalCorpus({
        spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      assert.equal(afterCorruptMeta.cacheHit, false);
      assert.equal(afterCorruptMeta.fileCount, 4);

      // 2. Missing sparsePath on disk -> automatic rebuild
      rmSync(join(afterCorruptMeta.corpusDir, "tests/built-ins"), {
        recursive: true,
        force: true,
      });
      const afterMissingSubset = await materializeExternalCorpus({
        spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      assert.equal(afterMissingSubset.cacheHit, false);
      assert.ok(
        existsSync(join(afterMissingSubset.corpusDir, "tests/built-ins/c.js")),
      );

      // 3. Stale .git checkout pointing to an older HEAD -> replaced cleanly
      rmSync(afterMissingSubset.corpusDir, { recursive: true, force: true });
      execFileSync("git", [
        "clone",
        "--quiet",
        bareOriginDir,
        afterMissingSubset.corpusDir,
      ]);
      execFileSync("git", [
        "-C",
        afterMissingSubset.corpusDir,
        "checkout",
        "--quiet",
        commitSha,
      ]);
      const afterWrongHead = await materializeExternalCorpus({
        spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      assert.equal(afterWrongHead.cacheHit, false);
      assert.equal(afterWrongHead.fileCount, 4);

      // 4. Explicit refreshCorpus / noCorpusCache bypasses cache and drops stale files
      writeFileSync(join(afterWrongHead.corpusDir, "stale.txt"), "stale");
      const refreshed = await materializeExternalCorpus({
        spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
        refreshCorpus: true,
      });
      assert.equal(refreshed.cacheHit, false);
      assert.equal(refreshed.cacheBypassed, true);
      assert.equal(existsSync(join(refreshed.corpusDir, "stale.txt")), false);

      const noCache = await materializeExternalCorpus({
        spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
        noCorpusCache: true,
      });
      assert.equal(noCache.cacheHit, false);
      assert.equal(noCache.cacheBypassed, true);
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });
});
