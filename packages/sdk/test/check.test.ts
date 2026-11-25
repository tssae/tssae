import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  REPORT_SCHEMA_VERSION,
  VeruleExecutionError,
  defineConfig,
  defineRule,
  openSession,
  runCheck,
  type SyntaxRef,
} from "@verule/sdk";
import { withTempHost } from "@verule/test-hosts";

void describe("@verule/sdk check execution", () => {
  void it("defaults to zero rules and omits observation details when observe is off", async () => {
    await withTempHost({ host: "sdk/check-basic" }, async (host) => {
      await host.typecheck();

      const config = defineConfig({ tsconfig: "tsconfig.json" });
      assert.deepEqual(config.rules, []);

      const report = await runCheck({ cwd: host.rootDir, config });
      assert.equal(report.reportVersion, REPORT_SCHEMA_VERSION);
      assert.equal(report.status, "clean");
      assert.deepEqual(report.diagnostics, []);
      assert.deepEqual(report.gaps, []);
      assert.equal("observation" in report, false);
      assert.equal("metrics" in report, false);
      assert.equal("debug" in report, false);
    });
  });

  void it("shares one AnalysisSession across two rules and reports narrowed-type diagnostics", async () => {
    await withTempHost({ host: "sdk/check-basic" }, async (host) => {
      await host.typecheck();

      // Merge the label and return onto one line in the disposable copy to
      // exercise UTF-16 column positioning after surrogate-pair characters.
      const sourceText = host.replaceText(
        "src/handlers.ts",
        '    const _label = "中文😀";\n    return sink(mode, input);',
        '    const _label = "中文😀"; return sink(mode, input);',
      );
      let capturedRef: SyntaxRef | undefined;

      const narrowedStringArgRule = defineRule({
        id: "rule-narrowed-string-arg",
        run: async (ctx) => {
          const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
          const sinkCalls = calls.filter((call) =>
            call.text.startsWith("sink("),
          );
          const secondArgs = sinkCalls
            .map((call) => ({ call, arg: call.arguments?.[1] }))
            .filter(
              (
                entry,
              ): entry is {
                call: (typeof sinkCalls)[number];
                arg: SyntaxRef;
              } => entry.arg !== undefined,
            );
          if (secondArgs.length === 0) return;

          capturedRef = secondArgs[0]!.arg;
          const types = await ctx.typeAt(secondArgs.map((entry) => entry.arg));
          for (const [index, typeInfo] of types.entries()) {
            if (typeInfo.typeText === "string") {
              const targetCall = secondArgs[index]!.call;
              ctx.report({
                at: targetCall.location,
                message: "sink() received a narrowed string value",
                findingKey: "narrowed-string",
                basis: [
                  {
                    kind: "type",
                    message: `Second argument narrowed to ${typeInfo.typeText}`,
                    at: targetCall.location,
                  },
                ],
              });
            }
          }
        },
      });

      const fastModeLiteralRule = defineRule({
        id: "rule-fast-mode-literal",
        run: async (ctx) => {
          const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
          const sinkCalls = calls.filter((call) =>
            call.text.startsWith("sink("),
          );
          const firstArgs = sinkCalls
            .map((call) => ({ call, arg: call.arguments?.[0] }))
            .filter(
              (
                entry,
              ): entry is {
                call: (typeof sinkCalls)[number];
                arg: SyntaxRef;
              } => entry.arg !== undefined,
            );
          if (firstArgs.length === 0) return;

          const types = await ctx.typeAt(firstArgs.map((entry) => entry.arg));
          for (const [index, typeInfo] of types.entries()) {
            if (typeInfo.isLiteral && typeInfo.literalValue === "fast") {
              const targetCall = firstArgs[index]!.call;
              ctx.report({
                at: targetCall.location,
                message: 'sink() called with narrowed literal mode "fast"',
                findingKey: "fast-mode",
                basis: [
                  {
                    kind: "type",
                    message: `First argument narrowed to ${typeInfo.typeText}`,
                    at: targetCall.location,
                  },
                ],
              });
            }
          }
        },
      });

      const config = defineConfig({
        tsconfig: "tsconfig.json",
        rules: [narrowedStringArgRule, fastModeLiteralRule],
      });

      const session = await openSession({ cwd: host.rootDir, config });
      let report1;
      try {
        assert.equal(session.projects.length, 1);
        report1 = await session.check();
      } finally {
        await session.close();
      }

      assert.equal(report1.status, "findings");
      assert.equal(report1.diagnostics.length, 2);

      const firstDiag = report1.diagnostics.find(
        (d) => d.ruleId === "rule-narrowed-string-arg",
      );
      assert.ok(firstDiag, "expected diagnostic from rule-narrowed-string-arg");
      assert.equal(firstDiag.at.file, "src/handlers.ts");
      assert.equal(firstDiag.at.line, 10);
      assert.equal(firstDiag.at.column, 35);
      assert.equal(
        Buffer.from(sourceText, "utf8")
          .subarray(firstDiag.at.start, firstDiag.at.end)
          .toString("utf8"),
        "sink(mode, input)",
      );
      assert.equal(firstDiag.evidence.length, 1);

      const secondDiag = report1.diagnostics.find(
        (d) => d.ruleId === "rule-fast-mode-literal",
      );
      assert.ok(secondDiag, "expected diagnostic from rule-fast-mode-literal");
      assert.equal(secondDiag.at.file, "src/handlers.ts");
      assert.equal(secondDiag.at.line, 13);

      // Re-running via runCheck yields identical stable IDs, locations, and evidence.
      const report2 = await runCheck({ cwd: host.rootDir, config });
      assert.deepEqual(report2, report1);

      // Stale SyntaxRef from closed session must fail execution if used in a new session.
      assert.ok(capturedRef);
      await assert.rejects(
        () =>
          runCheck({
            cwd: host.rootDir,
            config: defineConfig({
              tsconfig: "tsconfig.json",
              rules: [
                defineRule({
                  id: "stale-ref-rule",
                  run: async (ctx) => {
                    await ctx.typeAt([capturedRef!]);
                  },
                }),
              ],
            }),
          }),
        (err: unknown) =>
          err instanceof VeruleExecutionError && err.code === "STALE_REFERENCE",
      );
    });
  });

  void it("fails with VeruleExecutionError on missing tsconfig, invalid tsconfig, and throwing rules", async () => {
    await withTempHost({}, async (emptyHost) => {
      await assert.rejects(
        () => runCheck({ cwd: emptyHost.rootDir }),
        (err: unknown) =>
          err instanceof VeruleExecutionError &&
          err.code === "PROJECT_LOAD_ERROR",
      );
    });

    await withTempHost(
      {
        host: "sdk/check-basic",
        files: { "tsconfig.json": "{ broken json" },
      },
      async (brokenHost) => {
        await assert.rejects(
          () => runCheck({ cwd: brokenHost.rootDir }),
          (err: unknown) =>
            err instanceof VeruleExecutionError &&
            err.code === "PROJECT_LOAD_ERROR",
        );
      },
    );

    await withTempHost({ host: "sdk/check-basic" }, async (validHost) => {
      await assert.rejects(
        () =>
          runCheck({
            cwd: validHost.rootDir,
            config: defineConfig({
              tsconfig: "tsconfig.json",
              rules: [
                defineRule({
                  id: "boom-rule",
                  run: () => {
                    throw new Error("rule exploded");
                  },
                }),
              ],
            }),
          }),
        (err: unknown) =>
          err instanceof VeruleExecutionError &&
          err.code === "RULE_EXECUTION_ERROR" &&
          /boom-rule/u.test(err.message),
      );
    });
  });
});
