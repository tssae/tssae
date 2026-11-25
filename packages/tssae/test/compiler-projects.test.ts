import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  type PreparedTempHost,
  withTempHost,
  writeSyntheticPackage,
} from "@verule/test-hosts";
import { TssaeError, openProject, type SyntaxRef } from "tssae";

function materializeExtLib(host: PreparedTempHost): void {
  writeSyntheticPackage(host.rootDir, "ext-lib", {
    files: {
      "index.d.ts": host.readText("deps/ext-lib/index.d.ts"),
    },
  });
  host.removePath("deps");
}

void describe("tssae multi-project scope and origins", () => {
  void it("opens an empty-root aggregate tsconfig, explains file origins, and isolates @runtime types", async () => {
    await withTempHost(
      {
        host: "sdk-projects/compiler-projects",
        setup: materializeExtLib,
      },
      async (host) => {
        await host.typecheck("packages/web/tsconfig.json");
        await host.typecheck("packages/node/tsconfig.json");

        const observedTypesByProject = new Map<string, string>();
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });

        try {
          assert.equal(session.projects.length, 3);
          const webProj = session.projects.find(
            (p) => p.configFile === "packages/web/tsconfig.json",
          );
          const nodeProj = session.projects.find(
            (p) => p.configFile === "packages/node/tsconfig.json",
          );
          assert.ok(webProj);
          assert.ok(nodeProj);
          assert.equal(webProj.entryConfig, "tsconfig.json");
          assert.deepEqual(webProj.entryChain, [
            "tsconfig.json",
            "packages/web/tsconfig.json",
          ]);
          assert.equal(webProj.compilerOptions.strict, true);
          assert.deepEqual(webProj.compilerOptions.paths["@runtime"], [
            "./src/web.ts",
          ]);

          assert.equal(nodeProj.entryConfig, "tsconfig.json");
          assert.deepEqual(nodeProj.entryChain, [
            "tsconfig.json",
            "packages/node/tsconfig.json",
          ]);
          assert.equal(nodeProj.compilerOptions.strict, false);
          assert.equal(nodeProj.compilerOptions.allowJs, true);
          assert.deepEqual(nodeProj.compilerOptions.paths["@runtime"], [
            "./src/node.ts",
          ]);

          const sharedOrigin = session.fileOrigins.find(
            (o) => o.file === "packages/shared/shared.ts",
          );
          assert.ok(sharedOrigin);
          assert.deepEqual(
            sharedOrigin.projects.map((p) => p.projectId),
            ["packages/node/tsconfig.json", "packages/web/tsconfig.json"],
          );
          assert.equal(
            session.fileOrigins.some((o) => o.file.includes("node_modules")),
            false,
          );
          assert.equal(
            session.fileOrigins.some((o) => o.file.includes("lib.es")),
            false,
          );

          for (const ctx of session.projectContexts) {
            const calls = await ctx.findSyntax({
              files: ["packages/shared/shared.ts"],
              kinds: ["CallExpression"],
            });
            const checkCalls = calls.filter((c) =>
              c.text.startsWith("checkMode("),
            );
            for (const call of checkCalls) {
              const arg = call.arguments?.[0];
              if (!arg) continue;
              const [ty] = await ctx.typeAt([arg]);
              if (!ty) continue;
              observedTypesByProject.set(ctx.project.projectId, ty.typeText);
            }
          }
        } finally {
          await session.close();
        }

        assert.equal(
          observedTypesByProject.get("packages/web/tsconfig.json"),
          '"web"',
        );
        assert.equal(
          observedTypesByProject.get("packages/node/tsconfig.json"),
          '"node"',
        );
      },
    );
  });

  void it("supports multiple independent entry configs including jsconfig.json and rejects cross-project refs", async () => {
    await withTempHost(
      {
        hosts: [
          "sdk-projects/compiler-projects",
          "sdk-projects/compiler-projects-jsapp",
        ],
        setup: (host) => {
          host.removePath("tsconfig.json");
          materializeExtLib(host);
        },
      },
      async (host) => {
        await host.typecheck("packages/web/tsconfig.json");
        await host.typecheck("packages/node/tsconfig.json");

        let webRef: SyntaxRef | undefined;

        const session = await openProject({
          cwd: host.rootDir,
          projects: [
            "packages/web/tsconfig.json",
            "packages/node/tsconfig.json",
            "packages/jsapp/jsconfig.json",
          ],
        });
        try {
          assert.equal(session.projects.length, 3);
          for (const proj of session.projects) {
            assert.equal(proj.entryConfig, proj.configFile);
            assert.deepEqual(proj.entryChain, [proj.configFile]);
          }

          const webCtx = session.project("packages/web/tsconfig.json");
          const nodeCtx = session.project("packages/node/tsconfig.json");
          const webCalls = await webCtx.findSyntax({
            files: ["packages/shared/shared.ts"],
            kinds: ["CallExpression"],
          });
          webRef = webCalls[0]?.ref;
          assert.ok(webRef);

          await assert.rejects(
            () => nodeCtx.typeAt([webRef!]),
            (err: unknown) =>
              err instanceof TssaeError &&
              err.code === "CROSS_PROJECT_REFERENCE",
          );
        } finally {
          await session.close();
        }

        const singleSession = await openProject({
          cwd: host.rootDir,
          projects: ["packages/node/tsconfig.json"],
        });
        try {
          const singleNodeCtx = singleSession.project();
          await assert.rejects(
            () => singleNodeCtx.typeAt([webRef]),
            (err: unknown) =>
              err instanceof TssaeError &&
              (err.code === "STALE_REFERENCE" ||
                err.code === "CROSS_PROJECT_REFERENCE"),
          );
        } finally {
          await singleSession.close();
        }
      },
    );
  });
});
