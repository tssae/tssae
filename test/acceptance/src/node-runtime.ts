import { execFileSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  linkSync,
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const expectedHostNodeVersion = "24.0.0";

const nodeExecutableName = process.platform === "win32" ? "node.exe" : "node";
const moduleDir = dirname(fileURLToPath(import.meta.url));

let cachedWorkspaceNode24Executable: string | undefined;
let cachedSharedOutOfRepoBinDir: string | undefined;

/**
 * Resolves the workspace's Node 24.0.0 executable provisioned via `tssae`'s
 * package-level `devEngines.runtime`, verifying its runtime version once.
 */
function resolveWorkspaceNode24Executable(): string {
  if (cachedWorkspaceNode24Executable !== undefined) {
    return cachedWorkspaceNode24Executable;
  }

  const output = execFileSync(
    "pnpm",
    ["--filter", "tssae", "exec", "node", "-p", "process.execPath"],
    {
      cwd: moduleDir,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  const candidateBin = output
    .split(/\r?\n/u)
    .map((line) => line.trim())
    .findLast((line) => line.length > 0);
  if (candidateBin === undefined || !existsSync(candidateBin)) {
    throw new Error(
      `Missing Node ${expectedHostNodeVersion} runtime binary from pnpm --filter tssae exec; run pnpm install in the workspace first.`,
    );
  }

  const resolvedBin = realpathSync(candidateBin);
  const actualVersion = execFileSync(
    resolvedBin,
    ["-p", "process.versions.node"],
    {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  ).trim();
  if (actualVersion !== expectedHostNodeVersion) {
    throw new Error(
      `Expected tssae runtime binary ${resolvedBin} to report Node ${expectedHostNodeVersion}, got ${actualVersion}`,
    );
  }

  cachedWorkspaceNode24Executable = resolvedBin;
  return resolvedBin;
}

/**
 * Materializes the Node 24.0.0 binary inside an out-of-repo `runtimeDir/bin`
 * via hardlink (or copy across filesystems) so isolated hosts do not depend on
 * blocked workspace paths or trigger per-test runtime downloads.
 */
export function materializeOutOfRepoNode24Runtime(runtimeDir: string): {
  readonly binDir: string;
  readonly nodeBin: string;
} {
  const binDir = join(runtimeDir, "bin");
  const nodeBin = join(binDir, nodeExecutableName);
  if (existsSync(nodeBin)) {
    return { binDir, nodeBin };
  }

  mkdirSync(binDir, { recursive: true });
  const sourceBin = resolveWorkspaceNode24Executable();
  try {
    linkSync(sourceBin, nodeBin);
  } catch {
    copyFileSync(sourceBin, nodeBin);
    chmodSync(nodeBin, 0o755);
  }
  return { binDir, nodeBin };
}

/**
 * Returns a process-scoped out-of-repo `bin` directory containing Node 24.0.0
 * for standalone callers that do not pass an explicit `runtimeDir`.
 */
export function getSharedOutOfRepoNode24BinDir(): string {
  if (
    cachedSharedOutOfRepoBinDir !== undefined &&
    existsSync(join(cachedSharedOutOfRepoBinDir, nodeExecutableName))
  ) {
    return cachedSharedOutOfRepoBinDir;
  }

  const sharedRoot = mkdtempSync(join(tmpdir(), "tssae-node24-runtime-"));
  const { binDir } = materializeOutOfRepoNode24Runtime(sharedRoot);
  cachedSharedOutOfRepoBinDir = binDir;
  process.once("exit", () => {
    rmSync(sharedRoot, { recursive: true, force: true });
  });
  return binDir;
}

/**
 * Prepends directory entries onto an existing `PATH` string without introducing
 * trailing empty segments.
 */
export function prependToPath(
  prefixes: readonly string[],
  existingPath: string | undefined,
): string {
  const segments = [
    ...prefixes.filter((entry) => entry.length > 0),
    ...(existingPath !== undefined && existingPath.length > 0
      ? [existingPath]
      : []),
  ];
  return segments.join(delimiter);
}
