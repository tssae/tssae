import assert from "node:assert/strict";
import { readFileSync, renameSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  drizzleQueryProjection,
  drizzleZodSchemaResolver,
  drizzleZodValidator,
  extractDrizzleRowProjections,
  extractDrizzleTableColumns,
  extractDrizzleZodSchemas,
  type DrizzleTableColumnsResult,
  type DrizzleZodSchemaResult,
  type QueryRowProjectionExtractor,
  type QueryRowProjectionResult,
} from "@tssae/drizzle";
import {
  installPackedWorkspacePackages,
  linkNodeModulesPackage,
  withTempHost,
  type PreparedTempHost,
} from "@tssae/test-hosts";
import { extractZodInputFields } from "@tssae/zod";
import {
  TssaeError,
  evidenceOf,
  extractBasisEvidence,
  openProject,
  type Gap,
} from "tssae";

const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const drizzleOrmRoot = join(packageRoot, "node_modules", "drizzle-orm");
const drizzleZodRoot = join(packageRoot, "node_modules", "drizzle-zod");
const zodRoot = join(packageRoot, "node_modules", "zod");

function withDrizzleHost(
  hostName: string | undefined,
  callback: (host: PreparedTempHost) => Promise<void>,
  options: {
    readonly linkZod?: boolean;
    readonly files?: Readonly<Record<string, string>>;
    readonly setup?: (host: PreparedTempHost) => Promise<void> | void;
  } = {},
): Promise<void> {
  return withTempHost(
    {
      ...(hostName !== undefined ? { host: hostName } : {}),
      ...(options.files !== undefined ? { files: options.files } : {}),
      packageLinks: {
        "drizzle-orm": drizzleOrmRoot,
        "drizzle-zod": drizzleZodRoot,
        ...(options.linkZod ? { zod: zodRoot } : {}),
      },
      ...(options.setup !== undefined ? { setup: options.setup } : {}),
    },
    callback,
  );
}

