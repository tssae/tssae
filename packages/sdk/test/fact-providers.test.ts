import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  VeruleExecutionError,
  defineAdapter,
  defineConfig,
  defineFactProvider,
  defineFactType,
  defineRule,
  openSession,
  runCheck,
  type FactResult,
} from "@verule/sdk";
import { withTempHost } from "@verule/test-hosts";

type EndpointAuthCondition = {
  readonly scope?: "all" | "exported";
};

type EndpointAuthFactValue = {
  readonly roles: readonly string[];
  readonly deniedRoles?: readonly string[];
  readonly allowAnonymous?: boolean;
  readonly hasDynamicRoles: boolean;
};

/**
 * Third-party FactType contract with explicit subjectKind, condition, field meanings,
 * and symmetric partial-merge / mutual-exclusion semantics.
 */
const ThirdPartyEndpointAuthFact = defineFactType<
  "call" | "syntax",
  EndpointAuthCondition | undefined,
  EndpointAuthFactValue
>({
  id: "thirdparty.endpoint.auth",
  description:
    "Authorization roles and anonymous-access guarantees extracted by third-party route adapters.",
  subjectKind: ["call", "syntax"],
  merge: (existing, incoming) => {
    if (
      existing.value.allowAnonymous !== undefined &&
      incoming.value.allowAnonymous !== undefined &&
      existing.value.allowAnonymous !== incoming.value.allowAnonymous
    ) {
      return {
        ok: false,
        conflict: {
          message: `Conflicting allowAnonymous conclusion (${String(existing.value.allowAnonymous)} vs ${String(incoming.value.allowAnonymous)})`,
          existingConclusion: `allowAnonymous=${String(existing.value.allowAnonymous)}`,
          incomingConclusion: `allowAnonymous=${String(incoming.value.allowAnonymous)}`,
          existingEvidence: existing.evidence,
          incomingEvidence: incoming.evidence,
        },
      };
    }

    const existingDenied = new Set(existing.value.deniedRoles ?? []);
    const incomingDenied = new Set(incoming.value.deniedRoles ?? []);
    const roleConflict =
      existing.value.roles.find((role) => incomingDenied.has(role)) ??
      incoming.value.roles.find((role) => existingDenied.has(role));
    if (roleConflict !== undefined) {
      return {
        ok: false,
        conflict: {
          message: `Conflicting requirement for role "${roleConflict}"`,
          existingConclusion: `roles=[${existing.value.roles.join(",")}], deniedRoles=[${(existing.value.deniedRoles ?? []).join(",")}]`,
          incomingConclusion: `roles=[${incoming.value.roles.join(",")}], deniedRoles=[${(incoming.value.deniedRoles ?? []).join(",")}]`,
          existingEvidence: existing.evidence,
          incomingEvidence: incoming.evidence,
        },
      };
    }

    const roles = [
      ...new Set([...existing.value.roles, ...incoming.value.roles]),
    ].toSorted();
    const deniedRoles = [
      ...new Set([
        ...(existing.value.deniedRoles ?? []),
        ...(incoming.value.deniedRoles ?? []),
      ]),
    ].toSorted();
    const allowAnonymous =
      existing.value.allowAnonymous ?? incoming.value.allowAnonymous;
    return {
      ok: true,
      value: {
        roles,
        ...(deniedRoles.length > 0 ? { deniedRoles } : {}),
        ...(allowAnonymous !== undefined ? { allowAnonymous } : {}),
        hasDynamicRoles:
          existing.value.hasDynamicRoles || incoming.value.hasDynamicRoles,
      },
    };
  },
});

const ThirdPartyRouteMethodFact = defineFactType<
  "syntax",
  undefined,
  { readonly method: "GET" | "POST" }
>({
  id: "thirdparty.route.method",
  subjectKind: "syntax",
});

