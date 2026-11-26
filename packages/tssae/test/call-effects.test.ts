import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { copyDirectory, resolveHostDir, withTempHost } from "@tssae/test-hosts";
import {
  TssaeError,
  defineSummaries,
  evidenceOf,
  extractBasisEvidence,
  isSameInstance,
  openProject,
  type CallEffectsResult,
  type InstanceCleanupScopeResult,
  type InstanceRef,
  type SyntaxRef,
} from "tssae";

const LIFECYCLE_ALLOC_SUMMARIES = defineSummaries({
  id: "test-lifecycle-alloc-summaries",
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
        memberPath: ["Disposable", "prototype", "_register"],
      },
      instances: {
        completeness: "complete",
        outcome: { kind: "forwards-instance", from: { argIndex: 0 } },
      },
    },
  ],
});

const LIFECYCLE_SUMMARIES = defineSummaries({
  id: "test-lifecycle-summaries",
  entries: [
    ...LIFECYCLE_ALLOC_SUMMARIES.entries,
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
        memberPath: ["Disposable", "prototype", "dispose"],
      },
      effects: {
        completeness: "complete",
        resourceOps: [{ kind: "disposes-target", target: "receiver" }],
      },
    },
    {
      match: {
        package: "@acme/lifecycle",
        memberPath: ["DisposableStore", "prototype", "dispose"],
      },
      effects: {
        completeness: "complete",
        resourceOps: [{ kind: "disposes-target", target: "receiver" }],
      },
    },
    {
      match: {
        package: "@acme/lifecycle",
        memberPath: ["DisposableStore", "prototype", "add"],
      },
      instances: {
        completeness: "complete",
        outcome: { kind: "forwards-instance", from: { argIndex: 0 } },
      },
      effects: {
        completeness: "complete",
        resourceOps: [
          {
            kind: "registers-into-container",
            item: { argIndex: 0 },
            container: "receiver",
          },
        ],
      },
    },
    {
      match: {
        package: "@acme/lifecycle",
        memberPath: ["DisposableStore", "prototype", "delete"],
      },
      effects: {
        completeness: "complete",
        resourceOps: [
          {
            kind: "detaches-from-container",
            item: { argIndex: 0 },
            container: "receiver",
          },
        ],
      },
    },
    {
      match: {
        package: "@acme/lifecycle",
        memberPath: ["MutableDisposable", "prototype", "dispose"],
      },
      effects: {
        completeness: "complete",
        resourceOps: [{ kind: "disposes-target", target: "receiver" }],
      },
    },
    {
      match: {
        package: "@acme/lifecycle",
        memberPath: ["MutableDisposable", "prototype", "clear"],
      },
      effects: {
        completeness: "complete",
        resourceOps: [{ kind: "disposes-target", target: "receiver" }],
      },
    },
    {
      match: {
        package: "@acme/lifecycle",
        memberPath: ["MutableDisposable", "prototype", "value"],
      },
      effects: {
        completeness: "complete",
        resourceOps: [{ kind: "replaces-previous-value", target: "receiver" }],
      },
    },
    {
      match: {
        package: "@acme/lifecycle",
        memberPath: ["onDidDisconnect"],
      },
      calls: {
        completeness: "complete",
        invokesCallbacks: [{ callbackArgIndex: 0, timing: "lifecycle-end" }],
      },
    },
  ],
});

