import type { ConformanceCorpusId, ConformanceCorpusSpecMap } from "./types.ts";

export const DEFAULT_CONFORMANCE_CORPORA: readonly ConformanceCorpusId[] = [
  "test262",
  "typescript-conformance",
];

/**
 * Fixed-commit external conformance corpora used for static AST lowering,
 * `FunctionIr::validate_invariants()`, and crash/timeout-free query sweeps.
 */
export const CONFORMANCE_CORPORA: ConformanceCorpusSpecMap = {
  test262: {
    id: "test262",
    repoUrl: "https://github.com/tc39/test262.git",
    repoName: "test262",
    commitSha: "4db99ac4dc8924279ae07ed3246385924e878519",
    sparsePaths: ["test"],
    checkoutDirName: "test262-4db99ac4dc8924279ae07ed3246385924e878519",
    fileMatch: {
      scanPaths: ["test"],
      extensions: [".js"],
    },
    minFileCount: 45_000,
  },
  "typescript-conformance": {
    id: "typescript-conformance",
    repoUrl: "https://github.com/microsoft/TypeScript.git",
    repoName: "typescript",
    commitSha: "050880ce59e30b356b686bd3144efe24f875ebc8",
    sparsePaths: ["tests/cases/conformance"],
    checkoutDirName: "typescript-050880ce59e30b356b686bd3144efe24f875ebc8",
    fileMatch: {
      scanPaths: ["tests/cases/conformance"],
      extensions: [
        ".ts",
        ".tsx",
        ".mts",
        ".cts",
        ".js",
        ".jsx",
        ".mjs",
        ".cjs",
      ],
    },
    minFileCount: 5_500,
  },
};
