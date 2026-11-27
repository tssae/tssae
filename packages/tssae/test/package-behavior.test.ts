import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { promisify } from "node:util";

import { linkNodeModulesPackage, withTempHost } from "@tssae/test-hosts";
import {
  defineSummaries,
  isSameInstance,
  openProject,
  type SyntaxRef,
} from "tssae";

const execFileAsync = promisify(execFile);
const require = createRequire(import.meta.url);
const tscScriptPath = join(
  dirname(require.resolve("typescript-7/package.json")),
  "bin",
  "tsc",
);

async function buildTsProject(
  rootDir: string,
  tsconfigRelativePath: string,
): Promise<void> {
  const tsconfigPath = join(rootDir, tsconfigRelativePath);
  await execFileAsync(
    process.execPath,
    [tscScriptPath, "-p", tsconfigPath, "--pretty", "false"],
    { cwd: rootDir, encoding: "utf8" },
  );
}

void describe("tssae first-party implementation resolution", () => {
  void it("resolves first-party monorepo implementations in priority order: project references, customConditions, and declarationMap", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "first-party-priority-monorepo",
            private: true,
            type: "module",
          }),
          "packages/pkg-ref/package.json": JSON.stringify({
            name: "@ws/pkg-ref",
            version: "1.0.0",
            type: "module",
            exports: {
              ".": {
                types: "./dist/index.d.ts",
                default: "./dist/index.js",
              },
            },
          }),
          "packages/pkg-ref/tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              composite: true,
              declaration: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              rootDir: "src",
              outDir: "dist",
            },
            include: ["src/**/*.ts"],
          }),
          "packages/pkg-ref/src/index.ts": [
            "export function refRelay<T>(value: T): T {",
            "  return value;",
            "}",
            "",
          ].join("\n"),

          "packages/pkg-cond/package.json": JSON.stringify({
            name: "@ws/pkg-cond",
            version: "1.0.0",
            type: "module",
            exports: {
              ".": {
                "ws-source": "./src/index.ts",
                types: "./dist/index.d.ts",
                default: "./dist/index.js",
              },
            },
          }),
          "packages/pkg-cond/tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              declaration: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              rootDir: "src",
              outDir: "dist",
            },
            include: ["src/**/*.ts"],
          }),
          "packages/pkg-cond/src/index.ts": [
            "export function condRelay<T>(value: T): T {",
            "  return value;",
            "}",
            "",
          ].join("\n"),

          "packages/pkg-map/package.json": JSON.stringify({
            name: "@ws/pkg-map",
            version: "1.0.0",
            type: "module",
            exports: {
              ".": {
                types: "./dist/index.d.ts",
                default: "./dist/index.js",
              },
            },
          }),
          "packages/pkg-map/tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              declaration: true,
              declarationMap: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              rootDir: "src",
              outDir: "dist",
            },
            include: ["src/**/*.ts"],
          }),
          "packages/pkg-map/src/index.ts": [
            "export function mapRelay<T>(value: T): T {",
            "  return value;",
            "}",
            "",
          ].join("\n"),

          "packages/pkg-priority/package.json": JSON.stringify({
            name: "@ws/pkg-priority",
            version: "1.0.0",
            type: "module",
            exports: {
              ".": {
                "ws-source": "./src/index.ts",
                types: "./dist/index.d.ts",
                default: "./dist/index.js",
              },
            },
          }),
          "packages/pkg-priority/tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              composite: true,
              declaration: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              rootDir: "src",
              outDir: "dist",
            },
            include: ["src/**/*.ts"],
          }),
          "packages/pkg-priority/src/index.ts": [
            "export function priorityRelay<T>(value: T): T {",
            "  return value;",
            "}",
            "",
          ].join("\n"),

          "packages/app/package.json": JSON.stringify({
            name: "@ws/app",
            version: "1.0.0",
            private: true,
            type: "module",
          }),
          "packages/app/tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              customConditions: ["ws-source"],
              noEmit: true,
            },
            references: [{ path: "../pkg-ref" }, { path: "../pkg-priority" }],
            include: ["src/**/*.ts"],
          }),
          "packages/app/src/local.ts": [
            "export function localHelper<T>(value: T): T {",
            "  return value;",
            "}",
            "",
          ].join("\n"),
          "packages/app/src/app.ts": [
            'import { refRelay } from "@ws/pkg-ref";',
            'import { condRelay } from "@ws/pkg-cond";',
            'import { mapRelay } from "@ws/pkg-map";',
            'import { priorityRelay } from "@ws/pkg-priority";',
            'import { localHelper } from "./local.js";',
            "",
            "class Token {",
            "  readonly id: number;",
            "  constructor(id: number) {",
            "    this.id = id;",
            "  }",
            "}",
            "",
            "export function run() {",
            "  const seed = new Token(1);",
            "  const a = localHelper(seed);",
            "  const b = refRelay(seed);",
            "  const c = condRelay(seed);",
            "  const d = mapRelay(seed);",
            "  const e = priorityRelay(seed);",
            "  return { a, b, c, d, e };",
            "}",
            "",
          ].join("\n"),
        },
        setup: async (host) => {
          await buildTsProject(host.rootDir, "packages/pkg-ref/tsconfig.json");
          await buildTsProject(host.rootDir, "packages/pkg-cond/tsconfig.json");
          await buildTsProject(host.rootDir, "packages/pkg-map/tsconfig.json");
          await buildTsProject(
            host.rootDir,
            "packages/pkg-priority/tsconfig.json",
          );
          linkNodeModulesPackage(
            host.rootDir,
            "@ws/pkg-ref",
            host.resolvePath("packages/pkg-ref"),
          );
          linkNodeModulesPackage(
            host.rootDir,
            "@ws/pkg-cond",
            host.resolvePath("packages/pkg-cond"),
          );
          linkNodeModulesPackage(
            host.rootDir,
            "@ws/pkg-map",
            host.resolvePath("packages/pkg-map"),
          );
          linkNodeModulesPackage(
            host.rootDir,
            "@ws/pkg-priority",
            host.resolvePath("packages/pkg-priority"),
          );
        },
      },
      async (host) => {
        await host.typecheck("packages/app/tsconfig.json");

        const session = await openProject({
          cwd: host.rootDir,
          projects: [
            "packages/app/tsconfig.json",
            "packages/pkg-map/tsconfig.json",
          ],
        });
        try {
          const ctx = session.project("packages/app/tsconfig.json");
          const calls = await ctx.findSyntax({
            files: ["packages/app/src/app.ts"],
            kinds: ["CallExpression"],
          });
          const callOf = (prefix: string) => {
            const found = calls.find((c) => c.text.startsWith(prefix));
            assert.ok(found, `Missing call ${prefix}`);
            return found;
          };

          const localSyn = callOf("localHelper(");
          const refSyn = callOf("refRelay(");
          const condSyn = callOf("condRelay(");
          const mapSyn = callOf("mapRelay(");
          const prioritySyn = callOf("priorityRelay(");

          const resolvedCalls = await ctx.resolveCalls([
            localSyn.ref,
            refSyn.ref,
            condSyn.ref,
            mapSyn.ref,
            prioritySyn.ref,
          ]);
          const callRefs = resolvedCalls.map((row) => {
            assert.ok(row.call);
            return row.call;
          });

          const impls = await ctx.resolveImplementations(callRefs);
          assert.equal(impls.length, 5);

          const [localImpl, refImpl, condImpl, mapImpl, priorityImpl] = impls;

          assert.equal(localImpl?.status, "resolved");
          assert.equal(
            localImpl.implementation.resolutionSource,
            "current-project",
          );
          assert.equal(
            localImpl.implementation.file,
            "packages/app/src/local.ts",
          );
          assert.equal(localImpl.implementation.declarationFile, null);

          assert.equal(refImpl?.status, "resolved");
          assert.equal(
            refImpl.implementation.resolutionSource,
            "project-reference",
          );
          assert.equal(
            refImpl.implementation.file,
            "packages/pkg-ref/src/index.ts",
          );
          assert.equal(refImpl.implementation.packageName, "@ws/pkg-ref");

          assert.equal(condImpl?.status, "resolved");
          assert.equal(
            condImpl.implementation.resolutionSource,
            "custom-conditions",
          );
          assert.equal(
            condImpl.implementation.file,
            "packages/pkg-cond/src/index.ts",
          );
          assert.equal(condImpl.implementation.packageName, "@ws/pkg-cond");

          assert.equal(mapImpl?.status, "resolved");
          assert.equal(
            mapImpl.implementation.resolutionSource,
            "declaration-map",
          );
          assert.equal(
            mapImpl.implementation.file,
            "packages/pkg-map/src/index.ts",
          );
          assert.equal(
            mapImpl.implementation.declarationFile,
            "packages/pkg-map/dist/index.d.ts",
          );
          assert.equal(mapImpl.implementation.packageName, "@ws/pkg-map");
          assert.equal(mapImpl.implementation.location.line, 1);

          const [mapDeclSyn] = await ctx.syntaxOf(
            [mapImpl.implementation.declaration],
            { select: { location: true } },
          );
          assert.equal(
            mapDeclSyn?.location?.file,
            "packages/pkg-map/src/index.ts",
          );

          // Priority order: project-reference wins over custom-conditions when both apply.
          assert.equal(priorityImpl?.status, "resolved");
          assert.equal(
            priorityImpl.implementation.resolutionSource,
            "project-reference",
          );
          assert.equal(
            priorityImpl.implementation.file,
            "packages/pkg-priority/src/index.ts",
          );

          // Core analyses follow the declarationMap-mapped first-party source body.
          const [mapCallee] = await ctx.calleesOf([callRefs[3]!]);
          assert.equal(
            mapCallee?.resolvedTargets[0]?.implementation?.file,
            "packages/pkg-map/src/index.ts",
          );
          assert.equal(
            mapCallee?.resolvedTargets[0]?.implementation?.resolutionSource,
            "declaration-map",
          );

          const varDecls = await ctx.findSyntax({
            files: ["packages/app/src/app.ts"],
            kinds: ["VariableDeclaration"],
          });
          const initOf = (name: string): SyntaxRef => {
            const decl = varDecls.find((d) => d.text.startsWith(`${name} =`));
            assert.ok(decl?.initializer, `Missing ${name}`);
            return decl.initializer;
          };
          const [seedInst, mapOutInst] = await ctx.resolveInstances([
            initOf("seed"),
            initOf("d"),
          ]);
          assert.ok(
            seedInst?.status === "proved" &&
              seedInst.instance !== null &&
              mapOutInst?.status === "proved" &&
              mapOutInst.instance !== null,
          );
          assert.equal(
            isSameInstance(seedInst.instance, mapOutInst.instance),
            true,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("reports an inconclusive gap with generic TypeScript configuration hints when a first-party package only resolves to .d.ts", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "first-party-dts-only-monorepo",
            private: true,
            type: "module",
          }),
          "packages/pkg-dts-only/package.json": JSON.stringify({
            name: "@ws/pkg-dts-only",
            version: "1.0.0",
            type: "module",
            exports: {
              ".": {
                types: "./dist/index.d.ts",
                default: "./dist/index.js",
              },
            },
          }),
          "packages/pkg-dts-only/tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              declaration: true,
              declarationMap: false,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              rootDir: "src",
              outDir: "dist",
            },
            include: ["src/**/*.ts"],
          }),
          "packages/pkg-dts-only/src/index.ts": [
            "export function dtsOnlyOp<T>(value: T): T {",
            "  return value;",
            "}",
            "",
          ].join("\n"),
          "packages/app/package.json": JSON.stringify({
            name: "@ws/app",
            version: "1.0.0",
            private: true,
            type: "module",
          }),
          "packages/app/tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              noEmit: true,
            },
            include: ["src/**/*.ts"],
          }),
          "packages/app/src/app.ts": [
            'import { dtsOnlyOp } from "@ws/pkg-dts-only";',
            "export function run(x: string): string {",
            "  return dtsOnlyOp(x);",
            "}",
            "",
          ].join("\n"),
        },
        setup: async (host) => {
          await buildTsProject(
            host.rootDir,
            "packages/pkg-dts-only/tsconfig.json",
          );
          linkNodeModulesPackage(
            host.rootDir,
            "@ws/pkg-dts-only",
            host.resolvePath("packages/pkg-dts-only"),
          );
        },
      },
      async (host) => {
        await host.typecheck("packages/app/tsconfig.json");

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "packages/app/tsconfig.json",
        });
        try {
          const ctx = session.project();
          const calls = await ctx.findSyntax({
            files: ["packages/app/src/app.ts"],
            kinds: ["CallExpression"],
          });
          const dtsCall = calls.find((c) => c.text.startsWith("dtsOnlyOp("));
          assert.ok(dtsCall);

          const [impl] = await ctx.resolveImplementations([dtsCall.ref]);
          assert.equal(impl?.status, "inconclusive");
          assert.equal(
            impl.declarationFile,
            "packages/pkg-dts-only/dist/index.d.ts",
          );
          assert.ok(impl.gaps.length > 0);
          assert.equal(impl.gaps[0]?.kind, "missing-source-file");

          const msg = impl.gaps[0]?.message ?? "";
          assert.match(msg, /project references/i);
          assert.match(msg, /customConditions/);
          assert.match(msg, /declarationMap/);
          assert.doesNotMatch(
            msg,
            /\btsrs\b|SemanticFacade|SnapshotState|project_collection|handleId/i,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("treats third-party node_modules packages and standard library declarations as external without resolving or analyzing their JS implementations", async () => {
    await withTempHost(
      {
        files: {
          "package.json": JSON.stringify({
            name: "third-party-external-host",
            private: true,
            type: "module",
          }),
          "tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              allowJs: true,
              maxNodeModuleJsDepth: 1,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              noEmit: true,
            },
            include: ["src/**/*.ts"],
          }),
          "src/app.ts": [
            'import { forwardEsm } from "third-party-esm";',
            'import { forwardCjs } from "third-party-cjs";',
            'import { forwardJs } from "third-party-js";',
            "",
            "export function run(): unknown {",
            '  const seed = { kind: "secret" };',
            "  const outEsm = forwardEsm(seed);",
            "  const outCjs = forwardCjs(seed);",
            "  const outJs = forwardJs(seed);",
            "  const mapped = [seed].map((item) => item);",
            "  return { outEsm, outCjs, outJs, mapped };",
            "}",
            "",
          ].join("\n"),
        },
        syntheticPackages: {
          "third-party-esm": {
            packageJson: {
              name: "third-party-esm",
              version: "1.0.0",
              type: "module",
              exports: {
                ".": {
                  types: "./dist/index.d.ts",
                  import: "./dist/index.mjs",
                },
              },
            },
            files: {
              "dist/index.d.ts":
                "export declare function forwardEsm<T>(value: T): T;\n",
              "dist/index.mjs":
                "export function forwardEsm(value) {\n  return value;\n}\n",
            },
          },
          "third-party-cjs": {
            packageJson: {
              name: "third-party-cjs",
              version: "1.0.0",
              main: "./dist/index.cjs",
              types: "./dist/index.d.ts",
            },
            files: {
              "dist/index.d.ts":
                "export declare function forwardCjs<T>(value: T): T;\n",
              "dist/index.cjs":
                "exports.forwardCjs = function forwardCjs(value) {\n  return value;\n};\n",
            },
          },
          "third-party-js": {
            packageJson: {
              name: "third-party-js",
              version: "1.0.0",
              type: "module",
              main: "./index.js",
            },
            files: {
              "index.js":
                "export function forwardJs(value) {\n  return value;\n}\n",
            },
          },
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
          const callOf = (prefix: string) => {
            const found = calls.find((c) => c.text.includes(prefix));
            assert.ok(found, `Missing call ${prefix}`);
            return found;
          };

          const esmCallSyn = callOf("forwardEsm(");
          const cjsCallSyn = callOf("forwardCjs(");
          const jsCallSyn = callOf("forwardJs(");
          const mapCallSyn = callOf(".map(");

          const resolvedCalls = await ctx.resolveCalls([
            esmCallSyn.ref,
            cjsCallSyn.ref,
            jsCallSyn.ref,
            mapCallSyn.ref,
          ]);
          const callRefs = resolvedCalls.map((row) => {
            assert.ok(row.call);
            return row.call;
          });

          const [esmImpl, cjsImpl, jsImpl, mapImpl] =
            await ctx.resolveImplementations(callRefs);

          assert.equal(esmImpl?.status, "external");
          assert.equal(esmImpl.packageName, "third-party-esm");
          assert.equal(
            esmImpl.declarationFile,
            "node_modules/third-party-esm/dist/index.d.ts",
          );

          assert.equal(cjsImpl?.status, "external");
          assert.equal(cjsImpl.packageName, "third-party-cjs");
          assert.equal(
            cjsImpl.declarationFile,
            "node_modules/third-party-cjs/dist/index.d.ts",
          );

          assert.equal(jsImpl?.status, "external");
          assert.equal(jsImpl.packageName, "third-party-js");

          assert.equal(mapImpl?.status, "external");
          assert.equal(mapImpl.packageName, null);

          // Third-party calls must not expose JS implementation bodies in calleesOf.
          const callees = await ctx.calleesOf(callRefs.slice(0, 3));
          for (const row of callees) {
            for (const target of row.resolvedTargets) {
              assert.equal(target.implementation ?? null, null);
              assert.deepEqual(target.returnExpressions, []);
            }
          }

          // Value propagation and instance identity must not trace through compiled node_modules JS.
          const varDecls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["VariableDeclaration"],
          });
          const initOf = (name: string): SyntaxRef => {
            const decl = varDecls.find((d) => d.text.startsWith(`${name} =`));
            assert.ok(decl?.initializer, `Missing ${name}`);
            return decl.initializer;
          };
          const declOf = async (name: string) => {
            const decl = varDecls.find((d) => d.text.startsWith(`${name} =`));
            assert.ok(decl, `Missing ${name}`);
            const [row] = await ctx.resolveDeclarations([decl.ref]);
            assert.ok(row?.declaration);
            return row.declaration;
          };

          const seedInit = initOf("seed");
          const outEsmDecl = await declOf("outEsm");
          const outCjsDecl = await declOf("outCjs");
          const outJsDecl = await declOf("outJs");

          const prop = await ctx.valuePropagation({
            from: seedInit,
            to: [outEsmDecl, outCjsDecl, outJsDecl],
          });
          assert.equal(prop.results.length, 3);
          for (const res of prop.results) {
            assert.equal(res.status, "inconclusive");
            assert.ok(res.status === "inconclusive" && res.gaps.length > 0);
          }

          const instances = await ctx.resolveInstances([
            initOf("outEsm"),
            initOf("outCjs"),
            initOf("outJs"),
          ]);
          for (const inst of instances) {
            assert.equal(inst.status, "inconclusive");
            assert.ok(inst.status === "inconclusive" && inst.gaps.length > 0);
          }
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("resolves same-scope ClassDeclaration, namespace/IIFE calls, typed object literals, and non-escaping callersOf completeness", async () => {
    await withTempHost(
      {
        files: {
          "tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
            },
            include: ["src/**/*.ts"],
          }),
          "src/event.ts": [
            "export class Emitter {",
            "  constructor(public readonly label: string) {}",
            "}",
            "",
          ].join("\n"),
          "src/path.ts": [
            "export interface IPath {",
            "  normalize(p: string): string;",
            "}",
            "export const posix: IPath = {",
            "  normalize(p: string): string {",
            "    return p.trim();",
            "  },",
            "};",
            "",
          ].join("\n"),
          "src/dom.ts": [
            "export const { getWindow } = (function () {",
            "  return {",
            "    getWindow(el: string): string {",
            "      return el;",
            "    },",
            "  };",
            "})();",
            "",
          ].join("\n"),
          "src/app.ts": [
            "import * as event from './event.js';",
            "import * as path from './path.js';",
            "import { posix } from './path.js';",
            "import * as dom from './dom.js';",
            "import { getWindow } from './dom.js';",
            "",
            "class WindowManager {",
            "  static readonly INSTANCE = new WindowManager();",
            "  constructor() {}",
            "}",
            "",
            "namespace NS {",
            "  export class Foo {",
            "    constructor(public readonly x: number) {}",
            "  }",
            "  export function helper(v: number): number {",
            "    return v + 1;",
            "  }",
            "}",
            "",
            "const ctors = { Emitter: event.Emitter };",
            "",
            "function localHelper(x: number): number {",
            "  return x * 2;",
            "}",
            "",
            "function escapedHelper(x: number): number {",
            "  return x + 3;",
            "}",
            "",
            "interface Disposable {",
            "  dispose(): void;",
            "}",
            "",
            "export function runAll(h: Disposable, cb: (fn: (n: number) => number) => void): void {",
            "  const wm = new WindowManager();",
            "  const em = new event.Emitter('a');",
            "  const em2 = new ctors.Emitter('b');",
            "  const nsFoo = new NS.Foo(1);",
            "  const nsVal = NS.helper(2);",
            "  const p1 = posix.normalize(' x ');",
            "  const p2 = path.posix.normalize(' y ');",
            "  const w1 = getWindow('el1');",
            "  const w2 = dom.getWindow('el2');",
            "  const lh = localHelper(3);",
            "  h.dispose();",
            "  cb(escapedHelper);",
            "  void [wm, em, em2, nsFoo, nsVal, p1, p2, w1, w2, lh];",
            "}",
            "",
            "export function invokeDynamic(cb: () => void): void {",
            "  cb();",
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
          const newExprs = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["NewExpression"],
          });
          const callExprs = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const fns = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["FunctionDeclaration"],
          });

          const newWmStatic = newExprs.find(
            (n) => n.text === "new WindowManager()" && n.location.line === 8,
          )!;
          const newWmLocal = newExprs.find(
            (n) => n.text === "new WindowManager()" && n.location.line > 10,
          )!;
          const newEvent = newExprs.find((n) =>
            n.text.startsWith("new event.Emitter("),
          )!;
          const newCtors = newExprs.find((n) =>
            n.text.startsWith("new ctors.Emitter("),
          )!;
          const newNsFoo = newExprs.find((n) =>
            n.text.startsWith("new NS.Foo("),
          )!;

          const nsHelperCall = callExprs.find((c) =>
            c.text.startsWith("NS.helper("),
          )!;
          const posixCall = callExprs.find((c) =>
            c.text.startsWith("posix.normalize("),
          )!;
          const pathPosixCall = callExprs.find((c) =>
            c.text.startsWith("path.posix.normalize("),
          )!;
          const winDirectCall = callExprs.find((c) =>
            c.text.startsWith("getWindow("),
          )!;
          const winNsCall = callExprs.find((c) =>
            c.text.startsWith("dom.getWindow("),
          )!;

          const resolvedCallRows = await ctx.resolveCalls([
            newWmStatic.ref,
            newWmLocal.ref,
            newEvent.ref,
            newCtors.ref,
            newNsFoo.ref,
            nsHelperCall.ref,
            posixCall.ref,
            pathPosixCall.ref,
            winDirectCall.ref,
            winNsCall.ref,
          ]);
          const callRefs = resolvedCallRows.map((row) => {
            assert.ok(row.call);
            return row.call;
          });

          const callees = await ctx.calleesOf(callRefs);
          assert.equal(callees.length, 10);
          for (const row of callees) {
            assert.equal(
              row.completeness,
              "complete",
              row.completeness === "incomplete"
                ? `Expected complete resolution, got gaps: ${JSON.stringify(row.gaps)}`
                : "Expected complete resolution",
            );
            assert.equal(row.resolvedTargets.length, 1);
          }

          assert.equal(
            callees[2]!.resolvedTargets[0]!.location?.file,
            "src/event.ts",
          );
          assert.equal(
            callees[3]!.resolvedTargets[0]!.location?.file,
            "src/event.ts",
          );
          assert.equal(callees[5]!.resolvedTargets[0]!.name, "helper");
          assert.equal(
            callees[6]!.resolvedTargets[0]!.location?.file,
            "src/path.ts",
          );
          assert.equal(
            callees[7]!.resolvedTargets[0]!.location?.file,
            "src/path.ts",
          );
          assert.equal(
            callees[8]!.resolvedTargets[0]!.location?.file,
            "src/dom.ts",
          );
          assert.equal(
            callees[9]!.resolvedTargets[0]!.location?.file,
            "src/dom.ts",
          );

          const [posixImpl] = await ctx.resolveImplementations([posixCall.ref]);
          assert.equal(posixImpl?.status, "resolved");
          if (posixImpl?.status === "resolved") {
            assert.equal(posixImpl.implementation.location.file, "src/path.ts");
          }

          const localHelperFn = fns.find((f) =>
            f.text.includes("function localHelper("),
          )!;
          const escapedHelperFn = fns.find((f) =>
            f.text.includes("function escapedHelper("),
          )!;
          const [localDeclRow, escapedDeclRow] = await ctx.resolveDeclarations([
            localHelperFn.ref,
            escapedHelperFn.ref,
          ]);
          assert.ok(localDeclRow?.declaration);
          assert.ok(escapedDeclRow?.declaration);
          const [localCallers, escapedCallers] = await ctx.callersOf([
            localDeclRow.declaration,
            escapedDeclRow.declaration,
          ]);
          assert.equal(localCallers?.completeness, "complete");
          assert.equal(localCallers?.callSites.length, 1);
          assert.equal(escapedCallers?.completeness, "incomplete");
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("completes without stack overflow on outer mutable cyclic property reassignment", async () => {
    await withTempHost(
      {
        files: {
          "tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
            },
            include: ["src/**/*.ts"],
          }),
          "src/tree.ts": [
            "interface TreeNode {",
            "  children: TreeNode[];",
            "  parent?: TreeNode;",
            "}",
            "",
            "let currentParent: TreeNode = { children: [], parent: undefined };",
            "",
            "export function stepUp(): void {",
            "  if (currentParent.parent) {",
            "    currentParent = currentParent.parent!;",
            "  }",
            "}",
            "",
            "export function addChild(n: TreeNode): void {",
            "  currentParent.children.push(n);",
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
            files: ["src/tree.ts"],
            kinds: ["CallExpression"],
          });
          const fns = await ctx.findSyntax({
            files: ["src/tree.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const idents = await ctx.findSyntax({
            files: ["src/tree.ts"],
            kinds: ["Identifier"],
          });

          const pushCallSyn = calls.find((c) =>
            c.text.includes("currentParent.children.push("),
          );
          assert.ok(pushCallSyn);
          const [resolvedPush] = await ctx.resolveCalls([pushCallSyn.ref]);
          assert.ok(resolvedPush?.call);

          const [pushCallees] = await ctx.calleesOf([resolvedPush.call]);
          assert.ok(pushCallees);
          assert.equal(pushCallees.completeness, "incomplete");
          assert.ok(pushCallees.gaps.length > 0);

          const addChildFn = fns.find((f) =>
            f.text.includes("function addChild("),
          );
          const stepUpFn = fns.find((f) => f.text.includes("function stepUp("));
          assert.ok(addChildFn);
          assert.ok(stepUpFn);
          const declRows = await ctx.resolveDeclarations([
            addChildFn.ref,
            stepUpFn.ref,
          ]);
          const declRefs = declRows.map((r) => {
            assert.ok(r.declaration);
            return r.declaration;
          });
          const callers = await ctx.callersOf(declRefs);
          assert.equal(callers.length, 2);

          const parentIdentInAddChild = idents.find(
            (id) => id.text === "currentParent" && id.location.line === 15,
          );
          assert.ok(parentIdentInAddChild);
          const [parentInst] = await ctx.resolveInstances([
            parentIdentInAddChild.ref,
          ]);
          assert.ok(parentInst);

          const effects = await ctx.callEffects({
            targets: [resolvedPush.call, ...declRefs],
          });
          assert.equal(effects.length, 3);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("terminates cleanly via depth and visited bounds on const self-referential call and property cycles", async () => {
    await withTempHost(
      {
        files: {
          "tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
            },
            include: ["src/**/*.ts"],
          }),
          "src/cycle.ts": [
            "function helper(): any {",
            "  return box.next();",
            "}",
            "const box: any = { next: helper().next };",
            "export function trigger() {",
            "  return box.next();",
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
            files: ["src/cycle.ts"],
            kinds: ["CallExpression"],
          });
          const triggerCallSyn = calls.find(
            (c) => c.text === "box.next()" && c.location.line === 6,
          );
          assert.ok(triggerCallSyn);

          const [resolvedTrigger] = await ctx.resolveCalls([
            triggerCallSyn.ref,
          ]);
          assert.ok(resolvedTrigger?.call);

          const [callees] = await ctx.calleesOf([resolvedTrigger.call]);
          assert.ok(callees);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("completes binary-recursive methods and deep closure-capturing helper chains in instanceCleanup under 500ms", async () => {
    await withTempHost(
      {
        files: {
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
          "src/cleanup-recursion.ts": [
            "class Disposable {",
            "  dispose(): void {}",
            "}",
            "class DisposableStore {",
            "  add<T extends Disposable>(item: T): T { return item; }",
            "  dispose(): void {}",
            "}",
            "class HelperWrapper {",
            "  constructor(readonly inner: HelperWrapper | null) {}",
            "}",
            "class Sash {",
            "  onPointerStart(depth: number, factory: HelperWrapper): void {",
            "    if (depth <= 0) { return; }",
            "    this.onPointerStart(depth - 1, new HelperWrapper(factory));",
            "    this.onPointerStart(depth - 1, new HelperWrapper(factory));",
            "  }",
            "}",
            "function invokeCb(cb: () => void): void { cb(); }",
            "export function runBinaryAndDeepClosure(): void {",
            "  const cleaned = new Disposable();",
            "  const leaked = new Disposable();",
            "  const rootStore = new DisposableStore();",
            "  const sash = new Sash();",
            "  sash.onPointerStart(12, new HelperWrapper(null));",
            "  const step1 = (): void => { step2(); };",
            "  const step2 = (): void => { step3(); };",
            "  const step3 = (): void => { step4(); };",
            "  const step4 = (): void => { step5(); };",
            "  const step5 = (): void => { step6(); };",
            "  const step6 = (): void => { step7(); };",
            "  const step7 = (): void => { step8(); };",
            "  const step8 = (): void => { step9(); };",
            "  const step9 = (): void => { step10(); };",
            "  const step10 = (): void => {",
            "    invokeCb(() => {",
            "      rootStore.add(cleaned);",
            "    });",
            "  };",
            "  step1();",
            "  rootStore.dispose();",
            "  void leaked;",
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
          summaries: {
            packs: [
              defineSummaries({
                id: "cleanup-recursion-pack",
                entries: [
                  {
                    match: {
                      module: "src/cleanup-recursion.ts",
                      memberPath: ["Disposable", "prototype", "dispose"],
                    },
                    effects: {
                      completeness: "complete",
                      resourceOps: [
                        { kind: "disposes-target", target: "receiver" },
                      ],
                    },
                  },
                  {
                    match: {
                      module: "src/cleanup-recursion.ts",
                      memberPath: ["DisposableStore", "prototype", "dispose"],
                    },
                    effects: {
                      completeness: "complete",
                      resourceOps: [
                        { kind: "disposes-target", target: "receiver" },
                      ],
                    },
                  },
                  {
                    match: {
                      module: "src/cleanup-recursion.ts",
                      memberPath: ["DisposableStore", "prototype", "add"],
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
              }),
            ],
          },
        });
        try {
          const ctx = session.project();
          const fns = await ctx.findSyntax({
            files: ["src/cleanup-recursion.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const runFn = fns.find((f) =>
            f.text.includes("function runBinaryAndDeepClosure"),
          );
          const news = await ctx.findSyntax({
            files: ["src/cleanup-recursion.ts"],
            kinds: ["NewExpression"],
          });
          const disps = news.filter((n) => n.text === "new Disposable()");
          assert.ok(runFn);
          assert.equal(disps.length, 2);

          const instRows = await ctx.resolveInstances(disps.map((d) => d.ref));
          const targets = instRows.map((r) => {
            assert.ok(r.status === "proved" && r.instance);
            return r.instance;
          });

          const startMs = Date.now();
          const [cleanup] = await ctx.instanceCleanup({
            scopes: [runFn.ref],
            targets,
          });
          const elapsedMs = Date.now() - startMs;

          assert.ok(
            elapsedMs < 500,
            `expected fast instanceCleanup on binary recursion + deep closure chain, took ${elapsedMs}ms`,
          );
          assert.equal(cleanup?.targetResults[0]?.status, "proved");
          assert.equal(
            cleanup?.targetResults[1]?.status,
            "disproved",
            `unexpected targetResults[1]: ${JSON.stringify(cleanup?.targetResults[1])}`,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("resolves cross-file destructured IIFE exports for both calleesOf and callersOf", async () => {
    await withTempHost(
      {
        files: {
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
          "src/dom.ts": [
            "export const { getWindow, getWindowById } = (function () {",
            "  return {",
            "    getWindow() {",
            "      return 1;",
            "    },",
            "    getWindowById() {",
            "      return 2;",
            "    },",
            "  };",
            "})();",
            "",
          ].join("\n"),
          "src/consumer.ts": [
            "import { getWindow } from './dom.js';",
            "import * as ns from './dom.js';",
            "",
            "export function runDomCalls(): number {",
            "  return getWindow() + ns.getWindowById();",
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
            files: ["src/consumer.ts"],
            kinds: ["CallExpression"],
          });
          const directCall = calls.find((c) => c.text === "getWindow()");
          const nsCall = calls.find((c) => c.text === "ns.getWindowById()");
          assert.ok(directCall && nsCall);

          const callRows = await ctx.resolveCalls([directCall.ref, nsCall.ref]);
          const callRefs = callRows.map((r) => {
            assert.ok(r.call);
            return r.call;
          });
          const callees = await ctx.calleesOf(callRefs);
          assert.equal(callees.length, 2);
          for (const row of callees) {
            assert.equal(row.completeness, "complete");
            assert.equal(row.resolvedTargets.length, 1);
            assert.equal(row.resolvedTargets[0]?.location?.file, "src/dom.ts");
            assert.ok((row.resolvedTargets[0]?.location?.line ?? 0) >= 3);
          }

          const domMethods = await ctx.findSyntax({
            files: ["src/dom.ts"],
            kinds: ["MethodDeclaration"],
          });
          const getWinMethod = domMethods.find((m) =>
            m.text.startsWith("getWindow()"),
          );
          const getWinByIdMethod = domMethods.find((m) =>
            m.text.startsWith("getWindowById()"),
          );
          assert.ok(getWinMethod && getWinByIdMethod);

          const declRows = await ctx.resolveDeclarations([
            getWinMethod.ref,
            getWinByIdMethod.ref,
          ]);
          const declRefs = declRows.map((r) => {
            assert.ok(r.declaration);
            return r.declaration;
          });
          const callersRows = await ctx.callersOf(declRefs);
          assert.equal(callersRows.length, 2);
          assert.equal(callersRows[0]?.completeness, "complete");
          assert.equal(callersRows[0]?.callSites.length, 1);
          assert.equal(callersRows[1]?.completeness, "complete");
          assert.equal(callersRows[1]?.callSites.length, 1);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("lowers class member and parameter decorators for callersOf and calleesOf", async () => {
    await withTempHost(
      {
        files: {
          "tsconfig.json": JSON.stringify({
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              experimentalDecorators: true,
              noEmit: true,
            },
            include: ["src/**/*.ts"],
          }),
          "src/decorators.ts": [
            "function throttle(_delay: number) {",
            "  return function (_target: object, _key: string, _desc: PropertyDescriptor): void {};",
            "}",
            "function inject(_token: string) {",
            "  return function (_target: object, _key: string | symbol | undefined, _index: number): void {};",
            "}",
            "export class Service {",
            "  @throttle(100)",
            "  report(@inject('x') p: number): number {",
            "    return p;",
            "  }",
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
          const fns = await ctx.findSyntax({
            files: ["src/decorators.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const throttleFn = fns.find((f) =>
            f.text.includes("function throttle("),
          );
          const injectFn = fns.find((f) => f.text.includes("function inject("));
          assert.ok(throttleFn && injectFn);

          const [throttleDecl, injectDecl] = await ctx.resolveDeclarations([
            throttleFn.ref,
            injectFn.ref,
          ]);
          assert.ok(throttleDecl?.declaration && injectDecl?.declaration);

          const [throttleCallers, injectCallers] = await ctx.callersOf([
            throttleDecl.declaration,
            injectDecl.declaration,
          ]);
          assert.equal(throttleCallers?.completeness, "complete");
          assert.equal(throttleCallers?.callSites.length, 1);
          assert.equal(injectCallers?.completeness, "complete");
          assert.equal(injectCallers?.callSites.length, 1);

          const calls = await ctx.findSyntax({
            files: ["src/decorators.ts"],
            kinds: ["CallExpression"],
          });
          const throttleCall = calls.find((c) => c.text === "throttle(100)");
          const injectCall = calls.find((c) => c.text === "inject('x')");
          assert.ok(throttleCall && injectCall);

          const [resolvedThrottleCall, resolvedInjectCall] =
            await ctx.resolveCalls([throttleCall.ref, injectCall.ref]);
          assert.ok(resolvedThrottleCall?.call && resolvedInjectCall?.call);

          const [throttleCallees, injectCallees] = await ctx.calleesOf([
            resolvedThrottleCall.call,
            resolvedInjectCall.call,
          ]);
          assert.equal(throttleCallees?.completeness, "complete");
          assert.equal(throttleCallees?.resolvedTargets.length, 1);
          assert.equal(throttleCallees?.resolvedTargets[0]?.name, "throttle");
          assert.equal(injectCallees?.completeness, "complete");
          assert.equal(injectCallees?.resolvedTargets.length, 1);
          assert.equal(injectCallees?.resolvedTargets[0]?.name, "inject");
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("resolves new Promise executor via builtin stdlib summary in calleesOf and callEffects", async () => {
    await withTempHost(
      {
        files: {
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
          "src/promise-exec.ts": [
            "const state = { count: 0 };",
            "function helper(): void {",
            "  state.count += 1;",
            "}",
            "export function runPromise(): Promise<number> {",
            "  return new Promise((resolve) => {",
            "    helper();",
            "    resolve(1);",
            "  });",
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
          const newExprs = await ctx.findSyntax({
            files: ["src/promise-exec.ts"],
            kinds: ["NewExpression"],
          });
          const newPromiseSyn = newExprs.find((n) =>
            n.text.startsWith("new Promise("),
          );
          assert.ok(newPromiseSyn);

          const [resolvedCall] = await ctx.resolveCalls([newPromiseSyn.ref]);
          assert.ok(resolvedCall?.call);

          const [callees] = await ctx.calleesOf([resolvedCall.call]);
          assert.ok(callees);
          assert.equal(
            callees.completeness,
            "complete",
            `expected complete calleesOf for new Promise(...), got ${JSON.stringify(callees)}`,
          );
          assert.ok(
            callees.resolvedTargets.some(
              (t) => t.location?.file === "src/promise-exec.ts",
            ),
            `expected executor callback target in resolvedTargets, got ${JSON.stringify(callees.resolvedTargets)}`,
          );

          const fns = await ctx.findSyntax({
            files: ["src/promise-exec.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const runFn = fns.find((f) =>
            f.text.includes("function runPromise("),
          );
          assert.ok(runFn);

          const [effects] = await ctx.callEffects({
            targets: [runFn.ref],
            followCallbacks: true,
            includeWrites: true,
          });
          assert.ok(effects);
          assert.ok(
            effects.fieldWrites.some((w) => w.propertyName === "count"),
            `expected callEffects to observe write to 'count' inside Promise executor, got ${JSON.stringify(effects.fieldWrites)}`,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("marks overloaded local function escaping via IIFE object return as incomplete in callersOf", async () => {
    await withTempHost(
      {
        files: {
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
          "src/overload-escape.ts": [
            "export const { getWindowById } = (function () {",
            "  function getWindowById(id: number): number;",
            "  function getWindowById(id?: number): number {",
            "    return id ?? 0;",
            "  }",
            "  return { getWindowById };",
            "})();",
            "export function invokeDynamic(cb: (id?: number) => number): number {",
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
          const fns = await ctx.findSyntax({
            files: ["src/overload-escape.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const implFn = fns.find((f) => f.text.includes("return id ?? 0"));
          assert.ok(implFn);

          const [declRow] = await ctx.resolveDeclarations([implFn.ref]);
          assert.ok(declRow?.declaration);

          const [callers] = await ctx.callersOf([declRow.declaration]);
          assert.ok(callers);
          assert.equal(
            callers.completeness,
            "incomplete",
            `expected incomplete callersOf for escaped overloaded local function, got ${JSON.stringify(callers)}`,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("completes static self-recursive function traversal in callEffects without hitting max depth", async () => {
    await withTempHost(
      {
        files: {
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
          "src/static-rec.ts": [
            "const state = { count: 0 };",
            "export function deepClone(node: { next?: any } | undefined): any {",
            "  state.count += 1;",
            "  return node ? deepClone(node.next) : undefined;",
            "}",
            "export function entry(n: any) {",
            "  return deepClone(n);",
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
          const fns = await ctx.findSyntax({
            files: ["src/static-rec.ts"],
            kinds: ["FunctionDeclaration"],
          });
          const entryFn = fns.find((f) => f.text.includes("function entry("));
          assert.ok(entryFn);

          const [effects] = await ctx.callEffects({
            targets: [entryFn.ref],
            includeWrites: true,
            maxDepth: 4,
          });
          assert.ok(effects);
          assert.equal(
            effects.completeness,
            "complete",
            `expected complete callEffects for static self-recursion, got ${JSON.stringify(effects)}`,
          );
          assert.ok(
            effects.fieldWrites.some((w) => w.propertyName === "count"),
          );
        } finally {
          await session.close();
        }
      },
    );
  });
});
