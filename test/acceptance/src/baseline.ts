import { execFile, execFileSync, spawn } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { basename, join, resolve } from "node:path";
import { promisify } from "node:util";

import { packWorkspaceArtifacts } from "./artifacts.ts";

const execFileAsync = promisify(execFile);

export type ResolvedRevision = {
  readonly rev: string;
  readonly commitSha: string;
};

export type RepoHeadState = {
  readonly commitSha: string;
  readonly dirty: boolean;
};

export type BaselineArtifact = {
  readonly rev: string;
  readonly commitSha: string;
  readonly cacheKey: string;
  readonly cacheDir: string;
  readonly tarballPath: string;
  readonly version: string;
  readonly sha256: string;
  readonly binarySha256: string;
  readonly cacheHit: boolean;
};

export type PrepareBaselineArtifactOptions = {
  readonly repoRoot?: string;
  readonly baselineRev?: string;
  readonly cacheDir?: string;
  readonly refreshBaseline?: boolean;
};

type BaselineReadyMetadata = {
  readonly rev: string;
  readonly commitSha: string;
  readonly cacheKey: string;
  readonly tarballFilename: string;
  readonly version: string;
  readonly sha256: string;
  readonly binarySha256: string;
};

/**
 * Resolves the external A/B baseline cache directory (outside the Git repository),
 * keeping compatibility with `TSSAE_PERF_CACHE_DIR` so perf and semantic diff share caches.
 */
export function resolveBaselineCacheDir(override?: string): string {
  const configured = override ?? process.env.TSSAE_PERF_CACHE_DIR?.trim();
  if (configured !== undefined && configured.length > 0) {
    return resolve(configured);
  }
  const xdgCache = process.env.XDG_CACHE_HOME?.trim();
  const baseCache =
    xdgCache !== undefined && xdgCache.length > 0
      ? xdgCache
      : join(homedir(), ".cache");
  return join(baseCache, "tssae", "perf-ab");
}

/** Alias retained for existing perf callers and unit tests. */
export const resolvePerfCacheDir = resolveBaselineCacheDir;

