import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  VeruleExecutionError,
  defineConfig,
  defineFactType,
  defineRule,
  openSession,
  type FactResult,
} from "@verule/sdk";
import { withTempHost, type SyntheticPackageSpec } from "@verule/test-hosts";

import {
  FixtureCallSourceFact,
  FixtureSourceSpecFact,
  type FixtureCallSourceValue,
  type FixtureSourceSpecValue,
} from "../src/index.ts";

const fixtureRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function syntheticAdapterPackage(
  packageName: string,
  adapterId: string,
  moduleSource: string,
  nativeFile: string,
): SyntheticPackageSpec {
  return {
    packageJson: {
      name: packageName,
      type: "module",
      exports: {
        "./verule-adapter": "./dist/bridge.js",
        "./verule-native": "./dist/verule-native.json",
        "./native/linux-x64": "./dist/native/adapter.so",
      },
    },
    files: {
      "dist/bridge.js": moduleSource,
      "dist/native/adapter.so": readFileSync(nativeFile),
      "dist/verule-native.json": JSON.stringify({
        id: adapterId,
        "linux-x64": "./native/linux-x64",
      }),
    },
  };
}

void describe("independent native adapter package", () => {
  void it("loads the installed ESM export and library, reuses facts, tracks inputs, and refreshes on update", async () => {
    await withTempHost(
      {
        host: "adapters/fixture-basic",
        files: {
          "fixture.inputs.json": "first-input",
        },
      },
      async (host) => {
        await host.typecheck();

        let facts: readonly FactResult<"call", FixtureCallSourceValue>[] = [];
        const rule = defineRule({
          id: "fixture-native-fact",
          run: async (context) => {
            facts = await context.facts(FixtureCallSourceFact);
          },
        });
        const session = await openSession({
          cwd: host.rootDir,
          adapterResolutionRoot: fixtureRoot,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: ["@verule/adapter-fixture"],
            rules: [rule],
          }),
        });

        try {
          const report = await session.check();
          assert.equal(report.status, "clean");
          assert.equal(facts.length, 1);
          assert.deepEqual(facts[0]?.value, {
            text: "target(42)",
            input: "first-input",
            invocation: 1,
          });
          assert.equal(facts[0]?.evidence[0]?.at?.file, "src/app.ts");
          assert.equal(report.additionalInputs.length, 1);
          assert.equal(report.additionalInputs[0]?.file, "fixture.inputs.json");
          assert.deepEqual(report.additionalInputs[0]?.sources, [
            { kind: "adapter", id: "third-party-fixture" },
          ]);

          host.writeText("fixture.inputs.json", "second-input");
          host.replaceText("src/app.ts", "target(42)", "target(43)");
          await session.update({ changed: ["src/app.ts"] });
          facts = [];
          const refreshedReport = await session.check();
          assert.equal(refreshedReport.status, "clean");
          assert.equal(facts.length, 1);
          assert.deepEqual(facts[0]?.value, {
            text: "target(43)",
            input: "second-input",
            invocation: 2,
          });
          assert.equal(facts[0]?.evidence[0]?.at?.file, "src/app.ts");
          assert.equal(facts[0]?.evidence[0]?.at?.line, 2);
          assert.equal(facts[0]?.evidence[0]?.at?.column, 10);
          assert.equal(refreshedReport.additionalInputs.length, 1);
          assert.equal(
            refreshedReport.additionalInputs[0]?.file,
            "fixture.inputs.json",
          );
          assert.deepEqual(refreshedReport.additionalInputs[0]?.sources, [
            { kind: "adapter", id: "third-party-fixture" },
          ]);
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("reports configured package names for invalid bridges and ABI mismatches", async () => {
    const goodLibrary = join(
      fixtureRoot,
      "dist/native/libverule_test_adapter.so",
    );
    const badAbiLibrary = join(
      fixtureRoot,
      "native/target/release/libverule_bad_abi.so",
    );
    const invalidBridgeName = "fixture-invalid-bridge";
    const abiMismatchName = "fixture-abi-mismatch";
    const unsupportedPackageName = "fixture-native-capability-mismatch";

    await withTempHost(
      {
        host: "adapters/fixture-basic",
        files: {
          "fixture.inputs.json": "first-input",
        },
        syntheticPackages: {
          [invalidBridgeName]: syntheticAdapterPackage(
            invalidBridgeName,
            "invalid-bridge",
            `export default { id: "invalid-bridge", provides: [{ factType: {}, extract() {} }] };`,
            goodLibrary,
          ),
          [abiMismatchName]: syntheticAdapterPackage(
            abiMismatchName,
            "abi-mismatch",
            `export default { id: "abi-mismatch", provides: [{ factType: { id: "fixture.call-source" }, extract() { return []; } }] };`,
            badAbiLibrary,
          ),
          [unsupportedPackageName]: syntheticAdapterPackage(
            unsupportedPackageName,
            "third-party-fixture",
            `
const nativeProvider = (factTypeId) => ({
  factType: { id: factTypeId, subjectKind: "call" },
  extract(ctx, request) {
    return ctx.extractEngineFacts({
      factTypeId,
      providerId: "third-party-fixture",
      ...(request.targets !== undefined ? { targets: request.targets } : {}),
      ...(request.condition !== undefined ? { condition: request.condition } : {}),
    });
  },
});
export default {
  id: "third-party-fixture",
  provides: [
    nativeProvider("fixture.native-capability-missing"),
    nativeProvider("fixture.call-source"),
  ],
};
`,
            goodLibrary,
          ),
        },
      },
      async (host) => {
        await host.typecheck();
        const abiPackagePath = host.resolvePath(
          join("node_modules", abiMismatchName),
        );

        const configFor = (packageName: string) =>
          defineConfig({ tsconfig: "tsconfig.json", adapters: [packageName] });
        await assert.rejects(
          openSession({
            cwd: host.rootDir,
            adapterResolutionRoot: host.rootDir,
            config: configFor(invalidBridgeName),
          }),
          (error: unknown) =>
            error instanceof VeruleExecutionError &&
            error.message.includes(invalidBridgeName) &&
            error.message.includes("invalid JavaScript adapter bridge"),
        );

        await assert.rejects(
          openSession({
            cwd: host.rootDir,
            adapterResolutionRoot: host.rootDir,
            config: configFor(abiMismatchName),
          }),
          (error: unknown) =>
            error instanceof VeruleExecutionError &&
            error.code === "NATIVE_ADAPTER_LOAD" &&
            error.message.includes(abiMismatchName) &&
            error.message.includes("adapter ABI version 2 is unsupported") &&
            error.message.includes(
              resolve(abiPackagePath, "dist/native/adapter.so"),
            ),
        );

        const unsupportedFact = defineFactType<
          "call",
          undefined,
          { readonly unavailable: true }
        >({
          id: "fixture.native-capability-missing",
          subjectKind: "call",
        });
        const packageCapabilityRule = defineRule({
          id: "fixture-native-capability-mismatch",
          run: async (context) => {
            await assert.rejects(
              context.facts(unsupportedFact),
              (error: unknown) =>
                error instanceof VeruleExecutionError &&
                error.code === "NATIVE_ADAPTER_EXTRACT" &&
                error.message.includes(unsupportedPackageName) &&
                error.message.includes(unsupportedFact.id) &&
                error.message.includes("does not provide fact type"),
            );
            assert.equal(
              (await context.facts(FixtureCallSourceFact)).length,
              1,
            );
          },
        });
        const session = await openSession({
          cwd: host.rootDir,
          adapterResolutionRoot: host.rootDir,
          config: configFor(unsupportedPackageName),
        });
        try {
          const report = await session.check([packageCapabilityRule]);
          assert.equal(report.status, "clean");
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("extracts fixture.source-spec facts on declarations and targeted references", async () => {
    await withTempHost(
      {
        hosts: ["adapters/fixture-basic", "adapters/fixture-source-spec"],
        files: {
          "fixture.inputs.json": "first-input",
        },
      },
      async (host) => {
        await host.typecheck();

        let allSpecs: readonly FactResult<
          "syntax" | "declaration" | "call",
          FixtureSourceSpecValue
        >[] = [];
        let targetedSpecs: readonly FactResult<
          "syntax" | "declaration" | "call",
          FixtureSourceSpecValue
        >[] = [];

        const specRule = defineRule({
          id: "fixture-source-spec-rule",
          run: async (context) => {
            allSpecs = await context.facts(FixtureSourceSpecFact);
            const decls = await context.findSyntax({
              files: ["src/source.ts"],
              kinds: ["VariableDeclaration"],
            });
            const orderDecl = decls.find((d) => d.text.startsWith("orderSpec"));
            assert.ok(orderDecl);
            targetedSpecs = await context.facts(FixtureSourceSpecFact, {
              targets: [orderDecl.ref],
            });
          },
        });

        const session = await openSession({
          cwd: host.rootDir,
          adapterResolutionRoot: fixtureRoot,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: ["@verule/adapter-fixture"],
            rules: [specRule],
          }),
        });
        try {
          const report = await session.check();
          assert.equal(report.status, "clean");
          assert.equal(allSpecs.length, 1);
          assert.deepEqual(allSpecs[0]?.value, {
            sourceName: "order",
            fields: { id: "required", note: "optional" },
            requiredFields: ["id"],
            optionalFields: ["note"],
            defaultedFields: [],
            unknownFields: ["extra"],
            hasDynamicFields: true,
          });
          assert.equal(allSpecs[0]?.gaps.length, 1);
          assert.equal(allSpecs[0]?.gaps[0]?.kind, "unresolved-fixture-field");
          assert.equal(targetedSpecs.length, 1);
          assert.deepEqual(targetedSpecs[0]?.value, allSpecs[0]?.value);

          const explained = await session.explain({
            ruleId: "fixture-source-spec-rule",
            at: "src/source.ts:11:14",
          });
          assert.equal(explained.explanation.status, "blocked_by_gap");
          assert.equal(explained.explanation.gaps.length, 1);
          assert.equal(explained.explanation.localGaps.length, 1);
          assert.equal(
            explained.explanation.localGaps[0]?.status,
            "unresolved",
          );
          assert.equal(explained.explanation.localGaps[0]?.field, "extra");
          assert.equal(
            explained.explanation.localGaps[0]?.providerId,
            "third-party-fixture",
          );
        } finally {
          await session.close();
        }
      },
    );
  });
});
