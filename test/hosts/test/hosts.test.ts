import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { describe, it } from "node:test";

import {
  copyDirectory,
  discoverHostTsconfigs,
  installPackedWorkspacePackages,
  prepareTempHost,
  resolveHostDir,
  typecheckHostProjects,
  withTempHost,
} from "@tssae/test-hosts";

void describe("@tssae/test-hosts", () => {
  void it("materializes a checked host into an isolated writable directory and cleans up on dispose", async () => {
    const sourceHandlers = readFileSync(
      `${resolveHostDir("sdk/check-basic")}/src/handlers.ts`,
      "utf8",
    );

    let capturedRoot = "";
    await withTempHost({ host: "sdk/check-basic" }, async (host) => {
      capturedRoot = host.rootDir;
      assert.ok(existsSync(host.resolvePath("tsconfig.json")));
      assert.equal(host.readText("src/handlers.ts"), sourceHandlers);

      host.replaceText("src/handlers.ts", '"中文😀"', '"mutated"');
      assert.match(host.readText("src/handlers.ts"), /"mutated"/u);

      host.writeText("src/extra.ts", "export const extra = 1;\n");
      assert.equal(host.readText("src/extra.ts"), "export const extra = 1;\n");
      host.removePath("src/extra.ts");
      assert.equal(existsSync(host.resolvePath("src/extra.ts")), false);
    });

    assert.equal(existsSync(capturedRoot), false);

    // Rerunning starts from the identical initial checked state.
    const host2 = await prepareTempHost({ host: "sdk/check-basic" });
    try {
      assert.equal(host2.readText("src/handlers.ts"), sourceHandlers);
    } finally {
      await host2.dispose();
    }
    assert.equal(existsSync(host2.rootDir), false);
  });

  void it("typechecks a materialized host and rejects with diagnostics on type errors", async () => {
    await withTempHost({ host: "sdk/check-basic" }, async (host) => {
      await host.typecheck();

      host.replaceText(
        "src/handlers.ts",
        "return String(input);",
        "return input;",
      );
      await assert.rejects(
        () => host.typecheck(),
        (error: unknown) => {
          assert.ok(error instanceof Error);
          assert.match(error.message, /TS2322/u);
          return true;
        },
      );
    });
  });

  void it("supports file overrides, CRLF line-ending normalization, package links, and synthetic node_modules packages", async () => {
    await withTempHost({}, async (linkedPkgHost) => {
      linkedPkgHost.writeText(
        "package.json",
        JSON.stringify({
          name: "@acme/linked",
          version: "1.0.0",
          type: "module",
          exports: { ".": "./index.js" },
        }),
      );
      linkedPkgHost.writeText(
        "index.d.ts",
        "export declare function linkedFn(): number;\n",
      );
      linkedPkgHost.writeText(
        "index.js",
        "export function linkedFn() { return 42; }\n",
      );

      await withTempHost(
        {
          host: "sdk/check-basic",
          lineEnding: "crlf",
          files: {
            "src/consumer.ts":
              'import { linkedFn } from "@acme/linked";\nimport { synthValue } from "@acme/synth";\nexport const total: number = linkedFn() + synthValue;\n',
          },
          packageLinks: {
            "@acme/linked": linkedPkgHost.rootDir,
          },
          syntheticPackages: {
            "@acme/synth": {
              packageJson: {
                name: "@acme/synth",
                version: "1.0.0",
                type: "module",
                exports: { ".": "./index.js" },
              },
              files: {
                "index.d.ts": "export declare const synthValue: number;\n",
                "index.js": "export const synthValue = 8;\n",
              },
            },
          },
        },
        async (host) => {
          const handlersText = host.readText("src/handlers.ts");
          assert.ok(handlersText.includes("\r\n"));
          assert.equal(
            handlersText.replaceAll("\r\n", "").includes("\n"),
            false,
          );

          const consumerText = host.readText("src/consumer.ts");
          assert.ok(consumerText.includes("\r\n"));

          await host.typecheck();
        },
      );
    });
  });

  void it("runs LIFO cleanups and preserves original errors on setup failure, callback failure, and using disposal", async () => {
    const setupCleanupOrder: string[] = [];
    let setupRootDir = "";
    await assert.rejects(
      () =>
        prepareTempHost({
          host: "sdk/check-basic",
          setup: (host) => {
            setupRootDir = host.rootDir;
            host.addCleanup(() => {
              setupCleanupOrder.push("first");
            });
            host.addCleanup(() => {
              setupCleanupOrder.push("second-throws");
              throw new Error("secondary cleanup error");
            });
            throw new Error("original setup failure");
          },
        }),
      (error: unknown) =>
        error instanceof Error && error.message === "original setup failure",
    );
    assert.deepEqual(setupCleanupOrder, ["second-throws", "first"]);
    assert.equal(existsSync(setupRootDir), false);

    const callbackCleanupOrder: string[] = [];
    let callbackRootDir = "";
    await assert.rejects(
      () =>
        withTempHost({ host: "sdk/check-basic" }, (host) => {
          callbackRootDir = host.rootDir;
          host.addCleanup(() => {
            callbackCleanupOrder.push("first");
          });
          host.addCleanup(() => {
            callbackCleanupOrder.push("second-throws");
            throw new Error("cleanup error during callback failure");
          });
          throw new Error("original callback failure");
        }),
      (error: unknown) =>
        error instanceof Error && error.message === "original callback failure",
    );
    assert.deepEqual(callbackCleanupOrder, ["second-throws", "first"]);
    assert.equal(existsSync(callbackRootDir), false);

    let syncRoot = "";
    {
      using syncHost = await prepareTempHost({ host: "sdk/check-basic" });
      syncRoot = syncHost.rootDir;
      assert.equal(existsSync(syncRoot), true);
    }
    assert.equal(existsSync(syncRoot), false);

    let asyncRoot = "";
    {
      await using asyncHost = await prepareTempHost({
        host: "sdk/check-basic",
      });
      asyncRoot = asyncHost.rootDir;
      assert.equal(existsSync(asyncRoot), true);
    }
    assert.equal(existsSync(asyncRoot), false);
  });

  void it("discovers host tsconfigs while skipping untyped, node_modules, and .skip-host-typecheck directories", async () => {
    await withTempHost({}, async (host) => {
      host.writeText(
        "valid/tsconfig.json",
        JSON.stringify({
          compilerOptions: { strict: true, target: "ES2022" },
          include: ["src/**/*.ts"],
        }),
      );
      host.writeText("valid/src/ok.ts", "export const ok: number = 1;\n");
      host.writeText("untyped/tsconfig.json", "{ invalid json");
      host.writeText("node_modules/pkg/tsconfig.json", "{ invalid json");
      host.writeText("skipped/.skip-host-typecheck", "");
      host.writeText(
        "skipped/tsconfig.json",
        JSON.stringify({ compilerOptions: { strict: true } }),
      );
      host.writeText("skipped/bad.ts", "export const bad: number = 'nope';\n");

      const discovered = discoverHostTsconfigs(host.rootDir);
      assert.deepEqual(discovered, [host.resolvePath("valid/tsconfig.json")]);

      const results = await typecheckHostProjects(host.rootDir);
      assert.equal(results.length, 1);

      host.writeText("valid/src/ok.ts", "export const ok: number = 'oops';\n");
      await assert.rejects(
        () => typecheckHostProjects(host.rootDir),
        (error: unknown) =>
          error instanceof Error && /TS2322/u.test(error.message),
      );
    });

    await withTempHost({}, async (emptyHost) => {
      await assert.rejects(
        () => typecheckHostProjects(emptyHost.rootDir),
        /No host tsconfig\.json files found/u,
      );
      assert.deepEqual(
        await typecheckHostProjects(emptyHost.rootDir, { allowEmpty: true }),
        [],
      );
    });
  });

  void it("copyDirectory skips .git, node_modules, dist, and *.tsbuildinfo artifacts", async () => {
    await withTempHost({}, async (sourceHost) => {
      sourceHost.writeText("src/keep.ts", "export const kept = true;\n");
      sourceHost.writeText(".git/HEAD", "ref: refs/heads/main\n");
      sourceHost.writeText("node_modules/pkg/index.js", "export {};\n");
      sourceHost.writeText("dist/out.js", "export {};\n");
      sourceHost.writeText(".tsbuildinfo", "{}");
      sourceHost.writeText("nested/app.tsbuildinfo", "{}");

      await withTempHost({}, (targetHost) => {
        copyDirectory(sourceHost.rootDir, targetHost.rootDir);
        assert.equal(existsSync(targetHost.resolvePath("src/keep.ts")), true);
        assert.equal(existsSync(targetHost.resolvePath(".git")), false);
        assert.equal(existsSync(targetHost.resolvePath("node_modules")), false);
        assert.equal(existsSync(targetHost.resolvePath("dist")), false);
        assert.equal(existsSync(targetHost.resolvePath(".tsbuildinfo")), false);
        assert.equal(
          existsSync(targetHost.resolvePath("nested/app.tsbuildinfo")),
          false,
        );
      });
    });
  });

  void it("installPackedWorkspacePackages packs and extracts workspace artifacts into node_modules", async () => {
    await withTempHost({}, async (consumerHost) => {
      await installPackedWorkspacePackages(consumerHost.rootDir, [
        "@tssae/test-hosts",
      ]);
      assert.equal(
        existsSync(
          consumerHost.resolvePath(
            "node_modules/@tssae/test-hosts/dist/index.d.ts",
          ),
        ),
        true,
      );
    });
  });
});
