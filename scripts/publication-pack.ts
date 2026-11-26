import { execFileSync } from "node:child_process";
import { mkdirSync, rmSync } from "node:fs";
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

process.stdout.write(`Packed candidate release artifacts into ${packOutDir}\n`);
