import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  realpathSync,
} from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve, sep } from "node:path";

import type { PackedArtifact } from "./artifacts.ts";
import { DEFAULT_DIRECT_TSSAE_PACKAGES } from "./host-install.ts";
import { resolveDefaultBlockedPaths } from "./isolation.ts";

export type HostResolutionAudit = {
  readonly checkoutRealpath: string;
  readonly tssaePackagePath: string;
  readonly tssaeVersion: string;
  readonly tssaeResolvedPath: string;
  readonly hasSourceConditionInExports: boolean;
  readonly engineBinaryPath: string;
  readonly engineBinarySha256: string;
  readonly resolvedDirectPackages: Readonly<Record<string, string>>;
  readonly auditedSymlinkCount: number;
  readonly workspaceSymlinkCount: number;
  readonly storeSymlinkCount: number;
};

export type VerifyHostResolutionOptions = {
  readonly checkoutDir: string;
  readonly artifacts: readonly PackedArtifact[];
  readonly directPackages?: readonly string[];
  readonly blockedPaths?: readonly string[];
  readonly allowedStorePaths?: readonly string[];
};

function isInsideDirectory(
  candidatePath: string,
  directoryPath: string,
): boolean {
  return (
    candidatePath === directoryPath ||
    candidatePath.startsWith(directoryPath + sep)
  );
}

function collectAllowedStorePaths(
  checkoutDir: string,
  extraPaths: readonly string[] = [],
): readonly string[] {
  const paths = new Set<string>(extraPaths.map((item) => resolve(item)));
  if (existsSync("/pnpm/store")) {
    paths.add(realpathSync("/pnpm/store"));
  }
  try {
    const storePath = execFileSync("pnpm", ["store", "path"], {
      cwd: checkoutDir,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
    if (storePath.length > 0 && existsSync(storePath)) {
      paths.add(realpathSync(storePath));
    }
  } catch {
    // Ignore when pnpm store path is unavailable.
  }
  return [...paths];
}

function auditNodeModulesSymlinks(options: {
  readonly checkoutRealpath: string;
  readonly blockedPaths: readonly string[];
  readonly allowedStorePaths: readonly string[];
}): {
  readonly auditedSymlinkCount: number;
  readonly workspaceSymlinkCount: number;
  readonly storeSymlinkCount: number;
} {
  let auditedSymlinkCount = 0;
  let workspaceSymlinkCount = 0;
  let storeSymlinkCount = 0;

  function scanNodeModulesDir(dir: string): void {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const entryPath = join(dir, entry.name);
      if (entry.isSymbolicLink()) {
        auditedSymlinkCount += 1;
        let targetPath: string;
        try {
          targetPath = realpathSync(entryPath);
        } catch {
          targetPath = resolve(dirname(entryPath), readlinkSync(entryPath));
        }

        for (const blockedPath of options.blockedPaths) {
          if (isInsideDirectory(targetPath, blockedPath)) {
            throw new Error(
              `Symlink in host node_modules points into development repository: ${entryPath} -> ${targetPath}`,
            );
          }
        }

        if (isInsideDirectory(targetPath, options.checkoutRealpath)) {
          workspaceSymlinkCount += 1;
        } else if (
          options.allowedStorePaths.some((storePath) =>
            isInsideDirectory(targetPath, storePath),
          )
        ) {
          storeSymlinkCount += 1;
        } else {
          throw new Error(
            `Symlink in host node_modules resolves outside host checkout and package store: ${entryPath} -> ${targetPath}`,
          );
        }
      } else if (entry.isDirectory()) {
        scanNodeModulesDir(entryPath);
      }
    }
  }

  function findNodeModulesDirs(dir: string): void {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (!entry.isDirectory() || entry.name === ".git") {
        continue;
      }
      const entryPath = join(dir, entry.name);
      if (entry.name === "node_modules") {
        scanNodeModulesDir(entryPath);
      } else {
        findNodeModulesDirs(entryPath);
      }
    }
  }

  findNodeModulesDirs(options.checkoutRealpath);

  return {
    auditedSymlinkCount,
    workspaceSymlinkCount,
    storeSymlinkCount,
  };
}

/**
 * Verifies that `tssae`, its packaged `tssae-engine` binary, and any optional
 * direct packages resolve inside the host checkout's `node_modules` tree with
 * matching artifact digests and zero symlinks back to the development repository.
 */
