import assert from "node:assert/strict";
import { symlinkSync } from "node:fs";
import { describe, it } from "node:test";

import { withTempHost } from "@verule/test-hosts";
import {
  TssaeError,
  createAdditionalInputRegistry,
  defineAdapter,
  defineFactProvider,
  defineFactType,
  openProject,
  type AdditionalInputRegistrar,
  type AdditionalInputSource,
} from "tssae";

void describe("tssae additional inputs", () => {
  void it("merges config and adapter declarations for the same physical file while preserving all sources and keeping compiler sourceFiles unchanged", async () => {
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
          kind: "adapter",
          id: "package-allowlist-adapter",
        });
        registrar.registerAdditionalInput("allowed-packages.link.json", {
          kind: "adapter",
          id: "symlink-adapter",
        });

        const standaloneEntries = registrar.listAdditionalInputs();
        assert.equal(standaloneEntries.length, 1);
        assert.equal(standaloneEntries[0]?.file, "allowed-packages.json");
        assert.deepEqual(standaloneEntries[0]?.sources, [
          { kind: "config" },
          { kind: "adapter", id: "package-allowlist-adapter" },
          { kind: "adapter", id: "symlink-adapter" },
          { kind: "rule", id: "allowlist-reader" },
        ] satisfies readonly AdditionalInputSource[]);
        assert.equal(registrar.getTrackedAbsolutePaths().length, 1);

        const AllowlistFact = defineFactType<
          "syntax",
          undefined,
          { readonly allowed: readonly string[] }
        >({
          id: "policy.allowlist",
          subjectKind: "syntax",
        });

        const allowlistAdapter = defineAdapter({
          id: "allowlist-adapter",
          provides: [
            defineFactProvider(AllowlistFact, async (ctx) => {
              const raw = ctx.readAdditionalInput(allowlistPath);
              const parsed = JSON.parse(raw) as {
                readonly allowed: readonly string[];
              };
              const calls = await ctx.findSyntax({
                kinds: ["CallExpression"],
              });
              return calls.map((call) => ({
                subject: call.ref,
                value: { allowed: parsed.allowed },
              }));
            }),
          ],
        });

        // Session integration with adapter registration sharing the same mechanism
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          additionalInputs: ["./allowed-packages.json"],
          adapters: [allowlistAdapter],
        });
        try {
          session.registerAdditionalInput("./src/../allowed-packages.json", {
            kind: "adapter",
            id: "package-allowlist-adapter",
          });

          const ctx = session.project();
          const raw = ctx.readAdditionalInput("allowed-packages.json");
          assert.ok(JSON.parse(raw));
          const facts = await ctx.facts(AllowlistFact);
          assert.ok(facts.length >= 1);

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
            { kind: "adapter", id: "allowlist-adapter" },
            { kind: "adapter", id: "package-allowlist-adapter" },
          ]);
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

  void it("tracks additional inputs read inside LibraryAdapter.extract, preserves adapter sources on fact cache reuse, and invalidates facts when the adapter manifest changes", async () => {
    await withTempHost(
      { host: "sdk-projects/additional-inputs-adapter" },
      async (host) => {
        await host.typecheck();

        const RequiredFieldsFact = defineFactType<
          "syntax",
          undefined,
          readonly string[]
        >({
          id: "policy.required-fields",
          subjectKind: "syntax",
        });

        let extractCount = 0;
        const policyAdapter = defineAdapter({
          id: "policy-manifest-adapter",
          provides: [
            defineFactProvider(RequiredFieldsFact, async (ctx) => {
              extractCount += 1;
              const raw = ctx.readAdditionalInput("schema-policy.json");
              const parsed = JSON.parse(raw) as {
                readonly requiredFields: readonly string[];
              };
              const calls = await ctx.findSyntax({
                kinds: ["CallExpression"],
              });
              return calls
                .filter((call) => call.text.startsWith("validatePayload("))
                .map((call) => ({
                  subject: call.ref,
                  value: parsed.requiredFields,
                  evidence: [
                    {
                      kind: "source" as const,
                      message: `Policy requires [${parsed.requiredFields.join(", ")}]`,
                      at: call.location,
                    },
                  ],
                }));
            }),
          ],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [policyAdapter],
        });
        try {
          const ctx1 = session.project();
          const firstFacts = await ctx1.facts(RequiredFieldsFact);
          assert.equal(extractCount, 1);
          assert.deepEqual(firstFacts[0]?.value, ["id"]);
          assert.deepEqual(session.additionalInputs[0]?.sources, [
            { kind: "adapter", id: "policy-manifest-adapter" },
          ]);

          // Repeated query on same snapshot hits FactCache (extractCount stays 1) and preserves adapter source
          const secondFacts = await ctx1.facts(RequiredFieldsFact);
          assert.equal(extractCount, 1);
          assert.deepEqual(secondFacts, firstFacts);
          assert.deepEqual(session.additionalInputs[0]?.sources, [
            { kind: "adapter", id: "policy-manifest-adapter" },
          ]);

          // Modify ONLY schema-policy.json and call session.update() -> FactCache invalidates and re-extracts
          host.writeText(
            "schema-policy.json",
            JSON.stringify({ requiredFields: ["id", "tenantId"] }, null, 2),
          );
          const updateRes = await session.update();
          assert.ok(
            updateRes.recomputation.invalidatedBy.includes(
              "additional-input:schema-policy.json",
            ),
          );

          const updatedFacts = await session
            .project()
            .facts(RequiredFieldsFact);
          assert.equal(extractCount, 2);
          assert.deepEqual(updatedFacts[0]?.value, ["id", "tenantId"]);
        } finally {
          await session.close();
        }
      },
    );
  });
});
