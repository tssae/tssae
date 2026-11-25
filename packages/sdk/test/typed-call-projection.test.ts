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
  type SignatureRef,
  type TypeRef,
} from "@verule/sdk";
import { withTempHost } from "@verule/test-hosts";

void describe("@verule/sdk typed call projection", () => {
  void it("queries local syntax by source range, resolves calls & declarations across aliases, and maps back to syntax", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-aliases" },
      async (host) => {
        await host.typecheck();

        let foundCallKind: string | undefined;
        let foundFuncBodyKind: string | undefined;
        let foundParamCount: number | undefined;
        let resolvedDeclName: string | null | undefined;
        let resolvedDeclFile: string | undefined;
        let resolvedDeclLine: number | undefined;
        let reverseSyntaxKind: string | undefined;
        let reverseSyntaxFile: string | undefined;
        let reverseSyntaxLine: number | undefined;
        let reverseCallHasLocation = false;
        let omittedKeyPresentOnRuntimeRow = true;

        const rule = defineRule({
          id: "range-to-decl-and-call",
          run: async (ctx) => {
            const rangeCalls = await ctx.findSyntax({
              range: {
                file: "src/caller.ts",
                startLine: 5,
                endLine: 5,
              },
              kinds: ["CallExpression"],
            });
            assert.equal(rangeCalls.length, 1);
            const callSyntax = rangeCalls[0]!;
            foundCallKind = callSyntax.kind;
            assert.equal(callSyntax.arguments?.length, 2);
            assert.ok(callSyntax.expression);

            const defFuncs = await ctx.findSyntax({
              range: {
                file: "src/defs.ts",
                startLine: 1,
                endLine: 3,
              },
              kinds: ["FunctionDeclaration"],
            });
            assert.equal(defFuncs.length, 1);
            foundParamCount = defFuncs[0]!.parameters?.length;
            assert.ok(defFuncs[0]!.body);

            const callResolutions = await ctx.resolveCalls([callSyntax.ref], {
              select: {
                location: true,
                calleeDeclaration: true,
              },
            });
            assert.equal(callResolutions.length, 1);
            const callRow = callResolutions[0]!;
            omittedKeyPresentOnRuntimeRow =
              Object.hasOwn(callRow, "signature") ||
              Object.hasOwn(callRow, "returnType");
            const callRef: CallRef | null = callRow.call;
            assert.ok(callRef);
            assert.deepEqual(callRow.gaps, []);

            const declResolutions = await ctx.resolveDeclarations(
              [callSyntax.expression],
              {
                select: {
                  name: true,
                  location: true,
                  syntax: true,
                  kind: true,
                },
              },
            );
            assert.equal(declResolutions.length, 1);
            const declRow = declResolutions[0]!;
            const declRef: DeclarationRef | null = declRow.declaration;
            assert.ok(declRef);
            assert.deepEqual(declRow.gaps, []);
            resolvedDeclName = declRow.name;
            resolvedDeclFile = declRow.location?.file;
            resolvedDeclLine = declRow.location?.line;

            const reversed = await ctx.syntaxOf([declRef, callRef], {
              select: {
                kind: true,
                location: true,
                body: true,
                parameters: true,
              },
            });
            assert.equal(reversed.length, 2);
            const declSyntaxRow = reversed[0]!;
            reverseSyntaxKind = declSyntaxRow.kind ?? undefined;
            reverseSyntaxFile = declSyntaxRow.location?.file;
            reverseSyntaxLine = declSyntaxRow.location?.line;
            assert.equal(declSyntaxRow.parameters?.length, 2);
            assert.ok(declSyntaxRow.body);

            const bodySyntax = await ctx.findSyntax({
              range: {
                file: "src/defs.ts",
                start: declSyntaxRow.location!.start,
                end: declSyntaxRow.location!.end,
              },
              kinds: ["Block"],
            });
            foundFuncBodyKind = bodySyntax[0]?.kind;

            const callSyntaxLookup = reversed[1]!;
            reverseCallHasLocation = callSyntaxLookup.location?.line === 5;
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
        assert.equal(foundCallKind, "CallExpression");
        assert.equal(foundFuncBodyKind, "Block");
        assert.equal(foundParamCount, 2);
        assert.equal(resolvedDeclName, "targetHelper");
        assert.equal(resolvedDeclFile, "src/defs.ts");
        assert.equal(resolvedDeclLine, 1);
        assert.equal(reverseSyntaxKind, "FunctionDeclaration");
        assert.equal(reverseSyntaxFile, "src/defs.ts");
        assert.equal(reverseSyntaxLine, 1);
        assert.equal(reverseCallHasLocation, true);
        assert.equal(omittedKeyPresentOnRuntimeRow, false);
      },
    );
  });

  void it("filters call syntax by static callee names", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-callee-names" },
      async (host) => {
        await host.typecheck();

        let callLines: number[] = [];
        let textMatchedCallLines: number[] = [];
        let emptyCalleeMatches = -1;
        const rule = defineRule({
          id: "static-callee-filter",
          run: async (ctx) => {
            callLines = (
              await ctx.findSyntax({
                kinds: ["CallExpression"],
                calleeNames: ["createMenu"],
              })
            ).map((call) => call.location.line);
            textMatchedCallLines = (
              await ctx.findSyntax({
                kinds: ["CallExpression"],
                textIncludes: ["methodName"],
              })
            ).map((call) => call.location.line);
            emptyCalleeMatches = (
              await ctx.findSyntax({
                kinds: ["CallExpression"],
                calleeNames: [],
              })
            ).length;
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({ tsconfig: "tsconfig.json", rules: [rule] }),
        });

        assert.equal(report.status, "clean");
        assert.deepEqual(callLines, [4, 5, 6]);
        assert.deepEqual(textMatchedCallLines, [7]);
        assert.equal(emptyCalleeMatches, 0);
      },
    );
  });

  void it("projects the semantic owner of a resolved declaration", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-owner" },
      async (host) => {
        await host.typecheck();

        const declarationOwners = new Map<string, string | null>();
        const rule = defineRule({
          id: "declaration-owner-projection",
          run: async (ctx) => {
            const calls = await ctx.findSyntax({
              kinds: ["CallExpression"],
              calleeNames: ["createMenu", "once", "_register"],
            });
            const rows = await ctx.resolveDeclarations(
              calls.map((call) => call.ref),
              {
                select: { name: true, containerName: true },
              },
            );
            for (const [index, row] of rows.entries()) {
              declarationOwners.set(calls[index]!.text, row.containerName);
            }
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({ tsconfig: "tsconfig.json", rules: [rule] }),
        });

        assert.equal(report.status, "clean");
        assert.equal(
          declarationOwners.get('menu.createMenu("main")'),
          "IMenuService",
        );
        assert.equal(declarationOwners.get("Event.once()"), "Event");
        assert.equal(declarationOwners.get("this._register()"), "Disposable");
      },
    );
  });

  void it("evaluates overloads, generic instantiation, narrowing, and assignability in a single snapshot", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-overloads" },
      async (host) => {
        await host.typecheck();

        const rule = defineRule({
          id: "overload-generic-assignability",
          run: async (ctx) => {
            const callSyntax = await ctx.findSyntax({
              range: {
                file: "src/overloads.ts",
                startLine: 16,
                endLine: 28,
              },
              kinds: ["CallExpression"],
            });
            assert.equal(callSyntax.length, 4);

            const resolvedCalls = await ctx.resolveCalls(
              callSyntax.map((c) => c.ref),
            );
            const callRefs = resolvedCalls
              .map((r) => r.call)
              .filter((c): c is CallRef => c !== null);
            assert.equal(callRefs.length, 4);

            // Single batch call projection evaluating signature, instantiated parameters, and return type.
            const projectedCalls = await ctx.projectCalls(callRefs, {
              select: {
                location: true,
                signature: true,
                instantiatedParameterTypes: [0],
                returnType: true,
              },
            });

            const [textCall, numCall, wrapStrCall, wrapNumCall] =
              projectedCalls;
            assert.ok(
              textCall && numCall && wrapStrCall && wrapNumCall,
              "expected 4 projected calls",
            );

            // Overload 1 vs Overload 2
            assert.equal(
              textCall.instantiatedParameterTypes[0]?.type.typeText,
              "string",
            );
            assert.equal(
              numCall.instantiatedParameterTypes[0]?.type.typeText,
              "number",
            );
            assert.match(
              textCall.returnType?.typeText ?? "",
              /kind:\s*['"]text['"]/u,
            );
            assert.match(
              numCall.returnType?.typeText ?? "",
              /kind:\s*['"]num['"]/u,
            );

            // Follow-up SignatureRef query confirming exact overload declaration line + instantiation
            assert.ok(textCall.signature);
            assert.ok(numCall.signature);
            assert.ok(wrapStrCall.signature);
            const sigRows = await ctx.projectSignatures(
              [textCall.signature, numCall.signature, wrapStrCall.signature],
              {
                select: {
                  location: true,
                  isInstantiated: true,
                  instantiatedParameterTypes: true,
                  returnType: true,
                  parameters: true,
                },
              },
            );
            assert.equal(sigRows[0]!.location?.line, 1);
            assert.equal(sigRows[1]!.location?.line, 2);
            assert.equal(sigRows[0]!.isInstantiated, false);
            assert.equal(sigRows[2]!.isInstantiated, true);
            assert.equal(sigRows[2]!.parameters?.length, 2);
            assert.equal(
              sigRows[2]!.instantiatedParameterTypes?.[0]?.type.typeText,
              "string",
            );
            assert.equal(
              sigRows[2]!.instantiatedParameterTypes?.[1]?.type.typeText,
              "string",
            );

            // Generic instantiation + narrowing on wrap(input, ...)
            assert.equal(
              wrapStrCall.instantiatedParameterTypes[0]?.type.typeText,
              "string",
            );
            assert.equal(
              wrapNumCall.instantiatedParameterTypes[0]?.type.typeText,
              "number",
            );

            // Inspect properties of the instantiated returnType via projectTypes
            assert.ok(wrapStrCall.returnType);
            const [retTypeRow] = await ctx.projectTypes(
              [wrapStrCall.returnType.ref],
              {
                select: {
                  typeText: true,
                  properties: true,
                },
              },
            );
            assert.deepEqual(
              retTypeRow?.properties?.map((p) => p.name),
              ["item", "tag"],
            );

            // Compare narrowed parameter types and generic return types with isAssignableTo
            const strParamType =
              wrapStrCall.instantiatedParameterTypes[0].type.ref;
            const numParamType =
              wrapNumCall.instantiatedParameterTypes[0].type.ref;
            const paramDecls = await ctx.findSyntax({
              range: {
                file: "src/overloads.ts",
                startLine: 16,
                endLine: 19,
              },
              kinds: ["Parameter"],
            });
            assert.equal(paramDecls.length, 2);
            const [inputParamType, widerParamType] = await ctx.typeAt(
              paramDecls.map((p) => p.ref),
            );
            assert.ok(inputParamType && widerParamType);

            const assignability = await ctx.isAssignableTo([
              { source: strParamType, target: inputParamType.ref }, // string -> string | number: true
              { source: inputParamType.ref, target: strParamType }, // string | number -> string: false
              { source: strParamType, target: numParamType }, // string -> number: false
              {
                source: wrapStrCall.returnType.ref,
                target: widerParamType.ref,
              }, // { item: string; tag: string } -> { item: string | number; tag: string }: true
              {
                source: widerParamType.ref,
                target: wrapStrCall.returnType.ref,
              }, // wider -> narrower: false
            ]);

            assert.deepEqual(
              assignability.map((a) => a.assignable),
              [true, false, false, true, false],
            );

            // Use ordinary TS on scalar projection values and report a diagnostic when overload 2 is called
            if (
              numCall.instantiatedParameterTypes[0]?.type.typeText === "number"
            ) {
              ctx.report({
                at: numCall.location,
                message: "Matched numeric overload",
                findingKey: "numeric-overload",
                basis: [
                  {
                    kind: "type",
                    message: `Selected overload at line ${String(sigRows[1]!.location?.line)}`,
                    at: sigRows[1]!.location ?? numCall.location,
                  },
                ],
              });
            }
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            rules: [rule],
          }),
        });
        assert.equal(report.status, "findings");
        assert.equal(report.diagnostics.length, 1);
        assert.equal(report.diagnostics[0]!.at.line, 21);
        assert.deepEqual(report.gaps, []);
      },
    );
  });

  void it("distinguishes unrequested fields, requested empty values, and indeterminate gaps", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-three-state" },
      async (host) => {
        await host.typecheck();

        // Inject deliberate missing-module import and unresolved call to exercise Gap reporting.
        host.replaceText(
          "src/three-state.ts",
          "type MissingType = { readonly missing: true };",
          'import type { MissingType } from "./does-not-exist.js";',
        );
        host.replaceText(
          "src/three-state.ts",
          "  dyn(123);\n  ping();",
          '  dyn(123);\n  // @ts-expect-error unresolved call\n  missingFn("arg");',
        );

        const rule = defineRule({
          id: "three-state-check",
          run: async (ctx) => {
            const callNodes = await ctx.findSyntax({
              range: {
                file: "src/three-state.ts",
                startLine: 6,
                endLine: 12,
              },
              kinds: ["CallExpression"],
            });
            assert.equal(callNodes.length, 3);

            // 1. Unrequested vs Requested-but-empty on ping()
            const [pingRow] = await ctx.resolveCalls([callNodes[0]!.ref], {
              select: {
                arguments: true,
                instantiatedParameterTypes: true,
                returnType: true,
              },
            });
            assert.ok(pingRow);
            assert.equal(Object.hasOwn(pingRow, "signature"), false);
            assert.equal(Object.hasOwn(pingRow, "callee"), false);
            assert.equal(Object.hasOwn(pingRow, "location"), false);
            assert.equal(Object.hasOwn(pingRow, "arguments"), true);
            assert.deepEqual(pingRow.arguments, []);
            assert.deepEqual(pingRow.instantiatedParameterTypes, []);
            assert.equal(pingRow.returnType?.typeText, "void");
            assert.deepEqual(pingRow.gaps, []);

            // Requested-but-null on anonymous arrow function name
            const [arrowDecl] = await ctx.findSyntax({
              range: {
                file: "src/three-state.ts",
                startLine: 3,
                endLine: 3,
              },
              kinds: ["ArrowFunction"],
            });
            assert.ok(arrowDecl);
            const [arrowLookup] = await ctx.syntaxOf(
              [(await ctx.resolveCalls([callNodes[0]!.ref]))[0]!.call!],
              { select: { body: true } },
            );
            // CallExpression has no body -> requested `body` is present and null with no gaps
            assert.ok(arrowLookup);
            assert.equal(Object.hasOwn(arrowLookup, "body"), true);
            assert.equal(arrowLookup.body, null);
            assert.deepEqual(arrowLookup.gaps, []);

            // 2. Indeterminate candidate on dyn(123): calleeDeclaration known, signature unknown -> Gap
            const [dynRow] = await ctx.resolveCalls([callNodes[1]!.ref], {
              select: {
                calleeDeclaration: true,
                signature: true,
                arguments: true,
                instantiatedParameterTypes: true,
                returnType: true,
              },
            });
            assert.ok(dynRow);
            assert.ok(dynRow.call);
            assert.ok(dynRow.calleeDeclaration);
            assert.equal(dynRow.arguments.length, 1);
            assert.equal(dynRow.signature, null);
            assert.equal(dynRow.returnType, null);
            assert.deepEqual(dynRow.instantiatedParameterTypes, []);
            assert.ok(
              dynRow.gaps.some((g) => g.kind === "unresolved-signature"),
              "expected unresolved-signature gap on dyn(123)",
            );

            // 3. Unresolved identifier call missingFn('arg'): both declaration and signature are gaps, arguments preserved
            const [missingRow] = await ctx.resolveCalls([callNodes[2]!.ref], {
              select: {
                calleeDeclaration: true,
                signature: true,
                arguments: true,
              },
            });
            assert.ok(missingRow);
            assert.equal(missingRow.arguments.length, 1);
            assert.equal(missingRow.calleeDeclaration, null);
            assert.equal(missingRow.signature, null);
            assert.ok(
              missingRow.gaps.some((g) => g.kind === "unresolved-declaration"),
            );
            assert.ok(
              missingRow.gaps.some((g) => g.kind === "unresolved-signature"),
            );

            // 4. Error/unresolved type on `broken` identifier
            const brokenVars = await ctx.findSyntax({
              range: {
                file: "src/three-state.ts",
                startLine: 5,
                endLine: 5,
              },
              kinds: ["VariableDeclaration"],
            });
            assert.equal(brokenVars.length, 1);
            const [brokenType] = await ctx.typeAt([brokenVars[0]!.ref]);
            assert.ok(brokenType);
            const [brokenProj] = await ctx.projectTypes([brokenType.ref], {
              select: { typeText: true },
            });
            assert.ok(
              brokenProj?.gaps.some((g) => g.kind === "unresolved-type"),
              `expected unresolved-type gap, got ${JSON.stringify(brokenProj?.gaps)}`,
            );
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
        assert.ok(report.gaps.some((g) => g.kind === "unresolved-signature"));
        assert.ok(report.gaps.some((g) => g.kind === "unresolved-declaration"));
        assert.ok(report.gaps.some((g) => g.kind === "unresolved-type"));
      },
    );
  });

  void it("rejects stale references and cross-project references with VeruleExecutionError", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-multi-project" },
      async (host) => {
        await host.typecheck("proj-a/tsconfig.json");
        await host.typecheck("proj-b/tsconfig.json");

        let capturedCall: CallRef | undefined;
        let capturedSig: SignatureRef | undefined;
        let capturedDecl: DeclarationRef | undefined;
        let capturedType: TypeRef | undefined;

        // Cross-project reference check within the same session
        const session = await openSession({
          cwd: host.rootDir,
          config: defineConfig({
            projects: ["proj-a/tsconfig.json", "proj-b/tsconfig.json"],
          }),
        });
        try {
          assert.equal(session.projects.length, 2);

          const crossRule = defineRule({
            id: "capture-then-cross",
            run: async (ctx) => {
              if (ctx.project.configFile.startsWith("proj-a")) {
                const calls = await ctx.findSyntax({
                  kinds: ["CallExpression"],
                });
                const [res] = await ctx.resolveCalls([calls[0]!.ref], {
                  select: {
                    calleeDeclaration: true,
                    signature: true,
                    returnType: true,
                  },
                });
                capturedCall = res!.call!;
                capturedSig = res!.signature!;
                capturedDecl = res!.calleeDeclaration!;
                capturedType = res!.returnType!.ref;
              } else {
                assert.ok(capturedCall);
                await ctx.projectCalls([capturedCall], {
                  select: { location: true },
                });
              }
            },
          });

          await assert.rejects(
            () => session.check([crossRule]),
            (err: unknown) =>
              err instanceof VeruleExecutionError &&
              err.code === "CROSS_PROJECT_REFERENCE",
          );

          // Also verify cross-project SignatureRef, DeclarationRef, TypeRef
          const crossSigRule = defineRule({
            id: "cross-sig",
            run: async (ctx) => {
              if (ctx.project.configFile.startsWith("proj-b")) {
                await ctx.projectSignatures([capturedSig!], {
                  select: { returnType: true },
                });
              }
            },
          });
          await assert.rejects(
            () => session.check([crossSigRule]),
            (err: unknown) =>
              err instanceof VeruleExecutionError &&
              err.code === "CROSS_PROJECT_REFERENCE",
          );

          const crossDeclRule = defineRule({
            id: "cross-decl",
            run: async (ctx) => {
              if (ctx.project.configFile.startsWith("proj-b")) {
                await ctx.syntaxOf([capturedDecl!], {
                  select: { kind: true },
                });
              }
            },
          });
          await assert.rejects(
            () => session.check([crossDeclRule]),
            (err: unknown) =>
              err instanceof VeruleExecutionError &&
              err.code === "CROSS_PROJECT_REFERENCE",
          );

          const crossTypeRule = defineRule({
            id: "cross-type",
            run: async (ctx) => {
              if (ctx.project.configFile.startsWith("proj-b")) {
                await ctx.isAssignableTo([
                  { source: capturedType!, target: capturedType! },
                ]);
              }
            },
          });
          await assert.rejects(
            () => session.check([crossTypeRule]),
            (err: unknown) =>
              err instanceof VeruleExecutionError &&
              err.code === "CROSS_PROJECT_REFERENCE",
          );
        } finally {
          await session.close();
        }

        // Stale reference check in a brand-new session after closing the previous session
        for (const [label, fn] of [
          [
            "stale CallRef",
            async (
              ctx: Parameters<Parameters<typeof defineRule>[0]["run"]>[0],
            ) => {
              await ctx.projectCalls([capturedCall!], {
                select: { location: true },
              });
            },
          ],
          [
            "stale SignatureRef",
            async (
              ctx: Parameters<Parameters<typeof defineRule>[0]["run"]>[0],
            ) => {
              await ctx.projectSignatures([capturedSig!], {
                select: { returnType: true },
              });
            },
          ],
          [
            "stale DeclarationRef",
            async (
              ctx: Parameters<Parameters<typeof defineRule>[0]["run"]>[0],
            ) => {
              await ctx.syntaxOf([capturedDecl!], {
                select: { location: true },
              });
            },
          ],
          [
            "stale TypeRef",
            async (
              ctx: Parameters<Parameters<typeof defineRule>[0]["run"]>[0],
            ) => {
              await ctx.projectTypes([capturedType!], {
                select: { typeText: true },
              });
            },
          ],
        ] as const) {
          await assert.rejects(
            () =>
              runCheck({
                cwd: host.rootDir,
                config: defineConfig({
                  projects: ["proj-a/tsconfig.json"],
                  rules: [defineRule({ id: `stale-${label}`, run: fn })],
                }),
              }),
            (err: unknown) =>
              err instanceof VeruleExecutionError &&
              err.code === "STALE_REFERENCE",
            `expected STALE_REFERENCE for ${label}`,
          );
        }
      },
    );
  });
});
