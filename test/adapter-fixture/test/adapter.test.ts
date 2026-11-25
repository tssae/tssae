import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { withTempHost, type SyntheticPackageSpec } from "@tssae/test-hosts";
import {
  TssaeError,
  defineFactType,
  openProject,
  type FactResult,
} from "tssae";

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
        ".": "./dist/bridge.js",
        "./tssae-native": "./dist/tssae-native.json",
        "./native/linux-x64": "./dist/native/adapter.so",
      },
    },
    files: {
      "dist/bridge.js": moduleSource,
      "dist/native/adapter.so": readFileSync(nativeFile),
      "dist/tssae-native.json": JSON.stringify({
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
        const session = await openProject({
          cwd: host.rootDir,
          adapterResolutionRoot: fixtureRoot,
          tsconfig: "tsconfig.json",
          adapters: ["@tssae/adapter-fixture"],
        });

        try {
          facts = await session.project().facts(FixtureCallSourceFact);
          assert.equal(facts.length, 1);
          assert.deepEqual(facts[0]?.value, {
            text: "target(42)",
            input: "first-input",
            invocation: 1,
          });
          assert.equal(facts[0]?.evidence[0]?.at?.file, "src/app.ts");
          assert.equal(session.additionalInputs.length, 1);
          assert.equal(
            session.additionalInputs[0]?.file,
            "fixture.inputs.json",
          );
          assert.deepEqual(session.additionalInputs[0]?.sources, [
            { kind: "adapter", id: "third-party-fixture" },
          ]);

          host.writeText("fixture.inputs.json", "second-input");
          host.replaceText("src/app.ts", "target(42)", "target(43)");
          const updated = await session.update({ changed: ["src/app.ts"] });
          facts = await session.project().facts(FixtureCallSourceFact);
          assert.equal(facts.length, 1);
          assert.deepEqual(facts[0]?.value, {
            text: "target(43)",
            input: "second-input",
            invocation: 2,
          });
          assert.equal(facts[0]?.evidence[0]?.at?.file, "src/app.ts");
          assert.equal(facts[0]?.evidence[0]?.at?.line, 2);
          assert.equal(facts[0]?.evidence[0]?.at?.column, 10);
          assert.equal(updated.additionalInputs.length, 1);
          assert.equal(
            updated.additionalInputs[0]?.file,
            "fixture.inputs.json",
          );
          assert.deepEqual(updated.additionalInputs[0]?.sources, [
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
      "dist/native/libtssae_test_adapter.so",
    );
    const badAbiLibrary = join(
      fixtureRoot,
      "native/target/release/libtssae_bad_abi.so",
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

        await assert.rejects(
          openProject({
            cwd: host.rootDir,
            adapterResolutionRoot: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters: [invalidBridgeName],
          }),
          (error: unknown) =>
            error instanceof TssaeError &&
            error.message.includes(invalidBridgeName) &&
            error.message.includes("invalid JavaScript adapter bridge"),
        );

        await assert.rejects(
          openProject({
            cwd: host.rootDir,
            adapterResolutionRoot: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters: [abiMismatchName],
          }),
          (error: unknown) =>
            error instanceof TssaeError &&
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
        const session = await openProject({
          cwd: host.rootDir,
          adapterResolutionRoot: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [unsupportedPackageName],
        });
        try {
          const context = session.project();
          await assert.rejects(
            context.facts(unsupportedFact),
            (error: unknown) =>
              error instanceof TssaeError &&
              error.code === "NATIVE_ADAPTER_EXTRACT" &&
              error.message.includes(unsupportedPackageName) &&
              error.message.includes(unsupportedFact.id) &&
              error.message.includes("does not provide fact type"),
          );
          assert.equal((await context.facts(FixtureCallSourceFact)).length, 1);
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

        const session = await openProject({
          cwd: host.rootDir,
          adapterResolutionRoot: fixtureRoot,
          tsconfig: "tsconfig.json",
          adapters: ["@tssae/adapter-fixture"],
        });
        try {
          const context = session.project();
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
        } finally {
          await session.close();
        }
      },
    );
  });
});
