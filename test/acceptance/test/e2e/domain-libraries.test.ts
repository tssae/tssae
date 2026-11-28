import assert from "node:assert/strict";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { packWorkspaceArtifacts, withAcceptanceHost } from "@tssae/acceptance";

const acceptanceRequire = createRequire(import.meta.url);

const WORKSPACE_PACKAGE_SPECIFIERS = [
  "tssae",
  "@tssae/zod",
  "@tssae/drizzle",
  "@tssae/test-hosts",
  "@tssae/tooling-config",
  "@tssae/acceptance",
] as const;

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

function prepareMultiPackageHost(hostDir: string): void {
  renameSync(
    join(hostDir, "packages/app/tsconfig.runtime.json"),
    join(hostDir, "packages/app/tsconfig.json"),
  );
  rmSync(join(hostDir, "tsconfig.json"), { force: true });
}

function readDeclaredWorkspaceManifests(): readonly {
  readonly name: string;
  readonly private?: boolean;
}[] {
  return WORKSPACE_PACKAGE_SPECIFIERS.map((pkgName) => {
    const manifestPath = acceptanceRequire.resolve(`${pkgName}/package.json`);
    const parsed = JSON.parse(readFileSync(manifestPath, "utf8")) as {
      readonly name: string;
      readonly private?: boolean;
    };
    return {
      name: parsed.name,
      ...(parsed.private !== undefined ? { private: parsed.private } : {}),
    };
  });
}

const singleZodConsumer = `import assert from "node:assert/strict";

import {
  extractZodInputFields,
  extractZodOutputFields,
  zodValidator,
} from "@tssae/zod";
import {
  extractBasisEvidence,
  openProject,
  type EngineCallIoMetrics,
  type Gap,
  type QueryCompletedEvent,
} from "tssae";

assert.equal(process.versions.node, "24.0.0");
assert.equal(zodValidator.id, "zod");

const openRpcMetrics: EngineCallIoMetrics[] = [];
const session = await openProject({
  cwd: process.cwd(),
  projects: ["packages/db/tsconfig.json", "packages/app/tsconfig.json"],
  observe: "detail",
  onRpcMetrics: (metrics) => openRpcMetrics.push(metrics),
});

try {
  const observedQueries: QueryCompletedEvent[] = [];
  const recordedGaps: Gap[] = [];
  const queryRpcMetrics: EngineCallIoMetrics[] = [];

  const appCtx = session.project("packages/app/tsconfig.json", {
    observer: {
      observe: "detail",
      onQueryCompleted: (event) => observedQueries.push(event),
      onGaps: (gaps) => recordedGaps.push(...gaps),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });

  const inputCalls = await extractZodInputFields(appCtx, { scope: "calls" });
  const outputCalls = await extractZodOutputFields(appCtx, { scope: "calls" });

  const accountIn = inputCalls.find((r) => r.schemaName === "accountSchema");
  const accountOut = outputCalls.find((r) => r.schemaName === "accountSchema");
  const userIn = inputCalls.find((r) => r.schemaName === "userSchema");
  const userOut = outputCalls.find((r) => r.schemaName === "userSchema");
  assert.ok(accountIn && accountOut && userIn && userOut);

  assert.equal(accountIn.validatorMethod, "safeParse");
  assert.deepEqual(accountIn.requiredFields, ["id", "nullableName"]);
  assert.deepEqual(accountIn.optionalFields, [
    "fallback",
    "nickname",
    "state",
  ]);
  assert.deepEqual(accountIn.defaultedFields, ["fallback", "state"]);
  assert.equal(accountIn.fields.nullableName?.nullable, true);
  assert.equal(accountIn.hasDynamicFields, true);
  assert.deepEqual(accountOut.presentFields, [
    "fallback",
    "id",
    "nullableName",
    "state",
  ]);
  assert.deepEqual(accountOut.optionalFields, ["nickname"]);
  assert.deepEqual(accountOut.defaultedFields, ["fallback", "state"]);
  assert.ok(
    accountIn.gaps.some((gap) => gap.kind === "dynamic-schema-spread"),
  );
  assert.ok(accountIn.gaps.some((gap) => gap.kind === "dynamic-schema-field"));

  assert.equal(userIn.validatorMethod, "parse");
  assert.deepEqual(userIn.requiredFields, []);
  assert.deepEqual(userIn.optionalFields, ["age"]);
  assert.deepEqual(userIn.defaultedFields, ["age"]);
  assert.equal(userIn.hasDynamicFields, true);
  assert.deepEqual(userOut.presentFields, ["age"]);
  assert.deepEqual(userOut.defaultedFields, ["age"]);
  assert.equal(userIn.providerId, "zod");
  assert.equal(userOut.providerId, "zod");
  assert.ok(
    extractBasisEvidence([userIn]).some(
      (ev) => ev.at?.file === "packages/app/src/zod-fields.ts",
    ),
  );
  assert.equal(
    userIn.evidence.some(
      (ev) => ev.at?.file === "packages/db/src/user-table.ts",
    ),
    false,
  );
  assert.ok(
    userIn.gaps.some(
      (gap) =>
        gap.kind === "unresolved-schema" &&
        gap.at?.file === "packages/app/src/zod-fields.ts" &&
        gap.at.line === 21,
    ),
  );
  assert.ok(recordedGaps.some((gap) => gap.kind === "unresolved-schema"));

  const directSchema = (
    await appCtx.findSyntax({
      files: ["packages/app/src/zod-fields.ts"],
      kinds: ["CallExpression"],
      textIncludes: ["standaloneOnly"],
    })
  )[0];
  assert.ok(directSchema);
  const directInputs = await extractZodInputFields(appCtx, {
    scope: "schemas",
    targets: [directSchema.ref],
  });
  assert.equal(directInputs.length, 1);
  assert.deepEqual(directInputs[0]?.requiredFields, ["standaloneOnly"]);
  assert.equal(directInputs[0]?.providerId, "zod");
  assert.ok(observedQueries.length > 0);
  assert.ok(queryRpcMetrics.length > 0);

  process.stdout.write(
    JSON.stringify({
      nodeVersion: process.versions.node,
      providerId: zodValidator.id,
      userDefaultedFields: userIn.defaultedFields,
      userGapKinds: userIn.gaps.map((g) => g.kind),
    }) + "\\n",
  );
} finally {
  await session.close();
}
`;