export function verifyHostArtifactResolution(
  options: VerifyHostResolutionOptions,
): HostResolutionAudit {
  const checkoutRealpath = realpathSync(options.checkoutDir);
  const blockedPaths = (options.blockedPaths ?? resolveDefaultBlockedPaths())
    .filter((path) => existsSync(path))
    .map((path) => realpathSync(path));
  const allowedStorePaths = collectAllowedStorePaths(
    checkoutRealpath,
    options.allowedStorePaths,
  );

  const symlinkAudit = auditNodeModulesSymlinks({
    checkoutRealpath,
    blockedPaths,
    allowedStorePaths,
  });

  const hostRequire = createRequire(join(checkoutRealpath, "package.json"));
  const tssaePackagePath = realpathSync(
    hostRequire.resolve("tssae/package.json"),
  );
  if (!isInsideDirectory(tssaePackagePath, checkoutRealpath)) {
    throw new Error(
      `Resolved tssae/package.json outside host checkout: ${tssaePackagePath}`,
    );
  }
  const tssaeManifestRaw: unknown = JSON.parse(
    readFileSync(tssaePackagePath, "utf8"),
  );
  if (typeof tssaeManifestRaw !== "object" || tssaeManifestRaw === null) {
    throw new Error(`Invalid package.json at ${tssaePackagePath}`);
  }
  const tssaeManifest = tssaeManifestRaw as Record<string, unknown>;
  const tssaeVersion =
    typeof tssaeManifest.version === "string" ? tssaeManifest.version : "";
  if (tssaeVersion.length === 0) {
    throw new Error(`Missing version in ${tssaePackagePath}`);
  }
  const packedTssae = options.artifacts.find(
    (artifact) => artifact.name === "tssae",
  );
  if (packedTssae !== undefined && tssaeVersion !== packedTssae.version) {
    throw new Error(
      `Resolved tssae version ${tssaeVersion} does not match packed artifact version ${packedTssae.version}`,
    );
  }
  const rootExports = (
    tssaeManifest.exports as Record<string, unknown> | undefined
  )?.["."] as Record<string, unknown> | undefined;
  const hasSourceConditionInExports =
    rootExports !== undefined && "source" in rootExports;
  if (hasSourceConditionInExports) {
    throw new Error(
      `Installed tssae package.json unexpectedly retained "source" export condition: ${JSON.stringify(rootExports)}`,
    );
  }

  const tssaeResolvedPath = realpathSync(hostRequire.resolve("tssae"));
  if (!isInsideDirectory(tssaeResolvedPath, checkoutRealpath)) {
    throw new Error(
      `Resolved tssae outside host checkout: ${tssaeResolvedPath}`,
    );
  }

  const directPackages =
    options.directPackages ?? DEFAULT_DIRECT_TSSAE_PACKAGES;
  const resolvedDirectPackages: Record<string, string> = {};
  for (const packageName of directPackages) {
    let resolvedPath: string;
    try {
      resolvedPath = realpathSync(hostRequire.resolve(packageName));
    } catch {
      resolvedPath = realpathSync(
        hostRequire.resolve(`${packageName}/package.json`),
      );
    }
    if (!isInsideDirectory(resolvedPath, checkoutRealpath)) {
      throw new Error(
        `Resolved direct package ${packageName} outside host checkout: ${resolvedPath}`,
      );
    }
    resolvedDirectPackages[packageName] = resolvedPath;
  }

  const executableName =
    process.platform === "win32" ? "tssae-engine.exe" : "tssae-engine";
  const candidateBinaryPath = join(
    dirname(tssaePackagePath),
    "dist",
    "bin",
    executableName,
  );
  if (!existsSync(candidateBinaryPath)) {
    throw new Error(
      `Missing packaged tssae-engine binary at ${candidateBinaryPath}`,
    );
  }
  const engineBinaryPath = realpathSync(candidateBinaryPath);
  if (!isInsideDirectory(engineBinaryPath, checkoutRealpath)) {
    throw new Error(
      `Resolved tssae-engine binary outside host checkout: ${engineBinaryPath}`,
    );
  }

  const engineBinarySha256 = createHash("sha256")
    .update(readFileSync(engineBinaryPath))
    .digest("hex");
  if (
    packedTssae?.binarySha256 !== undefined &&
    engineBinarySha256 !== packedTssae.binarySha256
  ) {
    throw new Error(
      `Host tssae-engine SHA-256 ${engineBinarySha256} does not match packed artifact ${packedTssae.binarySha256}`,
    );
  }

  return {
    checkoutRealpath,
    tssaePackagePath,
    tssaeVersion,
    tssaeResolvedPath,
    hasSourceConditionInExports,
    engineBinaryPath,
    engineBinarySha256,
    resolvedDirectPackages,
    ...symlinkAudit,
  };
}

export type VerifiedNativeLibrary = {
  readonly specifier: string;
  readonly providerId: string;
  readonly descriptorPath: string;
  readonly path: string;
  readonly sha256: string;
};

