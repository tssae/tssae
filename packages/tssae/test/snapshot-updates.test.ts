import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@tssae/test-hosts";
import {
  TssaeError,
  defineSummaries,
  openProject,
  type BranchRef,
  type CallRef,
  type DeclarationRef,
  type EngineCallIoMetrics,
  type InstanceRef,
  type SignatureRef,
  type SyntaxRef,
  type TypeRef,
} from "tssae";

void describe("tssae snapshot updates and stale reference invalidation", () => {
  void it("rejects stale references of all ProgramRef kinds (including empty-counterpart queries) and stale ProjectContext after session.update()", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "snapshot-update-unit-host",
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
          "src/overloads.ts": [
            'export function pick(value: string): { kind: "str"; value: string };',
            'export function pick(value: number): { kind: "num"; value: number };',
            "export function pick(value: string | number) {",
            '  return typeof value === "string" ? { kind: "str", value } : { kind: "num", value };',
            "}",
            "",
          ].join("\n"),
          "src/app.ts": [
            'import { pick } from "./overloads.js";',
            "export function createClient(): { dispose(): void } {",
            "  return { dispose() {} };",
            "}",
            "export function run(flag: boolean): string {",
            "  const client = createClient();",
            '  const chosen = pick("initial");',
            "  if (flag) {",
            "    client.dispose();",
            "    return chosen.kind;",
            "  }",
            "  client.dispose();",
            "  return chosen.kind;",
            "}",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        const resourceSummaries = defineSummaries({
          id: "resource-summaries",
          entries: [
            {
              match: { memberPath: ["createClient"] },
              instances: {
                completeness: "complete",
                outcome: { kind: "allocates-fresh-instance" },
              },
            },
            {
              match: { memberPath: ["dispose"] },
              effects: {
                completeness: "complete",
                resourceOps: [{ kind: "disposes-target", target: "receiver" }],
              },
            },
          ],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { packs: [resourceSummaries] },
        });

        try {
          const ctx1 = session.project();
          const initialSnapshotId = session.snapshotId;

          const fns = await ctx1.findSyntax({
            files: ["src/app.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const runFn = fns.find((fn) => fn.text.includes("function run"));
          assert.ok(runFn);

          const callSyntaxes = await ctx1.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const pickSyntax = callSyntaxes.find((c) =>
            c.text.startsWith("pick("),
          );
          const createSyntax = callSyntaxes.find((c) =>
            c.text.startsWith("createClient("),
          );
          assert.ok(pickSyntax);
          assert.ok(createSyntax);

          const [resolvedCall] = await ctx1.resolveCalls([pickSyntax.ref], {
            select: {
              signature: true,
              calleeDeclaration: true,
              returnType: true,
            },
          });
          assert.ok(resolvedCall?.call);
          assert.ok(resolvedCall.signature);
          assert.ok(resolvedCall.calleeDeclaration);
          assert.ok(resolvedCall.returnType);

          const branchGroups = await ctx1.listBranches([runFn.ref]);
          const branchRef = branchGroups[0]?.branches[0]?.ref;
          assert.ok(branchRef);

          const [instResolution] = await ctx1.resolveInstances([
            createSyntax.ref,
          ]);
          const instanceRef =
            instResolution?.status === "proved"
              ? instResolution.instance
              : null;
          assert.ok(instanceRef);

          const staleSyntaxRef: SyntaxRef = pickSyntax.ref;
          const staleCallRef: CallRef = resolvedCall.call;
          const staleSignatureRef: SignatureRef = resolvedCall.signature;
          const staleDeclarationRef: DeclarationRef =
            resolvedCall.calleeDeclaration;
          const staleTypeRef: TypeRef = resolvedCall.returnType.ref;
          const staleBranchRef: BranchRef = branchRef;
          const staleInstanceRef: InstanceRef = instanceRef;

          host.replaceText("src/app.ts", 'pick("initial")', "pick(42)");
          const updateResult = await session.update({
            changed: ["src/app.ts"],
          });
          assert.equal(updateResult.previousSnapshotId, initialSnapshotId);
          assert.notEqual(updateResult.snapshotId, initialSnapshotId);

          // Old ProjectContext bound to snapshot 1 must fail with STALE_SNAPSHOT
          await assert.rejects(
            () => ctx1.findSyntax({ kinds: ["CallExpression"] }),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_SNAPSHOT",
          );

          const ctx2 = session.project();
          const freshRunFn = (
            await ctx2.findSyntax({
              files: ["src/app.ts"],
              kinds: ["FunctionDeclaration"],
            })
          ).find((fn) => fn.text.includes("function run"));
          assert.ok(freshRunFn);
          const freshCalls = await ctx2.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const freshPickSyntax = freshCalls.find((c) =>
            c.text.startsWith("pick("),
          );
          assert.ok(freshPickSyntax);
          const [freshResolvedCall] = await ctx2.resolveCalls(
            [freshPickSyntax.ref],
            {
              select: {
                signature: true,
                returnType: true,
              },
            },
          );
          assert.ok(freshResolvedCall?.returnType);

          // Every ProgramRef kind from snapshot 1 must throw STALE_REFERENCE on snapshot 2
          const staleChecks: readonly (() => Promise<unknown>)[] = [
            () => ctx2.typeAt([staleSyntaxRef]),
            () => ctx2.listBranches([staleSyntaxRef]),
            () =>
              ctx2.branchCoverage({
                function: staleSyntaxRef,
                branches: [],
                targets: [],
              }),
            () =>
              ctx2.branchCoverage({
                scope: staleSyntaxRef,
                branches: [],
                targets: [],
              }),
            () =>
              ctx2.branchCoverage({
                branches: [staleBranchRef],
                targets: [],
              }),
            () =>
              ctx2.branchCoverage({
                branches: [],
                targets: [staleSyntaxRef],
              }),
            () => ctx2.resolveDeclarations([staleSyntaxRef]),
            () => ctx2.resolveDeclarations([staleCallRef]),
            () => ctx2.resolveDeclarations([staleSignatureRef]),
            () => ctx2.resolveCalls([staleSyntaxRef]),
            () =>
              ctx2.projectCalls([staleCallRef], { select: { location: true } }),
            () =>
              ctx2.projectSignatures([staleSignatureRef], {
                select: { location: true },
              }),
            () =>
              ctx2.projectTypes([staleTypeRef], { select: { typeText: true } }),
            () =>
              ctx2.isAssignableTo([
                {
                  source: staleTypeRef,
                  target: freshResolvedCall.returnType!.ref,
                },
              ]),
            () =>
              ctx2.isAssignableTo([
                {
                  source: freshResolvedCall.returnType!.ref,
                  target: staleTypeRef,
                },
              ]),
            () => ctx2.syntaxOf([staleDeclarationRef]),
            () => ctx2.callersOf([staleDeclarationRef]),
            () => ctx2.calleesOf([staleCallRef]),
            () => ctx2.resolveImplementations([staleCallRef]),
            () =>
              ctx2.valuePropagation({
                from: staleSyntaxRef,
                to: [freshPickSyntax.ref],
              }),
            () =>
              ctx2.valuePropagation({
                from: [staleSyntaxRef],
                to: [],
              }),
            () =>
              ctx2.valuePropagation({
                from: [],
                to: [staleDeclarationRef],
              }),
            () => ctx2.resolveInstances([staleSyntaxRef]),
            () =>
              ctx2.projectInstances([staleInstanceRef], {
                select: { instanceKey: true },
              }),
            () => ctx2.callEffects({ targets: [staleCallRef] }),
            () =>
              ctx2.instanceCleanup({
                scopes: [staleSyntaxRef],
              }),
            () =>
              ctx2.instanceCleanup({
                scopes: [freshRunFn.ref],
                targets: [staleInstanceRef],
              }),
            () =>
              ctx2.instanceCleanup({
                scopes: [],
                targets: [staleInstanceRef],
              }),
          ];

          for (const check of staleChecks) {
            await assert.rejects(
              check,
              (err: unknown) =>
                err instanceof TssaeError && err.code === "STALE_REFERENCE",
            );
          }
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("reuses unchanged file IR caches and invalidates only edited files on session.update()", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "ir-cache-invalidation-host",
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
          "src/unchanged.ts": [
            "export function keepCached(flag: boolean): number {",
            "  if (flag) {",
            "    return 1;",
            "  }",
            "  return 2;",
            "}",
            "",
          ].join("\n"),
          "src/mutable.ts": [
            "export function willChange(flag: boolean): number {",
            "  if (flag) {",
            "    return 10;",
            "  }",
            "  return 20;",
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
          assert.equal(openMetrics.length, 1);
          assert.equal(
            openMetrics[0]?.engineMetrics?.lowering?.functionsLowered,
            0,
          );

          const queryMetrics1: EngineCallIoMetrics[] = [];
          const ctx1 = session.project(undefined, {
            observer: {
              observe: "summary",
              onRpcMetrics: (m) => queryMetrics1.push(m),
            },
          });
          const fns1 = await ctx1.findSyntax({
            kinds: ["FunctionDeclaration"],
          });
          const keep1 = fns1.find((f) => f.text.includes("keepCached"));
          const change1 = fns1.find((f) => f.text.includes("willChange"));
          assert.ok(keep1 && change1);

          await ctx1.listBranches([keep1.ref]);
          await ctx1.listBranches([change1.ref]);

          host.replaceText("src/mutable.ts", "return 10;", "return 99;");
          const updateMetrics: EngineCallIoMetrics[] = [];
          await session.update({
            changed: ["src/mutable.ts"],
            observe: "summary",
            onRpcMetrics: (m) => updateMetrics.push(m),
          });
          assert.equal(updateMetrics.length, 1);
          assert.equal(
            updateMetrics[0]?.engineMetrics?.irCache?.filesReused,
            1,
          );
          assert.equal(
            updateMetrics[0]?.engineMetrics?.irCache?.filesInvalidated,
            1,
          );

          const queryMetrics2: EngineCallIoMetrics[] = [];
          const ctx2 = session.project(undefined, {
            observer: {
              observe: "summary",
              onRpcMetrics: (m) => queryMetrics2.push(m),
            },
          });
          const fns2 = await ctx2.findSyntax({
            kinds: ["FunctionDeclaration"],
          });
          const keep2 = fns2.find((f) => f.text.includes("keepCached"));
          const change2 = fns2.find((f) => f.text.includes("willChange"));
          assert.ok(keep2 && change2);

          queryMetrics2.length = 0;
          await ctx2.listBranches([keep2.ref]);
          const keepMetric = queryMetrics2.at(-1)?.engineMetrics;
          assert.equal(keepMetric?.irCache?.functionHits, 1);
          assert.equal(keepMetric?.irCache?.functionMisses, 0);
          assert.equal(keepMetric?.lowering?.functionsLowered, 0);

          queryMetrics2.length = 0;
          await ctx2.listBranches([change2.ref]);
          const changeMetric = queryMetrics2.at(-1)?.engineMetrics;
          assert.equal(changeMetric?.irCache?.functionHits, 0);
          assert.equal(changeMetric?.irCache?.functionMisses, 1);
          assert.equal(changeMetric?.lowering?.functionsLowered, 1);
        } finally {
          await session.close();
        }
      },
    );
  });
});
