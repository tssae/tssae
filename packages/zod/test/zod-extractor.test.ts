import assert from "node:assert/strict";
import { readFileSync, renameSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  installPackedWorkspacePackages,
  linkNodeModulesPackage,
  withTempHost,
  type PreparedTempHost,
} from "@tssae/test-hosts";
import {
  buildValidatorInputFields,
  extractZodFieldModifiers,
  extractZodInputFields,
  extractZodOutputFields,
  zodValidator,
  type ValidatorFieldExtractor,
  type ValidatorFieldModifierResult,
  type ValidatorInputFieldsResult,
  type ValidatorOutputFieldsResult,
} from "@tssae/zod";
import {
  TssaeError,
  evidenceOf,
  extractBasisEvidence,
  openProject,
  type Gap,
  type ProjectContext,
} from "tssae";

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

void describe("@tssae/zod", () => {
  void it("extracts basic z.object schemas and .parse/.safeParse/.parseAsync/.safeParseAsync calls while ignoring non-Zod .parse", async () => {
    await withZodHost("adapters/zod-basic-calls", async (host) => {
      await host.typecheck();

      let inputResults: readonly ValidatorInputFieldsResult[] = [];
      let outputResults: readonly ValidatorOutputFieldsResult[] = [];

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project();
        inputResults = await extractZodInputFields(ctx);
        outputResults = await extractZodOutputFields(ctx);
      } finally {
        await session.close();
      }

      const calls = inputResults.filter((r) => r.siteKind === "call");
      assert.equal(calls.length, 4);
      const methods = calls
        .map((c) => c.validatorMethod ?? "")
        .toSorted((a, b) => (a < b ? -1 : a > b ? 1 : 0));
      assert.deepEqual(methods, [
        "parse",
        "parseAsync",
        "safeParse",
        "safeParseAsync",
      ]);
      for (const callRes of calls) {
        assert.deepEqual(callRes.requiredFields, ["age", "id"]);
        assert.deepEqual(callRes.optionalFields, ["bio"]);
        assert.deepEqual(callRes.defaultedFields, []);
        assert.equal(callRes.hasDynamicFields, false);
        assert.ok(
          callRes.evidence.some(
            (ev) => ev.at?.file === "src/handlers.ts" && ev.at.line === 3,
          ),
        );
        assert.ok(evidenceOf(callRes).length > 0);
        assert.ok(extractBasisEvidence([callRes]).length > 0);
      }

      const schemas = inputResults.filter((r) => r.siteKind === "schema");
      assert.equal(schemas.length, 1);
      assert.equal(schemas[0]!.schemaName, "userSchema");
      assert.deepEqual(schemas[0]!.requiredFields, ["age", "id"]);

      const outCalls = outputResults.filter((r) => r.siteKind === "call");
      assert.equal(outCalls.length, 4);
      assert.deepEqual(outCalls[0]!.presentFields, ["age", "id"]);
      assert.deepEqual(outCalls[0]!.optionalFields, ["bio"]);
    });
  });

  void it("distinguishes .nullable(), .optional(), .nullish(), v3 .default(), v4 .prefault()/.catch(), and unwraps outer chains", async () => {
    await withZodHost("adapters/zod-modifiers", async (host) => {
      await host.typecheck();

      let inputResults: readonly ValidatorInputFieldsResult[] = [];
      let outputResults: readonly ValidatorOutputFieldsResult[] = [];

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project();
        inputResults = await extractZodInputFields(ctx, { scope: "calls" });
        outputResults = await extractZodOutputFields(ctx, { scope: "calls" });
      } finally {
        await session.close();
      }

      const accountIn = inputResults.find(
        (r) => r.schemaName === "accountSchema",
      );
      assert.ok(accountIn);
      assert.deepEqual(accountIn.requiredFields, ["homepageUrl", "id"]);
      assert.equal(accountIn.fields.homepageUrl?.nullable, true);
      assert.deepEqual(accountIn.defaultedFields, [
        "fallbackTheme",
        "password",
        "plan",
        "timezone",
      ]);
      assert.deepEqual(accountIn.optionalFields, [
        "customTheme",
        "fallbackTheme",
        "nickname",
        "password",
        "plan",
        "timezone",
      ]);

      const accountOut = outputResults.find(
        (r) => r.schemaName === "accountSchema",
      );
      assert.ok(accountOut);
      assert.deepEqual(accountOut.presentFields, [
        "fallbackTheme",
        "homepageUrl",
        "id",
        "password",
        "plan",
        "timezone",
      ]);
      assert.deepEqual(accountOut.optionalFields, ["customTheme", "nickname"]);
      assert.deepEqual(accountOut.defaultedFields, [
        "fallbackTheme",
        "password",
        "plan",
        "timezone",
      ]);

      const pipedIn = inputResults.find((r) => r.schemaName === "pipedSchema");
      assert.ok(pipedIn);
      assert.deepEqual(pipedIn.requiredFields, ["rawId"]);
      assert.deepEqual(pipedIn.defaultedFields, ["role"]);

      const pipedOut = outputResults.find(
        (r) => r.schemaName === "pipedSchema",
      );
      assert.ok(pipedOut);
      assert.deepEqual(pipedOut.presentFields, ["rawId", "role"]);
    });
  });

  void it("resolves .extend, .merge, .pick, .omit, .partial, .required across files and invalidates cache after session.update()", async () => {
    await withZodHost("adapters/zod-compositions", async (host) => {
      await host.typecheck();

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });

      try {
        const staleCtx = session.project();
        let callInputs = await extractZodInputFields(staleCtx, {
          scope: "calls",
        });
        const bySchema = (name: string) =>
          callInputs.find((r) => r.schemaName === name);

        assert.deepEqual(bySchema("extendedPage")?.requiredFields, [
          "customDomain",
          "id",
          "secret",
          "slug",
          "title",
        ]);
        assert.deepEqual(bySchema("extendedPage")?.defaultedFields, [
          "locale",
          "password",
        ]);
        assert.ok(
          bySchema("extendedPage")?.evidence.some(
            (ev) => ev.at?.file === "src/schemas.ts",
          ),
        );

        assert.deepEqual(bySchema("pickedGate")?.requiredFields, [
          "slug",
          "title",
        ]);
        assert.deepEqual(bySchema("omittedPage")?.requiredFields, []);
        assert.deepEqual(bySchema("omittedPage")?.defaultedFields, ["locale"]);
        assert.deepEqual(bySchema("partialGate")?.requiredFields, []);
        assert.deepEqual(bySchema("partialGate")?.optionalFields, [
          "slug",
          "title",
        ]);
        assert.deepEqual(bySchema("requiredAgain")?.requiredFields, ["slug"]);
        assert.deepEqual(bySchema("requiredAgain")?.optionalFields, ["title"]);

        host.replaceText(
          "src/schemas.ts",
          "slug: z.string(),",
          "slug: z.string().optional(),",
        );
        await session.update({ changed: ["src/schemas.ts"] });

        assert.throws(
          () => staleCtx.recordGaps([]),
          (err: unknown) =>
            err instanceof TssaeError && err.code === "STALE_SNAPSHOT",
        );

        callInputs = await extractZodInputFields(session.project(), {
          scope: "calls",
        });

        assert.deepEqual(bySchema("pickedGate")?.requiredFields, ["title"]);
        assert.deepEqual(bySchema("pickedGate")?.optionalFields, ["slug"]);
      } finally {
        await session.close();
      }
    });
  });

  void it("preserves known deterministic fields and emits location-aware Gaps for dynamic constructs", async () => {
    await withZodHost("adapters/zod-dynamic", async (host) => {
      await host.typecheck();

      const recordedGaps: Gap[] = [];
      let inputResults: readonly ValidatorInputFieldsResult[] = [];

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project(undefined, {
          observer: {
            onGaps: (gaps) => recordedGaps.push(...gaps),
          },
        });
        inputResults = await extractZodInputFields(ctx, { scope: "calls" });
      } finally {
        await session.close();
      }

      const mixed = inputResults.find((r) => r.schemaName === "mixedSchema");
      assert.ok(mixed);
      assert.equal(mixed.hasDynamicFields, true);
      assert.deepEqual(mixed.requiredFields, ["extra", "id", "meta"]);
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

      const dict = inputResults.find((r) => r.schemaName === "dictSchema");
      assert.ok(dict);
      assert.equal(dict.hasDynamicFields, true);
      assert.ok(
        dict.gaps.some(
          (g) => g.kind === "dynamic-schema-shape" && g.at?.line === 14,
        ),
      );
      assert.ok(recordedGaps.length >= 5);
    });
  });

  void it("caches snapshot extractions per ProjectSummary with zero RPCs on repeated queries", async () => {
    await withZodHost("adapters/zod-lazy-cache", async (host) => {
      await host.typecheck();

      let rpcCount = 0;
      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project(undefined, {
          observer: {
            onRpcMetrics: () => {
              rpcCount += 1;
            },
          },
        });
        const firstInputs = await extractZodInputFields(ctx, {
          scope: "calls",
        });
        const rpcsAfterFirst = rpcCount;
        assert.ok(rpcsAfterFirst > 0);

        const secondInputs = await extractZodInputFields(ctx, {
          scope: "calls",
        });
        const secondOutputs = await extractZodOutputFields(ctx, {
          scope: "calls",
        });
        assert.equal(rpcCount, rpcsAfterFirst);
        assert.equal(firstInputs.length, 1);
        assert.equal(secondInputs.length, 1);
        assert.equal(secondOutputs.length, 1);
        assert.equal(firstInputs[0]!.providerId, "zod");
        assert.equal(secondInputs[0], firstInputs[0]);
      } finally {
        await session.close();
      }
    });
  });

  void it("extracts real zod v4 schemas and swaps seamlessly with a custom Valibot ValidatorFieldExtractor", async () => {
    const customValibotExtractor: ValidatorFieldExtractor = {
      id: "custom-valibot",
      extractInputFields: async (ctx) => {
        const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
        const results: ValidatorInputFieldsResult[] = [];
        for (const call of calls) {
          if (!call.text.startsWith("v.parse(")) {
            continue;
          }
          results.push(
            buildValidatorInputFields({
              siteKind: "call",
              validatorMethod: "validate",
              schemaName: "createUserSchema",
              target: call.ref,
              at: call.location,
              providerId: "custom-valibot",
              fields: {
                age: {
                  name: "age",
                  requirement: "required",
                  nullable: false,
                  evidence: [
                    {
                      kind: "source",
                      message: "Valibot schema requires age",
                      at: call.location,
                    },
                  ],
                },
                email: {
                  name: "email",
                  requirement: "required",
                  nullable: false,
                  evidence: [
                    {
                      kind: "source",
                      message: "Valibot schema requires email",
                      at: call.location,
                    },
                  ],
                },
              },
              hasDynamicFields: false,
              evidence: [
                {
                  kind: "source",
                  message: "Valibot v.parse call site",
                  at: call.location,
                },
              ],
              gaps: [],
            }),
          );
        }
        return results;
      },
      extractOutputFields: async () => [],
    };

    const queryAgeInputs = async (
      ctx: ProjectContext,
      extractor: ValidatorFieldExtractor,
    ) => {
      const inputs = await extractor.extractInputFields(ctx, {
        scope: "calls",
      });
      return inputs.filter((item) => item.requiredFields.includes("age"));
    };

    await withZodHost(
      "adapters/zod-real-v4",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        let inputResults: readonly ValidatorInputFieldsResult[];
        let outputResults: readonly ValidatorOutputFieldsResult[];
        try {
          const ctx = session.project();
          inputResults = await zodValidator.extractInputFields(ctx, {
            scope: "calls",
          });
          outputResults = await zodValidator.extractOutputFields(ctx, {
            scope: "calls",
          });
        } finally {
          await session.close();
        }

        assert.equal(inputResults.length, 1);
        assert.deepEqual(inputResults[0]!.requiredFields, [
          "id",
          "nullableKey",
        ]);
        assert.deepEqual(inputResults[0]!.defaultedFields, [
          "caught",
          "defaulted",
          "prefaulted",
        ]);
        assert.deepEqual(outputResults[0]!.presentFields, [
          "caught",
          "defaulted",
          "id",
          "nullableKey",
          "prefaulted",
        ]);
        assert.deepEqual(outputResults[0]!.optionalFields, ["optKey"]);
      },
      { linkZod: true },
    );

    await withZodHost("adapters/zod-provider-stub", async (zodHost) => {
      await zodHost.typecheck();
      const session = await openProject({
        cwd: zodHost.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const hits = await queryAgeInputs(session.project(), zodValidator);
        assert.equal(hits.length, 1);
        assert.equal(hits[0]!.providerId, "zod");
        assert.equal(hits[0]!.at.file, "src/app.ts");
        assert.equal(hits[0]!.at.line, 9);
      } finally {
        await session.close();
      }
    });

    await withZodHost("adapters/zod-provider-valibot", async (valibotHost) => {
      await valibotHost.typecheck();
      const session = await openProject({
        cwd: valibotHost.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const hits = await queryAgeInputs(
          session.project(),
          customValibotExtractor,
        );
        assert.equal(hits.length, 1);
        assert.equal(hits[0]!.providerId, "custom-valibot");
        assert.equal(hits[0]!.at.file, "src/app.ts");
        assert.equal(hits[0]!.at.line, 9);
        assert.ok(
          extractBasisEvidence([hits[0]!]).some((e) =>
            e.message.includes("Valibot"),
          ),
        );
      } finally {
        await session.close();
      }
    });
  });

  void it("retains explicit .extend/.default/.pick conclusions on standalone drizzle-zod schemas while surfacing unresolved-schema", async () => {
    await withZodHost(
      "adapters/zod-drizzle-standalone",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        let inputResults: readonly ValidatorInputFieldsResult[];
        let outputResults: readonly ValidatorOutputFieldsResult[];
        try {
          const ctx = session.project();
          inputResults = await extractZodInputFields(ctx, { scope: "calls" });
          outputResults = await extractZodOutputFields(ctx, { scope: "calls" });
        } finally {
          await session.close();
        }

        assert.equal(inputResults.length, 1);
        const gateIn = inputResults[0]!;
        assert.equal(gateIn.schemaName, "gateFieldsSchema");
        assert.deepEqual(gateIn.requiredFields, []);
        assert.deepEqual(gateIn.defaultedFields, ["accessType", "password"]);
        assert.deepEqual(gateIn.optionalFields, ["accessType", "password"]);
        assert.equal(gateIn.hasDynamicFields, true);
        assert.ok(
          gateIn.gaps.some(
            (gap) =>
              gap.kind === "unresolved-schema" &&
              gap.at?.file === "packages/db/src/validation.ts" &&
              gap.at.line === 6,
          ),
        );
        assert.ok(
          gateIn.evidence.some(
            (ev) => ev.at?.file === "packages/db/src/validation.ts",
          ),
        );

        const gateOut = outputResults[0]!;
        assert.deepEqual(gateOut.presentFields, ["accessType", "password"]);
        assert.deepEqual(gateOut.defaultedFields, ["accessType", "password"]);
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

  void it("tracks runtime field validators, clears superseded gaps on .extend(), and extracts field modifiers", async () => {
    await withZodHost(
      "adapters/zod-runtime-unknowns",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        let inputResults: readonly ValidatorInputFieldsResult[];
        let outputResults: readonly ValidatorOutputFieldsResult[];
        let modifiers: readonly ValidatorFieldModifierResult[];
        try {
          const ctx = session.project();
          inputResults = await extractZodInputFields(ctx, { scope: "calls" });
          outputResults = await extractZodOutputFields(ctx, { scope: "calls" });
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
          modifiers = await extractZodFieldModifiers(ctx, initRefs);
        } finally {
          await session.close();
        }

        const partialIn = inputResults.find(
          (r) => r.schemaName === "partialUnknownSchema",
        );
        assert.ok(partialIn);
        assert.equal(partialIn.hasDynamicFields, true);
        assert.deepEqual(partialIn.requiredFields, ["id"]);
        assert.equal(partialIn.fields.age, undefined);
        assert.equal(partialIn.fields.extra, undefined);
        assert.equal(partialIn.gaps.length, 2);
        assert.ok(
          partialIn.gaps.every((g) => g.kind === "unresolved-schema-field"),
        );

        const overriddenIn = inputResults.find(
          (r) => r.schemaName === "overriddenSchema",
        );
        const overriddenOut = outputResults.find(
          (r) => r.schemaName === "overriddenSchema",
        );
        assert.ok(overriddenIn && overriddenOut);
        assert.equal(overriddenIn.hasDynamicFields, false);
        assert.deepEqual(overriddenIn.gaps, []);
        assert.deepEqual(overriddenIn.requiredFields, ["id"]);
        assert.deepEqual(overriddenIn.defaultedFields, ["age"]);
        assert.deepEqual(overriddenIn.optionalFields, ["age", "extra"]);
        assert.deepEqual(overriddenOut.presentFields, ["age", "id"]);
        assert.deepEqual(overriddenOut.optionalFields, ["extra"]);

        assert.equal(modifiers.length, 6);
        assert.equal(modifiers.filter((m) => m.resolved).length, 5);
        const unresolved = modifiers.filter((m) => !m.resolved);
        assert.equal(unresolved.length, 1);
        assert.ok(
          unresolved[0]!.gaps.some((g) => g.kind === "unresolved-schema-field"),
        );
        assert.ok(
          modifiers.some(
            (m) =>
              m.resolved &&
              m.mode === "callback" &&
              m.requirement === "defaulted" &&
              m.fromDefault &&
              !m.nullabilityExplicit,
          ),
        );
        assert.ok(
          modifiers.some(
            (m) =>
              m.resolved &&
              m.mode === "callback" &&
              m.requirement === "required" &&
              m.nullable === true &&
              m.nullabilityExplicit,
          ),
        );
      },
      { linkZod: true },
    );
  });

  void it("enforces sound input vs output semantics for .default(21), .and(), z.intersection(), .pipe(), and z.pipe()", async () => {
    await withZodHost(
      "adapters/zod-combinators",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });

        try {
          const ctx = session.project();
          const inputResults = await extractZodInputFields(ctx, {
            scope: "calls",
          });
          const outputResults = await extractZodOutputFields(ctx, {
            scope: "calls",
          });

          const inputByName = (name: string) =>
            inputResults.find((r) => r.schemaName === name);
          const outputByName = (name: string) =>
            outputResults.find((r) => r.schemaName === name);

          const modIn = inputByName("modifierOverrideSchema")!;
          const modOut = outputByName("modifierOverrideSchema")!;
          assert.deepEqual(modIn.gaps, []);
          assert.deepEqual(modIn.defaultedFields, ["age"]);
          assert.equal(modIn.hasDynamicFields, false);
          assert.deepEqual(modOut.gaps, []);
          assert.deepEqual(modOut.presentFields, ["age", "id"]);
          assert.deepEqual(modOut.defaultedFields, ["age"]);
          assert.equal(modOut.hasDynamicFields, false);

          const andDefIn = inputByName("andDefaultedSchema")!;
          const andDefOut = outputByName("andDefaultedSchema")!;
          assert.deepEqual(andDefIn.requiredFields, ["id"]);
          assert.deepEqual(andDefIn.optionalFields, []);
          assert.deepEqual(andDefIn.defaultedFields, []);
          assert.equal(andDefIn.fields.age, undefined);
          assert.equal(andDefIn.hasDynamicFields, true);
          assert.ok(
            andDefIn.gaps.some(
              (g) => g.kind === "unresolved-schema-field" && g.at?.line === 12,
            ),
          );
          assert.deepEqual(andDefOut.gaps, []);
          assert.deepEqual(andDefOut.presentFields, ["age", "id"]);
          assert.deepEqual(andDefOut.defaultedFields, ["age"]);
          assert.equal(andDefOut.hasDynamicFields, false);

          const andReqIn = inputByName("andRequiredSchema")!;
          const andReqOut = outputByName("andRequiredSchema")!;
          assert.deepEqual(andReqIn.gaps, []);
          assert.deepEqual(andReqIn.requiredFields, ["age", "id"]);
          assert.equal(andReqIn.hasDynamicFields, false);
          assert.deepEqual(andReqOut.gaps, []);
          assert.deepEqual(andReqOut.presentFields, ["age", "id"]);
          assert.equal(andReqOut.hasDynamicFields, false);

          for (const pipeName of ["pipeOverrideSchema", "staticPipeSchema"]) {
            const pipeIn = inputByName(pipeName)!;
            const pipeOut = outputByName(pipeName)!;
            assert.deepEqual(pipeIn.requiredFields, ["id"]);
            assert.deepEqual(pipeIn.defaultedFields, []);
            assert.equal(pipeIn.fields.age, undefined);
            assert.equal(pipeIn.hasDynamicFields, true);
            assert.ok(
              pipeIn.gaps.some(
                (g) =>
                  g.kind === "unresolved-schema-field" && g.at?.line === 12,
              ),
            );
            assert.deepEqual(pipeOut.gaps, []);
            assert.deepEqual(pipeOut.presentFields, ["age", "id"]);
            assert.deepEqual(pipeOut.defaultedFields, ["age"]);
            assert.equal(pipeOut.hasDynamicFields, false);
          }

          const zIntIn = inputByName("staticIntersectionSchema")!;
          const zIntOut = outputByName("staticIntersectionSchema")!;
          assert.deepEqual(zIntIn.requiredFields, ["id"]);
          assert.deepEqual(zIntIn.defaultedFields, []);
          assert.equal(zIntIn.fields.age, undefined);
          assert.equal(zIntIn.hasDynamicFields, true);
          assert.ok(
            zIntIn.gaps.some(
              (g) => g.kind === "unresolved-schema-field" && g.at?.line === 12,
            ),
          );
          assert.deepEqual(zIntOut.gaps, []);
          assert.deepEqual(zIntOut.presentFields, ["age", "id"]);
          assert.deepEqual(zIntOut.defaultedFields, ["age"]);
          assert.equal(zIntOut.hasDynamicFields, false);
        } finally {
          await session.close();
        }
      },
      { linkZod: true },
    );
  });

  void it("replaces known base fields when overridden by unknown field validators and resolves chained overrides", async () => {
    await withZodHost(
      "adapters/zod-unknown-overrides",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });

        try {
          const ctx = session.project();
          const inputResults = await extractZodInputFields(ctx, {
            scope: "calls",
          });
          const outputResults = await extractZodOutputFields(ctx, {
            scope: "calls",
          });

          const inputByName = (name: string) =>
            inputResults.find((r) => r.schemaName === name);
          const outputByName = (name: string) =>
            outputResults.find((r) => r.schemaName === name);

          for (const [schemaName, expectedGapLine] of [
            ["extendUnknownSchema", 11],
            ["mergeUnknownSchema", 19],
            ["spreadUnknownSchema", 23],
          ] as const) {
            const inRes = inputByName(schemaName)!;
            const outRes = outputByName(schemaName)!;
            assert.deepEqual(inRes.requiredFields, ["id"]);
            assert.equal(inRes.fields.age, undefined);
            assert.equal(inRes.hasDynamicFields, true);
            assert.ok(
              inRes.gaps.some(
                (g) =>
                  g.kind === "unresolved-schema-field" &&
                  g.at?.line === expectedGapLine,
              ),
            );

            assert.deepEqual(outRes.presentFields, ["id"]);
            assert.equal(outRes.fields.age, undefined);
            assert.equal(outRes.hasDynamicFields, true);
            assert.ok(
              outRes.gaps.some(
                (g) =>
                  g.kind === "unresolved-schema-field" &&
                  g.at?.line === expectedGapLine,
              ),
            );
          }

          for (const schemaName of [
            "extendReResolvedSchema",
            "spreadReResolvedSchema",
          ] as const) {
            const inRes = inputByName(schemaName)!;
            const outRes = outputByName(schemaName)!;
            assert.deepEqual(inRes.gaps, []);
            assert.deepEqual(inRes.requiredFields, ["id"]);
            assert.deepEqual(inRes.optionalFields, ["age"]);
            assert.deepEqual(inRes.defaultedFields, ["age"]);
            assert.equal(inRes.hasDynamicFields, false);

            assert.deepEqual(outRes.gaps, []);
            assert.deepEqual(outRes.presentFields, ["age", "id"]);
            assert.deepEqual(outRes.defaultedFields, ["age"]);
            assert.equal(outRes.hasDynamicFields, false);
          }
        } finally {
          await session.close();
        }
      },
      { linkZod: true },
    );
  });

  void it("replaces output with unresolved-schema for unrecognized .pipe()/z.pipe() targets and preserves unknown in nullable .and()", async () => {
    await withZodHost(
      "adapters/zod-pipe-and-nullable",
      async (host) => {
        await host.typecheck();

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });

        try {
          const ctx = session.project();
          const inputResults = await extractZodInputFields(ctx, {
            scope: "calls",
          });
          const outputResults = await extractZodOutputFields(ctx, {
            scope: "calls",
          });

          const inputByName = (name: string) =>
            inputResults.find((r) => r.schemaName === name);
          const outputByName = (name: string) =>
            outputResults.find((r) => r.schemaName === name);

          for (const [schemaName, targetLine] of [
            ["methodPipeUnrecognized", 8],
            ["staticPipeUnrecognized", 12],
          ] as const) {
            const inRes = inputByName(schemaName)!;
            const outRes = outputByName(schemaName)!;
            assert.deepEqual(inRes.gaps, []);
            assert.deepEqual(inRes.requiredFields, ["rawId"]);
            assert.equal(inRes.hasDynamicFields, false);

            assert.deepEqual(outRes.presentFields, []);
            assert.deepEqual(outRes.optionalFields, []);
            assert.deepEqual(outRes.defaultedFields, []);
            assert.deepEqual(outRes.fields, {});
            assert.equal(outRes.hasDynamicFields, true);
            assert.ok(
              outRes.gaps.some(
                (g) =>
                  g.kind === "unresolved-schema" && g.at?.line === targetLine,
              ),
            );
          }

          const andIn = inputByName("andNullableOverUnknown")!;
          const andOut = outputByName("andNullableOverUnknown")!;
          assert.deepEqual(andIn.requiredFields, ["id"]);
          assert.equal(andIn.fields.age, undefined);
          assert.equal(andIn.hasDynamicFields, true);
          assert.ok(
            andIn.gaps.some(
              (g) => g.kind === "unresolved-schema-field" && g.at?.line === 16,
            ),
          );
          assert.deepEqual(andOut.presentFields, ["id"]);
          assert.equal(andOut.fields.age, undefined);
          assert.equal(andOut.hasDynamicFields, true);
          assert.ok(
            andOut.gaps.some(
              (g) => g.kind === "unresolved-schema-field" && g.at?.line === 16,
            ),
          );
        } finally {
          await session.close();
        }
      },
      { linkZod: true },
    );
  });

  void it("compiles an external TypeScript consumer against built tssae and @tssae/zod .d.ts", async () => {
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
            ["tssae", "@tssae/zod"],
            { cwd: packageRoot },
          );
        },
      },
      async (host) => {
        await host.typecheck();
      },
    );
  });
});