void describe("@tssae/drizzle", () => {
  void it("extracts column nullability, primary keys, defaults, SQL names, and select projections from drizzle-orm tables", async () => {
    await withDrizzleHost("adapters/drizzle-table-columns", async (host) => {
      await host.typecheck();

      let allTables: readonly DrizzleTableColumnsResult[] = [];
      let filteredTables: readonly DrizzleTableColumnsResult[] = [];
      let projections: readonly QueryRowProjectionResult[] = [];

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project();
        allTables = await extractDrizzleTableColumns(ctx);
        filteredTables = await extractDrizzleTableColumns(ctx, {
          tableName: "users",
        });
        const extractor: QueryRowProjectionExtractor = drizzleQueryProjection;
        projections = await extractor.extractRowProjections(ctx, {
          library: "drizzle",
        });
      } finally {
        await session.close();
      }

      assert.equal(allTables.length, 3);
      assert.equal(filteredTables.length, 1);

      const usersTable = filteredTables[0]!;
      assert.equal(usersTable.library, "drizzle");
      assert.equal(usersTable.tableName, "users");
      assert.equal(usersTable.sqlTableName, "users");
      assert.equal(usersTable.hasDynamicColumns, false);
      assert.deepEqual(usersTable.columnNames, [
        "createdAt",
        "displayName",
        "email",
        "externalId",
        "id",
        "role",
      ]);

      assert.deepEqual(
        {
          sqlName: usersTable.columns.id?.sqlName,
          nullable: usersTable.columns.id?.nullable,
          primaryKey: usersTable.columns.id?.primaryKey,
          hasDefault: usersTable.columns.id?.hasDefault,
        },
        { sqlName: "id", nullable: false, primaryKey: true, hasDefault: false },
      );
      assert.deepEqual(
        {
          sqlName: usersTable.columns.displayName?.sqlName,
          nullable: usersTable.columns.displayName?.nullable,
          primaryKey: usersTable.columns.displayName?.primaryKey,
          hasDefault: usersTable.columns.displayName?.hasDefault,
        },
        {
          sqlName: "display_name",
          nullable: true,
          primaryKey: false,
          hasDefault: false,
        },
      );
      assert.deepEqual(
        {
          sqlName: usersTable.columns.role?.sqlName,
          nullable: usersTable.columns.role?.nullable,
          primaryKey: usersTable.columns.role?.primaryKey,
          hasDefault: usersTable.columns.role?.hasDefault,
        },
        {
          sqlName: "role",
          nullable: false,
          primaryKey: false,
          hasDefault: true,
        },
      );
      assert.deepEqual(
        {
          sqlName: usersTable.columns.createdAt?.sqlName,
          nullable: usersTable.columns.createdAt?.nullable,
          primaryKey: usersTable.columns.createdAt?.primaryKey,
          hasDefault: usersTable.columns.createdAt?.hasDefault,
        },
        {
          sqlName: "created_at",
          nullable: false,
          primaryKey: false,
          hasDefault: true,
        },
      );
      assert.deepEqual(
        {
          sqlName: usersTable.columns.externalId?.sqlName,
          nullable: usersTable.columns.externalId?.nullable,
          primaryKey: usersTable.columns.externalId?.primaryKey,
          hasDefault: usersTable.columns.externalId?.hasDefault,
        },
        {
          sqlName: "external_id",
          nullable: true,
          primaryKey: false,
          hasDefault: true,
        },
      );

      assert.ok(
        usersTable.evidence.some(
          (item) => item.kind === "source" && item.at?.file === "src/schema.ts",
        ),
      );
      assert.ok(evidenceOf(usersTable).length > 0);
      assert.ok(extractBasisEvidence([usersTable]).length > 0);

      const sessionsTable = allTables.find((t) => t.tableName === "sessions")!;
      assert.equal(sessionsTable.columns.id?.primaryKey, true);
      assert.equal(sessionsTable.columns.id?.nullable, false);
      assert.equal(sessionsTable.columns.token?.nullable, false);

      const auditTable = allTables.find((t) => t.tableName === "auditLogs")!;
      assert.equal(auditTable.sqlTableName, "audit_logs");
      assert.equal(auditTable.columns.id?.primaryKey, true);
      assert.equal(auditTable.columns.note?.nullable, true);

      assert.equal(projections.length, 1);
      assert.deepEqual(projections[0]?.projectedColumns, ["email", "id"]);
    });
  });

  void it("preserves static columns, emits dynamic gaps, and extracts returning/getTableColumns/relational projections", async () => {
    await withDrizzleHost("adapters/drizzle-dynamic-tables", async (host) => {
      await host.typecheck();

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project();
        const tables = await extractDrizzleTableColumns(ctx);
        const projs = await extractDrizzleRowProjections(ctx);

        assert.equal(tables.length, 1);
        const eventsTable = tables[0]!;
        assert.equal(eventsTable.tableName, "events");
        assert.equal(eventsTable.hasDynamicColumns, true);
        assert.deepEqual(eventsTable.columnNames, ["createdAt", "id"]);
        assert.equal(eventsTable.columns.createdAt?.nullable, false);
        assert.equal(eventsTable.columns.createdAt?.hasDefault, true);
        assert.equal(eventsTable.gaps.length, 2);
        assert.ok(
          eventsTable.gaps.every((g) => g.kind === "dynamic-table-columns"),
        );

        assert.equal(projs.length, 1);
        assert.equal(projs[0]!.hasDynamicColumns, true);
        assert.deepEqual(projs[0]!.projectedColumns, ["id"]);
        assert.ok(
          projs[0]!.gaps.some((g) => g.kind === "dynamic-query-projection"),
        );
      } finally {
        await session.close();
      }
    });

    // Also verify full-table select(), getTableColumns(), and insert/update/delete(...).returning(...)
    await withDrizzleHost(
      undefined,
      async (host) => {
        await host.typecheck();
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const projs = await extractDrizzleRowProjections(session.project());
          assert.equal(projs.length, 4);
          const fullSelect = projs.find(
            (p) => p.queryKind === "select" && p.projectedColumns.length === 3,
          );
          const spreadCols = projs.find(
            (p) => p.queryKind === "select" && "extra" in p.columns,
          );
          const insertRet = projs.find(
            (p) =>
              p.queryKind === "returning" && p.projectedColumns.length === 3,
          );
          const updateRet = projs.find(
            (p) =>
              p.queryKind === "returning" && p.projectedColumns.length === 1,
          );
          assert.ok(fullSelect && spreadCols && insertRet && updateRet);
          assert.deepEqual(fullSelect.projectedColumns, [
            "email",
            "id",
            "role",
          ]);
          assert.deepEqual(spreadCols.projectedColumns, [
            "email",
            "extra",
            "id",
            "role",
          ]);
          assert.equal(spreadCols.columns.extra?.kind, "computed");
          assert.deepEqual(insertRet.projectedColumns, ["email", "id", "role"]);
          assert.deepEqual(updateRet.projectedColumns, ["id"]);
        } finally {
          await session.close();
        }
      },
      {
        files: {
          "tsconfig.json": JSON.stringify(
            {
              compilerOptions: {
                strict: true,
                skipLibCheck: true,
                target: "ES2024",
                module: "NodeNext",
                moduleResolution: "NodeNext",
                noEmit: true,
              },
              include: ["src/**/*.ts"],
            },
            null,
            2,
          ),
          "src/queries.ts": `import { getTableColumns, sql } from "drizzle-orm";
import { integer, sqliteTable, text, type BaseSQLiteDatabase } from "drizzle-orm/sqlite-core";

export const users = sqliteTable("users", {
  id: integer("id").primaryKey(),
  email: text("email").notNull(),
  role: text("role").default("member").notNull(),
});

declare const db: BaseSQLiteDatabase<"sync", unknown>;

export function runQueries() {
  const full = db.select().from(users).all();
  const spread = db
    .select({ ...getTableColumns(users), extra: sql<string>\`'ok'\` })
    .from(users)
    .all();
  const inserted = db
    .insert(users)
    .values({ email: "a@b.c" })
    .returning()
    .get();
  const updated = db
    .update(users)
    .set({ role: "admin" })
    .returning({ id: users.id })
    .get();
  return { full, spread, inserted, updated };
}
`,
        },
      },
    );
  });

  void it("resolves cross-package Drizzle table declarations for untargeted scans and targeted DeclarationRef/SyntaxRef queries", async () => {
    await withDrizzleHost(
      "adapters/drizzle-cross-package",
      async (host) => {
        await host.typecheck("packages/app/tsconfig.json");

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "packages/app/tsconfig.json",
        });
        try {
          const ctx = session.project();
          const untargetedTables = await extractDrizzleTableColumns(ctx);

          const calls = await ctx.findSyntax({
            files: ["packages/app/src/consumer.ts"],
            kinds: ["CallExpression"],
          });
          const registerTableNode = calls.find((node) =>
            node.text.startsWith("registerTable("),
          );
          const registerOtherNode = calls.find((node) =>
            node.text.startsWith("registerOther("),
          );
          const tableArgRef = registerTableNode?.arguments?.[0];
          const otherArgRef = registerOtherNode?.arguments?.[0];
          assert.ok(tableArgRef && otherArgRef);

          const declRows = await ctx.resolveDeclarations([tableArgRef]);
          const tableDeclRef = declRows[0]?.declaration;
          assert.ok(tableDeclRef);

          const bySyntaxRef = await extractDrizzleTableColumns(ctx, {
            targets: [tableArgRef],
          });
          const byDeclRef = await extractDrizzleTableColumns(ctx, {
            targets: [tableDeclRef],
          });
          const byUnrelatedRef = await extractDrizzleTableColumns(ctx, {
            targets: [otherArgRef],
          });

          assert.equal(untargetedTables.length, 1);
          assert.equal(untargetedTables[0]?.tableName, "accounts");

          assert.equal(bySyntaxRef.length, 1);
          assert.equal(bySyntaxRef[0]?.target.handleId, tableArgRef.handleId);
          assert.equal(bySyntaxRef[0]?.tableName, "accounts");
          assert.equal(bySyntaxRef[0]?.sqlTableName, "accounts");
          assert.deepEqual(bySyntaxRef[0]?.columnNames, [
            "id",
            "nickname",
            "ownerEmail",
          ]);
          assert.equal(bySyntaxRef[0]?.columns.ownerEmail?.nullable, false);
          assert.equal(bySyntaxRef[0]?.columns.nickname?.nullable, true);
          assert.equal(bySyntaxRef[0]?.columns.nickname?.hasDefault, true);
          assert.ok(
            bySyntaxRef[0]?.evidence.some(
              (item) => item.at?.file === "packages/data/src/schema.ts",
            ),
          );

          assert.equal(byDeclRef.length, 1);
          assert.equal(byDeclRef[0]?.target.handleId, tableDeclRef.handleId);
          assert.equal(byDeclRef[0]?.tableName, "accounts");
          assert.equal(byUnrelatedRef.length, 0);
        } finally {
          await session.close();
        }
      },
      {
        setup: (host) => {
          renameSync(
            host.resolvePath("packages/pkg-data"),
            host.resolvePath("packages/data"),
          );
          renameSync(
            host.resolvePath("packages/app/tsconfig.runtime.json"),
            host.resolvePath("packages/app/tsconfig.json"),
          );
          host.removePath("tsconfig.json");
          linkNodeModulesPackage(
            host.rootDir,
            "@acme/data",
            host.resolvePath("packages/data"),
          );
        },
      },
    );
  });

  void it("recognizes createSelectSchema and createSchemaFactory via real drizzle-zod provenance and rejects fake functions", async () => {
    await withDrizzleHost(
      "adapters/drizzle-zod-provenance",
      async (host) => {
        await host.typecheck();

        let schemas: readonly DrizzleZodSchemaResult[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          schemas = await extractDrizzleZodSchemas(session.project());
        } finally {
          await session.close();
        }

        // Exactly 7 real drizzle-zod calls; fakeSelect and fakeFactory.createSelectSchema are rejected
        assert.equal(schemas.length, 7, JSON.stringify(schemas));
        for (const schema of schemas) {
          assert.equal(schema.library, "drizzle-zod");
          assert.equal(schema.hasDynamicFields, false);
          assert.deepEqual(Object.keys(schema.fields).toSorted(), [
            "bio",
            "email",
            "id",
          ]);
          assert.equal(schema.fields.id?.resolved, true);
          assert.equal(schema.fields.id?.nullable, false);
          assert.equal(schema.fields.email?.nullable, false);
          assert.equal(schema.fields.bio?.nullable, true);
          assert.ok(
            schema.evidence.some((ev) => ev.at?.file === "src/schemas.ts"),
          );
        }
      },
      {
        linkZod: true,
        setup: (host) => {
          renameSync(
            host.resolvePath("tsconfig.runtime.json"),
            host.resolvePath("tsconfig.json"),
          );
          linkNodeModulesPackage(
            host.rootDir,
            "@acme/drizzle-helpers",
            host.resolvePath("packages/reexport"),
          );
        },
      },
    );
  });

  void it("resolves refinements when enabled, supports refineFields:false, and emits unresolved-drizzle-table gaps", async () => {
    await withDrizzleHost(
      "adapters/drizzle-zod-gaps",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        let unresolvedMode: readonly DrizzleZodSchemaResult[];
        let resolvedMode: readonly DrizzleZodSchemaResult[];
        try {
          const ctx = session.project();
          unresolvedMode = await extractDrizzleZodSchemas(ctx, {
            refineFields: false,
          });
          resolvedMode = await extractDrizzleZodSchemas(ctx, {
            refineFields: true,
          });
        } finally {
          await session.close();
        }

        assert.equal(unresolvedMode.length, 2);
        const unref = unresolvedMode.find((s) => "id" in s.fields)!;
        const unresTable = unresolvedMode.find((s) => !("id" in s.fields))!;
        assert.equal(unref.hasDynamicFields, true);
        assert.equal(unref.fields.id?.resolved, true);
        assert.equal(unref.fields.name?.resolved, true);
        assert.equal(unref.fields.nickname?.resolved, true);
        assert.equal(unref.fields.nickname?.nullable, true);
        assert.equal(unref.fields.age?.resolved, false);
        assert.ok(
          unref.fields.age?.gaps?.some(
            (g) => g.kind === "unresolved-schema-field",
          ),
        );
        assert.ok(unref.gaps.some((g) => g.kind === "dynamic-schema-spread"));
        assert.ok(unref.gaps.some((g) => g.kind === "dynamic-schema-field"));
        assert.equal(unresTable.hasDynamicFields, true);
        assert.ok(
          unresTable.gaps.some((g) => g.kind === "unresolved-drizzle-table"),
        );

        const resRef = resolvedMode.find((s) => "id" in s.fields)!;
        assert.equal(resRef.fields.age?.resolved, true);
        assert.equal(resRef.fields.age?.requirement, "defaulted");
        assert.equal(resRef.fields.age?.nullable, false);
        assert.equal(resRef.hasDynamicFields, true);
      },
      { linkZod: true },
    );
  });

  void it("composes drizzle-zod tables, callback refinements, .extend()/.pick(), relational projections, caching, and session.update()", async () => {
    await withDrizzleHost(
      "acceptance/official-adapters-drizzle-zod",
      async (host) => {
        let rpcCount = 0;
        const session = await openProject({
          cwd: host.rootDir,
          projects: ["packages/db/tsconfig.json", "packages/app/tsconfig.json"],
        });
        try {
          const dbCtx = session.project("packages/db/tsconfig.json", {
            observer: {
              onRpcMetrics: () => {
                rpcCount += 1;
              },
            },
          });
          const appCtx = session.project("packages/app/tsconfig.json");

          const dbInputs = await drizzleZodValidator.extractInputFields(dbCtx, {
            scope: "schemas",
          });
          const dbOutputs = await drizzleZodValidator.extractOutputFields(
            dbCtx,
            { scope: "schemas" },
          );
          const rpcsAfterFirst = rpcCount;
          assert.ok(rpcsAfterFirst > 0);
          // Cached within same ProjectSummary -> zero additional RPCs!
          const dbInputsCached = await extractZodInputFields(dbCtx, {
            scope: "schemas",
            externalSchemaResolvers: [drizzleZodSchemaResolver],
          });
          assert.equal(rpcCount, rpcsAfterFirst);
          assert.equal(dbInputsCached.length, dbInputs.length);

          const userIn = dbInputs.find((r) => r.schemaName === "userSchema")!;
          const userOut = dbOutputs.find((r) => r.schemaName === "userSchema")!;
          assert.deepEqual(userIn.requiredFields, ["fullName", "id", "status"]);
          assert.deepEqual(userIn.defaultedFields, ["age"]);
          assert.deepEqual(userOut.presentFields, [
            "age",
            "fullName",
            "id",
            "status",
          ]);
          assert.ok(
            userIn.evidence.some(
              (ev) =>
                ev.at?.file === "packages/db/src/drizzle-schema.ts" &&
                ev.at.line === 9,
            ),
          );

          const refinedIn = dbInputs.find(
            (r) => r.schemaName === "refinedWorkspaceSchema",
          )!;
          const refinedOut = dbOutputs.find(
            (r) => r.schemaName === "refinedWorkspaceSchema",
          )!;
          assert.deepEqual(refinedIn.requiredFields, ["id"]);
          assert.deepEqual(refinedIn.defaultedFields, ["name", "plan"]);
          assert.deepEqual(refinedIn.optionalFields, ["name", "plan", "slug"]);
          assert.equal(refinedIn.fields.name?.nullable, true);
          assert.equal(refinedIn.fields.slug?.nullable, false);
          assert.deepEqual(refinedOut.presentFields, ["id", "name", "plan"]);
          assert.deepEqual(refinedOut.optionalFields, ["slug"]);

          const gateIn = dbInputs.find(
            (r) => r.schemaName === "gateFieldsSchema",
          )!;
          assert.deepEqual(gateIn.requiredFields, ["customDomain", "slug"]);
          assert.equal(gateIn.fields.customDomain?.nullable, true);
          assert.deepEqual(gateIn.defaultedFields, ["password"]);

          // Cross-package .parse() call in appCtx resolves userSchema from @acceptance-host/db!
          const appCalls = await drizzleZodValidator.extractInputFields(
            appCtx,
            { scope: "calls" },
          );
          const userParse = appCalls.find(
            (r) =>
              r.schemaName === "userSchema" &&
              r.at.file === "packages/app/src/row-projection.ts",
          )!;
          assert.deepEqual(userParse.requiredFields, [
            "fullName",
            "id",
            "status",
          ]);
          assert.deepEqual(userParse.defaultedFields, ["age"]);

          // Fake local createSelectSchema emits unresolved-schema
          const appSchemas = await drizzleZodValidator.extractInputFields(
            appCtx,
            { scope: "schemas" },
          );
          const fakeLocal = appSchemas.find(
            (r) => r.schemaName === "fakeLocalSelectSchema",
          )!;
          assert.deepEqual(fakeLocal.requiredFields, []);
          assert.ok(fakeLocal.gaps.some((g) => g.kind === "unresolved-schema"));

          // Relational projection in dbCtx
          const dbProjs = await extractDrizzleRowProjections(dbCtx);
          const pageWithWs = dbProjs.find((p) => p.queryKind === "relational")!;
          assert.deepEqual(pageWithWs.projectedColumns, [
            "customDomain",
            "slug",
            "workspace",
          ]);
          assert.equal(pageWithWs.relations.workspace?.cardinality, "single");
          assert.deepEqual(pageWithWs.relations.workspace?.columns, [
            "id",
            "name",
            "plan",
            "slug",
          ]);

          // session.update() invalidates Drizzle caches and rejects stale ProjectContext
          host.replaceText(
            "packages/db/src/drizzle-schema.ts",
            'name: text("name"),',
            'name: text("name").notNull(),',
          );
          await session.update({
            changed: ["packages/db/src/drizzle-schema.ts"],
          });
          assert.throws(
            () => dbCtx.recordGaps([]),
            (err: unknown) =>
              err instanceof TssaeError && err.code === "STALE_SNAPSHOT",
          );
          const updatedTables = await extractDrizzleTableColumns(
            session.project("packages/db/tsconfig.json"),
            { tableName: "workspace" },
          );
          assert.equal(updatedTables[0]?.columns.name?.nullable, false);
        } finally {
          await session.close();
        }
      },
      {
        linkZod: true,
        setup: (host) => {
          renameSync(
            host.resolvePath("packages/app/tsconfig.runtime.json"),
            host.resolvePath("packages/app/tsconfig.json"),
          );
          host.removePath("tsconfig.json");
          linkNodeModulesPackage(
            host.rootDir,
            "@acceptance-host/db",
            host.resolvePath("packages/db"),
          );
        },
      },
    );
  });

  void it("compiles an external TypeScript consumer against built tssae, @tssae/zod, and @tssae/drizzle .d.ts", async () => {
    const externalConsumerSource = readFileSync(
      join(packageRoot, "test", "fixtures", "external-consumer.ts"),
      "utf8",
    );

    await withTempHost(
      {
        files: {
          "tsconfig.json": JSON.stringify(
            {
              compilerOptions: {
                strict: true,
                exactOptionalPropertyTypes: true,
                noUncheckedIndexedAccess: true,
                target: "ES2022",
                module: "NodeNext",
                moduleResolution: "NodeNext",
                types: [],
                noEmit: true,
              },
              include: ["src/**/*.ts"],
            },
            null,
            2,
          ),
          "src/consumer.ts": externalConsumerSource,
        },
        setup: async (host) => {
          await installPackedWorkspacePackages(
            host.rootDir,
            ["tssae", "@tssae/zod", "@tssae/drizzle"],
            { cwd: packageRoot },
          );
        },
      },
      async (host) => {
        await host.typecheck();
      },
    );
  });

  void it("discovers cross-package workspace row projections and relations when opened on a consuming API package", async () => {
    await withDrizzleHost(
      undefined,
      async (host) => {
        await host.typecheck("packages/api/tsconfig.json");

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "packages/api/tsconfig.json",
        });
        try {
          const projections = await extractDrizzleRowProjections(
            session.project(),
          );
          const selectProj = projections.find((p) => p.queryKind === "select");
          const relProj = projections.find((p) => p.queryKind === "relational");

          assert.ok(selectProj, "expected cross-package select projection");
          assert.deepEqual(selectProj.projectedColumns, ["email", "id"]);
          assert.equal(selectProj.tableName, "users");
          assert.equal(selectProj.at.file, "packages/db/src/queries.ts");

          assert.ok(relProj, "expected cross-package relational projection");
          assert.equal(relProj.tableName, "users");
          assert.deepEqual(relProj.projectedColumns, ["id", "posts"]);
          assert.equal(relProj.relations.posts?.cardinality, "many");
          assert.equal(relProj.relations.posts?.targetTable, "posts");
          assert.deepEqual(relProj.relations.posts?.columns, [
            "authorId",
            "id",
            "title",
          ]);
        } finally {
          await session.close();
        }
      },
      {
        files: {
          "packages/db/package.json": JSON.stringify(
            {
              name: "@acme/db",
              version: "1.0.0",
              type: "module",
              exports: {
                "./queries": {
                  source: "./src/queries.ts",
                  types: "./src/queries.ts",
                  default: "./src/queries.ts",
                },
                "./schema": {
                  source: "./src/schema.ts",
                  types: "./src/schema.ts",
                  default: "./src/schema.ts",
                },
              },
            },
            null,
            2,
          ),
          "packages/db/tsconfig.json": JSON.stringify(
            {
              compilerOptions: {
                strict: true,
                skipLibCheck: true,
                target: "ES2024",
                module: "NodeNext",
                moduleResolution: "NodeNext",
                noEmit: true,
              },
              include: ["src/**/*.ts"],
            },
            null,
            2,
          ),
          "packages/db/src/schema.ts": `import { relations } from "drizzle-orm";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";

export const users = sqliteTable("users", {
  id: integer("id").primaryKey(),
  email: text("email").notNull(),
});

export const posts = sqliteTable("posts", {
  id: integer("id").primaryKey(),
  authorId: integer("author_id").notNull(),
  title: text("title").notNull(),
});

export const usersRelations = relations(users, ({ many }) => ({
  posts: many(posts),
}));

export const postsRelations = relations(posts, ({ one }) => ({
  author: one(users, {
    fields: [posts.authorId],
    references: [users.id],
  }),
}));
`,
          "packages/db/src/queries.ts": `import type { BaseSQLiteDatabase } from "drizzle-orm/sqlite-core";

import { users, type postsRelations, type usersRelations } from "./schema.js";

type Schema = {
  users: typeof users;
  usersRelations: typeof usersRelations;
  postsRelations: typeof postsRelations;
};

export function selectUserEmails(
  db: BaseSQLiteDatabase<"sync", unknown, Schema>,
) {
  return db.select({ id: users.id, email: users.email }).from(users).all();
}

export function listUsersWithPosts(
  db: BaseSQLiteDatabase<"sync", unknown, Schema>,
) {
  return db.query.users.findMany({
    columns: { id: true },
    with: { posts: true },
  });
}
`,
          "packages/api/tsconfig.json": JSON.stringify(
            {
              compilerOptions: {
                strict: true,
                skipLibCheck: true,
                target: "ES2024",
                module: "NodeNext",
                moduleResolution: "NodeNext",
                customConditions: ["source"],
                noEmit: true,
              },
              include: ["src/**/*.ts"],
            },
            null,
            2,
          ),
          "packages/api/src/handler.ts": `import {
  listUsersWithPosts,
  selectUserEmails,
} from "@acme/db/queries";

export function handle(
  db: Parameters<typeof listUsersWithPosts>[0],
) {
  return {
    emails: selectUserEmails(db),
    withPosts: listUsersWithPosts(db),
  };
}
`,
        },
        setup: (host) => {
          linkNodeModulesPackage(
            host.rootDir,
            "@acme/db",
            host.resolvePath("packages/db"),
          );
        },
      },
    );
  });

  void it("suppresses superseded unresolved-schema-field gaps when .extend() overrides a refined drizzle-zod field", async () => {
    const baseTsConfig = JSON.stringify(
      {
        compilerOptions: {
          strict: true,
          skipLibCheck: true,
          target: "ES2024",
          module: "NodeNext",
          moduleResolution: "NodeNext",
          noEmit: true,
        },
        include: ["src/**/*.ts"],
      },
      null,
      2,
    );

    await withDrizzleHost(
      undefined,
      async (host) => {
        await host.typecheck();

        let recordedGaps: readonly Gap[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project(undefined, {
            observer: {
              onGaps: (gaps) => {
                recordedGaps = gaps;
              },
            },
          });

          const calls = await drizzleZodValidator.extractInputFields(ctx, {
            scope: "calls",
          });
          assert.equal(calls.length, 1);
          const parseRes = calls[0]!;
          assert.equal(parseRes.hasDynamicFields, false);
          assert.deepEqual(parseRes.requiredFields, ["id"]);
          assert.deepEqual(parseRes.defaultedFields, ["age"]);
          assert.equal(
            parseRes.gaps.filter((g) => g.kind === "unresolved-schema-field")
              .length,
            0,
          );
          assert.equal(recordedGaps.length, 0);
        } finally {
          await session.close();
        }
      },
      {
        linkZod: true,
        files: {
          "tsconfig.json": baseTsConfig,
          "src/index.ts": `import { integer, sqliteTable } from "drizzle-orm/sqlite-core";
import { createSelectSchema } from "drizzle-zod";
import { z } from "zod";

const users = sqliteTable("users", {
  id: integer("id").primaryKey(),
  age: integer("age").notNull(),
});

declare function runtimeAgeValidator(): z.ZodType<number>;

const overriddenSchema = createSelectSchema(users, {
  age: runtimeAgeValidator(),
}).extend({
  age: z.number().default(18),
});

export function parseOverridden(row: unknown) {
  return overriddenSchema.parse(row);
}
`,
        },
      },
    );

    await withDrizzleHost(
      undefined,
      async (host) => {
        await host.typecheck();

        let recordedGaps: readonly Gap[] = [];
        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project(undefined, {
            observer: {
              onGaps: (gaps) => {
                recordedGaps = gaps;
              },
            },
          });

          const calls = await drizzleZodValidator.extractInputFields(ctx, {
            scope: "calls",
          });
          assert.equal(calls.length, 1);
          const parseRes = calls[0]!;
          assert.equal(
            parseRes.gaps.filter((g) => g.kind === "unresolved-schema-field")
              .length,
            1,
          );
          assert.equal(
            recordedGaps.filter((g) => g.kind === "unresolved-schema-field")
              .length,
            1,
          );
        } finally {
          await session.close();
        }
      },
      {
        linkZod: true,
        files: {
          "tsconfig.json": baseTsConfig,
          "src/index.ts": `import { integer, sqliteTable } from "drizzle-orm/sqlite-core";
import { createSelectSchema } from "drizzle-zod";
import type { z } from "zod";

const users = sqliteTable("users", {
  id: integer("id").primaryKey(),
  age: integer("age").notNull(),
});

declare function runtimeAgeValidator(): z.ZodType<number>;

const rawRefinedSchema = createSelectSchema(users, {
  age: runtimeAgeValidator(),
});

export function parseRaw(row: unknown) {
  return rawRefinedSchema.parse(row);
}
`,
        },
      },
    );
  });
});
