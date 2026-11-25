import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  VeruleExecutionError,
  defineConfig,
  defineRule,
  openSession,
  runCheck,
  type SyntaxRef,
} from "@verule/sdk";
import {
  type PreparedTempHost,
  withTempHost,
  writeSyntheticPackage,
} from "@verule/test-hosts";

function materializeExtLib(host: PreparedTempHost): void {
  writeSyntheticPackage(host.rootDir, "ext-lib", {
    files: {
      "index.d.ts": host.readText("deps/ext-lib/index.d.ts"),
    },
  });
  host.removePath("deps");
}

void describe("@verule/sdk multi-project scope and merging", () => {
  void it("opens an empty-root aggregate tsconfig, explains file origins, isolates @runtime types, and merges only matching diagnostics", async () => {
    await withTempHost(
      {
        host: "sdk-projects/compiler-projects",
        setup: materializeExtLib,
      },
      async (host) => {
        await host.typecheck("packages/web/tsconfig.json");
        await host.typecheck("packages/node/tsconfig.json");

        const observedTypesByProject = new Map<string, string>();

        const runtimeModeRule = defineRule({
          id: "runtime-mode-rule",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({
              files: ["packages/shared/shared.ts"],
              kinds: ["CallExpression"],
            });
            const checkCalls = calls.filter((c) =>
              c.text.startsWith("checkMode("),
            );
            for (const call of checkCalls) {
              const arg = call.arguments?.[0];
              if (!arg) continue;
              const [ty] = await ctx.typeAt([arg]);
              if (!ty) continue;
              observedTypesByProject.set(ctx.project.projectId, ty.typeText);
              ctx.report({
                at: call.location,
                message: `Resolved @runtime mode is ${ty.typeText}`,
                findingKey: `runtime-${String(ty.literalValue ?? "unknown")}`,
                basis: [
                  {
                    kind: "type",
                    message: `@runtime resolved to ${ty.typeText}`,
                    at: call.location,
                  },
                ],
              });
            }
          },
        });

        const sharedSameConclusionRule = defineRule({
          id: "shared-same-conclusion",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({
              files: ["packages/shared/shared.ts"],
              kinds: ["CallExpression"],
            });
            for (const call of calls.filter((c) =>
              c.text.startsWith("checkMode("),
            )) {
              ctx.report({
                at: call.location,
                message: "Shared checkMode call must be audited",
                findingKey: "shared-audit",
                basis: [
                  {
                    kind: "source",
                    message: "Called checkMode in shared.ts",
                    at: call.location,
                  },
                ],
              });
            }
          },
        });

        const differentFindingKeyRule = defineRule({
          id: "different-finding-key",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({
              files: ["packages/shared/shared.ts"],
              kinds: ["CallExpression"],
            });
            for (const call of calls.filter((c) =>
              c.text.startsWith("checkMode("),
            )) {
              ctx.report({
                at: call.location,
                message: "Same message across projects",
                findingKey: `key-for-${ctx.project.projectId}`,
                basis: [
                  {
                    kind: "note",
                    message: "Same evidence across projects",
                    at: call.location,
                  },
                ],
              });
            }
          },
        });

        const differentEvidenceRule = defineRule({
          id: "different-evidence",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({
              files: ["packages/shared/shared.ts"],
              kinds: ["CallExpression"],
            });
            for (const call of calls.filter((c) =>
              c.text.startsWith("checkMode("),
            )) {
              ctx.report({
                at: call.location,
                message: "Same message and key across projects",
                findingKey: "same-key",
                basis: [
                  {
                    kind: "note",
                    message: `Evidence from ${ctx.project.projectId}`,
                    at: call.location,
                  },
                ],
              });
            }
          },
        });

        const config = defineConfig({
          tsconfig: "tsconfig.json",
          rules: [
            runtimeModeRule,
            sharedSameConclusionRule,
            differentFindingKeyRule,
            differentEvidenceRule,
          ],
        });

        const session = await openSession({ cwd: host.rootDir, config });
        let report;
        try {
          assert.equal(session.projects.length, 3);
          const webProj = session.projects.find(
            (p) => p.configFile === "packages/web/tsconfig.json",
          );
          const nodeProj = session.projects.find(
            (p) => p.configFile === "packages/node/tsconfig.json",
          );
          assert.ok(webProj);
          assert.ok(nodeProj);
          assert.equal(webProj.entryConfig, "tsconfig.json");
          assert.deepEqual(webProj.entryChain, [
            "tsconfig.json",
            "packages/web/tsconfig.json",
          ]);
          assert.equal(webProj.compilerOptions.strict, true);
          assert.deepEqual(webProj.compilerOptions.paths["@runtime"], [
            "./src/web.ts",
          ]);

          assert.equal(nodeProj.entryConfig, "tsconfig.json");
          assert.deepEqual(nodeProj.entryChain, [
            "tsconfig.json",
            "packages/node/tsconfig.json",
          ]);
          assert.equal(nodeProj.compilerOptions.strict, false);
          assert.equal(nodeProj.compilerOptions.allowJs, true);
          assert.deepEqual(nodeProj.compilerOptions.paths["@runtime"], [
            "./src/node.ts",
          ]);

          const sharedOrigin = session.fileOrigins.find(
            (o) => o.file === "packages/shared/shared.ts",
          );
          assert.ok(sharedOrigin);
          assert.deepEqual(
            sharedOrigin.projects.map((p) => p.projectId),
            ["packages/node/tsconfig.json", "packages/web/tsconfig.json"],
          );
          assert.equal(
            session.fileOrigins.some((o) => o.file.includes("node_modules")),
            false,
          );
          assert.equal(
            session.fileOrigins.some((o) => o.file.includes("lib.es")),
            false,
          );

          report = await session.check();
        } finally {
          await session.close();
        }

        assert.equal(
          observedTypesByProject.get("packages/web/tsconfig.json"),
          '"web"',
        );
        assert.equal(
          observedTypesByProject.get("packages/node/tsconfig.json"),
          '"node"',
        );

        assert.deepEqual(report.fileOrigins, session.fileOrigins);

        const runtimeDiags = report.diagnostics.filter(
          (d) => d.ruleId === "runtime-mode-rule",
        );
        assert.equal(runtimeDiags.length, 2);
        assert.deepEqual(
          runtimeDiags.find((d) => d.message.includes('"web"'))?.projects,
          ["packages/web/tsconfig.json"],
        );
        assert.deepEqual(
          runtimeDiags.find((d) => d.message.includes('"node"'))?.projects,
          ["packages/node/tsconfig.json"],
        );

        const mergedDiags = report.diagnostics.filter(
          (d) => d.ruleId === "shared-same-conclusion",
        );
        assert.equal(mergedDiags.length, 1);
        assert.deepEqual(mergedDiags[0]?.projects, [
          "packages/node/tsconfig.json",
          "packages/web/tsconfig.json",
        ]);

        const unmergedByKey = report.diagnostics.filter(
          (d) => d.ruleId === "different-finding-key",
        );
        assert.equal(unmergedByKey.length, 2);

        const unmergedByEvidence = report.diagnostics.filter(
          (d) => d.ruleId === "different-evidence",
        );
        assert.equal(unmergedByEvidence.length, 2);
        assert.deepEqual(
          unmergedByEvidence.map((d) => d.projects),
          [["packages/node/tsconfig.json"], ["packages/web/tsconfig.json"]],
        );
      },
    );
  });

  void it("supports multiple independent entry configs including jsconfig.json and rejects cross-project refs", async () => {
    await withTempHost(
      {
        hosts: [
          "sdk-projects/compiler-projects",
          "sdk-projects/compiler-projects-jsapp",
        ],
        setup: (host) => {
          host.removePath("tsconfig.json");
          materializeExtLib(host);
        },
      },
      async (host) => {
        await host.typecheck("packages/web/tsconfig.json");
        await host.typecheck("packages/node/tsconfig.json");

        let webRef: SyntaxRef | undefined;

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            projects: [
              "packages/web/tsconfig.json",
              "packages/node/tsconfig.json",
              "packages/jsapp/jsconfig.json",
            ],
            rules: [
              defineRule({
                id: "capture-shared-call",
                run: async (ctx) => {
                  const calls = await ctx.findSyntax({
                    files: ["packages/shared/shared.ts"],
                    kinds: ["CallExpression"],
                  });
                  const first = calls[0];
                  if (!first) return;
                  if (ctx.project.configFile === "packages/web/tsconfig.json") {
                    webRef = first.ref;
                  }
                  ctx.report({
                    at: first.location,
                    message: "Cross-entry shared finding",
                    findingKey: "cross-entry",
                    basis: [{ kind: "note", message: "Same evidence" }],
                  });
                },
              }),
            ],
          }),
        });

        assert.equal(report.projects.length, 3);
        for (const proj of report.projects) {
          assert.equal(proj.entryConfig, proj.configFile);
          assert.deepEqual(proj.entryChain, [proj.configFile]);
        }

        assert.equal(report.diagnostics.length, 1);
        assert.deepEqual(report.diagnostics[0]?.projects, [
          "packages/node/tsconfig.json",
          "packages/web/tsconfig.json",
        ]);

        assert.ok(webRef);
        await assert.rejects(
          () =>
            runCheck({
              cwd: host.rootDir,
              config: defineConfig({
                projects: ["packages/node/tsconfig.json"],
                rules: [
                  defineRule({
                    id: "cross-project-ref-check",
                    run: async (ctx) => {
                      await ctx.typeAt([webRef!]);
                    },
                  }),
                ],
              }),
            }),
          (err: unknown) =>
            err instanceof VeruleExecutionError &&
            (err.code === "STALE_REFERENCE" ||
              err.code === "CROSS_PROJECT_REFERENCE"),
        );

        // Also test CROSS_PROJECT_REFERENCE within the same active session.
        await assert.rejects(
          () =>
            runCheck({
              cwd: host.rootDir,
              config: defineConfig({
                projects: [
                  "packages/web/tsconfig.json",
                  "packages/node/tsconfig.json",
                ],
                rules: [
                  defineRule({
                    id: "same-session-cross-project",
                    run: async (ctx) => {
                      const calls = await ctx.findSyntax({
                        files: ["packages/shared/shared.ts"],
                        kinds: ["CallExpression"],
                      });
                      if (
                        ctx.project.configFile === "packages/web/tsconfig.json"
                      ) {
                        webRef = calls[0]?.ref;
                        return;
                      }
                      if (
                        ctx.project.configFile ===
                          "packages/node/tsconfig.json" &&
                        webRef
                      ) {
                        await ctx.typeAt([webRef]);
                      }
                    },
                  }),
                ],
              }),
            }),
          (err: unknown) =>
            err instanceof VeruleExecutionError &&
            err.code === "CROSS_PROJECT_REFERENCE",
        );
      },
    );
  });
});
