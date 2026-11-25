import { relative, resolve } from "node:path";

import { VeruleExecutionError } from "./types.ts";

function normalizeProjectFile(cwd: string, filePath: string): string {
  return relative(cwd, resolve(cwd, filePath)).replaceAll("\\", "/");
}

/** Validates and canonicalizes the reverse-search file set for one project. */
export function normalizeLookupFiles(
  lookupFiles: readonly string[] | undefined,
  cwd: string,
  sourceFiles: readonly string[],
): readonly string[] | undefined {
  if (lookupFiles === undefined) return undefined;

  const projectFiles = new Set(
    sourceFiles.map((filePath) => normalizeProjectFile(cwd, filePath)),
  );
  const normalized = new Set<string>();
  for (const filePath of lookupFiles) {
    const candidate = normalizeProjectFile(cwd, filePath);
    if (!projectFiles.has(candidate)) {
      throw new VeruleExecutionError(
        "INVALID_QUERY",
        `lookupFiles path "${filePath}" is not a source file in project.`,
      );
    }
    normalized.add(candidate);
  }

  return [...normalized].toSorted();
}
