import { execFile } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const maxPackBufferBytes = 64 * 1024 * 1024;
const hostsRootDir = fileURLToPath(new URL("../hosts", import.meta.url));

export type LineEnding = "lf" | "crlf";

export type SyntheticPackageSpec = {
  readonly packageJson?: Readonly<Record<string, unknown>>;
  readonly files: Readonly<Record<string, string | Uint8Array>>;
};

export type CopyDirectoryOptions = {
  readonly lineEnding?: LineEnding;
};

/**
 * Normalizes line endings in a string to LF (`\n`) or CRLF (`\r\n`).
 */
export function normalizeLineEndings(
  text: string,
  lineEnding: LineEnding,
): string {
  const lfText = text.replaceAll("\r\n", "\n").replaceAll("\r", "\n");
  return lineEnding === "crlf" ? lfText.replaceAll("\n", "\r\n") : lfText;
}

/**
 * Validates and resolves a relative path inside `rootDir`, rejecting absolute
 * paths and parent traversal escapes.
 */
export function resolvePathInsideRoot(
  rootDir: string,
  relativePath: string,
): string {
  if (relativePath.length === 0 || isAbsolute(relativePath)) {
    throw new Error(
      `Expected a non-empty relative path inside ${rootDir}, got ${JSON.stringify(relativePath)}`,
    );
  }
  const resolvedRoot = resolve(rootDir);
  const resolvedTarget = resolve(resolvedRoot, relativePath);
  const rel = relative(resolvedRoot, resolvedTarget);
  if (rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(
      `Path ${JSON.stringify(relativePath)} escapes root directory ${resolvedRoot}`,
    );
  }
  return resolvedTarget;
}

/**
 * Resolves a checked host directory under `baseDir`, rejecting invalid names,
 * traversal escapes, or missing host directories.
 */
export function resolveCheckedHostDir(
  baseDir: string,
  hostName: string,
): string {
  const segments = hostName.split("/");
  if (
    hostName.length === 0 ||
    hostName.includes("\\") ||
    segments.some(
      (segment) => segment.length === 0 || segment === "." || segment === "..",
    )
  ) {
    throw new Error(`Invalid checked host name: ${JSON.stringify(hostName)}`);
  }
  const dirPath = resolvePathInsideRoot(baseDir, hostName);
  if (!existsSync(dirPath) || !statSync(dirPath).isDirectory()) {
    throw new Error(`Checked host "${hostName}" does not exist at ${dirPath}`);
  }
  return dirPath;
}

/**
 * Resolves the checked host directory under `@verule/test-hosts/hosts/`.
 */
export function resolveHostDir(hostName: string): string {
  return resolveCheckedHostDir(hostsRootDir, hostName);
}

function writeNormalizedCopiedFile(
  sourcePath: string,
  targetPath: string,
  lineEnding: LineEnding | undefined,
): void {
  mkdirSync(dirname(targetPath), { recursive: true });
  if (lineEnding === undefined) {
    copyFileSync(sourcePath, targetPath);
    return;
  }
  const bytes = readFileSync(sourcePath);
  if (bytes.includes(0)) {
    writeFileSync(targetPath, bytes);
    return;
  }
  const normalized = normalizeLineEndings(bytes.toString("utf8"), lineEnding);
  writeFileSync(targetPath, normalized, "utf8");
}

/**
 * Copies all files from `sourceDir` into `targetDir`, skipping `.git`,
 * `node_modules`, `dist`, and `*.tsbuildinfo` files, and optionally
 * normalizing text line endings.
 */
export function copyDirectory(
  sourceDir: string,
  targetDir: string,
  options: CopyDirectoryOptions = {},
): void {
  mkdirSync(targetDir, { recursive: true });
  for (const entry of readdirSync(sourceDir, { withFileTypes: true })) {
    if (
      entry.name === ".git" ||
      entry.name === "node_modules" ||
      entry.name === "dist" ||
      (entry.isFile() && entry.name.endsWith(".tsbuildinfo"))
    ) {
      continue;
    }
    const sourcePath = join(sourceDir, entry.name);
    const targetPath = join(targetDir, entry.name);
    if (entry.isDirectory()) {
      copyDirectory(sourcePath, targetPath, options);
    } else if (entry.isFile()) {
      writeNormalizedCopiedFile(sourcePath, targetPath, options.lineEnding);
    }
  }
}

/**
 * Writes an arbitrary map of relative files into `rootDir`, creating parent
 * directories and applying optional line-ending normalization for string files.
 */
export function writeHostFiles(
  rootDir: string,
  files: Readonly<Record<string, string | Uint8Array>>,
  lineEnding?: LineEnding,
): void {
  for (const [relativePath, content] of Object.entries(files)) {
    const targetPath = resolvePathInsideRoot(rootDir, relativePath);
    mkdirSync(dirname(targetPath), { recursive: true });
    if (typeof content === "string") {
      const output =
        lineEnding === undefined
          ? content
          : normalizeLineEndings(content, lineEnding);
      writeFileSync(targetPath, output, "utf8");
    } else {
      writeFileSync(targetPath, content);
    }
  }
}

function resolveNodeModulesPackagePath(
  rootDir: string,
  packageName: string,
): string {
  const parts = packageName.split("/");
  const validScoped =
    parts.length === 2 && parts[0]!.startsWith("@") && parts[0]!.length > 1;
  const validUnscoped = parts.length === 1 && !parts[0]!.startsWith("@");
  if (
    (!validScoped && !validUnscoped) ||
    packageName.includes("\\") ||
    parts.some((part) => part.length === 0 || part === "." || part === "..")
  ) {
    throw new Error(`Invalid package name: ${JSON.stringify(packageName)}`);
  }
  return resolvePathInsideRoot(rootDir, join("node_modules", ...parts));
}

