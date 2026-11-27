import assert from "node:assert/strict";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";

import { linkNodeModulesPackage, withTempHost } from "@tssae/test-hosts";
import {
  TssaeError,
  defineSummaries,
  evidenceOf,
  openProject,
  type BehaviorSummaryPack,
  type CallRef,
  type DeclarationRef,
  type EngineCallIoMetrics,
} from "tssae";

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

function createPackageJson(
  name: string,
  extra: Record<string, unknown> = {},
): string {
  return JSON.stringify(
    {
      name,
      version: "1.0.0",
      type: "module",
      ...extra,
    },
    null,
    2,
  );
}

void describe("tssae behavior summaries", () => {
  void it("validates defineSummaries pure-data schemas and rejects functions, unknown keys, non-plain objects, and invalid semver", () => {
    const valid = defineSummaries({
      id: "pure-data-pack",
      version: "1.0.0",
      description: "Valid multi-aspect pack",
      entries: [
        {
          id: "entry-1",
          match: {
            package: "my-pkg",
            version: "^1.2.0",
            exportPath: "./sub",
            memberPath: ["Client", "run"],
          },
          calls: {
            completeness: "complete",
            invokesCallbacks: [
              {
                callbackArgIndex: 0,
                timing: "synchronous",
                cardinality: "at-most-once",
                thisArg: { fromArgIndex: 1 },
              },
            ],
          },
          values: {
            completeness: "partial",
            flows: [
              {
                from: "receiverElement",
                to: {
                  kind: "callbackParam",
                  callbackArgIndex: 0,
                  paramIndex: 0,
                },
              },
              {
                from: { kind: "callbackReturn", callbackArgIndex: 0 },
                to: "returnElement",
                preserveObjectPath: true,
              },
            ],
          },
          instances: {
            completeness: "complete",
            outcome: { kind: "allocates-fresh-instance" },
          },
          effects: {
            completeness: "complete",
            resourceOps: [
              {
                kind: "registers-into-container",
                item: { argIndex: 0 },
                container: "receiver",
              },
            ],
          },
        },
      ],
    });
    assert.equal(valid.id, "pure-data-pack");
    const validEvidence = evidenceOf(valid);
    assert.ok(
      validEvidence.some(
        (ev) =>
          ev.kind === "summary" &&
          ev.summaryId === "pure-data-pack" &&
          ev.summarySource === "local",
      ),
    );

    assert.throws(
      () =>
        defineSummaries({
          id: "has-fn",
          entries: [
            {
              match: { memberPath: ["foo"] },
              // @ts-expect-error runtime pure-data validation test
              extraFn: () => 1,
            },
          ],
        }),
      (err: unknown) =>
        err instanceof TssaeError && err.code === "INVALID_CONFIG",
    );

    assert.throws(
      () =>
        defineSummaries({
          id: "bad-semver",
          entries: [
            {
              match: {
                package: "foo",
                version: "not-a-semver-range",
                memberPath: ["bar"],
              },
            },
          ],
        }),
      (err: unknown) =>
        err instanceof TssaeError && err.code === "INVALID_CONFIG",
    );

    assert.throws(
      () =>
        defineSummaries({
          id: "non-plain-matcher",
          // @ts-expect-error runtime pure-data validation test
          entries: [new Date()],
        }),
      (err: unknown) =>
        err instanceof TssaeError && err.code === "INVALID_CONFIG",
    );

    assert.throws(
      () =>
        defineSummaries({
          id: "unknown-aspect-field",
          entries: [
            {
              match: { memberPath: ["foo"] },
              calls: {
                completeness: "complete",
                // @ts-expect-error unknown key rejection
                unknownField: true,
              },
            },
          ],
        }),
      (err: unknown) =>
        err instanceof TssaeError && err.code === "INVALID_CONFIG",
    );
  });

  void it("resolves built-in stdlib callbacks and forwarding for Array, ReadonlyArray, Promise, and Function.prototype.call/apply/bind with builtin evidence", async () => {
    await withTempHost(
      {
        files: {
          "package.json": createPackageJson("builtin-stdlib-summaries-host", {
            private: true,
          }),
          "tsconfig.json": BASE_TSCONFIG,
          "src/index.ts": [
            "function fnA(x: number): number {",
            "  return x + 1;",
            "}",
            "function fnB(x: number): number {",
            "  return x + 2;",
            "}",
            "const arrowFn = (x: number): number => x + 3;",
            "",
            "export function arrayMapAndFrom(): number[] {",
            "  const mapped = [fnA, fnB].map((mapCb) => mapCb(1));",
            "  const readonlyArr: readonly (() => number)[] = [() => fnA(2), () => fnB(3)];",
            "  readonlyArr.forEach((roCb) => roCb());",
            "  const fromMapped = Array.from([fnA, fnB], (fromCb) => fromCb(4));",
            "  return [...mapped, ...fromMapped];",
            "}",
            "",
            "export function promiseThenChain(): Promise<number> {",
            "  const p1 = Promise.resolve(fnA).then((p1Cb) => p1Cb(10));",
            "  const p2 = new Promise<(n: number) => number>((resolve) => {",
            "    resolve(fnB);",
            "  }).then((p2Cb) => p2Cb(20));",
            "  void p2;",
            "  return p1;",
            "}",
            "",
            "export function functionCallApplyBind(): number {",
            "  const a = fnA.call(undefined, 1);",
            "  const b = fnB.apply(undefined, [2]);",
            "  const bound = arrowFn.bind(undefined);",
            "  const c = bound(3);",
            "  return a + b + c;",
            "}",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        await host.typecheck();

        const rpcMetrics: EngineCallIoMetrics[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project(undefined, {
            observer: {
              observe: "summary",
              onRpcMetrics: (m) => rpcMetrics.push(m),
            },
          });

          const calls = await ctx.findSyntax({
            files: ["src/index.ts"],
            kinds: ["CallExpression"],
          });
          const decls = await ctx.findSyntax({
            files: ["src/index.ts"],
            kinds: ["FunctionDeclaration", "VariableDeclaration"],
          });

          const resolveCall = async (textPrefix: string): Promise<CallRef> => {
            const syn = calls.find(
              (c) => c.text === textPrefix || c.text.startsWith(textPrefix),
            );
            assert.ok(syn, `Missing call matching '${textPrefix}'`);
            const [res] = await ctx.resolveCalls([syn.ref]);
            assert.ok(res?.call, `Could not resolve call '${textPrefix}'`);
            return res.call;
          };

          const findDecl = async (snippet: string): Promise<DeclarationRef> => {
            const syn = decls.find((d) => d.text.includes(snippet));
            assert.ok(syn, `Missing declaration '${snippet}'`);
            const [res] = await ctx.resolveDeclarations([syn.ref]);
            assert.ok(res?.declaration);
            return res.declaration;
          };

          const mapCbCall = await resolveCall("mapCb(1)");
          const roCbCall = await resolveCall("roCb()");
          const fromCbCall = await resolveCall("fromCb(4)");
          const p1CbCall = await resolveCall("p1Cb(10)");
          const p2CbCall = await resolveCall("p2Cb(20)");
          const fnACallExpr = await resolveCall("fnA.call(");
          const fnBApplyExpr = await resolveCall("fnB.apply(");
          const boundCallExpr = await resolveCall("bound(3)");
          const promiseResolveCall = await resolveCall(
            "Promise.resolve(fnA).then(",
          );

          const [
            mapCbRes,
            roCbRes,
            fromCbRes,
            p1CbRes,
            p2CbRes,
            callRes,
            applyRes,
            bindRes,
            thenDirectRes,
          ] = await ctx.calleesOf([
            mapCbCall,
            roCbCall,
            fromCbCall,
            p1CbCall,
            p2CbCall,
            fnACallExpr,
            fnBApplyExpr,
            boundCallExpr,
            promiseResolveCall,
          ]);

          assert.equal(mapCbRes!.completeness, "complete");
          assert.deepEqual(
            mapCbRes!.resolvedTargets.map((t) => t.name),
            ["fnA", "fnB"],
          );
          assert.ok(
            mapCbRes!.evidence.some(
              (e) => e.kind === "summary" && e.summarySource === "builtin",
            ),
          );

          assert.equal(roCbRes!.completeness, "complete");
          assert.equal(roCbRes!.resolvedTargets.length, 2);
          assert.ok(
            roCbRes!.evidence.some(
              (e) => e.kind === "summary" && e.summarySource === "builtin",
            ),
          );

          assert.equal(fromCbRes!.completeness, "complete");
          assert.deepEqual(
            fromCbRes!.resolvedTargets.map((t) => t.name),
            ["fnA", "fnB"],
          );

          assert.equal(p1CbRes!.completeness, "complete");
          assert.deepEqual(
            p1CbRes!.resolvedTargets.map((t) => t.name),
            ["fnA"],
          );
          assert.ok(
            p1CbRes!.evidence.some(
              (e) => e.kind === "summary" && e.summarySource === "builtin",
            ),
          );

          assert.equal(p2CbRes!.completeness, "complete");
          assert.deepEqual(
            p2CbRes!.resolvedTargets.map((t) => t.name),
            ["fnB"],
          );

          // Promise.prototype.then also registers callback invocation directly
          assert.equal(thenDirectRes!.completeness, "complete");
          assert.ok(
            thenDirectRes!.evidence.some(
              (e) => e.kind === "summary" && e.summarySource === "builtin",
            ),
          );

          // Function.prototype.call / apply / bind forward to target callable
          assert.equal(callRes!.completeness, "complete");
          assert.deepEqual(
            callRes!.resolvedTargets.map((t) => t.name),
            ["fnA"],
          );
          assert.equal(
            callRes!.resolvedTargets[0]!.parameterBindings[0]?.parameterName,
            "x",
          );
          assert.ok(
            callRes!.evidence.some(
              (e) => e.kind === "summary" && e.summarySource === "builtin",
            ),
          );

          assert.equal(applyRes!.completeness, "complete");
          assert.deepEqual(
            applyRes!.resolvedTargets.map((t) => t.name),
            ["fnB"],
          );
          assert.ok(
            applyRes!.evidence.some(
              (e) => e.kind === "summary" && e.summarySource === "builtin",
            ),
          );

          assert.equal(bindRes!.completeness, "complete");
          assert.equal(bindRes!.resolvedTargets.length, 1);
          assert.equal(bindRes!.resolvedTargets[0]!.location?.line, 7);
          assert.ok(
            bindRes!.evidence.some(
              (e) => e.kind === "summary" && e.summarySource === "builtin",
            ),
          );

          // Reverse callersOf on fnA includes fnA.call(...)
          const fnADecl = await findDecl("function fnA");
          const [fnACallers] = await ctx.callersOf([fnADecl]);
          assert.ok(
            fnACallers!.callSites.some((site) => site.location.line === 27),
            "callersOf(fnA) must include fnA.call(...) at line 27",
          );

          // Engine summaryLookup metrics record builtin hits
          const lookupStats = rpcMetrics
            .map((m) => m.engineMetrics?.summaryLookup)
            .filter((s): s is NonNullable<typeof s> => s !== undefined);
          assert.ok(lookupStats.length > 0);
          const totalBuiltinHits = lookupStats.reduce(
            (sum, s) => sum + s.hitsBySource.builtin,
            0,
          );
          assert.ok(totalBuiltinHits > 0);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("auto-discovers local, package, and community summaries in priority order, enforces strict on-demand discovery, and ignores unused node_modules", async () => {
    await withTempHost(
      {
        files: {
          "package.json": createPackageJson("four-tier-priority-host", {
            private: true,
          }),
          "tsconfig.json": BASE_TSCONFIG,
          "tssae.summaries.ts": [
            "export default {",
            "  id: 'local-convention-pack',",
            "  entries: [",
            "    {",
            "      id: 'local-overrides-pkg-and-community',",
            "      match: {",
            "        package: 'pkg-all',",
            "        exportPath: '.',",
            "        memberPath: ['runAll'],",
            "      },",
            "      calls: {",
            "        completeness: 'complete',",
            "        invokesCallbacks: [{ callbackArgIndex: 0, timing: 'synchronous' }],",
            "      },",
            "      values: {",
            "        completeness: 'complete',",
            "        flows: [{ from: { kind: 'arg', index: 1 }, to: { kind: 'callbackParam', callbackArgIndex: 0, paramIndex: 0 } }],",
            "      },",
            "    },",
            "    {",
            "      id: 'local-source-precedence',",
            "      match: {",
            "        module: 'src/index.ts',",
            "        memberPath: ['localWithBody'],",
            "      },",
            "      calls: {",
            "        completeness: 'complete',",
            "        invokesCallbacks: [{ callbackArgIndex: 1, timing: 'synchronous' }],",
            "      },",
            "    },",
            "  ],",
            "};",
          ].join("\n"),
          "src/index.ts": [
            "import { runAll } from 'pkg-all';",
            "import { dispatch } from '@acme/runner/sub';",
            "import { trackOnly } from 'pkg-community-only';",
            "",
            "function fnFromArg0(): void {}",
            "function fnFromArg1(): void {}",
            "function fnCommunityOnly(): void {}",
            "function fnPkgSub(): void {}",
            "",
            "// Local source body calls arg0, whereas local summary says arg1",
            "export function localWithBody(cb0: () => void, _cb1: () => void): void {",
            "  cb0();",
            "}",
            "",
            "export function runScenario(): void {",
            "  localWithBody(fnFromArg0, fnFromArg1);",
            "  runAll((f) => f(), fnFromArg1);",
            "  dispatch(fnPkgSub);",
            "  trackOnly(fnCommunityOnly);",
            "}",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        // 1. Package pkg-all (v1.2.0): has BOTH package-bundled summary AND @tssae-summaries/pkg-all AND local override
        host.writeText(
          "packages/pkg-all/package.json",
          JSON.stringify({
            name: "pkg-all",
            version: "1.2.0",
            types: "./index.d.ts",
            tssaeSummaries: "./pkg.summaries.json",
          }),
        );
        host.writeText(
          "packages/pkg-all/index.d.ts",
          "export declare function runAll(cb: (fn: () => void) => void, payload: () => void): void;\n",
        );
        host.writeText(
          "packages/pkg-all/pkg.summaries.json",
          JSON.stringify({
            id: "pkg-all-bundled",
            entries: [
              {
                match: {
                  package: "pkg-all",
                  version: "^1.0.0",
                  memberPath: ["runAll"],
                },
                calls: { completeness: "complete", invokesCallbacks: [] },
              },
            ],
          }),
        );
        host.writeText(
          "packages/comm-pkg-all/package.json",
          JSON.stringify({
            name: "@tssae-summaries/pkg-all",
            version: "1.0.0",
            main: "./index.mjs",
          }),
        );
        host.writeText(
          "packages/comm-pkg-all/index.mjs",
          `export default ${JSON.stringify({
            id: "comm-pkg-all",
            entries: [
              {
                match: { package: "pkg-all", memberPath: ["runAll"] },
                calls: { completeness: "complete", invokesCallbacks: [] },
              },
            ],
          })};\n`,
        );

        // 2. Scoped package @acme/runner (v2.1.0): package-bundled overrides @tssae-summaries/acme__runner; semver ^2.0.0 matches, ./sub subpath matches
        host.writeText(
          "packages/acme-runner/package.json",
          JSON.stringify({
            name: "@acme/runner",
            version: "2.1.0",
            exports: {
              "./sub": { types: "./sub.d.ts" },
            },
            tssaeSummaries: "./summaries.mjs",
          }),
        );
        host.writeText(
          "packages/acme-runner/sub.d.ts",
          "export declare function dispatch(cb: () => void): void;\n",
        );
        host.writeText(
          "packages/acme-runner/summaries.mjs",
          `export default ${JSON.stringify({
            id: "acme-runner-pkg",
            entries: [
              {
                match: {
                  package: "@acme/runner",
                  version: "^2.0.0",
                  exportPath: "./sub",
                  memberPath: ["dispatch"],
                },
                calls: {
                  completeness: "complete",
                  invokesCallbacks: [
                    { callbackArgIndex: 0, timing: "synchronous" },
                  ],
                },
              },
            ],
          })};\n`,
        );
        host.writeText(
          "packages/comm-acme-runner/package.json",
          JSON.stringify({
            name: "@tssae-summaries/acme__runner",
            version: "1.0.0",
            main: "./index.json",
          }),
        );
        host.writeText(
          "packages/comm-acme-runner/index.json",
          JSON.stringify({
            id: "comm-acme-runner",
            entries: [
              {
                match: {
                  package: "@acme/runner",
                  exportPath: "./sub",
                  memberPath: ["dispatch"],
                },
                // Intentionally declares NO callbacks so if community beat package, test fails!
                calls: { completeness: "complete", invokesCallbacks: [] },
              },
            ],
          }),
        );

        // 3. Community-only package pkg-community-only with PARTIAL completeness
        host.writeText(
          "packages/pkg-comm-only/package.json",
          JSON.stringify({
            name: "pkg-community-only",
            version: "1.0.0",
            types: "./index.d.ts",
          }),
        );
        host.writeText(
          "packages/pkg-comm-only/index.d.ts",
          "export declare function trackOnly(cb: () => void): void;\n",
        );
        host.writeText(
          "packages/comm-only/package.json",
          JSON.stringify({
            name: "@tssae-summaries/pkg-community-only",
            version: "1.0.0",
            tssaeSummaries: "./summary.json",
          }),
        );
        host.writeText(
          "packages/comm-only/summary.json",
          JSON.stringify({
            id: "comm-only-pack",
            entries: [
              {
                match: {
                  package: "pkg-community-only",
                  memberPath: ["trackOnly"],
                },
                calls: {
                  completeness: "partial",
                  invokesCallbacks: [
                    { callbackArgIndex: 0, timing: "deferred" },
                  ],
                },
              },
            ],
          }),
        );

        // 4. UNUSED broken package & broken community summary in node_modules (MUST NOT be discovered!)
        host.writeText(
          "packages/unused-broken/package.json",
          JSON.stringify({
            name: "unused-broken",
            version: "1.0.0",
            types: "./index.d.ts",
            tssaeSummaries: "./broken-does-not-exist-or-invalid.ts",
          }),
        );
        host.writeText(
          "packages/unused-broken/index.d.ts",
          "export declare function unusedOp(): void;\n",
        );
        host.writeText(
          "packages/comm-unused-broken/package.json",
          JSON.stringify({
            name: "@tssae-summaries/unused-broken",
            version: "1.0.0",
            main: "./broken.json",
          }),
        );
        host.writeText(
          "packages/comm-unused-broken/broken.json",
          "{ invalid json !!!",
        );

        linkNodeModulesPackage(
          host.rootDir,
          "pkg-all",
          join(host.rootDir, "packages/pkg-all"),
        );
        linkNodeModulesPackage(
          host.rootDir,
          "@tssae-summaries/pkg-all",
          join(host.rootDir, "packages/comm-pkg-all"),
        );
        linkNodeModulesPackage(
          host.rootDir,
          "@acme/runner",
          join(host.rootDir, "packages/acme-runner"),
        );
        linkNodeModulesPackage(
          host.rootDir,
          "@tssae-summaries/acme__runner",
          join(host.rootDir, "packages/comm-acme-runner"),
        );
        linkNodeModulesPackage(
          host.rootDir,
          "pkg-community-only",
          join(host.rootDir, "packages/pkg-comm-only"),
        );
        linkNodeModulesPackage(
          host.rootDir,
          "@tssae-summaries/pkg-community-only",
          join(host.rootDir, "packages/comm-only"),
        );
        linkNodeModulesPackage(
          host.rootDir,
          "unused-broken",
          join(host.rootDir, "packages/unused-broken"),
        );
        linkNodeModulesPackage(
          host.rootDir,
          "@tssae-summaries/unused-broken",
          join(host.rootDir, "packages/comm-unused-broken"),
        );

        await host.typecheck();

        const rpcMetrics: EngineCallIoMetrics[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          observe: "summary",
          onRpcMetrics: (m) => rpcMetrics.push(m),
        });
        try {
          // Only one RPC metrics emission for openProject
          assert.equal(rpcMetrics.length, 1);

          // Discovered summaries registered in additionalInputs; unused packages absent
          const inputPaths = session.additionalInputs
            .filter((entry) => entry.sources.some((s) => s.kind === "summary"))
            .map((entry) => entry.file);
          assert.ok(inputPaths.includes("tssae.summaries.ts"));
          assert.ok(
            inputPaths.some((p) => p.includes("acme-runner/summaries.mjs")),
          );
          assert.ok(
            inputPaths.some((p) => p.includes("comm-only/summary.json")),
          );
          assert.ok(!inputPaths.some((p) => p.includes("unused-broken")));

          const ctx = session.project(undefined, {
            observer: {
              observe: "detail",
              onRpcMetrics: (m) => rpcMetrics.push(m),
            },
          });

          const calls = await ctx.findSyntax({
            files: ["src/index.ts"],
            kinds: ["CallExpression"],
          });
          const resolveCall = async (prefix: string): Promise<CallRef> => {
            const syn = calls.find((c) => c.text.startsWith(prefix));
            assert.ok(syn, `Missing call '${prefix}'`);
            const [res] = await ctx.resolveCalls([syn.ref]);
            assert.ok(res?.call);
            return res.call;
          };

          const [localBodyRes, runAllCbRes, dispatchRes, trackOnlyRes] =
            await ctx.calleesOf([
              await resolveCall("localWithBody("),
              await resolveCall("f()"),
              await resolveCall("dispatch("),
              await resolveCall("trackOnly("),
            ]);

          // 1. Source body vs local summary precedence: local summary says arg 1 (fnFromArg1), NOT body's arg 0
          const compareNames = (a: string | null, b: string | null): number =>
            (a ?? "").localeCompare(b ?? "");
          assert.equal(localBodyRes!.completeness, "complete");
          assert.deepEqual(
            localBodyRes!.resolvedTargets
              .map((t) => t.name)
              .toSorted(compareNames),
            ["fnFromArg1", "localWithBody"],
          );
          assert.ok(
            localBodyRes!.evidence.some(
              (e) =>
                e.kind === "summary" &&
                e.summarySource === "local" &&
                e.summaryId === "local-convention-pack",
            ),
          );

          // 2. Local overrides package & community for pkg-all:runAll -> f() resolves fnFromArg1 cleanly
          assert.equal(runAllCbRes!.completeness, "complete");
          assert.deepEqual(
            runAllCbRes!.resolvedTargets.map((t) => t.name),
            ["fnFromArg1"],
          );
          assert.ok(
            runAllCbRes!.evidence.some(
              (e) => e.kind === "summary" && e.summarySource === "local",
            ),
          );

          // 3. Package-bundled summary overrides @tssae-summaries/acme__runner
          assert.equal(dispatchRes!.completeness, "complete");
          assert.deepEqual(
            dispatchRes!.resolvedTargets
              .map((t) => t.name)
              .toSorted(compareNames),
            ["dispatch", "fnPkgSub"],
          );
          assert.ok(
            dispatchRes!.evidence.some(
              (e) =>
                e.kind === "summary" &&
                e.summarySource === "package" &&
                e.summaryId === "acme-runner-pkg",
            ),
          );

          // 4. Partial completeness from @tssae-summaries/pkg-community-only retains known target AND incomplete gap
          assert.equal(trackOnlyRes!.completeness, "incomplete");
          assert.deepEqual(
            trackOnlyRes!.resolvedTargets
              .map((t) => t.name)
              .toSorted(compareNames),
            ["fnCommunityOnly", "trackOnly"],
          );
          assert.ok(
            trackOnlyRes!.completeness === "incomplete" &&
              trackOnlyRes!.gaps.some(
                (g) => g.kind === "missing-behavior-summary",
              ),
          );
          assert.ok(
            trackOnlyRes!.evidence.some(
              (e) =>
                e.kind === "summary" &&
                e.summarySource === "community" &&
                e.summaryId === "comm-only-pack",
            ),
          );

          // Verify local, package, community lookup counters
          const querySummaryMetrics = rpcMetrics
            .slice(1)
            .map((m) => m.engineMetrics?.summaryLookup)
            .filter((s): s is NonNullable<typeof s> => s !== undefined);
          assert.ok(querySummaryMetrics.some((s) => s.hitsBySource.local > 0));
          assert.ok(
            querySummaryMetrics.some((s) => s.hitsBySource.package > 0),
          );
          assert.ok(
            querySummaryMetrics.some((s) => s.hitsBySource.community > 0),
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("throws SUMMARY_CONFLICT with both conflicting parties when two complete summaries in the same tier disagree", async () => {
    await withTempHost(
      {
        files: {
          "package.json": createPackageJson("conflict-host", { private: true }),
          "tsconfig.json": BASE_TSCONFIG,
          "src/index.ts": [
            "export declare function extTarget(a: () => void, b: () => void): void;",
            "function fn1(): void {}",
            "function fn2(): void {}",
            "export function main(): void { extTarget(fn1, fn2); }",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        await host.typecheck();

        const packA: BehaviorSummaryPack = defineSummaries({
          id: "local-pack-a",
          entries: [
            {
              id: "entry-a",
              match: { module: "src/index.ts", memberPath: ["extTarget"] },
              calls: {
                completeness: "complete",
                invokesCallbacks: [
                  { callbackArgIndex: 0, timing: "synchronous" },
                ],
              },
            },
          ],
        });
        const packB: BehaviorSummaryPack = defineSummaries({
          id: "local-pack-b",
          entries: [
            {
              id: "entry-b",
              match: { module: "src/index.ts", memberPath: ["extTarget"] },
              calls: {
                completeness: "complete",
                invokesCallbacks: [
                  { callbackArgIndex: 1, timing: "synchronous" },
                ],
              },
            },
          ],
        });

        await assert.rejects(
          async () =>
            openProject({
              cwd: host.rootDir,
              tsconfig: "tsconfig.json",
              summaries: { packs: [packA, packB], discovery: false },
            }),
          (err: unknown) => {
            assert.ok(err instanceof TssaeError);
            assert.equal(err.code, "SUMMARY_CONFLICT");
            assert.match(err.message, /local-pack-a/);
            assert.match(err.message, /local-pack-b/);
            const conflict = err.summaryConflict;
            assert.ok(conflict);
            assert.deepEqual(
              conflict.parties.map((p) => p.packId),
              ["local-pack-a", "local-pack-b"],
            );
            return true;
          },
        );
      },
    );
  });

  void it("also catches runtime same-tier conflicts between overlapping matchers inside community or engine lookup", async () => {
    await withTempHost(
      {
        files: {
          "package.json": createPackageJson("runtime-conflict-host", {
            private: true,
          }),
          "tsconfig.json": BASE_TSCONFIG,
          "src/index.ts": [
            "import { run } from 'comm-conflict';",
            "function f(): void {}",
            "export function main(): void { run(f); }",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        host.writeText(
          "packages/comm-conflict/package.json",
          JSON.stringify({
            name: "comm-conflict",
            version: "1.2.3",
            types: "./index.d.ts",
          }),
        );
        host.writeText(
          "packages/comm-conflict/index.d.ts",
          "export declare function run(cb: () => void): void;\n",
        );
        host.writeText(
          "packages/comm-summaries/package.json",
          JSON.stringify({
            name: "@tssae-summaries/comm-conflict",
            version: "1.0.0",
            main: "./index.json",
          }),
        );
        // Same community pack declares two conflicting entries with different version ranges both matching 1.2.3
        host.writeText(
          "packages/comm-summaries/index.json",
          JSON.stringify({
            id: "comm-conflict-pack",
            entries: [
              {
                id: "rule-v1",
                match: {
                  package: "comm-conflict",
                  version: "^1.0.0",
                  memberPath: ["run"],
                },
                calls: {
                  completeness: "complete",
                  invokesCallbacks: [
                    { callbackArgIndex: 0, timing: "synchronous" },
                  ],
                },
              },
              {
                id: "rule-v1-2",
                match: {
                  package: "comm-conflict",
                  version: ">=1.2.0",
                  memberPath: ["run"],
                },
                calls: { completeness: "complete", invokesCallbacks: [] },
              },
            ],
          }),
        );

        linkNodeModulesPackage(
          host.rootDir,
          "comm-conflict",
          join(host.rootDir, "packages/comm-conflict"),
        );
        linkNodeModulesPackage(
          host.rootDir,
          "@tssae-summaries/comm-conflict",
          join(host.rootDir, "packages/comm-summaries"),
        );
        await host.typecheck();

        await assert.rejects(
          async () =>
            openProject({ cwd: host.rootDir, tsconfig: "tsconfig.json" }),
          (err: unknown) => {
            assert.ok(err instanceof TssaeError);
            assert.equal(err.code, "SUMMARY_CONFLICT");
            assert.match(err.message, /rule-v1|comm-conflict-pack/);
            return true;
          },
        );
      },
    );
  });

  void it("respects openProject summaries.discovery restrictions and reloads edited .ts/.json summary files on session.update()", async () => {
    await withTempHost(
      {
        files: {
          "package.json": createPackageJson("discovery-and-update-host", {
            private: true,
          }),
          "tsconfig.json": BASE_TSCONFIG,
          "tssae.summaries.ts": [
            "export default {",
            "  id: 'updatable-local-ts',",
            "  entries: [",
            "    {",
            "      id: 'invoke-fn-initial',",
            "      match: { module: 'src/index.ts', memberPath: ['invokeHelper'] },",
            "      calls: {",
            "        completeness: 'complete',",
            "        invokesCallbacks: [{ callbackArgIndex: 0, timing: 'synchronous' }],",
            "      },",
            "      values: {",
            "        completeness: 'complete',",
            "        flows: [{ from: { kind: 'arg', index: 1 }, to: { kind: 'callbackParam', callbackArgIndex: 0, paramIndex: 0 } }],",
            "      },",
            "    },",
            "  ],",
            "};",
          ].join("\n"),
          "custom.summaries.mjs": [
            "export default {",
            "  id: 'explicit-mjs-file',",
            "  entries: [],",
            "};",
          ].join("\n"),
          "extra.summaries.json": JSON.stringify({
            id: "explicit-json-file",
            entries: [],
          }),
          "src/index.ts": [
            "function fnA(): void {}",
            "function fnB(): void {}",
            "export declare function invokeHelper(",
            "  cb: (paramFn: () => void) => void,",
            "  firstPayload: () => void,",
            "  secondPayload: () => void,",
            "): void;",
            "",
            "export function run(): void {",
            "  [fnA].map((cb) => cb());",
            "  invokeHelper((paramFn) => paramFn(), fnA, fnB);",
            "}",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        await host.typecheck();

        // 1. discovery: false disables builtins AND convention local summaries
        const noDiscSession = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { discovery: false },
        });
        try {
          assert.equal(noDiscSession.additionalInputs.length, 0);
          const ctx = noDiscSession.project();
          const calls = await ctx.findSyntax({
            files: ["src/index.ts"],
            kinds: ["CallExpression"],
          });
          const cbSyn = calls.find((c) => c.text === "cb()");
          const paramSyn = calls.find((c) => c.text === "paramFn()");
          const [cbCallRes, paramCallRes] = await ctx.resolveCalls([
            cbSyn!.ref,
            paramSyn!.ref,
          ]);
          const [cbCallees, paramCallees] = await ctx.calleesOf([
            cbCallRes!.call!,
            paramCallRes!.call!,
          ]);
          assert.equal(cbCallees!.completeness, "incomplete");
          assert.deepEqual(cbCallees!.targets, []);
          assert.equal(paramCallees!.completeness, "incomplete");
          assert.deepEqual(paramCallees!.targets, []);
        } finally {
          await noDiscSession.close();
        }

        // 2. discovery: ["builtin"] enables builtins ONLY, skipping local tssae.summaries.ts
        const builtinOnlySession = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: { discovery: ["builtin"] },
        });
        try {
          assert.equal(builtinOnlySession.additionalInputs.length, 0);
          const ctx = builtinOnlySession.project();
          const calls = await ctx.findSyntax({
            files: ["src/index.ts"],
            kinds: ["CallExpression"],
          });
          const cbSyn = calls.find((c) => c.text === "cb()");
          const paramSyn = calls.find((c) => c.text === "paramFn()");
          const [cbCallRes, paramCallRes] = await ctx.resolveCalls([
            cbSyn!.ref,
            paramSyn!.ref,
          ]);
          const [cbCallees, paramCallees] = await ctx.calleesOf([
            cbCallRes!.call!,
            paramCallRes!.call!,
          ]);
          assert.equal(cbCallees!.completeness, "complete");
          assert.deepEqual(
            cbCallees!.resolvedTargets.map((t) => t.name),
            ["fnA"],
          );
          assert.equal(paramCallees!.completeness, "incomplete");
          assert.deepEqual(paramCallees!.targets, []);
        } finally {
          await builtinOnlySession.close();
        }

        // 3. Edit local tssae.summaries.ts & reload via session.update() (cache-busted ESM import!)
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          summaries: {
            files: ["custom.summaries.mjs", "extra.summaries.json"],
          },
        });
        try {
          const summaryPaths = session.additionalInputs
            .filter((i) => i.sources.some((s) => s.kind === "summary"))
            .map((i) => i.file);
          assert.ok(summaryPaths.includes("tssae.summaries.ts"));
          assert.ok(summaryPaths.includes("custom.summaries.mjs"));
          assert.ok(summaryPaths.includes("extra.summaries.json"));

          const calls1 = await session
            .project()
            .findSyntax({ files: ["src/index.ts"], kinds: ["CallExpression"] });
          const [invRef1] = await session
            .project()
            .resolveCalls([calls1.find((c) => c.text === "paramFn()")!.ref]);
          const [res1] = await session.project().calleesOf([invRef1!.call!]);
          assert.equal(res1!.completeness, "complete");
          assert.deepEqual(
            res1!.resolvedTargets.map((t) => t.name),
            ["fnA"],
          );

          // Modify tssae.summaries.ts on disk to forward argIndex 2 (fnB) instead of argIndex 1 (fnA)
          writeFileSync(
            join(host.rootDir, "tssae.summaries.ts"),
            [
              "export default {",
              "  id: 'updatable-local-ts',",
              "  entries: [",
              "    {",
              "      id: 'invoke-fn-updated',",
              "      match: { module: 'src/index.ts', memberPath: ['invokeHelper'] },",
              "      calls: {",
              "        completeness: 'complete',",
              "        invokesCallbacks: [{ callbackArgIndex: 0, timing: 'synchronous' }],",
              "      },",
              "      values: {",
              "        completeness: 'complete',",
              "        flows: [{ from: { kind: 'arg', index: 2 }, to: { kind: 'callbackParam', callbackArgIndex: 0, paramIndex: 0 } }],",
              "      },",
              "    },",
              "  ],",
              "};",
            ].join("\n"),
            "utf8",
          );

          const updated = await session.update();
          assert.ok(updated.recomputation.invalidatedBy.includes("summaries"));
          assert.ok(
            updated.recomputation.invalidatedBy.some((item) =>
              item.includes("tssae.summaries.ts"),
            ),
          );

          const calls2 = await session
            .project()
            .findSyntax({ files: ["src/index.ts"], kinds: ["CallExpression"] });
          const [invRef2] = await session
            .project()
            .resolveCalls([calls2.find((c) => c.text === "paramFn()")!.ref]);
          const [res2] = await session.project().calleesOf([invRef2!.call!]);
          assert.equal(res2!.completeness, "complete");
          assert.deepEqual(
            res2!.resolvedTargets.map((t) => t.name),
            ["fnB"],
          );

          // Remove unused file test cleanly if needed
          rmSync(join(host.rootDir, "extra.summaries.json"), { force: true });
          mkdirSync(join(host.rootDir, "src"), { recursive: true });
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("allows disjoint semver ranges and rejects overlapping ranges for uninstalled packages and multi-version monorepo packages", async () => {
    await withTempHost(
      {
        files: {
          "package.json": createPackageJson("semver-conflict-host", {
            private: true,
          }),
          "apps/a/package.json": createPackageJson("app-a", { private: true }),
          "apps/a/tsconfig.json": BASE_TSCONFIG,
          "apps/a/src/index.ts": [
            'import { run } from "multi-ver";',
            "function fnA(): void {}",
            "export function mainA(): void { run(fnA); }",
            "",
          ].join("\n"),
          "apps/b/package.json": createPackageJson("app-b", { private: true }),
          "apps/b/tsconfig.json": BASE_TSCONFIG,
          "apps/b/src/index.ts": [
            'import { run } from "multi-ver";',
            "function fnB(): void {}",
            "export function mainB(): void { run(fnB); }",
            "",
          ].join("\n"),
        },
      },
      async (host) => {
        host.writeText(
          "packages/mv-1/package.json",
          JSON.stringify({
            name: "multi-ver",
            version: "1.2.0",
            types: "./index.d.ts",
          }),
        );
        host.writeText(
          "packages/mv-1/index.d.ts",
          "export declare function run(cb: () => void): void;\n",
        );
        host.writeText(
          "packages/mv-2/package.json",
          JSON.stringify({
            name: "multi-ver",
            version: "2.1.0",
            types: "./index.d.ts",
          }),
        );
        host.writeText(
          "packages/mv-2/index.d.ts",
          "export declare function run(cb: () => void): void;\n",
        );

        linkNodeModulesPackage(
          join(host.rootDir, "apps/a"),
          "multi-ver",
          join(host.rootDir, "packages/mv-1"),
        );
        linkNodeModulesPackage(
          join(host.rootDir, "apps/b"),
          "multi-ver",
          join(host.rootDir, "packages/mv-2"),
        );

        // 1. Uninstalled package with disjoint complete summaries (^1.0.0 vs ^2.0.0) -> no conflict
        //    AND multi-version resolved packages (1.2.0 and 2.1.0) with ^1.0.0 vs ^2.0.0 -> no conflict
        const disjointSession = await openProject({
          cwd: host.rootDir,
          projects: ["apps/a/tsconfig.json", "apps/b/tsconfig.json"],
          summaries: {
            discovery: false,
            packs: [
              defineSummaries({
                id: "pack-v1",
                entries: [
                  {
                    match: {
                      package: "uninstalled-foo",
                      version: "^1.0.0",
                      memberPath: ["doWork"],
                    },
                    calls: { completeness: "complete", invokesCallbacks: [] },
                  },
                  {
                    match: {
                      package: "multi-ver",
                      version: "^1.0.0",
                      memberPath: ["run"],
                    },
                    calls: {
                      completeness: "complete",
                      invokesCallbacks: [
                        { callbackArgIndex: 0, timing: "synchronous" },
                      ],
                    },
                  },
                ],
              }),
              defineSummaries({
                id: "pack-v2",
                entries: [
                  {
                    match: {
                      package: "uninstalled-foo",
                      version: "^2.0.0",
                      memberPath: ["doWork"],
                    },
                    calls: {
                      completeness: "complete",
                      invokesCallbacks: [
                        { callbackArgIndex: 0, timing: "synchronous" },
                      ],
                    },
                  },
                  {
                    match: {
                      package: "multi-ver",
                      version: "^2.0.0",
                      memberPath: ["run"],
                    },
                    calls: { completeness: "complete", invokesCallbacks: [] },
                  },
                ],
              }),
            ],
          },
        });
        await disjointSession.close();

        // 2. Uninstalled package with overlapping complete summaries (^1.0.0 vs >=1.2.0 <2.0.0) -> SUMMARY_CONFLICT
        await assert.rejects(
          async () =>
            openProject({
              cwd: host.rootDir,
              projects: ["apps/a/tsconfig.json"],
              summaries: {
                discovery: false,
                packs: [
                  defineSummaries({
                    id: "uninstalled-overlap-1",
                    entries: [
                      {
                        match: {
                          package: "uninstalled-foo",
                          version: "^1.0.0",
                          memberPath: ["doWork"],
                        },
                        calls: {
                          completeness: "complete",
                          invokesCallbacks: [],
                        },
                      },
                    ],
                  }),
                  defineSummaries({
                    id: "uninstalled-overlap-2",
                    entries: [
                      {
                        match: {
                          package: "uninstalled-foo",
                          version: ">=1.2.0 <2.0.0",
                          memberPath: ["doWork"],
                        },
                        calls: {
                          completeness: "complete",
                          invokesCallbacks: [
                            { callbackArgIndex: 0, timing: "synchronous" },
                          ],
                        },
                      },
                    ],
                  }),
                ],
              },
            }),
          (err: unknown) =>
            err instanceof TssaeError && err.code === "SUMMARY_CONFLICT",
        );

        // 3. Multi-version monorepo packages retain earlier 1.2.0 version when 2.1.0 is also resolved
        await assert.rejects(
          async () =>
            openProject({
              cwd: host.rootDir,
              projects: ["apps/a/tsconfig.json", "apps/b/tsconfig.json"],
              summaries: {
                discovery: false,
                packs: [
                  defineSummaries({
                    id: "mv-conflict-a",
                    entries: [
                      {
                        match: {
                          package: "multi-ver",
                          version: "^1.0.0",
                          memberPath: ["run"],
                        },
                        calls: {
                          completeness: "complete",
                          invokesCallbacks: [
                            { callbackArgIndex: 0, timing: "synchronous" },
                          ],
                        },
                      },
                    ],
                  }),
                  defineSummaries({
                    id: "mv-conflict-b",
                    entries: [
                      {
                        match: {
                          package: "multi-ver",
                          version: ">=1.2.0 <2.0.0",
                          memberPath: ["run"],
                        },
                        calls: {
                          completeness: "complete",
                          invokesCallbacks: [],
                        },
                      },
                    ],
                  }),
                ],
              },
            }),
          (err: unknown) =>
            err instanceof TssaeError && err.code === "SUMMARY_CONFLICT",
        );
      },
    );
  });
});
