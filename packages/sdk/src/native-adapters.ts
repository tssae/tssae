import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { validateAdapters, type LibraryAdapter } from "./facts.ts";
import { VeruleExecutionError } from "./types.ts";

export type NativeAdapterPackage = {
  readonly packageName: string;
  readonly adapterId: string;
  readonly libraryPath: string;
};

export type ResolvedNativeAdapterPackages = {
  readonly adapters: readonly LibraryAdapter[];
  readonly nativeAdapters: readonly NativeAdapterPackage[];
};

type NativeAdapterDescriptor = {
  readonly id?: unknown;
  readonly "linux-x64"?: unknown;
};

function invalidPackage(
  packageName: string,
  reason: string,
): VeruleExecutionError {
  return new VeruleExecutionError(
    "INVALID_CONFIG",
    `Failed to load configured adapter package "${packageName}": ${reason}`,
  );
}

function readNativeAdapterDescriptor(
  packageName: string,
  requireFromConfig: NodeJS.Require,
): NativeAdapterDescriptor {
  let descriptorPath: string;
  try {
    descriptorPath = requireFromConfig.resolve(`${packageName}/verule-native`);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw invalidPackage(
      packageName,
      `could not resolve its public "./verule-native" export: ${detail}`,
    );
  }

  let descriptor: NativeAdapterDescriptor;
  try {
    const parsed: unknown = JSON.parse(readFileSync(descriptorPath, "utf8"));
    if (
      typeof parsed !== "object" ||
      parsed === null ||
      Array.isArray(parsed)
    ) {
      throw new TypeError("native adapter descriptor must be an object");
    }
    descriptor = parsed as NativeAdapterDescriptor;
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw invalidPackage(
      packageName,
      `could not read its native adapter descriptor: ${detail}`,
    );
  }

  if (typeof descriptor.id !== "string" || descriptor.id.trim() === "") {
    throw invalidPackage(
      packageName,
      'its native adapter descriptor must define a non-empty "id"',
    );
  }
  if (
    typeof descriptor["linux-x64"] !== "string" ||
    !descriptor["linux-x64"].startsWith("./") ||
    descriptor["linux-x64"].includes("..")
  ) {
    throw invalidPackage(
      packageName,
      'its native adapter descriptor must define "linux-x64" as a package export path',
    );
  }
  return descriptor;
}

function validatePackageAdapter(
  value: unknown,
  packageName: string,
): LibraryAdapter {
  try {
    return validateAdapters([value as LibraryAdapter])[0]!;
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw invalidPackage(
      packageName,
      `invalid JavaScript adapter bridge: ${detail}`,
    );
  }
}

function selectLibraryAdapter(
  moduleExports: Record<string, unknown>,
  packageName: string,
): LibraryAdapter {
  const defaultAdapter = moduleExports.default ?? moduleExports.adapter;
  if (isLibraryAdapter(defaultAdapter)) {
    return validatePackageAdapter(defaultAdapter, packageName);
  }

  const candidates = Object.values(moduleExports).filter(isLibraryAdapter);
  if (candidates.length === 1) {
    return validatePackageAdapter(candidates[0], packageName);
  }
  throw invalidPackage(
    packageName,
    'its ESM entry must export one LibraryAdapter as "default" or "adapter"',
  );
}

function isLibraryAdapter(value: unknown): value is LibraryAdapter {
  return (
    typeof value === "object" &&
    value !== null &&
    "id" in value &&
    "provides" in value &&
    Array.isArray(value.provides)
  );
}

/**
 * Resolves the package's public bridge, native descriptor, and Linux x64 resource exports.
 */
export async function resolveNativeAdapterPackages(
  packageNames: readonly string[],
  resolutionRoot: string,
): Promise<ResolvedNativeAdapterPackages> {
  if (packageNames.length === 0) {
    return { adapters: [], nativeAdapters: [] };
  }
  if (process.platform !== "linux" || process.arch !== "x64") {
    throw invalidPackage(
      packageNames.join(", "),
      `prebuilt native adapters are supported on Linux x64; this host is ${process.platform}-${process.arch}`,
    );
  }

  const requireFromConfig = createRequire(
    resolve(resolutionRoot, "__verule_adapter_loader__.cjs"),
  );
  const adapters: LibraryAdapter[] = [];
  const nativeAdapters: NativeAdapterPackage[] = [];
  const packageOwnersByAdapterId = new Map<string, string>();

  for (const packageName of packageNames) {
    let entryPath: string;
    try {
      entryPath = requireFromConfig.resolve(`${packageName}/verule-adapter`);
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      throw invalidPackage(packageName, detail);
    }

    let imported: unknown;
    try {
      imported = await import(pathToFileURL(entryPath).href);
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      throw invalidPackage(
        packageName,
        `JavaScript ESM entry failed to load: ${detail}`,
      );
    }
    if (typeof imported !== "object" || imported === null) {
      throw invalidPackage(packageName, "JavaScript ESM entry has no exports");
    }

    const adapter = selectLibraryAdapter(
      imported as Record<string, unknown>,
      packageName,
    );
    const descriptor = readNativeAdapterDescriptor(
      packageName,
      requireFromConfig,
    );
    const adapterId = descriptor.id as string;
    if (adapter.id !== adapterId) {
      throw invalidPackage(
        packageName,
        `native descriptor id "${adapterId}" does not match JavaScript bridge adapter id "${adapter.id}"`,
      );
    }

    let libraryPath: string;
    try {
      libraryPath = requireFromConfig.resolve(
        `${packageName}/${(descriptor["linux-x64"] as string).slice(2)}`,
      );
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      throw invalidPackage(
        packageName,
        `could not resolve its declared Linux x64 resource through package exports: ${detail}`,
      );
    }

    const existingPackage = packageOwnersByAdapterId.get(adapter.id);
    if (existingPackage !== undefined) {
      throw invalidPackage(
        packageName,
        `adapter id "${adapter.id}" is already provided by package "${existingPackage}"`,
      );
    }
    packageOwnersByAdapterId.set(adapter.id, packageName);
    adapters.push(adapter);
    nativeAdapters.push({ packageName, adapterId, libraryPath });
  }

  return { adapters, nativeAdapters };
}
