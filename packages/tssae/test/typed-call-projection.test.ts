import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@verule/test-hosts";
import {
  TssaeError,
  openProject,
  type CallRef,
  type DeclarationRef,
  type SignatureRef,
  type TypeRef,
} from "tssae";

void describe("tssae typed call projection", () => {
  void it("queries local syntax by source range, resolves calls & declarations across aliases, and maps back to syntax", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-aliases" },
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();

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
          assert.equal(callSyntax.kind, "CallExpression");
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
          assert.equal(defFuncs[0]!.parameters?.length, 2);
          assert.ok(defFuncs[0]!.body);

          const callResolutions = await ctx.resolveCalls([callSyntax.ref], {
            select: {
              location: true,
              calleeDeclaration: true,
            },
          });
          assert.equal(callResolutions.length, 1);
          const callRow = callResolutions[0]!;
          assert.equal(
            Object.hasOwn(callRow, "signature") ||
              Object.hasOwn(callRow, "returnType"),
            false,
          );
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
          assert.equal(declRow.name, "targetHelper");
          assert.equal(declRow.location?.file, "src/defs.ts");
          assert.equal(declRow.location?.line, 1);

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
          assert.equal(declSyntaxRow.kind, "FunctionDeclaration");
          assert.equal(declSyntaxRow.location?.file, "src/defs.ts");
          assert.equal(declSyntaxRow.location?.line, 1);
          assert.equal(declSyntaxRow.parameters?.length, 2);
          assert.ok(declSyntaxRow.body);

          const bodySyntax = await ctx.findSyntax({
            range: {
              file: "src/defs.ts",
              start: declSyntaxRow.location.start,
              end: declSyntaxRow.location.end,
            },
            kinds: ["Block"],
          });
          assert.equal(bodySyntax[0]?.kind, "Block");

          const callSyntaxLookup = reversed[1]!;
          assert.equal(callSyntaxLookup.location?.line, 5);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("filters call syntax by static callee names", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-callee-names" },
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();

          const callLines = (
            await ctx.findSyntax({
              kinds: ["CallExpression"],
              calleeNames: ["createMenu"],
            })
          ).map((call) => call.location.line);
          const textMatchedCallLines = (
            await ctx.findSyntax({
              kinds: ["CallExpression"],
              textIncludes: ["methodName"],
            })
          ).map((call) => call.location.line);
          const emptyCalleeMatches = (
            await ctx.findSyntax({
              kinds: ["CallExpression"],
              calleeNames: [],
            })
          ).length;

          assert.deepEqual(callLines, [4, 5, 6]);
          assert.deepEqual(textMatchedCallLines, [7]);
          assert.equal(emptyCalleeMatches, 0);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("projects the semantic owner of a resolved declaration", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-owner" },
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();

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
          const declarationOwners = new Map<string, string | null>();
          for (const [index, row] of rows.entries()) {
            declarationOwners.set(calls[index]!.text, row.containerName);
          }

          assert.equal(
            declarationOwners.get('menu.createMenu("main")'),
            "IMenuService",
          );
          assert.equal(declarationOwners.get("Event.once()"), "Event");
          assert.equal(declarationOwners.get("this._register()"), "Disposable");
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("evaluates cross-file overloads and generic instantiation in a single snapshot", async () => {
    await withTempHost(
      { host: "sdk-queries/cross-file-overloads" },
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();

          const callSyntax = await ctx.findSyntax({
            files: ["src/caller.ts"],
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

          const projectedCalls = await ctx.projectCalls(callRefs, {
            select: {
              location: true,
              signature: true,
              instantiatedParameterTypes: true,
              returnType: true,
            },
          });
          const [textCall, numCall, wrapStrCall, wrapNumCall] = projectedCalls;
          assert.ok(textCall && numCall && wrapStrCall && wrapNumCall);
          assert.ok(
            textCall.signature &&
              numCall.signature &&
              wrapStrCall.signature &&
              wrapNumCall.signature,
          );

          assert.equal(
            textCall.instantiatedParameterTypes[0]?.type.typeText,
            "string",
          );
          assert.equal(
            numCall.instantiatedParameterTypes[0]?.type.typeText,
            "number",
          );

          const sigRows = await ctx.projectSignatures(
            [
              textCall.signature,
              numCall.signature,
              wrapStrCall.signature,
              wrapNumCall.signature,
            ],
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
          assert.equal(sigRows[0]!.location?.file, "src/overloads.ts");
          assert.equal(sigRows[0]!.location?.line, 1);
          assert.equal(sigRows[1]!.location?.file, "src/overloads.ts");
          assert.equal(sigRows[1]!.location?.line, 2);
          assert.equal(sigRows[2]!.location?.file, "src/overloads.ts");
          assert.equal(sigRows[2]!.location?.line, 12);
          assert.equal(sigRows[2]!.isInstantiated, true);
          assert.equal(
            sigRows[2]!.instantiatedParameterTypes[0]?.type.typeText,
            "string",
          );
          assert.equal(
            sigRows[3]!.instantiatedParameterTypes[0]?.type.typeText,
            "number",
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("evaluates overloads, generic instantiation, narrowing, and assignability in a single snapshot", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-overloads" },
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();

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

          const projectedCalls = await ctx.projectCalls(callRefs, {
            select: {
              location: true,
              signature: true,
              instantiatedParameterTypes: [0],
              returnType: true,
            },
          });

          const [textCall, numCall, wrapStrCall, wrapNumCall] = projectedCalls;
          assert.ok(
            textCall && numCall && wrapStrCall && wrapNumCall,
            "expected 4 projected calls",
          );

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

          assert.equal(
            wrapStrCall.instantiatedParameterTypes[0]?.type.typeText,
            "string",
          );
          assert.equal(
            wrapNumCall.instantiatedParameterTypes[0]?.type.typeText,
            "number",
          );

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
            { source: strParamType, target: inputParamType.ref },
            { source: inputParamType.ref, target: strParamType },
            { source: strParamType, target: numParamType },
            {
              source: wrapStrCall.returnType.ref,
              target: widerParamType.ref,
            },
            {
              source: widerParamType.ref,
              target: wrapStrCall.returnType.ref,
            },
          ]);

          assert.deepEqual(
            assignability.map((a) => a.assignable),
            [true, false, false, true, false],
          );
          assert.equal(numCall.location.line, 21);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("distinguishes unrequested fields, requested empty values, and indeterminate gaps", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-three-state" },
      async (host) => {
        await host.typecheck();

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

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();

          const callNodes = await ctx.findSyntax({
            range: {
              file: "src/three-state.ts",
              startLine: 6,
              endLine: 12,
            },
            kinds: ["CallExpression"],
          });
          assert.equal(callNodes.length, 3);

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
          assert.ok(arrowLookup);
          assert.equal(Object.hasOwn(arrowLookup, "body"), true);
          assert.equal(arrowLookup.body, null);
          assert.deepEqual(arrowLookup.gaps, []);

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
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("rejects stale references and cross-project references with TssaeError", async () => {
    await withTempHost(
      { host: "sdk-queries/typed-projection-multi-project" },
      async (host) => {
        await host.typecheck("proj-a/tsconfig.json");
        await host.typecheck("proj-b/tsconfig.json");

        let capturedCall: CallRef | undefined;
        let capturedSig: SignatureRef | undefined;
        let capturedDecl: DeclarationRef | undefined;
        let capturedType: TypeRef | undefined;

        const session = await openProject({
          cwd: host.rootDir,
          projects: ["proj-a/tsconfig.json", "proj-b/tsconfig.json"],
        });
        try {
          assert.equal(session.projects.length, 2);
          const projA = session.project("proj-a/tsconfig.json");
          const projB = session.project("proj-b/tsconfig.json");

          const calls = await projA.findSyntax({ kinds: ["CallExpression"] });
          const [res] = await projA.resolveCalls([calls[0]!.ref], {
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

          await assert.rejects(
            () =>
              projB.projectCalls([capturedCall!], {
                select: { location: true },
              }),
            (err: unknown) =>
              err instanceof TssaeError &&
              err.code === "CROSS_PROJECT_REFERENCE",
          );
          await assert.rejects(
            () =>
              projB.projectSignatures([capturedSig!], {
                select: { returnType: true },
              }),
            (err: unknown) =>
              err instanceof TssaeError &&
              err.code === "CROSS_PROJECT_REFERENCE",
          );
          await assert.rejects(
            () => projB.syntaxOf([capturedDecl!], { select: { kind: true } }),
            (err: unknown) =>
              err instanceof TssaeError &&
              err.code === "CROSS_PROJECT_REFERENCE",
          );
          await assert.rejects(
            () =>
              projB.isAssignableTo([
                { source: capturedType!, target: capturedType! },
              ]),
            (err: unknown) =>
              err instanceof TssaeError &&
              err.code === "CROSS_PROJECT_REFERENCE",
          );
        } finally {
          await session.close();
        }

        const freshSession = await openProject({
          cwd: host.rootDir,
          projects: ["proj-a/tsconfig.json"],
        });
        try {
          const freshProj = freshSession.project();

          await assert.rejects(
            () =>
              freshProj.projectCalls([capturedCall], {
                select: { location: true },
              }),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );
          await assert.rejects(
            () =>
              freshProj.projectSignatures([capturedSig], {
                select: { returnType: true },
              }),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );
          await assert.rejects(
            () =>
              freshProj.syntaxOf([capturedDecl], {
                select: { location: true },
              }),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );
          await assert.rejects(
            () =>
              freshProj.projectTypes([capturedType], {
                select: { typeText: true },
              }),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_REFERENCE",
          );
        } finally {
          await freshSession.close();
        }
      },
    );
  });
});
