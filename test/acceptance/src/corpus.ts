import { execFile, spawn } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { basename, join, relative, resolve, sep } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const RETRY_DELAYS_MS = [1_000, 3_000] as const;
const DEFAULT_SOURCE_EXTENSIONS: readonly string[] = [
  ".ts",
  ".tsx",
  ".mts",
  ".cts",
  ".js",
  ".jsx",
  ".mjs",
  ".cjs",
];
const DEFAULT_EXCLUDE_DIR_NAMES: readonly string[] = [".git", "node_modules"];
const COMMIT_SHA_PATTERN = /^[0-9a-f]{40}$/iu;

export type CorpusSourceOrigin = "cache" | "host-mirror" | "remote";

export type CorpusFileMatchRule = {
  /** File extensions included when counting corpus files. */
  readonly extensions?: readonly string[];
  /** Relative paths inside the checkout to scan (defaults to `sparsePaths`). */
  readonly scanPaths?: readonly string[];
  /** Directory names skipped during traversal (`.git` is always skipped). */
  readonly excludeDirNames?: readonly string[];
  /** Filename suffixes excluded from counting (e.g. `["_FIXTURE.js"]`). */
  readonly excludeFileSuffixes?: readonly string[];
  /** Optional predicate applied to POSIX-normalized relative file paths. */
  readonly predicate?: (relativePath: string) => boolean;
};

/** Generic fixed-commit external corpus specification shared across verification suites. */
export type ExternalCorpusSpec = {
  readonly id: string;
  readonly repoUrl: string;
  readonly repoName: string;
  readonly commitSha: string;
  readonly sparsePaths: readonly string[];
  readonly checkoutDirName?: string;
  readonly hostMirrorPaths?: readonly string[];
  readonly fileMatch?: CorpusFileMatchRule;
  readonly minFileCount?: number;
};

export type MaterializeExternalCorpusOptions = {
  readonly spec: ExternalCorpusSpec;
  readonly cacheDir?: string;
  readonly refreshCorpus?: boolean;
  readonly noCorpusCache?: boolean;
  readonly hostMirrorPaths?: readonly string[];
  readonly isCheckoutValid?: (checkoutDir: string) => boolean;
  readonly onAfterExtract?: (context: {
    readonly stagingDir: string;
    readonly spec: ExternalCorpusSpec;
    readonly fileCount: number;
  }) => void | Promise<void>;
};

export type MaterializedExternalCorpus = {
  readonly id: string;
  readonly repoUrl: string;
  readonly repoName: string;
  readonly commitSha: string;
  readonly sparsePaths: readonly string[];
  readonly corpusDir: string;
  readonly bareRepoDir: string;
  readonly fileCount: number;
  readonly cacheHit: boolean;
  readonly cacheBypassed: boolean;
  readonly sourceOrigin: CorpusSourceOrigin;
};

