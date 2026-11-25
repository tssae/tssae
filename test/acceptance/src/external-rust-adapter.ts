import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import type { PackedArtifact } from "./artifacts.ts";

const execFileAsync = promisify(execFile);
const maxOutputBytes = 64 * 1024 * 1024;
const defaultRepoRoot = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../../..",
);

export type PackedAdapterApiCrate = {
  readonly crateName: string;
  readonly version: string;
  readonly cratePath: string;
  readonly extractedDir: string;
  dispose(): void;
};

export type PackAdapterApiCrateOptions = {
  readonly cwd?: string;
  readonly tempRoot?: string;
  readonly allowDirty?: boolean;
};

export type ExternalRustAdapterVariant =
  | "valid"
  | "wrong-abi"
  | "missing-entry-symbol"
  | "missing-manifest-symbol";

export type BuildExternalRustAdapterOptions = {
  readonly extractedCrateDir: string;
  readonly adapterId?: string;
  readonly factTypeId?: string;
  readonly crateName?: string;
  readonly variant?: ExternalRustAdapterVariant;
  readonly tempRoot?: string;
};

export type BuiltExternalRustAdapter = {
  readonly projectDir: string;
  readonly libraryPath: string;
  readonly exportedSymbols: readonly string[];
  dispose(): void;
};

export type PackExternalAdapterPackageOptions = {
  readonly npmPackageName: string;
  readonly version?: string;
  readonly adapterId: string;
  readonly factTypeId: string;
  readonly libraryPath: string;
  readonly tssaeVersionRange?: string;
  readonly extraFactTypeIds?: readonly string[];
  readonly tempRoot?: string;
};

export type PackedExternalAdapterPackage = {
  readonly packageDir: string;
  readonly artifact: PackedArtifact;
  dispose(): void;
};

function sanitizeRustIdent(input: string): string {
  const normalized = input.replace(/[^a-zA-Z0-9_]+/gu, "_");
  return /^[a-zA-Z_]/u.test(normalized) ? normalized : `ext_${normalized}`;
}

async function isAdapterApiDirty(repoRoot: string): Promise<boolean> {
  try {
    const { stdout } = await execFileAsync(
      "git",
      ["status", "--porcelain", "--", "packages/adapter-api"],
      { cwd: repoRoot, encoding: "utf8" },
    );
    return stdout.trim().length > 0;
  } catch {
    return false;
  }
}

async function readDynamicSymbols(libraryPath: string): Promise<string[]> {
  const { stdout } = await execFileAsync(
    "nm",
    ["-D", "--defined-only", libraryPath],
    { encoding: "utf8", maxBuffer: maxOutputBytes },
  );
  const symbols: string[] = [];
  for (const line of stdout.split(/\r?\n/u)) {
    const parts = line.trim().split(/\s+/u);
    const symbol = parts.at(-1);
    if (symbol !== undefined && symbol.length > 0) {
      symbols.push(symbol);
    }
  }
  return symbols;
}

/**
 * Packages `packages/adapter-api` into a `.crate` archive in a temporary directory
 * and extracts it outside the repository for independent Cargo consumption.
 */
export async function packAdapterApiCrate(
  options: PackAdapterApiCrateOptions = {},
): Promise<PackedAdapterApiCrate> {
  const repoRoot = resolve(options.cwd ?? defaultRepoRoot);
  const manifestPath = join(repoRoot, "packages/adapter-api/Cargo.toml");
  const tempParent = options.tempRoot ?? tmpdir();
  mkdirSync(tempParent, { recursive: true });
  const workDir = mkdtempSync(join(tempParent, "tssae-crate-pack-"));

  try {
    const targetDir = join(workDir, "target");
    const extractedRoot = join(workDir, "extracted");
    mkdirSync(targetDir, { recursive: true });
    mkdirSync(extractedRoot, { recursive: true });

    const allowDirty =
      options.allowDirty ?? (await isAdapterApiDirty(repoRoot));
    const cargoArgs = [
      "package",
      "--locked",
      "--offline",
      ...(allowDirty ? ["--allow-dirty"] : []),
      "--manifest-path",
      manifestPath,
      "--target-dir",
      targetDir,
    ];
    await execFileAsync("cargo", cargoArgs, {
      cwd: repoRoot,
      encoding: "utf8",
      maxBuffer: maxOutputBytes,
    });

    const crateName = "tssae-adapter-api";
    const version = "0.1.0";
    const cratePath = join(
      targetDir,
      "package",
      `${crateName}-${version}.crate`,
    );
    if (!existsSync(cratePath)) {
      throw new Error(`Expected packed crate at ${cratePath}`);
    }

    await execFileAsync("tar", ["-xzf", cratePath, "-C", extractedRoot], {
      encoding: "utf8",
    });
    const extractedDir = join(extractedRoot, `${crateName}-${version}`);
    if (!existsSync(join(extractedDir, "Cargo.toml"))) {
      throw new Error(`Extracted crate missing Cargo.toml at ${extractedDir}`);
    }

    return {
      crateName,
      version,
      cratePath,
      extractedDir,
      dispose(): void {
        rmSync(workDir, { recursive: true, force: true });
      },
    };
  } catch (error) {
    rmSync(workDir, { recursive: true, force: true });
    throw error;
  }
}