void describe("tssae call-effects and instanceCleanup", () => {
  void it("identifies both target A and wrongly disposed B, preserves conclusions across cross-file helpers/wrappers, distinguishes .value/.clear() vs .delete(), and retains known effects with dynamic Gaps", async () => {
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

        const recorded: Record<string, InstanceCleanupScopeResult> = {};
        const instancesByName = new Map<string, InstanceRef>();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { packs: [LIFECYCLE_SUMMARIES] },
        });
        try {
          const ctx = session.project();
          const fns = await ctx.findSyntax({
            files: ["src/scenarios.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const varDecls = await ctx.findSyntax({
            files: ["src/scenarios.ts"],
            kinds: ["VariableDeclaration"],
          });
          const calls = await ctx.findSyntax({
            files: ["src/scenarios.ts"],
            kinds: ["CallExpression"],
          });

          const getVarInitInFn = (
            fnName: string,
            varName: string,
          ): SyntaxRef => {
            const fn = fns.find((f) => f.text.includes(`function ${fnName}(`))!;
            const decl = varDecls.find(
              (v) =>
                v.location.start >= fn.location.start &&
                v.location.end <= fn.location.end &&
                v.text.startsWith(`${varName} =`),
            )!;
            return decl.initializer ?? decl.ref;
          };

          const resolveVar = async (
            fnName: string,
            varName: string,
          ): Promise<InstanceRef> => {
            const target = getVarInitInFn(fnName, varName);
            const [res] = await ctx.resolveInstances([target]);
            assert.ok(
              res?.status === "proved" && res.instance !== null,
              `expected instance for ${fnName}.${varName}`,
            );
            instancesByName.set(`${fnName}.${varName}`, res.instance);
            return res.instance;
          };

          const getDisconnectCall = (fnName: string): SyntaxRef => {
            const fn = fns.find((f) => f.text.includes(`function ${fnName}(`))!;
            return calls.find(
              (c) =>
                c.location.start >= fn.location.start &&
                c.location.end <= fn.location.end &&
                c.text.startsWith("onDidDisconnect("),
            )!.ref;
          };

          // 1. Close A but clean B
          const instA1 = await resolveVar("closeAButCleanB", "resourceA");
          const instB1 = await resolveVar("closeAButCleanB", "resourceB");
          const [res1] = await ctx.instanceCleanup({
            scopes: [getDisconnectCall("closeAButCleanB")],
            targets: [instA1],
          });
          recorded.closeAButCleanB = res1!;
          const wrong = res1!.disposedInstances.find(
            (d) => !isSameInstance(d.instance, instA1),
          );
          assert.ok(wrong && isSameInstance(wrong.instance, instB1));

          // 2. Extracted cross-file helper (multi-hop)
          const instA2 = await resolveVar(
            "extractedHelperCleanup",
            "resourceA",
          );
          const [res2] = await ctx.instanceCleanup({
            scopes: [getDisconnectCall("extractedHelperCleanup")],
            targets: [instA2],
            lookupFiles: ["src/scenarios.ts"],
          });
          recorded.extractedHelperCleanup = res2!;

          // 3. Container wrapper ({ dispose: () => store.dispose() })
          const instA3 = await resolveVar(
            "containerWrapperCleanup",
            "resourceA",
          );
          const instB3 = await resolveVar(
            "containerWrapperCleanup",
            "resourceB",
          );
          const [containerWrapperNarrow] = await ctx.instanceCleanup({
            scopes: [getDisconnectCall("containerWrapperCleanup")],
            targets: [instA3, instB3],
            lookupFiles: ["src/scenarios.ts"],
          });
          recorded.containerWrapperNarrow = containerWrapperNarrow!;
          const [res3] = await ctx.instanceCleanup({
            scopes: [getDisconnectCall("containerWrapperCleanup")],
            targets: [instA3, instB3],
            lookupFiles: ["src/scenarios.ts", "src/helpers.ts"],
          });
          recorded.containerWrapperCleanup = res3!;

          // 4. MutableDisposable.value = ... and .clear()
          const firstInst = await resolveVar("mutableReplaceAndClear", "first");
          const secondInst = await resolveVar(
            "mutableReplaceAndClear",
            "second",
          );
          const replaceCb = getVarInitInFn(
            "mutableReplaceAndClear",
            "replaceWrite",
          );
          const clearCb = getVarInitInFn("mutableReplaceAndClear", "clearSlot");
          const [resReplace, resClear] = await ctx.instanceCleanup({
            scopes: [replaceCb, clearCb],
            targets: [firstInst, secondInst],
          });
          recorded.mutableReplace = resReplace!;
          recorded.mutableClear = resClear!;

          // 5. Detach via .delete() without dispose
          const connInst = await resolveVar("detachWithoutDispose", "conn");
          const [resDetach] = await ctx.instanceCleanup({
            scopes: [getDisconnectCall("detachWithoutDispose")],
            targets: [connInst],
          });
          recorded.detachWithoutDispose = resDetach!;

          // 6. Conditional, multi-branch, and dead cleanup
          const resPartial = await resolveVar(
            "conditionalAndDeadCleanup",
            "resPartial",
          );
          const resCovered = await resolveVar(
            "conditionalAndDeadCleanup",
            "resCovered",
          );
          const resDead = await resolveVar(
            "conditionalAndDeadCleanup",
            "resDead",
          );
          const [partialRes] = await ctx.instanceCleanup({
            scopes: [getVarInitInFn("conditionalAndDeadCleanup", "partialCb")],
            targets: [resPartial],
          });
          const [multiRes] = await ctx.instanceCleanup({
            scopes: [
              getVarInitInFn("conditionalAndDeadCleanup", "multiBranchCb"),
            ],
            targets: [resCovered],
          });
          const [deadRes] = await ctx.instanceCleanup({
            scopes: [getVarInitInFn("conditionalAndDeadCleanup", "deadCb")],
            targets: [resDead],
          });
          recorded.partial = partialRes!;
          recorded.multiBranch = multiRes!;
          recorded.dead = deadRes!;

          // 7. Dynamic callback alongside known static cleanup
          const dynA = await resolveVar("dynamicCallbackMix", "resA");
          const dynB = await resolveVar("dynamicCallbackMix", "resB");
          const [knownDynRes] = await ctx.instanceCleanup({
            scopes: [getVarInitInFn("dynamicCallbackMix", "knownPlusDyn")],
            targets: [dynA],
          });
          const [wrongDynRes] = await ctx.instanceCleanup({
            scopes: [getVarInitInFn("dynamicCallbackMix", "wrongPlusDyn")],
            targets: [dynA],
          });
          recorded.knownPlusDyn = knownDynRes!;
          recorded.wrongPlusDyn = wrongDynRes!;
          void dynB;
        } finally {
          await session.close();
        }

        // Verify close A clean B
        const closeAB = recorded.closeAButCleanB;
        const closeABTarget = closeAB.targetResults[0]!;
        assert.equal(closeABTarget.status, "disproved");
        assert.equal(closeABTarget.cleanedOnAnyPath, false);
        assert.equal(closeAB.disposedInstances.length, 1);
        assert.ok(
          isSameInstance(
            closeAB.disposedInstances[0]!.instance,
            instancesByName.get("closeAButCleanB.resourceB"),
          ),
        );
        assert.ok(closeAB.disposedInstances[0]!.allocationLocation !== null);

        // Verify cross-file helper multi-hop forwarding
        const helperRes = recorded.extractedHelperCleanup;
        assert.equal(helperRes.targetResults[0]!.status, "proved");
        assert.ok(
          helperRes.targetResults[0]!.evidence.some(
            (e) =>
              e.message.includes("cleanupHelper") ||
              e.message.includes("finalRelease"),
          ),
        );

        // Verify container wrapper only cleans wrapped A, not B
        const wrapRes = recorded.containerWrapperCleanup;
        assert.equal(wrapRes.targetResults[0]!.status, "proved");
        assert.equal(wrapRes.targetResults[1]!.status, "disproved");
        assert.equal(
          recorded.containerWrapperNarrow.targetResults[0]!.status,
          "disproved",
        );
        assert.equal(
          recorded.containerWrapperNarrow.targetResults[1]!.status,
          "disproved",
        );

        // Verify MutableDisposable.value = second disposes first, and .clear() disposes second
        const repRes = recorded.mutableReplace;
        assert.equal(repRes.targetResults[0]!.status, "proved");
        assert.equal(
          repRes.targetResults[0]!.cleanupSites[0]!.operationKind,
          "replaces-previous-value",
        );
        assert.equal(repRes.targetResults[1]!.status, "disproved");
        const clrRes = recorded.mutableClear;
        assert.equal(clrRes.targetResults[1]!.status, "proved");

        // Verify .delete() detaches without disposing
        const detRes = recorded.detachWithoutDispose;
        assert.equal(detRes.targetResults[0]!.status, "disproved");
        assert.equal(detRes.disposedInstances.length, 0);
        assert.equal(detRes.detachedInstances.length, 1);
        assert.ok(
          isSameInstance(
            detRes.detachedInstances[0]!.instance,
            instancesByName.get("detachWithoutDispose.conn"),
          ),
        );

        // Verify partial, multi-branch covered, and dead
        const partialTarget = recorded.partial.targetResults[0]!;
        assert.equal(partialTarget.status, "disproved");
        assert.equal(partialTarget.cleanedOnAnyPath, true);
        assert.ok(partialTarget.uncleanedPath.length > 0);

        assert.equal(recorded.multiBranch.targetResults[0]!.status, "proved");

        const deadTarget = recorded.dead.targetResults[0]!;
        assert.equal(deadTarget.status, "disproved");
        assert.equal(deadTarget.cleanedOnAnyPath, false);

        // Verify dynamic callback preserves known effects and gaps
        const kDyn = recorded.knownPlusDyn;
        assert.equal(kDyn.targetResults[0]!.status, "proved");
        assert.equal(kDyn.disposedInstances.length, 1);
        assert.ok(kDyn.gaps.some((g) => g.kind === "dynamic-callee"));

        const wDyn = recorded.wrongPlusDyn;
        const wDynTarget = wDyn.targetResults[0]!;
        // Under IR optional-chaining semantics (`extraCb?.()`), the nullish branch exits
        // without calling `extraCb` or disposing `resA`, yielding a known counterexample.
        assert.equal(wDynTarget.status, "disproved");
        assert.ok(wDynTarget.uncleanedPath.length > 0);
        assert.equal(wDyn.disposedInstances.length, 1);
        assert.ok(
          isSameInstance(
            wDyn.disposedInstances[0]!.instance,
            instancesByName.get("dynamicCallbackMix.resB"),
          ),
        );
        assert.ok(wDyn.gaps.some((g) => g.kind === "dynamic-callee"));
      },
    );
  });

  void it("records allocation, cleanup location, and model provenance evidence when cleanup covers an instance", async () => {
    await withTempHost(
      {
        host: "sdk-queries/call-effects-explain",
        setup: (host) => {
          copyDirectory(
            resolveHostDir("sdk-queries/acme-lifecycle"),
            host.resolvePath("node_modules/@acme/lifecycle"),
          );
        },
      },
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { packs: [LIFECYCLE_SUMMARIES] },
        });

        try {
          const ctx = session.project();
          const vars = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["VariableDeclaration"],
          });
          const calls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const connVar = vars.find((v) => v.text.startsWith("conn ="));
          const disconnectCall = calls.find((c) =>
            c.text.startsWith("onDidDisconnect("),
          );
          assert.ok(connVar?.initializer && disconnectCall);

          const [resolved] = await ctx.resolveInstances([connVar.initializer]);
          assert.ok(
            resolved?.status === "proved" && resolved.instance !== null,
          );

          const [cleanup] = await ctx.instanceCleanup({
            scopes: [disconnectCall.ref],
            targets: [resolved.instance],
          });
          const targetRes = cleanup?.targetResults[0];
          assert.ok(targetRes);
          assert.equal(targetRes.status, "proved");
          assert.equal(targetRes.allocationLocation?.file, "src/app.ts");
          assert.equal(targetRes.allocationLocation?.line, 6);
          assert.ok(
            targetRes.cleanupSites.some(
              (s) =>
                (s.location.file === "src/app.ts" && s.location.line === 8) ||
                (s.viaCallLocation?.file === "src/app.ts" &&
                  s.viaCallLocation.line === 8),
            ),
          );
          const evidence = evidenceOf(targetRes);
          assert.equal(
            targetRes.cleanupSites[0]?.summaryId,
            "test-lifecycle-summaries",
          );
          assert.ok(
            evidence.some((e) => e.message.includes("releaseResource")),
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("distinguishes covered-branch-but-raw-argument from validated-output using the same query set", async () => {
    await withTempHost(
      { host: "sdk-queries/call-effects-validation" },
      async (host) => {
        await host.typecheck();

        const routeAudit: Record<
          string,
          {
            readonly branchStatus: string;
            readonly rawPropagated: boolean;
            readonly parsedDataPropagated: boolean;
          }
        > = {};
        const flaggedRoutes: Array<{
          readonly file: string;
          readonly line: number;
        }> = [];

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const fns = await ctx.findSyntax({
            files: ["src/routes.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const calls = await ctx.findSyntax({
            files: ["src/routes.ts"],
            kinds: ["CallExpression"],
          });
          const branchesList = await ctx.listBranches(fns.map((fn) => fn.ref));
          const fnEffects = await ctx.callEffects({
            targets: fns.map((fn) => fn.ref),
          });

          for (const [fnIdx, fn] of fns.entries()) {
            const fnName = fn.text.includes("unsafeRoute")
              ? "unsafeRoute"
              : "safeRoute";
            const rawParam = fn.parameters?.[0];
            const handlerCall = calls.find(
              (c) =>
                c.location.start >= fn.location.start &&
                c.location.end <= fn.location.end &&
                c.text.startsWith("handler("),
            );
            const validateCall = calls.find(
              (c) =>
                c.location.start >= fn.location.start &&
                c.location.end <= fn.location.end &&
                c.text.startsWith("validate("),
            );
            if (!rawParam || !handlerCall || !validateCall) continue;

            const [resolvedHandler] = await ctx.resolveCalls(
              [handlerCall.ref],
              {
                select: { arguments: true },
              },
            );
            const handlerArg = resolvedHandler?.arguments?.[0];
            if (!handlerArg) continue;

            // 1. Check control-flow branch coverage through `!parsed.ok` false branch (or early-return guard)
            const fnBranches = branchesList[fnIdx]?.branches ?? [];
            const okGuardBranch = fnBranches.find(
              (b) =>
                b.conditionText.includes("parsed.ok") &&
                b.direction === "false",
            );
            const coverage = await ctx.branchCoverage({
              function: fn.ref,
              branches: okGuardBranch ? [okGuardBranch.ref] : [],
              targets: [handlerCall.ref],
            });

            // 2. Collect field reads (`parsed.data`) via callEffects
            const parsedDataReads = fnEffects[fnIdx]!.fieldReads.filter(
              (r) => r.propertyName === "data",
            );

            // 3. Check valuePropagation from `raw` AND from `parsed.data` to `handlerArg`
            const rawProp = await ctx.valuePropagation({
              from: rawParam,
              to: handlerArg,
            });
            const dataProp =
              parsedDataReads.length > 0
                ? await ctx.valuePropagation({
                    from: parsedDataReads.map((r) => r.syntax),
                    to: handlerArg,
                  })
                : null;

            const branchStatus = coverage[0]?.status ?? "inconclusive";
            const rawPropagated = rawProp.reachableTargets.length > 0;
            const parsedDataPropagated =
              (dataProp?.reachableTargets.length ?? 0) > 0;

            routeAudit[fnName] = {
              branchStatus,
              rawPropagated,
              parsedDataPropagated,
            };

            if (
              branchStatus !== "proved" ||
              rawPropagated ||
              !parsedDataPropagated
            ) {
              flaggedRoutes.push({
                file: handlerCall.location.file,
                line: handlerCall.location.line,
              });
            }
          }
        } finally {
          await session.close();
        }

        assert.deepEqual(routeAudit.unsafeRoute, {
          branchStatus: "proved",
          rawPropagated: true,
          parsedDataPropagated: false,
        });
        assert.deepEqual(routeAudit.safeRoute, {
          branchStatus: "proved",
          rawPropagated: false,
          parsedDataPropagated: true,
        });
        assert.deepEqual(flaggedRoutes, [{ file: "src/routes.ts", line: 8 }]);
      },
    );
  });

  void it("traces field reads/writes across cross-file callback helpers and checks branch guards on reentrant scheduler flags", async () => {
    await withTempHost(
      { host: "sdk-queries/call-effects-scheduler" },
      async (host) => {
        await host.typecheck();

        const effectsByFn = new Map<string, CallEffectsResult>();
        const postCallGuardStatus: Record<string, string> = {};
        const flaggedFns: string[] = [];

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const fns = await ctx.findSyntax({
            files: ["src/scheduler.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const effectsList = await ctx.callEffects({
            targets: fns.map((f) => f.ref),
          });
          const branchGroups = await ctx.listBranches(fns.map((f) => f.ref));

          for (const [idx, fn] of fns.entries()) {
            const nameMatch = /function\s+(\w+)/.exec(fn.text);
            const fnName = nameMatch?.[1] ?? `fn_${idx}`;
            const eff = effectsList[idx]!;
            effectsByFn.set(fnName, eff);

            if (!fnName.startsWith("flushJob")) continue;

            // Find post-call `job.flags! &= ~SchedulerJobFlags.QUEUED` write directly inside `fn`
            const directFlagClears = eff.fieldWrites.filter(
              (w) =>
                w.propertyName === "flags" &&
                w.operator === "&=" &&
                (w.valueText?.includes("QUEUED") ?? false) &&
                w.viaCall === null,
            );
            const postCallClear = directFlagClears.at(-1);
            assert.ok(
              postCallClear,
              `expected post-call clear write in ${fnName}`,
            );

            // Verify receiverSyntax can be chained into typeAt/resolveInstances
            const [recvType] = await ctx.typeAt([postCallClear.receiverSyntax]);
            assert.equal(recvType?.typeText, "SchedulerJob");

            // Verify postCallClear.syntax can be chained directly into branchCoverage
            const recurseFalseBranches = branchGroups[idx]!.branches.filter(
              (b) =>
                b.conditionText.includes("ALLOW_RECURSE") &&
                ((b.conditionText.startsWith("!") && b.direction === "true") ||
                  (!b.conditionText.startsWith("!") &&
                    b.direction === "false")),
            );
            const [cov] = await ctx.branchCoverage({
              function: fn.ref,
              branches: recurseFalseBranches.map((b) => b.ref),
              targets: [postCallClear.syntax],
            });
            postCallGuardStatus[fnName] = cov!.status;

            if (cov!.status !== "proved") {
              flaggedFns.push(fnName);
            }
          }
        } finally {
          await session.close();
        }

        // queueJob sets QUEUED with |=
        const queueJobEff = effectsByFn.get("queueJob")!;
        const queuedSet = queueJobEff.fieldWrites.find(
          (w) => w.propertyName === "flags" && w.operator === "|=",
        );
        assert.ok(queuedSet);
        assert.ok(queuedSet.valueText?.includes("SchedulerJobFlags.QUEUED"));

        // flushJobBuggy traces through errorHandling.ts -> callback -> queueJob -> job.flags |= QUEUED!
        const buggyEff = effectsByFn.get("flushJobBuggy")!;
        assert.ok(
          buggyEff.callbackInvocations.some(
            (cb) =>
              cb.location.file === "src/errorHandling.ts" &&
              cb.parameterIndex === 0,
          ),
        );
        const indirectQueuedSet = buggyEff.fieldWrites.find(
          (w) => w.propertyName === "flags" && w.operator === "|=",
        );
        assert.ok(indirectQueuedSet);
        assert.ok(indirectQueuedSet.viaCall !== null);
        assert.ok(
          indirectQueuedSet.evidence.some((e) =>
            e.message.includes("callWithErrorHandling"),
          ),
        );
        assert.ok(
          indirectQueuedSet.evidence.some((e) =>
            e.message.includes("queueJob"),
          ),
        );

        // flushJobDynamic preserves direct writes AND records callbackInvocation + dynamic-callee Gap
        const dynEff = effectsByFn.get("flushJobDynamic")!;
        assert.equal(dynEff.completeness, "incomplete");
        assert.ok(dynEff.fieldWrites.length >= 2);
        assert.ok(
          dynEff.callbackInvocations.some(
            (cb) => cb.location.file === "src/errorHandling.ts",
          ),
        );
        assert.ok(dynEff.gaps.some((g) => g.kind === "dynamic-callee"));

        // branchCoverage distinguishes unguarded vs guarded post-call clear
        assert.equal(postCallGuardStatus.flushJobBuggy, "disproved");
        assert.equal(postCallGuardStatus.flushJobDynamic, "disproved");
        assert.equal(postCallGuardStatus.flushJobFixed, "proved");

        // flushJobFixed's clear also records structural guard
        const fixedEff = effectsByFn.get("flushJobFixed")!;
        const fixedPostClear = fixedEff.fieldWrites
          .filter((w) => w.operator === "&=" && w.viaCall === null)
          .at(-1)!;
        assert.ok(
          fixedPostClear.guards.some(
            (g) =>
              g.conditionText.includes("ALLOW_RECURSE") &&
              g.direction === "true",
          ),
        );

        assert.deepEqual(flaggedFns, ["flushJobBuggy", "flushJobDynamic"]);
      },
    );
  });

  void it("rejects stale references in callEffects/instanceCleanup and invalidates cleanup cache when summaries update", async () => {
    await withTempHost(
      {
        host: "sdk-queries/call-effects-model-update",
        setup: (host) => {
          copyDirectory(
            resolveHostDir("sdk-queries/acme-lifecycle"),
            host.resolvePath("node_modules/@acme/lifecycle"),
          );
        },
      },
      async (host) => {
        await host.typecheck();

        const queryCleanupStatus = async (
          ctx: ReturnType<Awaited<ReturnType<typeof openProject>>["project"]>,
        ) => {
          const vars = await ctx.findSyntax({
            kinds: ["VariableDeclaration"],
          });
          const itemVar = vars.find((v) => v.text.startsWith("item ="))!;
          const closeVar = vars.find((v) => v.text.startsWith("close ="))!;
          const scope = closeVar.initializer!;
          const [instRes] = await ctx.resolveInstances([itemVar.initializer!]);
          assert.ok(instRes?.status === "proved" && instRes.instance !== null);
          const inst = instRes.instance;
          const [cleanupRes] = await ctx.instanceCleanup({
            scopes: [scope],
            targets: [inst],
          });
          return {
            scope,
            inst,
            status: cleanupRes!.targetResults[0]!.status,
          };
        };

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { packs: [LIFECYCLE_ALLOC_SUMMARIES] },
        });

        try {
          const first = await queryCleanupStatus(session.project());
          // Without cleanup summary, external `.dispose()` is unmodeled and yields `inconclusive`
          assert.equal(first.status, "inconclusive");

          // Update summaries to add `disposes-target` for `.dispose()` -> must invalidate cache and return `proved`
          await session.update({
            summaries: { packs: [LIFECYCLE_SUMMARIES] },
          });
          const ctx2 = session.project();
          const second = await queryCleanupStatus(ctx2);
          assert.equal(second.status, "proved");

          // Using stale refs captured from snapshot 1 in snapshot 2 must throw STALE_REFERENCE
          await assert.rejects(
            () => ctx2.callEffects({ targets: [first.scope] }),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );
          await assert.rejects(
            async () => {
              const fns = await ctx2.findSyntax({
                kinds: ["FunctionDeclaration"],
              });
              await ctx2.instanceCleanup({
                scopes: [fns[0]!.ref],
                targets: [first.inst],
              });
            },
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("exposes canonical receiver/helper/target DeclarationRefs, direct & indirect CallbackInvocationInfo, cfgScope, and per-item basis provenance", async () => {
    await withTempHost(
      { host: "sdk-queries/call-effects-canonical" },
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
          const helperFns = await ctx.findSyntax({
            files: ["src/helper.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const helperDecl = (
            await ctx.resolveDeclarations([helperFns[0]!.ref])
          )[0]!.declaration!;

          const runFn = fns.find((f) =>
            f.text.includes("function runDirectAndIndirect"),
          )!;
          const concreteFn = fns.find((f) =>
            f.text.includes("function runWithConcreteCallback"),
          )!;

          const [runEff, concreteEff] = await ctx.callEffects({
            targets: [runFn.ref, concreteFn.ref],
            followCallbacks: true,
          });
          assert.ok(runEff && concreteEff);

          const clearWrites = runEff.fieldWrites.filter(
            (w) => w.propertyName === "flags" && w.operator === "&=",
          );
          assert.equal(clearWrites.length, 2);
          const guardedWrite = clearWrites[0]!;
          const unguardedWrite = clearWrites[1]!;

          const resolvedReceivers = await ctx.resolveDeclarations([
            guardedWrite.receiverSyntax,
            unguardedWrite.receiverSyntax,
          ]);
          assert.equal(
            guardedWrite.receiverDeclaration?.handleId,
            resolvedReceivers[0]!.declaration?.handleId,
          );
          assert.equal(
            unguardedWrite.receiverDeclaration?.handleId,
            resolvedReceivers[1]!.declaration?.handleId,
          );
          assert.ok(guardedWrite.enclosingBlock !== null);
          assert.ok(guardedWrite.cfgScope !== null);

          const directInv = runEff.callbackInvocations.find(
            (inv) => inv.kind === "direct",
          );
          const indirectInv = runEff.callbackInvocations.find(
            (inv) => inv.kind === "indirect",
          );
          assert.ok(directInv);
          assert.ok(indirectInv);

          assert.equal(directInv.parameterIndex, null);
          assert.equal(directInv.helperDeclaration, null);
          assert.equal(
            directInv.targetDeclaration?.handleId,
            guardedWrite.receiverDeclaration?.handleId,
          );
          assert.equal(directInv.outerCall.handleId, directInv.call.handleId);
          assert.equal(directInv.outerCallLocation.line, 18);
          assert.equal(
            directInv.enclosingFunction?.handleId,
            runFn.ref.handleId,
          );
          assert.ok(directInv.cfgScope !== null);

          assert.equal(indirectInv.parameterIndex, 0);
          assert.equal(
            indirectInv.helperDeclaration?.handleId,
            helperDecl.handleId,
          );
          assert.equal(
            indirectInv.targetDeclaration?.handleId,
            unguardedWrite.receiverDeclaration?.handleId,
          );
          assert.equal(indirectInv.outerCallLocation.file, "src/queue.ts");
          assert.equal(indirectInv.outerCallLocation.line, 27);
          assert.equal(indirectInv.location.file, "src/helper.ts");
          assert.equal(indirectInv.location.line, 3);
          assert.equal(
            indirectInv.enclosingFunction?.handleId,
            runFn.ref.handleId,
          );
          assert.equal(
            indirectInv.cfgScope?.handleId,
            unguardedWrite.cfgScope?.handleId,
          );

          // Concrete callback targetDeclaration & resolvedTargets share canonical DeclarationRef identity
          const concreteInv = concreteEff.callbackInvocations.find(
            (inv) => inv.kind === "indirect",
          );
          assert.ok(concreteInv);
          const [argDecl] = await ctx.resolveDeclarations([
            concreteInv.callbackArgument,
          ]);
          assert.equal(
            concreteInv.targetDeclaration?.handleId,
            argDecl?.declaration?.handleId,
          );
          assert.equal(
            concreteInv.resolvedTargets[0]?.handleId,
            argDecl?.declaration?.handleId,
          );

          // Local CFG scope from cfgScope evaluates branchCoverage accurately inside try/for loops
          assert.equal(
            guardedWrite.cfgScope?.handleId,
            directInv.cfgScope?.handleId,
          );
          const [branchGroup] = await ctx.listBranches([guardedWrite.cfgScope]);
          const guardBranch = branchGroup!.branches.find(
            (b) =>
              b.conditionText.includes("cb.flags & 4") &&
              b.direction === "true",
          );
          assert.ok(guardBranch);
          const [guardedCov] = await ctx.branchCoverage({
            scope: guardedWrite.cfgScope,
            branches: [guardBranch.ref],
            targets: [guardedWrite.syntax],
          });
          assert.equal(guardedCov?.status, "proved");

          const [unguardedCov] = await ctx.branchCoverage({
            scope: unguardedWrite.cfgScope!,
            branches: [],
            targets: [unguardedWrite.syntax],
          });
          assert.equal(unguardedCov?.status, "disproved");

          const evidence = extractBasisEvidence([
            indirectInv,
            unguardedWrite,
            unguardedCov,
          ]);
          assert.ok(
            evidence.some(
              (e) => e.at?.file === "src/queue.ts" && e.at.line === 27,
            ),
          );
          assert.ok(
            evidence.some(
              (e) => e.at?.file === "src/helper.ts" && e.at.line === 1,
            ),
          );
          assert.ok(
            evidence.some(
              (e) => e.at?.file === "src/helper.ts" && e.at.line === 3,
            ),
          );
          assert.ok(
            evidence.some(
              (e) => e.at?.file === "src/queue.ts" && e.at.line === 28,
            ),
          );
          // Unrelated guarded write & queue.length = 0 must not leak
          assert.equal(
            evidence.some(
              (e) =>
                e.at?.file === "src/queue.ts" &&
                (e.at.line === 20 || e.at.line === 32),
            ),
            false,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("evaluates dual explicit/implicit-exception cleanup verdicts for mayThrow writes, try/finally, explicit throw, using [Symbol.dispose], extracted helpers, and multi-instance disambiguation", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "cleanup-dual-verdicts-host",
            private: true,
            type: "module",
          }),
          "tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2024",
              lib: ["ES2024", "ESNext.Disposable"],
              module: "NodeNext",
              moduleResolution: "NodeNext",
            },
            include: ["src/**/*.ts"],
          }),
          "node_modules/@acme/io/package.json": JSON.stringify({
            name: "@acme/io",
            version: "1.0.0",
            types: "./index.d.ts",
          }),
          "node_modules/@acme/io/index.d.ts": [
            "export interface Handle extends Disposable {",
            "  write(data: string): void;",
            "}",
            "export declare function acquire(): Handle;",
            "export declare function release(h: Handle): void;",
            "export declare function write(h: Handle): void;",
            "",
          ].join("\n"),
          "src/helpers.ts": [
            'import { release, type Handle } from "@acme/io";',
            "export function closeQuietly(h: Handle): void {",
            "  release(h);",
            "}",
            "",
          ].join("\n"),
          "src/scenarios.ts": [
            'import { acquire, release, write, type Handle } from "@acme/io";',
            'import { closeQuietly } from "./helpers.js";',
            "",
            "export function acquireWriteRelease(): void {",
            "  const r = acquire();",
            "  write(r);",
            "  release(r);",
            "}",
            "",
            "export function tryFinallyRelease(): void {",
            "  const r = acquire();",
            "  try {",
            "    write(r);",
            "  } finally {",
            "    release(r);",
            "  }",
            "}",
            "",
            "export function explicitThrowSkipsRelease(bad: boolean): void {",
            "  const r = acquire();",
            "  if (bad) {",
            '    throw new Error("boom");',
            "  }",
            "  release(r);",
            "}",
            "",
            "export function usingAutoDispose(): void {",
            "  using r = acquire();",
            "  write(r);",
            "}",
            "",
            "export function helperExtractedRelease(protectedByFinally: boolean): void {",
            "  const direct = acquire();",
            "  if (protectedByFinally) {",
            "    try {",
            "      write(direct);",
            "    } finally {",
            "      closeQuietly(direct);",
            "    }",
            "  } else {",
            "    write(direct);",
            "    closeQuietly(direct);",
            "  }",
            "}",
            "",
            "export function multiInstanceOnlyCleansB(): void {",
            "  const a = acquire();",
            "  const b = acquire();",
            "  try {",
            "    write(b);",
            "  } finally {",
            "    closeQuietly(b);",
            "  }",
            "  void a;",
            "}",
            "",
            "export function unknownDynamicCleanup(cb: (h: Handle) => void): void {",
            "  const r = acquire();",
            "  cb(r);",
            "}",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        await host.typecheck();

        const ioSummaries = defineSummaries({
          id: "acme-io-summaries",
          entries: [
            {
              match: { package: "@acme/io", memberPath: ["acquire"] },
              instances: {
                completeness: "complete",
                outcome: { kind: "allocates-fresh-instance" },
              },
            },
            {
              match: { package: "@acme/io", memberPath: ["release"] },
              effects: {
                completeness: "complete",
                resourceOps: [
                  { kind: "disposes-target", target: { argIndex: 0 } },
                ],
              },
            },
            {
              match: { package: "@acme/io", memberPath: ["write"] },
              effects: {
                completeness: "complete",
                resourceOps: [],
              },
            },
          ],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { packs: [ioSummaries] },
        });
        try {
          const ctx = session.project();
          const fns = await ctx.findSyntax({
            files: ["src/scenarios.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const varDecls = await ctx.findSyntax({
            files: ["src/scenarios.ts"],
            kinds: ["VariableDeclaration"],
          });

          const getFn = (name: string) =>
            fns.find((f) => f.text.includes(`function ${name}(`))!;
          const resolveVar = async (fnName: string, varName: string) => {
            const fn = getFn(fnName);
            const decl = varDecls.find(
              (v) =>
                v.location.start >= fn.location.start &&
                v.location.end <= fn.location.end &&
                v.text.startsWith(`${varName} =`),
            )!;
            const [res] = await ctx.resolveInstances([decl.initializer!]);
            assert.ok(
              res?.status === "proved" && res.instance !== null,
              `expected resolved instance for ${fnName}.${varName}`,
            );
            return res.instance;
          };

          // 1. acquire -> write -> release (no try/finally): primary proved, implicitExceptions disproved at write(r)
          const r1 = await resolveVar("acquireWriteRelease", "r");
          const [res1] = await ctx.instanceCleanup({
            scopes: [getFn("acquireWriteRelease").ref],
            targets: [r1],
          });
          const t1 = res1!.targetResults[0]!;
          assert.equal(t1.status, "proved");
          assert.equal(t1.implicitExceptions.status, "disproved");
          assert.ok(
            t1.implicitExceptions.mayThrowSites.some(
              (s) => s.file === "src/scenarios.ts" && s.line === 6,
            ),
          );
          assert.ok(t1.implicitExceptions.counterexamplePath.length > 0);
          assert.ok(
            evidenceOf(t1.implicitExceptions).some(
              (e) => e.at?.file === "src/scenarios.ts" && e.at.line === 6,
            ),
          );

          // 2. try { write(r); } finally { release(r); }: both primary & implicitExceptions proved
          const r2 = await resolveVar("tryFinallyRelease", "r");
          const [res2] = await ctx.instanceCleanup({
            scopes: [getFn("tryFinallyRelease").ref],
            targets: [r2],
          });
          const t2 = res2!.targetResults[0]!;
          assert.equal(t2.status, "proved");
          assert.equal(t2.implicitExceptions.status, "proved");

          // 3. Explicit throw skipping release: primary directly disproved with uncleanedPath
          const r3 = await resolveVar("explicitThrowSkipsRelease", "r");
          const [res3] = await ctx.instanceCleanup({
            scopes: [getFn("explicitThrowSkipsRelease").ref],
            targets: [r3],
          });
          const t3 = res3!.targetResults[0]!;
          assert.equal(t3.status, "disproved");
          assert.equal(t3.cleanedOnAnyPath, true);
          assert.ok(t3.uncleanedPath.length > 0);

          // 4. `using r = acquire(); write(r);`: stdlib Symbol.dispose gives both proved
          const r4 = await resolveVar("usingAutoDispose", "r");
          const [res4] = await ctx.instanceCleanup({
            scopes: [getFn("usingAutoDispose").ref],
            targets: [r4],
          });
          const t4 = res4!.targetResults[0]!;
          assert.equal(t4.status, "proved");
          assert.equal(t4.implicitExceptions.status, "proved");

          // 5. Cross-helper extraction (`closeQuietly`): preserves dual conclusions (protected finally vs unprotected path)
          const r5 = await resolveVar("helperExtractedRelease", "direct");
          const [res5] = await ctx.instanceCleanup({
            scopes: [getFn("helperExtractedRelease").ref],
            targets: [r5],
          });
          const t5 = res5!.targetResults[0]!;
          assert.equal(t5.status, "proved");
          assert.equal(t5.implicitExceptions.status, "disproved");
          assert.ok(
            t5.implicitExceptions.mayThrowSites.some(
              (s) => s.file === "src/scenarios.ts" && s.line === 41,
            ),
          );

          // 6. Multi-instance disambiguation: a is disproved, b is proved (explicit + implicit)
          const a = await resolveVar("multiInstanceOnlyCleansB", "a");
          const b = await resolveVar("multiInstanceOnlyCleansB", "b");
          const [res6] = await ctx.instanceCleanup({
            scopes: [getFn("multiInstanceOnlyCleansB").ref],
            targets: [a, b],
          });
          const resA = res6!.targetResults[0]!;
          const resB = res6!.targetResults[1]!;
          assert.equal(resA.status, "disproved");
          assert.equal(resA.allocationLocation?.line, 47);
          assert.equal(resB.status, "proved");
          assert.equal(resB.implicitExceptions.status, "proved");
          assert.equal(resB.allocationLocation?.line, 48);
          assert.ok(
            resB.cleanupSites.some(
              (s) =>
                s.location.file === "src/helpers.ts" ||
                s.viaCallLocation?.file === "src/scenarios.ts",
            ),
          );

          // 7. Unconditional dynamic callback yields `inconclusive` with critical dynamic-callee gap
          const r7 = await resolveVar("unknownDynamicCleanup", "r");
          const [res7] = await ctx.instanceCleanup({
            scopes: [getFn("unknownDynamicCleanup").ref],
            targets: [r7],
          });
          const t7 = res7!.targetResults[0]!;
          assert.equal(t7.status, "inconclusive");
          assert.ok(t7.gaps.some((g) => g.kind === "dynamic-callee"));
        } finally {
          await session.close();
        }
      },
    );
  });
});