void describe("@verule/sdk fact providers, replacement, and conflict semantics", () => {
  void it("complements partial facts and reports structured order-independent FACT_CONFLICT with both providers, conclusions, and evidence", async () => {
    await withTempHost(
      { host: "sdk-queries/fact-providers-conflicts" },
      async (host) => {
        await host.typecheck();

        const partialProviderA = defineAdapter({
          id: "auth-decorator-provider",
          provides: [
            defineFactProvider(ThirdPartyEndpointAuthFact, async (ctx) => {
              const calls = await ctx.findSyntax({
                kinds: ["CallExpression"],
              });
              const target = calls.find((c) =>
                c.text.startsWith("registerRoute("),
              );
              if (!target) return [];
              return [
                {
                  subject: target.ref,
                  value: {
                    roles: ["reader"],
                    hasDynamicRoles: true,
                  },
                  evidence: [
                    {
                      kind: "source",
                      message: "Decorator grants reader role",
                      at: target.location,
                    },
                  ],
                  gaps: [
                    {
                      kind: "dynamic-role-expression",
                      message:
                        "Additional role expression could not be resolved statically",
                      at: target.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        const partialProviderB = defineAdapter({
          id: "auth-policy-provider",
          provides: [
            defineFactProvider(ThirdPartyEndpointAuthFact, async (ctx) => {
              const calls = await ctx.findSyntax({
                kinds: ["CallExpression"],
              });
              const target = calls.find((c) =>
                c.text.startsWith("registerRoute("),
              );
              if (!target) return [];
              return [
                {
                  subject: target.ref,
                  value: {
                    roles: ["editor"],
                    allowAnonymous: false,
                    hasDynamicRoles: false,
                  },
                  evidence: [
                    {
                      kind: "note",
                      message:
                        "Policy requires editor role and disables anonymous access",
                      at: target.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        const conflictingProviderC = defineAdapter({
          id: "auth-override-provider",
          provides: [
            defineFactProvider(ThirdPartyEndpointAuthFact, async (ctx) => {
              const calls = await ctx.findSyntax({
                kinds: ["CallExpression"],
              });
              const target = calls.find((c) =>
                c.text.startsWith("registerRoute("),
              );
              if (!target) return [];
              return [
                {
                  subject: target.ref,
                  value: {
                    roles: [],
                    deniedRoles: ["reader"],
                    allowAnonymous: true,
                    hasDynamicRoles: false,
                  },
                  evidence: [
                    {
                      kind: "source",
                      message:
                        "Override sets allowAnonymous=true and denies reader",
                      at: target.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        const methodGetProvider = defineAdapter({
          id: "route-get-provider",
          provides: [
            defineFactProvider(ThirdPartyRouteMethodFact, async (ctx) => {
              const calls = await ctx.findSyntax({
                kinds: ["CallExpression"],
              });
              const target = calls.find((c) =>
                c.text.startsWith("registerRoute("),
              );
              if (!target) return [];
              return [
                {
                  subject: target.ref,
                  value: { method: "GET" as const },
                  evidence: [
                    {
                      kind: "source",
                      message: "Inferred GET from handler name",
                      at: target.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        const methodPostProvider = defineAdapter({
          id: "route-post-provider",
          provides: [
            defineFactProvider(ThirdPartyRouteMethodFact, async (ctx) => {
              const calls = await ctx.findSyntax({
                kinds: ["CallExpression"],
              });
              const target = calls.find((c) =>
                c.text.startsWith("registerRoute("),
              );
              if (!target) return [];
              return [
                {
                  subject: target.ref,
                  value: { method: "POST" as const },
                  evidence: [
                    {
                      kind: "source",
                      message: "Inferred POST from payload signature",
                      at: target.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        // 1. Complementary partial facts in [A, B] and [B, A] order
        let capturedForward: readonly FactResult<
          "call" | "syntax",
          EndpointAuthFactValue
        >[] = [];
        let capturedReverse: readonly FactResult<
          "call" | "syntax",
          EndpointAuthFactValue
        >[] = [];

        const makeRule = (
          onResult: (
            items: readonly FactResult<
              "call" | "syntax",
              EndpointAuthFactValue
            >[],
          ) => void,
        ) =>
          defineRule({
            id: "require-auth-roles",
            run: async (ctx) => {
              const items = await ctx.facts(ThirdPartyEndpointAuthFact, {
                condition: { scope: "exported" },
              });
              onResult(items);
              for (const item of items) {
                if (
                  item.value.roles.includes("reader") &&
                  item.value.roles.includes("editor")
                ) {
                  const firstEv = item.evidence[0];
                  if (firstEv?.at) {
                    ctx.report({
                      at: firstEv.at,
                      message: `Route requires roles [${item.value.roles.join(", ")}]`,
                      findingKey: "merged-auth-roles",
                      basis: [item],
                    });
                  }
                }
              }
            },
          });

        const reportForward = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: [partialProviderA, partialProviderB],
            rules: [
              makeRule((items) => {
                capturedForward = items;
              }),
            ],
          }),
        });
        const reportReverse = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: [partialProviderB, partialProviderA],
            rules: [
              makeRule((items) => {
                capturedReverse = items;
              }),
            ],
          }),
        });

        assert.equal(capturedForward.length, 1);
        assert.deepEqual(capturedForward[0]!.providers, [
          "auth-decorator-provider",
          "auth-policy-provider",
        ]);
        assert.deepEqual(capturedForward[0]!.value, {
          roles: ["editor", "reader"],
          allowAnonymous: false,
          hasDynamicRoles: true,
        });
        assert.equal(capturedForward[0]!.gaps.length, 1);
        assert.equal(capturedForward[0]!.evidence.length, 2);

        // Strict order independence for FactResult & CheckReport (including diagnostic IDs and identity)
        assert.deepEqual(
          {
            providers: capturedForward[0]!.providers,
            value: capturedForward[0]!.value,
            evidence: capturedForward[0]!.evidence,
            gaps: capturedForward[0]!.gaps,
          },
          {
            providers: capturedReverse[0]!.providers,
            value: capturedReverse[0]!.value,
            evidence: capturedReverse[0]!.evidence,
            gaps: capturedReverse[0]!.gaps,
          },
        );
        assert.deepEqual(reportForward, reportReverse);

        // 2. Mutual exclusion conflict via FactType.merge -> structured FACT_CONFLICT, load-order independent
        const authRule = defineRule({
          id: "query-auth",
          run: async (ctx) => {
            await ctx.facts(ThirdPartyEndpointAuthFact);
          },
        });

        let authErrForward: unknown;
        try {
          await runCheck({
            cwd: host.rootDir,
            config: defineConfig({
              tsconfig: "tsconfig.json",
              adapters: [partialProviderB, conflictingProviderC],
              rules: [authRule],
            }),
          });
        } catch (err) {
          authErrForward = err;
        }

        let authErrReverse: unknown;
        try {
          await runCheck({
            cwd: host.rootDir,
            config: defineConfig({
              tsconfig: "tsconfig.json",
              adapters: [conflictingProviderC, partialProviderB],
              rules: [authRule],
            }),
          });
        } catch (err) {
          authErrReverse = err;
        }

        assert.ok(authErrForward instanceof VeruleExecutionError);
        assert.ok(authErrReverse instanceof VeruleExecutionError);
        assert.equal(authErrForward.code, "FACT_CONFLICT");
        assert.equal(authErrReverse.code, "FACT_CONFLICT");
        assert.equal(authErrForward.message, authErrReverse.message);
        assert.ok(authErrForward.conflict);
        assert.deepEqual(authErrForward.conflict, authErrReverse.conflict);
        assert.equal(
          authErrForward.conflict.factTypeId,
          "thirdparty.endpoint.auth",
        );
        assert.deepEqual(
          authErrForward.conflict.parties.map((p) => p.providerId),
          ["auth-override-provider", "auth-policy-provider"],
        );
        assert.match(authErrForward.message, /allowAnonymous=true/u);
        assert.match(authErrForward.message, /allowAnonymous=false/u);
        assert.match(
          authErrForward.message,
          /Override sets allowAnonymous=true/u,
        );
        assert.match(authErrForward.message, /Policy requires editor role/u);

        // 3. Default conflict (FactType without custom merge) includes both conclusions & evidence
        const methodRule = defineRule({
          id: "query-method",
          run: async (ctx) => {
            await ctx.facts(ThirdPartyRouteMethodFact);
          },
        });
        let methodErrA: unknown;
        try {
          await runCheck({
            cwd: host.rootDir,
            config: defineConfig({
              tsconfig: "tsconfig.json",
              adapters: [methodGetProvider, methodPostProvider],
              rules: [methodRule],
            }),
          });
        } catch (err) {
          methodErrA = err;
        }
        let methodErrB: unknown;
        try {
          await runCheck({
            cwd: host.rootDir,
            config: defineConfig({
              tsconfig: "tsconfig.json",
              adapters: [methodPostProvider, methodGetProvider],
              rules: [methodRule],
            }),
          });
        } catch (err) {
          methodErrB = err;
        }

        assert.ok(methodErrA instanceof VeruleExecutionError);
        assert.ok(methodErrB instanceof VeruleExecutionError);
        assert.equal(methodErrA.code, "FACT_CONFLICT");
        assert.equal(methodErrA.message, methodErrB.message);
        assert.ok(methodErrA.conflict);
        assert.deepEqual(methodErrA.conflict, methodErrB.conflict);
        assert.deepEqual(
          methodErrA.conflict.parties.map((p) => p.conclusion),
          [{ method: "GET" }, { method: "POST" }],
        );
        assert.match(methodErrA.message, /"GET"/u);
        assert.match(methodErrA.message, /"POST"/u);
        assert.match(methodErrA.message, /Inferred GET/u);
        assert.match(methodErrA.message, /Inferred POST/u);
      },
    );
  });

  void it("replaces third-party FactType providers without modifying rule code and enforces explicit providerId filtering & observability", async () => {
    // Independent third-party validator FactType defined via @verule/sdk
    type ValidationSpecCondition = { readonly scope?: "all" | "calls" };
    type ValidationSpecValue = {
      readonly requiredFields: readonly string[];
      readonly optionalFields: readonly string[];
      readonly schemaName?: string;
    };

    const ThirdPartyValidatorSpecFact = defineFactType<
      "call",
      ValidationSpecCondition | undefined,
      ValidationSpecValue
    >({
      id: "thirdparty.validator.spec",
      description:
        "Required and optional input fields declared by any validator library.",
      subjectKind: "call",
      merge: (existing, incoming) => {
        const conflictField =
          existing.value.requiredFields.find((f) =>
            incoming.value.optionalFields.includes(f),
          ) ??
          incoming.value.requiredFields.find((f) =>
            existing.value.optionalFields.includes(f),
          );
        if (conflictField !== undefined) {
          return {
            ok: false,
            conflict: {
              message: `Conflicting requirement for field "${conflictField}"`,
              existingConclusion: `${conflictField}: ${existing.value.requiredFields.includes(conflictField) ? "required" : "optional"}`,
              incomingConclusion: `${conflictField}: ${incoming.value.requiredFields.includes(conflictField) ? "required" : "optional"}`,
            },
          };
        }
        return {
          ok: true,
          value: {
            requiredFields: [
              ...new Set([
                ...existing.value.requiredFields,
                ...incoming.value.requiredFields,
              ]),
            ].toSorted(),
            optionalFields: [
              ...new Set([
                ...existing.value.optionalFields,
                ...incoming.value.optionalFields,
              ]),
            ].toSorted(),
            ...((existing.value.schemaName ?? incoming.value.schemaName) !==
            undefined
              ? {
                  schemaName:
                    existing.value.schemaName ?? incoming.value.schemaName,
                }
              : {}),
          },
        };
      },
    });

    const UnrelatedFact = defineFactType<
      "syntax",
      undefined,
      { readonly ok: true }
    >({
      id: "thirdparty.unrelated",
      subjectKind: "syntax",
    });

    // Unchanged business rule that only depends on ThirdPartyValidatorSpecFact
    const requireEmailRule = defineRule({
      id: "require-email-field",
      run: async (ctx) => {
        const specs = await ctx.facts(ThirdPartyValidatorSpecFact, {
          condition: { scope: "calls" },
        });
        for (const spec of specs) {
          if (!spec.value.requiredFields.includes("email")) {
            const rows = await ctx.syntaxOf([spec.subject], {
              select: { location: true },
            });
            const at = rows[0]?.location ?? spec.evidence[0]?.at;
            if (!at) continue;
            ctx.report({
              at,
              message: `Validator "${spec.value.schemaName ?? "unknown"}" must require "email" (via ${spec.providers.join(", ")})`,
              findingKey: "missing-email-requirement",
              basis: [spec],
            });
          }
        }
      },
    });

    let zodExtracts = 0;
    let valibotExtracts = 0;

    const zodLikeProvider = defineAdapter({
      id: "zod-like-provider",
      provides: [
        defineFactProvider(ThirdPartyValidatorSpecFact, async (ctx) => {
          zodExtracts += 1;
          const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
          const resolved = await ctx.resolveCalls(calls.map((c) => c.ref));
          return resolved.flatMap((row, idx) => {
            const callSyntax = calls[idx];
            if (
              !callSyntax ||
              !callSyntax.text.startsWith("zParse(") ||
              row.call === null
            ) {
              return [];
            }
            return [
              {
                subject: row.call,
                value: {
                  requiredFields: ["name"],
                  optionalFields: [],
                  schemaName: "userSchema",
                },
                evidence: [
                  {
                    kind: "source" as const,
                    message: "Extracted from zParse(userSchema, ...)",
                    at: callSyntax.location,
                  },
                ],
              },
            ];
          });
        }),
      ],
    });

    const valibotLikeProvider = defineAdapter({
      id: "valibot-like-provider",
      provides: [
        defineFactProvider(ThirdPartyValidatorSpecFact, async (ctx) => {
          valibotExtracts += 1;
          const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
          const resolved = await ctx.resolveCalls(calls.map((c) => c.ref));
          return resolved.flatMap((row, idx) => {
            const callSyntax = calls[idx];
            if (
              !callSyntax ||
              !callSyntax.text.startsWith("vParse(") ||
              row.call === null
            ) {
              return [];
            }
            return [
              {
                subject: row.call,
                value: {
                  requiredFields: ["name"],
                  optionalFields: [],
                  schemaName: "userValibot",
                },
                evidence: [
                  {
                    kind: "source" as const,
                    message: "Extracted from vParse(userValibot, ...)",
                    at: callSyntax.location,
                  },
                ],
              },
            ];
          });
        }),
      ],
    });

    const unrelatedAdapter = defineAdapter({
      id: "unrelated-provider",
      provides: [defineFactProvider(UnrelatedFact, () => [])],
    });

    await withTempHost(
      { host: "sdk-queries/fact-providers-zod-style" },
      async (zodHost) => {
        await zodHost.typecheck();
        await withTempHost(
          { host: "sdk-queries/fact-providers-valibot-style" },
          async (valibotHost) => {
            await valibotHost.typecheck();

            // 1. Run rule with zodLikeProvider -> findings with zod evidence
            const repZod = await runCheck({
              cwd: zodHost.rootDir,
              config: defineConfig({
                tsconfig: "tsconfig.json",
                adapters: [zodLikeProvider],
                rules: [requireEmailRule],
              }),
            });
            assert.equal(repZod.status, "findings");
            assert.equal(repZod.diagnostics.length, 1);
            assert.match(
              repZod.diagnostics[0]!.message,
              /userSchema.*zod-like-provider/u,
            );

            // 2. Replace provider with valibotLikeProvider; zero rule changes!
            const repValibot = await runCheck({
              cwd: valibotHost.rootDir,
              config: defineConfig({
                tsconfig: "tsconfig.json",
                adapters: [valibotLikeProvider],
                rules: [requireEmailRule],
              }),
            });
            assert.equal(repValibot.status, "findings");
            assert.equal(repValibot.diagnostics.length, 1);
            assert.match(
              repValibot.diagnostics[0]!.message,
              /userValibot.*valibot-like-provider/u,
            );
            assert.ok(
              repValibot.diagnostics[0]!.evidence.some((ev) =>
                ev.message.includes("vParse(userValibot"),
              ),
            );

            // 3. Explicit providerId filtering: only invokes requested provider & reflects in observation/explain
            zodExtracts = 0;
            valibotExtracts = 0;
            const scopedRule = defineRule({
              id: "scoped-valibot-only",
              run: async (ctx) => {
                const items = await ctx.facts(ThirdPartyValidatorSpecFact, {
                  providerId: "valibot-like-provider",
                });
                assert.equal(items.length, 1);
                assert.deepEqual(items[0]!.providers, [
                  "valibot-like-provider",
                ]);
              },
            });
            const scopedReport = await runCheck({
              cwd: valibotHost.rootDir,
              config: defineConfig({
                tsconfig: "tsconfig.json",
                adapters: [zodLikeProvider, valibotLikeProvider],
                rules: [scopedRule],
              }),
              observe: "detail",
              at: "src/app.ts:4:10",
            });
            assert.equal(zodExtracts, 0);
            assert.equal(valibotExtracts, 1);
            assert.deepEqual(scopedReport.observation?.providers, [
              "engine",
              "valibot-like-provider",
            ]);
            assert.equal(
              scopedReport.observation?.providers.includes("zod-like-provider"),
              false,
            );
            assert.deepEqual(scopedReport.explanation?.providers, [
              "valibot-like-provider",
            ]);
            const valibotQuery = scopedReport.explanation?.queries.find(
              (q) => q.op === "facts:thirdparty.validator.spec",
            );
            assert.equal(valibotQuery?.provider, "valibot-like-provider");
            assert.equal(valibotQuery?.candidateCount, 1);

            // 4. Explicit providerId that is not enabled or doesn't support the FactType -> invokes neither provider, observable
            zodExtracts = 0;
            valibotExtracts = 0;
            const missingAndUnsupportedRule = defineRule({
              id: "missing-or-unsupported-provider",
              run: async (ctx) => {
                const notEnabled = await ctx.facts(
                  ThirdPartyValidatorSpecFact,
                  {
                    providerId: "not-enabled-provider",
                  },
                );
                const unsupported = await ctx.facts(
                  ThirdPartyValidatorSpecFact,
                  {
                    providerId: "unrelated-provider",
                  },
                );
                assert.deepEqual(notEnabled, []);
                assert.deepEqual(unsupported, []);
              },
            });
            const missingReport = await runCheck({
              cwd: valibotHost.rootDir,
              config: defineConfig({
                tsconfig: "tsconfig.json",
                adapters: [
                  zodLikeProvider,
                  valibotLikeProvider,
                  unrelatedAdapter,
                ],
                rules: [missingAndUnsupportedRule],
              }),
              observe: "detail",
              at: "src/app.ts:4:10",
            });
            assert.equal(zodExtracts, 0);
            assert.equal(valibotExtracts, 0);
            assert.deepEqual(missingReport.observation?.providers, []);
            assert.deepEqual(missingReport.explanation?.providers, []);
            const detailQueries = (
              missingReport.observation?.details ?? []
            ).filter((d) => d.kind === "query");
            assert.equal(detailQueries.length, 2);
            assert.deepEqual(detailQueries[0]?.detail, {
              factTypeId: "thirdparty.validator.spec",
              providers: [],
              providerFilter: "not-enabled-provider",
              providerFilterStatus: "not_enabled",
              candidateCount: 0,
            });
            assert.deepEqual(detailQueries[1]?.detail, {
              factTypeId: "thirdparty.validator.spec",
              providers: [],
              providerFilter: "unrelated-provider",
              providerFilterStatus: "unsupported",
              candidateCount: 0,
            });

            // 5. Empty/whitespace providerId throws INVALID_CONFIG
            await assert.rejects(
              () =>
                runCheck({
                  cwd: valibotHost.rootDir,
                  config: defineConfig({
                    tsconfig: "tsconfig.json",
                    adapters: [valibotLikeProvider],
                    rules: [
                      defineRule({
                        id: "empty-provider-filter",
                        run: async (ctx) => {
                          await ctx.facts(ThirdPartyValidatorSpecFact, {
                            providerId: "   ",
                          });
                        },
                      }),
                    ],
                  }),
                }),
              (err: unknown) =>
                err instanceof VeruleExecutionError &&
                err.code === "INVALID_CONFIG",
            );
          },
        );
      },
    );
  });

  void it("supports AdapterContext.facts composition across objects, transitive input attribution, explain traces, and cycle rejection", async () => {
    await withTempHost(
      { host: "sdk-queries/fact-providers-composition" },
      async (host) => {
        await host.typecheck();

        const SourceSpecFact = defineFactType<
          "syntax",
          undefined,
          {
            readonly sourceName: string;
            readonly fields: Readonly<Record<string, string>>;
          }
        >({
          id: "ts.composition.source-spec",
          subjectKind: "syntax",
        });

        const DerivedConsumerFact = defineFactType<
          "syntax",
          undefined,
          {
            readonly sourceName: string;
            readonly fields: Readonly<Record<string, string>>;
          }
        >({
          id: "ts.composition.derived-consumer",
          subjectKind: "syntax",
        });

        const UnusedFact = defineFactType<
          "syntax",
          undefined,
          { readonly ok: true }
        >({
          id: "ts.composition.unused",
          subjectKind: "syntax",
        });

        let unusedExtractCount = 0;

        const adapterA = defineAdapter({
          id: "ts-adapter-a",
          provides: [
            defineFactProvider(SourceSpecFact, async (ctx, request) => {
              const policyText = ctx.readAdditionalInput("a.inputs.json");
              const policy = JSON.parse(policyText) as {
                readonly mode: string;
              };
              assert.equal(policy.mode, "strict");
              const calls = await ctx.findSyntax({
                files: ["src/source.ts"],
                kinds: ["CallExpression"],
              });
              const specCall = calls.find((c) =>
                c.text.startsWith("defineSpec("),
              );
              if (!specCall) return [];
              if (
                request.targets &&
                !request.targets.some(
                  (t) => t.handleId === specCall.ref.handleId,
                )
              ) {
                return [];
              }
              return [
                {
                  subject: specCall.ref,
                  value: {
                    sourceName: "order",
                    fields: { orderId: "required", coupon: "optional" },
                  },
                  evidence: [
                    {
                      kind: "source",
                      message: "Adapter A extracted source spec on Object A.",
                      at: specCall.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        const adapterB = defineAdapter({
          id: "ts-adapter-b",
          provides: [
            defineFactProvider(DerivedConsumerFact, async (ctx) => {
              const calls = await ctx.findSyntax({
                files: ["src/consumer.ts"],
                kinds: ["CallExpression"],
              });
              const consumerCall = calls.find((c) =>
                c.text.startsWith("useSpec("),
              );
              const sourceCalls = await ctx.findSyntax({
                files: ["src/source.ts"],
                kinds: ["CallExpression"],
              });
              const targetA = sourceCalls.find((c) =>
                c.text.startsWith("defineSpec("),
              );
              if (!consumerCall || !targetA) return [];

              const sourceFacts = await ctx.facts(SourceSpecFact, {
                targets: [targetA.ref],
              });
              assert.equal(sourceFacts.length, 1);
              const factA = sourceFacts[0]!;
              return [
                {
                  subject: consumerCall.ref,
                  value: {
                    sourceName: factA.value.sourceName,
                    fields: { ...factA.value.fields, coupon: "defaulted" },
                  },
                  basis: [factA],
                  evidence: [
                    {
                      kind: "source",
                      message: "Adapter B derived consumer spec on Object B.",
                      at: consumerCall.location,
                    },
                  ],
                },
              ];
            }),
          ],
        });

        const unusedAdapter = defineAdapter({
          id: "ts-unused-adapter",
          provides: [
            defineFactProvider(UnusedFact, () => {
              unusedExtractCount += 1;
              return [];
            }),
          ],
        });

        const consumerRule = defineRule({
          id: "ts-composition-rule",
          run: async (ctx) => {
            const items = await ctx.facts(DerivedConsumerFact);
            for (const item of items) {
              const at = item.evidence.find((ev) =>
                ev.message.includes("Adapter B derived consumer spec"),
              )?.at;
              if (!at) continue;
              ctx.report({
                at,
                findingKey: "ts-derived-consumer",
                message: JSON.stringify(item.value),
                basis: [item],
              });
            }
          },
        });

        const cacheReuseRule = defineRule({
          id: "ts-composition-cache-rule",
          run: async (ctx) => {
            const items = await ctx.facts(DerivedConsumerFact);
            assert.equal(items.length, 1);
          },
        });

        const report = await runCheck({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: [adapterA, adapterB, unusedAdapter],
            rules: [consumerRule, cacheReuseRule],
          }),
          observe: "detail",
          at: "src/consumer.ts:5:30",
        });

        assert.equal(unusedExtractCount, 0);
        assert.equal(report.status, "findings");
        assert.equal(report.diagnostics.length, 1);
        const diag = report.diagnostics[0]!;
        assert.equal(diag.at.file, "src/consumer.ts");
        assert.deepEqual(JSON.parse(diag.message), {
          sourceName: "order",
          fields: { orderId: "required", coupon: "defaulted" },
        });
        assert.ok(
          diag.evidence.some(
            (ev) =>
              ev.message === "Adapter A extracted source spec on Object A." &&
              ev.at?.file === "src/source.ts",
          ),
        );
        assert.ok(
          diag.evidence.some(
            (ev) =>
              ev.message === "Adapter B derived consumer spec on Object B." &&
              ev.at?.file === "src/consumer.ts",
          ),
        );
        assert.equal(report.additionalInputs.length, 1);
        assert.equal(report.additionalInputs[0]?.file, "a.inputs.json");
        assert.equal(report.additionalInputs[0]?.exists, true);
        assert.deepEqual(report.additionalInputs[0]?.sources, [
          { kind: "adapter", id: "ts-adapter-a" },
        ]);
        const explanation = report.explanations?.find(
          (item) => item.ruleId === "ts-composition-rule",
        );
        assert.ok(explanation);
        assert.deepEqual(explanation.providers, [
          "ts-adapter-a",
          "ts-adapter-b",
        ]);
        assert.ok(
          explanation.queries.some(
            (q) => q.op === "facts:ts.composition.derived-consumer",
          ),
        );
        assert.ok(
          explanation.queries.some(
            (q) => q.op === "facts:ts.composition.source-spec",
          ),
        );
        const cacheExplanation = report.explanations?.find(
          (item) => item.ruleId === "ts-composition-cache-rule",
        );
        assert.ok(cacheExplanation);
        assert.equal(
          cacheExplanation.queries.find(
            (q) => q.op === "facts:ts.composition.derived-consumer",
          )?.cacheStatus,
          "reused",
        );

        // Cycle detection between Adapter B -> Adapter A -> Adapter B
        const CyclicA = defineFactType<
          "syntax",
          undefined,
          { readonly v: number }
        >({
          id: "ts.cycle.a",
          subjectKind: "syntax",
        });
        const CyclicB = defineFactType<
          "syntax",
          undefined,
          { readonly v: number }
        >({
          id: "ts.cycle.b",
          subjectKind: "syntax",
        });
        const cyclicAdapterA = defineAdapter({
          id: "cyclic-a",
          provides: [
            defineFactProvider(CyclicA, async (ctx) => {
              await ctx.facts(CyclicB);
              return [];
            }),
          ],
        });
        const cyclicAdapterB = defineAdapter({
          id: "cyclic-b",
          provides: [
            defineFactProvider(CyclicB, async (ctx) => {
              await ctx.facts(CyclicA);
              return [];
            }),
          ],
        });
        await assert.rejects(
          () =>
            runCheck({
              cwd: host.rootDir,
              config: defineConfig({
                tsconfig: "tsconfig.json",
                adapters: [cyclicAdapterA, cyclicAdapterB],
                rules: [
                  defineRule({
                    id: "trigger-cycle",
                    run: async (ctx) => {
                      await ctx.facts(CyclicB);
                    },
                  }),
                ],
              }),
            }),
          (err: unknown) =>
            err instanceof VeruleExecutionError &&
            err.code === "INVALID_QUERY" &&
            /cyclic/iu.test(err.message),
        );
      },
    );
  });

  void it("distinguishes skipped providers, unresolved gaps with blocked_by_gap, and resolved_by_override across same-snapshot explain", async () => {
    await withTempHost(
      { host: "sdk-queries/fact-providers-explain-gaps" },
      async (host) => {
        await host.typecheck();

        type SpecValue = {
          readonly sourceName: string;
          readonly fields: Readonly<Record<string, string>>;
          readonly unknownFields: readonly string[];
        };

        const SourceSpecFact = defineFactType<"syntax", undefined, SpecValue>({
          id: "ts.explain.source-spec",
          subjectKind: "syntax",
        });

        const DerivedConsumerFact = defineFactType<
          "syntax",
          undefined,
          SpecValue & { readonly consumerName: string }
        >({
          id: "ts.explain.derived-consumer",
          subjectKind: "syntax",
        });

        const UnusedFact = defineFactType<
          "syntax",
          undefined,
          { readonly ok: true }
        >({
          id: "ts.explain.unused",
          subjectKind: "syntax",
        });

        let adapterAExplainFlags: boolean[] = [];
        let adapterBExplainFlags: boolean[] = [];

        const adapterA = defineAdapter({
          id: "ts-gap-adapter-a",
          provides: [
            defineFactProvider(SourceSpecFact, async (ctx, request) => {
              adapterAExplainFlags.push(Boolean(request.explain));
              const calls = await ctx.findSyntax({
                files: ["src/source.ts"],
                kinds: ["CallExpression"],
              });
              const specCall = calls.find((c) =>
                c.text.startsWith("defineSpec("),
              );
              const dynamicCall = calls.find((c) =>
                c.text.startsWith("dynamicSpec("),
              );
              if (!specCall || !dynamicCall) return [];
              if (
                request.targets &&
                !request.targets.some(
                  (t) => t.handleId === specCall.ref.handleId,
                )
              ) {
                return [];
              }
              const gap = {
                kind: "unresolved-fixture-field",
                message:
                  'Field "couponCode" in "order" could not be statically resolved.',
                at: dynamicCall.location,
              };
              return [
                {
                  subject: specCall.ref,
                  value: {
                    sourceName: "order",
                    fields: { orderId: "required" },
                    unknownFields: ["couponCode"],
                  },
                  evidence: [
                    {
                      kind: "source",
                      message: "Adapter A extracted source spec on orderSpec.",
                      at: specCall.location,
                    },
                  ],
                  gaps: [gap],
                  ...(request.explain
                    ? {
                        explainTrace: {
                          localGaps: [
                            {
                              status: "unresolved" as const,
                              factTypeId: SourceSpecFact.id,
                              providerId: "ts-gap-adapter-a",
                              field: "couponCode",
                              gap,
                              subjectLocation: specCall.location,
                            },
                          ],
                        },
                      }
                    : {}),
                },
              ];
            }),
          ],
        });

        const adapterB = defineAdapter({
          id: "ts-gap-adapter-b",
          provides: [
            defineFactProvider(DerivedConsumerFact, async (ctx, request) => {
              adapterBExplainFlags.push(Boolean(request.explain));
              const consumerCalls = await ctx.findSyntax({
                files: ["src/consumer.ts"],
                kinds: ["CallExpression"],
              });
              const sourceCalls = await ctx.findSyntax({
                files: ["src/source.ts"],
                kinds: ["CallExpression"],
              });
              const targetA = sourceCalls.find((c) =>
                c.text.startsWith("defineSpec("),
              );
              if (!targetA) return [];
              const sourceFacts = await ctx.facts(SourceSpecFact, {
                targets: [targetA.ref],
              });
              const factA = sourceFacts[0];
              if (!factA) return [];

              const useCalls = consumerCalls.filter((c) =>
                c.text.startsWith("useSpec("),
              );
              const out = [];
              for (const call of useCalls) {
                if (
                  request.targets &&
                  !request.targets.some((t) => t.handleId === call.ref.handleId)
                ) {
                  continue;
                }
                const hasOverride = call.text.includes(
                  'couponCode: "defaulted"',
                );
                if (hasOverride) {
                  const fields = {
                    ...factA.value.fields,
                    couponCode: "defaulted",
                  };
                  const initialGap = factA.gaps[0]!;
                  out.push({
                    subject: call.ref,
                    value: {
                      consumerName: "overriddenConsumer",
                      sourceName: factA.value.sourceName,
                      fields,
                      unknownFields: [],
                    },
                    basis: [factA],
                    evidence: [
                      {
                        kind: "source" as const,
                        message:
                          'Adapter B overrode "couponCode" to "defaulted" on overriddenConsumer.',
                        at: call.location,
                      },
                    ],
                    gaps: [],
                    ...(request.explain
                      ? {
                          explainTrace: {
                            localGaps: [
                              {
                                status: "resolved_by_override" as const,
                                factTypeId: SourceSpecFact.id,
                                providerId: "ts-gap-adapter-a",
                                field: "couponCode",
                                gap: initialGap,
                                subjectLocation: call.location,
                                resolvedBy: {
                                  providerId: "ts-gap-adapter-b",
                                  operation: "override",
                                  conclusion: "couponCode=defaulted",
                                  at: call.location,
                                },
                              },
                            ],
                          },
                        }
                      : {}),
                  });
                } else {
                  out.push({
                    subject: call.ref,
                    value: {
                      consumerName: "rawConsumer",
                      sourceName: factA.value.sourceName,
                      fields: { ...factA.value.fields },
                      unknownFields: [...factA.value.unknownFields],
                    },
                    basis: [factA],
                    evidence: [
                      {
                        kind: "source" as const,
                        message:
                          "Adapter B derived rawConsumer without overriding couponCode.",
                        at: call.location,
                      },
                    ],
                    gaps: factA.gaps,
                    ...(request.explain
                      ? {
                          explainTrace: {
                            localGaps: [
                              {
                                status: "unresolved" as const,
                                factTypeId: SourceSpecFact.id,
                                providerId: "ts-gap-adapter-a",
                                field: "couponCode",
                                gap: factA.gaps[0]!,
                                subjectLocation: call.location,
                              },
                            ],
                          },
                        }
                      : {}),
                  });
                }
              }
              return out;
            }),
          ],
        });

        const unusedAdapter = defineAdapter({
          id: "ts-gap-unused",
          provides: [defineFactProvider(UnusedFact, () => [])],
        });

        const independentFieldRule = defineRule({
          id: "ts-independent-field-rule",
          run: async (ctx) => {
            const items = await ctx.facts(DerivedConsumerFact);
            for (const item of items) {
              if (item.value.fields.orderId === "required") {
                const at = item.evidence.find((ev) =>
                  ev.message.includes("Adapter B"),
                )?.at;
                if (at) {
                  ctx.report({
                    at,
                    findingKey: `req-orderId-${item.value.consumerName}`,
                    message: `${item.value.consumerName}: orderId is required`,
                    basis: [item],
                  });
                }
              }
            }
          },
        });

        const couponRequiredRule = defineRule({
          id: "ts-coupon-check-rule",
          run: async (ctx) => {
            const items = await ctx.facts(DerivedConsumerFact);
            for (const item of items) {
              if (item.value.unknownFields.includes("couponCode")) {
                continue;
              }
              if (item.value.fields.couponCode === "defaulted") {
                const at = item.evidence.find((ev) =>
                  ev.message.includes("Adapter B"),
                )?.at;
                if (at) {
                  ctx.report({
                    at,
                    findingKey: `coupon-defaulted-${item.value.consumerName}`,
                    message: `${item.value.consumerName}: couponCode defaulted`,
                    basis: [item],
                  });
                }
              }
            }
          },
        });

        const session = await openSession({
          cwd: host.rootDir,
          config: defineConfig({
            tsconfig: "tsconfig.json",
            adapters: [adapterA, adapterB, unusedAdapter],
            rules: [independentFieldRule, couponRequiredRule],
          }),
        });

        try {
          // Normal check (explain == false): only unresolved gap from rawConsumer leaks into CheckReport.gaps.
          const normalReport = await session.check();
          assert.deepEqual(adapterAExplainFlags, [false]);
          assert.deepEqual(adapterBExplainFlags, [false]);
          assert.equal(normalReport.gaps.length, 1);
          assert.equal(normalReport.gaps[0]?.kind, "unresolved-fixture-field");
          assert.equal(normalReport.gaps[0]?.at?.file, "src/source.ts");
          assert.equal(normalReport.gaps[0]?.at?.line, 9);
          // Independent field orderId still reports for both consumers, and overriddenConsumer reports couponCode defaulted.
          assert.equal(normalReport.diagnostics.length, 3);

          // Same-snapshot explain on rawConsumer (blocked_by_gap for ts-coupon-check-rule).
          const rawExplain = await session.explain({
            ruleId: "ts-coupon-check-rule",
            at: "src/consumer.ts:8:28",
          });
          assert.equal(
            rawExplain.identity.snapshotDigest,
            normalReport.identity.snapshotDigest,
          );
          assert.equal(rawExplain.explanation.status, "blocked_by_gap");
          assert.equal(rawExplain.explanation.gaps.length, 1);
          assert.equal(
            rawExplain.explanation.gaps[0]?.at?.file,
            "src/source.ts",
          );
          assert.equal(rawExplain.explanation.localGaps.length, 1);
          assert.equal(
            rawExplain.explanation.localGaps[0]?.status,
            "unresolved",
          );
          assert.equal(
            rawExplain.explanation.localGaps[0]?.field,
            "couponCode",
          );
          assert.equal(
            rawExplain.explanation.localGaps[0]?.providerId,
            "ts-gap-adapter-a",
          );
          assert.deepEqual(
            rawExplain.diagnostics,
            normalReport.diagnostics.filter(
              (diag) => diag.ruleId === "ts-coupon-check-rule",
            ),
          );
          assert.deepEqual(rawExplain.gaps, normalReport.gaps);

          const fullExplainSameSnapshot = await session.check({
            at: "src/consumer.ts:8:28",
          });
          assert.deepEqual(
            fullExplainSameSnapshot.diagnostics,
            normalReport.diagnostics,
          );
          assert.deepEqual(fullExplainSameSnapshot.gaps, normalReport.gaps);

          // Explain on overriddenConsumer shows resolved_by_override and keeps gaps empty for that target.
          const overriddenExplain = await session.explain({
            ruleId: "ts-coupon-check-rule",
            at: "src/consumer.ts:9:35",
          });
          assert.equal(overriddenExplain.explanation.status, "reported");
          assert.deepEqual(overriddenExplain.explanation.gaps, []);
          assert.equal(overriddenExplain.explanation.localGaps.length, 1);
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.status,
            "resolved_by_override",
          );
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.field,
            "couponCode",
          );
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.gap.at?.file,
            "src/source.ts",
          );
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.resolvedBy?.providerId,
            "ts-gap-adapter-b",
          );
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.resolvedBy?.at?.file,
            "src/consumer.ts",
          );
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.resolvedBy?.at?.line,
            9,
          );
          assert.ok(
            overriddenExplain.explanation.providerEvaluations.some(
              (entry) =>
                entry.providerId === "ts-gap-unused" &&
                entry.status === "skipped",
            ),
          );
          assert.ok(
            overriddenExplain.explanation.providerEvaluations.some(
              (entry) =>
                entry.providerId === "ts-gap-adapter-a" &&
                entry.status !== "skipped",
            ),
          );
          assert.ok(
            overriddenExplain.explanation.providerEvaluations.some(
              (entry) =>
                entry.providerId === "ts-gap-adapter-b" &&
                entry.status !== "skipped",
            ),
          );

          // Unrelated expression is skipped with no spurious gaps.
          const unrelatedExplain = await session.explain({
            ruleId: "ts-coupon-check-rule",
            at: "src/consumer.ts:12:30",
          });
          assert.equal(
            unrelatedExplain.explanation.status,
            "not_in_candidates",
          );
          assert.deepEqual(unrelatedExplain.explanation.gaps, []);
          assert.deepEqual(unrelatedExplain.explanation.localGaps, []);
          assert.ok(adapterAExplainFlags.includes(true));
          assert.ok(adapterBExplainFlags.includes(true));
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("propagates upstream localGaps through TS AdapterContext.facts(FactA) via basis and explainTrace during session.explain and runCheck", async () => {
    await withTempHost(
      { host: "sdk-queries/fact-providers-transitive-gaps" },
      async (host) => {
        await host.typecheck();

        type InputShape = {
          readonly fields: Readonly<Record<string, string>>;
          readonly unknownFields: readonly string[];
        };

        const InputFactA = defineFactType<"syntax", undefined, InputShape>({
          id: "ts.prop.input-a",
          subjectKind: "syntax",
        });

        const ConsumerFactB = defineFactType<
          "syntax",
          undefined,
          InputShape & { readonly consumer: string }
        >({
          id: "ts.prop.consumer-b",
          subjectKind: "syntax",
        });

        const adapterA = defineAdapter({
          id: "ts-prop-adapter-a",
          provides: [
            defineFactProvider(InputFactA, async (ctx, request) => {
              const calls = await ctx.findSyntax({
                files: ["src/source.ts"],
                kinds: ["CallExpression"],
              });
              const defCall = calls.find((c) =>
                c.text.startsWith("defineInput("),
              );
              const dynCall = calls.find((c) =>
                c.text.startsWith("dynamicPromo("),
              );
              if (!defCall || !dynCall) return [];
              const promoGap = {
                kind: "unresolved-promo-field",
                message: 'Field "promoCode" could not be statically resolved.',
                at: dynCall.location,
              };
              return [
                {
                  subject: defCall.ref,
                  value: {
                    fields: { id: "required" },
                    unknownFields: ["promoCode"],
                  },
                  evidence: [
                    {
                      kind: "source",
                      message: "Adapter A extracted checkoutInput.",
                      at: defCall.location,
                    },
                  ],
                  gaps: [promoGap],
                  ...(request.explain
                    ? {
                        explainTrace: {
                          localGaps: [
                            {
                              status: "unresolved" as const,
                              factTypeId: InputFactA.id,
                              providerId: "ts-prop-adapter-a",
                              field: "promoCode",
                              gap: promoGap,
                              subjectLocation: defCall.location,
                            },
                          ],
                        },
                      }
                    : {}),
                },
              ];
            }),
          ],
        });

        const adapterB = defineAdapter({
          id: "ts-prop-adapter-b",
          provides: [
            defineFactProvider(ConsumerFactB, async (ctx, request) => {
              const sourceFacts = await ctx.facts(InputFactA);
              const factA = sourceFacts[0];
              if (!factA) return [];
              const calls = await ctx.findSyntax({
                files: ["src/consumer.ts"],
                kinds: ["CallExpression"],
              });
              const wrapCalls = calls.filter((c) =>
                c.text.startsWith("wrapInput("),
              );
              return wrapCalls.map((call) => {
                const isOverride = call.text.includes('promoCode: "defaulted"');
                if (!isOverride) {
                  // Forward upstream FactA solely through basis without rebuilding explainTrace manually.
                  return {
                    subject: call.ref,
                    value: {
                      consumer: "forwardedConsumer",
                      fields: { ...factA.value.fields },
                      unknownFields: [...factA.value.unknownFields],
                    },
                    basis: [factA],
                    evidence: [
                      {
                        kind: "source" as const,
                        message: "Adapter B forwarded checkoutInput.",
                        at: call.location,
                      },
                    ],
                    gaps: factA.gaps,
                  };
                }

                const upstreamPromoLocalGap =
                  factA.explainTrace?.localGaps?.find(
                    (lg) => lg.field === "promoCode",
                  );
                return {
                  subject: call.ref,
                  value: {
                    consumer: "overriddenConsumer",
                    fields: {
                      ...factA.value.fields,
                      promoCode: "defaulted",
                    },
                    unknownFields: [],
                  },
                  basis: [factA],
                  evidence: [
                    {
                      kind: "source" as const,
                      message: "Adapter B overrode promoCode.",
                      at: call.location,
                    },
                  ],
                  gaps: [],
                  ...(request.explain && upstreamPromoLocalGap
                    ? {
                        explainTrace: {
                          localGaps: [
                            {
                              status: "resolved_by_override" as const,
                              factTypeId: upstreamPromoLocalGap.factTypeId,
                              providerId: upstreamPromoLocalGap.providerId,
                              field: "promoCode",
                              gap: upstreamPromoLocalGap.gap,
                              subjectLocation: call.location,
                              resolvedBy: {
                                providerId: "ts-prop-adapter-b",
                                operation: "override",
                                conclusion: "promoCode=defaulted",
                                at: call.location,
                              },
                            },
                          ],
                        },
                      }
                    : {}),
                };
              });
            }),
          ],
        });

        const consumerRule = defineRule({
          id: "ts-prop-consumer-rule",
          run: async (ctx) => {
            const items = await ctx.facts(ConsumerFactB);
            for (const item of items) {
              if (item.value.unknownFields.includes("promoCode")) {
                continue;
              }
              if (item.value.fields.promoCode === "defaulted") {
                const at = item.evidence.find((ev) =>
                  ev.message.includes("Adapter B"),
                )?.at;
                if (at) {
                  ctx.report({
                    at,
                    findingKey: `promo-${item.value.consumer}`,
                    message: `${item.value.consumer}: promoCode defaulted`,
                    basis: [item],
                  });
                }
              }
            }
          },
        });

        const config = defineConfig({
          tsconfig: "tsconfig.json",
          adapters: [adapterA, adapterB],
          rules: [consumerRule],
        });
        const session = await openSession({ cwd: host.rootDir, config });

        try {
          const forwardedExplain = await session.explain({
            ruleId: "ts-prop-consumer-rule",
            at: "src/consumer.ts:5:34",
          });
          assert.equal(forwardedExplain.explanation.status, "blocked_by_gap");
          assert.equal(forwardedExplain.explanation.localGaps.length, 1);
          assert.equal(
            forwardedExplain.explanation.localGaps[0]?.status,
            "unresolved",
          );
          assert.equal(
            forwardedExplain.explanation.localGaps[0]?.field,
            "promoCode",
          );
          assert.equal(
            forwardedExplain.explanation.localGaps[0]?.providerId,
            "ts-prop-adapter-a",
          );

          const overriddenExplain = await session.explain({
            ruleId: "ts-prop-consumer-rule",
            at: "src/consumer.ts:6:35",
          });
          assert.equal(overriddenExplain.explanation.status, "reported");
          assert.equal(overriddenExplain.explanation.localGaps.length, 1);
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.status,
            "resolved_by_override",
          );
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.field,
            "promoCode",
          );
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.providerId,
            "ts-prop-adapter-a",
          );
          assert.equal(
            overriddenExplain.explanation.localGaps[0]?.resolvedBy?.providerId,
            "ts-prop-adapter-b",
          );

          const oneShotOverridden = await runCheck({
            cwd: host.rootDir,
            config,
            at: "src/consumer.ts:6:35",
          });
          assert.equal(oneShotOverridden.explanation?.status, "reported");
          assert.equal(oneShotOverridden.explanation?.localGaps.length, 1);
          assert.equal(
            oneShotOverridden.explanation?.localGaps[0]?.status,
            "resolved_by_override",
          );
        } finally {
          await session.close();
        }
      },
    );
  });
});
