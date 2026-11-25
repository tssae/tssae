import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  INLINE_IGNORE_RULE_ID,
  defineConfig,
  defineRule,
  openSession,
  runCheck,
} from "@verule/sdk";
import { withTempHost } from "@verule/test-hosts";

void describe("@verule/sdk inline ignore directives", () => {
  void it("suppresses a single primary location, explains why, flags unused after fix, and preserves evidence and gaps", async () => {
    await withTempHost(
      { host: "sdk-projects/inline-ignore-basic" },
      async (host) => {
        await host.typecheck();

        const noDangerousRule = defineRule({
          id: "no-dangerous",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
            for (const call of calls) {
              if (call.text.startsWith("dangerous(")) {
                ctx.report({
                  at: call.location,
                  message: "Avoid dangerous() calls",
                  findingKey: "dangerous-call",
                });
              }
            }
            // Also exercise branchCoverage so a gap is produced inside the same function.
            const fns = await ctx.findSyntax({
              kinds: ["FunctionDeclaration"],
            });
            const runFn = fns.find((fn) =>
              fn.text.includes("export function run"),
            );
            if (runFn) {
              const helperCall = calls.find((c) =>
                c.text.startsWith("helper("),
              );
              if (helperCall) {
                await ctx.branchCoverage({
                  function: runFn.ref,
                  branches: [],
                  targets: [helperCall.ref],
                });
              }
            }
          },
        });

        const consumerRule = defineRule({
          id: "no-unannotated-consumer",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
            const dangerousCall = calls.find((c) =>
              c.text.startsWith("dangerous("),
            );
            const invokeCall = calls.find((c) => c.text.startsWith("run("));
            if (invokeCall && dangerousCall) {
              ctx.report({
                at: invokeCall.location,
                message: "Consumer calls function containing dangerous()",
                basis: [
                  {
                    kind: "source",
                    message: "Transitive dangerous() call",
                    at: dangerousCall.location,
                  },
                ],
              });
            }
          },
        });

        const config = defineConfig({
          tsconfig: "tsconfig.json",
          rules: [noDangerousRule, consumerRule],
        });

        const session = await openSession({ cwd: host.rootDir, config });
        try {
          // 1. Violation on line 6 is suppressed; cross-file evidence pointing to line 6 and gaps are preserved.
          const initial = await session.check();
          assert.equal(initial.status, "findings");
          assert.equal(
            initial.diagnostics.some((d) => d.ruleId === "no-dangerous"),
            false,
          );
          const consumerDiag = initial.diagnostics.find(
            (d) => d.ruleId === "no-unannotated-consumer",
          );
          assert.ok(consumerDiag, "consumer diagnostic should remain");
          assert.equal(consumerDiag.evidence[0]?.at?.file, "src/app.ts");
          assert.equal(consumerDiag.evidence[0]?.at?.line, 6);
          assert.ok(
            initial.gaps.some((g) => g.kind === "unmodeled-try-catch"),
            "gaps must not be deleted by inline ignore",
          );

          // 2. Explain why no-dangerous did not report at line 6.
          const explained = await session.explain({
            ruleId: "no-dangerous",
            at: "src/app.ts:6:3",
          });
          assert.equal(explained.explanation.status, "suppressed_by_ignore");
          assert.equal(explained.explanation.inlineIgnore?.matched, true);
          assert.equal(
            explained.explanation.inlineIgnore?.reason,
            "Legacy path kept for compatibility.",
          );
          assert.equal(
            explained.explanation.inlineIgnore?.commentAt?.file,
            "src/app.ts",
          );
          assert.equal(explained.explanation.inlineIgnore?.commentAt?.line, 5);

          // 3. Fix the violation while keeping the ignore directive -> reported as unused at line 4.
          host.writeText(
            "src/app.ts",
            [
              "declare function safeCall(label: string): void;",
              "",
              "export function run(_flag: boolean): void {",
              "  // verule-ignore-next-code-line no-dangerous -- Legacy path kept for compatibility.",
              '  safeCall("updated");',
              "}",
              "",
            ].join("\n"),
          );
          await session.update({ changed: ["src/app.ts"] });

          const afterFix = await session.check();
          assert.equal(afterFix.status, "findings");
          assert.equal(afterFix.diagnostics.length, 1);
          const unusedDiag = afterFix.diagnostics[0]!;
          assert.equal(unusedDiag.ruleId, INLINE_IGNORE_RULE_ID);
          assert.equal(unusedDiag.at.file, "src/app.ts");
          assert.equal(unusedDiag.at.line, 4);
          assert.match(unusedDiag.message, /unused/iu);

          // 4. Restore violation -> unused diagnostic disappears and suppression resumes cleanly.
          host.writeText(
            "src/app.ts",
            [
              "declare function dangerous(label: string): void;",
              "",
              "export function run(_flag: boolean): void {",
              "  // verule-ignore-next-code-line no-dangerous -- Legacy path kept for compatibility.",
              '  dangerous("legacy");',
              "}",
              "",
            ].join("\n"),
          );
          await session.update({ changed: ["src/app.ts"] });

          const restored = await session.check({ ruleIds: ["no-dangerous"] });
          assert.equal(restored.status, "clean");
          assert.deepEqual(restored.diagnostics, []);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("counts unique (file, start, end) ranges across multiple diagnostics and projects, and flags ambiguity when >= 2 ranges match", async () => {
    await withTempHost(
      { host: "sdk-projects/inline-ignore-multi-project" },
      async (host) => {
        await host.typecheck("packages/web/tsconfig.json");
        await host.typecheck("packages/node/tsconfig.json");

        const noRegisterRule = defineRule({
          id: "no-register",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
            for (const call of calls) {
              if (!call.text.startsWith("register(")) continue;
              const arg0 = call.arguments?.[0];
              const [argType] = arg0 ? await ctx.typeAt([arg0]) : [];
              if (call.text.includes('"single"')) {
                // Emit two distinct diagnostics on the exact same primary location AND divergent project-specific messages.
                ctx.report({
                  at: call.location,
                  message: `register() disallowed in ${ctx.project.projectId} (finding 1)`,
                  findingKey: "register-1",
                  basis: [
                    {
                      kind: "note",
                      message: `Project ${ctx.project.projectId}`,
                      at: call.location,
                    },
                  ],
                });
                ctx.report({
                  at: call.location,
                  message: `register() disallowed (finding 2: ${argType?.typeText ?? ""})`,
                  findingKey: "register-2",
                });
              } else if (arg0) {
                // For "first" and "second", report at each string argument's distinct span on the same line.
                const strings = await ctx.findSyntax({
                  kinds: ["StringLiteral"],
                  range: {
                    file: call.location.file,
                    start: call.location.start,
                    end: call.location.end,
                    match: "within",
                  },
                });
                for (const str of strings) {
                  ctx.report({
                    at: str.location,
                    message: `Forbidden registration key ${str.text}`,
                    findingKey: `key:${str.text}`,
                  });
                }
              }
            }
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [noRegisterRule],
          }),
        });

        assert.equal(report.projects.length, 3);
        assert.equal(report.status, "findings");

        // Line 6 ("single"): both findings across both projects share 1 (file, start, end) -> all suppressed.
        assert.equal(
          report.diagnostics.some(
            (d) => d.ruleId === "no-register" && d.at.line === 6,
          ),
          false,
        );

        // Line 9 ("first" and "second"): 2 distinct primary locations -> ambiguous diagnostic + both original diagnostics kept.
        const line9Originals = report.diagnostics.filter(
          (d) => d.ruleId === "no-register" && d.at.line === 9,
        );
        assert.equal(line9Originals.length, 2);

        const ambiguousDiag = report.diagnostics.find(
          (d) => d.ruleId === INLINE_IGNORE_RULE_ID,
        );
        assert.ok(ambiguousDiag, "expected ambiguous directive diagnostic");
        assert.equal(ambiguousDiag.at.file, "packages/shared/shared.ts");
        assert.equal(ambiguousDiag.at.line, 8);
        assert.match(ambiguousDiag.message, /ambiguous/iu);
        assert.equal(ambiguousDiag.evidence.length, 2);
        assert.deepEqual(
          ambiguousDiag.evidence.map((e) => e.at?.line),
          [9, 9],
        );
        assert.notEqual(
          ambiguousDiag.evidence[0]?.at?.start,
          ambiguousDiag.evidence[1]?.at?.start,
        );

        assert.equal(report.inlineIgnores?.length, 2);
        assert.equal(report.inlineIgnores?.[0]?.status, "suppressed");
        assert.equal(report.inlineIgnores?.[1]?.status, "ambiguous");
        assert.equal(report.inlineIgnores?.[1]?.matchedLocations?.length, 2);
      },
    );
  });

  void it("handles stacked directives, multi-line block comments, and standalone blank-line truncation", async () => {
    await withTempHost(
      { host: "sdk-projects/inline-ignore-stacked" },
      async (host) => {
        await host.typecheck();

        const ruleA = defineRule({
          id: "rule-a",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
            for (const call of calls) {
              if (call.text === "ruleACall()") {
                ctx.report({ at: call.location, message: "rule-a violation" });
              }
            }
          },
        });
        const ruleB = defineRule({
          id: "rule-b",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
            for (const call of calls) {
              if (call.text === "ruleBCall()") {
                ctx.report({ at: call.location, message: "rule-b violation" });
              }
            }
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [ruleA, ruleB],
          }),
        });

        assert.equal(report.status, "findings");
        // Line 12 suppressed for rule-a; lines 25 and 26 still report rule-a.
        const ruleADiags = report.diagnostics.filter(
          (d) => d.ruleId === "rule-a",
        );
        assert.deepEqual(
          ruleADiags.map((d) => d.at.line),
          [25, 26],
        );

        // Unused directives: line 6 (rule-b in stackedPartial), line 16 & 17 (stackedBothUnused), line 23 (blankLineBreaksMatch).
        const unusedDiags = report.diagnostics.filter(
          (d) => d.ruleId === INLINE_IGNORE_RULE_ID,
        );
        assert.deepEqual(
          unusedDiags.map((d) => d.at.line),
          [6, 16, 17, 23],
        );
      },
    );
  });

  void it("supports JSX child directives and ignores directive text in strings, templates, JSDoc, and non-standalone comments", async () => {
    await withTempHost(
      { host: "sdk-projects/inline-ignore-jsx" },
      async (host) => {
        await host.typecheck();

        const noBadWidget = defineRule({
          id: "no-bad-widget",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
            for (const call of calls) {
              if (call.text.startsWith("BadWidget(")) {
                ctx.report({ at: call.location, message: "Avoid BadWidget" });
              }
            }
            // Check JSX elements by identifier/call/range via findSyntax Identifier
            const ids = await ctx.findSyntax({ kinds: ["Identifier"] });
            for (const id of ids) {
              if (id.text === "BadWidget") {
                // Only flag JSX opening tags (not the declare function or CallExpression callee)
                const lineStartCall = calls.some(
                  (c) => c.location.start === id.location.start,
                );
                if (!lineStartCall && id.location.line > 5) {
                  ctx.report({
                    at: id.location,
                    message: "Avoid <BadWidget>",
                  });
                }
              }
            }
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [noBadWidget],
          }),
        });

        assert.equal(report.status, "findings");
        // Line 15 (template interpolation) and line 21 (JSX child) are suppressed.
        // Line 9 (export function View after JSDoc), line 25 (after blank line), line 32 (trailing), line 33 (inline) remain reported (or clean for line 9).
        const ruleDiags = report.diagnostics.filter(
          (d) => d.ruleId === "no-bad-widget",
        );
        assert.deepEqual(
          ruleDiags.map((d) => d.at.line),
          [25, 32, 33],
        );

        // Only the JSX directive at line 22-23 (truncated by blank line on line 24) is reported unused.
        // Strings, templates, JSDoc (lines 4-8), trailing (line 32), and inline (line 33) do NOT trigger unused/malformed!
        const inlineDiags = report.diagnostics.filter(
          (d) => d.ruleId === INLINE_IGNORE_RULE_ID,
        );
        assert.deepEqual(
          inlineDiags.map((d) => d.at.line),
          [22],
        );
        assert.equal(report.inlineIgnores?.length, 3);
        assert.deepEqual(
          report.inlineIgnores?.map((e) => ({
            status: e.status,
            line: e.at.line,
          })),
          [
            { status: "suppressed", line: 14 },
            { status: "suppressed", line: 20 },
            { status: "unused", line: 22 },
          ],
        );
      },
    );
  });

  void it("reports malformed and unknown rule IDs on full check, and keeps un-run directives undetermined under ruleIds", async () => {
    const ruleA = defineRule({
      id: "rule-a",
      run: async (ctx) => {
        const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
        for (const call of calls) {
          if (call.text === "bad()") {
            ctx.report({ at: call.location, message: "bad() violation" });
          }
        }
      },
    });
    const ruleOther = defineRule({
      id: "rule-other",
      run: () => {},
    });
    const ruleClean = defineRule({
      id: "rule-clean",
      run: () => {},
    });

    await withTempHost(
      { host: "sdk-projects/inline-ignore-cases" },
      async (host) => {
        await host.typecheck();

        // 1. Complete config check: reports malformed (lines 4, 6, 8, 10), unknown-rule (line 12), unused rule-other (line 14),
        //    and keeps all 5 bad() diagnostics (lines 5, 7, 9, 11, 13).
        const fullReport = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [ruleA, ruleOther],
          }),
        });
        assert.equal(fullReport.status, "findings");
        const ruleADiags = fullReport.diagnostics.filter(
          (d) => d.ruleId === "rule-a",
        );
        assert.deepEqual(
          ruleADiags.map((d) => d.at.line),
          [5, 7, 9, 11, 13],
        );
        const inlineDiags = fullReport.diagnostics.filter(
          (d) => d.ruleId === INLINE_IGNORE_RULE_ID,
        );
        assert.deepEqual(
          inlineDiags.map((d) => d.at.line),
          [4, 6, 8, 10, 12, 14],
        );
        assert.match(
          inlineDiags[4]!.message,
          /Unknown rule ID "unknown-rule"/u,
        );
      },
    );

    // 2. Partial check via ruleIds: running ONLY rule-clean on a clean file that has a valid directive for rule-a
    //    must NOT falsely report rule-a's directive as unused or unknown!
    await withTempHost(
      { host: "sdk-projects/inline-ignore-partial" },
      async (partialHost) => {
        await partialHost.typecheck();

        const partialReport = await runCheck({
          cwd: partialHost.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [ruleA, ruleClean],
          }),
          ruleIds: ["rule-clean"],
        });
        assert.equal(partialReport.status, "clean");
        assert.deepEqual(partialReport.diagnostics, []);
        assert.deepEqual(
          partialReport.inlineIgnores?.map((e) => e.status),
          ["undetermined", "undetermined"],
        );
      },
    );
  });
});
