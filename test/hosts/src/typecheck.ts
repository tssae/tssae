import { execFile } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, relative } from "node:path";

import { resolvePathInsideRoot } from "./materials.ts";

const require = createRequire(import.meta.url);
const tscScriptPath = join(
  dirname(require.resolve("typescript-7/package.json")),
  "bin",
  "tsc",
);

const SKIPPED_HOST_DIR_NAMES = new Set([".git", "node_modules", "untyped"]);
const SKIP_HOST_TYPECHECK_MARKER = ".skip-host-typecheck";

export type HostTypecheckResult = {
  readonly tsconfigPath: string;
  readonly stdout: string;
  readonly stderr: string;
};

export type TypecheckHostProjectsOptions = {
  readonly allowEmpty?: boolean;
};

/**
 * Recursively discovers `tsconfig.json` files under `hostsDir`, skipping
 * `.git`, `node_modules`, `untyped`, and any directory containing a
 * `.skip-host-typecheck` marker file.
 */
export function discoverHostTsconfigs(hostsDir: string): string[] {
  if (!existsSync(hostsDir)) {
    return [];
  }

  const discovered: string[] = [];
  const walk = (currentDir: string): void => {
    const entries = readdirSync(currentDir, { withFileTypes: true }).sort(
      (left, right) => left.name.localeCompare(right.name),
    );
    if (
      entries.some(
        (entry) => entry.isFile() && entry.name === SKIP_HOST_TYPECHECK_MARKER,
      )
    ) {
      return;
    }

    for (const entry of entries) {
      if (entry.isFile() && entry.name === "tsconfig.json") {
        discovered.push(join(currentDir, entry.name));
      } else if (
        entry.isDirectory() &&
        !SKIPPED_HOST_DIR_NAMES.has(entry.name)
      ) {
        walk(join(currentDir, entry.name));
      }
    }
  };

  walk(hostsDir);
  return discovered;
}

/**
 * Runs `tsc -p <tsconfigRelativePath> --noEmit --pretty false` inside `rootDir`,
 * rejecting with full compiler output when typechecking fails.
 */
export function runTscTypecheck(
  rootDir: string,
  tsconfigRelativePath = "tsconfig.json",
): Promise<HostTypecheckResult> {
  const tsconfigPath = resolvePathInsideRoot(rootDir, tsconfigRelativePath);
  if (!existsSync(tsconfigPath)) {
    return Promise.reject(
      new Error(`Missing tsconfig file for host typecheck: ${tsconfigPath}`),
    );
  }

  return new Promise((resolveResult, rejectResult) => {
    execFile(
      process.execPath,
      [tscScriptPath, "-p", tsconfigPath, "--noEmit", "--pretty", "false"],
      {
        cwd: rootDir,
        encoding: "utf8",
        maxBuffer: 16 * 1024 * 1024,
      },
      (error, stdout, stderr) => {
        if (error === null) {
          resolveResult({ tsconfigPath, stdout, stderr });
          return;
        }
        const exitCode = typeof error.code === "number" ? error.code : 1;
        const output = [stdout.trim(), stderr.trim()]
          .filter((part) => part.length > 0)
          .join("\n");
        rejectResult(
          new Error(
            `Typecheck failed for ${tsconfigRelativePath} (exit ${String(exitCode)})${output.length > 0 ? `:\n${output}` : ""}`,
          ),
        );
      },
    );
  });
}

/**
 * Discovers and typechecks all host `tsconfig.json` projects under `hostsDir`.
 * Rejects if zero projects are found (unless `allowEmpty` is true) or if any
 * host project fails `tsc`.
 */
export async function typecheckHostProjects(
  hostsDir: string,
  options: TypecheckHostProjectsOptions = {},
): Promise<readonly HostTypecheckResult[]> {
  const tsconfigPaths = discoverHostTsconfigs(hostsDir);
  if (tsconfigPaths.length === 0) {
    if (options.allowEmpty === true) {
      return [];
    }
    throw new Error(`No host tsconfig.json files found under ${hostsDir}`);
  }

  const results: HostTypecheckResult[] = [];
  const failures: string[] = [];

  for (const tsconfigPath of tsconfigPaths) {
    try {
      results.push(
        await runTscTypecheck(dirname(tsconfigPath), "tsconfig.json"),
      );
    } catch (error) {
      const label = relative(hostsDir, tsconfigPath) || tsconfigPath;
      const message = error instanceof Error ? error.message : String(error);
      failures.push(`[${label}] ${message}`);
    }
  }

  if (failures.length > 0) {
    throw new Error(
      `Host typecheck failed for ${String(failures.length)} project(s):\n${failures.join("\n\n")}`,
    );
  }

  return results;
}
