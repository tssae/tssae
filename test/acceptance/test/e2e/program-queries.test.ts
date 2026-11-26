import assert from "node:assert/strict";
import { test } from "node:test";

import { withAcceptanceHost } from "@tssae/acceptance";
import { linkNodeModulesPackage } from "@tssae/test-hosts";

const fixtureTsconfig = JSON.stringify(
  {
    compilerOptions: {
      strict: true,
      target: "ES2022",
      module: "NodeNext",
      moduleResolution: "NodeNext",
      jsx: "preserve",
      noEmit: true,
    },
    include: ["src/**/*.ts", "src/**/*.tsx"],
  },
  null,
  2,
);

const jsxAmbientSource = `declare namespace JSX {
  interface IntrinsicElements {
    div: { children?: unknown };
  }
}
`;

const sourcesTs = `import { createExternalResource } from "decl-only-ext";
import { dtsOnlyWorkspaceOp } from "@ws/dts-only";

export type RowPayload = {
  readonly id: number;
  readonly name: string;
  readonly secret: string;
};

export function loadRow(): RowPayload {
  return { id: 1, name: "alice", secret: "s3cr3t" };
}

export function passThrough<T>(value: T): T {
  return value;
}

export function uncalledHelper(): number {
  return 42;
}

export class LocalConnection {
  readonly label: string;
  constructor(label: string) {
    this.label = label;
  }
  close(): void {}
}

export function createConnection(label: string): LocalConnection {
  return new LocalConnection(label);
}

export function createWrappedExternal(name: string) {
  return createExternalResource(name);
}

export function callWorkspaceDtsOnly(value: string): string {
  return dtsOnlyWorkspaceOp(value);
}
`;

const appTs = `import {
  createConnection,
  createWrappedExternal,
  loadRow,
  passThrough,
} from "./sources.js";

declare function guardedSink(v: unknown): void;
declare function deadSink(v: string): void;
declare function acceptPublicRow(row: { readonly id: number; readonly name: string }): void;

/** verule-ignore-next-code-line demo/rule -- JSDoc example must not match */
// verule-ignore-next-code-line demo/guarded -- Stacked line directive
/* verule-ignore-next-code-line demo/block --
 * Multiline block directive
 */
/** JSDoc between standalone comments and target function */
export function processOrder(isReady: boolean, hasAuth: boolean): void {
  const row = loadRow();
  const forwarded = passThrough(row);
  const { secret: _omitted, ...publicRow } = forwarded;
  const unrelated = { id: 99, name: "other" };
  acceptPublicRow(publicRow);
  void unrelated;

  if (isReady) {
    if (hasAuth) {
      guardedSink(publicRow);
    }
  }

  if (false) {
    deadSink("unreachable");
  }

  const connA = createConnection("A");
  const aliasA = connA;
  const connB = createConnection("B");
  const ext = createWrappedExternal("ext");
  void [aliasA, ext];
  connA.close();
  void connB;
}

// verule-ignore-next-code-line demo/unused -- Truncated by blank line

const literalNotComment = "// verule-ignore-next-code-line demo/ignored -- string";
void literalNotComment;
`;

const viewTsx = `declare function Item(): unknown;

export function View() {
  return (
    <div>
      // JsxText literal must be ignored
      {/* verule-ignore-next-code-line ui/no-raw -- Allowed in host */}
      <Item />
    </div>
  );
}
`;

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
    include: ["consumer.ts"],
  },
  null,
  2,
);