const singleDrizzleConsumer = `import assert from "node:assert/strict";

import {
  drizzleQueryProjection,
  extractDrizzleRowProjections,
  extractDrizzleTableColumns,
  extractDrizzleZodSchemas,
} from "@tssae/drizzle";
import {
  extractBasisEvidence,
  openProject,
  type EngineCallIoMetrics,
  type Gap,
  type QueryCompletedEvent,
} from "tssae";

assert.equal(process.versions.node, "24.0.0");
assert.equal(drizzleQueryProjection.id, "drizzle");

const session = await openProject({
  cwd: process.cwd(),
  projects: ["packages/db/tsconfig.json", "packages/app/tsconfig.json"],
  observe: "detail",
});

try {
  const observedQueries: QueryCompletedEvent[] = [];
  const recordedGaps: Gap[] = [];
  const queryRpcMetrics: EngineCallIoMetrics[] = [];

  const dbCtx = session.project("packages/db/tsconfig.json", {
    observer: {
      observe: "detail",
      onQueryCompleted: (event) => observedQueries.push(event),
      onGaps: (gaps) => recordedGaps.push(...gaps),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });
  const appCtx = session.project("packages/app/tsconfig.json", {
    observer: {
      observe: "detail",
      onQueryCompleted: (event) => observedQueries.push(event),
      onGaps: (gaps) => recordedGaps.push(...gaps),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });

  const tables = await extractDrizzleTableColumns(dbCtx);
  assert.equal(tables.length, 4);

  const accountsTable = tables.find((t) => t.tableName === "accounts");
  assert.ok(accountsTable);
  assert.equal(accountsTable.sqlTableName, "accounts");
  assert.deepEqual(accountsTable.columnNames, [
    "createdAt",
    "displayName",
    "email",
    "id",
    "role",
  ]);
  assert.equal(accountsTable.columns.id?.primaryKey, true);
  assert.equal(accountsTable.columns.email?.nullable, false);
  assert.equal(accountsTable.columns.displayName?.nullable, true);
  assert.equal(accountsTable.columns.role?.hasDefault, true);
  assert.equal(accountsTable.columns.createdAt?.hasDefault, true);
  assert.ok(
    extractBasisEvidence([accountsTable]).some(
      (ev) => ev.at?.file === "packages/db/src/drizzle-tables.ts",
    ),
  );

  const dynamicTable = tables.find(
    (t) => t.tableName === "dynamicAuditEvents",
  );
  assert.ok(dynamicTable);
  assert.equal(dynamicTable.hasDynamicColumns, true);
  assert.deepEqual(dynamicTable.columnNames, ["id"]);
  assert.equal(dynamicTable.gaps.length, 2);
  assert.ok(
    dynamicTable.gaps.every((gap) => gap.kind === "dynamic-table-columns"),
  );

  const inspectCalls = await appCtx.findSyntax({
    files: ["packages/app/src/consumer.ts"],
    kinds: ["CallExpression"],
    calleeNames: ["inspectTable"],
  });
  const tableArg = inspectCalls[0]?.arguments?.[0];
  assert.ok(tableArg);
  const crossPackageTable = await extractDrizzleTableColumns(appCtx, {
    targets: [tableArg],
  });
  assert.equal(crossPackageTable.length, 1);
  assert.equal(crossPackageTable[0]?.tableName, "accounts");
  assert.ok(
    crossPackageTable[0]?.evidence.some(
      (ev) => ev.at?.file === "packages/db/src/drizzle-tables.ts",
    ),
  );

  const projections = await extractDrizzleRowProjections(dbCtx, {
    library: "drizzle",
  });
  assert.equal(projections.length, 3);

  const explicitProj = projections.find((p) =>
    p.projectedColumns.includes("displayName"),
  );
  assert.ok(explicitProj);
  assert.deepEqual(explicitProj.projectedColumns, ["displayName", "id"]);
  assert.equal(explicitProj.columns.displayName?.sqlName, "full_name");

  const relationalProj = projections.find(
    (p) => p.queryKind === "relational",
  );
  assert.ok(relationalProj);
  assert.deepEqual(relationalProj.projectedColumns, [
    "fullName",
    "id",
    "workspace",
  ]);
  assert.deepEqual(relationalProj.relations.workspace?.columns, [
    "displayName",
    "id",
    "slug",
  ]);

  const dynamicProj = projections.find((p) => p.hasDynamicColumns);
  assert.ok(dynamicProj);
  assert.deepEqual(dynamicProj.projectedColumns, ["id"]);
  assert.ok(
    dynamicProj.gaps.some((gap) => gap.kind === "dynamic-query-projection"),
  );

  const schemas = await extractDrizzleZodSchemas(dbCtx);
  assert.equal(schemas.length, 2);

  const strictSchema = schemas.find(
    (s) => s.schemaName === "strictWorkspaceSchema",
  );
  const refinedSchema = schemas.find(
    (s) => s.schemaName === "refinedWorkspaceSchema",
  );
  assert.ok(strictSchema && refinedSchema);
  assert.equal(strictSchema.library, "drizzle-zod");
  assert.deepEqual(Object.keys(strictSchema.fields).toSorted(), [
    "displayName",
    "id",
    "slug",
  ]);
  assert.equal(strictSchema.fields.slug?.resolved, true);
  assert.equal(refinedSchema.fields.id?.resolved, true);
  assert.equal(refinedSchema.fields.displayName?.resolved, true);
  assert.equal(refinedSchema.fields.slug?.resolved, true);
  assert.equal(refinedSchema.fields.slug?.requirement, "optional");
  assert.ok(observedQueries.length > 0);
  assert.ok(recordedGaps.length > 0);
  assert.ok(queryRpcMetrics.length > 0);

  process.stdout.write(
    JSON.stringify({
      nodeVersion: process.versions.node,
      tableNames: tables.map((t) => t.tableName).toSorted(),
      schemasCount: schemas.length,
    }) + "\\n",
  );
} finally {
  await session.close();
}
`;

