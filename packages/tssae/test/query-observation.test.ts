import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { copyDirectory, resolveHostDir, withTempHost } from "@tssae/test-hosts";
import {
  defineAdapter,
  defineFactProvider,
  defineFactType,
  defineModel,
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

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          models: [model],
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
});
