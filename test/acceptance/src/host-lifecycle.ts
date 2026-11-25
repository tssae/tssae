import { existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  type CleanupFn,
  copyDirectory,
  createHostFileAccessors,
  resolveHostDir,
  runCleanupsLifo,
  writeHostFiles,
} from "@tssae/test-hosts";

import {
  type PackedArtifact,
  type PackedArtifactBundle,
  packWorkspaceArtifacts,
} from "./artifacts.ts";
import {
  DEFAULT_DIRECT_TSSAE_PACKAGES,
  installArtifactPackages,
} from "./host-install.ts";
import { type IsolatedRunResult, runInIsolatedHost } from "./isolation.ts";
import type { PackageManager } from "./types.ts";
import {
  type HostResolutionAudit,
  verifyHostArtifactResolution,
} from "./verify.ts";

export type PrepareAcceptanceHostOptions = {
  readonly hostFixture?: string;
  readonly files?: Readonly<Record<string, string | Uint8Array>>;
  readonly additionalPackages?: readonly string[];
  readonly directPackages?: readonly string[];
  readonly bundle?: PackedArtifactBundle;
  readonly packageManager?: PackageManager;
  readonly blockedPaths?: readonly string[];
  readonly env?: Readonly<Record<string, string | undefined>>;
  readonly tempRoot?: string;
  readonly storeDir?: string;
  readonly cwd?: string;
};

export type HostRunOptions = {
  readonly isolated?: boolean;
  readonly timeoutMs?: number;
  readonly sharedReadWritePaths?: readonly string[];
  readonly env?: Readonly<Record<string, string | undefined>>;
};

export type PreparedAcceptanceHost = {
  readonly environmentDir: string;
  readonly hostDir: string;
  readonly storeDir: string;
  readonly runtimeDir: string;
  readonly artifacts: readonly PackedArtifact[];
  readonly audit: HostResolutionAudit;
  resolvePath(relativePath: string): string;
  readText(relativePath: string): string;
  writeText(relativePath: string, content: string): void;
  replaceText(
    relativePath: string,
    search: string,
    replacement: string,
  ): string;
  artifact(packageName: string): PackedArtifact;
  runIsolated(
    command: string,
    args: readonly string[],
    options?: HostRunOptions,
  ): Promise<IsolatedRunResult>;
  runTsc(
    tsconfigRelativePath: string,
    options?: HostRunOptions,
  ): Promise<IsolatedRunResult>;
  runNode(
    args: readonly string[],
    options?: HostRunOptions,
  ): Promise<IsolatedRunResult>;
  dispose(): Promise<void>;
  [Symbol.asyncDispose](): Promise<void>;
};

/**
 * Prepares an isolated out-of-repo acceptance host with packed `tssae`
 * artifacts installed and verified, guaranteeing LIFO resource reclamation on
 * any mid-preparation failure or subsequent `dispose()`.
 */
