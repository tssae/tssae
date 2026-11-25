import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";

import { withTempHost } from "@verule/test-hosts";
import {
  TssaeError,
  openProject,
  type DiagnosticLocation,
  type ScannedComment,
} from "tssae";

function writeFiles(
  rootDir: string,
  files: Readonly<Record<string, string>>,
): void {
  for (const [relPath, content] of Object.entries(files)) {
    const absPath = join(rootDir, relPath);
    mkdirSync(dirname(absPath), { recursive: true });
    writeFileSync(absPath, content, "utf8");
  }
}

type SimulatedDirective =
  | {
      readonly kind: "valid";
      readonly ruleId: string;
      readonly reason: string;
      readonly at: DiagnosticLocation;
      readonly targetLine: number | null;
    }
  | {
      readonly kind: "malformed";
      readonly message: string;
      readonly at: DiagnosticLocation;
    };

function parseConsumerDirectives(
  comments: readonly ScannedComment[],
): readonly SimulatedDirective[] {
  const prefix = "verule-ignore-next-code-line";
  const out: SimulatedDirective[] = [];
  for (const comment of comments) {
    const trimmed = comment.text.trim();
    if (!trimmed.startsWith(prefix)) continue;
    const afterPrefix = trimmed.slice(prefix.length);
    if (afterPrefix !== "" && !/^\s/u.test(afterPrefix)) continue;

    const rest = afterPrefix.trim();
    const sepMatch = /^(.*?)\s+--\s+([\s\S]+)$/u.exec(rest);
    if (!sepMatch) {
      out.push({
        kind: "malformed",
        message: "Expected <ruleId> -- <reason>",
        at: comment.at,
      });
      continue;
    }
    const ruleId = sepMatch[1]!.trim();
    const reason = sepMatch[2]!.trim();
    if (ruleId === "" || /\s|,/u.test(ruleId) || reason === "") {
      out.push({
        kind: "malformed",
        message: "Invalid ruleId or empty reason",
        at: comment.at,
      });
      continue;
    }
    out.push({
      kind: "valid",
      ruleId,
      reason,
      at: comment.at,
      targetLine: comment.nextCodeLine,
    });
  }
  return out;
}