/**
 * Links an existing directory into `<rootDir>/node_modules/<packageName>`.
 */
export function linkNodeModulesPackage(
  rootDir: string,
  packageName: string,
  targetDir: string,
): void {
  const resolvedTarget = resolve(targetDir);
  if (!existsSync(resolvedTarget) || !statSync(resolvedTarget).isDirectory()) {
    throw new Error(
      `Package link target for ${packageName} is not a directory: ${resolvedTarget}`,
    );
  }
  const linkPath = resolveNodeModulesPackagePath(rootDir, packageName);
  mkdirSync(dirname(linkPath), { recursive: true });
  rmSync(linkPath, { recursive: true, force: true });
  symlinkSync(
    resolvedTarget,
    linkPath,
    process.platform === "win32" ? "junction" : "dir",
  );
}

/**
 * Materializes a synthetic package under `<rootDir>/node_modules/<packageName>`.
 */
export function writeSyntheticPackage(
  rootDir: string,
  packageName: string,
  spec: SyntheticPackageSpec,
  lineEnding?: LineEnding,
): void {
  const packageDir = resolveNodeModulesPackagePath(rootDir, packageName);
  mkdirSync(packageDir, { recursive: true });
  if (spec.packageJson !== undefined) {
    const manifestText = `${JSON.stringify(spec.packageJson, null, 2)}\n`;
    writeFileSync(
      join(packageDir, "package.json"),
      lineEnding === undefined
        ? manifestText
        : normalizeLineEndings(manifestText, lineEnding),
      "utf8",
    );
  }
  writeHostFiles(packageDir, spec.files, lineEnding);
}

export type InstallPackedWorkspacePackagesOptions = {
  readonly cwd?: string;
};

function parsePackedFilename(stdout: string): string {
  const parsed: unknown = JSON.parse(stdout);
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    !("filename" in parsed) ||
    typeof parsed.filename !== "string" ||
    parsed.filename.length === 0
  ) {
    throw new Error(`Unexpected pnpm pack --json output: ${stdout}`);
  }
  return parsed.filename;
}

/**
 * Packs each workspace package in `packageNames` via `pnpm pack` and extracts
 * its published tarball contents into `<rootDir>/node_modules/<pkg>`.
 */
export async function installPackedWorkspacePackages(
  rootDir: string,
  packageNames: readonly string[],
  options: InstallPackedWorkspacePackagesOptions = {},
): Promise<void> {
  if (packageNames.length === 0) {
    return;
  }
  const cwd = options.cwd ?? process.cwd();
  const packDir = mkdtempSync(join(tmpdir(), "verule-packed-packages-"));
  try {
    for (const packageName of packageNames) {
      const targetDir = resolveNodeModulesPackagePath(rootDir, packageName);
      const { stdout } = await execFileAsync(
        "pnpm",
        [
          "--filter",
          packageName,
          "pack",
          "--pack-destination",
          packDir,
          "--json",
        ],
        { cwd, encoding: "utf8", maxBuffer: maxPackBufferBytes },
      );
      const rawFilename = parsePackedFilename(stdout);
      const tarballPath = isAbsolute(rawFilename)
        ? rawFilename
        : resolve(cwd, rawFilename);
      rmSync(targetDir, { recursive: true, force: true });
      mkdirSync(targetDir, { recursive: true });
      await execFileAsync(
        "tar",
        ["-xzf", tarballPath, "--strip-components=1", "-C", targetDir],
        { cwd, encoding: "utf8", maxBuffer: maxPackBufferBytes },
      );
    }
  } finally {
    rmSync(packDir, { recursive: true, force: true });
  }
}

export type HostFileAccessors = {
  readonly resolvePath: (relativePath: string) => string;
  readonly readText: (relativePath: string) => string;
  readonly writeText: (relativePath: string, content: string) => void;
  readonly replaceText: (
    relativePath: string,
    search: string,
    replacement: string,
  ) => string;
  readonly removePath: (relativePath: string) => void;
};

/**
 * Creates file read/write/edit/remove helpers anchored to `rootDir`.
 */
export function createHostFileAccessors(rootDir: string): HostFileAccessors {
  const resolvePath = (relativePath: string): string =>
    resolvePathInsideRoot(rootDir, relativePath);

  return {
    resolvePath,
    readText: (relativePath: string): string =>
      readFileSync(resolvePath(relativePath), "utf8"),
    writeText: (relativePath: string, content: string): void => {
      const target = resolvePath(relativePath);
      mkdirSync(dirname(target), { recursive: true });
      writeFileSync(target, content, "utf8");
    },
    replaceText: (
      relativePath: string,
      search: string,
      replacement: string,
    ): string => {
      const filePath = resolvePath(relativePath);
      const current = readFileSync(filePath, "utf8");
      if (!current.includes(search)) {
        throw new Error(
          `Expected ${relativePath} to contain ${JSON.stringify(search)}`,
        );
      }
      const next = current.replace(search, replacement);
      writeFileSync(filePath, next, "utf8");
      return next;
    },
    removePath: (relativePath: string): void => {
      rmSync(resolvePath(relativePath), { recursive: true, force: true });
    },
  };
}
