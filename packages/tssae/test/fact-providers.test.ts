import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@verule/test-hosts";
import {
  TssaeError,
  defineAdapter,
  defineFactProvider,
  defineFactType,
  evidenceOf,
  openProject,
  type FactResult,
} from "tssae";

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

void describe("tssae fact providers, replacement, and conflict semantics", () => {
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
        const queryEndpointAuth = async (
          adapters: readonly ReturnType<typeof defineAdapter>[],
        ): Promise<
          readonly FactResult<"call" | "syntax", EndpointAuthFactValue>[]
        > => {
          const session = await openProject({
            cwd: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters,
          });
          try {
            return await session.project().facts(ThirdPartyEndpointAuthFact, {
              condition: { scope: "exported" },
            });
          } finally {
            await session.close();
          }
        };

        const capturedForward = await queryEndpointAuth([
          partialProviderA,
          partialProviderB,
        ]);
        const capturedReverse = await queryEndpointAuth([
          partialProviderB,
          partialProviderA,
        ]);

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

        // Strict order independence for FactResult
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

        // 2. Mutual exclusion conflict via FactType.merge -> structured FACT_CONFLICT, load-order independent
        const runAuthQuery = async (
          adapters: readonly ReturnType<typeof defineAdapter>[],
        ): Promise<unknown> => {
          const session = await openProject({
            cwd: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters,
          });
          try {
            await session.project().facts(ThirdPartyEndpointAuthFact);
            return undefined;
          } catch (err) {
            return err;
          } finally {
            await session.close();
          }
        };

        const authErrForward = await runAuthQuery([
          partialProviderB,
          conflictingProviderC,
        ]);
        const authErrReverse = await runAuthQuery([
          conflictingProviderC,
          partialProviderB,
        ]);

        assert.ok(authErrForward instanceof TssaeError);
        assert.ok(authErrReverse instanceof TssaeError);
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
        const runMethodQuery = async (
          adapters: readonly ReturnType<typeof defineAdapter>[],
        ): Promise<unknown> => {
          const session = await openProject({
            cwd: host.rootDir,
            tsconfig: "tsconfig.json",
            adapters,
          });
          try {
            await session.project().facts(ThirdPartyRouteMethodFact);
            return undefined;
          } catch (err) {
            return err;
          } finally {
            await session.close();
          }
        };

        const methodErrA = await runMethodQuery([
          methodGetProvider,
          methodPostProvider,
        ]);
        const methodErrB = await runMethodQuery([
          methodPostProvider,
          methodGetProvider,
        ]);

        assert.ok(methodErrA instanceof TssaeError);
        assert.ok(methodErrB instanceof TssaeError);
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

  void it("replaces third-party FactType providers without modifying consumer query code and enforces explicit providerId filtering", async () => {
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

    const queryMissingEmail = async (
      cwd: string,
      adapters: readonly ReturnType<typeof defineAdapter>[],
    ) => {
      const session = await openProject({
        cwd,
        tsconfig: "tsconfig.json",
        adapters,
      });
      try {
        const ctx = session.project();
        const specs = await ctx.facts(ThirdPartyValidatorSpecFact, {
          condition: { scope: "calls" },
        });
        const findings: Array<{
          readonly message: string;
          readonly evidence: ReturnType<typeof evidenceOf>;
        }> = [];
        for (const spec of specs) {
          if (!spec.value.requiredFields.includes("email")) {
            findings.push({
              message: `Validator "${spec.value.schemaName ?? "unknown"}" must require "email" (via ${spec.providers.join(", ")})`,
              evidence: evidenceOf(spec),
            });
          }
        }
        return findings;
      } finally {
        await session.close();
      }
    };

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

            // 1. Run consumer query with zodLikeProvider -> findings with zod evidence
            const repZod = await queryMissingEmail(zodHost.rootDir, [
              zodLikeProvider,
            ]);
            assert.equal(repZod.length, 1);
            assert.match(repZod[0]!.message, /userSchema.*zod-like-provider/u);

            // 2. Replace provider with valibotLikeProvider; zero consumer code changes!
            const repValibot = await queryMissingEmail(valibotHost.rootDir, [
              valibotLikeProvider,
            ]);
            assert.equal(repValibot.length, 1);
            assert.match(
              repValibot[0]!.message,
              /userValibot.*valibot-like-provider/u,
            );
            assert.ok(
              repValibot[0]!.evidence.some((ev) =>
                ev.message.includes("vParse(userValibot"),
              ),
            );

            // 3. Explicit providerId filtering: only invokes requested provider
            zodExtracts = 0;
            valibotExtracts = 0;
            const filterSession = await openProject({
              cwd: valibotHost.rootDir,
              tsconfig: "tsconfig.json",
              adapters: [
                zodLikeProvider,
                valibotLikeProvider,
                unrelatedAdapter,
              ],
            });
            try {
              const ctx = filterSession.project();
              const items = await ctx.facts(ThirdPartyValidatorSpecFact, {
                providerId: "valibot-like-provider",
              });
              assert.equal(items.length, 1);
              assert.deepEqual(items[0]!.providers, ["valibot-like-provider"]);
              assert.equal(zodExtracts, 0);
              assert.equal(valibotExtracts, 1);

              // 4. Explicit providerId that is not enabled or doesn't support the FactType -> invokes neither provider
              zodExtracts = 0;
              valibotExtracts = 0;
              const notEnabled = await ctx.facts(ThirdPartyValidatorSpecFact, {
                providerId: "not-enabled-provider",
              });
              const unsupported = await ctx.facts(ThirdPartyValidatorSpecFact, {
                providerId: "unrelated-provider",
              });
              assert.deepEqual(notEnabled, []);
              assert.deepEqual(unsupported, []);
              assert.equal(zodExtracts, 0);
              assert.equal(valibotExtracts, 0);

              // 5. Empty/whitespace providerId throws INVALID_CONFIG
              await assert.rejects(
                () =>
                  ctx.facts(ThirdPartyValidatorSpecFact, {
                    providerId: "   ",
                  }),
                (err: unknown) =>
                  err instanceof TssaeError && err.code === "INVALID_CONFIG",
              );
            } finally {
              await filterSession.close();
            }
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
        let adapterAExtractCount = 0;
        let adapterBExtractCount = 0;

        const adapterA = defineAdapter({
          id: "ts-adapter-a",
          provides: [
            defineFactProvider(SourceSpecFact, async (ctx, request) => {
              adapterAExtractCount += 1;
              const parsed = JSON.parse(
                ctx.readAdditionalInput("a.inputs.json"),
              ) as { readonly mode: string };
              assert.equal(parsed.mode, "strict");
              const calls = await ctx.findSyntax({
                files: ["src/source.ts"],
                kinds: ["CallExpression"],
              });
              const sourceCall = calls.find((c) =>
                c.text.startsWith("defineSpec("),
              );
              if (!sourceCall) return [];
              if (
                request.targets &&
                !request.targets.some(
                  (t) => t.handleId === sourceCall.ref.handleId,
                )
              ) {
                return [];
              }
              return [
                {
                  subject: sourceCall.ref,
                  value: {
                    sourceName: "order",
                    fields: { orderId: "required", coupon: "optional" },
                  },
                  evidence: [
                    {
                      kind: "source",
                      message: "Adapter A extracted source spec on Object A.",
                      at: sourceCall.location,
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
              adapterBExtractCount += 1;
              const sourceFacts = await ctx.facts(SourceSpecFact);
              const sourceFact = sourceFacts[0];
              if (!sourceFact) return [];
              const calls = await ctx.findSyntax({
                files: ["src/consumer.ts"],
                kinds: ["CallExpression"],
              });
              const consumerCall = calls.find((c) =>
                c.text.startsWith("useSpec("),
              );
              if (!consumerCall) return [];
              return [
                {
                  subject: consumerCall.ref,
                  value: {
                    sourceName: sourceFact.value.sourceName,
                    fields: {
                      ...sourceFact.value.fields,
                      coupon: "defaulted",
                    },
                  },
                  basis: [sourceFact],
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

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [adapterA, adapterB, unusedAdapter],
        });
        try {
          const ctx = session.project();
          const items = await ctx.facts(DerivedConsumerFact);
          const cachedItems = await ctx.facts(DerivedConsumerFact);

          assert.equal(unusedExtractCount, 0);
          assert.equal(adapterAExtractCount, 1);
          assert.equal(adapterBExtractCount, 1);
          assert.equal(items.length, 1);
          assert.deepEqual(cachedItems, items);

          const item = items[0]!;
          assert.deepEqual(item.value, {
            sourceName: "order",
            fields: { orderId: "required", coupon: "defaulted" },
          });
          assert.deepEqual(item.providers, ["ts-adapter-b"]);
          const evidence = evidenceOf(item);
          assert.ok(
            evidence.some(
              (ev) =>
                ev.message === "Adapter A extracted source spec on Object A." &&
                ev.at?.file === "src/source.ts",
            ),
          );
          assert.ok(
            evidence.some(
              (ev) =>
                ev.message === "Adapter B derived consumer spec on Object B." &&
                ev.at?.file === "src/consumer.ts",
            ),
          );
          assert.equal(session.additionalInputs.length, 1);
          assert.equal(session.additionalInputs[0]?.file, "a.inputs.json");
          assert.equal(session.additionalInputs[0]?.exists, true);
          assert.deepEqual(session.additionalInputs[0]?.sources, [
            { kind: "adapter", id: "ts-adapter-a" },
          ]);
        } finally {
          await session.close();
        }

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
        const cycleSession = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [cyclicAdapterA, cyclicAdapterB],
        });
        try {
          await assert.rejects(
            () => cycleSession.project().facts(CyclicB),
            (err: unknown) =>
              err instanceof TssaeError &&
              err.code === "INVALID_QUERY" &&
              /cyclic/iu.test(err.message),
          );
        } finally {
          await cycleSession.close();
        }
      },
    );
  });

  void it("distinguishes skipped providers, unresolved gaps, and resolved_by_override with explain mode", async () => {
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

        const adapterAExplainFlags: boolean[] = [];
        const adapterBExplainFlags: boolean[] = [];
        let unusedCount = 0;

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
          provides: [
            defineFactProvider(UnusedFact, () => {
              unusedCount += 1;
              return [];
            }),
          ],
        });

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [adapterA, adapterB, unusedAdapter],
        });

        try {
          const ctx = session.project();
          // Normal query (explain: false): rawConsumer retains gap, overriddenConsumer has zero gaps
          const normalFacts = await ctx.facts(DerivedConsumerFact);
          assert.deepEqual(adapterAExplainFlags, [false]);
          assert.deepEqual(adapterBExplainFlags, [false]);
          assert.equal(unusedCount, 0);

          const rawNormal = normalFacts.find(
            (f) => f.value.consumerName === "rawConsumer",
          )!;
          const overNormal = normalFacts.find(
            (f) => f.value.consumerName === "overriddenConsumer",
          )!;
          assert.equal(rawNormal.gaps.length, 1);
          assert.equal(rawNormal.gaps[0]?.kind, "unresolved-fixture-field");
          assert.equal(rawNormal.gaps[0]?.at?.file, "src/source.ts");
          assert.equal(rawNormal.gaps[0]?.at?.line, 9);
          assert.equal(rawNormal.explainTrace, undefined);
          assert.deepEqual(overNormal.gaps, []);
          assert.equal(overNormal.explainTrace, undefined);

          // Explain query on same snapshot sets request.explain = true and populates explainTrace.localGaps
          const explainedFacts = await ctx.facts(DerivedConsumerFact, {
            explain: true,
          });
          assert.ok(adapterAExplainFlags.includes(true));
          assert.ok(adapterBExplainFlags.includes(true));
          assert.equal(unusedCount, 0);

          const rawExplained = explainedFacts.find(
            (f) => f.value.consumerName === "rawConsumer",
          )!;
          assert.equal(rawExplained.snapshotId, normalFacts[0]!.snapshotId);
          assert.equal(rawExplained.explainTrace?.localGaps?.length, 1);
          assert.equal(
            rawExplained.explainTrace?.localGaps?.[0]?.status,
            "unresolved",
          );
          assert.equal(
            rawExplained.explainTrace?.localGaps?.[0]?.field,
            "couponCode",
          );
          assert.equal(
            rawExplained.explainTrace?.localGaps?.[0]?.providerId,
            "ts-gap-adapter-a",
          );

          const overExplained = explainedFacts.find(
            (f) => f.value.consumerName === "overriddenConsumer",
          )!;
          assert.deepEqual(overExplained.gaps, []);
          assert.equal(overExplained.explainTrace?.localGaps?.length, 1);
          assert.equal(
            overExplained.explainTrace?.localGaps?.[0]?.status,
            "resolved_by_override",
          );
          assert.equal(
            overExplained.explainTrace?.localGaps?.[0]?.field,
            "couponCode",
          );
          assert.equal(
            overExplained.explainTrace?.localGaps?.[0]?.gap.at?.file,
            "src/source.ts",
          );
          assert.equal(
            overExplained.explainTrace?.localGaps?.[0]?.resolvedBy?.providerId,
            "ts-gap-adapter-b",
          );
          assert.equal(
            overExplained.explainTrace?.localGaps?.[0]?.resolvedBy?.at?.file,
            "src/consumer.ts",
          );
          assert.equal(
            overExplained.explainTrace?.localGaps?.[0]?.resolvedBy?.at?.line,
            9,
          );
        } finally {
          await session.close();
        }
      },
    );
  });

  void it("propagates upstream localGaps through TS AdapterContext.facts(FactA) via basis and explainTrace", async () => {
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

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
          adapters: [adapterA, adapterB],
        });

        try {
          const items = await session.project().facts(ConsumerFactB, {
            explain: true,
          });
          const forwarded = items.find(
            (item) => item.value.consumer === "forwardedConsumer",
          )!;
          const overridden = items.find(
            (item) => item.value.consumer === "overriddenConsumer",
          )!;

          assert.equal(forwarded.explainTrace?.localGaps?.length, 1);
          assert.equal(
            forwarded.explainTrace?.localGaps?.[0]?.status,
            "unresolved",
          );
          assert.equal(
            forwarded.explainTrace?.localGaps?.[0]?.field,
            "promoCode",
          );
          assert.equal(
            forwarded.explainTrace?.localGaps?.[0]?.providerId,
            "ts-prop-adapter-a",
          );

          assert.equal(overridden.explainTrace?.localGaps?.length, 1);
          assert.equal(
            overridden.explainTrace?.localGaps?.[0]?.status,
            "resolved_by_override",
          );
          assert.equal(
            overridden.explainTrace?.localGaps?.[0]?.field,
            "promoCode",
          );
          assert.equal(
            overridden.explainTrace?.localGaps?.[0]?.providerId,
            "ts-prop-adapter-a",
          );
          assert.equal(
            overridden.explainTrace?.localGaps?.[0]?.resolvedBy?.providerId,
            "ts-prop-adapter-b",
          );
        } finally {
          await session.close();
        }
      },
    );
  });
});
