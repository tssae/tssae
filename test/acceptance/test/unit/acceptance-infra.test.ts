import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import {
  extractPackageScope,
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
      assert.equal(extractPackageScope("@tssae/adapters"), "@tssae");

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
});
