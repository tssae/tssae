import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@tssae/test-hosts";
import {
  defineSummaries,
  evidenceOf,
  extractBasisEvidence,
  openProject,
  registerProvenance,
  type DiagnosticBasis,
  type Evidence,
} from "tssae";

void describe("tssae selected query evidence", () => {
  void it("threads provenance from selected CallTargetInfo, ValuePropagationPath/TargetResult, SyntaxNodeResult, and BehaviorSummaryPack while excluding unselected candidates", async () => {
    await withTempHost(
      { host: "sdk-queries/selected-evidence-propagation" },
      async (host) => {
        await host.typecheck();

        const policySummary = defineSummaries({
          id: "restricted-data-policy",
          description: "Restricted loader outputs must not reach public sinks.",
          entries: [],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { packs: [policySummary] },
        });
        try {
          const ctx = session.project();

          const calls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const resolvedCalls = await ctx.resolveCalls(
            calls.map((c) => c.ref),
            { select: { location: true, arguments: true } },
          );
          const calleeResults = await ctx.calleesOf(
            resolvedCalls
              .map((r) => r.call)
              .filter((c): c is NonNullable<typeof c> => c !== null),
          );
          const calleesByHandle = new Map(
            calleeResults.map((item) => [item.call.handleId, item]),
          );

          const loaderEntries: {
            readonly callSyntax: (typeof calls)[number];
            readonly callRef: NonNullable<
              (typeof resolvedCalls)[number]["call"]
            >;
            readonly targetInfo: (typeof calleeResults)[number]["resolvedTargets"][number];
            readonly isRestricted: boolean;
          }[] = [];

          const sinkArgs: {
            readonly argRef: NonNullable<
              NonNullable<(typeof resolvedCalls)[number]["arguments"]>[number]
            >;
            readonly callLoc: NonNullable<(typeof calls)[number]>["location"];
          }[] = [];

          for (const [index, resolved] of resolvedCalls.entries()) {
            const callSyntax = calls[index];
            if (!callSyntax || resolved.call === null) continue;
            const callees = calleesByHandle.get(resolved.call.handleId);
            if (!callees) continue;

            for (const targetInfo of callees.resolvedTargets) {
              if (
                targetInfo.name === "loadRestricted" ||
                targetInfo.name === "loadPublic"
              ) {
                loaderEntries.push({
                  callSyntax,
                  callRef: resolved.call,
                  targetInfo,
                  isRestricted: targetInfo.name === "loadRestricted",
                });
              }
              if (targetInfo.name === "sink" && resolved.arguments?.[0]) {
                sinkArgs.push({
                  argRef: resolved.arguments[0],
                  callLoc: callSyntax.location,
                });
              }
            }
          }

          const restrictedLoader = loaderEntries.find((e) => e.isRestricted);
          assert.ok(restrictedLoader);

          const propagation = await ctx.valuePropagation({
            propagation: "value",
            from: loaderEntries.map((e) => e.callSyntax.ref),
            to: sinkArgs.map((s) => s.argRef),
          });

          const selectedFlow = propagation.results.find(
            (res) =>
              res.from.handleId === restrictedLoader.callSyntax.ref.handleId &&
              res.status === "proved",
          );
          assert.ok(selectedFlow && selectedFlow.status === "proved");
          const selectedPath = selectedFlow.paths[0];
          assert.ok(selectedPath);

          const matchedSink = sinkArgs.find(
            (s) => s.argRef.handleId === selectedFlow.to.handleId,
          );
          assert.ok(matchedSink);

          const customNote: Evidence = {
            kind: "note",
            message: "Business rule: restricted data cannot enter sink().",
            at: matchedSink.callLoc,
          };

          const basis: readonly DiagnosticBasis[] = [
            policySummary,
            restrictedLoader.targetInfo,
            selectedPath,
            restrictedLoader.callSyntax,
            customNote,
            selectedPath,
            customNote,
          ];

          const evidence = extractBasisEvidence(basis);

          assert.deepEqual(evidence[0], {
            kind: "summary",
            summaryId: "restricted-data-policy",
            summarySource: "local",
            message: "Restricted loader outputs must not reach public sinks.",
          });

          assert.ok(
            evidence.some(
              (ev) =>
                ev.kind === "source" &&
                ev.at?.file === "src/sources.ts" &&
                ev.at.line === 1 &&
                ev.declaration === "loadRestricted",
            ),
            JSON.stringify(evidence),
          );

          assert.ok(
            evidence.some(
              (ev) =>
                ev.kind === "source" &&
                ev.at?.file === "src/app.ts" &&
                ev.at.line === 6,
            ),
            JSON.stringify(evidence),
          );

          const customNotes = evidence.filter(
            (ev) =>
              ev.kind === "note" &&
              ev.message ===
                "Business rule: restricted data cannot enter sink().",
          );
          assert.equal(customNotes.length, 1);
          assert.deepEqual(evidence.at(-1), customNotes[0]);

          assert.equal(
            evidence.some(
              (ev) =>
                ev.declaration === "loadPublic" ||
                (ev.at?.file === "src/sources.ts" && ev.at.line === 5) ||
                (ev.at?.file === "src/app.ts" &&
                  (ev.at.line === 5 || ev.at.line === 7)),
            ),
            false,
            JSON.stringify(evidence),
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("isolates FieldEffect and CallbackInvocationInfo evidence from CallEffectsResult aggregate evidence", async () => {
    await withTempHost(
      { host: "sdk-queries/selected-evidence-callbacks" },
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();

          const fns = await ctx.findSyntax({
            files: ["src/queue.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const flushFn = fns.find((fn) =>
            fn.text.includes("export function flush"),
          );
          assert.ok(flushFn);

          const [effects] = await ctx.callEffects({
            targets: [flushFn.ref],
            followCallbacks: true,
          });
          assert.ok(effects);

          const flagsWrite = effects.fieldWrites.find(
            (w) => w.propertyName === "flags",
          );
          const cbInvocation = effects.callbackInvocations[0];
          assert.ok(flagsWrite);
          assert.ok(cbInvocation);

          const evidence = extractBasisEvidence([flagsWrite, cbInvocation]);
          assert.ok(
            evidence.some(
              (ev) => ev.at?.file === "src/queue.ts" && ev.at.line === 11,
            ),
            JSON.stringify(evidence),
          );
          assert.ok(
            evidence.some(
              (ev) => ev.at?.file === "src/queue.ts" && ev.at.line === 6,
            ),
            JSON.stringify(evidence),
          );
          assert.equal(
            evidence.some(
              (ev) => ev.at?.file === "src/queue.ts" && ev.at.line === 9,
            ),
            false,
            `Aggregate read/write leaked into selected evidence: ${JSON.stringify(evidence)}`,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("supports BranchEdgeInfo, BranchCoverageItem, instance/cleanup/impl results, and projected lookup rows", async () => {
    await withTempHost(
      { host: "sdk-queries/selected-evidence-relations" },
      async (host) => {
        await host.typecheck();

        const cleanupSummaries = defineSummaries({
          id: "resource-cleanup-summary",
          entries: [
            {
              match: { memberPath: ["Resource", "prototype", "dispose"] },
              effects: {
                completeness: "complete",
                resourceOps: [
                  {
                    kind: "disposes-target",
                    target: "receiver",
                  },
                ],
              },
            },
          ],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { packs: [cleanupSummaries] },
        });
        try {
          const ctx = session.project();

          const fns = await ctx.findSyntax({
            files: ["src/impl.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const runFn = fns.find((fn) =>
            fn.text.includes("export function run"),
          );
          assert.ok(runFn);

          const calls = await ctx.findSyntax({
            files: ["src/impl.ts"],
            kinds: ["CallExpression"],
          });
          const makeCall = calls.find((c) => c.text === "makeResource()");
          const disposeCall = calls.find((c) => c.text === "res.dispose()");
          assert.ok(makeCall);
          assert.ok(disposeCall);

          const [branchesRes] = await ctx.listBranches([runFn.ref]);
          assert.ok(branchesRes);
          const trueBranch = branchesRes.branches.find(
            (b) => b.direction === "true",
          );
          assert.ok(trueBranch);

          const [covItem] = await ctx.branchCoverage({
            function: runFn.ref,
            branches: [trueBranch.ref],
            targets: [disposeCall.ref],
          });
          assert.ok(covItem);

          const [instRes] = await ctx.resolveInstances([makeCall.ref]);
          assert.ok(
            instRes && instRes.status === "proved" && instRes.instance !== null,
          );
          const instInfo = instRes.instances[0]!;

          const [projInstWithLoc] = await ctx.projectInstances(
            [instRes.instance],
            { select: { allocationLocation: true, originKind: true } },
          );
          const [projInstWithoutLoc] = await ctx.projectInstances(
            [instRes.instance],
            { select: { originKind: true } },
          );
          assert.ok(projInstWithLoc && projInstWithoutLoc);

          const [cleanupScope] = await ctx.instanceCleanup({
            scopes: [runFn.ref],
            targets: [instRes.instance],
          });
          const targetCleanup = cleanupScope?.targetResults[0];
          const cleanupSite = targetCleanup?.cleanupSites[0];
          assert.ok(targetCleanup && cleanupSite);

          const [implRes] = await ctx.resolveImplementations([makeCall.ref]);
          assert.ok(implRes);

          const [declWithLoc] = await ctx.resolveDeclarations([makeCall.ref], {
            select: { location: true, name: true },
          });
          const [declWithoutLoc] = await ctx.resolveDeclarations(
            [makeCall.ref],
            {
              select: { isExternal: true },
            },
          );
          const [syntaxWithLoc] = await ctx.syntaxOf([makeCall.ref], {
            select: { location: true, kind: true },
          });
          const [syntaxWithoutLoc] = await ctx.syntaxOf([makeCall.ref], {
            select: { text: true },
          });
          const [callWithLoc] = await ctx.resolveCalls([makeCall.ref], {
            select: { location: true },
          });
          assert.ok(
            declWithLoc &&
              declWithoutLoc &&
              syntaxWithLoc &&
              syntaxWithoutLoc &&
              callWithLoc,
          );

          const [projectedCall] =
            callWithLoc.call !== null
              ? await ctx.projectCalls([callWithLoc.call], {
                  select: { location: true },
                })
              : [];
          assert.ok(projectedCall);

          const evidence = extractBasisEvidence([
            trueBranch,
            covItem,
            instInfo,
            instRes,
            projInstWithLoc,
            projInstWithoutLoc,
            targetCleanup,
            cleanupSite,
            implRes,
            declWithLoc,
            declWithoutLoc,
            syntaxWithLoc,
            syntaxWithoutLoc,
            callWithLoc,
            projectedCall,
          ]);

          assert.ok(
            evidence.some((ev) => ev.at?.line === 9 && ev.kind === "source"),
            `Expected branch evidence on line 9: ${JSON.stringify(evidence)}`,
          );
          assert.ok(
            evidence.some(
              (ev) => ev.at?.line === 4 && ev.declaration === "makeResource",
            ),
            `Expected declaration evidence on line 4: ${JSON.stringify(evidence)}`,
          );
          assert.ok(
            evidence.some((ev) => ev.at?.line === 5),
            `Expected instance allocation evidence on line 5: ${JSON.stringify(evidence)}`,
          );
          assert.ok(
            evidence.some((ev) => ev.at?.line === 10),
            `Expected cleanup site evidence on line 10: ${JSON.stringify(evidence)}`,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("threads provenance registered via registerProvenance() through evidenceOf() and extractBasisEvidence()", () => {
    const loc = {
      file: "src/schema.ts",
      line: 4,
      column: 1,
      endLine: 4,
      endColumn: 20,
      start: 40,
      end: 60,
    };
    const domainItem = registerProvenance({ schemaName: "userSchema" }, [
      {
        kind: "source",
        message: "Zod object schema defined via z.object()",
        at: loc,
      },
    ]);

    assert.equal(evidenceOf(domainItem).length, 1);
    const combined = extractBasisEvidence([
      domainItem,
      { kind: "note", message: "Rule diagnostic note", at: loc },
    ]);
    assert.equal(combined.length, 2);
    assert.equal(
      combined[0]?.message,
      "Zod object schema defined via z.object()",
    );
    assert.equal(combined[1]?.message, "Rule diagnostic note");
  });
});
