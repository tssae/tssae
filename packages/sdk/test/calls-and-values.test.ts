import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  VeruleExecutionError,
  defineConfig,
  defineRule,
  openSession,
  runCheck,
  type CallRef,
  type DeclarationRef,
  type SyntaxRef,
} from "@verule/sdk";
import {
  copyDirectory,
  linkNodeModulesPackage,
  resolveHostDir,
  withTempHost,
} from "@verule/test-hosts";

void describe("@verule/sdk call graph and value propagation", () => {
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

        const rule = defineRule({
          id: "inspect-call-graph",
          run: async (ctx) => {
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
            assert.deepEqual(aliasedResult.gaps, []);
            aliasedTargetFile =
              aliasedResult.resolvedTargets[0]?.location?.file;
            aliasedTargetName = aliasedResult.resolvedTargets[0]?.name;
            aliasedBindingParamName =
              aliasedResult.resolvedTargets[0]?.parameterBindings[0]
                ?.parameterName;
            aliasedReturnCount =
              aliasedResult.resolvedTargets[0]?.returnExpressions.length ?? 0;

            const mixedResult = callees[1]!;
            mixedTargetNames = mixedResult.resolvedTargets.map((t) => t.name);
            mixedGapKinds = mixedResult.gaps.map((g) => g.kind);

            const ifaceResult = callees[2]!;
            ifaceGapKinds = ifaceResult.gaps.map((g) => g.kind);
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [rule],
          }),
        });

        assert.equal(report.status, "clean");
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
        assert.ok(ifaceGapKinds.includes("dynamic-callee"));
        assert.ok(report.gaps.length >= 2);
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

        const rule = defineRule({
          id: "mixed-ts-js-propagation",
          run: async (ctx) => {
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
            const varDeclRef = async (
              name: string,
            ): Promise<DeclarationRef> => {
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
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [rule],
          }),
        });

        assert.equal(report.status, "clean");
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

        const rule = defineRule({
          id: "workspace-symlink-forwarding",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({
              files: ["src/app.ts"],
              kinds: ["CallExpression"],
            });
            const relayCallSyntax = calls.find((c) =>
              c.text.startsWith("relayValue("),
            );
            assert.ok(relayCallSyntax);
            const [resolvedRelay] = await ctx.resolveCalls([
              relayCallSyntax.ref,
            ]);
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
            const boxedDecl = varDecls.find((d) =>
              d.text.startsWith("boxed ="),
            );
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
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [rule],
          }),
        });

        assert.equal(report.status, "clean");
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
        assert.ok(report.gaps.some((g) => g.kind === "missing-function-body"));
      },
    );
  });

  void it("invalidates call and value propagation caches on session.update() and rejects stale refs", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-update" },
      async (host) => {
        await host.typecheck();

        let staleDeclRef: DeclarationRef | undefined;
        let staleCallRef: CallRef | undefined;
        let staleSyntaxRef: SyntaxRef | undefined;

        const inspectRule = defineRule({
          id: "inspect-prop",
          run: async (ctx) => {
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
            await ctx.calleesOf([resolvedCall.call]);
            const targetDecl = callees?.targets[0];
            assert.ok(targetDecl);
            await ctx.callersOf([targetDecl]);
            await ctx.callersOf([targetDecl]);

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
            await ctx.valuePropagation({
              propagation: "value",
              from: originInit,
              to: [outRes.declaration],
            });

            staleDeclRef ??= targetDecl;
            staleCallRef ??= resolvedCall.call;
            staleSyntaxRef ??= originInit;

            if (prop.results[0]?.reachable) {
              ctx.report({
                at: relayCall.location,
                message: "Origin propagates through relay()",
                basis: [prop.results[0]],
              });
            }
          },
        });

        const session = await openSession({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [inspectRule],
          }),
        });

        try {
          const report1 = await session.check({ observe: "summary" });
          assert.equal(report1.status, "findings");
          assert.equal(report1.diagnostics.length, 1);
          assert.ok((report1.observation?.reusedQueryCount ?? 0) >= 3);

          // Update helper.ts to stop returning the argument -> propagation must invalidate and clear finding
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

          const report2 = await session.check();
          assert.equal(report2.status, "clean");
          assert.equal(report2.diagnostics.length, 0);
          assert.equal("observation" in report2, false);

          // Passing stale DeclarationRef / CallRef / SyntaxRef from snapshot 1 must fail with STALE_REFERENCE
          await assert.rejects(
            () =>
              session.check([
                defineRule({
                  id: "use-stale-decl",
                  run: async (ctx) => {
                    await ctx.callersOf([staleDeclRef!]);
                  },
                }),
              ]),
            (err: unknown) =>
              err instanceof VeruleExecutionError &&
              err.code === "STALE_REFERENCE",
          );

          await assert.rejects(
            () =>
              session.check([
                defineRule({
                  id: "use-stale-call",
                  run: async (ctx) => {
                    await ctx.calleesOf([staleCallRef!]);
                  },
                }),
              ]),
            (err: unknown) =>
              err instanceof VeruleExecutionError &&
              err.code === "STALE_REFERENCE",
          );

          await assert.rejects(
            () =>
              session.check([
                defineRule({
                  id: "use-stale-endpoint",
                  run: async (ctx) => {
                    await ctx.valuePropagation({
                      propagation: "value",
                      from: staleSyntaxRef!,
                      to: [staleDeclRef!],
                    });
                  },
                }),
              ]),
            (err: unknown) =>
              err instanceof VeruleExecutionError &&
              err.code === "STALE_REFERENCE",
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("distinguishes clean, gaps-only, findings-with-gaps, and gap-escalated-to-violation with observe off", async () => {
    await withTempHost(
      { host: "sdk-queries/calls-values-gap-policy" },
      async (host) => {
        await host.typecheck();

        const makeRule = (options: {
          readonly reportDirectFlow: boolean;
          readonly escalateGap: boolean;
          readonly includeDynamicCall: boolean;
        }) =>
          defineRule({
            id: "flow-and-gap-policy",
            run: async (ctx) => {
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

              if (options.reportDirectFlow) {
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
                if (prop.results[0]?.reachable) {
                  ctx.report({
                    at: safeDecl.location,
                    message: "Secret reached knownSink result",
                    basis: [prop.results[0]],
                  });
                }
              }

              if (options.escalateGap) {
                for (const row of callees) {
                  for (const gap of row.gaps) {
                    if (gap.at) {
                      ctx.report({
                        at: gap.at,
                        message: `Unresolved dynamic target escalated to error: ${gap.message}`,
                        basis: [
                          { kind: "note", message: gap.kind, at: gap.at },
                        ],
                      });
                    }
                  }
                }
              }
            },
          });

        // (a) No violations and no gaps
        const cleanNoGaps = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [
              makeRule({
                reportDirectFlow: false,
                escalateGap: false,
                includeDynamicCall: false,
              }),
            ],
          }),
        });
        assert.equal(cleanNoGaps.status, "clean");
        assert.deepEqual(cleanNoGaps.diagnostics, []);
        assert.deepEqual(cleanNoGaps.gaps, []);
        assert.equal("observation" in cleanNoGaps, false);

        // (b) No violations, but gap present
        const cleanWithGaps = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [
              makeRule({
                reportDirectFlow: false,
                escalateGap: false,
                includeDynamicCall: true,
              }),
            ],
          }),
        });
        assert.equal(cleanWithGaps.status, "clean");
        assert.deepEqual(cleanWithGaps.diagnostics, []);
        assert.equal(cleanWithGaps.gaps.length, 1);
        assert.equal(cleanWithGaps.gaps[0]?.kind, "dynamic-callee");
        assert.equal("observation" in cleanWithGaps, false);

        // (c) Violations and gaps coexisting
        const findingsAndGaps = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [
              makeRule({
                reportDirectFlow: true,
                escalateGap: false,
                includeDynamicCall: true,
              }),
            ],
          }),
        });
        assert.equal(findingsAndGaps.status, "findings");
        assert.equal(findingsAndGaps.diagnostics.length, 1);
        assert.equal(findingsAndGaps.gaps.length, 1);
        assert.equal(findingsAndGaps.gaps[0]?.kind, "dynamic-callee");
        assert.equal("observation" in findingsAndGaps, false);

        // (d) Rule actively escalates gap to violation
        const escalatedGap = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [
              makeRule({
                reportDirectFlow: false,
                escalateGap: true,
                includeDynamicCall: true,
              }),
            ],
          }),
        });
        assert.equal(escalatedGap.status, "findings");
        assert.equal(escalatedGap.diagnostics.length, 1);
        assert.match(escalatedGap.diagnostics[0]!.message, /escalated/u);
        assert.equal(escalatedGap.gaps.length, 1);
        assert.equal("observation" in escalatedGap, false);
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

        await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [
              defineRule({
                id: "lazy-call-graph-coverage",
                run: async (ctx) => {
                  const defsNodes = await ctx.findSyntax({
                    files: ["src/defs.ts"],
                    kinds: [
                      "FunctionDeclaration",
                      "Constructor",
                      "MethodDeclaration",
                    ],
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
                  const pingDecl = decls.find(
                    (d) => d.name === "ping",
                  )?.declaration;

                  assert.ok(
                    internalDecl && defaultDecl && ctorDecl && pingDecl,
                  );
                  const rows = await ctx.callersOf([
                    internalDecl,
                    defaultDecl,
                    ctorDecl,
                    pingDecl,
                  ]);
                  internalCallerLines = rows[0]!.callSites.map(
                    (s) => s.location.line,
                  );
                  defaultCallerLines = rows[1]!.callSites.map(
                    (s) => s.location.line,
                  );
                  ctorCallerLines = rows[2]!.callSites.map(
                    (s) => s.location.line,
                  );
                  pingCallerLines = rows[3]!.callSites.map(
                    (s) => s.location.line,
                  );
                },
              }),
            ],
          }),
        });

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
        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [
              defineRule({
                id: "returned-function-callers",
                run: async (ctx) => {
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
                  gapKinds = callers[0]!.gaps.map((gap) => gap.kind);
                },
              }),
            ],
          }),
        });

        assert.equal(report.status, "clean");
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
        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [
              defineRule({
                id: "generic-callers",
                run: async (ctx) => {
                  const callSyntaxes = await ctx.findSyntax({
                    files: ["src/entry.ts"],
                    kinds: ["CallExpression"],
                  });
                  const targetCall = callSyntaxes.find(
                    (call) => call.location.line === 3,
                  );
                  assert.ok(targetCall);
                  const [resolvedCall] = await ctx.resolveCalls([
                    targetCall.ref,
                  ]);
                  assert.ok(resolvedCall?.call);
                  const [callee] = await ctx.calleesOf([resolvedCall.call]);
                  const target = callee?.targets[0];
                  assert.ok(target);
                  const [callers] = await ctx.callersOf([target]);
                  callerLocations = callers!.callSites.map(
                    (site) => `${site.location.file}:${site.location.line}`,
                  );
                  callerLocations.sort();
                  gapKinds = callers!.gaps.map((gap) => gap.kind);
                },
              }),
            ],
          }),
        });

        assert.equal(report.status, "clean");
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
        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [
              defineRule({
                id: "scoped-reverse-query-contract",
                run: async (ctx) => {
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
                  const factoryEntryOnly = await ctx.callersOf(
                    [factoryTarget],
                    {
                      lookupFiles: ["src/entry.ts"],
                    },
                  );
                  const factoryEmpty = await ctx.callersOf([factoryTarget], {
                    lookupFiles: [],
                  });
                  const factoryNormalized = await ctx.callersOf(
                    [factoryTarget],
                    {
                      lookupFiles: [
                        host.resolvePath("src/entry.ts"),
                        "src/./entry.ts",
                        "src/entry.ts",
                      ],
                    },
                  );
                  observed.factoryFull = factoryFull[0]!.callSites.map(
                    (site) => `${site.location.file}:${site.location.line}`,
                  );
                  observed.factoryEntryOnly =
                    factoryEntryOnly[0]!.callSites.map(
                      (site) => `${site.location.file}:${site.location.line}`,
                    );
                  observed.factoryEmpty = factoryEmpty[0]!.callSites.map(
                    (site) => `${site.location.file}:${site.location.line}`,
                  );
                  observed.factoryNormalized =
                    factoryNormalized[0]!.callSites.map(
                      (site) => `${site.location.file}:${site.location.line}`,
                    );
                  gaps.factoryFull = factoryFull[0]!.gaps.map(
                    (gap) => gap.kind,
                  );
                  gaps.factoryEntryOnly = factoryEntryOnly[0]!.gaps.map(
                    (gap) => gap.kind,
                  );

                  const genericEntryOnly = await ctx.callersOf(
                    [genericTarget],
                    {
                      lookupFiles: ["src/entry.ts"],
                    },
                  );
                  const genericFull = await ctx.callersOf([genericTarget]);
                  observed.genericEntryOnly =
                    genericEntryOnly[0]!.callSites.map(
                      (site) => `${site.location.file}:${site.location.line}`,
                    );
                  observed.genericFull = genericFull[0]!.callSites.map(
                    (site) => `${site.location.file}:${site.location.line}`,
                  );
                  gaps.genericEntryOnly = genericEntryOnly[0]!.gaps.map(
                    (gap) => gap.kind,
                  );
                  gaps.genericFull = genericFull[0]!.gaps.map(
                    (gap) => gap.kind,
                  );

                  await assert.rejects(
                    () =>
                      ctx.callersOf([factoryTarget], {
                        lookupFiles: ["src/not-in-project.ts"],
                      }),
                    (error: unknown) =>
                      error instanceof VeruleExecutionError &&
                      error.message.includes("not a source file in project"),
                  );

                  const forwardCalls = await ctx.findSyntax({
                    kinds: ["CallExpression"],
                    files: ["src/entry.ts", "src/other.ts"],
                  });
                  const from = returnValue.find(
                    (node) =>
                      node.location.line === 13 && node.text === "value",
                  );
                  const entryTarget = forwardCalls.find(
                    (node) =>
                      node.location.file === "src/entry.ts" &&
                      node.location.line === 7,
                  );
                  const otherTarget = forwardCalls.find(
                    (node) =>
                      node.location.file === "src/other.ts" &&
                      node.location.line === 4,
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
                  propagationGaps.full = fullPropagation.gaps.map(
                    (gap) => gap.kind,
                  );
                  propagationGaps.empty = emptyPropagation.gaps.map(
                    (gap) => gap.kind,
                  );
                },
              }),
            ],
          }),
        });

        assert.equal(report.status, "clean");
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
});