const consumerSource = `import assert from "node:assert/strict";

import {
  TssaeError,
  defineModel,
  evidenceOf,
  extractBasisEvidence,
  isSameInstance,
  openProject,
  resolveRetainedObjectFields,
  syntaxLocationOf,
  type CallRef,
  type DeclarationRef,
  type DiagnosticBasis,
  type DiagnosticLocation,
  type EngineCallIoMetrics,
  type Gap,
  type InstanceRef,
  type QueryCompletedEvent,
  type ScannedComment,
  type SyntaxRef,
} from "tssae";

const resourceModel = defineModel({
  id: "acceptance-connection-model",
  description: "Local connections must be closed on all paths.",
  instanceBehaviors: [
    {
      kind: "allocates-fresh-instance",
      match: { symbol: "createConnection" },
    },
  ],
  cleanupOperations: [
    {
      kind: "disposes-target",
      match: { container: "LocalConnection", member: "close" },
      target: "receiver",
    },
  ],
});

const openMetrics: EngineCallIoMetrics[] = [];
const session = await openProject({
  cwd: process.cwd(),
  tsconfig: "tsconfig.json",
  models: [resourceModel],
  observe: "summary",
  onRpcMetrics: (m) => openMetrics.push(m),
});

try {
  assert.equal(openMetrics.length, 1);
  assert.ok(openMetrics[0]?.engineMetrics);

  const observedQueries: QueryCompletedEvent[] = [];
  const observedGaps: Gap[] = [];
  const observedRpc: EngineCallIoMetrics[] = [];

  const ctx = session.project(undefined, {
    observer: {
      observe: "detail",
      explainTarget: { file: "src/app.ts", line: 18 },
      onQueryCompleted: (ev) => observedQueries.push(ev),
      onGaps: (gaps) => observedGaps.push(...gaps),
      onRpcMetrics: (m) => observedRpc.push(m),
    },
  });

  // 1. Cross-file calls (positive & negative) + ProgramRef chaining
  const appCalls = await ctx.findSyntax({
    files: ["src/app.ts"],
    kinds: ["CallExpression"],
  });
  const sourceCalls = await ctx.findSyntax({
    files: ["src/sources.ts"],
    kinds: ["CallExpression"],
  });
  const sourceDecls = await ctx.findSyntax({
    files: ["src/sources.ts"],
    kinds: ["FunctionDeclaration"],
  });

  const loadRowCallSyn = appCalls.find((c) => c.text === "loadRow()");
  const passThroughCallSyn = appCalls.find((c) => c.text.startsWith("passThrough("));
  const connACallSyn = appCalls.find((c) => c.text === 'createConnection("A")');
  const connBCallSyn = appCalls.find((c) => c.text === 'createConnection("B")');
  const extCallSyn = sourceCalls.find((c) => c.text.startsWith("createExternalResource("));
  const wsDtsCallSyn = sourceCalls.find((c) => c.text.startsWith("dtsOnlyWorkspaceOp("));
  const loadRowDeclSyn = sourceDecls.find((d) => d.text.includes("function loadRow"));
  const uncalledDeclSyn = sourceDecls.find((d) => d.text.includes("function uncalledHelper"));
  const acceptRowCallSyn = appCalls.find((c) => c.text.startsWith("acceptPublicRow("));
  const guardedSinkCallSyn = appCalls.find((c) => c.text.startsWith("guardedSink("));
  const deadSinkCallSyn = appCalls.find((c) => c.text.startsWith("deadSink("));

  assert.ok(
    loadRowCallSyn &&
      passThroughCallSyn &&
      connACallSyn &&
      connBCallSyn &&
      extCallSyn &&
      wsDtsCallSyn &&
      loadRowDeclSyn &&
      uncalledDeclSyn &&
      acceptRowCallSyn &&
      guardedSinkCallSyn &&
      deadSinkCallSyn,
  );
  assert.deepEqual(syntaxLocationOf(loadRowCallSyn.ref), loadRowCallSyn.location);

  const resolvedAppCalls = await ctx.resolveCalls(
    [loadRowCallSyn.ref, acceptRowCallSyn.ref, connACallSyn.ref],
    { select: { signature: true, calleeDeclaration: true, arguments: true } },
  );
  const [loadRowResolved, acceptRowResolved, connAResolved] = resolvedAppCalls;
  assert.ok(loadRowResolved?.call && acceptRowResolved?.call && connAResolved?.call);

  const [extCallResolved, wsDtsCallResolved] = await ctx.resolveCalls([
    extCallSyn.ref,
    wsDtsCallSyn.ref,
  ]);
  assert.ok(extCallResolved?.call && wsDtsCallResolved?.call);

  // Chain CallRef -> calleesOf -> DeclarationRef -> callersOf
  const [loadRowCallees] = await ctx.calleesOf([loadRowResolved.call]);
  assert.ok(loadRowCallees);
  const loadRowTarget = loadRowCallees.resolvedTargets[0];
  assert.ok(loadRowTarget);
  assert.equal(loadRowTarget.name, "loadRow");
  assert.equal(loadRowTarget.location?.file, "src/sources.ts");

  const [uncalledDeclRow] = await ctx.resolveDeclarations([uncalledDeclSyn.ref], {
    select: { name: true, location: true },
  });
  assert.ok(uncalledDeclRow?.declaration);

  const callerResults = await ctx.callersOf([
    loadRowTarget.declaration,
    uncalledDeclRow.declaration,
  ]);
  assert.ok(
    callerResults[0]?.callSites.some(
      (site) => site.location.file === "src/app.ts",
    ),
  );
  assert.equal(callerResults[1]?.callSites.length, 0);

  // 2. Branch coverage (must-pass dominance: covered vs bypass vs unreachable)
  const appFns = await ctx.findSyntax({
    files: ["src/app.ts"],
    kinds: ["FunctionDeclaration"],
  });
  const processOrderFn = appFns.find((f) => f.text.includes("function processOrder"));
  assert.ok(processOrderFn);

  const [branchInfo] = await ctx.listBranches([processOrderFn.ref]);
  assert.ok(branchInfo);
  const readyTrueBranch = branchInfo.branches.find(
    (b) => b.conditionText.includes("isReady") && b.direction === "true",
  );
  const authTrueBranch = branchInfo.branches.find(
    (b) => b.conditionText.includes("hasAuth") && b.direction === "true",
  );
  const falseTrueBranch = branchInfo.branches.find(
    (b) => b.conditionText.includes("false") && b.direction === "true",
  );
  assert.ok(readyTrueBranch && authTrueBranch && falseTrueBranch);

  const [covPositive, covNegative, covDead] = await Promise.all([
    ctx.branchCoverage({
      function: processOrderFn.ref,
      branches: [readyTrueBranch.ref],
      targets: [guardedSinkCallSyn.ref],
    }),
    ctx.branchCoverage({
      function: processOrderFn.ref,
      branches: [falseTrueBranch.ref],
      targets: [guardedSinkCallSyn.ref],
    }),
    ctx.branchCoverage({
      function: processOrderFn.ref,
      branches: [falseTrueBranch.ref],
      targets: [deadSinkCallSyn.ref],
    }),
  ]);
  assert.equal(covPositive[0]?.status, "proved");
  assert.equal(covPositive[0]?.targetReachable, true);
  assert.equal(covNegative[0]?.status, "disproved");
  assert.equal(covNegative[0]?.targetReachable, true);
  assert.ok(
    covNegative[0]?.status === "disproved" &&
      covNegative[0].counterexamplePath.steps.length > 0,
  );
  assert.equal(covDead[0]?.status, "proved");
  assert.equal(covDead[0]?.targetReachable, false);

  // 3. Value propagation + retained object fields (positive & negative)
  const appVars = await ctx.findSyntax({
    files: ["src/app.ts"],
    kinds: ["VariableDeclaration"],
  });
  const unrelatedVar = appVars.find((v) => v.text.startsWith("unrelated ="));
  assert.ok(unrelatedVar?.initializer && acceptRowResolved.arguments?.[0]);

  const propResult = await ctx.valuePropagation({
    propagation: "value",
    includeMemberAccess: true,
    from: loadRowCallSyn.ref,
    to: [acceptRowResolved.arguments[0], unrelatedVar.initializer],
  });
  const [propPositive, propNegative] = propResult.results;
  assert.equal(propPositive?.reachable, true);
  assert.equal(propNegative?.reachable, false);

  const propPath = propPositive?.paths[0];
  assert.ok(propPath);
  const retained = resolveRetainedObjectFields(
    { fields: ["id", "name", "secret"] },
    propPath,
  );
  assert.ok(retained);
  assert.deepEqual(retained.fields, ["id", "name"]);
  assert.deepEqual(retained.omittedFields, ["secret"]);

  // 4. Object identity + cleanup (positive & negative)
  const aliasAVar = appVars.find((v) => v.text.startsWith("aliasA ="));
  assert.ok(aliasAVar?.initializer);
  const [instA, instAliasA, instB] = await ctx.resolveInstances([
    connACallSyn.ref,
    aliasAVar.initializer,
    connBCallSyn.ref,
  ]);
  assert.ok(instA?.instance && instAliasA?.instance && instB?.instance);
  assert.equal(isSameInstance(instA.instance, instAliasA.instance), true);
  assert.equal(isSameInstance(instA.instance, instB.instance), false);

  const [projectedInstA] = await ctx.projectInstances([instA.instance], {
    select: {
      instanceKey: true,
      originKind: true,
      allocationLocation: true,
      modelId: true,
      evidence: true,
    },
  });
  assert.equal(projectedInstA?.originKind, "allocation");
  assert.equal(projectedInstA?.allocationLocation?.file, "src/sources.ts");

  const [cleanupScope] = await ctx.instanceCleanup({
    scopes: [processOrderFn.ref],
    targets: [instA.instance, instB.instance],
  });
  assert.ok(cleanupScope);
  const cleanupA = cleanupScope.targetResults.find((tr) =>
    isSameInstance(tr.instance, instA.instance),
  );
  const cleanupB = cleanupScope.targetResults.find((tr) =>
    isSameInstance(tr.instance, instB.instance),
  );
  assert.equal(cleanupA?.cleanedOnAllPaths, true);
  assert.equal(cleanupB?.cleanedOnAllPaths, false);

  // 5. Evidence traceability & external declaration-only gap handling
  const basis: readonly DiagnosticBasis[] = [
    resourceModel,
    loadRowTarget,
    propPath,
    loadRowCallSyn,
  ];
  const combinedEvidence = extractBasisEvidence(basis);
  assert.ok(evidenceOf(propPath).length > 0);
  assert.ok(
    combinedEvidence.some(
      (ev) => ev.kind === "model" && ev.modelId === "acceptance-connection-model",
    ),
  );
  const originMap = new Map(
    session.fileOrigins.map((o) => [o.file, o.projects.map((p) => p.projectId)]),
  );
  const sourceLocations = combinedEvidence
    .map((ev) => ev.at)
    .filter((loc): loc is DiagnosticLocation => loc !== undefined);
  assert.ok(sourceLocations.length > 0);
  for (const loc of sourceLocations) {
    assert.ok(
      (originMap.get(loc.file)?.length ?? 0) > 0,
      \`Evidence location \${loc.file} must trace to an opened project origin\`,
    );
  }

  const implResults = await ctx.resolveImplementations([
    extCallResolved.call,
    connAResolved.call,
    wsDtsCallResolved.call,
  ]);
  const [extImpl, localImpl, wsDtsImpl] = implResults;
  assert.equal(extImpl?.status, "external");
  assert.equal(extImpl?.packageName, "decl-only-ext");
  assert.equal(localImpl?.status, "resolved");
  assert.equal(localImpl?.implementation.resolutionSource, "current-project");
  assert.equal(localImpl?.implementation.file, "src/sources.ts");
  assert.equal(wsDtsImpl?.status, "inconclusive");
  assert.ok((wsDtsImpl?.gaps.length ?? 0) > 0);
  assert.ok(observedGaps.length > 0);
  assert.ok(observedQueries.some((q) => q.op === "resolveImplementations"));
  assert.ok(observedRpc.some((m) => m.isQueryBatch && m.engineMetrics !== undefined));

  // 6. Comment & nextCodeLine scanning for consumer-side ADR 0007 suppression
  const comments: readonly ScannedComment[] = await session.scanComments();
  assert.equal(comments.length, 4);
  const directives = comments.flatMap((comment) => {
    const match = /^verule-ignore-next-code-line\\s+(\\S+)\\s+--\\s+([\\s\\S]+)$/u.exec(
      comment.text.trim(),
    );
    if (!match) return [];
    return [
      {
        ruleId: match[1]!,
        reason: match[2]!.trim(),
        at: comment.at,
        nextCodeLine: comment.nextCodeLine,
        isJsx: comment.isJsx,
      },
    ];
  });
  assert.equal(directives.length, 4);
  assert.equal(directives[0]?.ruleId, "demo/guarded");
  assert.equal(directives[0]?.nextCodeLine, 18);
  assert.equal(directives[1]?.ruleId, "demo/block");
  assert.equal(directives[1]?.reason, "Multiline block directive");
  assert.equal(directives[1]?.nextCodeLine, 18);
  assert.equal(directives[2]?.ruleId, "demo/unused");
  assert.equal(directives[2]?.nextCodeLine, null);
  assert.equal(directives[3]?.ruleId, "ui/no-raw");
  assert.equal(directives[3]?.isJsx, true);
  assert.equal(directives[3]?.at.file, "src/view.tsx");
  assert.equal(directives[3]?.nextCodeLine, 8);

  const shouldSuppress = (ruleId: string, file: string, line: number): boolean =>
    directives.some(
      (d) => d.ruleId === ruleId && d.at.file === file && d.nextCodeLine === line,
    );
  assert.equal(shouldSuppress("demo/guarded", "src/app.ts", 18), true);
  assert.equal(shouldSuppress("demo/rule", "src/app.ts", 12), false);
  assert.equal(shouldSuppress("demo/unused", "src/app.ts", 46), false);
  assert.equal(shouldSuppress("ui/no-raw", "src/view.tsx", 8), true);

  process.stdout.write(
    JSON.stringify({
      callersFound: callerResults[0]?.callSites.length ?? 0,
      branchStatuses: [
        covPositive[0]?.status,
        covNegative[0]?.status,
        covDead[0]?.status,
      ],
      propRetained: retained.fields,
      sameInstance: [
        isSameInstance(instA.instance, instAliasA.instance),
        isSameInstance(instA.instance, instB.instance),
      ],
      extImplStatus: extImpl?.status,
      wsDtsImplStatus: wsDtsImpl?.status,
      wsDtsGapKinds: wsDtsImpl?.gaps.map((g) => g.kind),
      wsDtsGapMessages: wsDtsImpl?.gaps.map((g) => g.message),
      scannedDirectiveRules: directives.map((d) => d.ruleId),
    }) + "\\n",
  );
} finally {
  await session.close();
}

await assert.rejects(
  () => session.scanComments(),
  (err: unknown) => err instanceof TssaeError && err.code === "SESSION_CLOSED",
);
`;

