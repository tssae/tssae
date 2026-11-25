import {
  defineAdapter,
  defineFactProvider,
  type LibraryAdapter,
} from "@verule/sdk";

import { FixtureCallSourceFact, FixtureSourceSpecFact } from "./facts.ts";

export {
  FixtureCallSourceFact,
  FixtureSourceSpecFact,
  type FixtureCallSourceValue,
  type FixtureFieldMode,
  type FixtureSourceSpecValue,
} from "./facts.ts";

/** Creates the JavaScript fact-provider bridge for the native test adapter. */
export function createFixtureAdapter(
  id = "third-party-fixture",
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
        providerId: "third-party-fixture",
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
      "Independent native adapter fixture used to verify the public Rust API.",
    provides: [bridge(FixtureCallSourceFact), bridge(FixtureSourceSpecFact)],
  });
}

/** Default bridge loaded when the package name is listed in `defineConfig`. */
export const fixtureAdapter = createFixtureAdapter();

export default fixtureAdapter;
