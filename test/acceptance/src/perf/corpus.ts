import { existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  materializeExternalCorpus,
  type ExternalCorpusSpec,
} from "../corpus.ts";
import type {
  CorpusProfileId,
  CorpusProfileSpec,
  MaterializedCorpus,
} from "./types.ts";

const VSCODE_COMMIT_SHA = "3fc62371e017d5e6d3d742b7aa79832c9fc9e71d";
const VSCODE_REPO_URL = "https://github.com/microsoft/vscode.git";

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

function writePerfTsconfig(
  stagingDir: string,
  includeGlobs: readonly string[],
): void {
  writeFileSync(
    join(stagingDir, "tsconfig.perf.json"),
    `${JSON.stringify(
      {
        extends: "./src/tsconfig.base.json",
        compilerOptions: {
          noEmit: true,
        },
        include: [...includeGlobs],
      },
      null,
      2,
    )}\n`,
  );
}

/**
 * Materializes a fixed-commit VS Code performance corpus via the shared corpus
 * foundation, attaching `tsconfig.perf.json` and perf scenario file metadata.
 */
export async function materializeCorpus(
  options: MaterializeCorpusOptions,
): Promise<MaterializedCorpus> {
  const spec = options.specOverride ?? CORPUS_PROFILES[options.profile];
  const externalSpec: ExternalCorpusSpec = {
    id: spec.id,
    repoUrl: spec.repoUrl,
    repoName: spec.repoName,
    commitSha: spec.commitSha,
    sparsePaths: spec.subsets,
    checkoutDirName: `${spec.repoName}-${spec.commitSha.slice(0, 12)}-${spec.id}`,
    minFileCount: 1,
  };

  const materialized = await materializeExternalCorpus({
    spec: externalSpec,
    ...(options.cacheDir === undefined ? {} : { cacheDir: options.cacheDir }),
    ...(options.refreshCorpus === undefined
      ? {}
      : { refreshCorpus: options.refreshCorpus }),
    ...(options.noCorpusCache === undefined
      ? {}
      : { noCorpusCache: options.noCorpusCache }),
    ...(options.hostMirrorPaths === undefined
      ? {}
      : { hostMirrorPaths: options.hostMirrorPaths }),
    isCheckoutValid: (checkoutDir) =>
      existsSync(join(checkoutDir, "tsconfig.perf.json")),
    onAfterExtract: ({ stagingDir }) => {
      writePerfTsconfig(stagingDir, spec.includeGlobs);
    },
  });

  return {
    profile: spec.id,
    repoUrl: materialized.repoUrl,
    repoName: materialized.repoName,
    commitSha: materialized.commitSha,
    subsets: materialized.sparsePaths,
    corpusDir: materialized.corpusDir,
    bareRepoDir: materialized.bareRepoDir,
    tsconfigPath: join(materialized.corpusDir, "tsconfig.perf.json"),
    fileCount: materialized.fileCount,
    primaryFile: spec.primaryFile,
    secondaryFile: spec.secondaryFile,
    targetCalleeName: spec.targetCalleeName,
    cacheHit: materialized.cacheHit,
    cacheBypassed: materialized.cacheBypassed,
    sourceOrigin: materialized.sourceOrigin,
  };
}
