import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@verule/test-hosts";
import { TssaeError, openProject } from "tssae";

void describe("TSSAE public native adapter loading contract", () => {
  void it("resolves root and subpath specifiers uniformly and rejects invalid descriptors or bridges", async () => {
    await withTempHost(
      {
        host: "adapters/fixture-basic",
        syntheticPackages: {
          "@acme/root-adapter": {
            packageJson: {
              name: "@acme/root-adapter",
              type: "module",
              exports: {
                ".": "./dist/index.js",
                "./tssae-native": "./dist/tssae-native.json",
                "./native/linux-x64": "./dist/native/root.so",
              },
            },
            files: {
              "dist/index.js": `export default { id: "acme-root", provides: [{ factType: { id: "acme.fact", subjectKind: "call" }, extract() { return []; } }] };`,
              "dist/tssae-native.json": JSON.stringify({
                id: "acme-root",
                "linux-x64": "./native/linux-x64",
              }),
              "dist/native/root.so": "not-a-real-elf",
            },
          },
          "@acme/bundle": {
            packageJson: {
              name: "@acme/bundle",
              type: "module",
              exports: {
                "./zod": "./dist/zod/index.js",
                "./zod/tssae-native": "./dist/zod/tssae-native.json",
                "./zod/native/linux-x64": "./dist/native/zod.so",
                "./drizzle": "./dist/drizzle/index.js",
                "./drizzle/tssae-native": "./dist/drizzle/tssae-native.json",
                "./drizzle/native/linux-x64": "./dist/native/drizzle.so",
              },
            },
            files: {
              "dist/zod/index.js": `export default { id: "bundle-zod", provides: [{ factType: { id: "acme.zod", subjectKind: "call" }, extract() { return []; } }] };`,
              "dist/zod/tssae-native.json": JSON.stringify({
                id: "bundle-zod",
                "linux-x64": "./native/linux-x64",
              }),
              "dist/native/zod.so": "not-a-real-elf-zod",
              "dist/drizzle/index.js": `export default { id: "bundle-drizzle", provides: [{ factType: { id: "acme.drizzle", subjectKind: "call" }, extract() { return []; } }] };`,
              "dist/drizzle/tssae-native.json": JSON.stringify({
                id: "bundle-drizzle",
                "linux-x64": "./native/linux-x64",
              }),
              "dist/native/drizzle.so": "not-a-real-elf-drizzle",
            },
          },
          "@acme/id-mismatch": {
            packageJson: {
              name: "@acme/id-mismatch",
              type: "module",
              exports: {
                ".": "./dist/index.js",
                "./tssae-native": "./dist/tssae-native.json",
                "./native/linux-x64": "./dist/native/mismatch.so",
              },
            },
            files: {
              "dist/index.js": `export default { id: "js-id", provides: [{ factType: { id: "acme.fact", subjectKind: "call" }, extract() { return []; } }] };`,
              "dist/tssae-native.json": JSON.stringify({
                id: "native-id",
                "linux-x64": "./native/linux-x64",
              }),
              "dist/native/mismatch.so": "not-a-real-elf",
            },
          },
          "@acme/missing-descriptor": {
            packageJson: {
              name: "@acme/missing-descriptor",
              type: "module",
              exports: {
                ".": "./dist/index.js",
              },
            },
            files: {
              "dist/index.js": `export default { id: "missing-desc", provides: [] };`,
            },
          },
        },
      },
      async (host) => {
        // Root specifier resolves through ".", "./tssae-native", "./native/linux-x64"
        // and reaches the engine dlopen stage with the configured package specifier.
        await assert.rejects(
          openProject({
            cwd: host.rootDir,
            adapterResolutionRoot: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters: ["@acme/root-adapter"],
          }),
          (error: unknown) =>
            error instanceof TssaeError &&
            error.code === "NATIVE_ADAPTER_LOAD" &&
            error.message.includes("@acme/root-adapter") &&
            error.message.includes("root.so"),
        );

        // Subpath specifier resolves through "./zod", "./zod/tssae-native", "./zod/native/linux-x64".
        await assert.rejects(
          openProject({
            cwd: host.rootDir,
            adapterResolutionRoot: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters: ["@acme/bundle/zod"],
          }),
          (error: unknown) =>
            error instanceof TssaeError &&
            error.code === "NATIVE_ADAPTER_LOAD" &&
            error.message.includes("@acme/bundle/zod") &&
            error.message.includes("zod.so"),
        );

        // Missing package reports INVALID_CONFIG naming the specifier.
        await assert.rejects(
          openProject({
            cwd: host.rootDir,
            adapterResolutionRoot: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters: ["@acme/does-not-exist"],
          }),
          (error: unknown) =>
            error instanceof TssaeError &&
            error.code === "INVALID_CONFIG" &&
            error.message.includes("@acme/does-not-exist"),
        );

        // Missing tssae-native descriptor export reports INVALID_CONFIG.
        await assert.rejects(
          openProject({
            cwd: host.rootDir,
            adapterResolutionRoot: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters: ["@acme/missing-descriptor"],
          }),
          (error: unknown) =>
            error instanceof TssaeError &&
            error.code === "INVALID_CONFIG" &&
            error.message.includes("@acme/missing-descriptor") &&
            error.message.includes("tssae-native"),
        );

        // JS bridge id != native descriptor id reports INVALID_CONFIG with both IDs.
        await assert.rejects(
          openProject({
            cwd: host.rootDir,
            adapterResolutionRoot: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters: ["@acme/id-mismatch"],
          }),
          (error: unknown) =>
            error instanceof TssaeError &&
            error.code === "INVALID_CONFIG" &&
            error.message.includes("@acme/id-mismatch") &&
            error.message.includes("native-id") &&
            error.message.includes("js-id"),
        );
      },
    );
  });
});