function renderExternalRustSource(options: {
  readonly adapterId: string;
  readonly factTypeId: string;
  readonly variant: ExternalRustAdapterVariant;
}): string {
  if (options.variant === "missing-manifest-symbol") {
    return `pub fn placeholder_symbol() -> u32 {
    1
}
`;
  }

  if (options.variant === "missing-entry-symbol") {
    return `use std::sync::OnceLock;

use tssae_adapter_api::{ADAPTER_ABI_VERSION, Adapter, AdapterError, AdapterManifest, Context, FactContribution, FactRequest, ffi};

#[derive(Default)]
struct MissingEntryAdapter;

impl Adapter for MissingEntryAdapter {
    fn manifest(&self) -> AdapterManifest {
        AdapterManifest {
            abi_version: ADAPTER_ABI_VERSION,
            id: ${JSON.stringify(options.adapterId)}.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            fact_type_ids: vec![${JSON.stringify(options.factTypeId)}.to_string()],
        }
    }

    fn extract<'a>(
        &self,
        _context: &'a Context<'_>,
        _request: &FactRequest<'a>,
    ) -> Result<Vec<FactContribution<'a>>, AdapterError> {
        Ok(Vec::new())
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn tssae_adapter_manifest_v1() -> ffi::FfiManifestSlice {
    static MANIFEST: OnceLock<Box<[u8]>> = OnceLock::new();
    ffi::manifest_slice::<MissingEntryAdapter>(&MANIFEST)
}
`;
  }

  const abiVersionExpr =
    options.variant === "wrong-abi"
      ? "ADAPTER_ABI_VERSION + 1"
      : "ADAPTER_ABI_VERSION";

  return `use serde_json::json;
use tssae_adapter_api::{
    ADAPTER_ABI_VERSION, Adapter, AdapterError, AdapterManifest, Context, Evidence,
    FactContribution, FactRequest, NodeKind,
};

#[derive(Default)]
struct ExternalRustAdapter;

impl Adapter for ExternalRustAdapter {
    fn manifest(&self) -> AdapterManifest {
        AdapterManifest {
            abi_version: ${abiVersionExpr},
            id: ${JSON.stringify(options.adapterId)}.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            fact_type_ids: vec![${JSON.stringify(options.factTypeId)}.to_string()],
        }
    }

    fn extract<'a>(
        &self,
        _context: &'a Context<'_>,
        request: &FactRequest<'a>,
    ) -> Result<Vec<FactContribution<'a>>, AdapterError> {
        if request.fact_type_id != ${JSON.stringify(options.factTypeId)} {
            return Err(AdapterError::UnsupportedFactType {
                adapter_id: ${JSON.stringify(options.adapterId)}.to_string(),
                fact_type_id: request.fact_type_id.to_string(),
            });
        }
        let mut out = Vec::new();
        for call in &request.calls {
            if call.kind()? != NodeKind::CallExpression {
                continue;
            }
            if let Some(targets) = &request.targets
                && !targets.contains(call)
            {
                continue;
            }
            let text = call.text()?.unwrap_or_default();
            if !text.starts_with("externalSignal(") {
                continue;
            }
            let arg_text = call
                .arguments()?
                .into_iter()
                .next()
                .and_then(|arg| arg.text().ok().flatten())
                .unwrap_or_default();
            let mut contribution = FactContribution::new(
                *call,
                json!({
                    "callText": text,
                    "signalArg": arg_text,
                    "adapterId": ${JSON.stringify(options.adapterId)},
                }),
            );
            if let Some(at) = call.location()? {
                contribution.evidence.push(Evidence {
                    kind: "source".to_string(),
                    message: "External Rust cdylib adapter extracted signal call.".to_string(),
                    at: Some(at),
                    model_id: None,
                    declaration: None,
                });
            }
            out.push(contribution);
        }
        Ok(out)
    }
}

tssae_adapter_api::export_adapter!(ExternalRustAdapter);
`;
}

