import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { describe, it } from "node:test";

import {
  copyDirectory,
  linkNodeModulesPackage,
  type PreparedTempHost,
  withTempHost,
} from "@verule/test-hosts";
import {
  defineModel,
  isSameInstance,
  openProject,
  type InstanceResolutionResult,
  type SyntaxRef,
} from "tssae";

function materializeDepsIntoNodeModules(
  host: PreparedTempHost,
  packages: readonly string[],
): void {
  for (const pkg of packages) {
    copyDirectory(
      host.resolvePath(`deps/${pkg}`),
      host.resolvePath(`node_modules/${pkg}`),
    );
    const artifactsDir = host.resolvePath(`node_modules/${pkg}/artifacts`);
    if (existsSync(artifactsDir)) {
      copyDirectory(artifactsDir, host.resolvePath(`node_modules/${pkg}/dist`));
      host.removePath(`node_modules/${pkg}/artifacts`);
    }
  }
  host.removePath("deps");
}

void describe("tssae package behavior resolution", () => {
  void it("resolves conditional exports separately for ESM import and CJS require callers without expanding checked source files", async () => {
    await withTempHost(
      {
        host: "sdk-projects/package-behavior-dual",
        setup: (host) => {
          materializeDepsIntoNodeModules(host, ["dual-pkg"]);
        },
      },
      async (host) => {
        await host.typecheck("packages/esm-app/tsconfig.json");
        await host.typecheck("packages/cjs-app/tsconfig.json");

        const observations = new Map<
          string,
          {
            readonly mainImplFile: string | null;
            readonly mainMode: string | null;
            readonly mainSubpath: string | null;
            readonly mainVersion: string | null;
            readonly subImplFile: string | null;
            readonly subMode: string | null;
            readonly subSubpath: string | null;
            readonly mainPropReachable: boolean;
            readonly mainPropEvidenceFiles: readonly string[];
            readonly subPropReachable: boolean;
            readonly inputInst: InstanceResolutionResult;
            readonly mainOutInst: InstanceResolutionResult;
            readonly subOutInst: InstanceResolutionResult;
          }
        >();

        const session = await openProject({
          cwd: host.rootDir,
          projects: [
            "packages/esm-app/tsconfig.json",
            "packages/cjs-app/tsconfig.json",
          ],
        });
        try {
          for (const proj of session.projects) {
            assert.ok(
              proj.sourceFiles.every((f) => !f.includes("node_modules/")),
              `node_modules must not enter ProjectSummary.sourceFiles: ${proj.sourceFiles.join(", ")}`,
            );
          }
          assert.ok(
            session.fileOrigins.every((o) => !o.file.includes("node_modules/")),
            `node_modules must not enter ProjectSession.fileOrigins`,
          );

          for (const proj of session.projects) {
            const ctx = session.project(proj.configFile);
            const calls = await ctx.findSyntax({
              kinds: ["CallExpression"],
            });
            const mainCallSyntax = calls.find((c) =>
              c.text.includes("transformPayload("),
            );
            const subCallSyntax = calls.find((c) =>
              c.text.includes("transformSub("),
            );
            assert.ok(mainCallSyntax && subCallSyntax);

            const [mainCallRow, subCallRow] = await ctx.resolveCalls([
              mainCallSyntax.ref,
              subCallSyntax.ref,
            ]);
            assert.ok(mainCallRow?.call && subCallRow?.call);

            const [mainImpl, subImpl] = await ctx.resolveImplementations([
              mainCallRow.call,
              subCallRow.call,
            ]);
            assert.ok(mainImpl && subImpl);

            const [mainCalleeInfo] = await ctx.calleesOf([mainCallRow.call]);
            assert.ok(mainCalleeInfo);
            assert.equal(
              mainCalleeInfo.resolvedTargets[0]?.implementation?.file,
              mainImpl.implementation?.file,
            );
            const retExprs =
              mainCalleeInfo.resolvedTargets[0]?.returnExpressions ?? [];
            assert.ok(retExprs.length > 0);
            const [retSyn] = await ctx.syntaxOf([retExprs[0]!], {
              select: { location: true },
            });
            assert.equal(retSyn?.location?.file, mainImpl.implementation?.file);

            const varDecls = await ctx.findSyntax({
              kinds: ["VariableDeclaration"],
            });
            const findInit = (prefix: string): SyntaxRef => {
              const found = varDecls.find((d) => d.text.startsWith(prefix));
              assert.ok(found?.initializer, `Missing ${prefix}`);
              return found.initializer;
            };
            const inputInit = findInit("input");
            const mainOutInit = findInit("mainOut");
            const subOutInit = findInit("subOut");

            const [inputDeclRow, mainOutDeclRow, subOutDeclRow] =
              await ctx.resolveDeclarations([
                varDecls.find((d) => d.text.startsWith("input"))!.ref,
                varDecls.find((d) => d.text.startsWith("mainOut"))!.ref,
                varDecls.find((d) => d.text.startsWith("subOut"))!.ref,
              ]);

            const prop = await ctx.valuePropagation({
              propagation: "value",
              from: inputInit,
              to: [mainOutDeclRow!.declaration!, subOutDeclRow!.declaration!],
            });

            const [inputInst, mainOutInst, subOutInst] =
              await ctx.resolveInstances([
                inputDeclRow!.declaration!,
                mainOutInit,
                subOutInit,
              ]);
            assert.ok(inputInst && mainOutInst && subOutInst);

            const key = ctx.project.configFile.includes("esm-app")
              ? "esm"
              : "cjs";
            observations.set(key, {
              mainImplFile: mainImpl.implementation?.file ?? null,
              mainMode: mainImpl.implementation?.resolutionMode ?? null,
              mainSubpath: mainImpl.implementation?.subpath ?? null,
              mainVersion: mainImpl.implementation?.packageVersion ?? null,
              subImplFile: subImpl.implementation?.file ?? null,
              subMode: subImpl.implementation?.resolutionMode ?? null,
              subSubpath: subImpl.implementation?.subpath ?? null,
              mainPropReachable: prop.results[0]!.reachable,
              mainPropEvidenceFiles: prop.results[0]!.evidence.map(
                (e) => e.at?.file,
              ).filter((f): f is string => f !== undefined),
              subPropReachable: prop.results[1]!.reachable,
              inputInst,
              mainOutInst,
              subOutInst,
            });
          }
        } finally {
          await session.close();
        }

        const esm = observations.get("esm");
        const cjs = observations.get("cjs");
        assert.ok(esm && cjs);

        assert.equal(esm.mainImplFile, "node_modules/dual-pkg/dist/index.mjs");
        assert.equal(esm.mainMode, "import");
        assert.equal(esm.mainSubpath, ".");
        assert.equal(esm.mainVersion, "1.2.3");
        assert.equal(esm.subImplFile, "node_modules/dual-pkg/dist/sub.mjs");
        assert.equal(esm.subMode, "import");
        assert.equal(esm.subSubpath, "./sub");
        assert.equal(esm.mainPropReachable, true);
        assert.ok(
          esm.mainPropEvidenceFiles.includes(
            "node_modules/dual-pkg/dist/index.mjs",
          ),
        );
        assert.equal(esm.subPropReachable, true);
        assert.equal(
          isSameInstance(esm.inputInst.instance, esm.mainOutInst.instance),
          true,
        );
        assert.equal(
          isSameInstance(esm.inputInst.instance, esm.subOutInst.instance),
          true,
        );
        assert.ok(
          esm.mainOutInst.evidence.some(
            (e) => e.at?.file === "node_modules/dual-pkg/dist/index.mjs",
          ),
        );

        assert.equal(cjs.mainImplFile, "node_modules/dual-pkg/dist/index.cjs");
        assert.equal(cjs.mainMode, "require");
        assert.equal(cjs.mainSubpath, ".");
        assert.equal(cjs.subImplFile, "node_modules/dual-pkg/dist/sub.cjs");
        assert.equal(cjs.subMode, "require");
        assert.equal(cjs.subSubpath, "./sub");
        assert.equal(cjs.mainPropReachable, false);
        assert.equal(cjs.subPropReachable, false);
        assert.equal(
          isSameInstance(cjs.inputInst.instance, cjs.mainOutInst.instance),
          false,
        );
        assert.equal(
          isSameInstance(cjs.inputInst.instance, cjs.subOutInst.instance),
          false,
        );
        assert.equal(
          cjs.mainOutInst.allocationLocation?.file,
          "node_modules/dual-pkg/dist/index.cjs",
        );
        assert.equal(
          cjs.subOutInst.allocationLocation?.file,
          "node_modules/dual-pkg/dist/sub.cjs",
        );
        assert.ok(
          cjs.mainOutInst.evidence.some(
            (e) => e.at?.file === "node_modules/dual-pkg/dist/index.cjs",
          ),
        );
      },
    );
  });

  void it("covers workspace symlinks, pure-JS dependencies, customConditions isolation, and intra-project .mts/.cts conditions", async () => {
    await withTempHost(
      {
        host: "sdk-projects/package-behavior-conditions",
        setup: (host) => {
          materializeDepsIntoNodeModules(host, ["pure-js-dep", "cond-custom"]);
          linkNodeModulesPackage(
            host.rootDir,
            "@ws/ts-relay",
            host.resolvePath("packages/ws-ts"),
          );
          linkNodeModulesPackage(
            host.rootDir,
            "@ws/js-relay",
            host.resolvePath("packages/ws-js"),
          );
        },
      },
      async (host) => {
        await host.typecheck("packages/fast-proj/tsconfig.json");
        await host.typecheck("packages/clone-proj/tsconfig.json");
        await host.typecheck("packages/mixed-app/tsconfig.json");

        const condResults = new Map<
          string,
          { readonly same: boolean; readonly file: string | null }
        >();
        let mixedWsSame = false;
        let mixedJsSame = false;
        let mixedEsmSame = false;
        let mixedCjsSame = true;
        let mixedPureJsImplFile: string | null = null;
        let mixedEsmImplFile: string | null = null;
        let mixedCjsImplFile: string | null = null;
        let mixedJsPropReachable = false;

        const session = await openProject({
          cwd: host.rootDir,
          projects: [
            "packages/fast-proj/tsconfig.json",
            "packages/clone-proj/tsconfig.json",
            "packages/mixed-app/tsconfig.json",
          ],
        });
        try {
          for (const proj of session.projects) {
            assert.ok(
              proj.sourceFiles.every((f) => !f.includes("node_modules/")),
              `node_modules must stay excluded from sourceFiles: ${proj.sourceFiles.join(", ")}`,
            );
          }

          for (const proj of session.projects) {
            const ctx = session.project(proj.configFile);
            if (ctx.project.configFile.includes("mixed-app")) {
              const varDecls = await ctx.findSyntax({
                kinds: ["VariableDeclaration"],
              });
              const calls = await ctx.findSyntax({
                kinds: ["CallExpression"],
              });

              const esmVars = await ctx.findSyntax({
                files: ["packages/mixed-app/src/esm.mts"],
                kinds: ["VariableDeclaration"],
              });
              const esmCalls = await ctx.findSyntax({
                files: ["packages/mixed-app/src/esm.mts"],
                kinds: ["CallExpression"],
              });
              const cjsVars = await ctx.findSyntax({
                files: ["packages/mixed-app/src/cjs.cts"],
                kinds: ["VariableDeclaration"],
              });
              const cjsCalls = await ctx.findSyntax({
                files: ["packages/mixed-app/src/cjs.cts"],
                kinds: ["CallExpression"],
              });

              const esmBase = esmVars.find((v) => v.text.startsWith("base ="));
              const viaWs = esmVars.find((v) => v.text.startsWith("viaWs ="));
              const viaWsJs = esmVars.find((v) =>
                v.text.startsWith("viaWsJs ="),
              );
              const viaJs = esmVars.find((v) => v.text.startsWith("viaJs ="));
              const viaCondEsm = esmVars.find((v) =>
                v.text.startsWith("viaCond ="),
              );
              const cjsBase = cjsVars.find((v) => v.text.startsWith("base ="));
              const viaCondCjs = cjsVars.find((v) =>
                v.text.startsWith("viaCond ="),
              );
              assert.ok(
                esmBase?.initializer &&
                  viaWs?.initializer &&
                  viaWsJs?.initializer &&
                  viaJs?.initializer &&
                  viaCondEsm?.initializer &&
                  cjsBase?.initializer &&
                  viaCondCjs?.initializer,
              );

              const [
                bInst,
                wsInst,
                wsJsInst,
                jsInst,
                esmCondInst,
                cjsBInst,
                cjsCondInst,
              ] = await ctx.resolveInstances([
                esmBase.initializer,
                viaWs.initializer,
                viaWsJs.initializer,
                viaJs.initializer,
                viaCondEsm.initializer,
                cjsBase.initializer,
                viaCondCjs.initializer,
              ]);
              mixedWsSame =
                isSameInstance(bInst!.instance, wsInst!.instance) &&
                isSameInstance(bInst!.instance, wsJsInst!.instance);
              mixedJsSame = isSameInstance(bInst!.instance, jsInst!.instance);
              mixedEsmSame = isSameInstance(
                bInst!.instance,
                esmCondInst!.instance,
              );
              mixedCjsSame = isSameInstance(
                cjsBInst!.instance,
                cjsCondInst!.instance,
              );

              const [viaJsDecl] = await ctx.resolveDeclarations([viaJs.ref]);
              const prop = await ctx.valuePropagation({
                from: esmBase.initializer,
                to: viaJsDecl!.declaration!,
              });
              mixedJsPropReachable = prop.results[0]!.reachable;

              const pureJsCall = esmCalls.find((c) =>
                c.text.startsWith("passPureJs("),
              );
              const esmAdaptCall = esmCalls.find((c) =>
                c.text.startsWith("adapt("),
              );
              const cjsAdaptCall = cjsCalls.find((c) =>
                c.text.includes("cond.adapt("),
              );
              assert.ok(pureJsCall && esmAdaptCall && cjsAdaptCall);

              const [pureImpl, esmImpl, cjsImpl] =
                await ctx.resolveImplementations([
                  pureJsCall.ref,
                  esmAdaptCall.ref,
                  cjsAdaptCall.ref,
                ]);
              mixedPureJsImplFile = pureImpl!.implementation?.file ?? null;
              mixedEsmImplFile = esmImpl!.implementation?.file ?? null;
              mixedCjsImplFile = cjsImpl!.implementation?.file ?? null;
              void [varDecls, calls];
              continue;
            }

            const varDecls = await ctx.findSyntax({
              kinds: ["VariableDeclaration"],
            });
            const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
            const seed = varDecls.find((d) => d.text.startsWith("seed ="));
            const out = varDecls.find((d) => d.text.startsWith("out ="));
            const adaptCall = calls.find((c) => c.text.startsWith("adapt("));
            assert.ok(seed?.initializer && out?.initializer && adaptCall);
            const [seedInst, outInst] = await ctx.resolveInstances([
              seed.initializer,
              out.initializer,
            ]);
            const [impl] = await ctx.resolveImplementations([adaptCall.ref]);
            const key = ctx.project.configFile.includes("fast-proj")
              ? "fast"
              : "clone";
            condResults.set(key, {
              same: isSameInstance(seedInst!.instance, outInst!.instance),
              file: impl!.implementation?.file ?? null,
            });
          }
        } finally {
          await session.close();
        }

        assert.deepEqual(condResults.get("fast"), {
          same: true,
          file: "node_modules/cond-custom/dist/fast.mjs",
        });
        assert.deepEqual(condResults.get("clone"), {
          same: false,
          file: "node_modules/cond-custom/dist/clone.mjs",
        });

        assert.equal(mixedWsSame, true);
        assert.equal(mixedJsSame, true);
        assert.equal(mixedJsPropReachable, true);
        assert.equal(mixedEsmSame, true);
        assert.equal(mixedCjsSame, false);
        assert.equal(mixedPureJsImplFile, "node_modules/pure-js-dep/index.js");
        assert.equal(
          mixedEsmImplFile,
          "node_modules/cond-custom/dist/fast.mjs",
        );
        assert.equal(
          mixedCjsImplFile,
          "node_modules/cond-custom/dist/clone.cjs",
        );
      },
    );
  });

  void it("preserves signatures, known Model evidence, and explicit Gaps when implementations are removed, packages are missing, or mappings are ambiguous", async () => {
    await withTempHost(
      {
        host: "sdk-projects/package-behavior-gaps",
        setup: (host) => {
          materializeDepsIntoNodeModules(host, [
            "removable-pkg",
            "ambiguous-pkg",
          ]);
        },
      },
      async (host) => {
        await host.typecheck();

        const model = defineModel({
          id: "removable-model",
          instanceBehaviors: [
            {
              kind: "forwards-argument",
              match: { module: "removable-pkg", member: "modeledOp" },
              argumentIndex: 0,
            },
          ],
        });

        const inspect = async (
          ctx: ReturnType<Awaited<ReturnType<typeof openProject>>["project"]>,
        ) => {
          const calls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const varDecls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["VariableDeclaration"],
          });

          const callOf = (prefix: string) => {
            const c = calls.find((node) => node.text.startsWith(prefix));
            assert.ok(c, `Missing call ${prefix}`);
            return c;
          };
          const initOf = (prefix: string): SyntaxRef => {
            const d = varDecls.find((node) => node.text.startsWith(prefix));
            assert.ok(d?.initializer, `Missing var ${prefix}`);
            return d.initializer;
          };

          const mCallSyn = callOf("modeledOp(");
          const uCallSyn = callOf("unmodeledOp(");
          const aCallSyn = callOf("ambigOp(");
          const zCallSyn = callOf("missingFn(");

          const resolvedCalls = await ctx.resolveCalls(
            [mCallSyn.ref, uCallSyn.ref],
            {
              select: {
                signature: true,
                instantiatedParameterTypes: [0],
                returnType: true,
              },
            },
          );
          const [mCallRow, uCallRow] = resolvedCalls;
          assert.ok(mCallRow?.call && uCallRow?.call);

          const [mProj, uProj] = await ctx.projectCalls(
            [mCallRow.call, uCallRow.call],
            {
              select: {
                signature: true,
                instantiatedParameterTypes: [0],
                returnType: true,
              },
            },
          );
          assert.ok(mProj?.signature && uProj?.signature);
          const [mSigProj] = await ctx.projectSignatures([mProj.signature], {
            select: { signatureText: true, parameters: true },
          });
          assert.ok(mSigProj?.signatureText?.includes("Item"));

          const [mRet, uRet, mParam0] = await ctx.projectTypes(
            [
              mProj.returnType!.ref,
              uProj.returnType!.ref,
              mProj.instantiatedParameterTypes[0]!.type.ref,
            ],
            { select: { typeText: true } },
          );

          const [itemInst, mInst, uInst, aInst, zInst] =
            await ctx.resolveInstances([
              initOf("item"),
              initOf("m ="),
              initOf("u ="),
              initOf("a ="),
              initOf("z ="),
            ]);

          const [uImpl, aImpl, zImpl] = await ctx.resolveImplementations([
            uCallSyn.ref,
            aCallSyn.ref,
            zCallSyn.ref,
          ]);

          return {
            modeledReturnType: mRet?.typeText ?? null,
            unmodeledReturnType: uRet?.typeText ?? null,
            modeledParamType: mParam0?.typeText ?? null,
            modeledSame: isSameInstance(itemInst!.instance, mInst!.instance),
            modeledUsesModel: mInst!.evidence.some(
              (e) => e.kind === "model" && e.modelId === "removable-model",
            ),
            unmodeledSame: isSameInstance(itemInst!.instance, uInst!.instance),
            unmodeledInstanceNull: uInst!.instance === null,
            unmodeledGapKind: uInst!.gaps[0]?.kind,
            unmodeledImplStatus: uImpl!.status,
            ambigImplStatus: aImpl!.status,
            ambigCandidateCount: aImpl!.candidates.length,
            ambigGapKind: aInst!.gaps[0]?.kind,
            missingImplStatus: zImpl!.status,
            missingGapKind: zInst!.gaps[0]?.kind,
            missingGapMsg: zInst!.gaps[0]?.message,
          };
        };

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          models: [model],
        });
        try {
          // Phase 1: with implementations present, unmodeledOp resolves and traces to index.mjs.
          const initial = await inspect(session.project());
          assert.equal(initial.unmodeledImplStatus, "resolved");
          assert.equal(initial.unmodeledInstanceNull, false);
          assert.equal(initial.unmodeledSame, true);

          // Phase 2: remove runtime `.mjs` and `.cjs` implementations, leaving only `.d.ts`.
          host.removePath("node_modules/removable-pkg/dist/index.mjs");
          host.removePath("node_modules/removable-pkg/dist/index.cjs");
          await session.update();

          const afterRemoval = await inspect(session.project());
          assert.equal(afterRemoval.modeledReturnType, "Item");
          assert.equal(afterRemoval.unmodeledReturnType, "Item");
          assert.equal(afterRemoval.modeledParamType, "Item");
          assert.equal(afterRemoval.modeledSame, true);
          assert.equal(afterRemoval.modeledUsesModel, true);
          assert.equal(afterRemoval.unmodeledInstanceNull, true);
          assert.equal(afterRemoval.unmodeledGapKind, "missing-instance-model");
          assert.equal(afterRemoval.unmodeledImplStatus, "declaration-only");
          assert.equal(afterRemoval.ambigImplStatus, "ambiguous");
          assert.equal(afterRemoval.ambigCandidateCount, 2);
          assert.equal(
            afterRemoval.ambigGapKind,
            "ambiguous-package-implementation",
          );
          assert.equal(afterRemoval.missingImplStatus, "missing-package");
          assert.equal(afterRemoval.missingGapKind, "missing-package");
          assert.ok(afterRemoval.missingGapMsg?.includes("not-installed-pkg"));
        } finally {
          await session.close();
        }
      },
    );
  });
});
