import { defineFactType } from "tssae";

export type FixtureCallSourceValue = {
  readonly text: string;
  readonly input: string;
  readonly invocation: number;
};

export const FixtureCallSourceFact = defineFactType<
  "call",
  undefined,
  FixtureCallSourceValue
>({
  id: "fixture.call-source",
  description:
    "Source text extracted by an independently built native adapter.",
  subjectKind: "call",
});

export type FixtureFieldMode = "required" | "optional" | "defaulted";

export type FixtureSourceSpecValue = {
  readonly sourceName: string;
  readonly fields: Readonly<Record<string, FixtureFieldMode>>;
  readonly requiredFields: readonly string[];
  readonly optionalFields: readonly string[];
  readonly defaultedFields: readonly string[];
  readonly unknownFields: readonly string[];
  readonly hasDynamicFields: boolean;
};

export const FixtureSourceSpecFact = defineFactType<
  "syntax" | "declaration" | "call",
  undefined,
  FixtureSourceSpecValue
>({
  id: "fixture.source-spec",
  description:
    "Source field specifications extracted by an independently built native adapter.",
  subjectKind: ["syntax", "declaration", "call"],
});