/**
 * Generates and compiles an isolated external Rust `cdylib` crate against the
 * extracted `tssae-adapter-api` `.crate` outside the repository tree.
 */
export async function buildExternalRustAdapter(
  options: BuildExternalRustAdapterOptions,
): Promise<BuiltExternalRustAdapter> {
  const adapterId = options.adapterId ?? "acme-external";
  const factTypeId = options.factTypeId ?? "acme.external.signal";
  const crateName = sanitizeRustIdent(
    options.crateName ?? `acme_external_${options.variant ?? "valid"}`,
  );
  const variant = options.variant ?? "valid";

  const tempParent = options.tempRoot ?? tmpdir();
  mkdirSync(tempParent, { recursive: true });
  const projectDir = mkdtempSync(join(tempParent, "tssae-ext-rust-"));

  try {
    mkdirSync(join(projectDir, "src"), { recursive: true });
    const cargoToml = `[package]
name = ${JSON.stringify(crateName.replace(/_/gu, "-"))}
version = "0.1.0"
edition = "2024"

[lib]
name = ${JSON.stringify(crateName)}
crate-type = ["cdylib"]

[dependencies]
tssae-adapter-api = { version = "0.1.0", default-features = false }
serde_json = "1"

[patch.crates-io]
tssae-adapter-api = { path = ${JSON.stringify(options.extractedCrateDir)} }

[workspace]
`;
    writeFileSync(join(projectDir, "Cargo.toml"), cargoToml, "utf8");
    writeFileSync(
      join(projectDir, "src", "lib.rs"),
      renderExternalRustSource({ adapterId, factTypeId, variant }),
      "utf8",
    );

    await execFileAsync("cargo", ["build", "--release", "--offline"], {
      cwd: projectDir,
      encoding: "utf8",
      maxBuffer: maxOutputBytes,
    });

    const libraryPath = join(
      projectDir,
      "target",
      "release",
      `lib${crateName}.so`,
    );
    if (!existsSync(libraryPath)) {
      throw new Error(`Compiled cdylib missing at ${libraryPath}`);
    }
    const exportedSymbols = await readDynamicSymbols(libraryPath);
    return {
      projectDir,
      libraryPath,
      exportedSymbols,
      dispose(): void {
        rmSync(projectDir, { recursive: true, force: true });
      },
    };
  } catch (error) {
    rmSync(projectDir, { recursive: true, force: true });
    throw error;
  }
}

/**
 * Packages a compiled external `.so` and TSSAE JS bridge + `tssae-native.json`
 * descriptor into an installable `.tgz` artifact for loopback registry hosts.
 */
