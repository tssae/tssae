import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import {
  buildExternalRustAdapter,
  packAdapterApiCrate,
  packExternalAdapterPackage,
  packWorkspaceArtifacts,
  withAcceptanceHost,
  type PackedArtifactBundle,
} from "@tssae/acceptance";

const adapterNpmName = "@acme/external-rust-adapter";
const adapterId = "acme-external";
const factTypeId = "acme.external.signal";

const consumerTsconfig = JSON.stringify(
  {
    compilerOptions: {
      strict: true,
      exactOptionalPropertyTypes: true,
      noUncheckedIndexedAccess: true,
      target: "ES2024",
      module: "NodeNext",
      moduleResolution: "NodeNext",
      erasableSyntaxOnly: true,
      rewriteRelativeImportExtensions: true,
      types: ["node"],
      noEmit: true,
    },
    include: ["consumer.ts"],
  },
  null,
  2,
);

const hostProjectTsconfig = JSON.stringify(
  {
    compilerOptions: {
      strict: true,
      target: "ES2024",
      module: "NodeNext",
      moduleResolution: "NodeNext",
      noEmit: true,
    },
    include: ["src/**/*.ts"],
  },
  null,
  2,
);

const hostAppSource = `declare function externalSignal(name: string): number;

export function run(): number {
  return externalSignal("order-created");
}
`;

