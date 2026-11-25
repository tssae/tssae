import assert from "node:assert/strict";
import { dirname } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
  type ValidatorInputFieldsFactValue,
} from "@verule/adapter-zod";
import {
  VeruleExecutionError,
  defineAdapter,
  defineConfig,
  defineFactProvider,
  defineRule,
  runCheck,
  type FactResult,
} from "@verule/sdk";
import { withTempHost, type PreparedTempHost } from "@verule/test-hosts";

const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));

function withZodProviderHost(
  hostName: string,
  callback: (host: PreparedTempHost) => Promise<void>,
): Promise<void> {
  return withTempHost(
    {
      host: hostName,
      packageLinks: {
        "@verule/adapter-zod": packageRoot,
      },
    },
    callback,
  );
}

const customValibotAdapter = defineAdapter({
  id: "custom-valibot",
  provides: [
    defineFactProvider(ValidatorInputFieldsFact, async (ctx) => {
      const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
      const results: {
        readonly subject: (typeof calls)[number]["ref"];
        readonly value: ValidatorInputFieldsFactValue;
        readonly evidence: readonly [
          {
            readonly kind: "source";
            readonly message: string;
            readonly at: (typeof calls)[number]["location"];
          },
        ];
      }[] = [];
      for (const call of calls) {
        if (!call.text.startsWith("v.parse(")) continue;
        results.push({
          subject: call.ref,
          value: {
            siteKind: "call",
            validatorMethod: "validate",
            schemaName: "createUserSchema",
            requiredFields: ["age", "email"],
            optionalFields: [],
            defaultedFields: [],
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
          },
          evidence: [
            {
              kind: "source",
              message: "Valibot v.parse call site",
              at: call.location,
            },
          ],
        });
      }
      return results;
    }),
  ],
});

