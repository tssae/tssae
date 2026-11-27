import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@tssae/test-hosts";
import { defineSummaries, openProject, type SyntaxRef } from "tssae";

const BASE_TSCONFIG = JSON.stringify(
  {
    compilerOptions: {
      strict: true,
      target: "ES2022",
      module: "NodeNext",
      moduleResolution: "NodeNext",
      noEmit: true,
    },
    include: ["src/**/*.ts"],
  },
  null,
  2,
);

void describe("tssae IR value propagation syntax, summaries, and tri-state isolation", () => {
  void it("propagates through array/object destructuring, spread, getters, for-of, closures, and logical conditionals", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "ir-vp-syntax",
            private: true,
            type: "module",
          }),
          "tsconfig.json": BASE_TSCONFIG,
          "src/app.ts": [
            "function sink(_tag: string, _v: unknown): void {}",
            "function origin(): string { return 'secret'; }",
            "",
            "class Vault {",
            "  private hidden: string;",
            "  constructor(initial: string) {",
            "    this.hidden = initial;",
            "  }",
            "  get value(): string {",
            "    return this.hidden;",
            "  }",
            "}",
            "",
            "function spreadReceiver(_ignored: string, forwarded: string, ...restArgs: string[]): void {",
            "  sink('spreadArg', forwarded);",
            "  sink('spreadRest', restArgs[0]);",
            "}",
            "",
            "export async function runSyntaxFlows(cond: boolean): Promise<void> {",
            "  const raw = origin();",
            "  const [firstSafe, secondSecret = 'fallback', ...tailArr] = ['safe', raw, 'tailItem'];",
            "  sink('arrFirst', firstSafe);",
            "  sink('arrSecond', secondSecret);",
            "  sink('arrTail', tailArr);",
            "",
            "  const objLit = { secret: raw, safe: 'clean' };",
            "  const spreadObj = { ...objLit, overrideKey: 'safe' };",
            "  const { safe: cleanField, ...restObj } = spreadObj;",
            "  sink('objClean', cleanField);",
            "  sink('objRestSecret', restObj.secret);",
            "",
            "  const spreadArr = ['safe', raw, raw] as const;",
            "  spreadReceiver(...spreadArr);",
            "",
            "  const objWithGetter = {",
            "    inner: raw,",
            "    get readInner(): string { return this.inner; },",
            "  };",
            "  sink('objGetter', objWithGetter.readInner);",
            "",
            "  const vault = new Vault(raw);",
            "  sink('classGetter', vault.value);",
            "",
            "  for (const elem of [raw]) {",
            "    sink('forOfElem', elem);",
            "  }",
            "",
            "  for await (const asyncElem of [Promise.resolve(raw)]) {",
            "    sink('forAwaitElem', asyncElem);",
            "  }",
            "",
            "  const readCaptured = () => raw;",
            "  sink('closureRead', readCaptured());",
            "",
            "  let mutatedByClosure = 'clean';",
            "  function writeClosure(): void {",
            "    mutatedByClosure = raw;",
            "  }",
            "  writeClosure();",
            "  sink('closureWrite', mutatedByClosure);",
            "",
            "  let killedBeforeClosure = raw;",
            "  killedBeforeClosure = 'sanitized';",
            "  const readAfterKill = () => killedBeforeClosure;",
            "  sink('closureKill', readAfterKill());",
            "",
            "  const condVal = cond ? raw : 'safe';",
            "  sink('condExpr', condVal);",
            "  let fallbackVal: string | undefined = undefined;",
            "  fallbackVal ||= raw;",
            "  sink('logicalOrAssign', fallbackVal);",
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
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const originCall = calls.find((c) => c.text === "origin()");
          assert.ok(originCall);

          const sinkCalls = calls.filter((c) => c.text.startsWith("sink("));
          const resolvedSinks = await ctx.resolveCalls(
            sinkCalls.map((c) => c.ref),
            { select: { arguments: true } },
          );
          const tagRefs = resolvedSinks.map((r) => r.arguments![0]!);
          const tagTexts = await ctx.syntaxOf(tagRefs, {
            select: { text: true },
          });

          const targets: { readonly tag: string; readonly ref: SyntaxRef }[] =
            [];
          for (const [idx, row] of resolvedSinks.entries()) {
            const tag = (tagTexts[idx]?.text ?? "").replaceAll("'", "");
            const argRef = row.arguments?.[1];
            assert.ok(argRef, `Missing sink arg for ${tag}`);
            targets.push({ tag, ref: argRef });
          }

          const prop = await ctx.valuePropagation({
            propagation: "value",
            includeMemberAccess: true,
            from: originCall.ref,
            to: targets.map((t) => t.ref),
          });

          const byTag = new Map(
            targets.map((t, i) => [t.tag, prop.results[i]!]),
          );

          for (const expectedProved of [
            "arrSecond",
            "objRestSecret",
            "spreadArg",
            "spreadRest",
            "objGetter",
            "classGetter",
            "forOfElem",
            "forAwaitElem",
            "closureRead",
            "closureWrite",
            "condExpr",
            "logicalOrAssign",
          ]) {
            const row = byTag.get(expectedProved);
            assert.equal(
              row?.status,
              "proved",
              `Expected '${expectedProved}' to be proved, got ${JSON.stringify(row)}`,
            );
          }

          for (const expectedDisproved of [
            "arrFirst",
            "arrTail",
            "objClean",
            "closureKill",
          ]) {
            const row = byTag.get(expectedDisproved);
            assert.equal(
              row?.status,
              "disproved",
              `Expected '${expectedDisproved}' to be disproved, got ${JSON.stringify(row)}`,
            );
          }
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("uses builtin stdlib and local summaries, reports critical gaps for unsummarized externals, and isolates unrelated unknowns", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "ir-vp-summaries",
            private: true,
            type: "module",
          }),
          "node_modules/ext-lib/package.json": JSON.stringify({
            name: "ext-lib",
            version: "1.0.0",
            types: "./index.d.ts",
          }),
          "node_modules/ext-lib/index.d.ts": [
            "export declare function unknownExternal(input: string): string;",
            "export declare function customSummarized(input: string): string;",
            "export declare function partialSummarized(flag: boolean, pass: string, drop: string): string;",
            "",
          ].join("\n"),
          "tsconfig.json": BASE_TSCONFIG,
          "src/app.ts": [
            "import { unknownExternal, customSummarized, partialSummarized } from 'ext-lib';",
            "",
            "function sink(_tag: string, _v: unknown): void {}",
            "function origin(): string { return 'raw-secret'; }",
            "",
            "export async function runSummaryAndGapChecks(cond: boolean): Promise<void> {",
            "  const src = origin();",
            "",
            "  // Unrelated unknown external call on an independent value",
            "  const unrelatedOut = unknownExternal('benign');",
            "  sink('unrelatedOut', unrelatedOut);",
            "",
            "  // Proved direct path (must remain proved with no gaps despite unrelated unknownExternal)",
            "  const directCopy = src;",
            "  sink('directCopy', directCopy);",
            "",
            "  // Disproved clean literal (must remain disproved with no gaps despite unrelated unknownExternal)",
            "  const cleanVal = 'safe';",
            "  sink('cleanVal', cleanVal);",
            "",
            "  // Builtin stdlib summary: Array.prototype.map",
            "  const [mappedOut] = [src].map((x) => x);",
            "  sink('mappedOut', mappedOut);",
            "",
            "  // Builtin stdlib summary: Promise.resolve().then()",
            "  const resolvedOut = await Promise.resolve(src).then((x) => x);",
            "  sink('promiseOut', resolvedOut);",
            "",
            "  // Local custom summary",
            "  const customOut = customSummarized(src);",
            "  sink('customOut', customOut);",
            "",
            "  // Partial summary: modeled arg[1] -> proved; unmodeled arg[2] -> inconclusive",
            "  const partialHit = partialSummarized(cond, src, 'safe');",
            "  const partialMiss = partialSummarized(cond, 'safe', src);",
            "  sink('partialHit', partialHit);",
            "  sink('partialMiss', partialMiss);",
            "",
            "  // Unsummarized external library call on critical path -> inconclusive with missing-behavior-summary",
            "  const unknownOut = unknownExternal(src);",
            "  sink('unknownOut', unknownOut);",
            "}",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        await host.typecheck();

        const localPack = defineSummaries({
          id: "local-ext-lib-values",
          entries: [
            {
              match: { package: "ext-lib", memberPath: ["customSummarized"] },
              values: {
                completeness: "complete",
                flows: [
                  {
                    from: { kind: "arg", index: 0 },
                    to: "return",
                    preserveObjectPath: true,
                  },
                ],
              },
            },
            {
              match: { package: "ext-lib", memberPath: ["partialSummarized"] },
              values: {
                completeness: "partial",
                flows: [
                  {
                    from: { kind: "arg", index: 1 },
                    to: "return",
                    preserveObjectPath: true,
                  },
                ],
              },
            },
          ],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { packs: [localPack] },
        });
        try {
          const ctx = session.project();
          const calls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const originCall = calls.find((c) => c.text === "origin()");
          assert.ok(originCall);

          const sinkCalls = calls.filter((c) => c.text.startsWith("sink("));
          const resolvedSinks = await ctx.resolveCalls(
            sinkCalls.map((c) => c.ref),
            { select: { arguments: true } },
          );
          const tagRefs = resolvedSinks.map((r) => r.arguments![0]!);
          const tagTexts = await ctx.syntaxOf(tagRefs, {
            select: { text: true },
          });

          const targets: { readonly tag: string; readonly ref: SyntaxRef }[] =
            [];
          for (const [idx, row] of resolvedSinks.entries()) {
            const tag = (tagTexts[idx]?.text ?? "").replaceAll("'", "");
            const argRef = row.arguments?.[1];
            assert.ok(argRef, `Missing sink arg for ${tag}`);
            targets.push({ tag, ref: argRef });
          }

          const prop = await ctx.valuePropagation({
            propagation: "value",
            from: originCall.ref,
            to: targets.map((t) => t.ref),
          });
          const byTag = new Map(
            targets.map((t, i) => [t.tag, prop.results[i]!]),
          );

          const directRow = byTag.get("directCopy")!;
          assert.equal(directRow.status, "proved");
          assert.equal("gaps" in directRow, false);

          const cleanRow = byTag.get("cleanVal")!;
          assert.equal(cleanRow.status, "disproved");
          assert.equal("gaps" in cleanRow, false);

          const unrelatedRow = byTag.get("unrelatedOut")!;
          assert.equal(unrelatedRow.status, "disproved");

          const mappedRow = byTag.get("mappedOut")!;
          assert.equal(mappedRow.status, "proved");
          assert.ok(
            mappedRow.evidence.some(
              (ev) => ev.kind === "summary" && ev.summarySource === "builtin",
            ),
            JSON.stringify(mappedRow.evidence),
          );

          const promiseRow = byTag.get("promiseOut")!;
          assert.equal(promiseRow.status, "proved");
          assert.ok(
            promiseRow.evidence.some(
              (ev) => ev.kind === "summary" && ev.summarySource === "builtin",
            ),
            JSON.stringify(promiseRow.evidence),
          );

          const customRow = byTag.get("customOut")!;
          assert.equal(customRow.status, "proved");
          assert.ok(
            customRow.evidence.some(
              (ev) =>
                ev.kind === "summary" &&
                ev.summarySource === "local" &&
                ev.summaryId === "local-ext-lib-values",
            ),
            JSON.stringify(customRow.evidence),
          );

          const partialHitRow = byTag.get("partialHit")!;
          assert.equal(partialHitRow.status, "proved");

          const partialMissRow = byTag.get("partialMiss")!;
          assert.equal(partialMissRow.status, "inconclusive");
          assert.ok(
            partialMissRow.status === "inconclusive" &&
              partialMissRow.gaps.some(
                (g) => g.kind === "missing-behavior-summary",
              ),
          );

          const unknownRow = byTag.get("unknownOut")!;
          assert.equal(unknownRow.status, "inconclusive");
          assert.ok(
            unknownRow.status === "inconclusive" &&
              unknownRow.gaps.some(
                (g) => g.kind === "missing-behavior-summary",
              ),
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("propagates yielded values across generator yield, yield*, for-of, and destructuring without false disproved", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "ir-vp-generator-yield",
            private: true,
            type: "module",
          }),
          "tsconfig.json": BASE_TSCONFIG,
          "src/app.ts": [
            "function sink(_tag: string, _v: unknown): void {}",
            "function origin(): string { return 'secret'; }",
            "",
            "function* yieldSingle(item: string): Generator<string> {",
            "  yield item;",
            "}",
            "",
            "function* delegateSingle(item: string): Generator<string> {",
            "  yield* yieldSingle(item);",
            "}",
            "",
            "export function runGeneratorFlows(): void {",
            "  const raw = origin();",
            "  for (const elem of yieldSingle(raw)) {",
            "    sink('genForOf', elem);",
            "  }",
            "  for (const delegated of delegateSingle(raw)) {",
            "    sink('genDelegate', delegated);",
            "  }",
            "  const [destructured] = yieldSingle(raw);",
            "  sink('genDestructure', destructured);",
            "  sink('genObjectItself', yieldSingle(raw));",
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
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const originCall = calls.find((c) => c.text === "origin()");
          assert.ok(originCall);

          const tags = [
            "genForOf",
            "genDelegate",
            "genDestructure",
            "genObjectItself",
          ] as const;
          const targets = tags.map((tag) => {
            const sinkCall = calls.find((c) =>
              c.text.startsWith(`sink('${tag}'`),
            );
            const arg = sinkCall?.arguments?.[1];
            assert.ok(arg, `Missing sink arg for ${tag}`);
            return { tag, ref: arg };
          });

          const prop = await ctx.valuePropagation({
            propagation: "value",
            from: originCall.ref,
            to: targets.map((t) => t.ref),
          });
          const byTag = new Map(
            targets.map((t, i) => [t.tag, prop.results[i]!]),
          );

          assert.equal(byTag.get("genForOf")!.status, "proved");
          assert.equal(byTag.get("genDelegate")!.status, "proved");
          assert.equal(byTag.get("genDestructure")!.status, "proved");
          assert.equal(byTag.get("genObjectItself")!.status, "disproved");
        } finally {
          await session.close();
        }
      },
    );
  });
});