const consumerSource = `import assert from "node:assert/strict";
import { copyFileSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

import { ExternalSignalFact, externalAdapterPackage } from "@acme/external-rust-adapter";
import { TssaeError, defineFactType, openProject } from "tssae";

assert.equal(externalAdapterPackage, "${adapterNpmName}");

const session = await openProject({
  cwd: process.cwd(),
  tsconfig: "tsconfig.json",
  adapters: [externalAdapterPackage],
});

try {
  assert.deepEqual(
    session.nativeAdapters.map((item) => ({
      packageName: item.packageName,
      providerId: item.providerId,
      status: item.status,
    })),
    [
      {
        packageName: externalAdapterPackage,
        providerId: "${adapterId}",
        status: "loaded",
      },
    ],
  );

  const project = session.project();
  const facts = await project.facts(ExternalSignalFact);
  assert.equal(facts.length, 1);
  const first = facts[0];
  assert.ok(first);
  assert.deepEqual(first.providers, ["${adapterId}"]);
  assert.deepEqual(first.value, {
    callText: 'externalSignal("order-created")',
    signalArg: '"order-created"',
    adapterId: "${adapterId}",
  });
  assert.equal(first.evidence[0]?.kind, "source");
  assert.equal(
    first.evidence[0]?.message,
    "External Rust cdylib adapter extracted signal call.",
  );
  assert.equal(first.evidence[0]?.at?.file, "src/app.ts");
  assert.equal(first.evidence[0]?.at?.line, 4);

  // Verify JS bridge declaring a fact type missing from native manifest fails with NATIVE_ADAPTER_EXTRACT.
  const unsupportedFact = defineFactType<"call", undefined, { readonly ok: boolean }>({
    id: "acme.external.undeclared-native",
    subjectKind: "call",
  });
  await assert.rejects(
    project.facts(unsupportedFact),
    (error: unknown) =>
      error instanceof TssaeError &&
      error.code === "NATIVE_ADAPTER_EXTRACT" &&
      error.message.includes(externalAdapterPackage) &&
      error.message.includes("acme.external.undeclared-native") &&
      error.message.includes("does not provide fact type"),
  );

  process.stdout.write(
    JSON.stringify({
      providers: first.providers,
      value: first.value,
      evidenceFile: first.evidence[0]?.at?.file,
      evidenceLine: first.evidence[0]?.at?.line,
    }) + "\\n",
  );
} finally {
  await session.close();
}

// Verify uninstalled adapter specifier fails with INVALID_CONFIG naming the package.
await assert.rejects(
  openProject({
    cwd: process.cwd(),
    tsconfig: "tsconfig.json",
    adapters: ["@acme/missing-adapter"],
  }),
  (error: unknown) =>
    error instanceof TssaeError &&
    error.code === "INVALID_CONFIG" &&
    error.message.includes("@acme/missing-adapter"),
);

// Verify invalid .so variants (ID mismatch, wrong ABI, missing tssae_adapter_v1 / tssae_adapter_manifest_v1)
// report NATIVE_ADAPTER_LOAD naming the configured package and reason.
const hostRequire = createRequire(join(process.cwd(), "package.json"));
const installedSoPath = hostRequire.resolve("${adapterNpmName}/native/linux-x64");
const originalSo = readFileSync(installedSoPath);

try {
  // 1. Native library manifest id mismatch
  copyFileSync(join(process.cwd(), "variants", "other-id.so"), installedSoPath);
  await assert.rejects(
    openProject({
      cwd: process.cwd(),
      tsconfig: "tsconfig.json",
      adapters: [externalAdapterPackage],
    }),
    (error: unknown) =>
      error instanceof TssaeError &&
      error.code === "NATIVE_ADAPTER_LOAD" &&
      error.message.includes(externalAdapterPackage) &&
      error.message.includes("${adapterId}") &&
      error.message.includes('manifest declares adapter id "other-external-id"'),
  );

  // 2. ABI version mismatch
  copyFileSync(join(process.cwd(), "variants", "wrong-abi.so"), installedSoPath);
  await assert.rejects(
    openProject({
      cwd: process.cwd(),
      tsconfig: "tsconfig.json",
      adapters: [externalAdapterPackage],
    }),
    (error: unknown) =>
      error instanceof TssaeError &&
      error.code === "NATIVE_ADAPTER_LOAD" &&
      error.message.includes(externalAdapterPackage) &&
      error.message.includes("adapter ABI version 2 is unsupported"),
  );

  // 3. Missing tssae_adapter_v1 entry symbol
  copyFileSync(join(process.cwd(), "variants", "missing-entry.so"), installedSoPath);
  await assert.rejects(
    openProject({
      cwd: process.cwd(),
      tsconfig: "tsconfig.json",
      adapters: [externalAdapterPackage],
    }),
    (error: unknown) =>
      error instanceof TssaeError &&
      error.code === "NATIVE_ADAPTER_LOAD" &&
      error.message.includes(externalAdapterPackage) &&
      error.message.includes("tssae_adapter_v1"),
  );

  // 4. Missing tssae_adapter_manifest_v1 symbol
  copyFileSync(join(process.cwd(), "variants", "missing-manifest.so"), installedSoPath);
  await assert.rejects(
    openProject({
      cwd: process.cwd(),
      tsconfig: "tsconfig.json",
      adapters: [externalAdapterPackage],
    }),
    (error: unknown) =>
      error instanceof TssaeError &&
      error.code === "NATIVE_ADAPTER_LOAD" &&
      error.message.includes(externalAdapterPackage) &&
      error.message.includes("tssae_adapter_manifest_v1"),
  );
} finally {
  writeFileSync(installedSoPath, originalSo);
}
`;

