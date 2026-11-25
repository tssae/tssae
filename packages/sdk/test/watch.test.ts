import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  VeruleExecutionError,
  defineConfig,
  defineModel,
  defineRule,
  openSession,
  runCheck,
  watchProject,
  type BranchRef,
  type CallRef,
  type DeclarationRef,
  type Model,
  type Rule,
  type SignatureRef,
  type SyntaxRef,
  type TypeRef,
} from "@verule/sdk";
import { withTempHost } from "@verule/test-hosts";

function createModeRulesAndModel(
  disallowedMode: string,
  modelVersion = "1",
): { readonly model: Model; readonly rules: readonly Rule[] } {
  const model = defineModel({
    id: "mode-policy",
    version: modelVersion,
    description: "Policy specifying which sink mode is disallowed.",
    facts: { disallowedMode },
  });

  const rule = defineRule({
    id: "disallow-configured-mode",
    run: async (ctx) => {
      const policy = ctx.getModel("mode-policy");
      const forbidden =
        typeof policy?.facts?.disallowedMode === "string"
          ? policy.facts.disallowedMode
          : "fast";

      const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
      const sinkCalls = calls.filter((call) => call.text.startsWith("sink("));
      const modeArgs = sinkCalls
        .map((call) => ({ call, arg: call.arguments?.[0] }))
        .filter(
          (
            entry,
          ): entry is { call: (typeof sinkCalls)[number]; arg: SyntaxRef } =>
            entry.arg !== undefined,
        );
      if (modeArgs.length === 0) return;

      const types = await ctx.typeAt(modeArgs.map((entry) => entry.arg));
      for (const [index, ty] of types.entries()) {
        if (ty.isLiteral && ty.literalValue === forbidden) {
          const targetCall = modeArgs[index]!.call;
          ctx.report({
            at: targetCall.location,
            message: `sink() called with disallowed mode "${forbidden}" (model v${policy?.version ?? "0"})`,
            findingKey: `disallowed-mode:${forbidden}`,
            basis: [
              {
                kind: "type",
                message: `Argument narrowed to ${ty.typeText}`,
                at: targetCall.location,
              },
              {
                kind: "note",
                message: `Model ${policy?.id ?? "default"}@${policy?.version ?? "0"}`,
              },
            ],
          });
        }
      }
    },
  });

  return { model, rules: [rule] };
}

