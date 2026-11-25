import assert from "node:assert/strict";
import { readFileSync, renameSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  ValidatorFieldFact,
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
  type ValidatorFieldFactValue,
  type ValidatorInputFieldsFactValue,
  type ValidatorOutputFieldsFactValue,
} from "@verule/adapter-zod";
import {
  installPackedWorkspacePackages,
  linkNodeModulesPackage,
  withTempHost,
  type PreparedTempHost,
} from "@verule/test-hosts";
import { defineAdapter, openProject, type FactResult } from "tssae";

const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const zodRoot = join(packageRoot, "node_modules", "zod");
const drizzleOrmRoot = join(packageRoot, "node_modules", "drizzle-orm");
const drizzleZodRoot = join(packageRoot, "node_modules", "drizzle-zod");

function withZodHost(
  hostName: string,
  callback: (host: PreparedTempHost) => Promise<void>,
  options: {
    readonly linkZod?: boolean;
    readonly linkDrizzle?: boolean;
    readonly setup?: (host: PreparedTempHost) => Promise<void> | void;
  } = {},
): Promise<void> {
  return withTempHost(
    {
      host: hostName,
      packageLinks: {
        "@verule/adapter-zod": packageRoot,
        ...(options.linkZod ? { zod: zodRoot } : {}),
        ...(options.linkDrizzle
          ? {
              "drizzle-orm": drizzleOrmRoot,
              "drizzle-zod": drizzleZodRoot,
            }
          : {}),
      },
      ...(options.setup !== undefined ? { setup: options.setup } : {}),
    },
    callback,
  );
}

