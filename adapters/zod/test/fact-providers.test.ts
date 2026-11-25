import assert from "node:assert/strict";
import { dirname } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
  type ValidatorInputFieldsFactValue,
} from "@verule/adapter-zod";
import { withTempHost, type PreparedTempHost } from "@verule/test-hosts";
import {
  TssaeError,
  defineAdapter,
  defineFactProvider,
  openProject,
  type FactResult,
  type ProjectContext,
} from "tssae";

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
  void it("runs the same query logic with the package-loaded Zod adapter or a custom validator adapter", async () => {
    const queryAgeInputs = async (ctx: ProjectContext) => {
      const inputs = await ctx.facts(ValidatorInputFieldsFact, {
        condition: { scope: "calls" },
      });
      const hits = [];
      for (const item of inputs) {
        if (!item.value.requiredFields.includes("age")) continue;
        const syntaxRows = await ctx.syntaxOf([item.subject], {
          select: { location: true },
        });
        const at = syntaxRows[0]?.location ?? item.evidence[0]?.at;
        if (!at) continue;
        hits.push({ item, at });
      }
      return hits;
    };

    await withZodProviderHost("adapters/zod-provider-stub", async (zodHost) => {
      await zodHost.typecheck();

      const session = await openProject({
        cwd: zodHost.rootDir,
        tsconfig: "tsconfig.json",
        adapters: ["@verule/adapter-zod"],
      });
      const hits = await (async () => {
        try {
          return await queryAgeInputs(session.project());
        } finally {
          await session.close();
        }
      })();
      assert.equal(hits.length, 1);
      assert.deepEqual(hits[0]!.item.providers, ["zod"]);
      assert.equal(hits[0]!.at.file, "src/app.ts");
      assert.equal(hits[0]!.at.line, 9);
    });

    await withZodProviderHost(
      "adapters/zod-provider-valibot",
      async (valibotHost) => {
        await valibotHost.typecheck();

        const session = await openProject({
          cwd: valibotHost.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [customValibotAdapter],
        });
        const hits = await (async () => {
          try {
            return await queryAgeInputs(session.project());
          } finally {
            await session.close();
          }
        })();
        assert.equal(hits.length, 1);
        assert.deepEqual(hits[0]!.item.providers, ["custom-valibot"]);
        assert.equal(hits[0]!.at.file, "src/app.ts");
        assert.equal(hits[0]!.at.line, 9);
        assert.ok(
          hits[0]!.item.evidence.some((e) => e.message.includes("Valibot")),
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

        const forwardSession = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [partialValidatorA, partialValidatorB],
        });
        try {
          mergedForward = await forwardSession
            .project()
            .facts(ValidatorInputFieldsFact);
        } finally {
          await forwardSession.close();
        }

        const reverseSession = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [partialValidatorB, partialValidatorA],
        });
        try {
          mergedReverse = await reverseSession
            .project()
            .facts(ValidatorInputFieldsFact);
        } finally {
          await reverseSession.close();
        }

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

        let errForward: unknown;
        const conflictFwd = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [partialValidatorB, conflictingValidatorC],
        });
        try {
          await conflictFwd.project().facts(ValidatorInputFieldsFact);
        } catch (err) {
          errForward = err;
        } finally {
          await conflictFwd.close();
        }

        let errReverse: unknown;
        const conflictRev = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [conflictingValidatorC, partialValidatorB],
        });
        try {
          await conflictRev.project().facts(ValidatorInputFieldsFact);
        } catch (err) {
          errReverse = err;
        } finally {
          await conflictRev.close();
        }

        assert.ok(errForward instanceof TssaeError);
        assert.ok(errReverse instanceof TssaeError);
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
