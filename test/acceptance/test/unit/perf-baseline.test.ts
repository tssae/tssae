import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import {
  buildPairedSamplingSchedule,
  resolveBaselineCacheDir,
  resolveBaselineRevision,
} from "@tssae/acceptance";

void describe("baseline resolution and paired ABBA sampling schedule", () => {
  void it("resolves merge-base HEAD main or explicit baseline revision", () => {
    const repoDir = mkdtempSync(join(tmpdir(), "tssae-baseline-unit-"));
    try {
      execFileSync("git", ["init", "--quiet", "-b", "main"], { cwd: repoDir });
      execFileSync("git", ["config", "user.email", "test@example.com"], {
        cwd: repoDir,
      });
      execFileSync("git", ["config", "user.name", "Test User"], {
        cwd: repoDir,
      });
      writeFileSync(join(repoDir, "file.txt"), "base\n");
      execFileSync("git", ["add", "."], { cwd: repoDir });
      execFileSync("git", ["commit", "--quiet", "-m", "base"], {
        cwd: repoDir,
      });
      const mainSha = execFileSync("git", ["rev-parse", "HEAD"], {
        cwd: repoDir,
        encoding: "utf8",
      }).trim();

      execFileSync("git", ["checkout", "--quiet", "-b", "feat/test"], {
        cwd: repoDir,
      });
      writeFileSync(join(repoDir, "file.txt"), "feature\n");
      execFileSync("git", ["commit", "--quiet", "-am", "feature"], {
        cwd: repoDir,
      });
      const featSha = execFileSync("git", ["rev-parse", "HEAD"], {
        cwd: repoDir,
        encoding: "utf8",
      }).trim();

      const resolvedDefault = resolveBaselineRevision(repoDir);
      assert.equal(resolvedDefault.rev, "main");
      assert.equal(resolvedDefault.commitSha, mainSha);

      const resolvedExplicit = resolveBaselineRevision(repoDir, featSha);
      assert.equal(resolvedExplicit.rev, featSha);
      assert.equal(resolvedExplicit.commitSha, featSha);
    } finally {
      rmSync(repoDir, { recursive: true, force: true });
    }
  });

  void it("resolves external baseline cache directory outside repository", () => {
    const cacheDir = resolveBaselineCacheDir("/tmp/custom-perf-cache");
    assert.equal(cacheDir, "/tmp/custom-perf-cache");
    const subDir = join(cacheDir, "baselines");
    mkdirSync(subDir, { recursive: true });
    rmSync(cacheDir, { recursive: true, force: true });
  });

  void it("alternates A/B execution order (ABBA...) without concurrent overlap", () => {
    const schedule = buildPairedSamplingSchedule(4);
    assert.deepEqual(schedule, [
      ["baseline", "candidate"],
      ["candidate", "baseline"],
      ["baseline", "candidate"],
      ["candidate", "baseline"],
    ]);
  });
});
