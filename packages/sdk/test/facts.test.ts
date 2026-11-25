import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  VeruleExecutionError,
  defineAdapter,
  defineConfig,
  defineFactType,
  defineRule,
  openSession,
  runCheck,
  type FactResult,
  type SyntaxRef,
} from "@verule/sdk";
import { withTempHost } from "@verule/test-hosts";

type TagSetValue = {
  readonly tags: readonly string[];
  readonly forbidden?: readonly string[];
};

const TagFact = defineFactType<
  "syntax",
  { readonly scope?: string } | undefined,
  TagSetValue
>({
  id: "test.tags",
  subjectKind: "syntax",
  merge: (existing, incoming) => {
    const overlap = existing.value.tags.filter((tag) =>
      incoming.value.forbidden?.includes(tag),
    );
    const reverseOverlap = incoming.value.tags.filter((tag) =>
      existing.value.forbidden?.includes(tag),
    );
    if (overlap.length > 0 || reverseOverlap.length > 0) {
      return {
        ok: false,
        conflict: {
          message: `Conflicting tag conclusions for [${[...overlap, ...reverseOverlap].join(", ")}]`,
        },
      };
    }
    const mergedTags = [
      ...new Set([...existing.value.tags, ...incoming.value.tags]),
    ].toSorted();
    const mergedForbidden = [
      ...new Set([
        ...(existing.value.forbidden ?? []),
        ...(incoming.value.forbidden ?? []),
      ]),
    ].toSorted();
    return {
      ok: true,
      value: {
        tags: mergedTags,
        ...(mergedForbidden.length > 0 ? { forbidden: mergedForbidden } : {}),
      },
    };
  },
});

