import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  type CleanupFn,
  runCleanupsLifo,
  runCleanupsLifoSync,
} from "./cleanup.ts";
import {
  copyDirectory,
  createHostFileAccessors,
  type HostFileAccessors,
  type LineEnding,
  linkNodeModulesPackage,
  resolveHostDir,
  type SyntheticPackageSpec,
  writeHostFiles,
  writeSyntheticPackage,
} from "./materials.ts";
import { type HostTypecheckResult, runTscTypecheck } from "./typecheck.ts";

export type PrepareTempHostOptions = {
  readonly host?: string;
  readonly hosts?: readonly string[];
  readonly hostDirs?: readonly string[];
  readonly files?: Readonly<Record<string, string | Uint8Array>>;
  readonly lineEnding?: LineEnding;
  readonly packageLinks?: Readonly<Record<string, string>>;
  readonly syntheticPackages?: Readonly<Record<string, SyntheticPackageSpec>>;
  readonly tempRoot?: string;
  readonly prefix?: string;
  readonly setup?: (host: PreparedTempHost) => Promise<void> | void;
};

export type PreparedTempHost = HostFileAccessors & {
  readonly rootDir: string;
  addCleanup(cleanup: CleanupFn): void;
  typecheck(tsconfigRelativePath?: string): Promise<HostTypecheckResult>;
  dispose(): Promise<void>;
  [Symbol.dispose](): void;
  [Symbol.asyncDispose](): Promise<void>;
};

/**
 * Materializes one or more checked hosts, file overlays, and `node_modules`
 * links/synthetic packages into an isolated writable temporary directory with
 * LIFO cleanup and `Symbol.dispose` / `Symbol.asyncDispose` support.
 */
export async function prepareTempHost(
  options: PrepareTempHostOptions = {},
): Promise<PreparedTempHost> {
  const cleanups: CleanupFn[] = [];
  const tempParent = options.tempRoot ?? tmpdir();
  mkdirSync(tempParent, { recursive: true });
  const rootDir = mkdtempSync(
    join(tempParent, options.prefix ?? "verule-temp-host-"),
  );
  cleanups.push(() => {
    rmSync(rootDir, { recursive: true, force: true });
  });

  try {
    const selectedHostDirs = [
      ...(options.host === undefined ? [] : [resolveHostDir(options.host)]),
      ...(options.hosts ?? []).map(resolveHostDir),
      ...(options.hostDirs ?? []),
    ];
    for (const hostDir of selectedHostDirs) {
      copyDirectory(
        hostDir,
        rootDir,
        options.lineEnding === undefined
          ? {}
          : { lineEnding: options.lineEnding },
      );
    }

    if (options.files !== undefined) {
      writeHostFiles(rootDir, options.files, options.lineEnding);
    }

    for (const [packageName, targetDir] of Object.entries(
      options.packageLinks ?? {},
    )) {
      linkNodeModulesPackage(rootDir, packageName, targetDir);
    }

    for (const [packageName, spec] of Object.entries(
      options.syntheticPackages ?? {},
    )) {
      writeSyntheticPackage(rootDir, packageName, spec, options.lineEnding);
    }

    const accessors = createHostFileAccessors(rootDir);
    let disposed = false;
    let disposePromise: Promise<void> | undefined;

    const dispose = (): Promise<void> => {
      if (disposePromise !== undefined) {
        return disposePromise;
      }
      disposed = true;
      disposePromise = (async () => {
        const cleanupError = await runCleanupsLifo(cleanups);
        if (cleanupError !== undefined) {
          throw cleanupError;
        }
      })();
      return disposePromise;
    };

    const syncDispose = (): void => {
      if (disposed) {
        return;
      }
      disposed = true;
      runCleanupsLifoSync(cleanups);
    };

    const host: PreparedTempHost = {
      rootDir,
      ...accessors,
      addCleanup(cleanup: CleanupFn): void {
        cleanups.push(cleanup);
      },
      typecheck(tsconfigRelativePath?: string): Promise<HostTypecheckResult> {
        return runTscTypecheck(rootDir, tsconfigRelativePath);
      },
      dispose,
      [Symbol.dispose]: syncDispose,
      [Symbol.asyncDispose]: dispose,
    };

    if (options.setup !== undefined) {
      await options.setup(host);
    }

    return host;
  } catch (error) {
    // Preserve the original failure while reclaiming all resources in LIFO order.
    await runCleanupsLifo(cleanups);
    throw error;
  }
}

/**
 * Runs `callback` against a prepared temporary host and always disposes
 * registered resources in LIFO order while preserving any callback error.
 */
export async function withPreparedTempHost<T>(
  prepare: () => Promise<PreparedTempHost>,
  callback: (host: PreparedTempHost) => Promise<T> | T,
): Promise<T> {
  const host = await prepare();
  let callbackFailed = false;
  try {
    return await callback(host);
  } catch (error) {
    callbackFailed = true;
    try {
      await host.dispose();
    } catch {
      // Preserve the original callback error.
    }
    throw error;
  } finally {
    if (!callbackFailed) {
      await host.dispose();
    }
  }
}

/**
 * Prepares a temporary host, passes it to `callback`, and always disposes
 * registered resources in LIFO order while preserving any callback error.
 */
export async function withTempHost<T>(
  options: PrepareTempHostOptions,
  callback: (host: PreparedTempHost) => Promise<T> | T,
): Promise<T> {
  return withPreparedTempHost(() => prepareTempHost(options), callback);
}
