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
});
