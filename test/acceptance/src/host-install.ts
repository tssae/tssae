import { execFile } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type { PackedArtifact } from "./artifacts.ts";
import {
  getSharedOutOfRepoNode24BinDir,
  prependToPath,
} from "./node-runtime.ts";
import { startArtifactRegistry } from "./registry.ts";
import type { PackageManager } from "./types.ts";

export const DEFAULT_DIRECT_TSSAE_PACKAGES: readonly string[] = ["tssae"];

export type CommandOutcome = {
  readonly status: number;
  readonly stdout: string;
  readonly stderr: string;
};

export type InstallArtifactPackagesOptions = {
  readonly checkoutDir: string;
  readonly packageManager: PackageManager;
  readonly artifacts: readonly PackedArtifact[];
  readonly directPackages?: readonly string[];
  readonly storeDir?: string;
  readonly nodeBinDir?: string;
  readonly engineStrict?: boolean;
  readonly env?: Readonly<Record<string, string | undefined>>;
};

export type InstallArtifactPackagesResult =
  | {
      readonly status: "ready";
      readonly directPackages: readonly string[];
    }
  | {
      readonly status: "install-failed";
      readonly command: string;
      readonly exitCode: number;
      readonly message: string;
      readonly stderr: string;
    };

const maxInstallBufferBytes = 64 * 1024 * 1024;

function runAsyncCommand(
  command: string,
  args: readonly string[],
  cwd: string,
  env: Readonly<Record<string, string | undefined>>,
): Promise<CommandOutcome> {
  return new Promise((resolveResult) => {
    execFile(
      command,
      args,
      {
        cwd,
        encoding: "utf8",
        maxBuffer: maxInstallBufferBytes,
        env: {
          ...env,
          GIT_TERMINAL_PROMPT: "0",
        },
      },
      (error, stdout, stderr) => {
        if (error === null) {
          resolveResult({ status: 0, stdout, stderr });
          return;
        }
        resolveResult({
          status: typeof error.code === "number" ? error.code : 1,
          stdout,
          stderr: stderr.length > 0 ? stderr : error.message,
        });
      },
    );
  });
}

/**
 * Installs candidate TSSAE packages (including unscoped `tssae`) from one local
 * artifact batch into a clean host checkout via the loopback registry.
 *
 * Defaults `engineStrict` to `true` and prepends the managed Node `24.0.0`
 * `bin` directory (`options.nodeBinDir` or the shared out-of-repo runtime) to
 * `PATH` so package-manager engine checks run against Node `24.0.0`.
 */
export async function installArtifactPackages(
  options: InstallArtifactPackagesOptions,
): Promise<InstallArtifactPackagesResult> {
  const directPackages = [
    ...new Set(options.directPackages ?? DEFAULT_DIRECT_TSSAE_PACKAGES),
  ];
  const artifactByName = new Map(
    options.artifacts.map((artifact) => [artifact.name, artifact]),
  );
  const specs = directPackages.map((name) => {
    const artifact = artifactByName.get(name);
    if (artifact === undefined) {
      throw new Error(`Missing packed artifact for direct package ${name}`);
    }
    return `${artifact.name}@${artifact.version}`;
  });

  const npmrcPath = join(options.checkoutDir, ".npmrc");
  const hadNpmrc = existsSync(npmrcPath);
  const previousNpmrc = hadNpmrc ? readFileSync(npmrcPath, "utf8") : "";
  const registry = await startArtifactRegistry(options.artifacts);
  let isolatedCacheHome: string | undefined;
  try {
    if (options.packageManager === "pnpm") {
      // The loopback port can be reused, making pnpm's URL-keyed metadata stale.
      isolatedCacheHome = mkdtempSync(join(tmpdir(), "tssae-pnpm-cache-"));
    }

    const npmrcLines: string[] = [];
    if (registry.unscopedPackages.length > 0) {
      // Unscoped packages such as `tssae` cannot use `<scope>:registry=`; route
      // the default registry through the loopback server (which proxies external deps).
      npmrcLines.push(`registry=${registry.url}/`);
    }
    for (const scope of registry.scopes) {
      npmrcLines.push(`${scope}:registry=${registry.url}/`);
    }
    const separator =
      previousNpmrc.length === 0 || previousNpmrc.endsWith("\n") ? "" : "\n";
    writeFileSync(
      npmrcPath,
      `${previousNpmrc}${separator}${npmrcLines.join("\n")}\n`,
    );

    const engineStrict = options.engineStrict ?? true;
    const nodeBinDir = options.nodeBinDir ?? getSharedOutOfRepoNode24BinDir();
    const commandArgs =
      options.packageManager === "npm"
        ? [
            "npm",
            "install",
            "--save-dev",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            ...(engineStrict ? ["--engine-strict"] : []),
            ...specs,
          ]
        : [
            "pnpm",
            ...(options.storeDir === undefined
              ? []
              : ["--store-dir", options.storeDir]),
            "add",
            "--save-dev",
            ...(existsSync(join(options.checkoutDir, "pnpm-workspace.yaml"))
              ? ["--workspace-root"]
              : []),
            "--ignore-scripts",
            ...(engineStrict ? ["--config.engine-strict=true"] : []),
            ...specs,
          ];
    const [command, ...args] = commandArgs;
    const baseEnv = options.env ?? process.env;
    const envWithNode24: Record<string, string | undefined> = {
      ...baseEnv,
      PATH: prependToPath([nodeBinDir], baseEnv.PATH),
      ...(isolatedCacheHome === undefined
        ? {}
        : { XDG_CACHE_HOME: isolatedCacheHome }),
    };
    const commandResult = await runAsyncCommand(
      command!,
      args,
      options.checkoutDir,
      envWithNode24,
    );
    if (commandResult.status !== 0) {
      return {
        status: "install-failed",
        command: commandArgs.join(" "),
        exitCode: commandResult.status,
        message: "Failed to install packed TSSAE artifacts.",
        stderr: commandResult.stderr.trim(),
      };
    }
    return { status: "ready", directPackages };
  } finally {
    if (hadNpmrc) {
      writeFileSync(npmrcPath, previousNpmrc);
    } else {
      rmSync(npmrcPath, { force: true });
    }
    try {
      await registry.close();
    } finally {
      if (isolatedCacheHome !== undefined) {
        rmSync(isolatedCacheHome, { recursive: true, force: true });
      }
    }
  }
}
