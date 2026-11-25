import assert from "node:assert/strict";
import { renameSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  ValidatorSchemaFieldsFact,
  type ValidatorSchemaFieldsFactValue,
  type ValidatorSchemaSubjectKind,
} from "@verule/adapter-drizzle";
import {
  linkNodeModulesPackage,
  withTempHost,
  type PreparedTempHost,
} from "@verule/test-hosts";
import { openProject, type FactResult } from "tssae";

const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const drizzleOrmRoot = join(packageRoot, "node_modules", "drizzle-orm");
const drizzleZodRoot = join(packageRoot, "node_modules", "drizzle-zod");
const zodRoot = join(packageRoot, "node_modules", "zod");

function withDrizzleZodHost(
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
        "drizzle-zod": drizzleZodRoot,
        zod: zodRoot,
      },
      ...(setup !== undefined ? { setup } : {}),
    },
    callback,
  );
}

void describe("@verule/adapter-drizzle drizzle-zod conversion", () => {
  void it("recognizes createSelectSchema and createSchemaFactory via real drizzle-zod provenance and rejects fake same-named functions", async () => {
    await withDrizzleZodHost(
      "adapters/drizzle-zod-provenance",
      async (host) => {
        await host.typecheck();

        let schemaFacts: readonly FactResult<
          ValidatorSchemaSubjectKind,
          ValidatorSchemaFieldsFactValue
        >[] = [];

        const session = await openProject({
          cwd: host.rootDir,
          adapterResolutionRoot: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: ["@verule/adapter-drizzle"],
        });
        try {
          schemaFacts = await session
            .project()
            .facts(ValidatorSchemaFieldsFact);
        } finally {
          await session.close();
        }

        // Exactly 7 real drizzle-zod calls; neither fakeSelect nor fakeFactory.createSelectSchema emits ValidatorSchemaFieldsFact.
        assert.equal(schemaFacts.length, 7, JSON.stringify(schemaFacts));
        for (const fact of schemaFacts) {
          assert.equal(fact.value.library, "drizzle-zod");
          assert.equal(fact.value.hasDynamicFields, false);
          assert.deepEqual(Object.keys(fact.value.fields).toSorted(), [
            "bio",
            "email",
            "id",
          ]);
          assert.equal(fact.value.fields.id?.resolved, true);
          assert.equal(fact.value.fields.id?.nullable, false);
          assert.equal(fact.value.fields.email?.nullable, false);
          assert.equal(fact.value.fields.bio?.nullable, true);
          assert.ok(
            fact.evidence.some((ev) => ev.at?.file === "src/schemas.ts"),
          );
        }
      },
      (host) => {
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
    );
  });

  void it("preserves unrefined columns and emits field-level and shape-level gaps when refinements or tables cannot be resolved standalone", async () => {
    await withDrizzleZodHost("adapters/drizzle-zod-gaps", async (host) => {
      await host.typecheck();

      let schemaFacts: readonly FactResult<
        ValidatorSchemaSubjectKind,
        ValidatorSchemaFieldsFactValue
      >[] = [];

      const session = await openProject({
        cwd: host.rootDir,
        adapterResolutionRoot: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-drizzle"],
      });
      try {
        schemaFacts = await session.project().facts(ValidatorSchemaFieldsFact);
      } finally {
        await session.close();
      }

      assert.equal(schemaFacts.length, 2);

      const refined = schemaFacts.find((f) => "id" in f.value.fields);
      const unresolvedTable = schemaFacts.find(
        (f) => !("id" in f.value.fields),
      );
      assert.ok(refined && unresolvedTable);

      assert.equal(refined.value.hasDynamicFields, true);
      assert.equal(refined.value.fields.id?.resolved, true);
      assert.equal(refined.value.fields.name?.resolved, true);
      assert.equal(refined.value.fields.nickname?.resolved, true);
      assert.equal(refined.value.fields.nickname?.nullable, true);
      // Without a validator provider enabled, refined `age` is marked unresolved while other columns stay intact
      assert.equal(refined.value.fields.age?.resolved, false);
      assert.ok(
        refined.value.fields.age?.gaps?.some(
          (g) => g.kind === "unresolved-schema-field",
        ),
      );
      assert.ok(refined.gaps.some((g) => g.kind === "dynamic-schema-spread"));
      assert.ok(refined.gaps.some((g) => g.kind === "dynamic-schema-field"));

      assert.equal(unresolvedTable.value.hasDynamicFields, true);
      assert.ok(
        unresolvedTable.gaps.some((g) => g.kind === "unresolved-drizzle-table"),
      );
    });
  });
});
