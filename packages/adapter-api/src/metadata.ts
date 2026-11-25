import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export type AdapterApiCargoMetadata = {
  readonly packageDir: string;
  readonly crateName: string;
  readonly version: string;
};

/**
 * Reads `[package]` `name` and `version` from this package's `Cargo.toml`
 * and verifies that the version matches `package.json`.
 */
export function readAdapterApiCargoMetadata(): AdapterApiCargoMetadata {
  const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const cargoTomlPath = join(packageDir, "Cargo.toml");
  const packageJsonPath = join(packageDir, "package.json");

  const cargoContent = readFileSync(cargoTomlPath, "utf8");
  let inPackageSection = false;
  let crateName: string | undefined;
  let cargoVersion: string | undefined;

  for (const rawLine of cargoContent.split(/\r?\n/u)) {
    const line = rawLine.trim();
    if (line.startsWith("[")) {
      inPackageSection = line === "[package]";
      continue;
    }
    if (!inPackageSection || line.length === 0 || line.startsWith("#")) {
      continue;
    }
    const nameMatch = /^name\s*=\s*"([^"]+)"/u.exec(line);
    if (nameMatch?.[1] !== undefined) {
      crateName = nameMatch[1];
      continue;
    }
    const versionMatch = /^version\s*=\s*"([^"]+)"/u.exec(line);
    if (versionMatch?.[1] !== undefined) {
      cargoVersion = versionMatch[1];
    }
  }

  if (crateName === undefined || cargoVersion === undefined) {
    throw new Error(
      `Failed to read [package] name and version from ${cargoTomlPath}`,
    );
  }

  const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf8")) as {
    readonly version?: unknown;
  };
  if (
    typeof packageJson.version === "string" &&
    packageJson.version !== cargoVersion
  ) {
    throw new Error(
      `Version mismatch between ${packageJsonPath} (${packageJson.version}) and ${cargoTomlPath} (${cargoVersion})`,
    );
  }

  return {
    packageDir,
    crateName,
    version: cargoVersion,
  };
}