void describe("@verule/sdk defineModel and updatable AnalysisSession", () => {
  void it("validates models and rejects duplicate model ids", () => {
    const valid = defineModel({
      id: "cleanup-model",
      version: "1.0.0",
      facts: { cleanupFns: ["dispose"] },
    });
    assert.equal(valid.id, "cleanup-model");

    assert.throws(
      () => defineModel({ id: "  " }),
      (err: unknown) =>
        err instanceof VeruleExecutionError && err.code === "INVALID_CONFIG",
    );

    assert.throws(
      () =>
        defineConfig({
          tsconfig: "tsconfig.json",
          models: [defineModel({ id: "dup" }), defineModel({ id: "dup" })],
        }),
      (err: unknown) =>
        err instanceof VeruleExecutionError && err.code === "INVALID_CONFIG",
    );
  });

  void it("updates snapshots incrementally, invalidates stale refs, recovers from failures, and matches cold runCheck on every step", async () => {
    await withTempHost({ host: "sdk-projects/watch-session" }, async (host) => {
      await host.typecheck("tsconfig.json");
      await host.typecheck("tsconfig.alt.json");

      const initialSetup = createModeRulesAndModel("fast", "1");
      let currentConfig = defineConfig({
        tsconfig: "tsconfig.json",
        models: [initialSetup.model],
        rules: initialSetup.rules,
      });

      const session = await openSession({
        cwd: host.rootDir,
        config: currentConfig,
      });
      try {
        // 1. Initial clean state
        const snap1 = session.snapshotId;
        const report1 = await session.check();
        assert.equal(report1.status, "clean");
        assert.deepEqual(
          report1,
          await runCheck({ cwd: host.rootDir, config: currentConfig }),
        );

        // Capture a SyntaxRef from snapshot 1 to verify stale-ref invalidation after update.
        let capturedRef: SyntaxRef | undefined;
        await session.check([
          defineRule({
            id: "capture-ref",
            run: async (ctx) => {
              const calls = await ctx.findSyntax({
                kinds: ["CallExpression"],
              });
              capturedRef = calls[0]?.ref;
            },
          }),
        ]);
        assert.ok(capturedRef, "expected captured SyntaxRef");

        // 2. Introduce violation directly in consumer.ts
        host.writeText(
          "src/consumer.ts",
          [
            'import { getMode } from "./shared.js";',
            "function sink(mode: string): string {",
            "  return mode;",
            "}",
            "export function runConsumer(): string {",
            '  void getMode(); return sink("fast");',
            "}",
            "",
          ].join("\n"),
        );
        const update2 = await session.update({
          changed: ["src/consumer.ts"],
        });
        assert.notEqual(update2.snapshotId, snap1);
        assert.ok(update2.memory.hostRssBytes > 0);
        if (update2.memory.engineRssBytes !== undefined) {
          assert.ok(update2.memory.engineRssBytes > 0);
        }

        // Old SyntaxRef must immediately fail with STALE_REFERENCE.
        await assert.rejects(
          () =>
            session.check([
              defineRule({
                id: "use-stale-ref",
                run: async (ctx) => {
                  await ctx.typeAt([capturedRef!]);
                },
              }),
            ]),
          (err: unknown) =>
            err instanceof VeruleExecutionError &&
            err.code === "STALE_REFERENCE",
        );

        const report2 = await session.check();
        assert.equal(report2.status, "findings");
        assert.equal(report2.diagnostics.length, 1);
        assert.deepEqual(
          report2,
          await runCheck({ cwd: host.rootDir, config: currentConfig }),
        );

        // 3. Fix violation (diagnostic retraction)
        host.writeText(
          "src/consumer.ts",
          [
            'import { getMode } from "./shared.js";',
            "function sink(mode: string): string {",
            "  return mode;",
            "}",
            "export function runConsumer(): string {",
            "  return sink(getMode());",
            "}",
            "",
          ].join("\n"),
        );
        await session.update({ changed: ["src/consumer.ts"] });
        const report3 = await session.check();
        assert.equal(report3.status, "clean");
        assert.deepEqual(
          report3,
          await runCheck({ cwd: host.rootDir, config: currentConfig }),
        );

        // 4. Modify shared definition in shared.ts -> affects consumer.ts via narrowed return type
        host.writeText(
          "src/shared.ts",
          'export function getMode(): "fast" {\n  return "fast";\n}\n',
        );
        const update4 = await session.update({ changed: ["src/shared.ts"] });
        assert.ok(
          update4.recomputation.recomputedFiles.includes("src/shared.ts"),
        );
        const report4 = await session.check();
        assert.equal(report4.status, "findings");
        assert.equal(report4.diagnostics.length, 1);
        assert.deepEqual(
          report4,
          await runCheck({ cwd: host.rootDir, config: currentConfig }),
        );

        // 5. Modify unrelated file -> consumer.ts and shared.ts are reused in tsrs
        host.writeText("src/unrelated.ts", "export const version = 2;\n");
        const update5 = await session.update({
          changed: ["src/unrelated.ts"],
        });
        assert.ok(
          update5.recomputation.recomputedFiles.includes("src/unrelated.ts"),
        );
        assert.ok(
          update5.recomputation.reusedFiles.includes("src/consumer.ts"),
        );
        assert.ok(update5.recomputation.reusedFiles.includes("src/shared.ts"));
        const report5 = await session.check();
        assert.deepEqual(
          report5,
          await runCheck({ cwd: host.rootDir, config: currentConfig }),
        );

        // 6. Failure and recovery: break tsconfig.json -> PROJECT_LOAD_ERROR, restore -> recovers
        const savedTsconfig = host.readText("tsconfig.json");
        host.writeText("tsconfig.json", "{ invalid json");
        await assert.rejects(
          () => session.update({ changed: ["tsconfig.json"] }),
          (err: unknown) =>
            err instanceof VeruleExecutionError &&
            err.code === "PROJECT_LOAD_ERROR",
        );
        host.writeText("tsconfig.json", savedTsconfig);
        await session.update({ changed: ["tsconfig.json"] });
        const report6 = await session.check();
        assert.deepEqual(
          report6,
          await runCheck({ cwd: host.rootDir, config: currentConfig }),
        );

        // 7a. Update model version/facts -> report identity & diagnostics change and match cold runCheck
        const updatedModelSetup = createModeRulesAndModel("safe", "2");
        currentConfig = defineConfig({
          tsconfig: "tsconfig.json",
          models: [updatedModelSetup.model],
          rules: updatedModelSetup.rules,
        });
        await session.update({ models: [updatedModelSetup.model] });
        const report7a = await session.check();
        assert.equal(report7a.status, "clean");
        assert.notEqual(
          report7a.identity.configDigest,
          report6.identity.configDigest,
        );
        assert.deepEqual(
          report7a,
          await runCheck({ cwd: host.rootDir, config: currentConfig }),
        );

        // 7b. Update rules -> report identity changes and matches cold runCheck
        const altRule = defineRule({
          id: "flag-any-sink",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({
              kinds: ["CallExpression"],
            });
            for (const call of calls) {
              if (call.text.startsWith("sink(")) {
                ctx.report({
                  at: call.location,
                  message: "sink() call detected by alt rule",
                });
              }
            }
          },
        });
        currentConfig = defineConfig({
          tsconfig: "tsconfig.json",
          models: [updatedModelSetup.model],
          rules: [altRule],
        });
        await session.update({ rules: [altRule] });
        const report7b = await session.check();
        assert.equal(report7b.status, "findings");
        assert.equal(report7b.diagnostics[0]?.ruleId, "flag-any-sink");
        assert.deepEqual(
          report7b,
          await runCheck({ cwd: host.rootDir, config: currentConfig }),
        );

        // 7c. Update project entries (switch to tsconfig.alt.json) -> matches cold runCheck
        const legacySetup = createModeRulesAndModel("legacy", "3");
        currentConfig = defineConfig({
          projects: ["tsconfig.alt.json"],
          models: [legacySetup.model],
          rules: legacySetup.rules,
        });
        await session.update({ config: currentConfig });
        const report7c = await session.check();
        assert.equal(report7c.status, "findings");
        assert.equal(report7c.diagnostics[0]?.at.file, "src/alt-only.ts");
        assert.deepEqual(
          report7c,
          await runCheck({ cwd: host.rootDir, config: currentConfig }),
        );
      } finally {
        await session.close();
      }
    });
  });

  void it("watches files on disk with watchProject, reuses unaffected projects, and shuts down on abort", async () => {
    await withTempHost(
      { host: "sdk-projects/watch-multi-package" },
      async (host) => {
        await host.typecheck("packages/a/tsconfig.json");
        await host.typecheck("packages/b/tsconfig.json");

        const setup = createModeRulesAndModel("fast", "1");
        const config = defineConfig({
          projects: ["packages/a/tsconfig.json", "packages/b/tsconfig.json"],
          models: [setup.model],
          rules: setup.rules,
        });

        const controller = new AbortController();
        const events = watchProject({
          cwd: host.rootDir,
          config,
          debounceMs: 25,
          signal: controller.signal,
        });

        try {
          const first = await events.next();
          assert.equal(first.done, false);
          assert.equal(first.value.kind, "report");
          assert.equal(first.value.report.status, "clean");
          assert.equal(first.value.report.projects.length, 2);

          // Edit only package B -> package A is reused in recomputation scope.
          host.replaceText(
            "packages/b/src/index.ts",
            'sink("safe")',
            'sink("fast")',
          );
          const second = await events.next();
          assert.equal(second.done, false);
          assert.equal(second.value.kind, "report");
          assert.equal(second.value.report.status, "findings");
          assert.equal(second.value.recomputation.reusedProjects.length, 1);
          assert.equal(second.value.recomputation.recomputedProjects.length, 1);
          assert.deepEqual(
            second.value.report,
            await runCheck({ cwd: host.rootDir, config }),
          );

          controller.abort();
          const ended = await events.next();
          assert.equal(ended.done, true);
        } finally {
          controller.abort();
          await events.return();
        }
      },
    );
  });

  void it("refreshes referenced multi-project fileOrigins and invalidates CallRef, SignatureRef, DeclarationRef, TypeRef, and BranchRef on update", async () => {
    await withTempHost(
      { host: "sdk-projects/watch-referenced-refs" },
      async (host) => {
        await host.typecheck("packages/sub/tsconfig.json");

        let oldCall: CallRef | undefined;
        let oldSig: SignatureRef | undefined;
        let oldDecl: DeclarationRef | undefined;
        let oldType: TypeRef | undefined;
        let oldBranch: BranchRef | undefined;
        let oldTargetSyntax: SyntaxRef | undefined;

        const config = defineConfig({ tsconfig: "tsconfig.json" });
        const session = await openSession({ cwd: host.rootDir, config });
        try {
          assert.equal(session.projects.length, 2);
          assert.deepEqual(
            session.fileOrigins.map((f) => f.file),
            ["packages/sub/src/mod.ts"],
          );

          await session.check([
            defineRule({
              id: "capture-all-ref-kinds",
              run: async (ctx) => {
                if (ctx.project.projectId !== "packages/sub/tsconfig.json") {
                  return;
                }
                const fns = await ctx.findSyntax({
                  kinds: ["FunctionDeclaration"],
                });
                const flowFn = fns.find((f) => f.text.includes("checkFlow"))!;
                const [branchesRes] = await ctx.listBranches([flowFn.ref]);
                oldBranch = branchesRes?.branches[0]?.ref;

                const calls = await ctx.findSyntax({
                  kinds: ["CallExpression"],
                });
                oldTargetSyntax = calls[0]!.ref;
                const [resolved] = await ctx.resolveCalls([oldTargetSyntax], {
                  select: {
                    calleeDeclaration: true,
                    signature: true,
                    returnType: true,
                  },
                });
                oldCall = resolved?.call ?? undefined;
                oldSig = resolved?.signature ?? undefined;
                oldDecl = resolved?.calleeDeclaration ?? undefined;
                oldType = resolved?.returnType?.ref;
              },
            }),
          ]);

          assert.ok(
            oldCall &&
              oldSig &&
              oldDecl &&
              oldType &&
              oldBranch &&
              oldTargetSyntax,
          );

          host.writeText(
            "packages/sub/src/extra.ts",
            "export const added = 1;\n",
          );
          const updateRes = await session.update({
            created: ["packages/sub/src/extra.ts"],
          });
          assert.deepEqual(
            updateRes.fileOrigins.map((f) => f.file),
            ["packages/sub/src/extra.ts", "packages/sub/src/mod.ts"],
          );
          assert.deepEqual(session.fileOrigins, updateRes.fileOrigins);

          for (const [name, runStale] of [
            [
              "CallRef",
              async (
                ctx: Parameters<Parameters<typeof defineRule>[0]["run"]>[0],
              ) => {
                await ctx.projectCalls([oldCall!], {
                  select: { location: true },
                });
              },
            ],
            [
              "SignatureRef",
              async (
                ctx: Parameters<Parameters<typeof defineRule>[0]["run"]>[0],
              ) => {
                await ctx.projectSignatures([oldSig!], {
                  select: { returnType: true },
                });
              },
            ],
            [
              "DeclarationRef",
              async (
                ctx: Parameters<Parameters<typeof defineRule>[0]["run"]>[0],
              ) => {
                await ctx.syntaxOf([oldDecl!], { select: { kind: true } });
              },
            ],
            [
              "TypeRef",
              async (
                ctx: Parameters<Parameters<typeof defineRule>[0]["run"]>[0],
              ) => {
                await ctx.projectTypes([oldType!], {
                  select: { typeText: true },
                });
              },
            ],
            [
              "BranchRef",
              async (
                ctx: Parameters<Parameters<typeof defineRule>[0]["run"]>[0],
              ) => {
                const calls = await ctx.findSyntax({
                  kinds: ["CallExpression"],
                });
                await ctx.branchCoverage({
                  branches: [oldBranch!],
                  targets: [calls[0]!.ref],
                });
              },
            ],
          ] as const) {
            await assert.rejects(
              () =>
                session.check([
                  defineRule({
                    id: `stale-${name}`,
                    run: async (ctx) => {
                      if (
                        ctx.project.projectId === "packages/sub/tsconfig.json"
                      ) {
                        await runStale(ctx);
                      }
                    },
                  }),
                ]),
              (err: unknown) =>
                err instanceof VeruleExecutionError &&
                err.code === "STALE_REFERENCE",
              `expected STALE_REFERENCE for ${name}`,
            );
          }

          assert.deepEqual(
            await session.check(),
            await runCheck({ cwd: host.rootDir, config }),
          );
        } finally {
          await session.close();
        }
      },
    );
  });
});
