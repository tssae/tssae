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
