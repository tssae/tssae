import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { copyDirectory, resolveHostDir, withTempHost } from "@tssae/test-hosts";
import {
  defineAdapter,
  defineFactProvider,
  defineFactType,
  defineModel,
  defineSummaries,
  openProject,
  syntaxLocationOf,
  type CleanupCoverageEvent,
  type EngineCallIoMetrics,
  type ExplainProviderEvaluation,
  type FactExplainEvent,
  type Gap,
  type QueryCompletedEvent,
} from "tssae";

void describe("tssae query observation, local gaps, and explain hooks", () => {
  void it("emits RPC metrics, query cache statuses, gaps, fact explain localGaps, cleanup paths, and syntaxLocationOf", async () => {
    await withTempHost(
      { host: "sdk-queries/fact-providers-explain-gaps" },
      async (host) => {
        await host.typecheck();

        const SourceSpecFact = defineFactType<
          "syntax",
          undefined,
          { readonly sourceName: string }
        >({
          id: "ts.obs.source-spec",
          subjectKind: "syntax",
        });

        const DerivedFact = defineFactType<
          "syntax",
          undefined,
          { readonly consumerName: string }
        >({
          id: "ts.obs.derived",
          subjectKind: "syntax",
        });

        const adapterA = defineAdapter({
          id: "obs-adapter-a",
          provides: [
            defineFactProvider(SourceSpecFact, async (ctx, request) => {
              const calls = await ctx.findSyntax({
                files: ["src/source.ts"],
                kinds: ["CallExpression"],
              });
              const specCall = calls.find((c) =>
                c.text.startsWith("defineSpec("),
              );
              const dynamicCall = calls.find((c) =>
                c.text.startsWith("dynamicSpec("),
              );
              if (!specCall || !dynamicCall) return [];
              const gap: Gap = {
                kind: "unresolved-field",
                message: 'Field "couponCode" is dynamic.',
                at: dynamicCall.location,
              };
              return [
                {
                  subject: specCall.ref,
                  value: { sourceName: "order" },
                  gaps: [gap],
                  ...(request.explain
                    ? {
                        explainTrace: {
                          localGaps: [
                            {
                              status: "unresolved" as const,
                              factTypeId: SourceSpecFact.id,
                              providerId: "obs-adapter-a",
                              field: "couponCode",
                              gap,
                              subjectLocation: specCall.location,
                            },
                          ],
                        },
                      }
                    : {}),
                },
              ];
            }),
          ],
        });

        const adapterB = defineAdapter({
          id: "obs-adapter-b",
          provides: [
            defineFactProvider(DerivedFact, async (ctx, request) => {
              const consumerCalls = await ctx.findSyntax({
                files: ["src/consumer.ts"],
                kinds: ["CallExpression"],
              });
              const target = consumerCalls.find((c) =>
                c.text.startsWith("useSpec("),
              );
              if (!target) return [];
              const upstream = await ctx.facts(SourceSpecFact);
              const firstUpstream = upstream[0];
              if (!firstUpstream) return [];
              return [
                {
                  subject: target.ref,
                  value: { consumerName: "derivedOrder" },
                  basis: [firstUpstream],
                  ...(request.explain
                    ? {
                        explainTrace: {
                          localGaps: [
                            {
                              status: "resolved_by_override" as const,
                              factTypeId: SourceSpecFact.id,
                              providerId: "obs-adapter-a",
                              field: "couponCode",
                              gap: firstUpstream.gaps[0]!,
                              resolvedBy: {
                                providerId: "obs-adapter-b",
                                operation: "extend",
                                conclusion: "required",
                                at: target.location,
                              },
                            },
                          ],
                        },
                      }
                    : {}),
                },
              ];
            }),
          ],
        });

        const openMetrics: EngineCallIoMetrics[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [adapterA, adapterB],
          observe: { mode: "summary" },
          onRpcMetrics: (m) => {
            openMetrics.push(m);
          },
        });

        try {
          assert.equal(openMetrics.length, 1);
          assert.equal(openMetrics[0]?.isQueryBatch, false);
          assert.ok(openMetrics[0]?.engineMetrics);
          assert.ok(Array.isArray(session.workspaceSourceFiles));
          assert.ok(
            session.fileOrigins.some((o) => o.file === "src/consumer.ts"),
          );

          const rpcMetrics: EngineCallIoMetrics[] = [];
          const queryEvents: QueryCompletedEvent[] = [];
          const recordedGaps: Gap[] = [];
          const factExplains: FactExplainEvent[] = [];
          const providerEvals: ExplainProviderEvaluation[] = [];

          const ctx = session.project(undefined, {
            observer: {
              observe: { mode: "detail", limit: { maxItems: 50 } },
              explainTarget: { file: "src/consumer.ts", line: 1 },
              onRpcMetrics: (m) => rpcMetrics.push(m),
              onQueryCompleted: (ev) => queryEvents.push(ev),
              onGaps: (gaps) => recordedGaps.push(...gaps),
              onFactExplain: (ev) => factExplains.push(ev),
              onProviderEvaluation: (ev) => providerEvals.push(ev),
            },
          });

          const calls1 = await ctx.findSyntax({
            files: ["src/consumer.ts"],
            kinds: ["CallExpression"],
          });
          assert.ok(calls1.length > 0);
          assert.deepEqual(
            syntaxLocationOf(calls1[0]!.ref),
            calls1[0]!.location,
          );

          const calls2 = await ctx.findSyntax({
            files: ["src/consumer.ts"],
            kinds: ["CallExpression"],
          });
          assert.equal(calls2.length, calls1.length);

          const derived = await ctx.facts(DerivedFact);
          assert.equal(derived.length, 1);

          const sourceFacts = await ctx.facts(SourceSpecFact);
          assert.equal(sourceFacts.length, 1);
          assert.ok(recordedGaps.length > 0);

          assert.ok(
            rpcMetrics.some(
              (m) => m.isQueryBatch && m.engineMetrics !== undefined,
            ),
          );
          assert.ok(
            queryEvents.some(
              (e) => e.op === "findSyntax" && e.cacheStatus === "computed",
            ),
          );
          assert.ok(
            queryEvents.some(
              (e) => e.op === "findSyntax" && e.cacheStatus === "reused",
            ),
          );
          assert.ok(
            providerEvals.some(
              (p) =>
                p.providerId === "obs-adapter-b" && p.status === "evaluated",
            ),
          );
          assert.ok(
            factExplains.some((fe) =>
              fe.localGaps.some((lg) => lg.status === "resolved_by_override"),
            ),
          );

          // Test typeAt gap propagation (unknown identifier or any) & scanComments metrics
          const commentMetrics: EngineCallIoMetrics[] = [];
          const comments = await session.scanComments({
            observe: "summary",
            onRpcMetrics: (m) => commentMetrics.push(m),
          });
          assert.ok(Array.isArray(comments));
          assert.equal(commentMetrics.length, 1);
          assert.ok(commentMetrics[0]?.engineMetrics);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("emits onCleanupCovered and onGaps for instanceCleanup and callEffects", async () => {
    await withTempHost(
      {
        host: "sdk-queries/call-effects-cleanup",
        setup: (host) => {
          copyDirectory(
            resolveHostDir("sdk-queries/acme-lifecycle"),
            host.resolvePath("node_modules/@acme/lifecycle"),
          );
        },
      },
      async (host) => {
        await host.typecheck();

        const model = defineModel({
          id: "cleanup-obs-model",
          instanceBehaviors: [
            {
              kind: "allocates-fresh-instance",
              match: { symbol: "createExternalResource" },
            },
          ],
          cleanupOperations: [
            {
              kind: "disposes-target",
              match: { member: "dispose" },
              target: "receiver",
            },
          ],
          lifecycleEvents: [
            {
              kind: "lifecycle-end-callback",
              match: { symbol: "onDidDisconnect" },
              callbackArgumentIndex: 0,
            },
          ],
        });

        const summaries = defineSummaries({
          id: "cleanup-obs-summaries",
          entries: [
            {
              match: {
                package: "@acme/lifecycle",
                memberPath: ["createExternalResource"],
              },
              instances: {
                completeness: "complete",
                outcome: { kind: "allocates-fresh-instance" },
              },
            },
          ],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          models: [model],
          summaries: { packs: [summaries] },
        });
        try {
          const cleanupEvents: CleanupCoverageEvent[] = [];
          const gapsObserved: Gap[] = [];
          const ctx = session.project(undefined, {
            observer: {
              explainTarget: { file: "src/scenarios.ts", line: 1 },
              onCleanupCovered: (ev) => cleanupEvents.push(ev),
              onGaps: (gaps) => gapsObserved.push(...gaps),
            },
          });

          const calls = await ctx.findSyntax({
            files: ["src/scenarios.ts"],
            kinds: ["CallExpression"],
          });
          const disconnectCalls = calls.filter((c) =>
            c.text.startsWith("onDidDisconnect("),
          );
          const varDecls = await ctx.findSyntax({
            files: ["src/scenarios.ts"],
            kinds: ["VariableDeclaration"],
          });
          const resourceVars = varDecls
            .map((v) => v.initializer)
            .filter(
              (init): init is NonNullable<typeof init> => init !== undefined,
            );
          const instances = await ctx.resolveInstances(resourceVars);
          const instRefs = instances
            .filter(
              (r): r is Extract<typeof r, { readonly status: "proved" }> =>
                r.status === "proved",
            )
            .map((r) => r.instance)
            .filter((i): i is NonNullable<typeof i> => i !== null);

          assert.ok(disconnectCalls.length > 0 && instRefs.length > 0);
          await ctx.instanceCleanup({
            scopes: disconnectCalls.map((c) => c.ref),
            targets: instRefs,
          });
          const dynCallbackDecl = varDecls.find((v) =>
            v.text.startsWith("knownPlusDyn"),
          );
          assert.ok(dynCallbackDecl?.initializer);
          await ctx.callEffects({
            targets: [dynCallbackDecl.initializer],
          });
          assert.ok(cleanupEvents.length > 0);
          assert.ok(cleanupEvents[0]!.paths.length > 0);
          assert.ok(gapsObserved.length > 0);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("tracks lazy per-function IR lowering and function-level cache hits in observe metrics", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "ir-observe-metrics-host",
            private: true,
            type: "module",
          }),
          "tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2024",
              module: "NodeNext",
              moduleResolution: "NodeNext",
            },
            include: ["src/**/*.ts"],
          }),
          "src/two-fns.ts": [
            "export function firstFn(flag: boolean): number {",
            "  if (flag) {",
            "    return 1;",
            "  }",
            "  return 2;",
            "}",
            "export function secondFn(flag: boolean): number {",
            "  if (flag) {",
            "    return 3;",
            "  }",
            "  return 4;",
            "}",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        const openMetrics: EngineCallIoMetrics[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          observe: "summary",
          onRpcMetrics: (m) => openMetrics.push(m),
        });
        try {
          assert.equal(
            openMetrics[0]?.engineMetrics?.lowering?.functionsLowered,
            0,
          );

          const rpcMetrics: EngineCallIoMetrics[] = [];
          const ctx = session.project(undefined, {
            observer: {
              observe: "summary",
              onRpcMetrics: (m) => rpcMetrics.push(m),
            },
          });
          const fns = await ctx.findSyntax({ kinds: ["FunctionDeclaration"] });
          const firstFn = fns.find((f) => f.text.includes("firstFn"));
          const secondFn = fns.find((f) => f.text.includes("secondFn"));
          const rets = await ctx.findSyntax({ kinds: ["ReturnStatement"] });
          const firstRet = rets.find((r) => r.text === "return 1;");
          assert.ok(firstFn && secondFn && firstRet);

          rpcMetrics.length = 0;
          const firstBranches = await ctx.listBranches([firstFn.ref]);
          const firstLowerMetric = rpcMetrics.at(-1)?.engineMetrics;
          assert.equal(firstLowerMetric?.lowering?.functionsLowered, 1);
          assert.equal(firstLowerMetric?.irCache?.functionMisses, 1);
          assert.equal(firstLowerMetric?.irCache?.functionHits, 0);
          assert.ok((firstLowerMetric?.lowering?.verticesCreated ?? 0) > 0);
          assert.equal(typeof firstLowerMetric?.lowering?.durationMs, "number");

          const branchRef = firstBranches[0]?.branches[0]?.ref;
          assert.ok(branchRef);
          rpcMetrics.length = 0;
          await ctx.branchCoverage({
            function: firstFn.ref,
            branches: [branchRef],
            targets: [firstRet.ref],
          });
          const firstHitMetric = rpcMetrics.at(-1)?.engineMetrics;
          assert.equal(firstHitMetric?.lowering?.functionsLowered, 0);
          assert.equal(firstHitMetric?.irCache?.functionHits, 1);
          assert.equal(firstHitMetric?.irCache?.functionMisses, 0);

          // Querying secondFn in the same file triggers lazy lowering only now.
          rpcMetrics.length = 0;
          await ctx.listBranches([secondFn.ref]);
          const secondLowerMetric = rpcMetrics.at(-1)?.engineMetrics;
          assert.equal(secondLowerMetric?.lowering?.functionsLowered, 1);
          assert.equal(secondLowerMetric?.irCache?.functionMisses, 1);
        } finally {
          await session.close();
        }
      },
    );
  });
});