void describe("@verule/sdk facts and library adapters", () => {
  void it("runs baseline rules without adapters and performs zero extraction when adapters are unqueried", async () => {
    await withTempHost({ host: "sdk-queries/facts-basic" }, async (host) => {
      await host.typecheck();
      let extractCount = 0;

      const adapter = defineAdapter({
        id: "unused-adapter",
        provides: [
          {
            factType: TagFact,
            extract: () => {
              extractCount += 1;
              return [];
            },
          },
        ],
      });

      const baselineRule = defineRule({
        id: "rule-no-facts",
        run: async (ctx) => {
          const calls = await ctx.findSyntax({
            kinds: ["FunctionDeclaration"],
          });
          assert.equal(calls.length, 1);
        },
      });

      // Without adapters, ctx.facts returns [] cleanly
      let emptyFactResultCount = -1;
      const noAdapterRule = defineRule({
        id: "rule-empty-facts",
        run: async (ctx) => {
          const facts = await ctx.facts(TagFact);
          emptyFactResultCount = facts.length;
        },
      });
      const noAdapterReport = await runCheck({
        cwd: host.rootDir,
        config: defineConfig({
          tsconfig: "tsconfig.json",
          rules: [noAdapterRule],
        }),
      });
      assert.equal(noAdapterReport.status, "clean");
      assert.equal(emptyFactResultCount, 0);

      // With adapter enabled but never queried: extractCount === 0 & provider absent
      const unqueriedReport = await runCheck({
        cwd: host.rootDir,
        config: defineConfig({
          tsconfig: "tsconfig.json",
          rules: [baselineRule],
          adapters: [adapter],
        }),
        observe: "detail",
      });
      assert.equal(unqueriedReport.status, "clean");
      assert.equal(extractCount, 0);
      assert.equal(
        unqueriedReport.observation?.providers.includes("unused-adapter"),
        false,
      );
    });
  });

  void it("shares cached fact extraction across rules, records observation/explain, and invalidates on session.update()", async () => {
    await withTempHost({ host: "sdk-queries/facts-update" }, async (host) => {
      await host.typecheck();

      let extractCount = 0;
      const tagAdapter = defineAdapter({
        id: "alpha-provider",
        provides: [
          {
            factType: TagFact,
            extract: async (ctx) => {
              extractCount += 1;
              const fns = await ctx.findSyntax({
                kinds: ["FunctionDeclaration"],
              });
              return fns.map((fn) => ({
                subject: fn.ref,
                value: {
                  tags: [fn.text.includes("alpha") ? "v1-alpha" : "v1-beta"],
                },
                evidence: [
                  {
                    kind: "source",
                    message: `Tagged function at ${fn.location.file}:${fn.location.line}`,
                    at: fn.location,
                  },
                ],
              }));
            },
          },
        ],
      });

      let firstRuleFacts: readonly FactResult<"syntax", TagSetValue>[] = [];
      let secondRuleFacts: readonly FactResult<"syntax", TagSetValue>[] = [];

      const ruleA = defineRule({
        id: "rule-a",
        run: async (ctx) => {
          firstRuleFacts = await ctx.facts(TagFact);
        },
      });
      const ruleB = defineRule({
        id: "rule-b",
        run: async (ctx) => {
          secondRuleFacts = await ctx.facts(TagFact);
        },
      });

      const session = await openSession({
        cwd: host.rootDir,
        config: defineConfig({
          tsconfig: "tsconfig.json",
          rules: [ruleA, ruleB],
          adapters: [tagAdapter],
        }),
      });

      try {
        const report1 = await session.check({
          observe: "detail",
          at: "src/app.ts:1:1",
        });
        assert.equal(extractCount, 1);
        assert.equal(firstRuleFacts.length, 2);
        assert.equal(secondRuleFacts.length, 2);
        assert.equal(firstRuleFacts[0]!.snapshotId, session.snapshotId);
        assert.equal(
          firstRuleFacts[0]!.projectId,
          session.projects[0]!.projectId,
        );
        assert.deepEqual(firstRuleFacts[0]!.providers, ["alpha-provider"]);

        assert.ok(report1.observation);
        assert.ok(report1.observation.providers.includes("alpha-provider"));
        const detailEntries = report1.observation.details ?? [];
        const ruleAQuery = detailEntries.find(
          (entry) =>
            entry.kind === "query" &&
            entry.ruleId === "rule-a" &&
            entry.provider === "alpha-provider",
        );
        const ruleBQuery = detailEntries.find(
          (entry) =>
            entry.kind === "query" &&
            entry.ruleId === "rule-b" &&
            entry.provider === "alpha-provider",
        );
        assert.equal(ruleAQuery?.cacheStatus, "computed");
        assert.equal(ruleAQuery?.candidateCount, 2);
        assert.equal(ruleBQuery?.cacheStatus, "reused");
        assert.equal(ruleBQuery?.candidateCount, 2);

        const explainB = report1.explanations?.find(
          (e) => e.ruleId === "rule-b",
        );
        assert.ok(explainB);
        assert.ok(explainB.providers.includes("alpha-provider"));
        assert.ok(
          explainB.queries.some(
            (q) =>
              q.provider === "alpha-provider" &&
              q.cacheStatus === "reused" &&
              q.candidateCount === 2,
          ),
        );

        // Update file -> snapshot changes -> cache invalidates -> re-extracts
        const staleSubject: SyntaxRef = firstRuleFacts[0]!.subject;
        host.writeText(
          "src/app.ts",
          [
            "export function alpha() { return 10; }",
            "export function beta() { return 20; }",
            "export function gamma() { return 30; }",
            "",
          ].join("\n"),
        );
        await session.update({ changed: ["src/app.ts"] });

        await session.check();
        assert.equal(extractCount, 2);
        assert.equal(firstRuleFacts.length, 3);
        assert.equal(firstRuleFacts[0]!.snapshotId, session.snapshotId);

        // Using stale fact subject from old snapshot throws STALE_REFERENCE
        let staleError: unknown;
        const staleConsumerRule = defineRule({
          id: "rule-stale-subject",
          run: async (ctx) => {
            try {
              await ctx.syntaxOf([staleSubject], {
                select: { location: true },
              });
            } catch (err) {
              staleError = err;
              throw err;
            }
          },
        });
        await assert.rejects(
          () => session.check([staleConsumerRule]),
          (err: unknown) =>
            err instanceof VeruleExecutionError &&
            err.code === "STALE_REFERENCE",
        );
        assert.ok(staleError instanceof VeruleExecutionError);
      } finally {
        await session.close();
      }
    });
  });

  void it("supports providerId filtering, partial fact merging, and order-independent conflict detection", async () => {
    await withTempHost({ host: "sdk-queries/facts-merge" }, async (host) => {
      await host.typecheck();

      const providerPartialA = defineAdapter({
        id: "provider-a",
        provides: [
          {
            factType: TagFact,
            extract: async (ctx) => {
              const decls = await ctx.findSyntax({
                kinds: ["VariableDeclaration"],
              });
              return decls.map((decl) => ({
                subject: decl.ref,
                value: { tags: ["part-a"] },
                evidence: [
                  {
                    kind: "source",
                    message: "provider-a saw part-a",
                    at: decl.location,
                  },
                ],
              }));
            },
          },
        ],
      });

      const providerPartialB = defineAdapter({
        id: "provider-b",
        provides: [
          {
            factType: TagFact,
            extract: async (ctx) => {
              const decls = await ctx.findSyntax({
                kinds: ["VariableDeclaration"],
              });
              return decls.map((decl) => ({
                subject: decl.ref,
                value: { tags: ["part-b"] },
                evidence: [
                  {
                    kind: "source",
                    message: "provider-b saw part-b",
                    at: decl.location,
                  },
                ],
              }));
            },
          },
        ],
      });

      const providerConflictC = defineAdapter({
        id: "provider-c",
        provides: [
          {
            factType: TagFact,
            extract: async (ctx) => {
              const decls = await ctx.findSyntax({
                kinds: ["VariableDeclaration"],
              });
              return decls.map((decl) => ({
                subject: decl.ref,
                value: { tags: [], forbidden: ["part-a"] },
                evidence: [
                  {
                    kind: "source",
                    message: "provider-c forbids part-a",
                    at: decl.location,
                  },
                ],
              }));
            },
          },
        ],
      });

      let mergedResults: readonly FactResult<"syntax", TagSetValue>[] = [];
      let filteredResults: readonly FactResult<"syntax", TagSetValue>[] = [];

      const queryBothAndFilter = defineRule({
        id: "rule-merge-and-filter",
        run: async (ctx) => {
          mergedResults = await ctx.facts(TagFact);
          filteredResults = await ctx.facts(TagFact, {
            providerId: "provider-b",
          });
        },
      });

      await runCheck({
        cwd: host.rootDir,
        config: defineConfig({
          tsconfig: "tsconfig.json",
          rules: [queryBothAndFilter],
          adapters: [providerPartialB, providerPartialA],
        }),
      });
      assert.equal(mergedResults.length, 1);
      assert.deepEqual(mergedResults[0]!.value.tags, ["part-a", "part-b"]);
      assert.deepEqual(mergedResults[0]!.providers, [
        "provider-a",
        "provider-b",
      ]);
      assert.equal(filteredResults.length, 1);
      assert.deepEqual(filteredResults[0]!.value.tags, ["part-b"]);
      assert.deepEqual(filteredResults[0]!.providers, ["provider-b"]);

      const queryConflict = defineRule({
        id: "rule-conflict",
        run: async (ctx) => {
          await ctx.facts(TagFact);
        },
      });

      let errForward: unknown;
      try {
        await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [queryConflict],
            adapters: [providerPartialA, providerConflictC],
          }),
        });
      } catch (err) {
        errForward = err;
      }

      let errReverse: unknown;
      try {
        await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [queryConflict],
            adapters: [providerConflictC, providerPartialA],
          }),
        });
      } catch (err) {
        errReverse = err;
      }

      assert.ok(errForward instanceof VeruleExecutionError);
      assert.ok(errReverse instanceof VeruleExecutionError);
      assert.equal(errForward.code, "FACT_CONFLICT");
      assert.equal(errReverse.code, "FACT_CONFLICT");
      assert.equal(errForward.message, errReverse.message);
      assert.match(errForward.message, /provider-a/u);
      assert.match(errForward.message, /provider-c/u);
    });
  });
});
