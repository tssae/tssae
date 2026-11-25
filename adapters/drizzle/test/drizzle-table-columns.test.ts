import assert from "node:assert/strict";
import { renameSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  DrizzleTableColumnsFact,
  QueryRowProjectionFact,
  buildDrizzleTableColumnsValue,
  type DrizzleTableColumnsFactValue,
  type QueryRowProjectionFactValue,
} from "@verule/adapter-drizzle";
import {
  linkNodeModulesPackage,
  withTempHost,
  type PreparedTempHost,
} from "@verule/test-hosts";
import {
  TssaeError,
  defineAdapter,
  defineFactProvider,
  openProject,
  type FactResult,
} from "tssae";

const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const drizzleOrmRoot = join(packageRoot, "node_modules", "drizzle-orm");

function withDrizzleHost(
  hostName: string,
  callback: (host: PreparedTempHost) => Promise<void>,
  setup?: (host: PreparedTempHost) => Promise<void> | void,
): Promise<void> {
  return withTempHost(
    {
      host: hostName,
      packageLinks: {
        "@verule/adapter-drizzle": packageRoot,
        "drizzle-orm": drizzleOrmRoot,
      },
      ...(setup !== undefined ? { setup } : {}),
    },
    callback,
  );
}

void describe("@verule/adapter-drizzle table columns facts", () => {
  void it("extracts column nullability, primary keys, defaults, and SQL names from real drizzle-orm tables alongside QueryRowProjectionFact", async () => {
    await withDrizzleHost("adapters/drizzle-table-columns", async (host) => {
      await host.typecheck();

      let allTables: readonly FactResult<
        "syntax" | "declaration" | "call",
        DrizzleTableColumnsFactValue
      >[] = [];
      let filteredTables: readonly FactResult<
        "syntax" | "declaration" | "call",
        DrizzleTableColumnsFactValue
      >[] = [];
      let projections: readonly FactResult<
        "syntax" | "call" | "declaration",
        QueryRowProjectionFactValue
      >[] = [];

      const session = await openProject({
        cwd: host.rootDir,
        adapterResolutionRoot: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-drizzle"],
      });
      try {
        const ctx = session.project();
        allTables = await ctx.facts(DrizzleTableColumnsFact);
        filteredTables = await ctx.facts(DrizzleTableColumnsFact, {
          condition: { tableName: "users" },
        });
        projections = await ctx.facts(QueryRowProjectionFact, {
          condition: { library: "drizzle" },
        });
      } finally {
        await session.close();
      }

      assert.equal(allTables.length, 3);
      assert.equal(filteredTables.length, 1);

      const usersFact = filteredTables[0];
      assert.ok(usersFact);
      assert.equal(usersFact.value.library, "drizzle");
      assert.equal(usersFact.value.tableName, "users");
      assert.equal(usersFact.value.sqlTableName, "users");
      assert.equal(usersFact.value.hasDynamicColumns, false);
      assert.deepEqual(usersFact.value.columnNames, [
        "createdAt",
        "displayName",
        "email",
        "externalId",
        "id",
        "role",
      ]);

      assert.deepEqual(
        {
          sqlName: usersFact.value.columns.id?.sqlName,
          nullable: usersFact.value.columns.id?.nullable,
          primaryKey: usersFact.value.columns.id?.primaryKey,
          hasDefault: usersFact.value.columns.id?.hasDefault,
        },
        { sqlName: "id", nullable: false, primaryKey: true, hasDefault: false },
      );
      assert.deepEqual(
        {
          sqlName: usersFact.value.columns.email?.sqlName,
          nullable: usersFact.value.columns.email?.nullable,
          primaryKey: usersFact.value.columns.email?.primaryKey,
          hasDefault: usersFact.value.columns.email?.hasDefault,
        },
        {
          sqlName: "email",
          nullable: false,
          primaryKey: false,
          hasDefault: false,
        },
      );
      assert.deepEqual(
        {
          sqlName: usersFact.value.columns.displayName?.sqlName,
          nullable: usersFact.value.columns.displayName?.nullable,
          primaryKey: usersFact.value.columns.displayName?.primaryKey,
          hasDefault: usersFact.value.columns.displayName?.hasDefault,
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
          sqlName: usersFact.value.columns.role?.sqlName,
          nullable: usersFact.value.columns.role?.nullable,
          primaryKey: usersFact.value.columns.role?.primaryKey,
          hasDefault: usersFact.value.columns.role?.hasDefault,
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
          sqlName: usersFact.value.columns.createdAt?.sqlName,
          nullable: usersFact.value.columns.createdAt?.nullable,
          primaryKey: usersFact.value.columns.createdAt?.primaryKey,
          hasDefault: usersFact.value.columns.createdAt?.hasDefault,
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
          sqlName: usersFact.value.columns.externalId?.sqlName,
          nullable: usersFact.value.columns.externalId?.nullable,
          primaryKey: usersFact.value.columns.externalId?.primaryKey,
          hasDefault: usersFact.value.columns.externalId?.hasDefault,
        },
        {
          sqlName: "external_id",
          nullable: true,
          primaryKey: false,
          hasDefault: true,
        },
      );

      assert.ok(
        usersFact.evidence.some(
          (item) => item.kind === "source" && item.at?.file === "src/schema.ts",
        ),
      );
      assert.ok(
        usersFact.value.columns.email?.evidence?.some(
          (item) => item.kind === "source" && item.at?.file === "src/schema.ts",
        ),
      );

      const sessionsFact = allTables.find(
        (fact) => fact.value.tableName === "sessions",
      );
      assert.ok(sessionsFact);
      assert.equal(sessionsFact.value.columns.id?.primaryKey, true);
      assert.equal(sessionsFact.value.columns.id?.nullable, false);
      assert.equal(sessionsFact.value.columns.token?.nullable, false);

      const auditFact = allTables.find(
        (fact) => fact.value.tableName === "auditLogs",
      );
      assert.ok(auditFact);
      assert.equal(auditFact.value.sqlTableName, "audit_logs");
      assert.equal(auditFact.value.columns.id?.primaryKey, true);
      assert.equal(auditFact.value.columns.note?.nullable, true);

      assert.equal(projections.length, 1);
      assert.deepEqual(projections[0]?.value.projectedColumns, ["email", "id"]);
    });
  });

  void it("preserves static columns and emits dynamic-table-columns gaps for dynamic spreads and computed keys while keeping dynamic-query-projection for select queries", async () => {
    await withDrizzleHost("adapters/drizzle-dynamic-tables", async (host) => {
      await host.typecheck();

      const session = await openProject({
        cwd: host.rootDir,
        adapterResolutionRoot: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-drizzle"],
      });
      try {
        const ctx = session.project();
        const tableFacts = await ctx.facts(DrizzleTableColumnsFact);
        const queryFacts = await ctx.facts(QueryRowProjectionFact, {
          condition: { library: "drizzle" },
        });

        assert.equal(tableFacts.length, 1);
        const eventsFact = tableFacts[0];
        assert.ok(eventsFact);
        assert.equal(eventsFact.value.tableName, "events");
        assert.equal(eventsFact.value.hasDynamicColumns, true);
        assert.deepEqual(eventsFact.value.columnNames, ["createdAt", "id"]);
        assert.equal(eventsFact.value.columns.createdAt?.nullable, false);
        assert.equal(eventsFact.value.columns.createdAt?.hasDefault, true);
        assert.equal(eventsFact.gaps.length, 2);
        assert.ok(
          eventsFact.gaps.every((gap) => gap.kind === "dynamic-table-columns"),
        );

        assert.equal(queryFacts.length, 1);
        assert.equal(queryFacts[0]?.value.hasDynamicColumns, true);
        assert.deepEqual(queryFacts[0]?.value.projectedColumns, ["id"]);
        assert.ok(
          queryFacts[0]?.gaps.some(
            (gap) => gap.kind === "dynamic-query-projection",
          ),
        );
      } finally {
        await session.close();
      }
    });
  });

  void it("resolves cross-package Drizzle table declarations for untargeted scans and targeted DeclarationRef and SyntaxRef queries", async () => {
    await withDrizzleHost(
      "adapters/drizzle-cross-package",
      async (host) => {
        await host.typecheck("packages/app/tsconfig.json");

        const session = await openProject({
          cwd: host.rootDir,
          adapterResolutionRoot: host.rootDir,
          tsconfig: "packages/app/tsconfig.json",
          adapters: ["@verule/adapter-drizzle"],
        });
        try {
          const ctx = session.project();

          const untargetedFacts = await ctx.facts(DrizzleTableColumnsFact);

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
          assert.ok(tableArgRef);
          assert.ok(otherArgRef);
          const expectedArgHandleId = tableArgRef.handleId;

          const declRows = await ctx.resolveDeclarations([tableArgRef]);
          const tableDeclRef = declRows[0]?.declaration;
          assert.ok(tableDeclRef);
          const expectedDeclHandleId = tableDeclRef.handleId;

          const bySyntaxRef = await ctx.facts(DrizzleTableColumnsFact, {
            targets: [tableArgRef],
          });
          const byDeclRef = await ctx.facts(DrizzleTableColumnsFact, {
            targets: [tableDeclRef],
          });
          const byUnrelatedRef = await ctx.facts(DrizzleTableColumnsFact, {
            targets: [otherArgRef],
          });

          assert.equal(untargetedFacts.length, 1);
          assert.equal(untargetedFacts[0]?.value.tableName, "accounts");

          assert.equal(bySyntaxRef.length, 1);
          assert.equal(bySyntaxRef[0]?.subject.handleId, expectedArgHandleId);
          assert.equal(bySyntaxRef[0]?.value.tableName, "accounts");
          assert.equal(bySyntaxRef[0]?.value.sqlTableName, "accounts");
          assert.deepEqual(bySyntaxRef[0]?.value.columnNames, [
            "id",
            "nickname",
            "ownerEmail",
          ]);
          assert.equal(
            bySyntaxRef[0]?.value.columns.ownerEmail?.nullable,
            false,
          );
          assert.equal(bySyntaxRef[0]?.value.columns.nickname?.nullable, true);
          assert.equal(
            bySyntaxRef[0]?.value.columns.nickname?.hasDefault,
            true,
          );
          assert.ok(
            bySyntaxRef[0]?.evidence.some(
              (item) => item.at?.file === "packages/data/src/schema.ts",
            ),
          );

          assert.equal(byDeclRef.length, 1);
          assert.equal(byDeclRef[0]?.subject.handleId, expectedDeclHandleId);
          assert.equal(byDeclRef[0]?.value.tableName, "accounts");
          assert.deepEqual(byDeclRef[0]?.value.columnNames, [
            "id",
            "nickname",
            "ownerEmail",
          ]);

          assert.equal(byUnrelatedRef.length, 0);
        } finally {
          await session.close();
        }
      },
      (host) => {
        // Rename pkg-data -> data outside the checked repo because root .gitignore ignores data/.
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
    );
  });

  void it("merges complementary DrizzleTableColumnsFact contributions and raises FACT_CONFLICT on column disagreements", async () => {
    await withDrizzleHost("adapters/drizzle-merge-conflict", async (host) => {
      await host.typecheck();

      const makeProvider = (id: string, nullable: boolean, extraCol?: string) =>
        defineAdapter({
          id,
          version: "0.0.0",
          provides: [
            defineFactProvider(DrizzleTableColumnsFact, async (ctx) => {
              const nodes = await ctx.findSyntax({
                files: ["src/app.ts"],
                kinds: ["VariableDeclaration"],
              });
              const target = nodes[0]?.ref;
              if (!target) {
                return [];
              }
              return [
                {
                  subject: target,
                  value: buildDrizzleTableColumnsValue({
                    tableName: "users",
                    sqlTableName: "users",
                    hasDynamicColumns: false,
                    columns: {
                      email: {
                        name: "email",
                        sqlName: "email",
                        nullable,
                        primaryKey: false,
                        hasDefault: false,
                      },
                      ...(extraCol !== undefined
                        ? {
                            [extraCol]: {
                              name: extraCol,
                              sqlName: extraCol,
                              nullable: true,
                              primaryKey: false,
                              hasDefault: false,
                            },
                          }
                        : {}),
                    },
                  }),
                  evidence: [],
                  gaps: [],
                },
              ];
            }),
          ],
        });

      const mergeSession = await openProject({
        cwd: host.rootDir,
        adapterResolutionRoot: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: [
          makeProvider("provider-a", false),
          makeProvider("provider-b", false, "bio"),
        ],
      });
      try {
        const mergedFacts = await mergeSession
          .project()
          .facts(DrizzleTableColumnsFact);
        assert.equal(mergedFacts.length, 1);
        assert.deepEqual(mergedFacts[0]?.value.columnNames, ["bio", "email"]);
      } finally {
        await mergeSession.close();
      }

      const conflictSession = await openProject({
        cwd: host.rootDir,
        adapterResolutionRoot: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: [
          makeProvider("provider-a", false),
          makeProvider("provider-b", true),
        ],
      });
      try {
        await assert.rejects(
          () => conflictSession.project().facts(DrizzleTableColumnsFact),
          (error: unknown) =>
            error instanceof TssaeError && error.code === "FACT_CONFLICT",
        );
      } finally {
        await conflictSession.close();
      }
    });
  });
});
