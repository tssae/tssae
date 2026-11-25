import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
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
  readonly fromPackDir?: string;
  readonly cwd?: string;
};

type PnpmPackJson = {
  readonly name: string;
  readonly version: string;
  readonly filename: string;
};

type PrepackedEntry = {
  readonly tarballPath: string;
  readonly expectedIntegrity?: string;
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

async function inspectTarballArtifact(
  tarballPath: string,
  expectedIntegrity?: string,
): Promise<PackedArtifact> {
  const bytes = readFileSync(tarballPath);
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  const sha1 = createHash("sha1").update(bytes).digest("hex");
  const sha512Integrity = `sha512-${createHash("sha512").update(bytes).digest("base64")}`;

  if (expectedIntegrity !== undefined) {
    const sha256Integrity = `sha256-${createHash("sha256").update(bytes).digest("base64")}`;
    if (
      expectedIntegrity !== sha256Integrity &&
      expectedIntegrity !== sha512Integrity
    ) {
      throw new Error(
        `Integrity mismatch for pre-packed tarball ${tarballPath}: expected ${expectedIntegrity}`,
      );
    }
  }

  const manifestRaw = await readTarEntry(tarballPath, "package/package.json");
  const manifestParsed: unknown = JSON.parse(manifestRaw.toString("utf8"));
  if (
    typeof manifestParsed !== "object" ||
    manifestParsed === null ||
    Array.isArray(manifestParsed)
  ) {
    throw new Error(`Invalid package.json inside ${tarballPath}`);
  }
  const manifest = Object.fromEntries(Object.entries(manifestParsed));
  const name = manifest.name;
  const version = manifest.version;
  if (typeof name !== "string" || typeof version !== "string") {
    throw new Error(
      `Missing name or version in package/package.json inside ${tarballPath}`,
    );
  }

  let binarySha256: string | undefined;
  if (name === "tssae") {
    const executableName =
      process.platform === "win32" ? "tssae-engine.exe" : "tssae-engine";
    const binaryBytes = await readTarEntry(
      tarballPath,
      `package/dist/bin/${executableName}`,
    );
    binarySha256 = createHash("sha256").update(binaryBytes).digest("hex");
  }

  return {
    name,
    version,
    filename: tarballPath,
    size: bytes.length,
    sha256,
    sha1,
    integrity: sha512Integrity,
    manifest,
    ...(binarySha256 === undefined ? {} : { binarySha256 }),
  };
}

async function discoverPrepackedTarballs(
  packDir: string,
): Promise<ReadonlyMap<string, PrepackedEntry>> {
  const entries = new Map<string, PrepackedEntry>();
  const planPath = join(packDir, "publish-plan.json");

  if (existsSync(planPath)) {
    const parsed: unknown = JSON.parse(readFileSync(planPath, "utf8"));
    if (
      typeof parsed !== "object" ||
      parsed === null ||
      !("plan" in parsed) ||
      !Array.isArray(parsed.plan)
    ) {
      throw new Error(`Invalid publish-plan.json at ${planPath}`);
    }
    for (const group of parsed.plan) {
      if (!Array.isArray(group)) continue;
      for (const item of group) {
        if (
          typeof item !== "object" ||
          item === null ||
          item.kind !== "publish" ||
          typeof item.name !== "string"
        ) {
          continue;
        }
        const tarball = (item as { readonly tarball?: unknown }).tarball;
        if (
          typeof tarball === "object" &&
          tarball !== null &&
          "path" in tarball &&
          typeof tarball.path === "string"
        ) {
          const tarballPath = resolve(packDir, tarball.path);
          const expectedIntegrity =
            "integrity" in tarball && typeof tarball.integrity === "string"
              ? tarball.integrity
              : undefined;
          entries.set(item.name, {
            tarballPath,
            ...(expectedIntegrity === undefined ? {} : { expectedIntegrity }),
          });
        }
      }
    }
  }

  for (const dir of [join(packDir, "packages"), packDir]) {
    if (!existsSync(dir)) continue;
    for (const dirent of readdirSync(dir, { withFileTypes: true })) {
      if (!dirent.isFile() || !dirent.name.endsWith(".tgz")) continue;
      const tarballPath = join(dir, dirent.name);
      const manifestRaw = await readTarEntry(
        tarballPath,
        "package/package.json",
      );
      const manifestParsed = JSON.parse(manifestRaw.toString("utf8")) as {
        readonly name?: unknown;
      };
      if (
        typeof manifestParsed.name === "string" &&
        !entries.has(manifestParsed.name)
      ) {
        entries.set(manifestParsed.name, { tarballPath });
      }
    }
  }

  return entries;
}

/**
 * Resolves pre-packed candidate `.tgz` tarballs from `fromPackDir` /
 * `TSSAE_ACCEPTANCE_PACK_DIR` when provided, or packs `tssae` and optional
 * packages into local `.tgz` tarballs with computed digests.
 */
export async function packWorkspaceArtifacts(
  options: PackWorkspaceArtifactsOptions = {},
): Promise<PackedArtifactBundle> {
  const basePackages = options.packages ?? CORE_TSSAE_PACKAGES;
  const packages = [
    ...new Set([...basePackages, ...(options.additionalPackages ?? [])]),
  ];

  const envPackDir = process.env.TSSAE_ACCEPTANCE_PACK_DIR?.trim();
  const explicitFromPackDir =
    options.fromPackDir ??
    (envPackDir !== undefined && envPackDir.length > 0
      ? envPackDir
      : undefined);

  if (explicitFromPackDir !== undefined) {
    const resolvedPackDir = resolve(explicitFromPackDir);
    if (!existsSync(resolvedPackDir)) {
      throw new Error(
        `Pre-packed artifact directory does not exist: ${resolvedPackDir}`,
      );
    }
    const discovered = await discoverPrepackedTarballs(resolvedPackDir);
    const artifacts: PackedArtifact[] = [];
    for (const pkg of packages) {
      const entry = discovered.get(pkg);
      if (entry === undefined) {
        throw new Error(
          `Pre-packed tarball for ${pkg} not found in ${resolvedPackDir}`,
        );
      }
      if (!existsSync(entry.tarballPath)) {
        throw new Error(
          `Pre-packed tarball for ${pkg} missing on disk: ${entry.tarballPath}`,
        );
      }
      artifacts.push(
        await inspectTarballArtifact(
          entry.tarballPath,
          entry.expectedIntegrity,
        ),
      );
    }
    return {
      packDir: resolvedPackDir,
      artifacts,
      dispose(): void {
        // Externally supplied candidate tarballs must be retained.
      },
    };
  }

  const ownsPackDir = options.packDir === undefined;
  const packDir =
    options.packDir ?? mkdtempSync(join(tmpdir(), "tssae-artifacts-"));
  const cwd = options.cwd ?? process.cwd();

  const artifacts: PackedArtifact[] = [];
  try {
    for (const pkg of packages) {
      const { stdout } = await execFileAsync(
        "pnpm",
        ["--filter", pkg, "pack", "--pack-destination", packDir, "--json"],
        { cwd, encoding: "utf8", maxBuffer: maxTarBufferBytes },
      );
      const packed = parsePackOutput(stdout);
      const artifact = await inspectTarballArtifact(packed.filename);
      if (
        artifact.name !== packed.name ||
        artifact.version !== packed.version
      ) {
        throw new Error(
          `Packed tarball ${packed.filename} metadata (${artifact.name}@${artifact.version}) mismatches pnpm pack output (${packed.name}@${packed.version})`,
        );
      }
      artifacts.push(artifact);
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
