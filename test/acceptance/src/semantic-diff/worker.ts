import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { pathToFileURL } from "node:url";

import type { ProjectSession } from "tssae";

import { collectQueryDigest } from "./digest.ts";
import type { CollectQueryDigestOptions } from "./types.ts";

export type SemanticDiffWorkerConfig = {
  readonly tssaeEntryPath: string;
  readonly projectDir: string;
  readonly tsconfig?: string;
  readonly digestOutPath: string;
  readonly collectOptions?: CollectQueryDigestOptions;
};

type TssaeModule = {
  readonly openProject: (options: {
    readonly cwd: string;
    readonly tsconfig?: string;
  }) => Promise<ProjectSession>;
};

async function runWorker(): Promise<void> {
  const configPath = process.argv[2];
  if (configPath === undefined) {
    throw new Error(
      "Semantic diff worker requires a JSON config path argument.",
    );
  }
  const config = JSON.parse(
    readFileSync(resolve(configPath), "utf8"),
  ) as SemanticDiffWorkerConfig;

  const tssae = (await import(
    pathToFileURL(resolve(config.tssaeEntryPath)).href
  )) as TssaeModule;

  const startedAt = performance.now();
  const session = await tssae.openProject({
    cwd: config.projectDir,
    ...(config.tsconfig === undefined ? {} : { tsconfig: config.tsconfig }),
  });

  let fileCount = 0;
  let sampleCount = 0;
  try {
    const digest = await collectQueryDigest(session, config.collectOptions);
    fileCount = digest.fileCount;
    sampleCount = digest.sampleCount;
    writeFileSync(
      resolve(config.digestOutPath),
      `${JSON.stringify(digest)}\n`,
      "utf8",
    );
  } finally {
    await session.close();
  }

  const durationMs = performance.now() - startedAt;
  process.stdout.write(
    `${JSON.stringify({ fileCount, sampleCount, durationMs })}\n`,
  );
}

void runWorker().catch((error: unknown) => {
  process.stderr.write(
    `${error instanceof Error ? (error.stack ?? error.message) : String(error)}\n`,
  );
  process.exitCode = 1;
});
