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
  isSameInstance,
  openProject,
  type InstanceRef,
  type InstanceResolutionResult,
  type SyntaxRef,
} from "tssae";

void describe("tssae instance identity", () => {
  void it("distinguishes separate constructor and factory calls while preserving identity across aliases, import renaming, same-name declarations, and cross-file forwarding", async () => {
    await withTempHost(
      { host: "sdk-queries/instance-identity-factories" },
      async (host) => {
        await host.typecheck();

        const resultsByVar = new Map<string, InstanceResolutionResult>();
        let projectedKey: string | undefined;
        let projectedAllocFile: string | undefined;

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const varDecls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["VariableDeclaration"],
          });
          const names = [
            "direct1",
            "direct2",
            "fac1",
            "fac2",
            "aliasFac1",
            "forwardedFac1",
            "forwardedFresh",
            "wrap1",
            "wrap2",
            "otherFac",
            "localFac",
          ];
          const targets: SyntaxRef[] = [];
          const targetNames: string[] = [];
          for (const name of names) {
            const node = varDecls.find((d) => d.text.startsWith(`${name} =`));
            assert.ok(node?.initializer, `Missing variable ${name}`);
            targets.push(node.initializer);
            targetNames.push(name);
          }

          const resolved = await ctx.resolveInstances(targets);
          for (const [idx, row] of resolved.entries()) {
            resultsByVar.set(targetNames[idx]!, row);
          }

          const fac1Ref = resultsByVar.get("fac1")?.instance;
          assert.ok(fac1Ref, "fac1 should resolve to an InstanceRef");
          const [projected] = await ctx.projectInstances([fac1Ref], {
            select: {
              instanceKey: true,
              allocationLocation: true,
              originKind: true,
              evidence: true,
            },
          });
          assert.ok(projected);
          projectedKey = projected.instanceKey;
          projectedAllocFile = projected.allocationLocation?.file;
          assert.equal(projected.originKind, "allocation");
          assert.ok(projected.evidence.some((e) => e.kind === "source"));
        } finally {
          await session.close();
        }

        const get = (name: string): InstanceResolutionResult => {
          const res = resultsByVar.get(name);
          assert.ok(res, `Missing result for ${name}`);
          assert.ok(res.instance, `Expected resolved instance for ${name}`);
          assert.equal(res.originKind, "allocation");
          assert.deepEqual(res.gaps, []);
          assert.ok(
            res.evidence.some((e) => e.kind === "source"),
            `Expected source evidence for ${name}`,
          );
          return res;
        };

        const direct1 = get("direct1");
        const direct2 = get("direct2");
        assert.equal(isSameInstance(direct1.instance, direct2.instance), false);
        assert.notEqual(direct1.instanceKey, direct2.instanceKey);
        assert.equal(direct1.allocationLocation?.file, "src/app.ts");

        const fac1 = get("fac1");
        const fac2 = get("fac2");
        const aliasFac1 = get("aliasFac1");
        const forwardedFac1 = get("forwardedFac1");
        const forwardedFresh = get("forwardedFresh");

        assert.equal(isSameInstance(fac1.instance, fac2.instance), false);
        assert.notEqual(fac1.instanceKey, fac2.instanceKey);

        assert.equal(isSameInstance(fac1.instance, aliasFac1.instance), true);
        assert.equal(fac1.instanceKey, aliasFac1.instanceKey);

        assert.equal(
          isSameInstance(fac1.instance, forwardedFac1.instance),
          true,
        );
        assert.equal(fac1.instanceKey, forwardedFac1.instanceKey);

        assert.equal(
          isSameInstance(fac1.instance, forwardedFresh.instance),
          false,
        );
        assert.notEqual(fac1.instanceKey, forwardedFresh.instanceKey);

        const wrap1 = get("wrap1");
        const wrap2 = get("wrap2");
        assert.equal(isSameInstance(wrap1.instance, wrap2.instance), false);
        assert.equal(wrap1.allocationLocation?.file, "src/resource.ts");

        const otherFac = get("otherFac");
        const localFac = get("localFac");
        assert.equal(otherFac.allocationLocation?.file, "src/other.ts");
        assert.equal(localFac.allocationLocation?.file, "src/app.ts");
        assert.equal(isSameInstance(fac1.instance, otherFac.instance), false);
        assert.equal(isSameInstance(fac1.instance, localFac.instance), false);
        assert.equal(
          isSameInstance(otherFac.instance, localFac.instance),
          false,
        );

        assert.equal(projectedKey, fac1.instanceKey);
        assert.equal(projectedAllocFile, "src/resource.ts");
      },
    );
  });

  void it("tracks object and class fields per host instance and field name, closure captures, and module singletons across files", async () => {
    await withTempHost(
      { host: "sdk-queries/instance-identity-fields" },
      async (host) => {
        await host.typecheck();

        const resultsByName = new Map<string, InstanceResolutionResult>();
        let ownStore: InstanceRef | undefined;
        let ownOther: InstanceRef | undefined;

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const varDecls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["VariableDeclaration"],
          });
          const names = [
            "a",
            "b",
            "c",
            "readH1Item",
            "readH1Other",
            "readH2Item",
            "m1StoreBefore",
            "m1StoreAgain",
            "m1Other",
            "m2Store",
            "m1StoreAfter",
            "inner",
            "g1First",
            "g1Second",
            "g2First",
            "sing1",
            "sing2",
          ];
          const targets: SyntaxRef[] = [];
          const targetNames: string[] = [];
          for (const name of names) {
            const node = varDecls.find((d) => d.text.startsWith(`${name} =`));
            assert.ok(node?.initializer, `Missing variable ${name}`);
            targets.push(node.initializer);
            targetNames.push(name);
          }

          const resolved = await ctx.resolveInstances(targets);
          for (const [idx, row] of resolved.entries()) {
            assert.deepEqual(row.gaps, []);
            resultsByName.set(targetNames[idx]!, row);
          }

          // Inside class method `inspectOwn()`, `this._store` and `this._other` remain distinct
          const props = await ctx.findSyntax({
            files: ["src/holder.ts"],
            kinds: ["PropertyAccessExpression"],
          });
          const storeAccess = props.find((p) => p.text === "this._store");
          const otherAccess = props.find((p) => p.text === "this._other");
          assert.ok(storeAccess && otherAccess);
          const [r1, r2] = await ctx.resolveInstances([
            storeAccess.ref,
            otherAccess.ref,
          ]);
          ownStore = r1?.instance ?? undefined;
          ownOther = r2?.instance ?? undefined;
        } finally {
          await session.close();
        }

        const getInst = (name: string) => {
          const row = resultsByName.get(name);
          assert.ok(row?.instance, `Expected instance for ${name}`);
          return row.instance;
        };

        const a = getInst("a");
        const b = getInst("b");
        const c = getInst("c");

        // Same host different fields vs different hosts same field
        const readH1Item = getInst("readH1Item");
        const readH1Other = getInst("readH1Other");
        const readH2Item = getInst("readH2Item");
        assert.equal(isSameInstance(readH1Item, a), true);
        assert.equal(isSameInstance(readH1Other, b), true);
        assert.equal(isSameInstance(readH2Item, c), true);
        assert.equal(isSameInstance(readH1Item, readH1Other), false);
        assert.equal(isSameInstance(readH1Item, readH2Item), false);

        // Class instance fields & sequential overwrite
        const m1StoreBefore = getInst("m1StoreBefore");
        const m1StoreAgain = getInst("m1StoreAgain");
        const m1Other = getInst("m1Other");
        const m2Store = getInst("m2Store");
        const m1StoreAfter = getInst("m1StoreAfter");
        assert.equal(isSameInstance(m1StoreBefore, a), true);
        assert.equal(isSameInstance(m1StoreAgain, a), true);
        assert.equal(isSameInstance(m1StoreBefore, m1Other), false);
        assert.equal(isSameInstance(m2Store, b), true);
        assert.equal(isSameInstance(m1StoreBefore, m2Store), false);
        assert.equal(isSameInstance(m1StoreAfter, c), true);
        assert.equal(isSameInstance(m1StoreAfter, m1StoreBefore), false);

        // Closure capture & closure factory
        const inner = getInst("inner");
        assert.equal(isSameInstance(inner, a), true);
        const innerRow = resultsByName.get("inner")!;
        assert.ok(innerRow.evidence.some((e) => e.kind === "source"));

        const g1First = getInst("g1First");
        const g1Second = getInst("g1Second");
        const g2First = getInst("g2First");
        assert.equal(isSameInstance(g1First, g1Second), true);
        assert.equal(isSameInstance(g1First, g2First), false);

        // Module-level singleton shared across callers
        const sing1 = getInst("sing1");
        const sing2 = getInst("sing2");
        assert.equal(isSameInstance(sing1, sing2), true);

        assert.ok(ownStore && ownOther);
        assert.equal(isSameInstance(ownStore, ownOther), false);
      },
    );
  });

  void it("applies Model behaviors for external .d.ts factories/forwarders, reports gaps when unmodeled, invalidates on update, and rejects stale InstanceRef", async () => {
    await withTempHost(
      {
        host: "sdk-queries/instance-identity-model",
        setup: (host) => {
          copyDirectory(
            resolveHostDir("sdk-queries/acme-ui"),
            host.resolvePath("node_modules/@acme/ui"),
          );
        },
      },
      async (host) => {
        await host.typecheck();

        const baseModel = defineModel({
          id: "acme-ui-model",
          version: "1.0.0",
          facts: { compatible: true },
          instanceBehaviors: [
            {
              kind: "allocates-fresh-instance",
              match: { module: "@acme/ui", symbol: "Event.once" },
            },
            {
              kind: "allocates-fresh-instance",
              match: {
                module: "@acme/ui",
                container: "IMenuService",
                member: "createMenu",
              },
            },
            {
              kind: "allocates-fresh-instance",
              match: {
                container: "IInstantiationService",
                member: "createInstance",
              },
            },
            {
              kind: "forwards-argument",
              match: { container: "Disposable", member: "_register" },
              argumentIndex: 0,
            },
            {
              kind: "forwards-argument",
              match: { container: "DisposableStore", member: "add" },
              argumentIndex: 0,
            },
          ],
        });

        const resolveAppInstances = async (
          ctx: ReturnType<Awaited<ReturnType<typeof openProject>>["project"]>,
        ): Promise<Map<string, InstanceResolutionResult>> => {
          const resolvedMap = new Map<string, InstanceResolutionResult>();
          const varDecls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["VariableDeclaration"],
          });
          const names = [
            "w1",
            "registered",
            "added",
            "evOnce1",
            "evOnce2",
            "menu1",
            "menu2",
            "menu1Alias",
            "inst1",
            "inst2",
            "unknownExt",
            "partial",
            "dyn",
          ];
          const targets: SyntaxRef[] = [];
          const order: string[] = [];
          for (const name of names) {
            const node = varDecls.find((d) => d.text.startsWith(`${name} =`));
            assert.ok(node?.initializer, `Missing ${name}`);
            targets.push(node.initializer);
            order.push(name);
          }

          const rows = await ctx.resolveInstances(targets);
          for (const [idx, row] of rows.entries()) {
            resolvedMap.set(order[idx]!, row);
          }
          return resolvedMap;
        };

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          models: [baseModel],
        });

        try {
          const resolvedMap = await resolveAppInstances(session.project());

          const w1 = resolvedMap.get("w1")!;
          const registered = resolvedMap.get("registered")!;
          const added = resolvedMap.get("added")!;
          assert.ok(w1.instance);
          assert.equal(isSameInstance(w1.instance, registered.instance), true);
          assert.equal(isSameInstance(w1.instance, added.instance), true);
          assert.ok(registered.evidence.some((e) => e.kind === "source"));
          assert.ok(
            registered.evidence.some(
              (e) =>
                e.kind === "model" &&
                e.modelId === "acme-ui-model" &&
                e.declaration?.includes("_register"),
            ),
            `Expected model evidence for _register, got ${JSON.stringify(registered.evidence)}`,
          );
          assert.ok(
            added.evidence.some(
              (e) =>
                e.kind === "model" &&
                e.modelId === "acme-ui-model" &&
                e.declaration?.includes("add"),
            ),
          );

          const evOnce1 = resolvedMap.get("evOnce1")!;
          const evOnce2 = resolvedMap.get("evOnce2")!;
          assert.ok(evOnce1.instance && evOnce2.instance);
          assert.equal(evOnce1.originKind, "model-allocation");
          assert.equal(
            isSameInstance(evOnce1.instance, evOnce2.instance),
            false,
          );
          assert.ok(
            evOnce1.evidence.some(
              (e) => e.kind === "model" && e.modelId === "acme-ui-model",
            ),
          );

          const menu1 = resolvedMap.get("menu1")!;
          const menu2 = resolvedMap.get("menu2")!;
          const menu1Alias = resolvedMap.get("menu1Alias")!;
          assert.ok(menu1.instance && menu2.instance && menu1Alias.instance);
          assert.equal(menu1.originKind, "model-allocation");
          assert.equal(isSameInstance(menu1.instance, menu2.instance), false);
          assert.equal(
            isSameInstance(menu1.instance, menu1Alias.instance),
            true,
          );

          const inst1 = resolvedMap.get("inst1")!;
          const inst2 = resolvedMap.get("inst2")!;
          assert.ok(inst1.instance && inst2.instance);
          assert.equal(isSameInstance(inst1.instance, inst2.instance), false);

          // Unmodeled .d.ts call -> null instance + missing-instance-model gap
          const unknownExt = resolvedMap.get("unknownExt")!;
          assert.equal(unknownExt.instance, null);
          assert.equal(unknownExt.instances.length, 0);
          assert.ok(
            unknownExt.gaps.some((g) => g.kind === "missing-instance-model"),
          );

          // Partial resolution: keeps known branch `menu1` while reporting gap for `unmodeledExternal()`
          const partial = resolvedMap.get("partial")!;
          assert.equal(partial.instances.length, 1);
          assert.equal(isSameInstance(partial.instance, menu1.instance), true);
          assert.ok(
            partial.gaps.some((g) => g.kind === "missing-instance-model"),
          );

          // Dynamic callee -> null instance + dynamic-callee gap
          const dyn = resolvedMap.get("dyn")!;
          assert.equal(dyn.instance, null);
          assert.ok(dyn.gaps.some((g) => g.kind === "dynamic-callee"));

          // Capture snapshot-1 InstanceRef, then update models to cover `unmodeledExternal`
          const staleInstRef = menu1.instance;
          const extendedModel = defineModel({
            ...baseModel,
            version: "1.1.0",
            instanceBehaviors: [
              ...(baseModel.instanceBehaviors ?? []),
              {
                kind: "allocates-fresh-instance",
                match: { module: "@acme/ui", symbol: "unmodeledExternal" },
              },
            ],
          });

          const updateRes = await session.update({ models: [extendedModel] });
          assert.ok(updateRes.recomputation.invalidatedBy.includes("models"));

          const ctx2 = session.project();
          await assert.rejects(
            () =>
              ctx2.projectInstances([staleInstRef], {
                select: { instanceKey: true },
              }),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );

          const resolvedMap2 = await resolveAppInstances(ctx2);

          const nowModeled = resolvedMap2.get("unknownExt")!;
          assert.ok(
            nowModeled.instance,
            "Expected unmodeledExternal to resolve after model update",
          );
          assert.equal(nowModeled.originKind, "model-allocation");
          assert.deepEqual(nowModeled.gaps, []);
          assert.ok(
            nowModeled.evidence.some(
              (e) => e.kind === "model" && e.modelId === "acme-ui-model",
            ),
          );

          const partialAfter = resolvedMap2.get("partial")!;
          assert.equal(partialAfter.instances.length, 2);
          assert.deepEqual(partialAfter.gaps, []);

          // Edit source file to alias `menu2` instead of `menu1` and verify cache invalidation
          host.replaceText(
            "src/app.ts",
            "const menu1Alias = menu1;",
            "const menu1Alias = menu2;",
          );
          const srcUpdate = await session.update({ changed: ["src/app.ts"] });
          assert.ok(
            srcUpdate.recomputation.invalidatedBy.includes(
              "changed:src/app.ts",
            ),
          );

          const resolvedMap3 = await resolveAppInstances(session.project());
          const menu1AfterEdit = resolvedMap3.get("menu1")!;
          const menu2AfterEdit = resolvedMap3.get("menu2")!;
          const menu1AliasAfterEdit = resolvedMap3.get("menu1Alias")!;
          assert.equal(
            isSameInstance(
              menu1AliasAfterEdit.instance,
              menu2AfterEdit.instance,
            ),
            true,
          );
          assert.equal(
            isSameInstance(
              menu1AliasAfterEdit.instance,
              menu1AfterEdit.instance,
            ),
            false,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("validates Model instanceBehaviors definitions", () => {
    assert.throws(
      () =>
        defineModel({
          id: "bad-match",
          instanceBehaviors: [
            {
              kind: "allocates-fresh-instance",
              match: {},
            },
          ],
        }),
      (err) => err instanceof TssaeError && err.code === "INVALID_CONFIG",
    );

    assert.throws(
      () =>
        defineModel({
          id: "bad-arg-index",
          instanceBehaviors: [
            {
              kind: "forwards-argument",
              match: { member: "_register" },
              argumentIndex: -1,
            },
          ],
        }),
      (err) => err instanceof TssaeError && err.code === "INVALID_CONFIG",
    );
  });

  void it("scopes only unanchored instance caller lookup and keeps known cross-file factories intact", async () => {
    await withTempHost(
      { host: "sdk-queries/instance-identity-scoped" },
      async (host) => {
        await host.typecheck();

        let scopedLocations: string[] = [];
        let fullLocations: string[] = [];
        let scopedFactoryKey = "";
        let fullFactoryKey = "";

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const params = await ctx.findSyntax({
            files: ["src/forward.ts"],
            kinds: ["Parameter"],
          });
          const valueParam = params.find((param) =>
            param.text.startsWith("value:"),
          );
          const appFactory = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const factoryCall = appFactory.find(
            (call) =>
              call.text.startsWith("createStore") && call.location.line === 4,
          );
          assert.ok(valueParam && factoryCall);

          const [scoped] = await ctx.resolveInstances([valueParam.ref], {
            lookupFiles: ["src/app.ts"],
          });
          const [full] = await ctx.resolveInstances([valueParam.ref]);
          scopedLocations = scoped!.instances
            .map((instance) => instance.allocationLocation?.file ?? "")
            .toSorted();
          fullLocations = full!.instances
            .map((instance) => instance.allocationLocation?.file ?? "")
            .toSorted();

          const [scopedFactory] = await ctx.resolveInstances(
            [factoryCall.ref],
            { lookupFiles: ["src/app.ts"] },
          );
          const [fullFactory] = await ctx.resolveInstances([factoryCall.ref]);
          assert.ok(scopedFactory?.instance && fullFactory?.instance);
          assert.equal(
            isSameInstance(scopedFactory.instance, fullFactory.instance),
            true,
          );
          scopedFactoryKey = scopedFactory.instanceKey!;
          fullFactoryKey = fullFactory.instanceKey!;
        } finally {
          await session.close();
        }

        assert.deepEqual(scopedLocations, ["src/resource.ts"]);
        assert.deepEqual(fullLocations, ["src/other.ts", "src/resource.ts"]);
        assert.equal(scopedFactoryKey, fullFactoryKey);
      },
    );
  });
});
