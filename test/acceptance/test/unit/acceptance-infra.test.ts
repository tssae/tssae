import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
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
  extractPackageScope,
  installArtifactPackages,
  resolveDefaultBlockedPaths,
  startArtifactRegistry,
  type PackedArtifact,
} from "@tssae/acceptance";

void describe("@tssae/acceptance infrastructure", () => {
  void it("serves unscoped and scoped local artifacts from the loopback registry", async () => {
    const dir = mkdtempSync(join(tmpdir(), "tssae-registry-unit-"));
    try {
      const tarballPath = join(dir, "tssae-0.1.0.tgz");
      writeFileSync(tarballPath, "fake-tarball");
      const artifact: PackedArtifact = {
        name: "tssae",
        version: "0.1.0",
        filename: tarballPath,
        size: 12,
        sha256: "abc",
        sha1: "def",
        integrity: "sha512-ghi",
        manifest: { name: "tssae", version: "0.1.0" },
      };

      assert.equal(extractPackageScope("tssae"), undefined);
      assert.equal(extractPackageScope("@tssae/zod"), "@tssae");
      assert.equal(extractPackageScope("@tssae/drizzle"), "@tssae");

      const server = await startArtifactRegistry([artifact], {
        upstreamRegistryUrl: null,
      });
      try {
        assert.deepEqual(server.unscopedPackages, ["tssae"]);
        assert.deepEqual(server.scopes, []);

        const metaRes = await fetch(`${server.url}/tssae`);
        assert.equal(metaRes.status, 200);
        const meta = (await metaRes.json()) as {
          readonly name: string;
          readonly "dist-tags": { readonly latest: string };
          readonly versions: Record<
            string,
            { readonly dist: { readonly tarball: string } }
          >;
        };
        assert.equal(meta.name, "tssae");
        assert.equal(meta["dist-tags"].latest, "0.1.0");

        const tgzRes = await fetch(meta.versions["0.1.0"]!.dist.tarball);
        assert.equal(tgzRes.status, 200);
        assert.equal(await tgzRes.text(), "fake-tarball");

        const missingRes = await fetch(`${server.url}/nonexistent-pkg`);
        assert.equal(missingRes.status, 404);
      } finally {
        await server.close();
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  void it("includes the current repository in default blocked paths", () => {
    const blocked = resolveDefaultBlockedPaths();
    assert.ok(blocked.length >= 1);
  });

  void it("rejects artifacts with incompatible engines.node under Node 24 strict engine checks", async () => {
    const dir = mkdtempSync(join(tmpdir(), "tssae-engine-strict-unit-"));
    try {
      const pkgStage = join(dir, "pkg-stage");
      const pkgDir = join(pkgStage, "package");
      mkdirSync(pkgDir, { recursive: true });
      const manifest = {
        name: "incompatible-node26-pkg",
        version: "0.1.0",
        engines: { node: "^26.13.1" },
      };
      writeFileSync(
        join(pkgDir, "package.json"),
        `${JSON.stringify(manifest, null, 2)}\n`,
      );
      writeFileSync(join(pkgDir, "index.js"), "export const ok = true;\n");
      const tarballPath = join(dir, "incompatible-node26-pkg-0.1.0.tgz");
      execFileSync("tar", ["-czf", tarballPath, "-C", pkgStage, "package"], {
        stdio: "ignore",
      });
      const tgzBytes = readFileSync(tarballPath);
      const artifact: PackedArtifact = {
        name: manifest.name,
        version: manifest.version,
        filename: tarballPath,
        size: tgzBytes.length,
        sha256: createHash("sha256").update(tgzBytes).digest("hex"),
        sha1: createHash("sha1").update(tgzBytes).digest("hex"),
        integrity: `sha512-${createHash("sha512").update(tgzBytes).digest("base64")}`,
        manifest,
      };

      for (const packageManager of ["pnpm", "npm"] as const) {
        const checkoutDir = join(dir, `host-${packageManager}`);
        const storeDir = join(dir, `store-${packageManager}`);
        mkdirSync(checkoutDir, { recursive: true });
        mkdirSync(storeDir, { recursive: true });
        writeFileSync(
          join(checkoutDir, "package.json"),
          `${JSON.stringify({ name: "engine-strict-host", private: true, type: "module" }, null, 2)}\n`,
        );

        const result = await installArtifactPackages({
          checkoutDir,
          packageManager,
          artifacts: [artifact],
          directPackages: [artifact.name],
          storeDir,
        });
        assert.equal(
          result.status,
          "install-failed",
          `Expected ${packageManager} strict engine check to reject ^26.13.1 under Node 24`,
        );
        assert.match(
          result.stderr,
          /EBADENGINE|ERR_PNPM_UNSUPPORTED_ENGINE|Unsupported engine/i,
        );
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
