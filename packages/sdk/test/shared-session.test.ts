import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { performance } from "node:perf_hooks";
import { describe, it } from "node:test";

import {
  VeruleExecutionError,
  defineConfig,
  defineModel,
  defineRule,
  requestSharedReport,
  runCheck,
  startSharedSession,
  type CheckReport,
  type Rule,
  type SyntaxRef,
} from "@verule/sdk";
import { withTempHost } from "@verule/test-hosts";

function createModeConfig(disallowedMode: string, version = "1") {
  const model = defineModel({
    id: "mode-policy",
    version,
    facts: { disallowedMode },
  });
  const rule: Rule = defineRule({
    id: "disallow-mode",
    run: async (ctx) => {
      const policy = ctx.getModel("mode-policy");
      const forbidden =
        typeof policy?.facts?.disallowedMode === "string"
          ? policy.facts.disallowedMode
          : "fast";
      const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
      const sinks = calls.filter((call) => call.text.startsWith("sink("));
      const args = sinks
        .map((call) => ({ call, arg: call.arguments?.[0] }))
        .filter(
          (entry): entry is { call: (typeof sinks)[number]; arg: SyntaxRef } =>
            entry.arg !== undefined,
        );
      if (args.length === 0) return;

      const types = await ctx.typeAt(args.map((entry) => entry.arg));
      for (const [index, ty] of types.entries()) {
        if (ty.isLiteral && ty.literalValue === forbidden) {
          const targetCall = args[index]!.call;
          ctx.report({
            at: targetCall.location,
            message: `Disallowed mode "${forbidden}" (v${policy?.version ?? "0"})`,
            findingKey: `mode:${forbidden}`,
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
  return defineConfig({
    tsconfig: "tsconfig.json",
    models: [model],
    rules: [rule],
  });
}

function assertReportMatchesCold(
  actual: CheckReport,
  expected: CheckReport,
  label: string,
): void {
  assert.deepEqual(
    {
      reportVersion: actual.reportVersion,
      status: actual.status,
      projects: actual.projects,
      fileOrigins: actual.fileOrigins,
      diagnostics: actual.diagnostics,
      gaps: actual.gaps,
      identity: actual.identity,
    },
    {
      reportVersion: expected.reportVersion,
      status: expected.status,
      projects: expected.projects,
      fileOrigins: expected.fileOrigins,
      diagnostics: expected.diagnostics,
      gaps: expected.gaps,
      identity: expected.identity,
    },
    `Shared report diverged from cold runCheck at "${label}"`,
  );
}

void describe("@verule/sdk shared session", () => {
  void it("serves immediately saved edits without watcher race, recovers from failures, and cleans up on close", async (t) => {
    await withTempHost(
      { host: "sdk-projects/shared-session" },
      async (host) => {
        await host.typecheck();

        await withTempHost(
          { prefix: "verule-sdk-sock-" },
          async (socketHost) => {
            const socketPath = socketHost.resolvePath("session.sock");
            const configFilePath = host.resolvePath("verule.config.mjs");
            const validTsconfig = host.readText("tsconfig.json");

            let currentConfig = createModeConfig("fast", "1");
            let generation = 1;
            const controller = new AbortController();
            const sharedSession = await startSharedSession({
              cwd: host.rootDir,
              endpoint: socketPath,
              watchFiles: [configFilePath],
              signal: controller.signal,
              loadConfig: async () => {
                const text = await import(
                  `${new URL(`file://${configFilePath}`).href}?g=${generation++}`
                );
                const raw = text.default as {
                  readonly mode: string;
                  readonly version: string;
                  readonly shouldThrow: boolean;
                };
                currentConfig = raw.shouldThrow
                  ? defineConfig({
                      tsconfig: "tsconfig.json",
                      rules: [
                        defineRule({
                          id: "broken-rule",
                          run: () => {
                            throw new Error("rule boom");
                          },
                        }),
                      ],
                    })
                  : createModeConfig(raw.mode, raw.version);
                return {
                  config: currentConfig,
                  watchFiles: [configFilePath],
                };
              },
            });

            try {
              assert.equal(existsSync(socketPath), true);
              assert.equal(sharedSession.closed, false);

              // 1. Initial clean report
              const t1 = performance.now();
              const report1 = await requestSharedReport({
                endpoint: socketPath,
              });
              t.diagnostic(
                `SDK shared query (initial clean): ${(performance.now() - t1).toFixed(2)}ms`,
              );
              assert.equal(report1.status, "clean");
              assertReportMatchesCold(
                report1,
                await runCheck({ cwd: host.rootDir, config: currentConfig }),
                "initial-clean",
              );

              // 2. Introduce violation and query immediately (0ms sleep)
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
              const t2 = performance.now();
              const report2 = await requestSharedReport({
                endpoint: socketPath,
              });
              t.diagnostic(
                `SDK shared query (immediate after violation): ${(performance.now() - t2).toFixed(2)}ms`,
              );
              assert.equal(report2.status, "findings");
              assert.equal(report2.diagnostics.length, 1);
              assertReportMatchesCold(
                report2,
                await runCheck({ cwd: host.rootDir, config: currentConfig }),
                "introduce-violation",
              );

              // 3. Fix violation and query immediately -> retracted
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
              const t3 = performance.now();
              const report3 = await requestSharedReport({
                endpoint: socketPath,
              });
              t.diagnostic(
                `SDK shared query (immediate after fix): ${(performance.now() - t3).toFixed(2)}ms`,
              );
              assert.equal(report3.status, "clean");
              assertReportMatchesCold(
                report3,
                await runCheck({ cwd: host.rootDir, config: currentConfig }),
                "fix-violation",
              );

              // 4. Create brand new file with violation and query immediately
              host.writeText(
                "src/added.ts",
                [
                  "function sink(mode: string): string { return mode; }",
                  'export const added = sink("fast");',
                  "",
                ].join("\n"),
              );
              const report4 = await requestSharedReport({
                endpoint: socketPath,
              });
              assert.equal(report4.status, "findings");
              assert.equal(report4.diagnostics[0]?.at.file, "src/added.ts");
              assertReportMatchesCold(
                report4,
                await runCheck({ cwd: host.rootDir, config: currentConfig }),
                "new-file",
              );

              // 5. Rule failure -> client gets RULE_EXECUTION_ERROR, session stays alive and recovers
              host.writeText(
                "verule.config.mjs",
                'export default { mode: "fast", version: "1", shouldThrow: true };\n',
              );
              await assert.rejects(
                () => requestSharedReport({ endpoint: socketPath }),
                (err: unknown) =>
                  err instanceof VeruleExecutionError &&
                  err.code === "RULE_EXECUTION_ERROR",
              );

              // Recover immediately after fixing rule config
              host.writeText(
                "verule.config.mjs",
                'export default { mode: "safe", version: "2", shouldThrow: false };\n',
              );
              const report5 = await requestSharedReport({
                endpoint: socketPath,
              });
              assert.equal(report5.status, "clean");
              assert.equal(report5.identity.models[0]?.version, "2");
              assertReportMatchesCold(
                report5,
                await runCheck({ cwd: host.rootDir, config: currentConfig }),
                "recover-after-rule-error",
              );

              // 6. Break tsconfig.json -> PROJECT_LOAD_ERROR, recover immediately when fixed
              host.writeText("tsconfig.json", "{ invalid json");
              await assert.rejects(
                () => requestSharedReport({ endpoint: socketPath }),
                (err: unknown) =>
                  err instanceof VeruleExecutionError &&
                  err.code === "PROJECT_LOAD_ERROR",
              );
              host.writeText("tsconfig.json", validTsconfig);
              const report6 = await requestSharedReport({
                endpoint: socketPath,
              });
              assertReportMatchesCold(
                report6,
                await runCheck({ cwd: host.rootDir, config: currentConfig }),
                "recover-after-tsconfig-error",
              );

              // 7. Close shared session -> socket removed, subsequent client fails, standalone runCheck still works
              await sharedSession.close();
              await sharedSession.waitUntilClosed();
              assert.equal(sharedSession.closed, true);
              assert.equal(existsSync(socketPath), false);

              await assert.rejects(
                () => requestSharedReport({ endpoint: socketPath }),
                (err: unknown) =>
                  err instanceof VeruleExecutionError &&
                  err.code === "ENGINE_ERROR",
              );

              const postCloseCold = await runCheck({
                cwd: host.rootDir,
                config: currentConfig,
              });
              assertReportMatchesCold(
                report6,
                postCloseCold,
                "post-close-standalone",
              );
            } finally {
              controller.abort();
              await sharedSession.close();
            }
          },
        );
      },
    );
  });
});
