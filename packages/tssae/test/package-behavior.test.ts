import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { promisify } from "node:util";

import { linkNodeModulesPackage, withTempHost } from "@tssae/test-hosts";
import { isSameInstance, openProject, type SyntaxRef } from "tssae";

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
          assert.ok(seedInst?.instance && mapOutInst?.instance);
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
            assert.equal(res.reachable, false);
            assert.ok(res.gaps.length > 0);
          }

          const instances = await ctx.resolveInstances([
            initOf("outEsm"),
            initOf("outCjs"),
            initOf("outJs"),
          ]);
          for (const inst of instances) {
            assert.equal(inst.instance, null);
            assert.ok(inst.gaps.length > 0);
          }
        } finally {
          await session.close();
        }
      },
    );
  });
});
