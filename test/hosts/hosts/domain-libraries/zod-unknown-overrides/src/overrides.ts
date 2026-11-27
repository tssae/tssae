import { z } from "zod";

declare function runtimeFieldValidator(): z.ZodType<number>;

const baseSchema = z.object({
  id: z.string(),
  age: z.number(),
});

export const extendUnknownSchema = baseSchema.extend({
  age: runtimeFieldValidator(),
});

export const extendReResolvedSchema = extendUnknownSchema.extend({
  age: z.number().default(21),
});

export const mergeUnknownSchema = baseSchema.merge(
  z.object({ age: runtimeFieldValidator() }),
);

const unknownAgeShape: Record<string, z.ZodType<number>> = {
  age: runtimeFieldValidator(),
};

export const spreadUnknownSchema = z.object({
  id: z.string(),
  age: z.number(),
  ...unknownAgeShape,
});

export const spreadReResolvedSchema = spreadUnknownSchema.extend({
  age: z.number().default(21),
});

export function validateOverrides(input: unknown) {
  return {
    extendUnknown: extendUnknownSchema.parse(input),
    extendReResolved: extendReResolvedSchema.parse(input),
    mergeUnknown: mergeUnknownSchema.parse(input),
    spreadUnknown: spreadUnknownSchema.parse(input),
    spreadReResolved: spreadReResolvedSchema.parse(input),
  };
}