void describe("@verule/adapter-zod provider replacement, complementation, and conflicts", () => {
  void it("runs the same business rule with the package-loaded Zod adapter or a custom validator adapter", async () => {
    // Shared business rule written once against ValidatorInputFieldsFact:
    const requireAgeRule = defineRule({
      id: "require-age-input",
      run: async (ctx) => {
        const inputs = await ctx.facts(ValidatorInputFieldsFact, {
          condition: { scope: "calls" },
        });
        for (const item of inputs) {
          if (!item.value.requiredFields.includes("age")) continue;
          const syntaxRows = await ctx.syntaxOf([item.subject], {
            select: { location: true },
          });
          const at = syntaxRows[0]?.location ?? item.evidence[0]?.at;
          if (!at) continue;
          ctx.report({
            at,
            message: `Validator requires "age" via [${item.providers.join(", ")}]`,
            findingKey: "requires-age",
            basis: [item],
          });
        }
      },
    });

    await withZodProviderHost("adapters/zod-provider-stub", async (zodHost) => {
      await zodHost.typecheck();

      const zodReport = await runCheck({
        cwd: zodHost.rootDir,
        config: defineConfig({
          tsconfig: "tsconfig.json",
          adapters: ["@verule/adapter-zod"],
          rules: [requireAgeRule],
        }),
      });
      assert.equal(zodReport.status, "findings");
      assert.equal(zodReport.diagnostics.length, 1);
      assert.match(zodReport.diagnostics[0]!.message, /\[zod\]/u);
      assert.equal(zodReport.diagnostics[0]!.at.file, "src/app.ts");
      assert.equal(zodReport.diagnostics[0]!.at.line, 9);
    });

    await withZodProviderHost(
      "adapters/zod-provider-valibot",
      async (valibotHost) => {
        await valibotHost.typecheck();

        const valibotReport = await runCheck({
          cwd: valibotHost.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: [customValibotAdapter],
            rules: [requireAgeRule],
          }),
        });
        assert.equal(valibotReport.status, "findings");
        assert.equal(valibotReport.diagnostics.length, 1);
        assert.match(
          valibotReport.diagnostics[0]!.message,
          /\[custom-valibot\]/u,
        );
        assert.equal(valibotReport.diagnostics[0]!.at.file, "src/app.ts");
        assert.equal(valibotReport.diagnostics[0]!.at.line, 9);
        assert.ok(
          valibotReport.diagnostics[0]!.evidence.some((e) =>
            e.message.includes("Valibot"),
          ),
        );
      },
    );
  });

  void it("complements partial ValidatorInputFieldsFact contributions and raises load-order-independent FACT_CONFLICT on contradictory field requirements", async () => {
    await withZodProviderHost(
      "adapters/zod-provider-conflict",
      async (host) => {
        await host.typecheck();

        const partialValidatorA = defineAdapter({
          id: "validator-part-a",
          provides: [
            defineFactProvider(ValidatorInputFieldsFact, async (ctx) => {
              const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
              const target = calls.find((c) =>
                c.text.startsWith("validateUser("),
              );
              if (!target) return [];
              return [
                {
                  subject: target.ref,
                  value: {
                    siteKind: "call" as const,
                    schemaName: "UserInput",
                    requiredFields: ["id"],
                    optionalFields: [],
                    defaultedFields: [],
                    fields: {
                      id: {
                        name: "id",
                        requirement: "required" as const,
                        nullable: false,
                        evidence: [
                          {
                            kind: "source" as const,
                            message: "part-a resolved field id as required",
                            at: target.location,
                          },
                        ],
                      },
                    },
                    hasDynamicFields: true,
                  },
                  evidence: [
                    {
                      kind: "source",
                      message: "part-a inspected partial schema",
                      at: target.location,
                    },
                  ],
                  gaps: [
                    {
                      kind: "dynamic-schema-spread",
                      message: "part-a saw dynamic spread for remaining fields",
                      at: target.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        const partialValidatorB = defineAdapter({
          id: "validator-part-b",
          provides: [
            defineFactProvider(ValidatorInputFieldsFact, async (ctx) => {
              const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
              const target = calls.find((c) =>
                c.text.startsWith("validateUser("),
              );
              if (!target) return [];
              return [
                {
                  subject: target.ref,
                  value: {
                    siteKind: "call" as const,
                    schemaName: "UserInput",
                    requiredFields: ["age"],
                    optionalFields: ["bio"],
                    defaultedFields: [],
                    fields: {
                      age: {
                        name: "age",
                        requirement: "required" as const,
                        nullable: false,
                        evidence: [
                          {
                            kind: "source" as const,
                            message: "part-b resolved field age as required",
                            at: target.location,
                          },
                        ],
                      },
                      bio: {
                        name: "bio",
                        requirement: "optional" as const,
                        nullable: false,
                        evidence: [
                          {
                            kind: "source" as const,
                            message: "part-b resolved field bio as optional",
                            at: target.location,
                          },
                        ],
                      },
                    },
                    hasDynamicFields: false,
                  },
                  evidence: [
                    {
                      kind: "source",
                      message: "part-b completed remaining fields",
                      at: target.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        const conflictingValidatorC = defineAdapter({
          id: "validator-conflict-c",
          provides: [
            defineFactProvider(ValidatorInputFieldsFact, async (ctx) => {
              const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
              const target = calls.find((c) =>
                c.text.startsWith("validateUser("),
              );
              if (!target) return [];
              return [
                {
                  subject: target.ref,
                  value: {
                    siteKind: "call" as const,
                    schemaName: "UserInput",
                    requiredFields: [],
                    optionalFields: ["age"],
                    defaultedFields: [],
                    fields: {
                      age: {
                        name: "age",
                        requirement: "optional" as const,
                        nullable: false,
                        evidence: [
                          {
                            kind: "source" as const,
                            message:
                              "conflict-c resolved field age as optional",
                            at: target.location,
                          },
                        ],
                      },
                    },
                    hasDynamicFields: false,
                  },
                  evidence: [
                    {
                      kind: "source",
                      message: "conflict-c schema evidence",
                      at: target.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        let mergedForward: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorInputFieldsFactValue
        >[] = [];
        let mergedReverse: readonly FactResult<
          "syntax" | "call" | "declaration",
          ValidatorInputFieldsFactValue
        >[] = [];

        const makeRule = (
          capture: (
            items: readonly FactResult<
              "syntax" | "call" | "declaration",
              ValidatorInputFieldsFactValue
            >[],
          ) => void,
        ) =>
          defineRule({
            id: "inspect-merged",
            run: async (ctx) => {
              capture(await ctx.facts(ValidatorInputFieldsFact));
            },
          });

        const repForward = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: [partialValidatorA, partialValidatorB],
            rules: [
              makeRule((items) => {
                mergedForward = items;
              }),
            ],
          }),
        });
        const repReverse = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: [partialValidatorB, partialValidatorA],
            rules: [
              makeRule((items) => {
                mergedReverse = items;
              }),
            ],
          }),
        });

        assert.equal(mergedForward.length, 1);
        assert.deepEqual(mergedForward[0]!.providers, [
          "validator-part-a",
          "validator-part-b",
        ]);
        assert.deepEqual(mergedForward[0]!.value.requiredFields, ["age", "id"]);
        assert.deepEqual(mergedForward[0]!.value.optionalFields, ["bio"]);
        assert.deepEqual(mergedForward[0]!.value, mergedReverse[0]!.value);
        assert.deepEqual(
          mergedForward[0]!.evidence,
          mergedReverse[0]!.evidence,
        );
        assert.deepEqual(mergedForward[0]!.gaps, mergedReverse[0]!.gaps);
        assert.deepEqual(repForward, repReverse);

        // Conflict between validator-part-b (age=required) and validator-conflict-c (age=optional)
        const conflictRule = defineRule({
          id: "detect-conflict",
          run: async (ctx) => {
            await ctx.facts(ValidatorInputFieldsFact);
          },
        });

        let errForward: unknown;
        try {
          await runCheck({
            cwd: host.rootDir,
            config: defineConfig({
              tsconfig: "tsconfig.json",
              adapters: [partialValidatorB, conflictingValidatorC],
              rules: [conflictRule],
            }),
          });
        } catch (err) {
          errForward = err;
        }

        let errReverse: unknown;
        try {
          await runCheck({
            cwd: host.rootDir,
            config: defineConfig({
              tsconfig: "tsconfig.json",
              adapters: [conflictingValidatorC, partialValidatorB],
              rules: [conflictRule],
            }),
          });
        } catch (err) {
          errReverse = err;
        }

        assert.ok(errForward instanceof VeruleExecutionError);
        assert.ok(errReverse instanceof VeruleExecutionError);
        assert.equal(errForward.code, "FACT_CONFLICT");
        assert.equal(errForward.message, errReverse.message);
        assert.deepEqual(errForward.conflict, errReverse.conflict);
        assert.ok(errForward.conflict);
        assert.deepEqual(
          errForward.conflict.parties.map((p) => p.providerId),
          ["validator-conflict-c", "validator-part-b"],
        );
        assert.deepEqual(
          errForward.conflict.parties.map((p) => p.conclusionSummary),
          [
            'field "age": optional (nullable=false)',
            'field "age": required (nullable=false)',
          ],
        );
        assert.match(
          errForward.message,
          /conflict-c resolved field age as optional/u,
        );
        assert.match(
          errForward.message,
          /part-b resolved field age as required/u,
        );
        // Unused import guard
        void ValidatorOutputFieldsFact;
      },
    );
  });
});