void describe("@verule/adapter-zod", () => {
  void it("extracts basic z.object schemas and .parse/.safeParse/.parseAsync/.safeParseAsync calls while ignoring non-Zod .parse", async () => {
    await withZodHost("adapters/zod-basic-calls", async (host) => {
      await host.typecheck();

      let inputFacts: readonly FactResult<
        "syntax" | "call" | "declaration",
        ValidatorInputFieldsFactValue
      >[] = [];
      let outputFacts: readonly FactResult<
        "syntax" | "call" | "declaration",
        ValidatorOutputFieldsFactValue
      >[] = [];

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-zod"],
      });
      try {
        const ctx = session.project();
        inputFacts = await ctx.facts(ValidatorInputFieldsFact);
        outputFacts = await ctx.facts(ValidatorOutputFieldsFact);
      } finally {
        await session.close();
      }

      const calls = inputFacts.filter((f) => f.value.siteKind === "call");
      assert.equal(calls.length, 4);
      const methods = calls
        .map((c) => c.value.validatorMethod ?? "")
        .toSorted((a, b) => (a < b ? -1 : a > b ? 1 : 0));
      assert.deepEqual(methods, [
        "parse",
        "parseAsync",
        "safeParse",
        "safeParseAsync",
      ]);
      for (const callFact of calls) {
        assert.deepEqual(callFact.value.requiredFields, ["age", "id"]);
        assert.deepEqual(callFact.value.optionalFields, ["bio"]);
        assert.deepEqual(callFact.value.defaultedFields, []);
        assert.equal(callFact.value.hasDynamicFields, false);
        assert.ok(
          callFact.evidence.some(
            (ev) => ev.at?.file === "src/handlers.ts" && ev.at.line === 3,
          ),
        );
      }

      const schemas = inputFacts.filter((f) => f.value.siteKind === "schema");
      assert.equal(schemas.length, 1);
      assert.equal(schemas[0]!.value.schemaName, "userSchema");
      assert.deepEqual(schemas[0]!.value.requiredFields, ["age", "id"]);

      const outCalls = outputFacts.filter((f) => f.value.siteKind === "call");
      assert.equal(outCalls.length, 4);
      assert.deepEqual(outCalls[0]!.value.presentFields, ["age", "id"]);
      assert.deepEqual(outCalls[0]!.value.optionalFields, ["bio"]);
    });
  });

  void it("distinguishes .nullable(), .optional(), .nullish(), v3 .default(), v4 .prefault()/.catch(), and unwraps outer chains", async () => {
    await withZodHost("adapters/zod-modifiers", async (host) => {
      await host.typecheck();

      let inputFacts: readonly FactResult<
        "syntax" | "call" | "declaration",
        ValidatorInputFieldsFactValue
      >[] = [];
      let outputFacts: readonly FactResult<
        "syntax" | "call" | "declaration",
        ValidatorOutputFieldsFactValue
      >[] = [];

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-zod"],
      });
      try {
        const ctx = session.project();
        inputFacts = await ctx.facts(ValidatorInputFieldsFact, {
          condition: { scope: "calls" },
        });
        outputFacts = await ctx.facts(ValidatorOutputFieldsFact, {
          condition: { scope: "calls" },
        });
      } finally {
        await session.close();
      }

      const accountIn = inputFacts.find(
        (f) => f.value.schemaName === "accountSchema",
      );
      assert.ok(accountIn);
      // homepageUrl has .nullable() ONLY -> still required on input!
      assert.deepEqual(accountIn.value.requiredFields, ["homepageUrl", "id"]);
      assert.equal(accountIn.value.fields.homepageUrl?.nullable, true);
      assert.deepEqual(accountIn.value.defaultedFields, [
        "fallbackTheme",
        "password",
        "plan",
        "timezone",
      ]);
      assert.deepEqual(accountIn.value.optionalFields, [
        "customTheme",
        "fallbackTheme",
        "nickname",
        "password",
        "plan",
        "timezone",
      ]);

      const accountOut = outputFacts.find(
        (f) => f.value.schemaName === "accountSchema",
      );
      assert.ok(accountOut);
      // On output, defaulted/prefaulted/caught fields ARE guaranteed present!
      assert.deepEqual(accountOut.value.presentFields, [
        "fallbackTheme",
        "homepageUrl",
        "id",
        "password",
        "plan",
        "timezone",
      ]);
      assert.deepEqual(accountOut.value.optionalFields, [
        "customTheme",
        "nickname",
      ]);
      assert.deepEqual(accountOut.value.defaultedFields, [
        "fallbackTheme",
        "password",
        "plan",
        "timezone",
      ]);

      const pipedIn = inputFacts.find(
        (f) => f.value.schemaName === "pipedSchema",
      );
      assert.ok(pipedIn);
      assert.deepEqual(pipedIn.value.requiredFields, ["rawId"]);
      assert.deepEqual(pipedIn.value.defaultedFields, ["role"]);

      const pipedOut = outputFacts.find(
        (f) => f.value.schemaName === "pipedSchema",
      );
      assert.ok(pipedOut);
      assert.deepEqual(pipedOut.value.presentFields, ["rawId", "role"]);
    });
  });

  void it("resolves .extend, .merge, .pick, .omit, .partial, .required across files and re-extracts after session.update()", async () => {
    await withZodHost("adapters/zod-compositions", async (host) => {
      await host.typecheck();

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-zod"],
      });

      try {
        let callFacts = await session
          .project()
          .facts(ValidatorInputFieldsFact, {
            condition: { scope: "calls" },
          });
        const bySchema = (name: string) =>
          callFacts.find((f) => f.value.schemaName === name);

        assert.deepEqual(bySchema("extendedPage")?.value.requiredFields, [
          "customDomain",
          "id",
          "secret",
          "slug",
          "title",
        ]);
        assert.deepEqual(bySchema("extendedPage")?.value.defaultedFields, [
          "locale",
          "password",
        ]);
        // Evidence points back into src/schemas.ts!
        assert.ok(
          bySchema("extendedPage")?.evidence.some(
            (ev) => ev.at?.file === "src/schemas.ts",
          ),
        );

        assert.deepEqual(bySchema("pickedGate")?.value.requiredFields, [
          "slug",
          "title",
        ]);
        assert.deepEqual(bySchema("omittedPage")?.value.requiredFields, []);
        assert.deepEqual(bySchema("omittedPage")?.value.defaultedFields, [
          "locale",
        ]);
        assert.deepEqual(bySchema("partialGate")?.value.requiredFields, []);
        assert.deepEqual(bySchema("partialGate")?.value.optionalFields, [
          "slug",
          "title",
        ]);
        assert.deepEqual(bySchema("requiredAgain")?.value.requiredFields, [
          "slug",
        ]);
        assert.deepEqual(bySchema("requiredAgain")?.value.optionalFields, [
          "title",
        ]);

        // Modify src/schemas.ts and update session -> pickedGate/extendedPage facts & evidence update!
        host.replaceText(
          "src/schemas.ts",
          "slug: z.string(),",
          "slug: z.string().optional(),",
        );
        await session.update({ changed: ["src/schemas.ts"] });
        callFacts = await session.project().facts(ValidatorInputFieldsFact, {
          condition: { scope: "calls" },
        });

        assert.deepEqual(bySchema("pickedGate")?.value.requiredFields, [
          "title",
        ]);
        assert.deepEqual(bySchema("pickedGate")?.value.optionalFields, [
          "slug",
        ]);
        assert.equal(bySchema("pickedGate")?.snapshotId, session.snapshotId);
      } finally {
        await session.close();
      }
    });
  });

  void it("preserves known deterministic fields and emits location-aware Gaps for dynamic constructs", async () => {
    await withZodHost("adapters/zod-dynamic", async (host) => {
      await host.typecheck();

      let inputFacts: readonly FactResult<
        "syntax" | "call" | "declaration",
        ValidatorInputFieldsFactValue
      >[] = [];

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-zod"],
      });
      try {
        inputFacts = await session.project().facts(ValidatorInputFieldsFact, {
          condition: { scope: "calls" },
        });
      } finally {
        await session.close();
      }

      const mixed = inputFacts.find(
        (f) => f.value.schemaName === "mixedSchema",
      );
      assert.ok(mixed);
      assert.equal(mixed.value.hasDynamicFields, true);
      // Known deterministic fields preserved!
      assert.deepEqual(mixed.value.requiredFields, ["extra", "id", "meta"]);
      assert.ok(
        mixed.gaps.some(
          (g) => g.kind === "dynamic-schema-spread" && g.at?.line === 8,
        ),
      );
      assert.ok(
        mixed.gaps.some(
          (g) => g.kind === "dynamic-schema-field" && g.at?.line === 9,
        ),
      );
      assert.ok(
        mixed.gaps.some(
          (g) => g.kind === "dynamic-schema-field" && g.at?.line === 10,
        ),
      );
      assert.ok(
        mixed.gaps.some(
          (g) => g.kind === "dynamic-schema-field" && g.at?.line === 11,
        ),
      );

      const dict = inputFacts.find((f) => f.value.schemaName === "dictSchema");
      assert.ok(dict);
      assert.equal(dict.value.hasDynamicFields, true);
      assert.ok(
        dict.gaps.some(
          (g) => g.kind === "dynamic-schema-shape" && g.at?.line === 14,
        ),
      );
    });
  });

  void it("loads the native package only when queried and shares cached facts within a snapshot", async () => {
    await withZodHost("adapters/zod-lazy-cache", async (host) => {
      await host.typecheck();

      // 1. Registered adapter stays dormant when only non-fact queries run
      const unqueriedSession = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-zod"],
      });
      try {
        const ctx = unqueriedSession.project();
        const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
        assert.ok(calls.length > 0);
        assert.ok(
          unqueriedSession.nativeAdapters.some(
            (entry) => entry.packageName === "@verule/adapter-zod",
          ),
        );
      } finally {
        await unqueriedSession.close();
      }

      // 2. Repeated queries for the same fact within one snapshot hit FactCache and match exactly
      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-zod"],
      });
      try {
        const ctx = session.project();
        const firstFacts = await ctx.facts(ValidatorInputFieldsFact, {
          condition: { scope: "calls" },
          explain: true,
        });
        const secondFacts = await ctx.facts(ValidatorInputFieldsFact, {
          condition: { scope: "calls" },
          explain: true,
        });
        assert.equal(firstFacts.length, 1);
        assert.equal(secondFacts.length, 1);
        assert.deepEqual(firstFacts[0]!.providers, ["zod"]);
        assert.deepEqual(secondFacts, firstFacts);
      } finally {
        await session.close();
      }
    });
  });

  void it("compiles an external TypeScript consumer against built tssae and @verule/adapter-zod .d.ts", async () => {
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
            ["tssae", "@verule/adapter-zod"],
            { cwd: packageRoot },
          );
        },
      },
      async (host) => {
        await host.typecheck();
      },
    );
  });

  void it("extracts real zod v4 schemas (.prefault, .catch, .default, .nullable, .transform, .pipe) and allows alternative providers to implement the same FactType", async () => {
    await withZodHost(
      "adapters/zod-real-v4",
      async (host) => {
        await host.typecheck();

        const zodSession = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: ["@verule/adapter-zod"],
        });
        let inputResults: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorInputFieldsFactValue
        >[];
        let outputResults: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorOutputFieldsFactValue
        >[];
        try {
          const ctx = zodSession.project();
          inputResults = await ctx.facts(ValidatorInputFieldsFact, {
            condition: { scope: "calls" },
          });
          outputResults = await ctx.facts(ValidatorOutputFieldsFact, {
            condition: { scope: "calls" },
          });
        } finally {
          await zodSession.close();
        }

        assert.equal(inputResults.length, 1);
        assert.deepEqual(inputResults[0]!.value.requiredFields, [
          "id",
          "nullableKey",
        ]);
        assert.deepEqual(inputResults[0]!.value.defaultedFields, [
          "caught",
          "defaulted",
          "prefaulted",
        ]);
        assert.deepEqual(outputResults[0]!.value.presentFields, [
          "caught",
          "defaulted",
          "id",
          "nullableKey",
          "prefaulted",
        ]);
        assert.deepEqual(outputResults[0]!.value.optionalFields, ["optKey"]);

        // Same query works unchanged when an alternative validator implements ValidatorInputFieldsFact
        const altAdapter = defineAdapter({
          id: "alt-validator",
          provides: [
            {
              factType: ValidatorInputFieldsFact,
              extract: async (ctx) => {
                const calls = await ctx.findSyntax({
                  kinds: ["CallExpression"],
                });
                const safeParseCall = calls.find((c) =>
                  c.text.includes("realSchema.safeParse("),
                );
                if (!safeParseCall) return [];
                return [
                  {
                    subject: safeParseCall.ref,
                    value: {
                      siteKind: "call",
                      validatorMethod: "safeParse",
                      requiredFields: ["altOnly"],
                      optionalFields: [],
                      defaultedFields: [],
                      fields: {
                        altOnly: {
                          name: "altOnly",
                          requirement: "required",
                          nullable: false,
                        },
                      },
                      hasDynamicFields: false,
                    },
                  },
                ];
              },
            },
          ],
        });

        const altSession = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [altAdapter],
        });
        let altResults: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorInputFieldsFactValue
        >[];
        try {
          altResults = await altSession
            .project()
            .facts(ValidatorInputFieldsFact, { condition: { scope: "calls" } });
        } finally {
          await altSession.close();
        }
        assert.equal(altResults.length, 1);
        assert.deepEqual(altResults[0]!.value.requiredFields, ["altOnly"]);
        assert.deepEqual(altResults[0]!.providers, ["alt-validator"]);
      },
      { linkZod: true },
    );
  });

  void it("retains explicit .extend/.default/.pick conclusions on single-Zod drizzle-zod schemas while surfacing unresolved-schema for base table fields", async () => {
    await withZodHost(
      "adapters/zod-drizzle-standalone",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: ["@verule/adapter-zod"],
        });
        let inputResults: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorInputFieldsFactValue
        >[];
        let outputResults: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorOutputFieldsFactValue
        >[];
        try {
          const ctx = session.project();
          inputResults = await ctx.facts(ValidatorInputFieldsFact, {
            condition: { scope: "calls" },
          });
          outputResults = await ctx.facts(ValidatorOutputFieldsFact, {
            condition: { scope: "calls" },
          });
        } finally {
          await session.close();
        }
        assert.equal(inputResults.length, 1);

        const gateIn = inputResults[0]!;
        assert.equal(gateIn.value.schemaName, "gateFieldsSchema");
        // Without @verule/adapter-drizzle, base createSelectSchema(page) fields are unknown,
        // while explicit .extend() defaults/prefaults and .pick() filtering are retained!
        assert.deepEqual(gateIn.value.requiredFields, []);
        assert.deepEqual(gateIn.value.defaultedFields, [
          "accessType",
          "password",
        ]);
        assert.deepEqual(gateIn.value.optionalFields, [
          "accessType",
          "password",
        ]);
        assert.equal(gateIn.value.hasDynamicFields, true);
        assert.ok(
          gateIn.gaps.some(
            (gap) =>
              gap.kind === "unresolved-schema" &&
              gap.at?.file === "packages/db/src/validation.ts" &&
              gap.at.line === 6,
          ),
          JSON.stringify(gateIn.gaps),
        );
        assert.ok(
          gateIn.evidence.some(
            (ev) => ev.at?.file === "packages/db/src/validation.ts",
          ),
        );

        const gateOut = outputResults[0]!;
        assert.deepEqual(gateOut.value.presentFields, [
          "accessType",
          "password",
        ]);
        assert.deepEqual(gateOut.value.defaultedFields, [
          "accessType",
          "password",
        ]);
      },
      {
        linkZod: true,
        linkDrizzle: true,
        setup: (host) => {
          renameSync(
            host.resolvePath("tsconfig.runtime.json"),
            host.resolvePath("tsconfig.json"),
          );
          linkNodeModulesPackage(
            host.rootDir,
            "@acme/db",
            host.resolvePath("packages/db"),
          );
        },
      },
    );
  });

  void it("tracks runtime field validators as field-scoped unknowns (not requiredFields), clears superseded gaps on .extend(), and extracts ValidatorFieldFact", async () => {
    await withZodHost(
      "adapters/zod-runtime-unknowns",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: ["@verule/adapter-zod"],
        });
        let inputResults: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorInputFieldsFactValue
        >[];
        let outputResults: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorOutputFieldsFactValue
        >[];
        let fieldFacts: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorFieldFactValue
        >[];
        try {
          const ctx = session.project();
          inputResults = await ctx.facts(ValidatorInputFieldsFact, {
            condition: { scope: "calls" },
            explain: true,
          });
          outputResults = await ctx.facts(ValidatorOutputFieldsFact, {
            condition: { scope: "calls" },
            explain: true,
          });
          const props = await ctx.findSyntax({
            files: ["src/runtime.ts"],
            kinds: ["PropertyAssignment"],
          });
          const targetNames = [
            "directReplace",
            "nullableReplace",
            "unknownReplace",
            "cbDefault",
            "cbOptional",
            "cbNullable",
          ];
          const initRefs = props
            .filter((p) =>
              targetNames.some((name) => p.text.startsWith(`${name}:`)),
            )
            .map((p) => p.initializer!)
            .filter(Boolean);
          fieldFacts = await ctx.facts(ValidatorFieldFact, {
            targets: initRefs,
          });
        } finally {
          await session.close();
        }

        const partialIn = inputResults.find(
          (f) => f.value.schemaName === "partialUnknownSchema",
        );
        assert.ok(partialIn);
        assert.equal(partialIn.value.hasDynamicFields, true);
        // Runtime validators MUST NOT become requiredFields!
        assert.deepEqual(partialIn.value.requiredFields, ["id"]);
        assert.equal(partialIn.value.fields.age, undefined);
        assert.equal(partialIn.value.fields.extra, undefined);
        assert.equal(partialIn.gaps.length, 2);
        assert.ok(
          partialIn.gaps.every((g) => g.kind === "unresolved-schema-field"),
        );

        const overriddenIn = inputResults.find(
          (f) => f.value.schemaName === "overriddenSchema",
        );
        const overriddenOut = outputResults.find(
          (f) => f.value.schemaName === "overriddenSchema",
        );
        assert.ok(overriddenIn && overriddenOut);
        assert.equal(overriddenIn.value.hasDynamicFields, false);
        assert.deepEqual(overriddenIn.gaps, []);
        assert.deepEqual(overriddenIn.value.requiredFields, ["id"]);
        assert.deepEqual(overriddenIn.value.defaultedFields, ["age"]);
        assert.deepEqual(overriddenIn.value.optionalFields, ["age", "extra"]);
        assert.deepEqual(overriddenOut.value.presentFields, ["age", "id"]);
        assert.deepEqual(overriddenOut.value.optionalFields, ["extra"]);

        assert.equal(fieldFacts.length, 6);
        const byResolved = fieldFacts.filter((f) => f.value.resolved);
        const byUnresolved = fieldFacts.filter((f) => !f.value.resolved);
        assert.equal(byResolved.length, 5);
        assert.equal(byUnresolved.length, 1);
        assert.ok(
          byUnresolved[0]!.gaps.some(
            (g) => g.kind === "unresolved-schema-field",
          ),
        );
        assert.ok(
          byResolved.some(
            (f) =>
              f.value.mode === "callback" &&
              f.value.requirement === "defaulted" &&
              f.value.fromDefault === true &&
              f.value.nullabilityExplicit === false,
          ),
        );
        assert.ok(
          byResolved.some(
            (f) =>
              f.value.mode === "callback" &&
              f.value.requirement === "required" &&
              f.value.nullable === true &&
              f.value.nullabilityExplicit === true,
          ),
        );

        assert.ok(
          partialIn.explainTrace?.localGaps?.some(
            (lg) =>
              lg.status === "unresolved" &&
              lg.field === "age" &&
              lg.gap.kind === "unresolved-schema-field",
          ),
          JSON.stringify(partialIn.explainTrace?.localGaps),
        );

        assert.ok(
          overriddenIn.explainTrace?.localGaps?.some(
            (lg) =>
              lg.status === "resolved_by_override" &&
              lg.field === "age" &&
              lg.resolvedBy?.operation === ".extend()" &&
              lg.resolvedBy.providerId === "zod",
          ),
          JSON.stringify(overriddenIn.explainTrace?.localGaps),
        );
      },
      { linkZod: true },
    );
  });

  void it("enforces sound input vs output gap semantics for runtimeFieldValidator().default(21), .and(), z.intersection(), .pipe(), and z.pipe()", async () => {
    await withZodHost(
      "adapters/zod-combinators",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: ["@verule/adapter-zod"],
        });

        try {
          const ctx = session.project();
          const inputResults = await ctx.facts(ValidatorInputFieldsFact, {
            condition: { scope: "calls" },
            explain: true,
          });
          const outputResults = await ctx.facts(ValidatorOutputFieldsFact, {
            condition: { scope: "calls" },
            explain: true,
          });

          const inputByName = (name: string) =>
            inputResults.find((f) => f.value.schemaName === name);
          const outputByName = (name: string) =>
            outputResults.find((f) => f.value.schemaName === name);

          // 1. Field modifier .default(21) resolves BOTH input and output.
          const modIn = inputByName("modifierOverrideSchema")!;
          const modOut = outputByName("modifierOverrideSchema")!;
          assert.deepEqual(modIn.gaps, []);
          assert.deepEqual(modIn.value.defaultedFields, ["age"]);
          assert.equal(modIn.value.hasDynamicFields, false);
          assert.deepEqual(modOut.gaps, []);
          assert.deepEqual(modOut.value.presentFields, ["age", "id"]);
          assert.deepEqual(modOut.value.defaultedFields, ["age"]);
          assert.equal(modOut.value.hasDynamicFields, false);

          // 2. .and({ age: z.number().default(21) }) keeps input gap (unknown) while resolving output.
          const andDefIn = inputByName("andDefaultedSchema")!;
          const andDefOut = outputByName("andDefaultedSchema")!;
          assert.deepEqual(andDefIn.value.requiredFields, ["id"]);
          assert.deepEqual(andDefIn.value.optionalFields, []);
          assert.deepEqual(andDefIn.value.defaultedFields, []);
          assert.equal(andDefIn.value.fields.age, undefined);
          assert.equal(andDefIn.value.hasDynamicFields, true);
          assert.ok(
            andDefIn.gaps.some(
              (g) => g.kind === "unresolved-schema-field" && g.at?.line === 12,
            ),
            JSON.stringify(andDefIn.gaps),
          );
          assert.deepEqual(andDefOut.gaps, []);
          assert.deepEqual(andDefOut.value.presentFields, ["age", "id"]);
          assert.deepEqual(andDefOut.value.defaultedFields, ["age"]);
          assert.equal(andDefOut.value.hasDynamicFields, false);

          // 3. .and({ age: z.number() }) (Required) resolves BOTH input (Required) and output (Present).
          const andReqIn = inputByName("andRequiredSchema")!;
          const andReqOut = outputByName("andRequiredSchema")!;
          assert.deepEqual(andReqIn.gaps, []);
          assert.deepEqual(andReqIn.value.requiredFields, ["age", "id"]);
          assert.equal(andReqIn.value.hasDynamicFields, false);
          assert.deepEqual(andReqOut.gaps, []);
          assert.deepEqual(andReqOut.value.presentFields, ["age", "id"]);
          assert.equal(andReqOut.value.hasDynamicFields, false);

          // 4. .pipe({ id, age: z.number().default(21) }) and z.pipe(...) keep inputSchema's gap on input, resolve output.
          for (const pipeName of ["pipeOverrideSchema", "staticPipeSchema"]) {
            const pipeIn = inputByName(pipeName)!;
            const pipeOut = outputByName(pipeName)!;
            assert.deepEqual(pipeIn.value.requiredFields, ["id"]);
            assert.deepEqual(pipeIn.value.defaultedFields, []);
            assert.equal(pipeIn.value.fields.age, undefined);
            assert.equal(pipeIn.value.hasDynamicFields, true);
            assert.ok(
              pipeIn.gaps.some(
                (g) =>
                  g.kind === "unresolved-schema-field" && g.at?.line === 12,
              ),
              JSON.stringify(pipeIn.gaps),
            );
            assert.deepEqual(pipeOut.gaps, []);
            assert.deepEqual(pipeOut.value.presentFields, ["age", "id"]);
            assert.deepEqual(pipeOut.value.defaultedFields, ["age"]);
            assert.equal(pipeOut.value.hasDynamicFields, false);
          }

          // 5. z.intersection(base, { age: z.number().default(21) }) matches .and() input vs output semantics.
          const zIntIn = inputByName("staticIntersectionSchema")!;
          const zIntOut = outputByName("staticIntersectionSchema")!;
          assert.deepEqual(zIntIn.value.requiredFields, ["id"]);
          assert.deepEqual(zIntIn.value.defaultedFields, []);
          assert.equal(zIntIn.value.fields.age, undefined);
          assert.equal(zIntIn.value.hasDynamicFields, true);
          assert.ok(
            zIntIn.gaps.some(
              (g) => g.kind === "unresolved-schema-field" && g.at?.line === 12,
            ),
            JSON.stringify(zIntIn.gaps),
          );
          assert.deepEqual(zIntOut.gaps, []);
          assert.deepEqual(zIntOut.value.presentFields, ["age", "id"]);
          assert.deepEqual(zIntOut.value.defaultedFields, ["age"]);
          assert.equal(zIntOut.value.hasDynamicFields, false);

          // Targeted explainTrace checks on input vs output facts:
          assert.ok(
            modIn.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "resolved_by_override" &&
                lg.field === "age" &&
                lg.gap.at?.line === 7 &&
                lg.resolvedBy?.providerId === "zod" &&
                lg.resolvedBy.operation === ".default()" &&
                lg.resolvedBy.at?.line === 7,
            ),
            JSON.stringify(modIn.explainTrace?.localGaps),
          );

          assert.ok(
            modOut.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "resolved_by_override" &&
                lg.field === "age" &&
                lg.gap.at?.line === 7 &&
                lg.resolvedBy?.providerId === "zod" &&
                lg.resolvedBy.operation === ".default()" &&
                lg.resolvedBy.at?.line === 7,
            ),
            JSON.stringify(modOut.explainTrace?.localGaps),
          );

          assert.ok(
            andDefIn.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "unresolved" &&
                lg.field === "age" &&
                lg.gap.kind === "unresolved-schema-field" &&
                lg.gap.at?.line === 12,
            ),
            JSON.stringify(andDefIn.explainTrace?.localGaps),
          );

          assert.ok(
            andDefOut.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "resolved_by_override" &&
                lg.field === "age" &&
                lg.gap.at?.line === 12 &&
                lg.resolvedBy?.providerId === "zod" &&
                lg.resolvedBy.operation === ".and()" &&
                lg.resolvedBy.at?.line === 16,
            ),
            JSON.stringify(andDefOut.explainTrace?.localGaps),
          );

          assert.ok(
            andReqIn.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "resolved_by_override" &&
                lg.field === "age" &&
                lg.gap.at?.line === 12 &&
                lg.resolvedBy?.providerId === "zod" &&
                lg.resolvedBy.operation === ".and()" &&
                lg.resolvedBy.at?.line === 20,
            ),
            JSON.stringify(andReqIn.explainTrace?.localGaps),
          );

          const pipeOverrideIn = inputByName("pipeOverrideSchema")!;
          const pipeOverrideOut = outputByName("pipeOverrideSchema")!;
          assert.ok(
            pipeOverrideIn.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "unresolved" &&
                lg.field === "age" &&
                lg.gap.kind === "unresolved-schema-field" &&
                lg.gap.at?.line === 12,
            ),
            JSON.stringify(pipeOverrideIn.explainTrace?.localGaps),
          );

          assert.ok(
            pipeOverrideOut.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "resolved_by_override" &&
                lg.field === "age" &&
                lg.gap.at?.line === 12 &&
                lg.resolvedBy?.providerId === "zod" &&
                lg.resolvedBy.operation === ".pipe()" &&
                lg.resolvedBy.at?.line === 24,
            ),
            JSON.stringify(pipeOverrideOut.explainTrace?.localGaps),
          );
        } finally {
          await session.close();
        }
      },
      { linkZod: true },
    );
  });

  void it("replaces known base fields when overridden by unknown field validators in .extend(), .merge(), and object spread, and resolves chained overrides", async () => {
    await withZodHost(
      "adapters/zod-unknown-overrides",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: ["@verule/adapter-zod"],
        });

        try {
          const ctx = session.project();
          const inputResults = await ctx.facts(ValidatorInputFieldsFact, {
            condition: { scope: "calls" },
            explain: true,
          });
          const outputResults = await ctx.facts(ValidatorOutputFieldsFact, {
            condition: { scope: "calls" },
            explain: true,
          });

          const inputByName = (name: string) =>
            inputResults.find((f) => f.value.schemaName === name);
          const outputByName = (name: string) =>
            outputResults.find((f) => f.value.schemaName === name);

          for (const [schemaName, expectedGapLine] of [
            ["extendUnknownSchema", 11],
            ["mergeUnknownSchema", 19],
            ["spreadUnknownSchema", 23],
          ] as const) {
            const inFact = inputByName(schemaName)!;
            const outFact = outputByName(schemaName)!;
            assert.deepEqual(inFact.value.requiredFields, ["id"]);
            assert.equal(inFact.value.fields.age, undefined);
            assert.equal(inFact.value.hasDynamicFields, true);
            assert.ok(
              inFact.gaps.some(
                (g) =>
                  g.kind === "unresolved-schema-field" &&
                  g.at?.line === expectedGapLine,
              ),
              `${schemaName} input gaps: ${JSON.stringify(inFact.gaps)}`,
            );

            assert.deepEqual(outFact.value.presentFields, ["id"]);
            assert.equal(outFact.value.fields.age, undefined);
            assert.equal(outFact.value.hasDynamicFields, true);
            assert.ok(
              outFact.gaps.some(
                (g) =>
                  g.kind === "unresolved-schema-field" &&
                  g.at?.line === expectedGapLine,
              ),
              `${schemaName} output gaps: ${JSON.stringify(outFact.gaps)}`,
            );
          }

          for (const schemaName of [
            "extendReResolvedSchema",
            "spreadReResolvedSchema",
          ] as const) {
            const inFact = inputByName(schemaName)!;
            const outFact = outputByName(schemaName)!;
            assert.deepEqual(inFact.gaps, []);
            assert.deepEqual(inFact.value.requiredFields, ["id"]);
            assert.deepEqual(inFact.value.optionalFields, ["age"]);
            assert.deepEqual(inFact.value.defaultedFields, ["age"]);
            assert.equal(inFact.value.hasDynamicFields, false);

            assert.deepEqual(outFact.gaps, []);
            assert.deepEqual(outFact.value.presentFields, ["age", "id"]);
            assert.deepEqual(outFact.value.defaultedFields, ["age"]);
            assert.equal(outFact.value.hasDynamicFields, false);
          }

          assert.ok(
            inputByName("extendUnknownSchema")!.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "unresolved" &&
                lg.field === "age" &&
                lg.gap.kind === "unresolved-schema-field" &&
                lg.gap.at?.line === 11,
            ),
            JSON.stringify(
              inputByName("extendUnknownSchema")!.explainTrace?.localGaps,
            ),
          );

          assert.ok(
            inputByName(
              "extendReResolvedSchema",
            )!.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "resolved_by_override" &&
                lg.field === "age" &&
                lg.gap.at?.line === 11 &&
                lg.resolvedBy?.providerId === "zod" &&
                lg.resolvedBy.operation === ".extend()" &&
                lg.resolvedBy.at?.line === 15,
            ),
            JSON.stringify(
              inputByName("extendReResolvedSchema")!.explainTrace?.localGaps,
            ),
          );

          assert.ok(
            inputByName("spreadUnknownSchema")!.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "unresolved" &&
                lg.field === "age" &&
                lg.gap.kind === "unresolved-schema-field" &&
                lg.gap.at?.line === 23,
            ),
            JSON.stringify(
              inputByName("spreadUnknownSchema")!.explainTrace?.localGaps,
            ),
          );

          assert.ok(
            inputByName(
              "spreadReResolvedSchema",
            )!.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "resolved_by_override" &&
                lg.field === "age" &&
                lg.gap.at?.line === 23 &&
                lg.resolvedBy?.providerId === "zod" &&
                lg.resolvedBy.operation === ".extend()" &&
                lg.resolvedBy.at?.line === 33,
            ),
            JSON.stringify(
              inputByName("spreadReResolvedSchema")!.explainTrace?.localGaps,
            ),
          );
        } finally {
          await session.close();
        }
      },
      { linkZod: true },
    );
  });

  void it("replaces output with an unresolved SchemaSide for unrecognized .pipe() / z.pipe() targets and avoids resolving nullable .and() over unknown fields", async () => {
    await withZodHost(
      "adapters/zod-pipe-and-nullable",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: ["@verule/adapter-zod"],
        });

        try {
          const ctx = session.project();
          const inputResults = await ctx.facts(ValidatorInputFieldsFact, {
            condition: { scope: "calls" },
            explain: true,
          });
          const outputResults = await ctx.facts(ValidatorOutputFieldsFact, {
            condition: { scope: "calls" },
            explain: true,
          });

          const inputByName = (name: string) =>
            inputResults.find((f) => f.value.schemaName === name);
          const outputByName = (name: string) =>
            outputResults.find((f) => f.value.schemaName === name);

          for (const [schemaName, targetLine] of [
            ["methodPipeUnrecognized", 8],
            ["staticPipeUnrecognized", 12],
          ] as const) {
            const inFact = inputByName(schemaName)!;
            const outFact = outputByName(schemaName)!;
            assert.deepEqual(inFact.gaps, []);
            assert.deepEqual(inFact.value.requiredFields, ["rawId"]);
            assert.equal(inFact.value.hasDynamicFields, false);

            assert.deepEqual(outFact.value.presentFields, []);
            assert.deepEqual(outFact.value.optionalFields, []);
            assert.deepEqual(outFact.value.defaultedFields, []);
            assert.deepEqual(outFact.value.fields, {});
            assert.equal(outFact.value.hasDynamicFields, true);
            assert.ok(
              outFact.gaps.some(
                (g) =>
                  g.kind === "unresolved-schema" && g.at?.line === targetLine,
              ),
              `${schemaName} output gaps: ${JSON.stringify(outFact.gaps)}`,
            );

            assert.ok(
              outFact.explainTrace?.localGaps?.some(
                (lg) =>
                  lg.status === "unresolved" &&
                  lg.gap.kind === "unresolved-schema" &&
                  lg.gap.at?.line === targetLine,
              ),
              `${schemaName} localGaps: ${JSON.stringify(outFact.explainTrace?.localGaps)}`,
            );
          }

          const andIn = inputByName("andNullableOverUnknown")!;
          const andOut = outputByName("andNullableOverUnknown")!;
          assert.deepEqual(andIn.value.requiredFields, ["id"]);
          assert.equal(andIn.value.fields.age, undefined);
          assert.equal(andIn.value.hasDynamicFields, true);
          assert.ok(
            andIn.gaps.some(
              (g) => g.kind === "unresolved-schema-field" && g.at?.line === 16,
            ),
            JSON.stringify(andIn.gaps),
          );
          assert.deepEqual(andOut.value.presentFields, ["id"]);
          assert.equal(andOut.value.fields.age, undefined);
          assert.equal(andOut.value.hasDynamicFields, true);
          assert.ok(
            andOut.gaps.some(
              (g) => g.kind === "unresolved-schema-field" && g.at?.line === 16,
            ),
            JSON.stringify(andOut.gaps),
          );

          assert.ok(
            andIn.explainTrace?.localGaps?.some(
              (lg) =>
                lg.status === "unresolved" &&
                lg.field === "age" &&
                lg.gap.kind === "unresolved-schema-field" &&
                lg.gap.at?.line === 16,
            ),
            JSON.stringify(andIn.explainTrace?.localGaps),
          );
        } finally {
          await session.close();
        }
      },
      { linkZod: true },
    );
  });
});