void test("compiles external Rust cdylib from packed .crate, loads it in an isolated host, and diagnoses invalid extensions", async (t) => {
  const packedCrate = await packAdapterApiCrate();
  const validRust = await buildExternalRustAdapter({
    extractedCrateDir: packedCrate.extractedDir,
    adapterId,
    factTypeId,
    crateName: "acme_external_adapter",
    variant: "valid",
  });
  const otherIdRust = await buildExternalRustAdapter({
    extractedCrateDir: packedCrate.extractedDir,
    adapterId: "other-external-id",
    factTypeId,
    crateName: "acme_other_id_adapter",
    variant: "valid",
  });
  const wrongAbiRust = await buildExternalRustAdapter({
    extractedCrateDir: packedCrate.extractedDir,
    adapterId,
    factTypeId,
    crateName: "acme_wrong_abi_adapter",
    variant: "wrong-abi",
  });
  const missingEntryRust = await buildExternalRustAdapter({
    extractedCrateDir: packedCrate.extractedDir,
    adapterId,
    factTypeId,
    crateName: "acme_missing_entry_adapter",
    variant: "missing-entry-symbol",
  });
  const missingManifestRust = await buildExternalRustAdapter({
    extractedCrateDir: packedCrate.extractedDir,
    adapterId,
    factTypeId,
    crateName: "acme_missing_manifest_adapter",
    variant: "missing-manifest-symbol",
  });

  try {
    assert.ok(validRust.exportedSymbols.includes("tssae_adapter_v1"));
    assert.ok(validRust.exportedSymbols.includes("tssae_adapter_manifest_v1"));
    assert.ok(!validRust.exportedSymbols.includes("verule_adapter_v1"));
    assert.ok(
      !validRust.exportedSymbols.includes("verule_adapter_manifest_v1"),
    );

    const extNpm = await packExternalAdapterPackage({
      npmPackageName: adapterNpmName,
      adapterId,
      factTypeId,
      libraryPath: validRust.libraryPath,
      extraFactTypeIds: ["acme.external.undeclared-native"],
    });
    const tssaeBundle = await packWorkspaceArtifacts();

    const combinedBundle: PackedArtifactBundle = {
      packDir: tssaeBundle.packDir,
      artifacts: [...tssaeBundle.artifacts, extNpm.artifact],
      dispose(): void {
        extNpm.dispose();
        tssaeBundle.dispose();
      },
    };

    try {
      await withAcceptanceHost(
        {
          bundle: combinedBundle,
          directPackages: ["tssae", adapterNpmName],
          files: {
            "package.json": JSON.stringify(
              {
                name: "tssae-external-rust-adapter-host",
                private: true,
                type: "module",
                devDependencies: {
                  "@types/node": "^26.0.0",
                  typescript: "^7.0.2",
                },
              },
              null,
              2,
            ),
            "tsconfig.json": hostProjectTsconfig,
            "src/app.ts": hostAppSource,
            "consumer.tsconfig.json": consumerTsconfig,
            "consumer.ts": consumerSource,
            "variants/other-id.so": readFileSync(otherIdRust.libraryPath),
            "variants/wrong-abi.so": readFileSync(wrongAbiRust.libraryPath),
            "variants/missing-entry.so": readFileSync(
              missingEntryRust.libraryPath,
            ),
            "variants/missing-manifest.so": readFileSync(
              missingManifestRust.libraryPath,
            ),
          },
        },
        async (host) => {
          const tscResult = await host.runTsc("consumer.tsconfig.json");
          assert.equal(
            tscResult.status,
            0,
            `Consumer tsc --noEmit failed:\n${tscResult.stdout}\n${tscResult.stderr}`,
          );
          assert.equal(tscResult.isolation?.controlProbeBlocked, true);

          const runResult = await host.runNode(["consumer.ts"]);
          assert.equal(
            runResult.status,
            0,
            `External adapter acceptance script failed (status=${String(runResult.status)}):\nstdout:\n${runResult.stdout}\nstderr:\n${runResult.stderr}`,
          );
          assert.equal(runResult.timedOut, false);
          assert.equal(runResult.isolation?.controlProbeBlocked, true);

          const summary = JSON.parse(runResult.stdout.trim()) as {
            readonly providers: readonly string[];
            readonly value: {
              readonly callText: string;
              readonly signalArg: string;
              readonly adapterId: string;
            };
            readonly evidenceFile: string;
            readonly evidenceLine: number;
          };
          assert.deepEqual(summary.providers, [adapterId]);
          assert.equal(summary.value.adapterId, adapterId);
          assert.equal(summary.evidenceFile, "src/app.ts");
          assert.equal(summary.evidenceLine, 4);

          t.diagnostic(
            `Packed ${packedCrate.crateName}-${packedCrate.version}.crate; compiled external cdylib (${validRust.exportedSymbols.filter((s) => s.startsWith("tssae_adapter_")).join(", ")}); host isolation ${runResult.isolation?.mode}`,
          );
        },
      );
    } finally {
      combinedBundle.dispose();
    }
  } finally {
    missingManifestRust.dispose();
    missingEntryRust.dispose();
    wrongAbiRust.dispose();
    otherIdRust.dispose();
    validRust.dispose();
    packedCrate.dispose();
  }
});