export async function prepareAcceptanceHost(
  options: PrepareAcceptanceHostOptions = {},
): Promise<PreparedAcceptanceHost> {
  const cleanups: CleanupFn[] = [];
  const tempParent = options.tempRoot ?? tmpdir();
  mkdirSync(tempParent, { recursive: true });
  const environmentDir = mkdtempSync(join(tempParent, "tssae-host-"));
  cleanups.push(() => {
    rmSync(environmentDir, { recursive: true, force: true });
  });

  const hostDir = join(environmentDir, "host");
  const storeDir = options.storeDir ?? join(environmentDir, "store");
  const runtimeDir = join(environmentDir, "runtime");

  try {
    mkdirSync(hostDir, { recursive: true });
    mkdirSync(storeDir, { recursive: true });
    mkdirSync(runtimeDir, { recursive: true });

    if (options.hostFixture !== undefined) {
      copyDirectory(resolveHostDir(options.hostFixture), hostDir);
    }
    if (options.files !== undefined) {
      writeHostFiles(hostDir, options.files);
    }
    if (!existsSync(join(hostDir, "package.json"))) {
      writeHostFiles(hostDir, {
        "package.json": `${JSON.stringify(
          {
            name: "tssae-acceptance-host",
            private: true,
            type: "module",
          },
          null,
          2,
        )}\n`,
      });
    }

    const bundle =
      options.bundle ??
      (await packWorkspaceArtifacts({
        ...(options.additionalPackages === undefined
          ? {}
          : { additionalPackages: options.additionalPackages }),
        ...(options.cwd === undefined ? {} : { cwd: options.cwd }),
      }));
    if (options.bundle === undefined) {
      cleanups.push(() => {
        bundle.dispose();
      });
    }

    const directPackages = [
      ...new Set(
        options.directPackages ?? [
          ...DEFAULT_DIRECT_TSSAE_PACKAGES,
          ...(options.additionalPackages ?? []),
        ],
      ),
    ];

    const installation = await installArtifactPackages({
      checkoutDir: hostDir,
      packageManager: options.packageManager ?? "pnpm",
      artifacts: bundle.artifacts,
      directPackages,
      storeDir,
      ...(options.env === undefined ? {} : { env: options.env }),
    });
    if (installation.status === "install-failed") {
      throw new Error(
        `${installation.message} (${installation.command}, exit=${String(installation.exitCode)}): ${installation.stderr}`,
      );
    }

    const audit = verifyHostArtifactResolution({
      checkoutDir: hostDir,
      artifacts: bundle.artifacts,
      directPackages,
      ...(options.blockedPaths === undefined
        ? {}
        : { blockedPaths: options.blockedPaths }),
      allowedStorePaths: [storeDir],
    });

    const artifacts = bundle.artifacts;
    const artifactByName = new Map(
      artifacts.map((artifact) => [artifact.name, artifact]),
    );
    const getArtifact = (packageName: string): PackedArtifact => {
      const found = artifactByName.get(packageName);
      if (found === undefined) {
        throw new Error(`Missing packed artifact for ${packageName}`);
      }
      return found;
    };

    let disposePromise: Promise<void> | undefined;
    const dispose = (): Promise<void> => {
      if (disposePromise !== undefined) {
        return disposePromise;
      }
      disposePromise = (async () => {
        const cleanupError = await runCleanupsLifo(cleanups);
        if (cleanupError !== undefined) {
          throw cleanupError;
        }
      })();
      return disposePromise;
    };

    const hostFiles = createHostFileAccessors(hostDir);
    const runIsolated = (
      command: string,
      args: readonly string[],
      runOptions: HostRunOptions = {},
    ): Promise<IsolatedRunResult> => {
      const mergedEnv = runOptions.env ?? options.env;
      return runInIsolatedHost({
        checkoutDir: hostDir,
        command,
        args,
        isolated: runOptions.isolated ?? true,
        pnpmStoreDir: storeDir,
        ...(options.blockedPaths === undefined
          ? {}
          : { blockedPaths: options.blockedPaths }),
        ...(runOptions.sharedReadWritePaths === undefined
          ? {}
          : { sharedReadWritePaths: runOptions.sharedReadWritePaths }),
        ...(runOptions.timeoutMs === undefined
          ? {}
          : { timeoutMs: runOptions.timeoutMs }),
        ...(mergedEnv === undefined ? {} : { env: mergedEnv }),
      });
    };

    return {
      environmentDir,
      hostDir,
      storeDir,
      runtimeDir,
      artifacts,
      audit,
      resolvePath: hostFiles.resolvePath,
      readText: hostFiles.readText,
      writeText: hostFiles.writeText,
      replaceText: hostFiles.replaceText,
      artifact: getArtifact,
      runIsolated,
      runTsc: (tsconfigRelativePath, runOptions) =>
        runIsolated(
          "tsc",
          ["-p", tsconfigRelativePath, "--noEmit", "--pretty", "false"],
          runOptions,
        ),
      runNode: (args, runOptions) =>
        runIsolated(process.execPath, args, runOptions),
      dispose,
      [Symbol.asyncDispose]: dispose,
    };
  } catch (error) {
    // Preserve the original failure while reclaiming all resources created so far.
    await runCleanupsLifo(cleanups);
    throw error;
  }
}

/**
 * Prepares an acceptance host, passes it to `callback`, and always disposes all
 * temporary directories in a `finally` block.
 */
export async function withAcceptanceHost<T>(
  options: PrepareAcceptanceHostOptions,
  callback: (host: PreparedAcceptanceHost) => Promise<T>,
): Promise<T> {
  const host = await prepareAcceptanceHost(options);
  try {
    return await callback(host);
  } finally {
    await host.dispose();
  }
}