const dualDomainLibsConsumer = `import assert from "node:assert/strict";

import {
  drizzleQueryProjection,
  drizzleZodSchemaResolver,
  drizzleZodValidator,
  extractDrizzleRowProjections,
  extractDrizzleTableColumns,
  extractDrizzleZodSchemas,
} from "@tssae/drizzle";
import {
  extractZodInputFields,
  extractZodOutputFields,
  zodValidator,
} from "@tssae/zod";
import {
  extractBasisEvidence,
  openProject,
  resolveRetainedObjectFields,
  type EngineCallIoMetrics,
  type QueryCompletedEvent,
} from "tssae";

assert.equal(process.versions.node, "24.0.0");
assert.equal(drizzleQueryProjection.id, "drizzle");
assert.equal(drizzleZodSchemaResolver.id, "drizzle-zod");
assert.equal(drizzleZodValidator.id, "drizzle-zod");
assert.equal(zodValidator.id, "zod");

const session = await openProject({
  cwd: process.cwd(),
  projects: ["packages/db/tsconfig.json", "packages/app/tsconfig.json"],
  observe: "detail",
});

try {
  const observedQueries: QueryCompletedEvent[] = [];
  const queryRpcMetrics: EngineCallIoMetrics[] = [];

  const dbCtx = session.project("packages/db/tsconfig.json", {
    observer: {
      observe: "detail",
      onQueryCompleted: (event) => observedQueries.push(event),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });
  const appCtx = session.project("packages/app/tsconfig.json", {
    observer: {
      observe: "detail",
      onQueryCompleted: (event) => observedQueries.push(event),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });

  // 1. Complete known table columns from @acceptance-host/db
  const tables = await extractDrizzleTableColumns(dbCtx);
  assert.equal(tables.length, 3);
  assert.deepEqual(
    tables.map((t) => t.tableName).toSorted(),
    ["page", "users", "workspace"],
  );
  const usersTable = tables.find((t) => t.tableName === "users");
  assert.ok(usersTable);
  assert.deepEqual(usersTable.columnNames, [
    "age",
    "fullName",
    "id",
    "status",
  ]);
  assert.equal(usersTable.columns.age?.nullable, false);

  // 2. Only real drizzle-zod calls emit schemas (fake local createSelectSchema excluded)
  const dbSchemas = await extractDrizzleZodSchemas(dbCtx);
  const appSchemas = await extractDrizzleZodSchemas(appCtx);
  assert.equal(dbSchemas.length, 4);
  assert.equal(appSchemas.length, 0);

  // 3. Field override order: Drizzle base + drizzle-zod refinement + Zod .extend() / .pick()
  const dbInputs = await drizzleZodValidator.extractInputFields(dbCtx, {
    scope: "schemas",
  });
  const dbOutputs = await drizzleZodValidator.extractOutputFields(dbCtx, {
    scope: "schemas",
  });

  const userIn = dbInputs.find((r) => r.schemaName === "userSchema");
  const userOut = dbOutputs.find((r) => r.schemaName === "userSchema");
  assert.ok(userIn && userOut);
  assert.deepEqual(userIn.requiredFields, ["fullName", "id", "status"]);
  assert.deepEqual(userIn.defaultedFields, ["age"]);
  assert.deepEqual(userIn.optionalFields, ["age"]);
  assert.deepEqual(userOut.presentFields, [
    "age",
    "fullName",
    "id",
    "status",
  ]);
  assert.deepEqual(userOut.defaultedFields, ["age"]);
  assert.ok(
    userIn.evidence.some(
      (ev) =>
        ev.at?.file === "packages/db/src/drizzle-schema.ts" &&
        ev.at.line === 9,
    ),
  );
  assert.ok(
    userIn.evidence.some(
      (ev) =>
        ev.at?.file === "packages/db/src/drizzle-schema.ts" &&
        ev.at.line === 34,
    ),
  );

  const refinedIn = dbInputs.find(
    (r) => r.schemaName === "refinedWorkspaceSchema",
  );
  const refinedOut = dbOutputs.find(
    (r) => r.schemaName === "refinedWorkspaceSchema",
  );
  assert.ok(refinedIn && refinedOut);
  assert.deepEqual(refinedIn.requiredFields, ["id"]);
  assert.deepEqual(refinedIn.defaultedFields, ["name", "plan"]);
  assert.deepEqual(refinedIn.optionalFields, ["name", "plan", "slug"]);
  assert.equal(refinedIn.fields.name?.nullable, true);
  assert.equal(refinedIn.fields.slug?.nullable, false);
  assert.deepEqual(refinedOut.presentFields, ["id", "name", "plan"]);
  assert.deepEqual(refinedOut.optionalFields, ["slug"]);

  const gateIn = dbInputs.find((r) => r.schemaName === "gateFieldsSchema");
  assert.ok(gateIn);
  assert.deepEqual(gateIn.requiredFields, ["customDomain", "slug"]);
  assert.equal(gateIn.fields.customDomain?.nullable, true);
  assert.deepEqual(gateIn.optionalFields, ["password"]);
  assert.deepEqual(gateIn.defaultedFields, ["password"]);

  // 4. Cross-package query + validator call evidence and propagation in @acceptance-host/app
  const appInputCalls = await extractZodInputFields(appCtx, {
    scope: "calls",
    externalSchemaResolvers: [drizzleZodSchemaResolver],
  });
  const appInputSchemas = await extractZodOutputFields(appCtx, {
    scope: "schemas",
    externalSchemaResolvers: [drizzleZodSchemaResolver],
  });

  const fakeLocalOut = appInputSchemas.find(
    (r) => r.schemaName === "fakeLocalSelectSchema",
  );
  assert.ok(fakeLocalOut);
  assert.deepEqual(fakeLocalOut.presentFields, []);
  assert.equal(fakeLocalOut.hasDynamicFields, true);
  assert.ok(
    fakeLocalOut.gaps.some((gap) => gap.kind === "unresolved-schema"),
  );

  const appCalls = await appCtx.findSyntax({
    files: ["packages/app/src/row-projection.ts"],
    kinds: ["CallExpression"],
  });
  const getUserBriefSyn = appCalls.find((c) => c.text === "getUserBrief()");
  const briefParseSyn = appCalls.find(
    (c) => c.text === "userSchema.parse(brief)",
  );
  assert.ok(getUserBriefSyn && briefParseSyn);

  const [briefParseResolved] = await appCtx.resolveCalls([briefParseSyn.ref], {
    select: { arguments: true },
  });
  assert.ok(briefParseResolved?.arguments?.[0]);

  const briefParseInputs = await drizzleZodValidator.extractInputFields(
    appCtx,
    {
      scope: "calls",
      targets: [briefParseSyn.ref],
    },
  );
  assert.equal(briefParseInputs.length, 1);
  const briefParseRes = briefParseInputs[0]!;
  assert.deepEqual(briefParseRes.requiredFields, [
    "fullName",
    "id",
    "status",
  ]);
  assert.deepEqual(briefParseRes.defaultedFields, ["age"]);
  assert.ok(
    extractBasisEvidence([briefParseRes]).some(
      (ev) => ev.at?.file === "packages/app/src/row-projection.ts",
    ),
  );
  assert.ok(
    briefParseRes.evidence.some(
      (ev) => ev.at?.file === "packages/db/src/drizzle-schema.ts",
    ),
  );

  const dbProjections = await extractDrizzleRowProjections(dbCtx);
  const briefProj = dbProjections.find(
    (p) =>
      p.queryKind === "select" &&
      p.projectedColumns.length === 3 &&
      p.projectedColumns.includes("status"),
  );
  assert.ok(briefProj);
  assert.ok(
    briefProj.evidence.some(
      (ev) => ev.at?.file === "packages/db/src/drizzle-projections.ts",
    ),
  );
  assert.ok(
    briefProj.evidence.some(
      (ev) => ev.at?.file === "packages/db/src/drizzle-schema.ts",
    ),
  );

  const propResult = await appCtx.valuePropagation({
    propagation: "value",
    includeMemberAccess: true,
    from: getUserBriefSyn.ref,
    to: [briefParseResolved.arguments[0]],
  });
  const propRow = propResult.results[0];
  assert.ok(propRow?.status === "proved");
  const propPath = propRow.paths[0];
  const retained = resolveRetainedObjectFields(
    { fields: briefProj.projectedColumns },
    propPath,
  );
  assert.ok(retained);
  assert.deepEqual(retained.fields, ["fullName", "id", "status"]);
  assert.ok(appInputCalls.length >= 4);
  assert.ok(observedQueries.length > 0);
  assert.ok(queryRpcMetrics.length > 0);

  process.stdout.write(
    JSON.stringify({
      nodeVersion: process.versions.node,
      refinedRequired: refinedIn.requiredFields,
      refinedDefaulted: refinedIn.defaultedFields,
      refinedOptional: refinedIn.optionalFields,
      retainedBriefFields: retained.fields,
    }) + "\\n",
  );
} finally {
  await session.close();
}
`;

