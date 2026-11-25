import { defineFactType } from "tssae";

export type FixtureComplementCondition = {
  readonly mode: "normal" | "conflict" | "targeted-conflict";
};

export type FixtureComplementValue = {
  readonly text: string;
  readonly input: string;
  readonly annotation: string;
  readonly secondaryInvocation: number;
};

export const FixtureCallSourceFact = defineFactType<
  "call",
  FixtureComplementCondition,
  FixtureComplementValue
>({
  id: "fixture.call-source",
  description:
    "Additional source annotations extracted by a separate native fixture provider.",
  subjectKind: "call",
});

export type FixtureDerivedFieldMode = "required" | "optional" | "defaulted";

export type FixtureDerivedConsumerValue = {
  readonly consumerName: string;
  readonly sourceName: string;
  readonly fields: Readonly<Record<string, FixtureDerivedFieldMode>>;
  readonly requiredFields: readonly string[];
  readonly optionalFields: readonly string[];
  readonly defaultedFields: readonly string[];
  readonly unknownFields: readonly string[];
  readonly hasDynamicFields: boolean;
  readonly sourceProviders: readonly string[];
};

export const FixtureDerivedConsumerFact = defineFactType<
  "syntax" | "call" | "declaration",
  undefined,
  FixtureDerivedConsumerValue
>({
  id: "fixture.derived-consumer",
  description:
    "Consumer specifications derived across objects by querying fixture.source-spec.",
  subjectKind: ["syntax", "call", "declaration"],
});
