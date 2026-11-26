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
  CORPUS_PROFILES,
  materializeCorpus,
  type CorpusProfileSpec,
} from "@tssae/acceptance";

function createLocalFixtureGitRepo(rootDir: string): {
  readonly bareOriginDir: string;
  readonly commitSha: string;
} {
  const workDir = join(rootDir, "upstream-work");
  const bareOriginDir = join(rootDir, "upstream-origin.git");
  mkdirSync(join(workDir, "src/vs/base/common"), { recursive: true });
  mkdirSync(join(workDir, "src/vs/platform/common"), { recursive: true });
  mkdirSync(join(workDir, "src/typings"), { recursive: true });

  writeFileSync(
    join(workDir, "src/tsconfig.base.json"),
    JSON.stringify({ compilerOptions: { strict: true, target: "ES2024" } }),
  );
  writeFileSync(
    join(workDir, "src/typings/base-common.d.ts"),
    "export type Brand<T> = T;\n",
  );
  writeFileSync(
    join(workDir, "src/vs/base/common/lifecycle.ts"),
    "export function dispose(): void {}\n",
  );
  writeFileSync(
    join(workDir, "src/vs/base/common/event.ts"),
    "export function emit(): number { return 1; }\n",
  );
  writeFileSync(
    join(workDir, "src/vs/platform/common/service.ts"),
    "export const serviceId = 's';\n",
  );

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

void describe("fixed-commit corpus cache", () => {
  void it("defines medium and large VS Code profiles on the same commit", () => {
    assert.equal(CORPUS_PROFILES.medium.id, "medium");
    assert.equal(CORPUS_PROFILES.large.id, "large");
    assert.equal(
      CORPUS_PROFILES.medium.commitSha,
      CORPUS_PROFILES.large.commitSha,
    );
    assert.ok(CORPUS_PROFILES.medium.subsets.includes("src/vs/base"));
    assert.ok(CORPUS_PROFILES.large.subsets.includes("src/vs/platform"));
  });

  void it("caches fixed-commit checkouts, rebuilds missing cache, and bypasses on refresh", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-corpus-unit-"));
    try {
      const { bareOriginDir, commitSha } = createLocalFixtureGitRepo(tempDir);
      const cacheDir = join(tempDir, "cache");
      const spec: CorpusProfileSpec = {
        id: "medium",
        repoUrl: bareOriginDir,
        repoName: "fixture-vscode",
        commitSha,
        subsets: ["src/vs/base", "src/typings", "src/tsconfig.base.json"],
        includeGlobs: ["src/vs/base/**/*.ts", "src/typings/**/*.d.ts"],
        primaryFile: "src/vs/base/common/lifecycle.ts",
        secondaryFile: "src/vs/base/common/event.ts",
        targetCalleeName: "dispose",
      };

      const first = await materializeCorpus({
        profile: "medium",
        specOverride: spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      assert.equal(first.cacheHit, false);
      assert.equal(first.cacheBypassed, false);
      assert.equal(first.commitSha, commitSha);
      assert.equal(first.fileCount, 3);
      assert.ok(existsSync(first.tsconfigPath));
      assert.ok(!existsSync(join(first.corpusDir, "src/vs/platform")));

      const second = await materializeCorpus({
        profile: "medium",
        specOverride: spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      assert.equal(second.cacheHit, true);
      assert.equal(second.sourceOrigin, "cache");
      assert.equal(second.corpusDir, first.corpusDir);

      // Corrupt/delete cached checkout -> automatic rebuild from bare repo cache
      rmSync(first.corpusDir, { recursive: true, force: true });
      const rebuilt = await materializeCorpus({
        profile: "medium",
        specOverride: spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      assert.equal(rebuilt.cacheHit, false);
      assert.equal(rebuilt.fileCount, 3);
      assert.ok(existsSync(rebuilt.tsconfigPath));

      // Explicit cache bypass forces re-fetch & fresh checkout
      writeFileSync(join(rebuilt.corpusDir, "stale-marker.txt"), "stale");
      const refreshed = await materializeCorpus({
        profile: "medium",
        specOverride: spec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
        refreshCorpus: true,
      });
      assert.equal(refreshed.cacheHit, false);
      assert.equal(refreshed.cacheBypassed, true);
      assert.equal(
        existsSync(join(refreshed.corpusDir, "stale-marker.txt")),
        false,
      );

      // Large profile uses the same bare repo & implementation with wider subsets
      const largeSpec: CorpusProfileSpec = {
        ...spec,
        id: "large",
        subsets: [
          "src/vs/base",
          "src/vs/platform",
          "src/typings",
          "src/tsconfig.base.json",
        ],
        includeGlobs: [
          "src/vs/base/**/*.ts",
          "src/vs/platform/**/*.ts",
          "src/typings/**/*.d.ts",
        ],
      };
      const large = await materializeCorpus({
        profile: "large",
        specOverride: largeSpec,
        cacheDir,
        hostMirrorPaths: [bareOriginDir],
      });
      assert.equal(large.fileCount, 4);
      const parsedTsconfig = JSON.parse(
        readFileSync(large.tsconfigPath, "utf8"),
      ) as { readonly include?: readonly string[] };
      assert.ok(parsedTsconfig.include?.includes("src/vs/platform/**/*.ts"));
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });
});
