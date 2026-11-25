import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const maxTarBufferBytes = 64 * 1024 * 1024;

export const CORE_TSSAE_PACKAGES: readonly string[] = ["tssae"];

export type PackedArtifact = {
  readonly name: string;
  readonly version: string;
  readonly filename: string;
  readonly size: number;
  readonly sha256: string;
  readonly sha1: string;
  readonly integrity: string;
  readonly manifest: Record<string, unknown>;
  readonly binarySha256?: string;
};

export type PackedArtifactBundle = {
  readonly packDir: string;
  readonly artifacts: readonly PackedArtifact[];
  dispose(): void;
};

export type PackWorkspaceArtifactsOptions = {
  readonly packages?: readonly string[];
  readonly additionalPackages?: readonly string[];
  readonly packDir?: string;
  readonly cwd?: string;
};

type PnpmPackJson = {
  readonly name: string;
  readonly version: string;
  readonly filename: string;
};

function parsePackOutput(stdout: string): PnpmPackJson {
  const parsed: unknown = JSON.parse(stdout);
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    !("name" in parsed) ||
    !("version" in parsed) ||
    !("filename" in parsed) ||
    typeof parsed.name !== "string" ||
    typeof parsed.version !== "string" ||
    typeof parsed.filename !== "string"
  ) {
    throw new Error(`Unexpected pnpm pack --json output: ${stdout}`);
  }
  return {
    name: parsed.name,
    version: parsed.version,
    filename: parsed.filename,
  };
}

async function readTarEntry(
  tarballPath: string,
  entryPath: string,
): Promise<Buffer> {
  const { stdout } = await execFileAsync(
    "tar",
    ["-xOf", tarballPath, entryPath],
    { encoding: "buffer", maxBuffer: maxTarBufferBytes },
  );
  return stdout;
}

/**
 * Packs `tssae` and any optional packages into local `.tgz` tarballs with computed digests.
 */
export async function packWorkspaceArtifacts(
  options: PackWorkspaceArtifactsOptions = {},
): Promise<PackedArtifactBundle> {
  const ownsPackDir = options.packDir === undefined;
  const packDir =
    options.packDir ?? mkdtempSync(join(tmpdir(), "tssae-artifacts-"));
  const cwd = options.cwd ?? process.cwd();
  const basePackages = options.packages ?? CORE_TSSAE_PACKAGES;
  const packages = [
    ...new Set([...basePackages, ...(options.additionalPackages ?? [])]),
  ];

  const artifacts: PackedArtifact[] = [];
  try {
    for (const pkg of packages) {
      const { stdout } = await execFileAsync(
        "pnpm",
        ["--filter", pkg, "pack", "--pack-destination", packDir, "--json"],
        { cwd, encoding: "utf8", maxBuffer: maxTarBufferBytes },
      );
      const packed = parsePackOutput(stdout);
      const bytes = readFileSync(packed.filename);
      const manifestRaw = await readTarEntry(
        packed.filename,
        "package/package.json",
      );
      const manifestParsed: unknown = JSON.parse(manifestRaw.toString("utf8"));
      if (
        typeof manifestParsed !== "object" ||
        manifestParsed === null ||
        Array.isArray(manifestParsed)
      ) {
        throw new Error(`Invalid package.json inside ${packed.filename}`);
      }
      const manifest = Object.fromEntries(Object.entries(manifestParsed));
      let binarySha256: string | undefined;
      if (packed.name === "tssae") {
        const executableName =
          process.platform === "win32" ? "tssae-engine.exe" : "tssae-engine";
        const binaryBytes = await readTarEntry(
          packed.filename,
          `package/dist/bin/${executableName}`,
        );
        binarySha256 = createHash("sha256").update(binaryBytes).digest("hex");
      }

      artifacts.push({
        name: packed.name,
        version: packed.version,
        filename: packed.filename,
        size: bytes.length,
        sha256: createHash("sha256").update(bytes).digest("hex"),
        sha1: createHash("sha1").update(bytes).digest("hex"),
        integrity: `sha512-${createHash("sha512").update(bytes).digest("base64")}`,
        manifest,
        ...(binarySha256 === undefined ? {} : { binarySha256 }),
      });
    }
  } catch (error) {
    if (ownsPackDir) {
      rmSync(packDir, { recursive: true, force: true });
    }
    throw error;
  }

  return {
    packDir,
    artifacts,
    dispose(): void {
      if (ownsPackDir) {
        rmSync(packDir, { recursive: true, force: true });
      }
    },
  };
}
