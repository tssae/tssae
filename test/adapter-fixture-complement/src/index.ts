import { defineAdapter, defineFactProvider, type LibraryAdapter } from "tssae";

import { FixtureCallSourceFact, FixtureDerivedConsumerFact } from "./facts.ts";

export {
  FixtureCallSourceFact,
  FixtureDerivedConsumerFact,
  type FixtureComplementCondition,
  type FixtureComplementValue,
  type FixtureDerivedConsumerValue,
  type FixtureDerivedFieldMode,
} from "./facts.ts";

/** Creates the JavaScript bridge for the independent complementary Rust adapter. */
export function createFixtureComplementAdapter(
  id = "third-party-fixture-complement",
): LibraryAdapter {
  const bridge = <
    SubjectKind extends "call" | "syntax" | "declaration",
    Condition,
    Value,
  >(
    factType: Parameters<
      typeof defineFactProvider<SubjectKind, Condition, Value>
    >[0],
  ) =>
    defineFactProvider(factType, (ctx, request) =>
      ctx.extractEngineFacts({
        factTypeId: request.factType.id,
        providerId: "third-party-fixture-complement",
        reportProviderId: id,
        ...(request.targets !== undefined ? { targets: request.targets } : {}),
        ...(request.condition !== undefined
          ? { condition: request.condition }
          : {}),
        ...(request.explain !== undefined ? { explain: request.explain } : {}),
      }),
    );

  return defineAdapter({
    id,
    version: "1.0.0",
    description:
      "Independent native provider that adds source annotations to fixture call facts.",
    provides: [
      bridge(FixtureCallSourceFact),
      bridge(FixtureDerivedConsumerFact),
    ],
  });
}

/** Default bridge selected when the package is listed in `defineConfig.adapters`. */
export const fixtureComplementAdapter = createFixtureComplementAdapter();

export default fixtureComplementAdapter;
