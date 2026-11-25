import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@verule/test-hosts";
import {
  TssaeError,
  defineAdapter,
  defineFactType,
  openProject,
  type FactResult,
  type SyntaxRef,
} from "tssae";

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

void describe("tssae facts and library adapters", () => {
  void it("runs baseline queries without adapters and performs zero extraction when adapters are unqueried", async () => {
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

      // Without adapters, ctx.facts returns [] cleanly
      const noAdapterSession = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const emptyFacts = await noAdapterSession.project().facts(TagFact);
        assert.equal(emptyFacts.length, 0);
      } finally {
        await noAdapterSession.close();
      }

      // With adapter enabled but never queried: extractCount === 0
      const unqueriedSession = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: [adapter],
      });
      try {
        const decls = await unqueriedSession
          .project()
          .findSyntax({ kinds: ["FunctionDeclaration"] });
        assert.equal(decls.length, 1);
        assert.equal(extractCount, 0);
      } finally {
        await unqueriedSession.close();
      }
    });
  });

  void it("shares cached fact extraction within a snapshot and invalidates on session.update()", async () => {
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

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: [tagAdapter],
      });

      try {
        const ctx1 = session.project();
        const firstFacts: readonly FactResult<"syntax", TagSetValue>[] =
          await ctx1.facts(TagFact);
        const secondFacts: readonly FactResult<"syntax", TagSetValue>[] =
          await ctx1.facts(TagFact);

        assert.equal(extractCount, 1);
        assert.equal(firstFacts.length, 2);
        assert.equal(secondFacts.length, 2);
        assert.equal(firstFacts[0]!.snapshotId, session.snapshotId);
        assert.equal(firstFacts[0]!.projectId, session.projects[0]!.projectId);
        assert.deepEqual(firstFacts[0]!.providers, ["alpha-provider"]);
        assert.deepEqual(secondFacts, firstFacts);

        // Update file -> snapshot changes -> cache invalidates -> re-extracts
        const staleSubject: SyntaxRef = firstFacts[0]!.subject;
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

        const ctx2 = session.project();
        const updatedFacts = await ctx2.facts(TagFact);
        assert.equal(extractCount, 2);
        assert.equal(updatedFacts.length, 3);
        assert.equal(updatedFacts[0]!.snapshotId, session.snapshotId);

        // Using stale fact subject from old snapshot throws STALE_REFERENCE
        await assert.rejects(
          () =>
            ctx2.syntaxOf([staleSubject], {
              select: { location: true },
            }),
          (err: unknown) =>
            err instanceof TssaeError && err.code === "STALE_REFERENCE",
        );
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

      const mergeSession = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: [providerPartialB, providerPartialA],
      });
      try {
        const ctx = mergeSession.project();
        mergedResults = await ctx.facts(TagFact);
        filteredResults = await ctx.facts(TagFact, {
          providerId: "provider-b",
        });
      } finally {
        await mergeSession.close();
      }
      assert.equal(mergedResults.length, 1);
      assert.deepEqual(mergedResults[0]!.value.tags, ["part-a", "part-b"]);
      assert.deepEqual(mergedResults[0]!.providers, [
        "provider-a",
        "provider-b",
      ]);
      assert.equal(filteredResults.length, 1);
      assert.deepEqual(filteredResults[0]!.value.tags, ["part-b"]);
      assert.deepEqual(filteredResults[0]!.providers, ["provider-b"]);

      let errForward: unknown;
      const forwardSession = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: [providerPartialA, providerConflictC],
      });
      try {
        await forwardSession.project().facts(TagFact);
      } catch (err) {
        errForward = err;
      } finally {
        await forwardSession.close();
      }

      let errReverse: unknown;
      const reverseSession = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: [providerConflictC, providerPartialA],
      });
      try {
        await reverseSession.project().facts(TagFact);
      } catch (err) {
        errReverse = err;
      } finally {
        await reverseSession.close();
      }

      assert.ok(errForward instanceof TssaeError);
      assert.ok(errReverse instanceof TssaeError);
      assert.equal(errForward.code, "FACT_CONFLICT");
      assert.equal(errReverse.code, "FACT_CONFLICT");
      assert.equal(errForward.message, errReverse.message);
      assert.match(errForward.message, /provider-a/u);
      assert.match(errForward.message, /provider-c/u);
    });
  });
});