void test("installs candidate tssae out of repo and verifies program queries, evidence/gaps, and comment scanning", async (t) => {
  await withAcceptanceHost(
    {
      files: {
        "package.json": JSON.stringify(
          {
            name: "tssae-program-queries-consumer",
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
        "src/jsx-ambient.d.ts": jsxAmbientSource,
        "src/sources.ts": sourcesTs,
        "src/app.ts": appTs,
        "src/view.tsx": viewTsx,
        "consumer.tsconfig.json": consumerTsconfig,
        "consumer.ts": consumerSource,
      },
    },
    async (host) => {
      host.writeText(
        "node_modules/decl-only-ext/package.json",
        JSON.stringify(
          {
            name: "decl-only-ext",
            version: "1.0.0",
            types: "./index.d.ts",
          },
          null,
          2,
        ),
      );
      host.writeText(
        "node_modules/decl-only-ext/index.d.ts",
        "export declare function createExternalResource(name: string): { readonly name: string };\n",
      );
      host.writeText(
        "packages/ws-dts/package.json",
        JSON.stringify(
          {
            name: "@ws/dts-only",
            version: "1.0.0",
            types: "./dist/index.d.ts",
          },
          null,
          2,
        ),
      );
      host.writeText(
        "packages/ws-dts/dist/index.d.ts",
        "export declare function dtsOnlyWorkspaceOp(value: string): string;\n",
      );
      linkNodeModulesPackage(
        host.hostDir,
        "@ws/dts-only",
        host.resolvePath("packages/ws-dts"),
      );

      const tscFixture = await host.runTsc("tsconfig.json");
      assert.equal(
        tscFixture.status,
        0,
        `Fixture tsc --noEmit failed:\n${tscFixture.stdout}\n${tscFixture.stderr}`,
      );

      const tscConsumer = await host.runTsc("consumer.tsconfig.json");
      assert.equal(
        tscConsumer.status,
        0,
        `Consumer tsc --noEmit failed:\n${tscConsumer.stdout}\n${tscConsumer.stderr}`,
      );
      assert.equal(tscConsumer.isolation?.controlProbeBlocked, true);

      const runResult = await host.runNode(["consumer.ts"]);
      assert.equal(
        runResult.status,
        0,
        `Consumer script failed (status=${String(runResult.status)}):\nstdout:\n${runResult.stdout}\nstderr:\n${runResult.stderr}`,
      );
      assert.equal(runResult.timedOut, false);
      assert.equal(runResult.isolation?.controlProbeBlocked, true);

      const summary = JSON.parse(runResult.stdout.trim()) as {
        readonly callersFound: number;
        readonly branchStatuses: readonly string[];
        readonly propRetained: readonly string[];
        readonly sameInstance: readonly boolean[];
        readonly extImplStatus: string;
        readonly wsDtsImplStatus: string;
        readonly wsDtsGapKinds: readonly string[];
        readonly wsDtsGapMessages: readonly string[];
        readonly scannedDirectiveRules: readonly string[];
      };

      assert.equal(summary.callersFound, 1);
      assert.deepEqual(summary.branchStatuses, [
        "proved",
        "disproved",
        "proved",
      ]);
      assert.deepEqual(summary.propRetained, ["id", "name"]);
      assert.deepEqual(summary.sameInstance, [true, false]);
      assert.equal(summary.extImplStatus, "external");
      assert.equal(summary.wsDtsImplStatus, "inconclusive");
      assert.ok(summary.wsDtsGapKinds.includes("missing-source-file"));
      assert.match(summary.wsDtsGapMessages[0] ?? "", /project references/i);
      assert.match(summary.wsDtsGapMessages[0] ?? "", /customConditions/);
      assert.match(summary.wsDtsGapMessages[0] ?? "", /declarationMap/);
      assert.deepEqual(summary.scannedDirectiveRules, [
        "demo/guarded",
        "demo/block",
        "demo/unused",
        "ui/no-raw",
      ]);

      t.diagnostic(
        `Verified out-of-repo program queries, evidence/gaps, and comment scanning; isolation=${runResult.isolation?.mode}`,
      );
    },
  );
});
