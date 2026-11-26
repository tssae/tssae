import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  copyDirectory,
  linkNodeModulesPackage,
  resolveHostDir,
  withTempHost,
} from "@tssae/test-hosts";
import {
  TssaeError,
  openProject,
  type CallRef,
  type DeclarationRef,
  type SyntaxRef,
} from "tssae";

void describe("tssae call graph and value propagation", () => {
  void it("resolves callersOf and calleesOf across aliases, same-name symbols, and dynamic call targets with gaps", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-aliases" },
      async (host) => {
        await host.typecheck();

        let innerCallLines: number[] = [];
        let otherCallLines: number[] = [];
        let localCallLines: number[] = [];
        let aliasedTargetFile: string | undefined;
        let aliasedTargetName: string | null | undefined;
        let aliasedBindingParamName: string | null | undefined;
        let aliasedReturnCount = 0;
        let mixedTargetNames: (string | null)[] = [];
        let mixedGapKinds: string[] = [];
        let ifaceGapKinds: string[] = [];

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const funcDecls = await ctx.findSyntax({
            kinds: ["FunctionDeclaration"],
          });
          const declResolutions = await ctx.resolveDeclarations(
            funcDecls.map((f) => f.ref),
            { select: { name: true, location: true } },
          );

          const findDecl = (file: string, name: string): DeclarationRef => {
            const found = declResolutions.find(
              (row) => row.location?.file === file && row.name === name,
            );
            assert.ok(found?.declaration, `Missing decl ${name} in ${file}`);
            return found.declaration;
          };

          const innerDecl = findDecl("src/inner.ts", "processItem");
          const otherDecl = findDecl("src/other.ts", "processItem");
          const localDecl = findDecl("src/entry.ts", "processItem");

          const callers = await ctx.callersOf([
            innerDecl,
            otherDecl,
            localDecl,
          ]);
          assert.equal(callers.length, 3);
          innerCallLines = callers[0]!.callSites.map((s) => s.location.line);
          otherCallLines = callers[1]!.callSites.map((s) => s.location.line);
          localCallLines = callers[2]!.callSites.map((s) => s.location.line);

          const callNodes = await ctx.findSyntax({
            files: ["src/entry.ts"],
            kinds: ["CallExpression"],
          });
          const callRes = await ctx.resolveCalls(
            callNodes.map((n) => n.ref),
            { select: { location: true } },
          );
          const callAtLine = (line: number): CallRef => {
            const row = callRes.find((r) => r.location?.line === line);
            assert.ok(row?.call, `Missing call at line ${line}`);
            return row.call;
          };

          const aliasedCall = callAtLine(17);
          const mixedCall = callAtLine(21);
          const ifaceCall = callAtLine(22);

          const callees = await ctx.calleesOf([
            aliasedCall,
            mixedCall,
            ifaceCall,
          ]);
          assert.equal(callees.length, 3);

          const aliasedResult = callees[0]!;
          assert.equal(aliasedResult.targets.length, 1);
          assert.equal(aliasedResult.completeness, "complete");
          aliasedTargetFile = aliasedResult.resolvedTargets[0]?.location?.file;
          aliasedTargetName = aliasedResult.resolvedTargets[0]?.name;
          aliasedBindingParamName =
            aliasedResult.resolvedTargets[0]?.parameterBindings[0]
              ?.parameterName;
          aliasedReturnCount =
            aliasedResult.resolvedTargets[0]?.returnExpressions.length ?? 0;

          const mixedResult = callees[1]!;
          mixedTargetNames = mixedResult.resolvedTargets.map((t) => t.name);
          assert.equal(mixedResult.completeness, "incomplete");
          mixedGapKinds = mixedResult.gaps.map((g) => g.kind);

          const ifaceResult = callees[2]!;
          assert.equal(ifaceResult.completeness, "incomplete");
          ifaceGapKinds = ifaceResult.gaps.map((g) => g.kind);
        } finally {
          await session.close();
        }

        // Line 17 is direct aliasedProcess(input); line 21 is mixed(a) which may call aliasedProcess
        assert.ok(
          innerCallLines.includes(17),
          `Got ${innerCallLines.join(",")}`,
        );
        assert.ok(
          !innerCallLines.includes(18) && !innerCallLines.includes(19),
          "Must not confuse same-name symbols",
        );
        assert.deepEqual(otherCallLines, [18]);
        assert.deepEqual(localCallLines, [19]);

        assert.equal(aliasedTargetFile, "src/inner.ts");
        assert.equal(aliasedTargetName, "processItem");
        assert.equal(aliasedBindingParamName, "item");
        assert.equal(aliasedReturnCount, 1);

        assert.deepEqual(mixedTargetNames, ["processItem"]);
        assert.ok(mixedGapKinds.includes("dynamic-callee"));
        assert.ok(ifaceGapKinds.includes("open-external-hierarchy"));
      },
    );
  });

  void it("traces mixed TS -> JS -> TS calls, reads JSDoc param types with checkJs: false, and evaluates propagation: value", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-mixed-js" },
      async (host) => {
        await host.typecheck();

        let jsDocTypeText: string | undefined;
        let jsDocUnionMembers: readonly string[] | undefined;
        let bridgeCallerFile: string | undefined;
        let sinkCallerFile: string | undefined;
        let deliveredReachable = false;
        let deliveredEvidenceFiles: string[] = [];
        let deliveredStepKinds: string[] = [];
        let extractedReachable = false;
        let withoutSecretReachable = true;
        let extraReachable = true;
        let isolatedAReachable = false;
        let isolatedBReachable = true;
        let templatedReachable = true;
        let overwrittenReachable = true;
        let restRowReachable = false;
        let mixedReachable = false;
        let mixedGapKinds: string[] = [];

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          assert.equal(ctx.project.compilerOptions.allowJs, true);
          assert.equal(ctx.project.compilerOptions.checkJs, false);

          const entryCalls = await ctx.findSyntax({
            files: ["src/entry.ts"],
            kinds: ["CallExpression"],
          });
          const bridgeCalls = await ctx.findSyntax({
            files: ["src/bridge.js"],
            kinds: ["CallExpression"],
          });

          const forwardCallSyntax = entryCalls.find((c) =>
            c.text.startsWith("forwardBridge("),
          );
          assert.ok(forwardCallSyntax);
          const [resolvedForward] = await ctx.resolveCalls(
            [forwardCallSyntax.ref],
            {
              select: {
                instantiatedParameterTypes: [0],
                calleeDeclaration: true,
              },
            },
          );
          const param0 = resolvedForward?.instantiatedParameterTypes[0];
          jsDocTypeText = param0?.type.typeText;
          jsDocUnionMembers = param0?.type.unionMembers;

          assert.ok(resolvedForward?.call);
          const [bridgeCallee] = await ctx.calleesOf([resolvedForward.call]);
          assert.equal(
            bridgeCallee?.resolvedTargets[0]?.location?.file,
            "src/bridge.js",
          );

          const deliverCallSyntax = bridgeCalls.find((c) =>
            c.text.startsWith("deliver("),
          );
          assert.ok(deliverCallSyntax);
          const [resolvedDeliver] = await ctx.resolveCalls([
            deliverCallSyntax.ref,
          ]);
          assert.ok(resolvedDeliver?.call);
          const [deliverCallee] = await ctx.calleesOf([resolvedDeliver.call]);
          assert.equal(
            deliverCallee?.resolvedTargets[0]?.location?.file,
            "src/sink.ts",
          );

          const callers = await ctx.callersOf([
            resolvedForward.calleeDeclaration!,
            deliverCallee.targets[0]!,
          ]);
          bridgeCallerFile = callers[0]?.callSites[0]?.location.file;
          sinkCallerFile = callers[1]?.callSites[0]?.location.file;

          // Trace raw = createToken() -> various targets
          const varDecls = await ctx.findSyntax({
            files: ["src/entry.ts"],
            kinds: ["VariableDeclaration"],
          });
          const varInitOf = (name: string): SyntaxRef => {
            const decl = varDecls.find(
              (d) =>
                d.text.startsWith(`${name} =`) ||
                d.text.startsWith(`{ ${name}`) ||
                d.text.includes(`...${name}`),
            );
            assert.ok(decl?.initializer, `Missing var ${name}`);
            return decl.initializer;
          };
          const varDeclRef = async (name: string): Promise<DeclarationRef> => {
            const decl = varDecls.find((d) => d.text.startsWith(`${name} =`));
            assert.ok(decl, `Missing varDecl ${name}`);
            const [res] = await ctx.resolveDeclarations([decl.ref]);
            return res!.declaration!;
          };

          const rawOrigin = varInitOf("raw");
          const deliveredTarget = await varDeclRef("delivered");
          const isolatedATarget = await varDeclRef("isolatedA");
          const isolatedBTarget = await varDeclRef("isolatedB");
          const templatedTarget = await varDeclRef("templated");
          const mixedTarget = await varDeclRef("mixedOut");

          const bindingIdentifiers = await ctx.findSyntax({
            files: ["src/entry.ts"],
            kinds: ["Identifier"],
          });
          const findIdent = (name: string, line: number): SyntaxRef => {
            const match = bindingIdentifiers.find(
              (id) => id.text === name && id.location.line === line,
            );
            assert.ok(match, `Missing identifier ${name} at line ${line}`);
            return match.ref;
          };

          const extraTarget = findIdent("extra", 26);
          const extractedTarget = findIdent("extracted", 28);
          const withoutSecretTarget = findIdent("withoutSecret", 28);
          const overwrittenRead = findIdent("overwritten", 37);

          const propRes = await ctx.valuePropagation({
            propagation: "value",
            from: rawOrigin,
            to: [
              deliveredTarget,
              extractedTarget,
              withoutSecretTarget,
              extraTarget,
              isolatedATarget,
              isolatedBTarget,
              templatedTarget,
              overwrittenRead,
              mixedTarget,
            ],
          });

          deliveredReachable = propRes.results[0]!.reachable;
          deliveredEvidenceFiles = propRes.results[0]!.evidence.map(
            (e) => e.at?.file,
          ).filter((f): f is string => f !== undefined);
          deliveredStepKinds =
            propRes.results[0]!.paths[0]?.steps.map((s) => s.kind) ?? [];
          extractedReachable = propRes.results[1]!.reachable;
          withoutSecretReachable = propRes.results[2]!.reachable;
          extraReachable = propRes.results[3]!.reachable;
          isolatedAReachable = propRes.results[4]!.reachable;
          isolatedBReachable = propRes.results[5]!.reachable;
          templatedReachable = propRes.results[6]!.reachable;
          overwrittenReachable = propRes.results[7]!.reachable;
          mixedReachable = propRes.results[8]!.reachable;
          mixedGapKinds = propRes.results[8]!.gaps.map((g) => g.kind);

          // Rest destructuring on opaque object: opaque -> restRow
          const opaqueOrigin = varInitOf("opaque");
          const restRowIdent = findIdent("restRow", 35);
          const opaqueProp = await ctx.valuePropagation({
            propagation: "value",
            from: opaqueOrigin,
            to: [restRowIdent],
          });
          restRowReachable = opaqueProp.results[0]!.reachable;
        } finally {
          await session.close();
        }

        assert.equal(jsDocTypeText, '"alpha" | "beta"');
        assert.deepEqual(jsDocUnionMembers, ['"alpha"', '"beta"']);
        assert.equal(bridgeCallerFile, "src/entry.ts");
        assert.equal(sinkCallerFile, "src/bridge.js");

        assert.equal(deliveredReachable, true);
        assert.ok(deliveredEvidenceFiles.includes("src/entry.ts"));
        assert.ok(deliveredEvidenceFiles.includes("src/bridge.js"));
        assert.ok(deliveredEvidenceFiles.includes("src/sink.ts"));
        assert.ok(deliveredStepKinds.includes("awaitUnwrap"));
        assert.ok(deliveredStepKinds.includes("typeAssertion"));
        assert.ok(deliveredStepKinds.includes("propertyRead"));
        assert.ok(deliveredStepKinds.includes("callArgument"));
        assert.ok(deliveredStepKinds.includes("returnValue"));

        assert.equal(extractedReachable, true);
        assert.equal(withoutSecretReachable, false);
        assert.equal(extraReachable, false);
        assert.equal(isolatedAReachable, true);
        assert.equal(isolatedBReachable, false);
        assert.equal(templatedReachable, false);
        assert.equal(overwrittenReachable, false);
        assert.equal(restRowReachable, true);
        assert.equal(mixedReachable, true);
        assert.ok(mixedGapKinds.includes("dynamic-callee"));
      },
    );
  });

  void it("follows workspace symlink packages and reports missing-function-body gaps for .d.ts-only dependencies", async () => {
    await withTempHost(
      {
        host: "sdk-queries/calls-values-workspace",
        setup: (host) => {
          copyDirectory(
            resolveHostDir("sdk-queries/acme-decl-only"),
            host.resolvePath("node_modules/@acme/decl-only"),
          );
          linkNodeModulesPackage(
            host.rootDir,
            "@acme/ws-bridge",
            host.resolvePath("packages/ws-bridge"),
          );
        },
      },
      async (host) => {
        await host.typecheck();

        let wsTargetsCount = 0;
        let wsCallerCount = 0;
        let relayedReachable = false;
        let relayedFiles: string[] = [];
        let boxedReachable = true;
        let boxedGapKinds: string[] = [];

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const calls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const relayCallSyntax = calls.find((c) =>
            c.text.startsWith("relayValue("),
          );
          assert.ok(relayCallSyntax);
          const [resolvedRelay] = await ctx.resolveCalls([relayCallSyntax.ref]);
          assert.ok(resolvedRelay?.call);
          const [relayCallees] = await ctx.calleesOf([resolvedRelay.call]);
          wsTargetsCount = relayCallees?.targets.length ?? 0;

          const [relayDecl] = relayCallees!.targets;
          assert.ok(relayDecl);
          const [relayCallers] = await ctx.callersOf([relayDecl]);
          wsCallerCount = relayCallers?.calls.length ?? 0;

          const varDecls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["VariableDeclaration"],
          });
          const originInit = varDecls.find((d) =>
            d.text.startsWith("origin ="),
          )?.initializer;
          assert.ok(originInit);

          const relayedDecl = varDecls.find((d) =>
            d.text.startsWith("relayed ="),
          );
          const boxedDecl = varDecls.find((d) => d.text.startsWith("boxed ="));
          const [relayedRes, boxedRes] = await ctx.resolveDeclarations([
            relayedDecl!.ref,
            boxedDecl!.ref,
          ]);

          const prop = await ctx.valuePropagation({
            propagation: "value",
            from: originInit,
            to: [relayedRes!.declaration!, boxedRes!.declaration!],
          });

          relayedReachable = prop.results[0]!.reachable;
          relayedFiles = prop.results[0]!.evidence.map(
            (e) => e.at?.file,
          ).filter((f): f is string => f !== undefined);
          boxedReachable = prop.results[1]!.reachable;
          boxedGapKinds = prop.results[1]!.gaps.map((g) => g.kind);
        } finally {
          await session.close();
        }

        assert.equal(wsTargetsCount, 1);
        assert.equal(wsCallerCount, 1);
        assert.equal(relayedReachable, true);
        assert.ok(
          relayedFiles.some((f) =>
            f.includes("packages/ws-bridge/src/index.ts"),
          ),
          `Expected workspace file in evidence, got ${relayedFiles.join(", ")}`,
        );
        assert.equal(boxedReachable, false);
        assert.ok(boxedGapKinds.includes("missing-function-body"));
      },
    );
  });

  void it("invalidates call and value propagation caches on session.update() and rejects stale refs", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-update" },
      async (host) => {
        await host.typecheck();

        const runPropCheck = async (
          ctx: ReturnType<Awaited<ReturnType<typeof openProject>>["project"]>,
        ) => {
          const calls = await ctx.findSyntax({
            files: ["src/main.ts"],
            kinds: ["CallExpression"],
          });
          const relayCall = calls.find((c) => c.text.startsWith("relay("));
          assert.ok(relayCall);
          const [resolvedCall] = await ctx.resolveCalls([relayCall.ref]);
          assert.ok(resolvedCall?.call);
          const [callees] = await ctx.calleesOf([resolvedCall.call]);
          // Query a second time in the same snapshot to exercise SDK cache reuse
          const [calleesCached] = await ctx.calleesOf([resolvedCall.call]);
          assert.deepEqual(calleesCached, callees);

          const targetDecl = callees?.targets[0];
          assert.ok(targetDecl);
          const callersFirst = await ctx.callersOf([targetDecl]);
          const callersSecond = await ctx.callersOf([targetDecl]);
          assert.deepEqual(callersSecond, callersFirst);

          const varDecls = await ctx.findSyntax({
            files: ["src/main.ts"],
            kinds: ["VariableDeclaration"],
          });
          const originInit = varDecls.find((d) =>
            d.text.startsWith("origin ="),
          )?.initializer;
          const outDecl = varDecls.find((d) => d.text.startsWith("out ="));
          assert.ok(originInit && outDecl);
          const [outRes] = await ctx.resolveDeclarations([outDecl.ref]);
          assert.ok(outRes?.declaration);

          const prop = await ctx.valuePropagation({
            propagation: "value",
            from: originInit,
            to: [outRes.declaration],
          });
          const propCached = await ctx.valuePropagation({
            propagation: "value",
            from: originInit,
            to: [outRes.declaration],
          });
          assert.deepEqual(propCached, prop);

          return {
            targetDecl,
            callRef: resolvedCall.call,
            originInit,
            reachable: prop.results[0]?.reachable ?? false,
          };
        };

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });

        try {
          const snap1 = await runPropCheck(session.project());
          assert.equal(snap1.reachable, true);

          // Update helper.ts to stop returning the argument -> propagation must invalidate and clear reachability
          host.writeText(
            "src/helper.ts",
            [
              "export function relay(_x: string): string {",
              '  return "constant";',
              "}",
              "",
            ].join("\n"),
          );
          await session.update({ changed: ["src/helper.ts"] });

          const ctx2 = session.project();
          const snap2 = await runPropCheck(ctx2);
          assert.equal(snap2.reachable, false);

          // Passing stale DeclarationRef / CallRef / SyntaxRef from snapshot 1 must fail with STALE_REFERENCE
          await assert.rejects(
            () => ctx2.callersOf([snap1.targetDecl]),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );

          await assert.rejects(
            () => ctx2.calleesOf([snap1.callRef]),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );

          await assert.rejects(
            () =>
              ctx2.valuePropagation({
                propagation: "value",
                from: snap1.originInit,
                to: [snap1.targetDecl],
              }),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("distinguishes direct flow reachability from dynamic-callee gaps", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-gap-policy" },
      async (host) => {
        await host.typecheck();

        const queryCase = async (options: {
          readonly checkDirectFlow: boolean;
          readonly includeDynamicCall: boolean;
        }) => {
          const session = await openProject({
            cwd: host.rootDir,
            tsconfig: "tsconfig.json",
          });
          try {
            const ctx = session.project();
            const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
            const relevantCalls = calls.filter(
              (c) =>
                c.text.startsWith("knownSink(") ||
                (options.includeDynamicCall && c.text.startsWith("cb(")),
            );
            const resolved = await ctx.resolveCalls(
              relevantCalls.map((c) => c.ref),
            );
            const callees = await ctx.calleesOf(
              resolved.map((r) => r.call!).filter(Boolean),
            );
            const gaps = callees.flatMap((row) =>
              row.completeness === "incomplete" ? row.gaps : [],
            );

            let reachable = false;
            if (options.checkDirectFlow) {
              const varDecls = await ctx.findSyntax({
                kinds: ["VariableDeclaration"],
              });
              const secretInit = varDecls.find((d) =>
                d.text.startsWith("secret ="),
              )?.initializer;
              const safeDecl = varDecls.find((d) =>
                d.text.startsWith("safe ="),
              );
              assert.ok(secretInit && safeDecl);
              const [safeRes] = await ctx.resolveDeclarations([safeDecl.ref]);
              const prop = await ctx.valuePropagation({
                propagation: "value",
                from: secretInit,
                to: [safeRes!.declaration!],
              });
              reachable = prop.results[0]?.reachable ?? false;
            }
            return { reachable, gaps };
          } finally {
            await session.close();
          }
        };

        // (a) Known sink without dynamic call: zero gaps
        const knownOnly = await queryCase({
          checkDirectFlow: false,
          includeDynamicCall: false,
        });
        assert.equal(knownOnly.reachable, false);
        assert.deepEqual(knownOnly.gaps, []);

        // (b) Dynamic call included without flow query: gap present
        const withDynamicGap = await queryCase({
          checkDirectFlow: false,
          includeDynamicCall: true,
        });
        assert.equal(withDynamicGap.reachable, false);
        assert.equal(withDynamicGap.gaps.length, 1);
        assert.equal(withDynamicGap.gaps[0]?.kind, "dynamic-callee");

        // (c) Direct flow and dynamic gap coexisting
        const flowAndGap = await queryCase({
          checkDirectFlow: true,
          includeDynamicCall: true,
        });
        assert.equal(flowAndGap.reachable, true);
        assert.equal(flowAndGap.gaps.length, 1);
        assert.equal(flowAndGap.gaps[0]?.kind, "dynamic-callee");
      },
    );
  });

  void it("resolves callersOf across default exports, exported variable aliases, constructors, super(), and string element calls", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-exports-and-ctors" },
      async (host) => {
        await host.typecheck();

        let internalCallerLines: number[] = [];
        let defaultCallerLines: number[] = [];
        let ctorCallerLines: number[] = [];
        let pingCallerLines: number[] = [];

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const defsNodes = await ctx.findSyntax({
            files: ["src/defs.ts"],
            kinds: ["FunctionDeclaration", "Constructor", "MethodDeclaration"],
          });
          const decls = await ctx.resolveDeclarations(
            defsNodes.map((n) => n.ref),
            { select: { name: true, kind: true } },
          );

          const internalDecl = decls.find(
            (d) => d.name === "internalMake",
          )?.declaration;
          const defaultDecl = decls.find(
            (d) => d.name === "default" || d.name === "defaultFactory",
          )?.declaration;
          const ctorDecl = decls.find(
            (d) => d.kind === "Constructor",
          )?.declaration;
          const pingDecl = decls.find((d) => d.name === "ping")?.declaration;

          assert.ok(internalDecl && defaultDecl && ctorDecl && pingDecl);
          const rows = await ctx.callersOf([
            internalDecl,
            defaultDecl,
            ctorDecl,
            pingDecl,
          ]);
          internalCallerLines = rows[0]!.callSites.map((s) => s.location.line);
          defaultCallerLines = rows[1]!.callSites.map((s) => s.location.line);
          ctorCallerLines = rows[2]!.callSites.map((s) => s.location.line);
          pingCallerLines = rows[3]!.callSites.map((s) => s.location.line);
        } finally {
          await session.close();
        }

        assert.deepEqual(internalCallerLines, [9]);
        assert.deepEqual(defaultCallerLines, [11]);
        assert.deepEqual(ctorCallerLines, [4, 10]);
        assert.deepEqual(pingCallerLines, [12, 12]);
      },
    );
  });

  void it("finds callers of functions returned by a factory", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-returned-factory" },
      async (host) => {
        await host.typecheck();

        let callerLocations: string[] = [];
        let gapKinds: string[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const declarations = await ctx.findSyntax({
            files: ["src/defs.ts"],
            kinds: ["FunctionExpression"],
          });
          const projected = await ctx.resolveDeclarations(
            declarations.map((node) => node.ref),
            { select: { name: true } },
          );
          const target = projected.find(
            (row) => row.name === "computeUnique",
          )?.declaration;
          assert.ok(target);

          const callers = await ctx.callersOf([target]);
          callerLocations = callers[0]!.callSites.map(
            (site) => `${site.location.file}:${site.location.line}`,
          );
          callerLocations.sort();
          gapKinds =
            callers[0]!.completeness === "incomplete"
              ? callers[0]!.gaps.map((gap) => gap.kind)
              : [];
        } finally {
          await session.close();
        }

        assert.deepEqual(callerLocations, [
          "src/defs.ts:3",
          "src/entry.ts:3",
          "src/entry.ts:4",
        ]);
        assert.deepEqual(gapKinds, []);
      },
    );
  });

  void it("finds callers through a generic identity function", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-generic-identity" },
      async (host) => {
        await host.typecheck();

        let callerLocations: string[] = [];
        let gapKinds: string[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const callSyntaxes = await ctx.findSyntax({
            files: ["src/entry.ts"],
            kinds: ["CallExpression"],
          });
          const targetCall = callSyntaxes.find(
            (call) => call.location.line === 3,
          );
          assert.ok(targetCall);
          const [resolvedCall] = await ctx.resolveCalls([targetCall.ref]);
          assert.ok(resolvedCall?.call);
          const [callee] = await ctx.calleesOf([resolvedCall.call]);
          const target = callee?.targets[0];
          assert.ok(target);
          const [callers] = await ctx.callersOf([target]);
          callerLocations = callers!.callSites.map(
            (site) => `${site.location.file}:${site.location.line}`,
          );
          callerLocations.sort();
          gapKinds =
            callers!.completeness === "incomplete"
              ? callers!.gaps.map((gap) => gap.kind)
              : [];
        } finally {
          await session.close();
        }

        assert.deepEqual(callerLocations, [
          "src/entry.ts:3",
          "src/library.ts:2",
        ]);
        assert.deepEqual(gapKinds, []);
      },
    );
  });

  void it("limits reverse caller and return searches to normalized lookup files without mixing cached scopes", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-lookup-files" },
      async (host) => {
        await host.typecheck();

        let observed: Record<string, readonly string[]> = {};
        let gaps: Record<string, readonly string[]> = {};
        let propagation: Record<string, readonly boolean[]> = {};
        let propagationGaps: Record<string, readonly string[]> = {};

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const functions = await ctx.findSyntax({
            files: ["src/library.ts"],
            kinds: ["FunctionExpression", "FunctionDeclaration"],
          });
          const declarations = await ctx.resolveDeclarations(
            functions.map((node) => node.ref),
            { select: { name: true } },
          );
          const factoryTarget = declarations.find(
            (row) => row.name === "factoryResult",
          )?.declaration;
          const forwardTarget = declarations.find(
            (row) => row.name === "forward",
          )?.declaration;
          assert.ok(factoryTarget && forwardTarget);

          const entryCalls = await ctx.findSyntax({
            files: ["src/entry.ts"],
            kinds: ["CallExpression"],
          });
          const genericCall = entryCalls.find(
            (call) => call.location.line === 6,
          );
          const returnValue = await ctx.findSyntax({
            files: ["src/library.ts"],
            kinds: ["Identifier"],
          });
          assert.ok(genericCall && returnValue.length > 0);
          const [resolvedGenericCall] = await ctx.resolveCalls([
            genericCall.ref,
          ]);
          assert.ok(resolvedGenericCall?.call);
          const [genericCallee] = await ctx.calleesOf([
            resolvedGenericCall.call,
          ]);
          const genericTarget = genericCallee?.targets[0];
          assert.ok(genericTarget);

          const factoryFull = await ctx.callersOf([factoryTarget]);
          const factoryEntryOnly = await ctx.callersOf([factoryTarget], {
            lookupFiles: ["src/entry.ts"],
          });
          const factoryEmpty = await ctx.callersOf([factoryTarget], {
            lookupFiles: [],
          });
          const factoryNormalized = await ctx.callersOf([factoryTarget], {
            lookupFiles: [
              host.resolvePath("src/entry.ts"),
              "src/./entry.ts",
              "src/entry.ts",
            ],
          });
          observed.factoryFull = factoryFull[0]!.callSites.map(
            (site) => `${site.location.file}:${site.location.line}`,
          );
          observed.factoryEntryOnly = factoryEntryOnly[0]!.callSites.map(
            (site) => `${site.location.file}:${site.location.line}`,
          );
          observed.factoryEmpty = factoryEmpty[0]!.callSites.map(
            (site) => `${site.location.file}:${site.location.line}`,
          );
          observed.factoryNormalized = factoryNormalized[0]!.callSites.map(
            (site) => `${site.location.file}:${site.location.line}`,
          );
          gaps.factoryFull =
            factoryFull[0]!.completeness === "incomplete"
              ? factoryFull[0]!.gaps.map((gap) => gap.kind)
              : [];
          gaps.factoryEntryOnly =
            factoryEntryOnly[0]!.completeness === "incomplete"
              ? factoryEntryOnly[0]!.gaps.map((gap) => gap.kind)
              : [];

          const genericEntryOnly = await ctx.callersOf([genericTarget], {
            lookupFiles: ["src/entry.ts"],
          });
          const genericFull = await ctx.callersOf([genericTarget]);
          observed.genericEntryOnly = genericEntryOnly[0]!.callSites.map(
            (site) => `${site.location.file}:${site.location.line}`,
          );
          observed.genericFull = genericFull[0]!.callSites.map(
            (site) => `${site.location.file}:${site.location.line}`,
          );
          gaps.genericEntryOnly =
            genericEntryOnly[0]!.completeness === "incomplete"
              ? genericEntryOnly[0]!.gaps.map((gap) => gap.kind)
              : [];
          gaps.genericFull =
            genericFull[0]!.completeness === "incomplete"
              ? genericFull[0]!.gaps.map((gap) => gap.kind)
              : [];

          await assert.rejects(
            () =>
              ctx.callersOf([factoryTarget], {
                lookupFiles: ["src/not-in-project.ts"],
              }),
            (error: unknown) =>
              error instanceof TssaeError &&
              error.message.includes("not a source file in project"),
          );

          const forwardCalls = await ctx.findSyntax({
            kinds: ["CallExpression"],
            files: ["src/entry.ts", "src/other.ts"],
          });
          const from = returnValue.find(
            (node) => node.location.line === 13 && node.text === "value",
          );
          const entryTarget = forwardCalls.find(
            (node) =>
              node.location.file === "src/entry.ts" && node.location.line === 7,
          );
          const otherTarget = forwardCalls.find(
            (node) =>
              node.location.file === "src/other.ts" && node.location.line === 4,
          );
          assert.ok(from && entryTarget && otherTarget);
          const scopedPropagation = await ctx.valuePropagation({
            from: from.ref,
            to: [entryTarget.ref, otherTarget.ref],
            lookupFiles: ["src/entry.ts"],
          });
          const fullPropagation = await ctx.valuePropagation({
            from: from.ref,
            to: [entryTarget.ref, otherTarget.ref],
          });
          const emptyPropagation = await ctx.valuePropagation({
            from: from.ref,
            to: [entryTarget.ref, otherTarget.ref],
            lookupFiles: [],
          });
          propagation.scoped = scopedPropagation.results.map(
            (row) => row.reachable,
          );
          propagation.full = fullPropagation.results.map(
            (row) => row.reachable,
          );
          propagation.empty = emptyPropagation.results.map(
            (row) => row.reachable,
          );
          propagationGaps.scoped = scopedPropagation.gaps.map(
            (gap) => gap.kind,
          );
          propagationGaps.full = fullPropagation.gaps.map((gap) => gap.kind);
          propagationGaps.empty = emptyPropagation.gaps.map((gap) => gap.kind);
        } finally {
          await session.close();
        }

        assert.deepEqual(observed.factoryFull, [
          "src/entry.ts:3",
          "src/entry.ts:4",
          "src/library.ts:3",
          "src/other.ts:2",
        ]);
        assert.deepEqual(observed.factoryEntryOnly, [
          "src/entry.ts:3",
          "src/entry.ts:4",
        ]);
        assert.deepEqual(observed.factoryEmpty, []);
        assert.deepEqual(observed.factoryNormalized, observed.factoryEntryOnly);
        assert.deepEqual(gaps.factoryFull, []);
        assert.deepEqual(gaps.factoryEntryOnly, []);
        assert.deepEqual(observed.genericEntryOnly, ["src/entry.ts:6"]);
        assert.deepEqual(observed.genericFull, [
          "src/entry.ts:6",
          "src/library.ts:7",
          "src/other.ts:3",
        ]);
        assert.deepEqual(gaps.genericEntryOnly, []);
        assert.deepEqual(gaps.genericFull, []);
        assert.deepEqual(propagation.scoped, [true, false]);
        assert.deepEqual(propagation.full, [true, true]);
        assert.deepEqual(propagation.empty, [false, false]);
        assert.deepEqual(propagationGaps.scoped, []);
        assert.deepEqual(propagationGaps.full, []);
        assert.deepEqual(propagationGaps.empty, []);
      },
    );
  });

  void it("resolves polymorphic overrides with closed/open completeness and tracks closure, ||=, destructuring, for-of, and callback gaps without signature fallback", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "ir-call-resolution-regressions",
            private: true,
            type: "module",
          }),
          "tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              noEmit: true,
            },
            include: ["src/**/*.ts"],
          }),
          "src/index.ts": [
            "function fnA(): number {",
            "  return 1;",
            "}",
            "function fnB(): number {",
            "  return 2;",
            "}",
            "",
            "class ClosedBase {",
            "  run(): number {",
            "    return 10;",
            "  }",
            "}",
            "class ClosedSub extends ClosedBase {",
            "  override run(): number {",
            "    return 20;",
            "  }",
            "}",
            "export class OpenBase {",
            "  run(): number {",
            "    return 30;",
            "  }",
            "}",
            "export class OpenSub extends OpenBase {",
            "  override run(): number {",
            "    return 40;",
            "  }",
            "}",
            "",
            "export function callPoly(closed: ClosedBase, open: OpenBase): number {",
            "  const exact = new OpenSub();",
            "  return closed.run() + open.run() + exact.run();",
            "}",
            "",
            "export function closureMutation(): number {",
            "  let fn = fnA;",
            "  function mutate() {",
            "    fn = fnB;",
            "  }",
            "  mutate();",
            "  return fn();",
            "}",
            "",
            "export function logicalOrAssign(flag: boolean): number {",
            "  let fn: (() => number) | undefined = flag ? fnA : undefined;",
            "  fn ||= fnB;",
            "  return fn();",
            "}",
            "",
            "export function destructureAndForOf(): number {",
            "  const { run: objFn } = { run: fnA };",
            "  const [, secondFn] = [fnA, fnB];",
            "  let sum = objFn() + secondFn();",
            "  for (const itemFn of [fnA, fnB]) {",
            "    sum += itemFn();",
            "  }",
            "  return sum;",
            "}",
            "",
            "export function mapCallbackNoFallback(): number[] {",
            "  return [fnA, fnB].map((f) => f());",
            "}",
            "",
            "export function plainCallbackNoFallback(cb: () => number): number {",
            "  return cb();",
            "}",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const calls = await ctx.findSyntax({
            files: ["src/index.ts"],
            kinds: ["CallExpression"],
          });
          const resolveByPrefix = async (prefix: string): Promise<CallRef> => {
            const syn = calls.find(
              (c) => c.text === prefix || c.text.startsWith(prefix),
            );
            assert.ok(syn, `Missing call matching ${prefix}`);
            const [res] = await ctx.resolveCalls([syn.ref]);
            assert.ok(res?.call, `resolveCalls returned null for ${prefix}`);
            return res.call;
          };

          const closedCall = await resolveByPrefix("closed.run()");
          const openCall = await resolveByPrefix("open.run()");
          const exactCall = await resolveByPrefix("exact.run()");
          const objDestructCall = await resolveByPrefix("objFn()");
          const arrDestructCall = await resolveByPrefix("secondFn()");
          const forOfCall = await resolveByPrefix("itemFn()");
          const mapCbCall = await resolveByPrefix("f()");
          const plainCbCall = await resolveByPrefix("cb()");

          // Verify both `fn()` call sites explicitly by line number
          const allCallsResolved = await ctx.resolveCalls(
            calls.map((c) => c.ref),
            { select: { location: true } },
          );
          const callByLineAndText = (text: string, line: number): CallRef => {
            const idx = calls.findIndex(
              (c) => c.text === text && c.location.line === line,
            );
            assert.notEqual(idx, -1, `Missing call '${text}' at line ${line}`);
            const call = allCallsResolved[idx]?.call;
            assert.ok(call);
            return call;
          };

          const closureFnCall = callByLineAndText("fn()", 40);
          const logicalOrFnCall = callByLineAndText("fn()", 46);

          const [
            closedRes,
            openRes,
            exactRes,
            closureRes,
            orRes,
            objRes,
            arrRes,
            forOfRes,
            mapCbRes,
            plainCbRes,
          ] = await ctx.calleesOf([
            closedCall,
            openCall,
            exactCall,
            closureFnCall,
            logicalOrFnCall,
            objDestructCall,
            arrDestructCall,
            forOfCall,
            mapCbCall,
            plainCbCall,
          ]);

          // 1. Polymorphic overrides: unexported ClosedBase -> complete with [10, 20]; exported OpenBase -> incomplete with [30, 40] + gap
          assert.equal(closedRes!.completeness, "complete");
          assert.deepEqual(
            closedRes!.resolvedTargets.map((t) => t.location?.line),
            [9, 14],
          );

          assert.equal(openRes!.completeness, "incomplete");
          assert.deepEqual(
            openRes!.resolvedTargets.map((t) => t.location?.line),
            [19, 24],
          );
          assert.ok(
            openRes!.completeness === "incomplete" &&
              openRes!.gaps.some((g) => g.kind === "open-external-hierarchy"),
          );

          assert.equal(exactRes!.completeness, "complete");
          assert.deepEqual(
            exactRes!.resolvedTargets.map((t) => t.location?.line),
            [24],
          );

          // 2. Closure mutation
          assert.equal(closureRes!.completeness, "complete");
          assert.deepEqual(
            closureRes!.resolvedTargets.map((t) => t.name),
            ["fnA", "fnB"],
          );

          // 3. ||= logical assignment
          assert.equal(orRes!.completeness, "complete");
          assert.deepEqual(
            orRes!.resolvedTargets.map((t) => t.name),
            ["fnA", "fnB"],
          );

          // 4. Destructuring & for-of
          assert.equal(objRes!.completeness, "complete");
          assert.deepEqual(
            objRes!.resolvedTargets.map((t) => t.name),
            ["fnA"],
          );

          assert.equal(arrRes!.completeness, "complete");
          assert.deepEqual(
            arrRes!.resolvedTargets.map((t) => t.name),
            ["fnB"],
          );

          assert.equal(forOfRes!.completeness, "complete");
          assert.deepEqual(
            forOfRes!.resolvedTargets.map((t) => t.name),
            ["fnA", "fnB"],
          );

          // 5. [fnA, fnB].map(f => f()) uses the built-in Array.prototype.map summary,
          // while an unsummarized callback parameter cb() still yields no signature fallback.
          assert.equal(mapCbRes!.completeness, "complete");
          assert.deepEqual(
            mapCbRes!.resolvedTargets.map((t) => t.name),
            ["fnA", "fnB"],
          );
          assert.ok(
            mapCbRes!.evidence.some(
              (ev) => ev.kind === "summary" && ev.summarySource === "builtin",
            ),
          );

          assert.equal(plainCbRes!.completeness, "incomplete");
          assert.deepEqual(plainCbRes!.targets, []);
          assert.ok(
            plainCbRes!.completeness === "incomplete" &&
              plainCbRes!.gaps.some((g) => g.kind === "dynamic-callee"),
          );

          // Verify callersOf on OpenSub.run includes open.run() & exact.run() and is incomplete due to open hierarchy
          const openSubRunDecl = openRes!.targets[1]!;
          const [openSubCallers] = await ctx.callersOf([openSubRunDecl]);
          assert.equal(openSubCallers!.completeness, "incomplete");
          assert.equal(openSubCallers!.callSites.length, 2);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("includes implicit getter, setter, using dispose, tagged template, and super calls in calleesOf and callersOf", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "ir-implicit-calls",
            private: true,
            type: "module",
          }),
          "tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              lib: ["ES2022", "ESNext.Disposable"],
              noEmit: true,
            },
            include: ["src/**/*.ts"],
          }),
          "src/index.ts": [
            "function html(strings: TemplateStringsArray): string {",
            '  return strings.join("");',
            "}",
            "class Parent {",
            "  constructor(public v: number) {}",
            "}",
            "class Child extends Parent {",
            "  private _val: number;",
            "  constructor(v: number) {",
            "    super(v);",
            "    this._val = v;",
            "  }",
            "  get val(): number {",
            "    return this._val;",
            "  }",
            "  set val(next: number) {",
            "    this._val = next;",
            "  }",
            "  [Symbol.dispose](): void {}",
            "}",
            "export function useAll(): number {",
            "  using c = new Child(1);",
            "  c.val = 2;",
            "  void html`ok`;",
            "  return c.val;",
            "}",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const candidates = await ctx.findSyntax({
            files: ["src/index.ts"],
            kinds: [
              "CallExpression",
              "TaggedTemplateExpression",
              "PropertyAccessExpression",
              "BinaryExpression",
              "VariableDeclaration",
            ],
          });

          const taggedSyn = candidates.find((s) => s.text === "html`ok`");
          const superSyn = candidates.find((s) => s.text === "super(v)");
          const getterSyn = candidates.find(
            (s) => s.text === "c.val" && s.location.line === 25,
          );
          const setterSyn = candidates.find((s) => s.text === "c.val = 2");
          const usingSyn = candidates.find(
            (s) =>
              s.text.startsWith("c = new Child(1)") && s.location.line === 22,
          );
          const nonGetterProp = candidates.find(
            (s) => s.text === "this._val" && s.location.line === 14,
          );
          assert.ok(
            taggedSyn &&
              superSyn &&
              getterSyn &&
              setterSyn &&
              usingSyn &&
              nonGetterProp,
          );

          const resolved = await ctx.resolveCalls([
            taggedSyn.ref,
            superSyn.ref,
            getterSyn.ref,
            setterSyn.ref,
            usingSyn.ref,
            nonGetterProp.ref,
          ]);
          assert.ok(resolved[0]?.call);
          assert.ok(resolved[1]?.call);
          assert.ok(resolved[2]?.call);
          assert.ok(resolved[3]?.call);
          assert.ok(resolved[4]?.call);
          assert.equal(resolved[5]?.call, null);

          const callees = await ctx.calleesOf([
            resolved[0].call,
            resolved[1].call,
            resolved[2].call,
            resolved[3].call,
            resolved[4].call,
          ]);
          assert.equal(callees[0]?.resolvedTargets[0]?.name, "html");
          assert.equal(callees[1]?.resolvedTargets[0]?.name, "constructor");
          assert.equal(callees[2]?.resolvedTargets[0]?.location?.line, 13);
          assert.equal(callees[3]?.resolvedTargets[0]?.location?.line, 16);
          assert.equal(callees[4]?.resolvedTargets[0]?.location?.line, 19);

          const [htmlCallers] = await ctx.callersOf([callees[0]!.targets[0]!]);
          assert.equal(htmlCallers?.callSites.length, 1);
          assert.equal(htmlCallers?.callSites[0]?.location.line, 24);
        } finally {
          await session.close();
        }
      },
    );
  });
});
