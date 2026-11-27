import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import { openProject } from "tssae";

void describe("tssae conformance edge cases", () => {
  void it("preserves NDJSON responses containing U+2028/U+2029 and handles phase import meta-property calls without panicking", async () => {
    const dir = mkdtempSync(join(tmpdir(), "tssae-conformance-repro-"));
    try {
      writeFileSync(
        join(dir, "tsconfig.json"),
        JSON.stringify({
          compilerOptions: {
            allowJs: true,
            checkJs: false,
            noEmit: true,
            noResolve: true,
            types: [],
            target: "esnext",
            module: "preserve",
          },
          files: ["line-sep.mjs", "phase-import.mjs"],
        }),
        "utf8",
      );
      writeFileSync(
        join(dir, "line-sep.mjs"),
        `export const text = "a\u2028b\u2029c";\n`,
        "utf8",
      );
      writeFileSync(
        join(dir, "phase-import.mjs"),
        `export function run() {\n  const a = import.defer("./dep.mjs");\n  const b = import.source("./dep.mjs");\n  return [a, b];\n}\n`,
        "utf8",
      );

      const session = await openProject({
        cwd: dir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project();
        const lineSepNodes = await ctx.findSyntax({
          range: { file: "line-sep.mjs" },
        });
        assert.ok(lineSepNodes.length > 0);

        const calls = await ctx.findSyntax({
          files: ["phase-import.mjs"],
          kinds: ["CallExpression"],
        });
        assert.equal(calls.length, 2);

        const metaProps = (
          await ctx.findSyntax({ range: { file: "phase-import.mjs" } })
        ).filter((node) => node.kind === "MetaProperty");
        assert.equal(metaProps.length, 2);

        const types = await ctx.typeAt(metaProps.map((node) => node.ref));
        assert.equal(types.length, 2);

        const resolvedCalls = await ctx.resolveCalls(
          calls.map((node) => node.ref),
        );
        const validCalls = resolvedCalls
          .map((row) => row.call)
          .filter((ref): ref is NonNullable<typeof ref> => ref !== null);
        assert.equal(validCalls.length, 2);

        const callees = await ctx.calleesOf(validCalls);
        assert.equal(callees.length, 2);
      } finally {
        await session.close();
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  void it("separates class statement IR cache from synthesized default constructor IR and exposes instance field initializer branches", async () => {
    const dir = mkdtempSync(join(tmpdir(), "tssae-class-default-ctor-cache-"));
    try {
      writeFileSync(
        join(dir, "tsconfig.json"),
        JSON.stringify({
          compilerOptions: {
            strict: true,
            noEmit: true,
            target: "es2022",
            module: "nodenext",
            moduleResolution: "nodenext",
          },
          files: ["app.ts"],
        }),
        "utf8",
      );
      writeFileSync(
        join(dir, "app.ts"),
        [
          "class Resource {}",
          "function sourceA(): string { return 'alpha'; }",
          "function sourceB(): string { return 'beta'; }",
          "function sink(_v: string): void {}",
          "declare const cond: boolean;",
          "declare const flag: boolean;",
          "",
          "class Box {",
          "  static header = flag ? 1 : 2;",
          "  token = cond ? sourceA() : sourceB();",
          "  alloc = new Resource();",
          "}",
          "",
          "const ExprBox = class {",
          "  item = flag ? sourceA() : sourceB();",
          "};",
          "",
          "class ReverseOrderBox {",
          "  val = cond ? sourceA() : sourceB();",
          "  static s = flag ? 10 : 20;",
          "}",
          "",
          "export function exercise(): void {",
          "  const box = new Box();",
          "  sink(box.token);",
          "  const heldAlloc = box.alloc;",
          "  void heldAlloc;",
          "  const exprBox = new ExprBox();",
          "  sink(exprBox.item);",
          "  const rev = new ReverseOrderBox();",
          "  sink(rev.val);",
          "}",
          "",
        ].join("\n"),
        "utf8",
      );

      const session = await openProject({
        cwd: dir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project();
        const nodes = await ctx.findSyntax({ range: { file: "app.ts" } });

        const boxClass = nodes.find(
          (n) =>
            n.kind === "ClassDeclaration" && n.text.startsWith("class Box"),
        );
        const revClass = nodes.find(
          (n) =>
            n.kind === "ClassDeclaration" &&
            n.text.startsWith("class ReverseOrderBox"),
        );
        const tokenProp = nodes.find(
          (n) =>
            n.kind === "PropertyDeclaration" && n.text.startsWith("token ="),
        );
        const allocProp = nodes.find(
          (n) =>
            n.kind === "PropertyDeclaration" && n.text.startsWith("alloc ="),
        );
        const exprItemProp = nodes.find(
          (n) =>
            n.kind === "PropertyDeclaration" && n.text.startsWith("item ="),
        );
        assert.ok(
          boxClass && revClass && tokenProp && allocProp && exprItemProp,
        );

        // 1. Call listBranches on ClassDeclaration FIRST in this snapshot (statement-level class header).
        const [boxStmtBranches] = await ctx.listBranches([boxClass.ref]);
        assert.ok(boxStmtBranches);
        assert.deepEqual(boxStmtBranches.gaps, []);
        assert.equal(boxStmtBranches.branches.length, 2);
        assert.ok(
          boxStmtBranches.branches.every((b) => b.conditionText === "flag"),
        );

        const sourceACallInBox = nodes.find(
          (n) =>
            n.kind === "CallExpression" &&
            n.text === "sourceA()" &&
            n.location.start >= tokenProp.location.start &&
            n.location.end <= tokenProp.location.end,
        );
        assert.ok(sourceACallInBox);

        // 2. Immediately in the SAME snapshot (before listBranches on PropertyDeclaration),
        //    verify semantic queries resolve Box's synthesized default constructor without cache collision.
        const alphaLiteral = nodes.find(
          (n) => n.kind === "StringLiteral" && n.text === "'alpha'",
        );
        const sinkCalls = nodes.filter(
          (n) => n.kind === "CallExpression" && n.text.startsWith("sink("),
        );
        assert.ok(alphaLiteral);
        assert.equal(sinkCalls.length, 3);
        const sinkArgs = sinkCalls.map((c) => c.arguments![0]!);

        const vp = await ctx.valuePropagation({
          propagation: "value",
          from: [alphaLiteral.ref, sourceACallInBox.ref],
          to: sinkArgs,
        });
        assert.equal(vp.results[0]?.status, "proved");
        assert.equal(vp.results[1]?.status, "proved");
        assert.equal(vp.results[2]?.status, "proved");
        assert.equal(vp.results[3]?.status, "proved");

        // Reverse order: listBranches([revClass.ref]) AFTER default constructor IR was cached must still see static `flag`.
        const [revStmtBranches] = await ctx.listBranches([revClass.ref]);
        assert.ok(revStmtBranches);
        assert.equal(revStmtBranches.branches.length, 2);
        assert.ok(
          revStmtBranches.branches.every((b) => b.conditionText === "flag"),
        );

        // resolveInstances: new Resource() inside Box.alloc and heldAlloc in exercise() resolve to same instance.
        const heldAllocDecl = nodes.find(
          (n) =>
            n.kind === "VariableDeclaration" &&
            n.text.startsWith("heldAlloc ="),
        );
        assert.ok(heldAllocDecl?.initializer && allocProp.initializer);
        const [allocInst, readInst] = await ctx.resolveInstances([
          allocProp.initializer,
          heldAllocDecl.initializer,
        ]);
        assert.equal(allocInst?.status, "proved");
        assert.equal(readInst?.status, "proved");
        if (allocInst?.status === "proved" && readInst?.status === "proved") {
          assert.ok(allocInst.instance && readInst.instance);
          const [allocProj, readProj] = await ctx.projectInstances(
            [allocInst.instance, readInst.instance],
            { select: { allocationLocation: true } },
          );
          assert.deepEqual(
            readProj?.allocationLocation,
            allocProj?.allocationLocation,
          );
        }

        // calleesOf & callEffects on `new Box()` and `sourceA()` inside Box.token initializer.
        const newBoxNode = nodes.find(
          (n) => n.kind === "NewExpression" && n.text === "new Box()",
        );
        assert.ok(newBoxNode);
        const resolvedCalls = await ctx.resolveCalls([
          sourceACallInBox.ref,
          newBoxNode.ref,
        ]);
        const sourceACallRef = resolvedCalls[0]?.call;
        const newBoxCallRef = resolvedCalls[1]?.call;
        assert.ok(sourceACallRef && newBoxCallRef);

        const [sourceACallees] = await ctx.calleesOf([sourceACallRef]);
        assert.equal(sourceACallees?.completeness, "complete");
        assert.equal(sourceACallees?.resolvedTargets.length, 1);

        const [newBoxEffects] = await ctx.callEffects({
          targets: [newBoxCallRef],
          includeWrites: true,
        });
        assert.equal(newBoxEffects?.completeness, "complete");
        const writtenProps = newBoxEffects?.fieldWrites
          .map((w) => w.propertyName)
          .sort();
        assert.deepEqual(writtenProps, ["alloc", "token"]);

        // 3. Observe branches inside constructorless ClassDeclaration & ClassExpression instance field initializers.
        const [tokenPropBranches, exprPropBranches] = await ctx.listBranches([
          tokenProp.ref,
          exprItemProp.ref,
        ]);
        assert.ok(tokenPropBranches && exprPropBranches);
        assert.deepEqual(tokenPropBranches.gaps, []);
        assert.equal(tokenPropBranches.branches.length, 2);
        assert.ok(
          tokenPropBranches.branches.every((b) => b.conditionText === "cond"),
        );
        assert.deepEqual(exprPropBranches.gaps, []);
        assert.equal(exprPropBranches.branches.length, 2);
        assert.ok(
          exprPropBranches.branches.every((b) => b.conditionText === "flag"),
        );

        const condTrueBranch = tokenPropBranches.branches.find(
          (b) => b.direction === "true",
        )!;
        const condFalseBranch = tokenPropBranches.branches.find(
          (b) => b.direction === "false",
        )!;

        const [covTrue] = await ctx.branchCoverage({
          scope: tokenProp.ref,
          branches: [condTrueBranch.ref],
          targets: [sourceACallInBox.ref],
        });
        const [covFalse] = await ctx.branchCoverage({
          scope: tokenProp.ref,
          branches: [condFalseBranch.ref],
          targets: [sourceACallInBox.ref],
        });
        assert.equal(covTrue?.status, "proved");
        assert.equal(covFalse?.status, "disproved");
      } finally {
        await session.close();
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
