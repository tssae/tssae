import { existsSync } from "node:fs";
import { join } from "node:path";

import { readAdapterApiCargoMetadata } from "./metadata.ts";

export type AdapterApiCrateInfo = {
  readonly crateName: string;
  readonly version: string;
  readonly cratePath: string;
};

/**
 * Returns crate metadata and the packaged `.crate` archive path produced by
 * `@tssae/adapter-api#build`.
 */
export function getAdapterApiCrateInfo(): AdapterApiCrateInfo {
  const { packageDir, crateName, version } = readAdapterApiCargoMetadata();
  const cratePath = join(packageDir, "dist", `${crateName}-${version}.crate`);
  if (!existsSync(cratePath)) {
    throw new Error(
      `Missing packaged crate at ${cratePath}; build @tssae/adapter-api first`,
    );
  }
  return { crateName, version, cratePath };
}

/**
 * Resolves the path to the packaged `tssae-adapter-api` `.crate` artifact.
 */
export function resolveAdapterApiCratePath(): string {
  return getAdapterApiCrateInfo().cratePath;
}
