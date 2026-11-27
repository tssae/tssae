import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";
import { pathToFileURL } from "node:url";

import { collectPreciseCoverage } from "@tssae/acceptance";

function utf8ByteSpan(
  source: string,
  needle: string,
  fromIndex = 0,
): {
  readonly start: number;
  readonly end: number;
  readonly nextIndex: number;
} {
  const idx = source.indexOf(needle, fromIndex);
  assert.ok(idx >= 0, `Missing needle: ${needle}`);
  const start = Buffer.byteLength(source.slice(0, idx), "utf8");
  const end = start + Buffer.byteLength(needle, "utf8");
  return { start, end, nextIndex: idx + needle.length };
}

void describe("V8 precise byte coverage with UTF-8 to UTF-16 offset mapping", () => {
  void it("tracks executed vs skipped branches and optional-chain short-circuits across non-ASCII TypeScript", async () => {
    const dir = mkdtempSync(join(tmpdir(), "tssae-v8-cov-"));
    try {
      const filePath = join(dir, "sample.ts");
      const source = [
        'type EmojiTag = "🚀🎉"; // 中文注释 🚀',
        "function hitGuard(): number { return 1; }",
        "function targetCall(): number { return 2; }",
        "export function run(flag: boolean, obj?: { guard(): number }): EmojiTag {",
        "  if (flag) { hitGuard(); }",
        "  const shortAnd = flag && hitGuard();",
        "  const shortOpt = obj?.guard();",
        "  void shortAnd;",
        "  void shortOpt;",
        "  targetCall();",
        '  return "🚀🎉";',
        "}",
        "",
      ].join("\n");
      writeFileSync(filePath, source, "utf8");

      const coverage = await collectPreciseCoverage([filePath], async () => {
        const mod = (await import(pathToFileURL(filePath).href)) as {
          run: (flag: boolean, obj?: { guard(): number }) => string;
        };
        mod.run(false, undefined);
      });

      const ifGuard = utf8ByteSpan(source, "hitGuard();");
      const andGuard = utf8ByteSpan(
        source,
        "flag && hitGuard()",
        ifGuard.nextIndex,
      );
      const optGuard = utf8ByteSpan(source, "obj?.guard()");
      const target = utf8ByteSpan(source, "targetCall();");

      assert.equal(
        coverage.getExecutionCount(filePath, ifGuard.start, ifGuard.end),
        0,
      );
      assert.equal(
        coverage.getExecutionCount(filePath, andGuard.start, andGuard.end, {
          shortCircuitSubrangeZeroAsSkipped: true,
        }),
        0,
      );
      assert.equal(
        coverage.getExecutionCount(filePath, optGuard.start, optGuard.end, {
          shortCircuitSubrangeZeroAsSkipped: true,
        }),
        0,
      );
      assert.equal(
        coverage.getExecutionCount(filePath, target.start, target.end),
        1,
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