export type VerifyInstalledNativeLibraryOptions = {
  readonly checkoutDir: string;
  readonly specifier: string;
  readonly artifact?: PackedArtifact;
  readonly resolutionBaseDir?: string;
  readonly expectedProviderId?: string;
};

function currentNativePlatformKey(): string {
  return `${process.platform}-${process.arch}`;
}

function resolveArtifactExportSubpath(
  specifier: string,
  artifactName: string,
  relativeExport: string,
): string {
  const normalizedRelative = relativeExport.replace(/^\.\//u, "");
  if (specifier === artifactName) {
    return `./${normalizedRelative}`;
  }
  if (!specifier.startsWith(`${artifactName}/`)) {
    throw new Error(
      `Adapter specifier ${specifier} does not belong to artifact ${artifactName}`,
    );
  }
  const subpathPrefix = specifier.slice(artifactName.length + 1);
  return `./${subpathPrefix}/${normalizedRelative}`;
}

/**
 * Verifies that an installed native adapter (root or subpath specifier) resolves
 * its `tssae-native` descriptor and native library inside `checkoutDir` with
 * an optional digest check against its `.tgz`.
 */
export function verifyInstalledNativeLibrary(
  options: VerifyInstalledNativeLibraryOptions,
): VerifiedNativeLibrary {
  const checkoutRealpath = realpathSync(options.checkoutDir);
  const resolutionRoot = options.resolutionBaseDir ?? checkoutRealpath;
  const hostRequire = createRequire(join(resolutionRoot, "package.json"));
  const descriptorPath = realpathSync(
    hostRequire.resolve(`${options.specifier}/tssae-native`),
  );
  if (!isInsideDirectory(descriptorPath, checkoutRealpath)) {
    throw new Error(
      `Resolved ${options.specifier}/tssae-native outside host checkout: ${descriptorPath}`,
    );
  }

  const descriptorRaw: unknown = JSON.parse(
    readFileSync(descriptorPath, "utf8"),
  );
  if (
    typeof descriptorRaw !== "object" ||
    descriptorRaw === null ||
    !("id" in descriptorRaw) ||
    typeof descriptorRaw.id !== "string"
  ) {
    throw new Error(`Invalid native descriptor in ${descriptorPath}`);
  }
  const providerId = descriptorRaw.id;
  if (
    options.expectedProviderId !== undefined &&
    providerId !== options.expectedProviderId
  ) {
    throw new Error(
      `Expected native adapter ${options.specifier} id "${options.expectedProviderId}", got "${providerId}"`,
    );
  }

  const platformKey = currentNativePlatformKey();
  const exportSubpath =
    platformKey in descriptorRaw
      ? (descriptorRaw as Record<string, unknown>)[platformKey]
      : undefined;
  if (typeof exportSubpath !== "string" || !exportSubpath.startsWith("./")) {
    throw new Error(
      `Missing ${platformKey} entry in native descriptor ${descriptorPath}`,
    );
  }

  const targetSpecifier = `${options.specifier}/${exportSubpath.slice(2)}`;
  const libraryPath = realpathSync(hostRequire.resolve(targetSpecifier));
  if (!isInsideDirectory(libraryPath, checkoutRealpath)) {
    throw new Error(
      `Resolved native library for ${options.specifier} outside host checkout: ${libraryPath}`,
    );
  }

  const installedSha256 = createHash("sha256")
    .update(readFileSync(libraryPath))
    .digest("hex");

  if (options.artifact !== undefined) {
    const artifactExports = options.artifact.manifest.exports as
      | Record<string, unknown>
      | undefined;
    const exportKey = resolveArtifactExportSubpath(
      options.specifier,
      options.artifact.name,
      exportSubpath,
    );
    const archiveLibraryPath = artifactExports?.[exportKey];
    if (typeof archiveLibraryPath !== "string") {
      throw new Error(
        `Missing ${exportKey} export in packed artifact ${options.artifact.name}`,
      );
    }
    const packedBytes = execFileSync(
      "tar",
      [
        "-xOf",
        options.artifact.filename,
        `package/${archiveLibraryPath.replace(/^\.\//u, "")}`,
      ],
      { encoding: "buffer", maxBuffer: 32 * 1024 * 1024 },
    );
    const packedSha256 = createHash("sha256").update(packedBytes).digest("hex");
    if (installedSha256 !== packedSha256) {
      throw new Error(
        `Installed native library SHA-256 ${installedSha256} for ${options.specifier} does not match packed artifact ${packedSha256}`,
      );
    }
  }

  return {
    specifier: options.specifier,
    providerId,
    descriptorPath,
    path: libraryPath,
    sha256: installedSha256,
  };
}
