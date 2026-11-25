import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  defineAdapter,
  defineConfig,
  defineFactProvider,
  defineFactType,
  defineModel,
  defineRule,
  openSession,
  runCheck,
  type DiagnosticBasis,
  type Evidence,
} from "@verule/sdk";
import { withTempHost } from "@verule/test-hosts";

void describe("@verule/sdk selected query evidence", () => {
  void it("threads provenance from selected CallTargetInfo, ValuePropagationPath/TargetResult, SyntaxNodeResult, and Model while excluding unselected candidates", async () => {
    await withTempHost(
      { host: "sdk-queries/selected-evidence-propagation" },
      async (host) => {
        await host.typecheck();

        const policyModel = defineModel({
          id: "restricted-data-policy",
          description: "Restricted loader outputs must not reach public sinks.",
        });

        const rule = defineRule({
          id: "selected-relation-basis",
          run: async (ctx) => {
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
                res.from.handleId ===
                  restrictedLoader.callSyntax.ref.handleId && res.reachable,
            );
            assert.ok(selectedFlow);
            const selectedPath = selectedFlow.paths[0];
            assert.ok(selectedPath);

            const matchedSink = sinkArgs.find(
              (s) => s.argRef.handleId === selectedFlow.to.handleId,
            );
            assert.ok(matchedSink);

            ctx.checkpoint(
              "selected-sink-flow",
              matchedSink.callLoc,
              "pass",
              () => ({ target: selectedFlow.to.handleId }),
            );

            const model = ctx.getModel("restricted-data-policy");
            assert.ok(model);

            const customNote: Evidence = {
              kind: "note",
              message: "Business rule: restricted data cannot enter sink().",
              at: matchedSink.callLoc,
            };

            const basis: readonly DiagnosticBasis[] = [
              model,
              restrictedLoader.targetInfo,
              selectedPath,
              restrictedLoader.callSyntax,
              customNote,
              // Repeated query results and notes retain their first-seen positions.
              selectedPath,
              customNote,
            ];

            ctx.report({
              at: matchedSink.callLoc,
              message: "Restricted data reaches sink().",
              basis,
            });
          },
        });

        const session = await openSession({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            models: [policyModel],
            rules: [rule],
          }),
        });
        try {
          const report = await session.check({
            at: "src/app.ts:6:3",
            observe: "detail",
          });
          assert.equal(report.diagnostics.length, 1);
          const diag = report.diagnostics[0]!;
          assert.equal(diag.at.file, "src/app.ts");
          assert.equal(diag.at.line, 6);

          // Model evidence is emitted first as requested in basis order.
          assert.deepEqual(diag.evidence[0], {
            kind: "model",
            modelId: "restricted-data-policy",
            message: "Restricted loader outputs must not reach public sinks.",
          });

          // Target declaration evidence points to loadRestricted in src/sources.ts.
          assert.ok(
            diag.evidence.some(
              (ev) =>
                ev.kind === "source" &&
                ev.at?.file === "src/sources.ts" &&
                ev.at.line === 1 &&
                ev.declaration === "loadRestricted",
            ),
            JSON.stringify(diag.evidence),
          );

          // Propagation path evidence reaches line 6 sink(restricted).
          assert.ok(
            diag.evidence.some(
              (ev) =>
                ev.kind === "source" &&
                ev.at?.file === "src/app.ts" &&
                ev.at.line === 6,
            ),
            JSON.stringify(diag.evidence),
          );

          // Custom note appears once (deduplicated) at the end.
          const customNotes = diag.evidence.filter(
            (ev) =>
              ev.kind === "note" &&
              ev.message ===
                "Business rule: restricted data cannot enter sink().",
          );
          assert.equal(customNotes.length, 1);
          assert.deepEqual(diag.evidence.at(-1), customNotes[0]);

          // Unselected candidates (loadPublic on line 5 of sources.ts and line 5/7 of app.ts) must NOT leak.
          assert.equal(
            diag.evidence.some(
              (ev) =>
                ev.declaration === "loadPublic" ||
                (ev.at?.file === "src/sources.ts" && ev.at.line === 5) ||
                (ev.at?.file === "src/app.ts" &&
                  (ev.at.line === 5 || ev.at.line === 7)),
            ),
            false,
            JSON.stringify(diag.evidence),
          );

          // Named checkpoint remains recorded in explain output.
          assert.ok(
            report.explanation?.checkpoints.some(
              (cp) => cp.name === "selected-sink-flow" && cp.outcome === "pass",
            ),
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

        const rule = defineRule({
          id: "selected-effect-and-callback",
          run: async (ctx) => {
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

            // Aggregate CallEffectsResult contains job.safe = 1 on line 9; selecting only flagsWrite + cbInvocation must exclude line 9.
            ctx.report({
              at: flagsWrite.location,
              message: "Selected field write inside callback.",
              basis: [flagsWrite, cbInvocation],
            });
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({ tsconfig: "tsconfig.json", rules: [rule] }),
        });

        assert.equal(report.diagnostics.length, 1);
        const diag = report.diagnostics[0]!;
        assert.ok(
          diag.evidence.some(
            (ev) => ev.at?.file === "src/queue.ts" && ev.at.line === 11,
          ),
          JSON.stringify(diag.evidence),
        );
        assert.ok(
          diag.evidence.some(
            (ev) => ev.at?.file === "src/queue.ts" && ev.at.line === 6,
          ),
          JSON.stringify(diag.evidence),
        );
        assert.equal(
          diag.evidence.some(
            (ev) => ev.at?.file === "src/queue.ts" && ev.at.line === 9,
          ),
          false,
          `Aggregate read/write leaked into selected diagnostic: ${JSON.stringify(diag.evidence)}`,
        );
      },
    );
  });

  void it("supports BranchEdgeInfo, BranchCoverageItem, instance/cleanup/impl results, and projected lookup rows", async () => {
    await withTempHost(
      { host: "sdk-queries/selected-evidence-relations" },
      async (host) => {
        await host.typecheck();

        const cleanupModel = defineModel({
          id: "resource-cleanup-model",
          cleanupOperations: [
            {
              kind: "disposes-target",
              match: { container: "Resource", member: "dispose" },
              target: "receiver",
            },
          ],
        });

        const rule = defineRule({
          id: "selected-other-relations",
          run: async (ctx) => {
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
            assert.ok(instRes && instRes.instance !== null);
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

            const [declWithLoc] = await ctx.resolveDeclarations(
              [makeCall.ref],
              {
                select: { location: true, name: true },
              },
            );
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

            ctx.report({
              at: disposeCall.location,
              message: "Selected relation bundle diagnostic.",
              basis: [
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
              ],
            });
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            models: [cleanupModel],
            rules: [rule],
          }),
        });

        assert.equal(report.diagnostics.length, 1);
        const diag = report.diagnostics[0]!;
        assert.ok(
          diag.evidence.some((ev) => ev.at?.line === 9 && ev.kind === "source"),
          `Expected branch evidence on line 9: ${JSON.stringify(diag.evidence)}`,
        );
        assert.ok(
          diag.evidence.some(
            (ev) => ev.at?.line === 4 && ev.declaration === "makeResource",
          ),
          `Expected declaration evidence on line 4: ${JSON.stringify(diag.evidence)}`,
        );
        assert.ok(
          diag.evidence.some((ev) => ev.at?.line === 5),
          `Expected instance allocation evidence on line 5: ${JSON.stringify(diag.evidence)}`,
        );
        assert.ok(
          diag.evidence.some((ev) => ev.at?.line === 10),
          `Expected cleanup site evidence on line 10: ${JSON.stringify(diag.evidence)}`,
        );
      },
    );
  });

  void it("threads FactResult and FactContribution.basis transitively into derived facts and diagnostics", async () => {
    await withTempHost(
      { host: "sdk-queries/selected-evidence-facts" },
      async (host) => {
        await host.typecheck();

        const SourceOriginFact = defineFactType<
          "call",
          undefined,
          { readonly tag: string }
        >({
          id: "test.source-origin",
          subjectKind: "call",
        });
        const DerivedSinkFact = defineFactType<
          "syntax",
          undefined,
          { readonly tainted: boolean }
        >({
          id: "test.derived-sink",
          subjectKind: "syntax",
        });

        const sourceAdapter = defineAdapter({
          id: "test-source-adapter",
          provides: [
            defineFactProvider(SourceOriginFact, async (ctx) => {
              const calls = await ctx.findSyntax({
                files: ["src/service.ts"],
                kinds: ["CallExpression"],
              });
              const resolved = await ctx.resolveCalls(
                calls.map((c) => c.ref),
                { select: { location: true } },
              );
              const callees = await ctx.calleesOf(
                resolved
                  .map((r) => r.call)
                  .filter((c): c is NonNullable<typeof c> => c !== null),
              );
              const out = [];
              for (const entry of callees) {
                const target = entry.resolvedTargets.find(
                  (t) => t.name === "fetchSecret",
                );
                if (!target) continue;
                out.push({
                  subject: entry.call,
                  value: { tag: "secret" },
                  basis: [entry, target],
                });
              }
              return out;
            }),
          ],
        });

        const derivedAdapter = defineAdapter({
          id: "test-derived-adapter",
          provides: [
            defineFactProvider(DerivedSinkFact, async (ctx) => {
              const calls = await ctx.findSyntax({
                files: ["src/service.ts"],
                kinds: ["CallExpression"],
              });
              const fetchCall = calls.find((c) => c.text === "fetchSecret()");
              if (!fetchCall) return [];
              const [resolved] = await ctx.resolveCalls([fetchCall.ref], {
                select: { location: true },
              });
              if (!resolved?.call) return [];
              const [callTarget] = await ctx.calleesOf([resolved.call]);
              if (!callTarget) return [];
              return [
                {
                  subject: fetchCall.ref,
                  value: { tainted: true },
                  basis: [callTarget],
                  evidence: [
                    {
                      kind: "note",
                      message: "Derived taint from call target provenance.",
                      at: fetchCall.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        const rule = defineRule({
          id: "fact-basis-rule",
          run: async (ctx) => {
            const origins = await ctx.facts(SourceOriginFact);
            const derived = await ctx.facts(DerivedSinkFact);
            assert.equal(origins.length, 1);
            assert.equal(derived.length, 1);
            const calls = await ctx.findSyntax({
              files: ["src/service.ts"],
              kinds: ["CallExpression"],
            });
            const fetchCall = calls.find((c) => c.text === "fetchSecret()");
            assert.ok(fetchCall);

            ctx.report({
              at: fetchCall.location,
              message: "Derived fact violation.",
              basis: [origins[0]!, derived[0]!],
            });
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: [sourceAdapter, derivedAdapter],
            rules: [rule],
          }),
        });

        assert.equal(report.diagnostics.length, 1);
        const diag = report.diagnostics[0]!;
        assert.ok(
          diag.evidence.some(
            (ev) =>
              ev.kind === "source" &&
              ev.at?.file === "src/service.ts" &&
              ev.at.line === 1,
          ),
          JSON.stringify(diag.evidence),
        );
        assert.ok(
          diag.evidence.some(
            (ev) =>
              ev.kind === "note" &&
              ev.message === "Derived taint from call target provenance." &&
              ev.at?.line === 5,
          ),
          JSON.stringify(diag.evidence),
        );
      },
    );
  });
});