type CorpusReadyMetadata = {
  readonly id?: string;
  readonly profile?: string;
  readonly repoName: string;
  readonly commitSha: string;
  readonly subsets: readonly string[];
  readonly fileCount: number;
  readonly matchSignature?: string;
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

/** Derives the checkout directory name under `checkouts/` for a corpus specification. */
export function resolveCorpusCheckoutDirName(
  spec: Pick<
    ExternalCorpusSpec,
    "id" | "repoName" | "commitSha" | "checkoutDirName"
  >,
): string {
  if (spec.checkoutDirName !== undefined) {
    const trimmed = spec.checkoutDirName.trim();
    if (
      trimmed.length === 0 ||
      trimmed === "." ||
      trimmed === ".." ||
      trimmed.includes("/") ||
      trimmed.includes("\\")
    ) {
      throw new Error(
        `Invalid checkoutDirName "${spec.checkoutDirName}": expected a single directory name.`,
      );
    }
    return trimmed;
  }
  if (spec.id === spec.repoName) {
    return `${spec.repoName}-${spec.commitSha}`;
  }
  return `${spec.repoName}-${spec.commitSha}-${spec.id}`;
}

function buildMatchSignature(spec: ExternalCorpusSpec): string {
  const rule = spec.fileMatch;
  return JSON.stringify({
    extensions: [...(rule?.extensions ?? DEFAULT_SOURCE_EXTENSIONS)],
    scanPaths: [...(rule?.scanPaths ?? spec.sparsePaths)],
    excludeDirNames: [...(rule?.excludeDirNames ?? DEFAULT_EXCLUDE_DIR_NAMES)],
    excludeFileSuffixes: [...(rule?.excludeFileSuffixes ?? [])],
  });
}

function normalizePosixRelPath(relPath: string): string {
  return sep === "/" ? relPath : relPath.split(sep).join("/");
}

/** Collects sorted POSIX-relative paths inside `rootDir` matching the corpus `fileMatch` rule. */
export function collectMatchingCorpusFiles(
  rootDir: string,
  spec: Pick<ExternalCorpusSpec, "sparsePaths" | "fileMatch">,
): readonly string[] {
  const rule = spec.fileMatch;
  const extensions = rule?.extensions ?? DEFAULT_SOURCE_EXTENSIONS;
  const scanPaths = rule?.scanPaths ?? spec.sparsePaths;
  const excludeDirs = new Set([
    ".git",
    ...(rule?.excludeDirNames ?? DEFAULT_EXCLUDE_DIR_NAMES),
  ]);
  const excludeSuffixes = rule?.excludeFileSuffixes ?? [];
  const predicate = rule?.predicate;

  const matched = new Set<string>();

  const matchesFile = (relPath: string, fileName: string): boolean => {
    if (!extensions.some((ext) => fileName.endsWith(ext))) {
      return false;
    }
    if (excludeSuffixes.some((suffix) => fileName.endsWith(suffix))) {
      return false;
    }
    if (predicate !== undefined && !predicate(relPath)) {
      return false;
    }
    return true;
  };

  for (const scanPath of scanPaths) {
    const target = resolve(rootDir, scanPath);
    if (!existsSync(target)) {
      continue;
    }
    const targetStat = statSync(target);
    if (targetStat.isFile()) {
      const relPath = normalizePosixRelPath(relative(rootDir, target));
      if (matchesFile(relPath, basename(target))) {
        matched.add(relPath);
      }
      continue;
    }
    if (!targetStat.isDirectory()) {
      continue;
    }
    const stack = [target];
    while (stack.length > 0) {
      const current = stack.pop()!;
      for (const entry of readdirSync(current, { withFileTypes: true })) {
        if (entry.isDirectory()) {
          if (!excludeDirs.has(entry.name)) {
            stack.push(join(current, entry.name));
          }
          continue;
        }
        if (entry.isFile()) {
          const fullPath = join(current, entry.name);
          const relPath = normalizePosixRelPath(relative(rootDir, fullPath));
          if (matchesFile(relPath, entry.name)) {
            matched.add(relPath);
          }
        }
      }
    }
  }

  return [...matched].sort();
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
}): Promise<CorpusSourceOrigin> {
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
  sparsePaths: readonly string[],
  destDir: string,
): Promise<void> {
  await new Promise<void>((resolveArchive, rejectArchive) => {
    const gitProc = spawn(
      "git",
      [
        "-C",
        bareDir,
        "archive",
        "--format=tar",
        commitSha,
        "--",
        ...sparsePaths,
      ],
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

function allSparsePathsExist(
  checkoutDir: string,
  sparsePaths: readonly string[],
): boolean {
  return sparsePaths.every((sparsePath) =>
    existsSync(join(checkoutDir, sparsePath)),
  );
}

function tryReadReadyCheckout(
  checkoutDir: string,
  spec: ExternalCorpusSpec,
  minFileCount: number,
  matchSignature: string,
): number | undefined {
  const readyPath = join(checkoutDir, ".corpus-ready.json");
  if (!existsSync(readyPath)) {
    return undefined;
  }
  try {
    const parsed = JSON.parse(
      readFileSync(readyPath, "utf8"),
    ) as Partial<CorpusReadyMetadata>;
    const readyId = parsed.id ?? parsed.profile;
    if (
      readyId !== spec.id ||
      parsed.commitSha !== spec.commitSha ||
      parsed.repoName !== spec.repoName ||
      typeof parsed.fileCount !== "number" ||
      !Number.isInteger(parsed.fileCount) ||
      parsed.fileCount < minFileCount ||
      !Array.isArray(parsed.subsets) ||
      parsed.subsets.join("\0") !== spec.sparsePaths.join("\0")
    ) {
      return undefined;
    }
    if (
      parsed.matchSignature !== undefined &&
      parsed.matchSignature !== matchSignature
    ) {
      return undefined;
    }
    if (!allSparsePathsExist(checkoutDir, spec.sparsePaths)) {
      return undefined;
    }
    return parsed.fileCount;
  } catch {
    return undefined;
  }
}

async function tryAdoptGitCheckout(
  checkoutDir: string,
  spec: ExternalCorpusSpec,
  minFileCount: number,
  matchSignature: string,
): Promise<number | undefined> {
  if (!existsSync(join(checkoutDir, ".git"))) {
    return undefined;
  }
  try {
    const { stdout } = await runGit(["-C", checkoutDir, "rev-parse", "HEAD"]);
    if (stdout.trim() !== spec.commitSha) {
      return undefined;
    }
  } catch {
    return undefined;
  }

  if (!allSparsePathsExist(checkoutDir, spec.sparsePaths)) {
    return undefined;
  }

  const fileCount = collectMatchingCorpusFiles(checkoutDir, spec).length;
  if (fileCount < minFileCount) {
    return undefined;
  }

  const readyMetadata: CorpusReadyMetadata = {
    id: spec.id,
    repoName: spec.repoName,
    commitSha: spec.commitSha,
    subsets: spec.sparsePaths,
    fileCount,
    matchSignature,
  };
  try {
    writeFileSync(
      join(checkoutDir, ".corpus-ready.json"),
      `${JSON.stringify(readyMetadata, null, 2)}\n`,
    );
  } catch {
    // Read-only pre-populated cache directories remain valid without persisting metadata.
  }
  return fileCount;
}

function resolveHostMirrors(
  options: MaterializeExternalCorpusOptions,
): readonly string[] {
  if (options.hostMirrorPaths !== undefined) {
    return options.hostMirrorPaths;
  }
  if (options.spec.hostMirrorPaths !== undefined) {
    return options.spec.hostMirrorPaths;
  }
  const envMirrorDir = process.env.TSSAE_CORPUS_HOST_MIRROR_DIR?.trim();
  if (envMirrorDir !== undefined && envMirrorDir.length > 0) {
    return [envMirrorDir, join(envMirrorDir, `${options.spec.repoName}.git`)];
  }
  return [`/root/.cache/verule/host-repos/${options.spec.repoName}.git`];
}

/**
 * Materializes a fixed-commit sparse external corpus in the cache directory,
 * supporting `.corpus-ready.json` and existing `.git` sparse worktree hits,
 * local mirror reuse, corruption rebuild, and explicit cache bypass.
 */
export async function materializeExternalCorpus(
  options: MaterializeExternalCorpusOptions,
): Promise<MaterializedExternalCorpus> {
  const { spec } = options;
  if (!COMMIT_SHA_PATTERN.test(spec.commitSha)) {
    throw new Error(
      `Invalid commitSha "${spec.commitSha}" for corpus "${spec.id}": expected a 40-character hexadecimal SHA.`,
    );
  }
  if (spec.sparsePaths.length === 0) {
    throw new Error(
      `Corpus "${spec.id}" must declare at least one sparse path.`,
    );
  }

  const minFileCount = spec.minFileCount ?? 1;
  const cacheRoot = resolveCorpusCacheDir(options.cacheDir);
  const bypassCache =
    options.refreshCorpus === true || options.noCorpusCache === true;
  const bareRepoDir = join(cacheRoot, "repos", `${spec.repoName}.git`);
  const checkoutsDir = join(cacheRoot, "checkouts");
  const checkoutDirName = resolveCorpusCheckoutDirName(spec);
  const corpusDir = join(checkoutsDir, checkoutDirName);
  const matchSignature = buildMatchSignature(spec);

  mkdirSync(checkoutsDir, { recursive: true });

  if (!bypassCache && existsSync(corpusDir)) {
    const customValid = options.isCheckoutValid?.(corpusDir) ?? true;
    if (customValid) {
      const readyFileCount = tryReadReadyCheckout(
        corpusDir,
        spec,
        minFileCount,
        matchSignature,
      );
      if (readyFileCount !== undefined) {
        return {
          id: spec.id,
          repoUrl: spec.repoUrl,
          repoName: spec.repoName,
          commitSha: spec.commitSha,
          sparsePaths: spec.sparsePaths,
          corpusDir,
          bareRepoDir,
          fileCount: readyFileCount,
          cacheHit: true,
          cacheBypassed: false,
          sourceOrigin: "cache",
        };
      }

      if (!existsSync(join(corpusDir, ".corpus-ready.json"))) {
        const adoptedFileCount = await tryAdoptGitCheckout(
          corpusDir,
          spec,
          minFileCount,
          matchSignature,
        );
        if (adoptedFileCount !== undefined) {
          return {
            id: spec.id,
            repoUrl: spec.repoUrl,
            repoName: spec.repoName,
            commitSha: spec.commitSha,
            sparsePaths: spec.sparsePaths,
            corpusDir,
            bareRepoDir,
            fileCount: adoptedFileCount,
            cacheHit: true,
            cacheBypassed: false,
            sourceOrigin: "cache",
          };
        }
      }
    }
  }

  const existingGitHasCommit =
    existsSync(join(corpusDir, ".git")) &&
    (await hasCommit(corpusDir, spec.commitSha));
  const baseMirrors = resolveHostMirrors(options);
  const hostMirrors = existingGitHasCommit
    ? [...baseMirrors, corpusDir]
    : baseMirrors;
  const sourceOrigin = await ensureBareRepoCommit({
    bareDir: bareRepoDir,
    repoUrl: spec.repoUrl,
    commitSha: spec.commitSha,
    bypassCache,
    hostMirrorPaths: hostMirrors,
  });

  const stagingDir = mkdtempSync(
    join(checkoutsDir, `.staging-${checkoutDirName}-`),
  );
  try {
    await extractGitArchiveSubsets(
      bareRepoDir,
      spec.commitSha,
      spec.sparsePaths,
      stagingDir,
    );
    const fileCount = collectMatchingCorpusFiles(stagingDir, spec).length;
    if (fileCount < minFileCount) {
      throw new Error(
        `Corpus "${spec.id}" (${spec.repoName}@${spec.commitSha}) matched ${String(fileCount)} files, below minFileCount=${String(minFileCount)}.`,
      );
    }

    if (options.onAfterExtract !== undefined) {
      await options.onAfterExtract({ stagingDir, spec, fileCount });
    }

    const readyMetadata: CorpusReadyMetadata = {
      id: spec.id,
      repoName: spec.repoName,
      commitSha: spec.commitSha,
      subsets: spec.sparsePaths,
      fileCount,
      matchSignature,
    };
    writeFileSync(
      join(stagingDir, ".corpus-ready.json"),
      `${JSON.stringify(readyMetadata, null, 2)}\n`,
    );

    rmSync(corpusDir, { recursive: true, force: true });
    renameSync(stagingDir, corpusDir);

    return {
      id: spec.id,
      repoUrl: spec.repoUrl,
      repoName: spec.repoName,
      commitSha: spec.commitSha,
      sparsePaths: spec.sparsePaths,
      corpusDir,
      bareRepoDir,
      fileCount,
      cacheHit: false,
      cacheBypassed: bypassCache,
      sourceOrigin,
    };
  } catch (error) {
    rmSync(stagingDir, { recursive: true, force: true });
    throw error;
  }
}
