import { execFile, execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, delimiter, dirname, join, resolve } from "node:path";

export type IsolationMode = "bwrap" | "node-permission";

export type IsolationEvidence = {
  readonly mode: IsolationMode;
  readonly blockedPaths: readonly string[];
  readonly controlProbeBlocked: boolean;
  readonly controlProbeError: string;
};

export type IsolatedRunResult = {
  readonly status: number;
  readonly stdout: string;
  readonly stderr: string;
  readonly durationMs: number;
  readonly timedOut: boolean;
  readonly signal: NodeJS.Signals | null;
  readonly isolation?: IsolationEvidence;
};

export type RunInIsolatedHostOptions = {
  readonly checkoutDir: string;
  readonly command: string;
  readonly args: readonly string[];
  readonly isolated?: boolean;
  readonly pnpmStoreDir?: string;
  readonly blockedPaths?: readonly string[];
  readonly sharedReadWritePaths?: readonly string[];
  readonly allowNet?: boolean;
  readonly env?: Readonly<Record<string, string | undefined>>;
  readonly timeoutMs?: number;
};

const maxOutputBytes = 16 * 1024 * 1024;
const defaultTimeoutMs = 30_000;
const bwrapProbeCache = new Map<string, Promise<boolean>>();

function runAsyncExec(
  command: string,
  args: readonly string[],
  options: {
    readonly cwd: string;
    readonly env: Readonly<Record<string, string | undefined>>;
    readonly timeoutMs: number;
  },
): Promise<{
  readonly status: number;
  readonly stdout: string;
  readonly stderr: string;
  readonly durationMs: number;
  readonly timedOut: boolean;
  readonly signal: NodeJS.Signals | null;
}> {
  return new Promise((resolveResult) => {
    const startedAt = performance.now();
    execFile(
      command,
      args,
      {
        cwd: options.cwd,
        env: options.env,
        encoding: "utf8",
        timeout: options.timeoutMs,
        maxBuffer: maxOutputBytes,
      },
      (error, stdout, stderr) => {
        const durationMs = performance.now() - startedAt;
        if (error === null) {
          resolveResult({
            status: 0,
            stdout,
            stderr,
            durationMs,
            timedOut: false,
            signal: null,
          });
          return;
        }
        const timedOut =
          error.killed === true && !error.message.includes("maxBuffer");
        resolveResult({
          status: typeof error.code === "number" ? error.code : 1,
          stdout,
          stderr: stderr.length > 0 ? stderr : error.message,
          durationMs,
          timedOut,
          signal: error.signal ?? null,
        });
      },
    );
  });
}

/**
 * Discovers development repository paths that must remain unreadable to the
 * inspected host process.
 */
