import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { copyDirectory, resolveHostDir, withTempHost } from "@tssae/test-hosts";
import {
  defineSummaries,
  openProject,
  syntaxLocationOf,
  type CleanupCoverageEvent,
  type EngineCallIoMetrics,
  type Gap,
  type QueryCompletedEvent,
} from "tssae";

void describe("tssae query observation, gaps, and explain hooks", () => {
  void it("emits RPC metrics, query cache statuses, recordGaps(), and syntaxLocationOf", async () => {
    await withTempHost(
      { host: "sdk-queries/query-observation-gaps" },
      async (host) => {
        await host.typecheck();

        const openMetrics: EngineCallIoMetrics[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
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

          const ctx = session.project(undefined, {
            observer: {
              observe: { mode: "detail", limit: { maxItems: 50 } },
              explainTarget: { file: "src/consumer.ts", line: 1 },
              onRpcMetrics: (m) => rpcMetrics.push(m),
              onQueryCompleted: (ev) => queryEvents.push(ev),
              onGaps: (gaps) => recordedGaps.push(...gaps),
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

          ctx.recordGaps([
            {
              kind: "dynamic-schema-field",
              message: "Domain library gap forwarded via recordGaps()",
              at: calls1[0]!.location,
            },
          ]);
          assert.equal(recordedGaps.length, 1);
          assert.equal(recordedGaps[0]?.kind, "dynamic-schema-field");

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

          // Test scanComments metrics
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
            {
              match: {
                package: "@acme/lifecycle",
                memberPath: ["IDisposable", "prototype", "dispose"],
              },
              effects: {
                completeness: "complete",
                resourceOps: [{ kind: "disposes-target", target: "receiver" }],
              },
            },
            {
              match: {
                package: "@acme/lifecycle",
                memberPath: ["onDidDisconnect"],
              },
              calls: {
                completeness: "complete",
                invokesCallbacks: [
                  {
                    callbackArgIndex: 0,
                    timing: "lifecycle-end",
                    eventName: "disconnect",
                  },
                ],
              },
            },
          ],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
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
