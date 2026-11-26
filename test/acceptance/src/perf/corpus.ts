import { execFile, spawn } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";

import type {
  CorpusProfileId,
  CorpusProfileSpec,
  MaterializedCorpus,
} from "./types.ts";

const execFileAsync = promisify(execFile);
const VSCODE_COMMIT_SHA = "3fc62371e017d5e6d3d742b7aa79832c9fc9e71d";
const VSCODE_REPO_URL = "https://github.com/microsoft/vscode.git";
const DEFAULT_HOST_MIRROR_PATHS: readonly string[] = [
  "/root/.cache/verule/host-repos/vscode.git",
];
const RETRY_DELAYS_MS = [1_000, 3_000] as const;
const SOURCE_EXTENSIONS = [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx"];

/** Built-in corpus profiles sharing the same fixed-commit VS Code checkout pipeline. */
export const CORPUS_PROFILES: Readonly<
  Record<CorpusProfileId, CorpusProfileSpec>
> = {
  medium: {
    id: "medium",
    repoUrl: VSCODE_REPO_URL,
    repoName: "vscode",
    commitSha: VSCODE_COMMIT_SHA,
    subsets: ["src/vs/base", "src/typings", "src/tsconfig.base.json"],
    includeGlobs: ["src/vs/base/**/*.ts", "src/typings/**/*.d.ts"],
    primaryFile: "src/vs/base/common/lifecycle.ts",
    secondaryFile: "src/vs/base/common/event.ts",
    targetCalleeName: "toDisposable",
  },
  large: {
    id: "large",
    repoUrl: VSCODE_REPO_URL,
    repoName: "vscode",
    commitSha: VSCODE_COMMIT_SHA,
    subsets: [
      "src/vs/base",
      "src/vs/platform",
      "src/typings",
      "src/tsconfig.base.json",
    ],
    includeGlobs: [
      "src/vs/base/**/*.ts",
      "src/vs/platform/**/*.ts",
      "src/typings/**/*.d.ts",
    ],
    primaryFile: "src/vs/base/common/lifecycle.ts",
    secondaryFile: "src/vs/base/common/event.ts",
    targetCalleeName: "toDisposable",
  },
};

export type MaterializeCorpusOptions = {
  readonly profile: CorpusProfileId;
  readonly cacheDir?: string;
  readonly refreshCorpus?: boolean;
  readonly noCorpusCache?: boolean;
  readonly hostMirrorPaths?: readonly string[];
  readonly specOverride?: CorpusProfileSpec;
};

type CorpusReadyMetadata = {
  readonly profile: CorpusProfileId;
  readonly repoName: string;
  readonly commitSha: string;
  readonly subsets: readonly string[];
  readonly fileCount: number;
};

/** Resolves the external corpus cache directory (never inside the Git repository). */
export function resolveCorpusCacheDir(override?: string): string {
  const configured = override ?? process.env.TSSAE_CORPUS_CACHE_DIR?.trim();
  if (configured !== undefined && configured.length > 0) {
    return resolve(configured);
  }
  const xdgCache = process.env.XDG_CACHE_HOME?.trim();
  const baseCache =
    xdgCache !== undefined && xdgCache.length > 0
      ? xdgCache
      : join(homedir(), ".cache");
  return join(baseCache, "tssae", "corpora");
}

async function runGit(
  args: readonly string[],
  cwd?: string,
): Promise<{ readonly stdout: string; readonly stderr: string }> {
  return execFileAsync("git", [...args], {
    ...(cwd !== undefined ? { cwd } : {}),
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
    env: {
      ...process.env,
      GIT_TERMINAL_PROMPT: "0",
    },
  });
}

async function hasCommit(bareDir: string, commitSha: string): Promise<boolean> {
  if (!existsSync(bareDir)) {
    return false;
  }
  try {
    await runGit(["-C", bareDir, "cat-file", "-e", `${commitSha}^{commit}`]);
    return true;
  } catch {
    return false;
  }
}

async function sleep(ms: number): Promise<void> {
  await new Promise<void>((resolveTimer) => {
    setTimeout(resolveTimer, ms);
  });
}

async function fetchCommitWithRetry(
  bareDir: string,
  sourceUrl: string,
  commitSha: string,
): Promise<void> {
  let lastError: unknown;
  for (let attempt = 0; attempt <= RETRY_DELAYS_MS.length; attempt += 1) {
    try {
      await runGit([
        "-C",
        bareDir,
        "fetch",
        "--quiet",
        "--depth=1",
        sourceUrl,
        commitSha,
      ]);
      await runGit([
        "-C",
        bareDir,
        "update-ref",
        `refs/tssae/${commitSha}`,
        commitSha,
      ]);
      return;
    } catch (error) {
      lastError = error;
      const delay = RETRY_DELAYS_MS[attempt];
      if (delay !== undefined) {
        await sleep(delay);
      }
    }
  }
  throw new Error(
    `Failed to fetch commit ${commitSha} from ${sourceUrl}: ${lastError instanceof Error ? lastError.message : String(lastError)}`,
  );
}

async function ensureBareRepoCommit(options: {
  readonly bareDir: string;
  readonly repoUrl: string;
  readonly commitSha: string;
  readonly bypassCache: boolean;
  readonly hostMirrorPaths: readonly string[];
}): Promise<"cache" | "host-mirror" | "remote"> {
  const { bareDir, repoUrl, commitSha, bypassCache, hostMirrorPaths } = options;

  if (bypassCache && existsSync(bareDir)) {
    rmSync(bareDir, { recursive: true, force: true });
  }

  if (!existsSync(bareDir)) {
    mkdirSync(bareDir, { recursive: true });
    await runGit(["init", "--bare", "--quiet", bareDir]);
    await runGit([
      "-C",
      bareDir,
      "config",
      "uploadpack.allowAnySHA1InWant",
      "true",
    ]);
  }

  if (!bypassCache && (await hasCommit(bareDir, commitSha))) {
    return "cache";
  }

  for (const mirrorPath of hostMirrorPaths) {
    if (await hasCommit(mirrorPath, commitSha)) {
      await fetchCommitWithRetry(bareDir, mirrorPath, commitSha);
      return "host-mirror";
    }
  }

  await fetchCommitWithRetry(bareDir, repoUrl, commitSha);
  return "remote";
}

async function extractGitArchiveSubsets(
  bareDir: string,
  commitSha: string,
  subsets: readonly string[],
  destDir: string,
): Promise<void> {
  await new Promise<void>((resolveArchive, rejectArchive) => {
    const gitProc = spawn(
      "git",
      ["-C", bareDir, "archive", "--format=tar", commitSha, "--", ...subsets],
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
        rejectArchive(
          new Error(
            `git archive / tar extraction failed (git=${String(gitCode)}, tar=${String(tarCode)}): ${gitStderr.join("")} ${tarStderr.join("")}`,
          ),
        );
      } else {
        resolveArchive();
      }
    };

    gitProc.once("error", (err) => {
      if (!settled) {
        settled = true;
        rejectArchive(err);
      }
    });
    tarProc.once("error", (err) => {
      if (!settled) {
        settled = true;
        rejectArchive(err);
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

function countSourceFiles(dir: string): number {
  let count = 0;
  const stack = [dir];
  while (stack.length > 0) {
    const current = stack.pop()!;
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const fullPath = join(current, entry.name);
      if (entry.isDirectory()) {
        stack.push(fullPath);
      } else if (
        entry.isFile() &&
        SOURCE_EXTENSIONS.some((ext) => entry.name.endsWith(ext))
      ) {
        count += 1;
      }
    }
  }
  return count;
}

function isValidReadyCheckout(
  checkoutDir: string,
  spec: CorpusProfileSpec,
): CorpusReadyMetadata | undefined {
  const readyPath = join(checkoutDir, ".corpus-ready.json");
  const tsconfigPath = join(checkoutDir, "tsconfig.perf.json");
  if (!existsSync(readyPath) || !existsSync(tsconfigPath)) {
    return undefined;
  }
  try {
    const parsed = JSON.parse(
      readFileSync(readyPath, "utf8"),
    ) as Partial<CorpusReadyMetadata>;
    if (
      parsed.profile !== spec.id ||
      parsed.commitSha !== spec.commitSha ||
      parsed.repoName !== spec.repoName ||
      typeof parsed.fileCount !== "number" ||
      parsed.fileCount <= 0 ||
      !Array.isArray(parsed.subsets) ||
      parsed.subsets.join("\0") !== spec.subsets.join("\0")
    ) {
      return undefined;
    }
    return parsed as CorpusReadyMetadata;
  } catch {
    return undefined;
  }
}

/**
 * Materializes a fixed-commit corpus subset in the external cache directory,
 * preferring local host mirror repositories and supporting explicit cache bypass.
 */
export async function materializeCorpus(
  options: MaterializeCorpusOptions,
): Promise<MaterializedCorpus> {
  const spec = options.specOverride ?? CORPUS_PROFILES[options.profile];
  const cacheRoot = resolveCorpusCacheDir(options.cacheDir);
  const bypassCache =
    options.refreshCorpus === true || options.noCorpusCache === true;
  const bareRepoDir = join(cacheRoot, "repos", `${spec.repoName}.git`);
  const checkoutsDir = join(cacheRoot, "checkouts");
  const shortSha = spec.commitSha.slice(0, 12);
  const corpusDir = join(
    checkoutsDir,
    `${spec.repoName}-${shortSha}-${spec.id}`,
  );
  const tsconfigPath = join(corpusDir, "tsconfig.perf.json");

  mkdirSync(checkoutsDir, { recursive: true });

  if (!bypassCache) {
    const ready = isValidReadyCheckout(corpusDir, spec);
    if (ready !== undefined) {
      return {
        profile: spec.id,
        repoUrl: spec.repoUrl,
        repoName: spec.repoName,
        commitSha: spec.commitSha,
        subsets: spec.subsets,
        corpusDir,
        bareRepoDir,
        tsconfigPath,
        fileCount: ready.fileCount,
        primaryFile: spec.primaryFile,
        secondaryFile: spec.secondaryFile,
        targetCalleeName: spec.targetCalleeName,
        cacheHit: true,
        cacheBypassed: false,
        sourceOrigin: "cache",
      };
    }
  }

  const hostMirrors =
    options.hostMirrorPaths ??
    (process.env.TSSAE_CORPUS_HOST_MIRROR_DIR !== undefined
      ? [process.env.TSSAE_CORPUS_HOST_MIRROR_DIR]
      : DEFAULT_HOST_MIRROR_PATHS);

  const sourceOrigin = await ensureBareRepoCommit({
    bareDir: bareRepoDir,
    repoUrl: spec.repoUrl,
    commitSha: spec.commitSha,
    bypassCache,
    hostMirrorPaths: hostMirrors,
  });

  const stagingDir = mkdtempSync(
    join(checkoutsDir, `.staging-${spec.repoName}-${shortSha}-${spec.id}-`),
  );
  try {
    await extractGitArchiveSubsets(
      bareRepoDir,
      spec.commitSha,
      spec.subsets,
      stagingDir,
    );
    writeFileSync(
      join(stagingDir, "tsconfig.perf.json"),
      `${JSON.stringify(
        {
          extends: "./src/tsconfig.base.json",
          compilerOptions: {
            noEmit: true,
          },
          include: [...spec.includeGlobs],
        },
        null,
        2,
      )}\n`,
    );
    const fileCount = countSourceFiles(join(stagingDir, "src"));
    const readyMetadata: CorpusReadyMetadata = {
      profile: spec.id,
      repoName: spec.repoName,
      commitSha: spec.commitSha,
      subsets: spec.subsets,
      fileCount,
    };
    writeFileSync(
      join(stagingDir, ".corpus-ready.json"),
      `${JSON.stringify(readyMetadata, null, 2)}\n`,
    );

    rmSync(corpusDir, { recursive: true, force: true });
    renameSync(stagingDir, corpusDir);

    return {
      profile: spec.id,
      repoUrl: spec.repoUrl,
      repoName: spec.repoName,
      commitSha: spec.commitSha,
      subsets: spec.subsets,
      corpusDir,
      bareRepoDir,
      tsconfigPath,
      fileCount,
      primaryFile: spec.primaryFile,
      secondaryFile: spec.secondaryFile,
      targetCalleeName: spec.targetCalleeName,
      cacheHit: false,
      cacheBypassed: bypassCache,
      sourceOrigin,
    };
  } catch (error) {
    rmSync(stagingDir, { recursive: true, force: true });
    throw error;
  }
}