export function resolveDefaultBlockedPaths(
  cwd: string = process.cwd(),
): readonly string[] {
  const candidates = new Set<string>();
  try {
    const topLevel = execFileSync("git", ["rev-parse", "--show-toplevel"], {
      cwd,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
    if (topLevel.length > 0 && existsSync(topLevel)) {
      const repoRealpath = realpathSync(topLevel);
      candidates.add(repoRealpath);
      const reposParent = dirname(repoRealpath);
      if (basename(reposParent) === "repos") {
        const workspaceRoot = dirname(reposParent);
        if (
          existsSync(join(workspaceRoot, "repos", "verule")) ||
          existsSync(join(workspaceRoot, "repos", "tssae"))
        ) {
          candidates.add(workspaceRoot);
        }
      }
    }
  } catch {
    // Ignore when invoked outside a git repository.
  }
  if (existsSync("/workspaces/verule")) {
    candidates.add(realpathSync("/workspaces/verule"));
  }
  return [...candidates];
}

function resolvePnpmStorePath(
  cwd: string,
  env: Readonly<Record<string, string | undefined>>,
  storeDir?: string,
): string | undefined {
  if (storeDir !== undefined && existsSync(storeDir)) {
    return realpathSync(storeDir);
  }
  try {
    const storePath = execFileSync("pnpm", ["store", "path"], {
      cwd,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...env },
    }).trim();
    return storePath.length > 0 && existsSync(storePath)
      ? realpathSync(storePath)
      : undefined;
  } catch {
    return undefined;
  }
}

function canUseBwrap(
  blockedPaths: readonly string[],
  cwd: string,
): Promise<boolean> {
  if (process.platform !== "linux") {
    return Promise.resolve(false);
  }
  const cacheKey = blockedPaths.join("\0");
  const cached = bwrapProbeCache.get(cacheKey);
  if (cached !== undefined) {
    return cached;
  }
  const tmpfsArgs = blockedPaths.flatMap((path) => ["--tmpfs", path]);
  const probePromise = runAsyncExec(
    "bwrap",
    [
      "--ro-bind",
      "/",
      "/",
      "--dev",
      "/dev",
      "--proc",
      "/proc",
      ...tmpfsArgs,
      "--",
      "true",
    ],
    { cwd, env: process.env, timeoutMs: 5_000 },
  )
    .then((probe) => probe.status === 0)
    .catch(() => false);
  bwrapProbeCache.set(cacheKey, probePromise);
  return probePromise;
}

function buildNodePermissionOptions(options: {
  readonly checkoutRealpath: string;
  readonly isolatedTmpDir: string;
  readonly sharedReadWritePaths: readonly string[];
  readonly allowNet: boolean;
  readonly pnpmStorePath?: string;
}): string {
  const readAllowed = new Set<string>([
    options.checkoutRealpath,
    options.isolatedTmpDir,
    ...options.sharedReadWritePaths,
    dirname(process.execPath),
    dirname(realpathSync(process.execPath)),
  ]);
  for (const systemDir of [
    "/pnpm/store",
    "/usr",
    "/lib",
    "/lib64",
    "/etc",
    "/proc",
    "/dev",
  ]) {
    if (existsSync(systemDir)) {
      readAllowed.add(systemDir);
    }
  }
  if (options.pnpmStorePath !== undefined) {
    readAllowed.add(options.pnpmStorePath);
  }

  const writeAllowed = [
    ...new Set([
      options.checkoutRealpath,
      options.isolatedTmpDir,
      ...options.sharedReadWritePaths,
    ]),
  ];
  const flags = [
    "--permission",
    ...[...readAllowed].map((path) => `--allow-fs-read=${path}`),
    ...writeAllowed.map((path) => `--allow-fs-write=${path}`),
    ...(options.allowNet ? ["--allow-net"] : []),
    "--allow-child-process",
    "--allow-worker",
  ];
  return flags.join(" ");
}

async function runControlProbe(options: {
  readonly mode: IsolationMode;
  readonly blockedPaths: readonly string[];
  readonly checkoutRealpath: string;
  readonly env: Readonly<Record<string, string | undefined>>;
  readonly bwrapPrefixArgs: readonly string[];
}): Promise<{ readonly blocked: boolean; readonly errorMessage: string }> {
  const probeTarget =
    options.blockedPaths.find((path) =>
      existsSync(join(path, "package.json")),
    ) ?? options.blockedPaths[0];
  if (probeTarget === undefined) {
    throw new Error(
      "At least one blocked path is required to verify dev-repo read isolation",
    );
  }
  const probeFile = existsSync(join(probeTarget, "package.json"))
    ? join(probeTarget, "package.json")
    : probeTarget;
  const probeScript =
    "const fs = require('node:fs'); fs.statSync(process.argv[1]);";

  const command = options.mode === "bwrap" ? "bwrap" : process.execPath;
  const args =
    options.mode === "bwrap"
      ? [
          ...options.bwrapPrefixArgs,
          "--",
          process.execPath,
          "-e",
          probeScript,
          probeFile,
        ]
      : ["-e", probeScript, probeFile];

  const probe = await runAsyncExec(command, args, {
    cwd: options.checkoutRealpath,
    env: options.env,
    timeoutMs: 10_000,
  });

  if (probe.status === 0) {
    return {
      blocked: false,
      errorMessage: `Control probe unexpectedly accessed ${probeFile}`,
    };
  }
  if (
    options.mode === "node-permission" &&
    !probe.stderr.includes("ERR_ACCESS_DENIED")
  ) {
    return {
      blocked: false,
      errorMessage: `Control probe failed without ERR_ACCESS_DENIED: ${probe.stderr}`,
    };
  }
  return {
    blocked: true,
    errorMessage: probe.stderr.trim(),
  };
}

/**
 * Runs a command inside `checkoutDir`, optionally isolating filesystem reads so
 * the inspected consumer process cannot access the development repository.
 */
export async function runInIsolatedHost(
  options: RunInIsolatedHostOptions,
): Promise<IsolatedRunResult> {
  const checkoutRealpath = realpathSync(options.checkoutDir);
  const binDir = join(checkoutRealpath, "node_modules", ".bin");
  const baseEnv = options.env ?? process.env;
  const envWithPath: Record<string, string | undefined> = {
    ...baseEnv,
    PATH: `${binDir}${delimiter}${baseEnv.PATH ?? ""}`,
  };
  const timeoutMs = options.timeoutMs ?? defaultTimeoutMs;

  if (options.isolated === false) {
    return runAsyncExec(options.command, options.args, {
      cwd: checkoutRealpath,
      env: envWithPath,
      timeoutMs,
    });
  }

  const blockedPaths = [
    ...new Set(
      (options.blockedPaths ?? resolveDefaultBlockedPaths())
        .filter((path) => existsSync(path))
        .map((path) => realpathSync(path)),
    ),
  ];
  const sharedReadWritePaths = [
    ...new Set(
      (options.sharedReadWritePaths ?? [])
        .filter((path) => existsSync(path))
        .map((path) => realpathSync(path)),
    ),
  ];
  const isolatedTmpDir = realpathSync(
    mkdtempSync(join(tmpdir(), "tssae-isolated-tmp-")),
  );

  try {
    const useBwrap = await canUseBwrap(blockedPaths, checkoutRealpath);
    const mode: IsolationMode = useBwrap ? "bwrap" : "node-permission";
    const bwrapPrefixArgs = useBwrap
      ? [
          "--ro-bind",
          "/",
          "/",
          "--dev",
          "/dev",
          "--proc",
          "/proc",
          "--bind",
          checkoutRealpath,
          checkoutRealpath,
          "--bind",
          isolatedTmpDir,
          isolatedTmpDir,
          ...sharedReadWritePaths.flatMap((path) => ["--bind", path, path]),
          ...blockedPaths.flatMap((path) => ["--tmpfs", path]),
        ]
      : [];

    const pnpmStorePath = resolvePnpmStorePath(
      checkoutRealpath,
      baseEnv,
      options.pnpmStoreDir,
    );
    const isolatedEnv: Record<string, string | undefined> = useBwrap
      ? {
          ...envWithPath,
          TMPDIR: isolatedTmpDir,
        }
      : {
          ...envWithPath,
          NODE_NO_WARNINGS: "1",
          NODE_OPTIONS: buildNodePermissionOptions({
            checkoutRealpath,
            isolatedTmpDir,
            sharedReadWritePaths,
            allowNet: options.allowNet === true,
            ...(pnpmStorePath === undefined ? {} : { pnpmStorePath }),
          }),
          TMPDIR: isolatedTmpDir,
        };

    const probe = await runControlProbe({
      mode,
      blockedPaths,
      checkoutRealpath,
      env: isolatedEnv,
      bwrapPrefixArgs,
    });
    if (!probe.blocked) {
      throw new Error(
        `Dev-repo read isolation control probe failed (${mode}): ${probe.errorMessage}`,
      );
    }

    const command = useBwrap ? "bwrap" : options.command;
    const resolvedBinCommand = existsSync(join(binDir, options.command))
      ? resolve(binDir, options.command)
      : options.command;
    const args = useBwrap
      ? [...bwrapPrefixArgs, "--", resolvedBinCommand, ...options.args]
      : options.args;

    const result = await runAsyncExec(command, args, {
      cwd: checkoutRealpath,
      env: isolatedEnv,
      timeoutMs,
    });
    return {
      ...result,
      isolation: {
        mode,
        blockedPaths,
        controlProbeBlocked: probe.blocked,
        controlProbeError: probe.errorMessage,
      },
    };
  } finally {
    rmSync(isolatedTmpDir, { recursive: true, force: true });
  }
}