/** Finds the Git top-level directory containing `startDir`. */
export function resolveRepoRoot(startDir: string = process.cwd()): string {
  return execFileSync("git", ["rev-parse", "--show-toplevel"], {
    cwd: startDir,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
}

/** Returns current HEAD commit SHA and whether the working tree has uncommitted edits. */
export function readRepoHeadState(repoRoot: string): RepoHeadState {
  const commitSha = execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
  const porcelain = execFileSync("git", ["status", "--porcelain"], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
  return { commitSha, dirty: porcelain.length > 0 };
}

/**
 * Resolves the baseline commit via `explicitRev` or `git merge-base HEAD main`
 * (falling back to `origin/main`).
 */
export function resolveBaselineRevision(
  repoRoot: string,
  explicitRev?: string,
): ResolvedRevision {
  if (explicitRev !== undefined) {
    try {
      const commitSha = execFileSync(
        "git",
        ["rev-parse", "--verify", `${explicitRev}^{commit}`],
        {
          cwd: repoRoot,
          encoding: "utf8",
          stdio: ["ignore", "pipe", "pipe"],
        },
      ).trim();
      return { rev: explicitRev, commitSha };
    } catch (error) {
      throw new Error(
        `Failed to resolve baseline revision "${explicitRev}": ${error instanceof Error ? error.message : String(error)}`,
        { cause: error },
      );
    }
  }

  for (const candidateRef of ["main", "origin/main"]) {
    try {
      const commitSha = execFileSync(
        "git",
        ["merge-base", "HEAD", candidateRef],
        {
          cwd: repoRoot,
          encoding: "utf8",
          stdio: ["ignore", "pipe", "pipe"],
        },
      ).trim();
      if (commitSha.length > 0) {
        return { rev: candidateRef, commitSha };
      }
    } catch {
      // Try next fallback ref.
    }
  }

  throw new Error(
    "Cannot resolve git merge-base against main or origin/main (is this a shallow clone without fetch-depth: 0?). Fetch main or pass --baseline <rev>.",
  );
}

async function extractGitRevision(
  repoRoot: string,
  commitSha: string,
  destDir: string,
): Promise<void> {
  await new Promise<void>((resolveExtract, rejectExtract) => {
    const gitProc = spawn(
      "git",
      ["-C", repoRoot, "archive", "--format=tar", commitSha],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    const tarProc = spawn("tar", ["-x", "-C", destDir], {
      stdio: ["pipe", "ignore", "pipe"],
    });

    const gitStderr: string[] = [];
    const tarStderr: string[] = [];
    gitProc.stderr.setEncoding("utf8");
    tarProc.stderr.setEncoding("utf8");
    gitProc.stderr.on("data", (chunk: string) => gitStderr.push(chunk));
    tarProc.stderr.on("data", (chunk: string) => tarStderr.push(chunk));

    gitProc.stdout.pipe(tarProc.stdin);

    let gitCode: number | null = null;
    let tarCode: number | null = null;
    let settled = false;

    const finish = (): void => {
      if (settled || gitCode === null || tarCode === null) {
        return;
      }
      settled = true;
      if (gitCode !== 0 || tarCode !== 0) {
        rejectExtract(
          new Error(
            `git archive export for ${commitSha} failed (git=${String(gitCode)}, tar=${String(tarCode)}): ${gitStderr.join("")} ${tarStderr.join("")}`,
          ),
        );
      } else {
        resolveExtract();
      }
    };

    gitProc.once("error", (err) => {
      if (!settled) {
        settled = true;
        rejectExtract(err);
      }
    });
    tarProc.once("error", (err) => {
      if (!settled) {
        settled = true;
        rejectExtract(err);
      }
    });
    gitProc.once("close", (code) => {
      gitCode = code ?? 1;
      finish();
    });
    tarProc.once("close", (code) => {
      tarCode = code ?? 1;
      finish();
    });
  });
}

async function runCommand(
  command: string,
  args: readonly string[],
  cwd: string,
): Promise<void> {
  await execFileAsync(command, [...args], {
    cwd,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    env: {
      ...process.env,
      GIT_TERMINAL_PROMPT: "0",
    },
  });
}

async function tryLoadCachedBaseline(
  cacheDir: string,
  resolved: ResolvedRevision,
  cacheKey: string,
): Promise<BaselineArtifact | undefined> {
  const readyPath = join(cacheDir, "ready.json");
  if (!existsSync(readyPath)) {
    return undefined;
  }
  try {
    const metadata = JSON.parse(
      readFileSync(readyPath, "utf8"),
    ) as Partial<BaselineReadyMetadata>;
    if (
      metadata.commitSha !== resolved.commitSha ||
      metadata.cacheKey !== cacheKey ||
      typeof metadata.tarballFilename !== "string" ||
      typeof metadata.sha256 !== "string" ||
      typeof metadata.binarySha256 !== "string"
    ) {
      return undefined;
    }
    const tarballPath = join(cacheDir, metadata.tarballFilename);
    if (!existsSync(tarballPath)) {
      return undefined;
    }
    const bundle = await packWorkspaceArtifacts({
      packages: ["tssae"],
      fromPackDir: cacheDir,
    });
    const artifact = bundle.artifacts[0];
    if (
      artifact === undefined ||
      artifact.sha256 !== metadata.sha256 ||
      artifact.binarySha256 !== metadata.binarySha256
    ) {
      return undefined;
    }
    return {
      rev: resolved.rev,
      commitSha: resolved.commitSha,
      cacheKey,
      cacheDir,
      tarballPath: artifact.filename,
      version: artifact.version,
      sha256: artifact.sha256,
      binarySha256: artifact.binarySha256,
      cacheHit: true,
    };
  } catch {
    return undefined;
  }
}

/**
 * Builds and caches the baseline `tssae` tarball from an isolated `git archive`
 * snapshot so the active working tree and `.git/worktrees` remain untouched.
 */
export async function prepareBaselineArtifact(
  options: PrepareBaselineArtifactOptions = {},
): Promise<BaselineArtifact> {
  const repoRoot = options.repoRoot ?? resolveRepoRoot();
  const resolved = resolveBaselineRevision(repoRoot, options.baselineRev);
  const nodeMajor = process.versions.node.split(".")[0] ?? "0";
  const cacheKey = `${resolved.commitSha}-${process.platform}-${process.arch}-node${nodeMajor}`;
  const cacheRoot = resolveBaselineCacheDir(options.cacheDir);
  const baselinesRoot = join(cacheRoot, "baselines");
  const cacheDir = join(baselinesRoot, cacheKey);

  mkdirSync(baselinesRoot, { recursive: true });

  if (options.refreshBaseline !== true) {
    const cached = await tryLoadCachedBaseline(cacheDir, resolved, cacheKey);
    if (cached !== undefined) {
      return cached;
    }
  }

  const stagingRoot = mkdtempSync(
    join(cacheRoot, `.staging-${cacheKey.slice(0, 12)}-`),
  );
  const sourceDir = join(stagingRoot, "src");
  const outDir = join(stagingRoot, "out");
  mkdirSync(sourceDir, { recursive: true });
  mkdirSync(outDir, { recursive: true });

  try {
    await extractGitRevision(repoRoot, resolved.commitSha, sourceDir);
    await runCommand("pnpm", ["install", "--frozen-lockfile"], sourceDir);
    await runCommand("pnpm", ["--filter", "tssae", "build"], sourceDir);
    await runCommand(
      "pnpm",
      ["--filter", "tssae", "pack", "--pack-destination", outDir],
      sourceDir,
    );

    const bundle = await packWorkspaceArtifacts({
      packages: ["tssae"],
      fromPackDir: outDir,
    });
    const artifact = bundle.artifacts[0];
    if (artifact === undefined || artifact.binarySha256 === undefined) {
      throw new Error(
        `Baseline tssae tarball for ${resolved.commitSha} is missing expected package or engine binary.`,
      );
    }

    const tarballFilename = basename(artifact.filename);
    const readyMetadata: BaselineReadyMetadata = {
      rev: resolved.rev,
      commitSha: resolved.commitSha,
      cacheKey,
      tarballFilename,
      version: artifact.version,
      sha256: artifact.sha256,
      binarySha256: artifact.binarySha256,
    };
    writeFileSync(
      join(outDir, "ready.json"),
      `${JSON.stringify(readyMetadata, null, 2)}\n`,
    );

    rmSync(cacheDir, { recursive: true, force: true });
    renameSync(outDir, cacheDir);

    return {
      rev: resolved.rev,
      commitSha: resolved.commitSha,
      cacheKey,
      cacheDir,
      tarballPath: join(cacheDir, tarballFilename),
      version: artifact.version,
      sha256: artifact.sha256,
      binarySha256: artifact.binarySha256,
      cacheHit: false,
    };
  } finally {
    rmSync(stagingRoot, { recursive: true, force: true });
  }
}
