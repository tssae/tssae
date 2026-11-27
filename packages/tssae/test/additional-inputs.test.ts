import assert from "node:assert/strict";
import { symlinkSync } from "node:fs";
import { describe, it } from "node:test";

import { withTempHost } from "@tssae/test-hosts";
import {
  TssaeError,
  createAdditionalInputRegistry,
  openProject,
  type AdditionalInputRegistrar,
  type AdditionalInputSource,
} from "tssae";

void describe("tssae additional inputs", () => {
  void it("merges config, summary, and rule declarations for the same physical file, invalidates on update, and keeps compiler sourceFiles unchanged", async () => {
    await withTempHost(
      {
        host: "sdk-projects/additional-inputs-basic",
        setup: (host) => {
          symlinkSync(
            host.resolvePath("allowed-packages.json"),
            host.resolvePath("allowed-packages.link.json"),
          );
        },
      },
      async (host) => {
        await host.typecheck();

        const allowlistPath = host.resolvePath("allowed-packages.json");

        // Standalone registry test using the shared AdditionalInputRegistrar hook
        const registrar: AdditionalInputRegistrar =
          createAdditionalInputRegistry({
            cwd: host.rootDir,
          });
        registrar.registerAdditionalInput("./allowed-packages.json", {
          kind: "config",
        });
        registrar.registerAdditionalInput(allowlistPath, {
          kind: "rule",
          id: "allowlist-reader",
        });
        registrar.registerAdditionalInput("./src/../allowed-packages.json", {
          kind: "summary",
          id: "package-allowlist-summary",
        });
        registrar.registerAdditionalInput("allowed-packages.link.json", {
          kind: "summary",
          id: "symlink-summary",
        });

        const standaloneEntries = registrar.listAdditionalInputs();
        assert.equal(standaloneEntries.length, 1);
        assert.equal(standaloneEntries[0]?.file, "allowed-packages.json");
        assert.deepEqual(standaloneEntries[0]?.sources, [
          { kind: "config" },
          { kind: "summary", id: "package-allowlist-summary" },
          { kind: "summary", id: "symlink-summary" },
          { kind: "rule", id: "allowlist-reader" },
        ] satisfies readonly AdditionalInputSource[]);
        assert.equal(registrar.getTrackedAbsolutePaths().length, 1);

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          additionalInputs: ["./allowed-packages.json"],
        });
        try {
          session.registerAdditionalInput("./src/../allowed-packages.json", {
            kind: "rule",
            id: "allowlist-rule",
          });

          const ctx = session.project();
          const raw = ctx.readAdditionalInput("allowed-packages.json");
          assert.ok(JSON.parse(raw));

          assert.deepEqual(session.projects[0]?.sourceFiles, ["src/app.ts"]);
          assert.deepEqual(
            session.fileOrigins.map((origin) => origin.file),
            ["src/app.ts"],
          );
          assert.equal(session.additionalInputs.length, 1);
          const merged = session.additionalInputs[0]!;
          assert.equal(merged.file, "allowed-packages.json");
          assert.equal(merged.exists, true);
          assert.ok(merged.digest && merged.digest.length > 0);
          assert.deepEqual(merged.sources, [
            { kind: "config" },
            { kind: "rule", id: "allowlist-rule" },
          ]);

          host.writeText(
            "allowed-packages.json",
            JSON.stringify({ allowed: ["tssae", "zod"] }, null, 2),
          );
          const updateRes = await session.update();
          assert.ok(
            updateRes.recomputation.invalidatedBy.includes(
              "additional-input:allowed-packages.json",
            ),
          );
        } finally {
          await session.close();
        }

        // Open options validation rejects empty additionalInputs strings
        await assert.rejects(
          () =>
            openProject({
              cwd: host.rootDir,
              tsconfig: "tsconfig.json",
              additionalInputs: ["   "],
            }),
          (err: unknown) =>
            err instanceof TssaeError && err.code === "INVALID_CONFIG",
        );
      },
    );
  });
});