void test("installs candidate tssae, @tssae/zod, and @tssae/drizzle tarballs independently or together in isolated hosts", async (t) => {
  const manifests = readDeclaredWorkspaceManifests();
  const publishableNames = manifests
    .filter((m) => m.private !== true)
    .map((m) => m.name)
    .toSorted();
  assert.deepEqual(publishableNames, ["@tssae/drizzle", "@tssae/zod", "tssae"]);

  const sharedStoreDir = mkdtempSync(join(tmpdir(), "tssae-official-store-"));
  mkdirSync(sharedStoreDir, { recursive: true });
  const bundle = await packWorkspaceArtifacts({
    additionalPackages: ["@tssae/zod", "@tssae/drizzle"],
  });

  try {
    const publishPlanPath = join(bundle.packDir, "publish-plan.json");
    if (existsSync(publishPlanPath)) {
      const planJson = JSON.parse(readFileSync(publishPlanPath, "utf8")) as {
        readonly plan: readonly (readonly {
          readonly kind: string;
          readonly name?: string;
        }[])[];
      };
      const plannedPublishNames = planJson.plan
        .flat()
        .filter((entry) => entry.kind === "publish" && entry.name !== undefined)
        .map((entry) => entry.name!)
        .toSorted();
      assert.deepEqual(plannedPublishNames, [
        "@tssae/drizzle",
        "@tssae/zod",
        "tssae",
      ]);
    }

    const tssaeArtifact = bundle.artifacts.find((a) => a.name === "tssae");
    const zodArtifact = bundle.artifacts.find((a) => a.name === "@tssae/zod");
    const drizzleArtifact = bundle.artifacts.find(
      (a) => a.name === "@tssae/drizzle",
    );
    assert.ok(tssaeArtifact && zodArtifact && drizzleArtifact);

    const expectedRepositoryDirectories = [
      [tssaeArtifact, "packages/tssae"],
      [zodArtifact, "packages/zod"],
      [drizzleArtifact, "packages/drizzle"],
    ] as const;
    for (const [artifact, directory] of expectedRepositoryDirectories) {
      assert.deepEqual(artifact.manifest.repository, {
        type: "git",
        url: "git+https://github.com/tssae/tssae.git",
        directory,
      });
    }

    assert.deepEqual(zodArtifact.manifest.dependencies, {
      tssae: "^0.2.0",
    });
    assert.deepEqual(drizzleArtifact.manifest.dependencies, {
      "@tssae/zod": "^0.1.0",
      tssae: "^0.2.0",
    });

    for (const artifact of [zodArtifact, drizzleArtifact]) {
      assert.equal(artifact.version, "0.1.0");
      assert.equal(artifact.manifest.private, undefined);
      assert.deepEqual(artifact.manifest.publishConfig, {
        access: "public",
      });
      assert.deepEqual(artifact.manifest.exports, {
        ".": {
          types: "./dist/index.d.ts",
          default: "./dist/index.js",
        },
        "./package.json": "./package.json",
      });
      const rootEntry = (artifact.manifest.exports as Record<string, unknown>)[
        "."
      ] as Record<string, unknown>;
      assert.equal("source" in rootEntry, false);
    }

    // 1. Standalone tssae + @tssae/zod
    await withAcceptanceHost(
      {
        hostFixture: "acceptance/domain-libraries-zod",
        bundle,
        storeDir: sharedStoreDir,
        directPackages: ["tssae", "@tssae/zod"],
        files: {
          "consumer.tsconfig.json": consumerTsconfig,
          "consumer.ts": singleZodConsumer,
        },
      },
      async (host) => {
        prepareMultiPackageHost(host.hostDir);

        for (const tsconfigPath of [
          "packages/db/tsconfig.json",
          "packages/app/tsconfig.json",
          "consumer.tsconfig.json",
        ]) {
          const tscRes = await host.runTsc(tsconfigPath);
          assert.equal(
            tscRes.status,
            0,
            `${tsconfigPath} typecheck failed:\n${tscRes.stdout}\n${tscRes.stderr}`,
          );
          assert.equal(tscRes.isolation?.controlProbeBlocked, true);
        }

        const runRes = await host.runNode(["consumer.ts"]);
        assert.equal(
          runRes.status,
          0,
          `Single-Zod acceptance script failed:\nstdout:\n${runRes.stdout}\nstderr:\n${runRes.stderr}`,
        );
        assert.equal(runRes.isolation?.controlProbeBlocked, true);
        const summary = JSON.parse(runRes.stdout.trim()) as {
          readonly nodeVersion: string;
          readonly providerId: string;
          readonly userDefaultedFields: readonly string[];
        };
        assert.equal(summary.nodeVersion, "24.0.0");
        assert.equal(summary.providerId, "zod");
        assert.deepEqual(summary.userDefaultedFields, ["age"]);

        t.diagnostic(
          `Single-Zod verified ${zodArtifact.name}@${zodArtifact.version} (tgz sha256=${zodArtifact.sha256.slice(0, 12)})`,
        );
      },
    );

    // 2. Standalone tssae + @tssae/drizzle
    await withAcceptanceHost(
      {
        hostFixture: "acceptance/domain-libraries-drizzle",
        bundle,
        storeDir: sharedStoreDir,
        directPackages: ["tssae", "@tssae/drizzle"],
        files: {
          "consumer.tsconfig.json": consumerTsconfig,
          "consumer.ts": singleDrizzleConsumer,
        },
      },
      async (host) => {
        prepareMultiPackageHost(host.hostDir);

        for (const tsconfigPath of [
          "packages/db/tsconfig.json",
          "packages/app/tsconfig.json",
          "consumer.tsconfig.json",
        ]) {
          const tscRes = await host.runTsc(tsconfigPath);
          assert.equal(
            tscRes.status,
            0,
            `${tsconfigPath} typecheck failed:\n${tscRes.stdout}\n${tscRes.stderr}`,
          );
          assert.equal(tscRes.isolation?.controlProbeBlocked, true);
        }

        const runRes = await host.runNode(["consumer.ts"]);
        assert.equal(
          runRes.status,
          0,
          `Single-Drizzle acceptance script failed:\nstdout:\n${runRes.stdout}\nstderr:\n${runRes.stderr}`,
        );
        assert.equal(runRes.isolation?.controlProbeBlocked, true);
        const summary = JSON.parse(runRes.stdout.trim()) as {
          readonly nodeVersion: string;
          readonly tableNames: readonly string[];
          readonly schemasCount: number;
        };
        assert.equal(summary.nodeVersion, "24.0.0");
        assert.equal(summary.schemasCount, 2);
        assert.equal(summary.tableNames.length, 4);

        t.diagnostic(
          `Single-Drizzle verified ${drizzleArtifact.name}@${drizzleArtifact.version} (tgz sha256=${drizzleArtifact.sha256.slice(0, 12)})`,
        );
      },
    );

    // 3. Combined tssae + @tssae/drizzle + @tssae/zod
    await withAcceptanceHost(
      {
        hostFixture: "acceptance/domain-libraries-drizzle-zod",
        bundle,
        storeDir: sharedStoreDir,
        directPackages: ["tssae", "@tssae/drizzle", "@tssae/zod"],
        files: {
          "consumer.tsconfig.json": consumerTsconfig,
          "consumer.ts": dualDomainLibsConsumer,
        },
      },
      async (host) => {
        prepareMultiPackageHost(host.hostDir);

        for (const tsconfigPath of [
          "packages/db/tsconfig.json",
          "packages/app/tsconfig.json",
          "consumer.tsconfig.json",
        ]) {
          const tscRes = await host.runTsc(tsconfigPath);
          assert.equal(
            tscRes.status,
            0,
            `${tsconfigPath} typecheck failed:\n${tscRes.stdout}\n${tscRes.stderr}`,
          );
          assert.equal(tscRes.isolation?.controlProbeBlocked, true);
        }

        const runRes = await host.runNode(["consumer.ts"]);
        assert.equal(
          runRes.status,
          0,
          `Dual Drizzle+Zod acceptance script failed:\nstdout:\n${runRes.stdout}\nstderr:\n${runRes.stderr}`,
        );
        assert.equal(runRes.isolation?.controlProbeBlocked, true);
        const summary = JSON.parse(runRes.stdout.trim()) as {
          readonly nodeVersion: string;
          readonly retainedBriefFields: readonly string[];
        };
        assert.equal(summary.nodeVersion, "24.0.0");
        assert.deepEqual(summary.retainedBriefFields, [
          "fullName",
          "id",
          "status",
        ]);

        t.diagnostic(
          `Dual Drizzle+Zod verified (${drizzleArtifact.name}@${drizzleArtifact.version}, ${zodArtifact.name}@${zodArtifact.version}, engine=${host.audit.engineBinarySha256.slice(0, 12)}, isolation=${runRes.isolation?.mode})`,
        );
      },
    );
  } finally {
    bundle.dispose();
    rmSync(sharedStoreDir, { recursive: true, force: true });
  }
});
