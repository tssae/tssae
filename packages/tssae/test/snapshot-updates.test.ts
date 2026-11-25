import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@verule/test-hosts";
import {
  TssaeError,
  defineAdapter,
  defineFactProvider,
  defineFactType,
  defineModel,
  openProject,
  type BranchRef,
  type CallRef,
  type DeclarationRef,
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
        const model = defineModel({
          id: "resource-model",
          instanceBehaviors: [
            {
              kind: "allocates-fresh-instance",
              match: { symbol: "createClient" },
            },
          ],
          cleanupOperations: [
            {
              kind: "disposes-target",
              match: { member: "dispose" },
              target: "receiver",
            },
          ],
        });

        const CallTagFact = defineFactType<"call", undefined, string>({
          id: "test.call-tag",
          subjectKind: "call",
        });

        const callTagAdapter = defineAdapter({
          id: "call-tag-adapter",
          provides: [
            defineFactProvider(CallTagFact, async (ctx) => {
              const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
              const resolved = await ctx.resolveCalls(calls.map((c) => c.ref));
              return resolved
                .filter(
                  (row): row is typeof row & { call: CallRef } =>
                    row.call !== null,
                )
                .map((row) => ({ subject: row.call, value: "tagged" }));
            }),
          ],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          models: [model],
          adapters: [callTagAdapter],
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
          const instanceRef = instResolution?.instance;
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
            () =>
              ctx2.facts(CallTagFact, {
                targets: [staleCallRef],
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
});