void describe("tssae comment and nextCodeLine scanning", () => {
  void it("scans standalone line/block/JSX comments, bridges stacked comments and JSDoc, truncates on blank lines, and excludes literals", async () => {
    await withTempHost({}, async (host) => {
      writeFiles(host.rootDir, {
        "tsconfig.json": JSON.stringify({
          compilerOptions: {
            strict: true,
            target: "ES2022",
            module: "NodeNext",
            moduleResolution: "NodeNext",
            jsx: "preserve",
            noEmit: true,
          },
          include: ["src/**/*.ts", "src/**/*.tsx"],
        }),
        "src/jsx-ambient.d.ts": [
          "declare namespace JSX {",
          "  interface IntrinsicElements {",
          "    div: { children?: unknown };",
          "  }",
          "}",
          "",
        ].join("\n"),
        "src/cases.ts": [
          "declare function sink(v: string): void;",
          "declare function register(name: string): string;",
          "/** JSDoc comment must be excluded: verule-ignore-next-code-line r1 -- doc */",
          "// stacked line comment",
          "/* stacked multiline",
          " * block comment",
          " */",
          "/** Intermediate JSDoc between standalone comments and target */",
          'export function targetFn(): void { sink("a"); } // trailing inline comment excluded',
          "",
          "// blank-line truncated comment",
          "",
          'sink("b");',
          'const str = "// not a comment";',
          "const tpl = `/* not a comment ${str} */`;",
          "const re = /\\/\\/ not a comment/u;",
          "/* inline block */ void re;",
          "// folded formatting comment",
          "(",
          "  register('folded')",
          ");",
          "// merged formatting comment",
          'const merged = [register("old"), register("new")];',
          "void merged;",
          "// trailing eof comment",
        ].join("\n"),
        "src/view.tsx": [
          "declare function Item(): unknown;",
          "export function View() {",
          "  return (",
          "    <div>",
          "      // literal text inside JsxText, not a comment",
          "      {/* jsx standalone comment */}",
          "      <Item />",
          "    </div>",
          "  );",
          "}",
          "",
        ].join("\n"),
      });

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const allComments = await session.scanComments();
        assert.deepEqual(
          allComments.map((c) => ({
            file: c.at.file,
            kind: c.kind,
            text: c.text,
            line: c.at.line,
            endLine: c.at.endLine,
            nextCodeLine: c.nextCodeLine,
            isJsx: c.isJsx,
          })),
          [
            {
              file: "src/cases.ts",
              kind: "line",
              text: "stacked line comment",
              line: 4,
              endLine: 4,
              nextCodeLine: 9,
              isJsx: false,
            },
            {
              file: "src/cases.ts",
              kind: "block",
              text: "stacked multiline block comment",
              line: 5,
              endLine: 7,
              nextCodeLine: 9,
              isJsx: false,
            },
            {
              file: "src/cases.ts",
              kind: "line",
              text: "blank-line truncated comment",
              line: 11,
              endLine: 11,
              nextCodeLine: null,
              isJsx: false,
            },
            {
              file: "src/cases.ts",
              kind: "line",
              text: "folded formatting comment",
              line: 18,
              endLine: 18,
              nextCodeLine: 19,
              isJsx: false,
            },
            {
              file: "src/cases.ts",
              kind: "line",
              text: "merged formatting comment",
              line: 22,
              endLine: 22,
              nextCodeLine: 23,
              isJsx: false,
            },
            {
              file: "src/cases.ts",
              kind: "line",
              text: "trailing eof comment",
              line: 25,
              endLine: 25,
              nextCodeLine: null,
              isJsx: false,
            },
            {
              file: "src/view.tsx",
              kind: "block",
              text: "jsx standalone comment",
              line: 6,
              endLine: 6,
              nextCodeLine: 7,
              isJsx: true,
            },
          ],
        );

        const jsxOnly = await session.scanComments({ files: ["src/view.tsx"] });
        assert.equal(jsxOnly.length, 1);
        assert.equal(jsxOnly[0]?.at.file, "src/view.tsx");
        assert.equal(jsxOnly[0]?.at.column, 7);
        assert.equal(jsxOnly[0]?.at.endColumn, 37);

        const emptyFilter = await session.scanComments({ files: [] });
        assert.deepEqual(emptyFilter, []);
      } finally {
        await session.close();
      }
    });
  });

  void it("deduplicates shared files across multiple projects and lets consumers resolve ADR 0007 directives", async () => {
    await withTempHost({}, async (host) => {
      writeFiles(host.rootDir, {
        "packages/a/tsconfig.json": JSON.stringify({
          compilerOptions: { strict: true, noEmit: true },
          include: ["../shared/shared.ts"],
        }),
        "packages/b/tsconfig.json": JSON.stringify({
          compilerOptions: { strict: true, noEmit: true },
          include: ["../shared/shared.ts"],
        }),
        "packages/shared/shared.ts": [
          "declare function register(name: string): string;",
          "/** verule-ignore-next-code-line r/doc -- JSDoc must be ignored */",
          "// verule-ignore-next-code-line r/one -- Allow first",
          "/* verule-ignore-next-code-line r/two --",
          " * Allow second",
          " */",
          'const items = [register("old"), register("new")];',
          "// verule-ignore-next-code-line r/unused -- Separated by blank line",
          "",
          "void items;",
          "// verule-ignore-next-code-line r/a r/b -- malformed multi id",
        ].join("\n"),
      });

      const session = await openProject({
        cwd: host.rootDir,
        projects: ["packages/a/tsconfig.json", "packages/b/tsconfig.json"],
      });
      try {
        const comments = await session.scanComments();
        assert.equal(comments.length, 4);
        assert.ok(
          comments.every((c) => c.at.file === "packages/shared/shared.ts"),
        );

        const directives = parseConsumerDirectives(comments);
        assert.equal(directives.length, 4);
        assert.equal(directives[0]?.kind, "valid");
        if (directives[0]?.kind === "valid") {
          assert.equal(directives[0].ruleId, "r/one");
          assert.equal(directives[0].targetLine, 7);
        }
        assert.equal(directives[1]?.kind, "valid");
        if (directives[1]?.kind === "valid") {
          assert.equal(directives[1].ruleId, "r/two");
          assert.equal(directives[1].reason, "Allow second");
          assert.equal(directives[1].targetLine, 7);
        }
        assert.equal(directives[2]?.kind, "valid");
        if (directives[2]?.kind === "valid") {
          assert.equal(directives[2].ruleId, "r/unused");
          assert.equal(directives[2].targetLine, null);
        }
        assert.equal(directives[3]?.kind, "malformed");

        // Verify updated comments reflect file edits after session.update()
        writeFiles(host.rootDir, {
          "packages/shared/shared.ts": [
            "// updated comment",
            "export const x = 1;",
          ].join("\n"),
        });
        await session.update({ changed: ["packages/shared/shared.ts"] });
        const updatedComments = await session.scanComments();
        assert.equal(updatedComments.length, 1);
        assert.equal(updatedComments[0]?.text, "updated comment");
        assert.equal(updatedComments[0]?.nextCodeLine, 2);
      } finally {
        await session.close();
      }

      await assert.rejects(
        () => session.scanComments(),
        (err: unknown) =>
          err instanceof TssaeError && err.code === "SESSION_CLOSED",
      );
    });
  });
});
