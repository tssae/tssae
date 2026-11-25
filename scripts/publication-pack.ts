import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const repoRoot = process.cwd();
const packOutDir = join(repoRoot, ".changeset", "packed");

rmSync(packOutDir, { recursive: true, force: true });
mkdirSync(packOutDir, { recursive: true });

execFileSync("pnpm", ["exec", "turbo", "run", "build"], {
  cwd: repoRoot,
  stdio: "inherit",
});

execFileSync(
  "pnpm",
  ["exec", "changeset", "pack", "--out-dir", ".changeset/packed"],
  {
    cwd: repoRoot,
    stdio: "inherit",
  },
);

const crateTargetDir = mkdtempSync(join(tmpdir(), "tssae-crate-release-"));
try {
  execFileSync(
    "cargo",
    [
      "package",
      "--locked",
      "--manifest-path",
      join(repoRoot, "packages", "adapter-api", "Cargo.toml"),
      "--target-dir",
      crateTargetDir,
    ],
    {
      cwd: repoRoot,
      stdio: "inherit",
    },
  );

  const packageDir = join(crateTargetDir, "package");
  const crates = readdirSync(packageDir).filter((name) =>
    name.endsWith(".crate"),
  );
  if (crates.length !== 1) {
    throw new Error(
      `Expected 1 packed .crate in ${packageDir}, found ${String(crates.length)}`,
    );
  }
  const crateFilename = crates[0]!;
  copyFileSync(
    join(packageDir, crateFilename),
    join(packOutDir, crateFilename),
  );
  process.stdout.write(
    `Packed candidate release artifacts into ${packOutDir} (including ${crateFilename})\n`,
  );
} finally {
  rmSync(crateTargetDir, { recursive: true, force: true });
}
