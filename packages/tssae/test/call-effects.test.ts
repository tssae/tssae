import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  copyDirectory,
  resolveHostDir,
  withTempHost,
} from "@verule/test-hosts";
import {
  TssaeError,
  defineModel,
  evidenceOf,
  extractBasisEvidence,
  isSameInstance,
  openProject,
  type CallEffectsResult,
  type InstanceCleanupScopeResult,
  type InstanceRef,
  type SyntaxRef,
} from "tssae";

const LIFECYCLE_MODEL = defineModel({
  id: "test-lifecycle-model",
  instanceBehaviors: [
    {
      kind: "allocates-fresh-instance",
      match: { symbol: "createExternalResource" },
      description: "createExternalResource allocates a fresh disposable",
    },
    {
      kind: "forwards-argument",
      match: { container: "Disposable", member: "_register" },
      argumentIndex: 0,
      description: "_register returns its registered argument",
    },
  ],
  cleanupOperations: [
    {
      kind: "disposes-target",
      match: { member: "dispose" },
      target: "receiver",
      description: ".dispose() releases the receiver instance",
    },
    {
      kind: "disposes-target",
      match: { container: "MutableDisposable", member: "clear" },
      target: "receiver",
      description: "MutableDisposable.clear() disposes its held value",
    },
    {
      kind: "replaces-previous-value",
      match: { container: "MutableDisposable", member: "value" },
      target: "receiver",
      description:
        "Assigning MutableDisposable.value disposes the previous held value",
    },
    {
      kind: "registers-into-container",
      match: { container: "DisposableStore", member: "add" },
      target: { argumentIndex: 0 },
      containerTarget: "receiver",
      description: "DisposableStore.add registers child into store",
    },
    {
      kind: "detaches-from-container",
      match: { container: "DisposableStore", member: "delete" },
      target: { argumentIndex: 0 },
      containerTarget: "receiver",
      description: "DisposableStore.delete detaches without disposing",
    },
  ],
  lifecycleEvents: [
    {
      kind: "lifecycle-end-callback",
      match: { symbol: "onDidDisconnect" },
      callbackArgumentIndex: 0,
      eventName: "disconnect",
      description: "onDidDisconnect callback runs when connection ends",
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
          models: [LIFECYCLE_MODEL],
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
              res?.instance,
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
        assert.equal(closeAB.targetResults[0]!.status, "not-cleaned");
        assert.equal(closeAB.targetResults[0]!.cleanedOnAllPaths, false);
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
        assert.equal(helperRes.targetResults[0]!.status, "covered");
        assert.equal(helperRes.targetResults[0]!.cleanedOnAllPaths, true);
        assert.ok(
          helperRes.targetResults[0]!.evidence.some(
            (e) =>
              e.message.includes("cleanupHelper") ||
              e.message.includes("finalRelease"),
          ),
        );

        // Verify container wrapper only cleans wrapped A, not B
        const wrapRes = recorded.containerWrapperCleanup;
        assert.equal(wrapRes.targetResults[0]!.status, "covered");
        assert.equal(wrapRes.targetResults[1]!.status, "not-cleaned");
        assert.equal(
          recorded.containerWrapperNarrow.targetResults[0]!.status,
          "not-cleaned",
        );
        assert.equal(
          recorded.containerWrapperNarrow.targetResults[1]!.status,
          "not-cleaned",
        );

        // Verify MutableDisposable.value = second disposes first, and .clear() disposes second
        const repRes = recorded.mutableReplace;
        assert.equal(repRes.targetResults[0]!.status, "covered");
        assert.equal(
          repRes.targetResults[0]!.cleanupSites[0]!.operationKind,
          "replaces-previous-value",
        );
        assert.equal(repRes.targetResults[1]!.status, "not-cleaned");
        const clrRes = recorded.mutableClear;
        assert.equal(clrRes.targetResults[1]!.status, "covered");

        // Verify .delete() detaches without disposing
        const detRes = recorded.detachWithoutDispose;
        assert.equal(detRes.targetResults[0]!.status, "not-cleaned");
        assert.equal(detRes.disposedInstances.length, 0);
        assert.equal(detRes.detachedInstances.length, 1);
        assert.ok(
          isSameInstance(
            detRes.detachedInstances[0]!.instance,
            instancesByName.get("detachWithoutDispose.conn"),
          ),
        );

        // Verify partial, multi-branch covered, and dead
        assert.equal(recorded.partial.targetResults[0]!.status, "partial");
        assert.equal(recorded.partial.targetResults[0]!.cleanedOnAnyPath, true);
        assert.equal(
          recorded.partial.targetResults[0]!.cleanedOnAllPaths,
          false,
        );

        assert.equal(recorded.multiBranch.targetResults[0]!.status, "covered");
        assert.equal(
          recorded.multiBranch.targetResults[0]!.cleanedOnAllPaths,
          true,
        );

        assert.equal(recorded.dead.targetResults[0]!.status, "not-cleaned");
        assert.equal(recorded.dead.targetResults[0]!.cleanedOnAnyPath, false);

        // Verify dynamic callback preserves known effects and gaps
        const kDyn = recorded.knownPlusDyn;
        assert.equal(kDyn.targetResults[0]!.status, "covered");
        assert.equal(kDyn.disposedInstances.length, 1);
        assert.ok(kDyn.gaps.some((g) => g.kind === "dynamic-callee"));

        const wDyn = recorded.wrongPlusDyn;
        assert.equal(wDyn.targetResults[0]!.status, "unknown");
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
          models: [LIFECYCLE_MODEL],
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
          assert.ok(resolved?.instance);

          const [cleanup] = await ctx.instanceCleanup({
            scopes: [disconnectCall.ref],
            targets: [resolved.instance],
          });
          const targetRes = cleanup?.targetResults[0];
          assert.ok(targetRes);
          assert.equal(targetRes.status, "covered");
          assert.equal(targetRes.cleanedOnAllPaths, true);
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
            targetRes.cleanupSites[0]?.modelId,
            "test-lifecycle-model",
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

            const branchStatus = coverage[0]?.status ?? "unknown";
            const rawPropagated = rawProp.reachableTargets.length > 0;
            const parsedDataPropagated =
              (dataProp?.reachableTargets.length ?? 0) > 0;

            routeAudit[fnName] = {
              branchStatus,
              rawPropagated,
              parsedDataPropagated,
            };

            if (
              branchStatus !== "covered" ||
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
          branchStatus: "covered",
          rawPropagated: true,
          parsedDataPropagated: false,
        });
        assert.deepEqual(routeAudit.safeRoute, {
          branchStatus: "covered",
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

            if (cov!.status !== "covered") {
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
        assert.ok(dynEff.fieldWrites.length >= 2);
        assert.ok(
          dynEff.callbackInvocations.some(
            (cb) => cb.location.file === "src/errorHandling.ts",
          ),
        );
        assert.ok(dynEff.gaps.some((g) => g.kind === "dynamic-callee"));

        // branchCoverage distinguishes unguarded vs guarded post-call clear
        assert.equal(postCallGuardStatus.flushJobBuggy, "bypass");
        assert.equal(postCallGuardStatus.flushJobDynamic, "bypass");
        assert.equal(postCallGuardStatus.flushJobFixed, "covered");

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

  void it("rejects stale references in callEffects/instanceCleanup and invalidates cleanup cache when models update", async () => {
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
          const inst = instRes!.instance!;
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
          models: [
            defineModel({
              id: "base-alloc",
              instanceBehaviors: [
                {
                  kind: "allocates-fresh-instance",
                  match: { symbol: "createExternalResource" },
                },
              ],
            }),
          ],
        });

        try {
          const first = await queryCleanupStatus(session.project());
          // Without cleanupOperations model, external `.dispose()` is unmodeled and yields `unknown`
          assert.equal(first.status, "unknown");

          // Update models to add `disposes-target` for `.dispose()` -> must invalidate cache and return `covered`
          await session.update({
            models: [LIFECYCLE_MODEL],
          });
          const ctx2 = session.project();
          const second = await queryCleanupStatus(ctx2);
          assert.equal(second.status, "covered");

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
          assert.equal(guardedCov?.status, "covered");

          const [unguardedCov] = await ctx.branchCoverage({
            scope: unguardedWrite.cfgScope!,
            branches: [],
            targets: [unguardedWrite.syntax],
          });
          assert.equal(unguardedCov?.status, "bypass");

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
});
