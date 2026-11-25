import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { join } from "node:path";

import { readAdapterApiCargoMetadata } from "../src/metadata.ts";

const { packageDir, crateName, version } = readAdapterApiCargoMetadata();
const targetDir = join(packageDir, "target");
const distDir = join(packageDir, "dist");

execFileSync(
  "cargo",
  [
    "package",
    "--locked",
    "--offline",
    "--allow-dirty",
    "--no-verify",
    "--manifest-path",
    join(packageDir, "Cargo.toml"),
    "--target-dir",
    targetDir,
  ],
  { cwd: packageDir, stdio: "inherit" },
);

const crateFilename = `${crateName}-${version}.crate`;
const packagedPath = join(targetDir, "package", crateFilename);
if (!existsSync(packagedPath)) {
  throw new Error(`Expected packed crate at ${packagedPath}`);
}

mkdirSync(distDir, { recursive: true });
copyFileSync(packagedPath, join(distDir, crateFilename));