export async function packExternalAdapterPackage(
  options: PackExternalAdapterPackageOptions,
): Promise<PackedExternalAdapterPackage> {
  const version = options.version ?? "0.1.0";
  const tempParent = options.tempRoot ?? tmpdir();
  mkdirSync(tempParent, { recursive: true });
  const packageDir = mkdtempSync(join(tempParent, "tssae-ext-npm-"));

  try {
    const distDir = join(packageDir, "dist");
    const nativeDir = join(distDir, "native");
    const packDir = join(packageDir, "pack");
    mkdirSync(nativeDir, { recursive: true });
    mkdirSync(packDir, { recursive: true });

    const libraryFilename = basename(options.libraryPath);
    copyFileSync(options.libraryPath, join(nativeDir, libraryFilename));

    const descriptor = {
      id: options.adapterId,
      "linux-x64": "./native/linux-x64",
    };
    writeFileSync(
      join(distDir, "tssae-native.json"),
      `${JSON.stringify(descriptor, null, 2)}\n`,
      "utf8",
    );

    const extraProvides = (options.extraFactTypeIds ?? []).map(
      (extraId) => `    defineFactProvider(
      defineFactType({ id: ${JSON.stringify(extraId)}, subjectKind: "call" }),
      (ctx, request) =>
        ctx.extractEngineFacts({
          factTypeId: request.factType.id,
          providerId: ${JSON.stringify(options.adapterId)},
          ...(request.targets !== undefined ? { targets: request.targets } : {}),
          ...(request.explain !== undefined ? { explain: request.explain } : {}),
        }),
    ),`,
    );

    const indexJs = `import { defineAdapter, defineFactProvider, defineFactType } from "tssae";

export const ExternalSignalFact = defineFactType({
  id: ${JSON.stringify(options.factTypeId)},
  description: "Call signal extracted by an external out-of-repo Rust cdylib adapter.",
  subjectKind: "call",
});

export const externalAdapterPackage = ${JSON.stringify(options.npmPackageName)};

const externalAdapter = defineAdapter({
  id: ${JSON.stringify(options.adapterId)},
  version: ${JSON.stringify(version)},
  description: "Out-of-repo external Rust adapter package.",
  provides: [
    defineFactProvider(ExternalSignalFact, (ctx, request) =>
      ctx.extractEngineFacts({
        factTypeId: request.factType.id,
        providerId: ${JSON.stringify(options.adapterId)},
        reportProviderId: ${JSON.stringify(options.adapterId)},
        ...(request.targets !== undefined ? { targets: request.targets } : {}),
        ...(request.condition !== undefined ? { condition: request.condition } : {}),
        ...(request.explain !== undefined ? { explain: request.explain } : {}),
      }),
    ),
${extraProvides.join("\n")}
  ],
});

export default externalAdapter;
`;

    const indexDts = `import type { FactType, LibraryAdapter } from "tssae";

export type ExternalSignalValue = {
  readonly callText: string;
  readonly signalArg: string;
  readonly adapterId: string;
};

export declare const ExternalSignalFact: FactType<"call", undefined, ExternalSignalValue>;
export declare const externalAdapterPackage: string;
declare const externalAdapter: LibraryAdapter;
export default externalAdapter;
`;

    writeFileSync(join(distDir, "index.js"), indexJs, "utf8");
    writeFileSync(join(distDir, "index.d.ts"), indexDts, "utf8");

    const packageJson = {
      name: options.npmPackageName,
      version,
      license: "MIT",
      type: "module",
      files: ["dist"],
      exports: {
        ".": {
          types: "./dist/index.d.ts",
          default: "./dist/index.js",
        },
        "./tssae-native": "./dist/tssae-native.json",
        "./native/linux-x64": `./dist/native/${libraryFilename}`,
        "./package.json": "./package.json",
      },
      dependencies: {
        tssae: options.tssaeVersionRange ?? "0.1.0",
      },
    };
    writeFileSync(
      join(packageDir, "package.json"),
      `${JSON.stringify(packageJson, null, 2)}\n`,
      "utf8",
    );

    const { stdout } = await execFileAsync(
      "npm",
      ["pack", "--pack-destination", packDir, "--json"],
      { cwd: packageDir, encoding: "utf8", maxBuffer: maxOutputBytes },
    );
    const packedList = JSON.parse(stdout) as readonly {
      readonly filename: string;
    }[];
    const tarballName = packedList[0]?.filename;
    if (tarballName === undefined) {
      throw new Error(`Unexpected npm pack output: ${stdout}`);
    }
    const tarballPath = join(packDir, tarballName);
    const bytes = readFileSync(tarballPath);

    const artifact: PackedArtifact = {
      name: options.npmPackageName,
      version,
      filename: tarballPath,
      size: bytes.length,
      sha256: createHash("sha256").update(bytes).digest("hex"),
      sha1: createHash("sha1").update(bytes).digest("hex"),
      integrity: `sha512-${createHash("sha512").update(bytes).digest("base64")}`,
      manifest: packageJson,
    };

    return {
      packageDir,
      artifact,
      dispose(): void {
        rmSync(packageDir, { recursive: true, force: true });
      },
    };
  } catch (error) {
    rmSync(packageDir, { recursive: true, force: true });
    throw error;
  }
}
