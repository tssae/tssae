import assert from "node:assert/strict";
import { test } from "node:test";

import { withAcceptanceHost } from "@tssae/acceptance";

const fixtureTsconfig = JSON.stringify(
  {
    compilerOptions: {
      strict: true,
      target: "ES2022",
      module: "NodeNext",
      moduleResolution: "NodeNext",
      noEmit: true,
    },
    include: ["src/**/*.ts"],
  },
  null,
  2,
);

const consumerTsconfig = JSON.stringify(
  {
    compilerOptions: {
      strict: true,
      exactOptionalPropertyTypes: true,
      noUncheckedIndexedAccess: true,
      target: "ES2024",
      module: "NodeNext",
      moduleResolution: "NodeNext",
      erasableSyntaxOnly: true,
      rewriteRelativeImportExtensions: true,
      types: ["node"],
      noEmit: true,
    },
    include: ["consumer.ts", "tssae.summaries.ts"],
  },
  null,
  2,
);

const localSummaryTs = `import { defineSummaries } from "tssae";

export default defineSummaries({
  id: "acceptance-local-summaries",
  entries: [
    {
      id: "ext-pkg-run",
      match: {
        package: "ext-runner",
        version: "^1.0.0",
        memberPath: ["runTask"],
      },
      calls: {
        completeness: "complete",
        invokesCallbacks: [{ callbackArgIndex: 0, timing: "synchronous" }],
      },
    },
  ],
});
`;

const appTs = `import { runTask } from "ext-runner";

function fnA(): number {
  return 1;
}

function fnB(): number {
  return 2;
}

export function main(): number[] {
  runTask(fnA);
  return [fnA, fnB].map((f) => f());
}
`;

const consumerTs = `import assert from "node:assert/strict";

import { openProject, type EngineCallIoMetrics } from "tssae";

const metrics: EngineCallIoMetrics[] = [];
const session = await openProject({
  cwd: process.cwd(),
  tsconfig: "tsconfig.json",
  observe: "summary",
  onRpcMetrics: (m) => metrics.push(m),
});

try {
  const ctx = session.project(undefined, {
    observer: {
      observe: "detail",
      onRpcMetrics: (m) => metrics.push(m),
    },
  });

  const calls = await ctx.findSyntax({
    files: ["src/app.ts"],
    kinds: ["CallExpression"],
  });
  const runTaskSyn = calls.find((c) => c.text === "runTask(fnA)");
  const mapCbSyn = calls.find((c) => c.text === "f()");
  assert.ok(runTaskSyn && mapCbSyn);

  const [runTaskRef, mapCbRef] = await ctx.resolveCalls([
    runTaskSyn.ref,
    mapCbSyn.ref,
  ]);
  assert.ok(runTaskRef?.call && mapCbRef?.call);

  const [runTaskRes, mapCbRes] = await ctx.calleesOf([
    runTaskRef.call,
    mapCbRef.call,
  ]);
  assert.ok(runTaskRes && mapCbRes);

  assert.equal(runTaskRes.completeness, "complete");
  assert.deepEqual(
    runTaskRes.resolvedTargets.map((t) => t.name).toSorted(),
    ["fnA", "runTask"],
  );
  assert.ok(
    runTaskRes.evidence.some(
      (e) =>
        e.kind === "summary" &&
        e.summarySource === "local" &&
        e.summaryId === "acceptance-local-summaries",
    ),
  );

  assert.equal(mapCbRes.completeness, "complete");
  assert.deepEqual(
    mapCbRes.resolvedTargets.map((t) => t.name),
    ["fnA", "fnB"],
  );
  assert.ok(
    mapCbRes.evidence.some(
      (e) => e.kind === "summary" && e.summarySource === "builtin",
    ),
  );

  const summaryInputs = session.additionalInputs
    .filter((entry) => entry.sources.some((s) => s.kind === "summary"))
    .map((entry) => entry.file);
  assert.ok(summaryInputs.includes("tssae.summaries.ts"));

  console.log(
    JSON.stringify({
      runTaskTargets: runTaskRes.resolvedTargets.map((t) => t.name).toSorted(),
      mapCbTargets: mapCbRes.resolvedTargets.map((t) => t.name),
      summaryInputs,
    }),
  );
} finally {
  await session.close();
}
`;

void test("tarball consumer compiles defineSummaries and executes local and builtin behavior summaries end-to-end", async (t) => {
  await withAcceptanceHost(
    {
      files: {
        "package.json": JSON.stringify(
          {
            name: "acceptance-summaries-host",
            private: true,
            type: "module",
            devDependencies: {
              "@types/node": "^26.0.0",
              typescript: "^7.0.2",
            },
          },
          null,
          2,
        ),
        "tsconfig.json": fixtureTsconfig,
        "tssae.summaries.ts": localSummaryTs,
        "src/app.ts": appTs,
        "consumer.tsconfig.json": consumerTsconfig,
        "consumer.ts": consumerTs,
      },
    },
    async (host) => {
      host.writeText(
        "node_modules/ext-runner/package.json",
        JSON.stringify({
          name: "ext-runner",
          version: "1.0.0",
          types: "./index.d.ts",
        }),
      );
      host.writeText(
        "node_modules/ext-runner/index.d.ts",
        "export declare function runTask(cb: () => void): void;\n",
      );

      const tscFixture = await host.runTsc("tsconfig.json");
      assert.equal(
        tscFixture.status,
        0,
        `Fixture tsc failed:\n${tscFixture.stdout}\n${tscFixture.stderr}`,
      );

      const tscConsumer = await host.runTsc("consumer.tsconfig.json");
      assert.equal(
        tscConsumer.status,
        0,
        `Consumer tsc failed:\n${tscConsumer.stdout}\n${tscConsumer.stderr}`,
      );

      const runResult = await host.runNode(["consumer.ts"]);
      assert.equal(
        runResult.status,
        0,
        `Consumer run failed:\nstdout:\n${runResult.stdout}\nstderr:\n${runResult.stderr}`,
      );
      const parsed = JSON.parse(runResult.stdout.trim()) as {
        readonly runTaskTargets: readonly string[];
        readonly mapCbTargets: readonly string[];
        readonly summaryInputs: readonly string[];
      };
      assert.deepEqual(parsed.runTaskTargets, ["fnA", "runTask"]);
      assert.deepEqual(parsed.mapCbTargets, ["fnA", "fnB"]);
      assert.ok(parsed.summaryInputs.includes("tssae.summaries.ts"));
      t.diagnostic(
        `Verified out-of-repo defineSummaries and behavior summary E2E execution (${runResult.isolation?.mode})`,
      );
    },
  );
});
