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
  prepareBaselineArtifact,
  readRepoHeadState,
  resolveBaselineCacheDir,
  resolveBaselineRevision,
  resolveRepoRoot,
} from "@tssae/acceptance";

function createMinimalTssaeRepo(repoDir: string): {
  readonly mainSha: string;
  readonly featSha: string;
} {
  mkdirSync(join(repoDir, "packages/tssae/dist/bin"), { recursive: true });
  writeFileSync(
    join(repoDir, "package.json"),
    JSON.stringify({ name: "fixture-root", private: true }),
  );
  writeFileSync(
    join(repoDir, "pnpm-workspace.yaml"),
    "packages:\n  - packages/*\n",
  );
  writeFileSync(
    join(repoDir, "pnpm-lock.yaml"),
    [
      "lockfileVersion: '9.0'",
      "",
      "importers:",
      "  .: {}",
      "  packages/tssae: {}",
      "",
    ].join("\n"),
  );
  writeFileSync(
    join(repoDir, "packages/tssae/package.json"),
    JSON.stringify({
      name: "tssae",
      version: "0.1.0-fixture",
      type: "module",
      main: "./dist/index.js",
      scripts: {
        build: "node -e \"console.log('built')\"",
      },
    }),
  );
  writeFileSync(
    join(repoDir, "packages/tssae/dist/index.js"),
    "export const version = 'base';\n",
  );
  writeFileSync(
    join(repoDir, "packages/tssae/dist/bin/tssae-engine"),
    "#!/bin/sh\necho base\n",
  );

  execFileSync("git", ["init", "--quiet", "-b", "main"], { cwd: repoDir });
  execFileSync("git", ["config", "user.email", "test@example.com"], {
    cwd: repoDir,
  });
  execFileSync("git", ["config", "user.name", "Test User"], { cwd: repoDir });
  execFileSync("git", ["add", "."], { cwd: repoDir });
  execFileSync("git", ["commit", "--quiet", "-m", "main base"], {
    cwd: repoDir,
  });
  const mainSha = execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: repoDir,
    encoding: "utf8",
  }).trim();

  execFileSync("git", ["checkout", "--quiet", "-b", "feat/change"], {
    cwd: repoDir,
  });
  writeFileSync(
    join(repoDir, "packages/tssae/dist/index.js"),
    "export const version = 'feat';\n",
  );
  execFileSync("git", ["commit", "--quiet", "-am", "feature edit"], {
    cwd: repoDir,
  });
  const featSha = execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: repoDir,
    encoding: "utf8",
  }).trim();

  return { mainSha, featSha };
}

void describe("shared baseline artifact foundation", () => {
  void it("resolves baseline cache directory compatibly with TSSAE_PERF_CACHE_DIR", () => {
    const custom = resolveBaselineCacheDir("/tmp/tssae-baseline-custom");
    assert.equal(custom, "/tmp/tssae-baseline-custom");
  });

  void it("resolves repo root, head dirty state, and merge-base revision", () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-baseline-rev-"));
    try {
      const { mainSha, featSha } = createMinimalTssaeRepo(tempDir);
      assert.equal(resolveRepoRoot(join(tempDir, "packages/tssae")), tempDir);

      const cleanHead = readRepoHeadState(tempDir);
      assert.equal(cleanHead.commitSha, featSha);
      assert.equal(cleanHead.dirty, false);

      writeFileSync(join(tempDir, "untracked.txt"), "dirty\n");
      const dirtyHead = readRepoHeadState(tempDir);
      assert.equal(dirtyHead.dirty, true);

      const resolvedDefault = resolveBaselineRevision(tempDir);
      assert.equal(resolvedDefault.rev, "main");
      assert.equal(resolvedDefault.commitSha, mainSha);

      const resolvedExplicit = resolveBaselineRevision(tempDir, featSha);
      assert.equal(resolvedExplicit.rev, featSha);
      assert.equal(resolvedExplicit.commitSha, featSha);

      assert.throws(
        () => resolveBaselineRevision(tempDir, "nonexistent-ref-xyz"),
        /Failed to resolve baseline revision/u,
      );
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });

  void it("builds, caches, rebuilds corrupted cache, and refreshes baseline tssae artifacts", async () => {
    const tempDir = mkdtempSync(join(tmpdir(), "tssae-baseline-build-"));
    try {
      const repoDir = join(tempDir, "repo");
      const cacheDir = join(tempDir, "cache");
      mkdirSync(repoDir, { recursive: true });
      const { mainSha } = createMinimalTssaeRepo(repoDir);

      const first = await prepareBaselineArtifact({
        repoRoot: repoDir,
        cacheDir,
      });
      assert.equal(first.cacheHit, false);
      assert.equal(first.commitSha, mainSha);
      assert.equal(first.rev, "main");
      assert.ok(existsSync(first.tarballPath));
      assert.ok(existsSync(join(first.cacheDir, "ready.json")));

      const second = await prepareBaselineArtifact({
        repoRoot: repoDir,
        cacheDir,
      });
      assert.equal(second.cacheHit, true);
      assert.equal(second.sha256, first.sha256);
      assert.equal(second.binarySha256, first.binarySha256);
      assert.equal(second.tarballPath, first.tarballPath);

      // Corrupt cached tarball -> automatic rebuild
      writeFileSync(first.tarballPath, "corrupted-tarball");
      const rebuilt = await prepareBaselineArtifact({
        repoRoot: repoDir,
        cacheDir,
      });
      assert.equal(rebuilt.cacheHit, false);
      assert.equal(rebuilt.sha256, first.sha256);

      // Corrupt ready.json -> automatic rebuild
      const readyRaw = readFileSync(
        join(rebuilt.cacheDir, "ready.json"),
        "utf8",
      );
      assert.ok(readyRaw.includes(mainSha));
      writeFileSync(join(rebuilt.cacheDir, "ready.json"), "{broken");
      const rebuiltFromMeta = await prepareBaselineArtifact({
        repoRoot: repoDir,
        cacheDir,
      });
      assert.equal(rebuiltFromMeta.cacheHit, false);

      // Explicit refreshBaseline bypasses valid cache
      const refreshed = await prepareBaselineArtifact({
        repoRoot: repoDir,
        cacheDir,
        refreshBaseline: true,
      });
      assert.equal(refreshed.cacheHit, false);
      assert.equal(refreshed.sha256, first.sha256);
    } finally {
      rmSync(tempDir, { recursive: true, force: true });
    }
  });
});
