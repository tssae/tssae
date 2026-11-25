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

import {
  packWorkspaceArtifacts,
  verifyInstalledNativeLibrary,
  withAcceptanceHost,
} from "@tssae/acceptance";

const acceptanceRequire = createRequire(import.meta.url);

const WORKSPACE_PACKAGE_SPECIFIERS = [
  "tssae",
  "@tssae/adapters",
  "@tssae/adapter-api",
  "@tssae/adapter-fixture",
  "@tssae/adapter-fixture-complement",
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
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
  zodAdapter,
} from "@tssae/adapters/zod";
import {
  openProject,
  type EngineCallIoMetrics,
  type ExplainProviderEvaluation,
  type FactExplainEvent,
  type QueryCompletedEvent,
} from "tssae";

assert.equal(zodAdapter, "@tssae/adapters/zod");

const openRpcMetrics: EngineCallIoMetrics[] = [];
const session = await openProject({
  cwd: process.cwd(),
  projects: ["packages/db/tsconfig.json", "packages/app/tsconfig.json"],
  adapters: [zodAdapter],
  observe: "detail",
  onRpcMetrics: (metrics) => openRpcMetrics.push(metrics),
});

try {
  assert.deepEqual(
    session.nativeAdapters.map((entry) => ({
      packageName: entry.packageName,
      providerId: entry.providerId,
      status: entry.status,
    })),
    [
      {
        packageName: "@tssae/adapters/zod",
        providerId: "zod",
        status: "loaded",
      },
    ],
  );

  const observedQueries: QueryCompletedEvent[] = [];
  const observedEvaluations: ExplainProviderEvaluation[] = [];
  const observedFactExplains: FactExplainEvent[] = [];
  const queryRpcMetrics: EngineCallIoMetrics[] = [];

  const appCtx = session.project("packages/app/tsconfig.json", {
    observer: {
      observe: "detail",
      explainTarget: { file: "packages/app/src/zod-fields.ts", line: 21 },
      onQueryCompleted: (event) => observedQueries.push(event),
      onProviderEvaluation: (evaluation) =>
        observedEvaluations.push(evaluation),
      onFactExplain: (event) => observedFactExplains.push(event),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });

  const inputCallFacts = await appCtx.facts(ValidatorInputFieldsFact, {
    condition: { scope: "calls" },
    explain: true,
  });
  const outputCallFacts = await appCtx.facts(ValidatorOutputFieldsFact, {
    condition: { scope: "calls" },
    explain: true,
  });

  const accountIn = inputCallFacts.find(
    (fact) => fact.value.schemaName === "accountSchema",
  );
  const accountOut = outputCallFacts.find(
    (fact) => fact.value.schemaName === "accountSchema",
  );
  const userIn = inputCallFacts.find(
    (fact) => fact.value.schemaName === "userSchema",
  );
  const userOut = outputCallFacts.find(
    (fact) => fact.value.schemaName === "userSchema",
  );
  assert.ok(accountIn && accountOut && userIn && userOut);

  assert.equal(accountIn.value.validatorMethod, "safeParse");
  assert.deepEqual(accountIn.value.requiredFields, ["id", "nullableName"]);
  assert.deepEqual(accountIn.value.optionalFields, [
    "fallback",
    "nickname",
    "state",
  ]);
  assert.deepEqual(accountIn.value.defaultedFields, ["fallback", "state"]);
  assert.equal(accountIn.value.fields.nullableName?.nullable, true);
  assert.equal(accountIn.value.hasDynamicFields, true);
  assert.deepEqual(accountOut.value.presentFields, [
    "fallback",
    "id",
    "nullableName",
    "state",
  ]);
  assert.deepEqual(accountOut.value.optionalFields, ["nickname"]);
  assert.deepEqual(accountOut.value.defaultedFields, ["fallback", "state"]);
  assert.ok(
    accountIn.gaps.some((gap) => gap.kind === "dynamic-schema-spread"),
  );
  assert.ok(accountIn.gaps.some((gap) => gap.kind === "dynamic-schema-field"));

  assert.equal(userIn.value.validatorMethod, "parse");
  assert.deepEqual(userIn.value.requiredFields, []);
  assert.deepEqual(userIn.value.optionalFields, ["age"]);
  assert.deepEqual(userIn.value.defaultedFields, ["age"]);
  assert.equal(userIn.value.hasDynamicFields, true);
  assert.deepEqual(userOut.value.presentFields, ["age"]);
  assert.deepEqual(userOut.value.defaultedFields, ["age"]);
  assert.deepEqual(userIn.providers, ["zod"]);
  assert.deepEqual(userOut.providers, ["zod"]);
  assert.ok(
    userIn.evidence.some(
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
  assert.ok(
    userIn.explainTrace?.localGaps?.some(
      (lg) =>
        lg.status === "unresolved" &&
        lg.providerId === "zod" &&
        lg.gap.kind === "unresolved-schema" &&
        lg.gap.message.includes("createSelectSchema(userTable)") &&
        lg.gap.at?.file === "packages/app/src/zod-fields.ts" &&
        lg.gap.at.line === 21,
    ),
  );
  assert.ok(
    observedFactExplains.some((event) =>
      event.localGaps.some(
        (lg) =>
          lg.status === "unresolved" &&
          lg.gap.kind === "unresolved-schema" &&
          lg.gap.message.includes("createSelectSchema(userTable)"),
      ),
    ),
  );

  const directSchema = (
    await appCtx.findSyntax({
      files: ["packages/app/src/zod-fields.ts"],
      kinds: ["CallExpression"],
      textIncludes: ["standaloneOnly"],
    })
  )[0];
  assert.ok(directSchema);
  const directFacts = await appCtx.facts(ValidatorInputFieldsFact, {
    condition: { scope: "schemas" },
    targets: [directSchema.ref],
  });
  assert.equal(directFacts.length, 1);
  assert.deepEqual(directFacts[0]?.value.requiredFields, ["standaloneOnly"]);
  assert.deepEqual(directFacts[0]?.providers, ["zod"]);

  const queryProviders = new Set(
    observedQueries.flatMap((q) => q.providers ?? [q.provider]),
  );
  const evaluatedProviders = new Set(
    observedEvaluations.map((e) => e.providerId),
  );
  const rpcExtractionProviders = new Set(
    queryRpcMetrics.flatMap((m) =>
      Object.keys(m.engineMetrics?.providerExtractions ?? {}),
    ),
  );
  assert.ok(queryProviders.has("zod"));
  assert.equal(queryProviders.has("drizzle"), false);
  assert.deepEqual([...evaluatedProviders], ["zod"]);
  assert.deepEqual([...rpcExtractionProviders], ["zod"]);

  process.stdout.write(
    JSON.stringify({
      loadedAdapters: session.nativeAdapters.map((a) => a.providerId),
      queryProviders: [...queryProviders],
      evaluatedProviders: [...evaluatedProviders],
      rpcExtractionProviders: [...rpcExtractionProviders],
      userDefaultedFields: userIn.value.defaultedFields,
      userGapKinds: userIn.gaps.map((g) => g.kind),
    }) + "\\n",
  );
} finally {
  await session.close();
}
`;

const singleDrizzleConsumer = `import assert from "node:assert/strict";

import {
  DrizzleTableColumnsFact,
  QueryRowProjectionFact,
  ValidatorSchemaFieldsFact,
  drizzleAdapter,
} from "@tssae/adapters/drizzle";
import {
  openProject,
  type EngineCallIoMetrics,
  type ExplainProviderEvaluation,
  type QueryCompletedEvent,
} from "tssae";

assert.equal(drizzleAdapter, "@tssae/adapters/drizzle");

const session = await openProject({
  cwd: process.cwd(),
  projects: ["packages/db/tsconfig.json", "packages/app/tsconfig.json"],
  adapters: [drizzleAdapter],
  observe: "detail",
});

try {
  assert.deepEqual(
    session.nativeAdapters.map((entry) => ({
      packageName: entry.packageName,
      providerId: entry.providerId,
      status: entry.status,
    })),
    [
      {
        packageName: "@tssae/adapters/drizzle",
        providerId: "drizzle",
        status: "loaded",
      },
    ],
  );

  const observedQueries: QueryCompletedEvent[] = [];
  const observedEvaluations: ExplainProviderEvaluation[] = [];
  const queryRpcMetrics: EngineCallIoMetrics[] = [];

  const dbCtx = session.project("packages/db/tsconfig.json", {
    observer: {
      observe: "detail",
      explainTarget: { file: "packages/db/src/drizzle-schema.ts", line: 29 },
      onQueryCompleted: (event) => observedQueries.push(event),
      onProviderEvaluation: (evaluation) =>
        observedEvaluations.push(evaluation),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });
  const appCtx = session.project("packages/app/tsconfig.json", {
    observer: {
      observe: "detail",
      onQueryCompleted: (event) => observedQueries.push(event),
      onProviderEvaluation: (evaluation) =>
        observedEvaluations.push(evaluation),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });

  const tableFacts = await dbCtx.facts(DrizzleTableColumnsFact);
  assert.equal(tableFacts.length, 4);
  for (const fact of tableFacts) {
    assert.deepEqual(fact.providers, ["drizzle"]);
  }

  const accountsFact = tableFacts.find(
    (fact) => fact.value.tableName === "accounts",
  );
  assert.ok(accountsFact);
  assert.equal(accountsFact.value.sqlTableName, "accounts");
  assert.deepEqual(accountsFact.value.columnNames, [
    "createdAt",
    "displayName",
    "email",
    "id",
    "role",
  ]);
  assert.equal(accountsFact.value.columns.id?.primaryKey, true);
  assert.equal(accountsFact.value.columns.email?.nullable, false);
  assert.equal(accountsFact.value.columns.displayName?.nullable, true);
  assert.equal(accountsFact.value.columns.role?.hasDefault, true);
  assert.equal(accountsFact.value.columns.createdAt?.hasDefault, true);
  assert.ok(
    accountsFact.evidence.some(
      (ev) => ev.at?.file === "packages/db/src/drizzle-tables.ts",
    ),
  );

  const dynamicTableFact = tableFacts.find(
    (fact) => fact.value.tableName === "dynamicAuditEvents",
  );
  assert.ok(dynamicTableFact);
  assert.equal(dynamicTableFact.value.hasDynamicColumns, true);
  assert.deepEqual(dynamicTableFact.value.columnNames, ["id"]);
  assert.equal(dynamicTableFact.gaps.length, 2);
  assert.ok(
    dynamicTableFact.gaps.every(
      (gap) => gap.kind === "dynamic-table-columns",
    ),
  );

  const inspectCalls = await appCtx.findSyntax({
    files: ["packages/app/src/consumer.ts"],
    kinds: ["CallExpression"],
    calleeNames: ["inspectTable"],
  });
  const tableArg = inspectCalls[0]?.arguments?.[0];
  assert.ok(tableArg);
  const crossPackageTable = await appCtx.facts(DrizzleTableColumnsFact, {
    targets: [tableArg],
  });
  assert.equal(crossPackageTable.length, 1);
  assert.equal(crossPackageTable[0]?.value.tableName, "accounts");
  assert.deepEqual(crossPackageTable[0]?.providers, ["drizzle"]);
  assert.ok(
    crossPackageTable[0]?.evidence.some(
      (ev) => ev.at?.file === "packages/db/src/drizzle-tables.ts",
    ),
  );

  const projectionFacts = await dbCtx.facts(QueryRowProjectionFact, {
    condition: { library: "drizzle" },
  });
  assert.equal(projectionFacts.length, 3);
  for (const fact of projectionFacts) {
    assert.deepEqual(fact.providers, ["drizzle"]);
  }

  const explicitProj = projectionFacts.find((fact) =>
    fact.value.projectedColumns.includes("displayName"),
  );
  assert.ok(explicitProj);
  assert.deepEqual(explicitProj.value.projectedColumns, ["displayName", "id"]);
  assert.equal(explicitProj.value.columns.displayName?.sqlName, "full_name");

  const relationalProj = projectionFacts.find(
    (fact) => fact.value.queryKind === "relational",
  );
  assert.ok(relationalProj);
  assert.deepEqual(relationalProj.value.projectedColumns, [
    "fullName",
    "id",
    "workspace",
  ]);
  assert.deepEqual(relationalProj.value.relations.workspace?.columns, [
    "displayName",
    "id",
    "slug",
  ]);

  const dynamicProj = projectionFacts.find(
    (fact) => fact.value.hasDynamicColumns,
  );
  assert.ok(dynamicProj);
  assert.deepEqual(dynamicProj.value.projectedColumns, ["id"]);
  assert.ok(
    dynamicProj.gaps.some((gap) => gap.kind === "dynamic-query-projection"),
  );

  const schemaFacts = await dbCtx.facts(ValidatorSchemaFieldsFact, {
    explain: true,
  });
  assert.equal(schemaFacts.length, 2);
  for (const fact of schemaFacts) {
    assert.deepEqual(fact.providers, ["drizzle"]);
  }

  const strictSchemaFact = schemaFacts.find(
    (fact) => !fact.value.hasDynamicFields,
  );
  const refinedSchemaFact = schemaFacts.find(
    (fact) => fact.value.hasDynamicFields,
  );
  assert.ok(strictSchemaFact && refinedSchemaFact);
  assert.equal(strictSchemaFact.value.library, "drizzle-zod");
  assert.deepEqual(Object.keys(strictSchemaFact.value.fields).toSorted(), [
    "displayName",
    "id",
    "slug",
  ]);
  assert.equal(strictSchemaFact.value.fields.slug?.resolved, true);
  assert.ok(
    strictSchemaFact.evidence.some(
      (ev) => ev.at?.file === "packages/db/src/drizzle-schema.ts",
    ),
  );

  // Without Zod enabled, createSelectSchema refinement callback for \`slug\` remains unresolved.
  assert.equal(refinedSchemaFact.value.fields.id?.resolved, true);
  assert.equal(refinedSchemaFact.value.fields.displayName?.resolved, true);
  assert.equal(refinedSchemaFact.value.fields.slug?.resolved, false);
  assert.ok(
    refinedSchemaFact.value.fields.slug?.gaps?.some(
      (gap) => gap.kind === "unresolved-schema-field",
    ),
  );

  const queryProviders = new Set(
    observedQueries.flatMap((q) => q.providers ?? [q.provider]),
  );
  const evaluatedProviders = new Set(
    observedEvaluations.map((e) => e.providerId),
  );
  const rpcExtractionProviders = new Set(
    queryRpcMetrics.flatMap((m) =>
      Object.keys(m.engineMetrics?.providerExtractions ?? {}),
    ),
  );
  assert.ok(queryProviders.has("drizzle"));
  assert.equal(queryProviders.has("zod"), false);
  assert.deepEqual([...evaluatedProviders], ["drizzle"]);
  assert.deepEqual([...rpcExtractionProviders], ["drizzle"]);

  process.stdout.write(
    JSON.stringify({
      loadedAdapters: session.nativeAdapters.map((a) => a.providerId),
      queryProviders: [...queryProviders],
      evaluatedProviders: [...evaluatedProviders],
      rpcExtractionProviders: [...rpcExtractionProviders],
      tableNames: tableFacts.map((f) => f.value.tableName).toSorted(),
      schemaFactsCount: schemaFacts.length,
    }) + "\\n",
  );
} finally {
  await session.close();
}
`;

const dualAdaptersConsumer = `import assert from "node:assert/strict";

import {
  DrizzleTableColumnsFact,
  QueryRowProjectionFact,
  ValidatorSchemaFieldsFact,
  drizzleAdapter,
} from "@tssae/adapters/drizzle";
import {
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
  zodAdapter,
} from "@tssae/adapters/zod";
import {
  openProject,
  resolveRetainedObjectFields,
  type EngineCallIoMetrics,
  type ExplainProviderEvaluation,
  type QueryCompletedEvent,
} from "tssae";

assert.equal(drizzleAdapter, "@tssae/adapters/drizzle");
assert.equal(zodAdapter, "@tssae/adapters/zod");

const session = await openProject({
  cwd: process.cwd(),
  projects: ["packages/db/tsconfig.json", "packages/app/tsconfig.json"],
  adapters: [drizzleAdapter, zodAdapter],
  observe: "detail",
});

try {
  assert.deepEqual(
    session.nativeAdapters.map((entry) => ({
      packageName: entry.packageName,
      providerId: entry.providerId,
      status: entry.status,
    })),
    [
      {
        packageName: "@tssae/adapters/drizzle",
        providerId: "drizzle",
        status: "loaded",
      },
      {
        packageName: "@tssae/adapters/zod",
        providerId: "zod",
        status: "loaded",
      },
    ],
  );

  const observedQueries: QueryCompletedEvent[] = [];
  const observedEvaluations: ExplainProviderEvaluation[] = [];
  const queryRpcMetrics: EngineCallIoMetrics[] = [];

  const dbCtx = session.project("packages/db/tsconfig.json", {
    observer: {
      observe: "detail",
      explainTarget: { file: "packages/db/src/drizzle-schema.ts", line: 33 },
      onQueryCompleted: (event) => observedQueries.push(event),
      onProviderEvaluation: (evaluation) =>
        observedEvaluations.push(evaluation),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });
  const appCtx = session.project("packages/app/tsconfig.json", {
    observer: {
      observe: "detail",
      explainTarget: { file: "packages/app/src/row-projection.ts", line: 19 },
      onQueryCompleted: (event) => observedQueries.push(event),
      onProviderEvaluation: (evaluation) =>
        observedEvaluations.push(evaluation),
      onRpcMetrics: (metrics) => queryRpcMetrics.push(metrics),
    },
  });

  // 1. Complete known table columns from @acceptance-host/db
  const tableFacts = await dbCtx.facts(DrizzleTableColumnsFact);
  assert.equal(tableFacts.length, 3);
  assert.deepEqual(
    tableFacts.map((f) => f.value.tableName).toSorted(),
    ["page", "users", "workspace"],
  );
  const usersTable = tableFacts.find((f) => f.value.tableName === "users");
  assert.ok(usersTable);
  assert.deepEqual(usersTable.value.columnNames, [
    "age",
    "fullName",
    "id",
    "status",
  ]);
  assert.equal(usersTable.value.columns.age?.nullable, false);

  // 2. Only real drizzle-zod calls emit ValidatorSchemaFieldsFact (fake local createSelectSchema excluded)
  const schemaFieldsFacts = await dbCtx.facts(ValidatorSchemaFieldsFact);
  const appSchemaFieldsFacts = await appCtx.facts(ValidatorSchemaFieldsFact);
  assert.equal(schemaFieldsFacts.length, 4);
  assert.equal(appSchemaFieldsFacts.length, 4);

  // 3. Field override order: Drizzle base + drizzle-zod refinement + Zod .extend() / .pick()
  const dbInputFacts = await dbCtx.facts(ValidatorInputFieldsFact, {
    condition: { scope: "schemas" },
    explain: true,
  });
  const dbOutputFacts = await dbCtx.facts(ValidatorOutputFieldsFact, {
    condition: { scope: "schemas" },
    explain: true,
  });

  const userIn = dbInputFacts.find((f) => f.value.schemaName === "userSchema");
  const userOut = dbOutputFacts.find((f) => f.value.schemaName === "userSchema");
  assert.ok(userIn && userOut);
  assert.deepEqual(userIn.value.requiredFields, ["fullName", "id", "status"]);
  assert.deepEqual(userIn.value.defaultedFields, ["age"]);
  assert.deepEqual(userIn.value.optionalFields, ["age"]);
  assert.deepEqual(userOut.value.presentFields, [
    "age",
    "fullName",
    "id",
    "status",
  ]);
  assert.deepEqual(userOut.value.defaultedFields, ["age"]);
  // Evidence traces back to BOTH table column declaration (line 9) and .extend override (line 34)
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
  assert.ok(
    userIn.value.fields.age?.evidence?.some(
      (ev) =>
        ev.at?.file === "packages/db/src/drizzle-schema.ts" &&
        ev.at.line === 34,
    ),
  );

  const refinedIn = dbInputFacts.find(
    (f) => f.value.schemaName === "refinedWorkspaceSchema",
  );
  const refinedOut = dbOutputFacts.find(
    (f) => f.value.schemaName === "refinedWorkspaceSchema",
  );
  assert.ok(refinedIn && refinedOut);
  assert.deepEqual(refinedIn.value.requiredFields, ["id"]);
  assert.deepEqual(refinedIn.value.defaultedFields, ["name", "plan"]);
  assert.deepEqual(refinedIn.value.optionalFields, ["name", "plan", "slug"]);
  assert.equal(refinedIn.value.fields.name?.nullable, true);
  assert.equal(refinedIn.value.fields.slug?.nullable, false);
  assert.deepEqual(refinedOut.value.presentFields, ["id", "name", "plan"]);
  assert.deepEqual(refinedOut.value.optionalFields, ["slug"]);

  const gateIn = dbInputFacts.find(
    (f) => f.value.schemaName === "gateFieldsSchema",
  );
  assert.ok(gateIn);
  assert.deepEqual(gateIn.value.requiredFields, ["customDomain", "slug"]);
  assert.equal(gateIn.value.fields.customDomain?.nullable, true);
  assert.deepEqual(gateIn.value.optionalFields, ["password"]);
  assert.deepEqual(gateIn.value.defaultedFields, ["password"]);

  // 4. Cross-package query + validator call evidence and propagation in @acceptance-host/app
  const appInputCalls = await appCtx.facts(ValidatorInputFieldsFact, {
    condition: { scope: "calls" },
    explain: true,
  });
  const appInputSchemas = await appCtx.facts(ValidatorInputFieldsFact, {
    condition: { scope: "schemas" },
  });

  const fakeLocalIn = appInputSchemas.find(
    (f) => f.value.schemaName === "fakeLocalSelectSchema",
  );
  assert.ok(fakeLocalIn);
  assert.deepEqual(fakeLocalIn.value.requiredFields, []);
  assert.equal(fakeLocalIn.value.hasDynamicFields, true);
  assert.ok(
    fakeLocalIn.gaps.some((gap) => gap.kind === "unresolved-schema"),
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

  const briefParseFacts = await appCtx.facts(ValidatorInputFieldsFact, {
    condition: { scope: "calls" },
    targets: [briefParseSyn.ref],
  });
  assert.equal(briefParseFacts.length, 1);
  const briefParseFact = briefParseFacts[0]!;
  assert.deepEqual(briefParseFact.value.requiredFields, [
    "fullName",
    "id",
    "status",
  ]);
  assert.deepEqual(briefParseFact.value.defaultedFields, ["age"]);
  assert.ok(
    briefParseFact.evidence.some(
      (ev) => ev.at?.file === "packages/app/src/row-projection.ts",
    ),
  );
  assert.ok(
    briefParseFact.evidence.some(
      (ev) => ev.at?.file === "packages/db/src/drizzle-schema.ts",
    ),
  );

  const dbProjections = await dbCtx.facts(QueryRowProjectionFact, {
    condition: { library: "drizzle" },
  });
  const briefProj = dbProjections.find(
    (p) =>
      p.value.queryKind === "select" &&
      p.value.projectedColumns.length === 3 &&
      p.value.projectedColumns.includes("status"),
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
  const propPath = propResult.results[0]?.paths[0];
  assert.ok(propResult.results[0]?.reachable && propPath);
  const retained = resolveRetainedObjectFields(
    { fields: briefProj.value.projectedColumns },
    propPath,
  );
  assert.ok(retained);
  assert.deepEqual(retained.fields, ["fullName", "id", "status"]);
  assert.ok(appInputCalls.length >= 4);

  const queryProviders = new Set(
    observedQueries.flatMap((q) => q.providers ?? [q.provider]),
  );
  const evaluatedProviders = new Set(
    observedEvaluations.map((e) => e.providerId),
  );
  const rpcExtractionProviders = new Set(
    queryRpcMetrics.flatMap((m) =>
      Object.keys(m.engineMetrics?.providerExtractions ?? {}),
    ),
  );
  assert.ok(queryProviders.has("drizzle"));
  assert.ok(queryProviders.has("zod"));
  assert.ok(evaluatedProviders.has("drizzle"));
  assert.ok(evaluatedProviders.has("zod"));
  assert.ok(rpcExtractionProviders.has("drizzle"));
  assert.ok(rpcExtractionProviders.has("zod"));

  process.stdout.write(
    JSON.stringify({
      loadedAdapters: session.nativeAdapters.map((a) => a.providerId),
      queryProviders: [...queryProviders].toSorted(),
      evaluatedProviders: [...evaluatedProviders].toSorted(),
      rpcExtractionProviders: [...rpcExtractionProviders].toSorted(),
      refinedRequired: refinedIn.value.requiredFields,
      refinedDefaulted: refinedIn.value.defaultedFields,
      refinedOptional: refinedIn.value.optionalFields,
      retainedBriefFields: retained.fields,
    }) + "\\n",
  );
} finally {
  await session.close();
}
`;

void test("installs one candidate @tssae/adapters tarball alongside tssae, enables Zod and Drizzle independently or together, and keeps test fixtures private", async (t) => {
  const manifests = readDeclaredWorkspaceManifests();
  const fixtureManifest = manifests.find(
    (m) => m.name === "@tssae/adapter-fixture",
  );
  const complementManifest = manifests.find(
    (m) => m.name === "@tssae/adapter-fixture-complement",
  );
  assert.ok(fixtureManifest && complementManifest);
  assert.equal(fixtureManifest.private, true);
  assert.equal(complementManifest.private, true);
  const publishableNames = manifests
    .filter((m) => m.private !== true)
    .map((m) => m.name)
    .toSorted();
  assert.deepEqual(publishableNames, ["@tssae/adapters", "tssae"]);

  const sharedStoreDir = mkdtempSync(join(tmpdir(), "tssae-official-store-"));
  mkdirSync(sharedStoreDir, { recursive: true });
  const bundle = await packWorkspaceArtifacts({
    additionalPackages: ["@tssae/adapters"],
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
      assert.deepEqual(plannedPublishNames, ["@tssae/adapters", "tssae"]);
    }

    const tssaeArtifact = bundle.artifacts.find((a) => a.name === "tssae");
    const adaptersArtifact = bundle.artifacts.find(
      (a) => a.name === "@tssae/adapters",
    );
    assert.ok(tssaeArtifact && adaptersArtifact);
    assert.equal(adaptersArtifact.version, "0.1.0");
    assert.equal(adaptersArtifact.manifest.private, undefined);
    assert.deepEqual(adaptersArtifact.manifest.publishConfig, {
      access: "public",
    });
    assert.deepEqual(adaptersArtifact.manifest.exports, {
      "./zod": {
        types: "./dist/zod/index.d.ts",
        default: "./dist/zod/index.js",
      },
      "./zod/tssae-native": "./dist/zod/tssae-native.json",
      "./zod/native/linux-x64": "./dist/native/libtssae_adapter_zod.so",
      "./drizzle": {
        types: "./dist/drizzle/index.d.ts",
        default: "./dist/drizzle/index.js",
      },
      "./drizzle/tssae-native": "./dist/drizzle/tssae-native.json",
      "./drizzle/native/linux-x64": "./dist/native/libtssae_adapter_drizzle.so",
      "./package.json": "./package.json",
    });
    const packedExports = adaptersArtifact.manifest.exports as Record<
      string,
      unknown
    >;
    for (const key of ["./zod", "./drizzle"]) {
      const entry = packedExports[key] as Record<string, unknown> | undefined;
      assert.ok(entry);
      assert.equal("source" in entry, false);
    }

    // 1. Single-enable @tssae/adapters/zod
    await withAcceptanceHost(
      {
        hostFixture: "acceptance/official-adapters-zod",
        bundle,
        storeDir: sharedStoreDir,
        directPackages: ["tssae", "@tssae/adapters"],
        files: {
          "consumer.tsconfig.json": consumerTsconfig,
          "consumer.ts": singleZodConsumer,
        },
      },
      async (host) => {
        prepareMultiPackageHost(host.hostDir);
        const verifiedZod = verifyInstalledNativeLibrary({
          checkoutDir: host.hostDir,
          specifier: "@tssae/adapters/zod",
          artifact: adaptersArtifact,
          expectedProviderId: "zod",
        });

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
          readonly loadedAdapters: readonly string[];
          readonly evaluatedProviders: readonly string[];
          readonly rpcExtractionProviders: readonly string[];
        };
        assert.deepEqual(summary.loadedAdapters, ["zod"]);
        assert.deepEqual(summary.evaluatedProviders, ["zod"]);
        assert.deepEqual(summary.rpcExtractionProviders, ["zod"]);

        t.diagnostic(
          `Single-Zod verified ${verifiedZod.specifier} (${verifiedZod.sha256.slice(0, 12)})`,
        );
      },
    );

    // 2. Single-enable @tssae/adapters/drizzle
    await withAcceptanceHost(
      {
        hostFixture: "acceptance/official-adapters-drizzle",
        bundle,
        storeDir: sharedStoreDir,
        directPackages: ["tssae", "@tssae/adapters"],
        files: {
          "consumer.tsconfig.json": consumerTsconfig,
          "consumer.ts": singleDrizzleConsumer,
        },
      },
      async (host) => {
        prepareMultiPackageHost(host.hostDir);
        const verifiedDrizzle = verifyInstalledNativeLibrary({
          checkoutDir: host.hostDir,
          specifier: "@tssae/adapters/drizzle",
          artifact: adaptersArtifact,
          expectedProviderId: "drizzle",
        });

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
          readonly loadedAdapters: readonly string[];
          readonly evaluatedProviders: readonly string[];
          readonly rpcExtractionProviders: readonly string[];
        };
        assert.deepEqual(summary.loadedAdapters, ["drizzle"]);
        assert.deepEqual(summary.evaluatedProviders, ["drizzle"]);
        assert.deepEqual(summary.rpcExtractionProviders, ["drizzle"]);

        t.diagnostic(
          `Single-Drizzle verified ${verifiedDrizzle.specifier} (${verifiedDrizzle.sha256.slice(0, 12)})`,
        );
      },
    );

    // 3. Dual-enable @tssae/adapters/drizzle + @tssae/adapters/zod from the same tarball
    await withAcceptanceHost(
      {
        hostFixture: "acceptance/official-adapters-drizzle-zod",
        bundle,
        storeDir: sharedStoreDir,
        directPackages: ["tssae", "@tssae/adapters"],
        files: {
          "consumer.tsconfig.json": consumerTsconfig,
          "consumer.ts": dualAdaptersConsumer,
        },
      },
      async (host) => {
        prepareMultiPackageHost(host.hostDir);
        const verifiedDrizzle = verifyInstalledNativeLibrary({
          checkoutDir: host.hostDir,
          specifier: "@tssae/adapters/drizzle",
          artifact: adaptersArtifact,
          expectedProviderId: "drizzle",
        });
        const verifiedZod = verifyInstalledNativeLibrary({
          checkoutDir: host.hostDir,
          specifier: "@tssae/adapters/zod",
          artifact: adaptersArtifact,
          expectedProviderId: "zod",
        });

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
          `Dual-adapter acceptance script failed:\nstdout:\n${runRes.stdout}\nstderr:\n${runRes.stderr}`,
        );
        assert.equal(runRes.isolation?.controlProbeBlocked, true);
        const summary = JSON.parse(runRes.stdout.trim()) as {
          readonly loadedAdapters: readonly string[];
          readonly queryProviders: readonly string[];
          readonly evaluatedProviders: readonly string[];
          readonly rpcExtractionProviders: readonly string[];
          readonly retainedBriefFields: readonly string[];
        };
        assert.deepEqual(summary.loadedAdapters, ["drizzle", "zod"]);
        assert.ok(summary.queryProviders.includes("drizzle"));
        assert.ok(summary.queryProviders.includes("zod"));
        assert.ok(summary.evaluatedProviders.includes("drizzle"));
        assert.ok(summary.evaluatedProviders.includes("zod"));
        assert.ok(summary.rpcExtractionProviders.includes("drizzle"));
        assert.ok(summary.rpcExtractionProviders.includes("zod"));
        assert.deepEqual(summary.retainedBriefFields, [
          "fullName",
          "id",
          "status",
        ]);

        t.diagnostic(
          `Dual Drizzle+Zod verified from ${adaptersArtifact.name}@${adaptersArtifact.version} (tgz sha256=${adaptersArtifact.sha256.slice(0, 12)}, drizzle.so=${verifiedDrizzle.sha256.slice(0, 12)}, zod.so=${verifiedZod.sha256.slice(0, 12)}, engine=${host.audit.engineBinarySha256.slice(0, 12)}, isolation=${runRes.isolation?.mode})`,
        );
      },
    );
  } finally {
    bundle.dispose();
    rmSync(sharedStoreDir, { recursive: true, force: true });
  }
});
