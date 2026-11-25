import { z } from "zod";

declare function runtimeAgeValidator(): z.ZodType<number>;
const helpers = {
  makeValidator: (): z.ZodType<string> => z.string(),
};

export const partialUnknownSchema = z.object({
  id: z.string(),
  age: runtimeAgeValidator(),
  extra: helpers.makeValidator(),
});

export const overriddenSchema = partialUnknownSchema.extend({
  age: z.number().default(18),
  extra: z.string().optional(),
});

export const callbackProbe = {
  directReplace: z.number().default(18),
  nullableReplace: z.string().nullable(),
  unknownReplace: runtimeAgeValidator(),
  cbDefault: (s: z.ZodType<number>) => s.default(18),
  cbOptional: (s: z.ZodType<string>) => s.optional(),
  cbNullable: (s: z.ZodType<string>) => {
    return s.nullable();
  },
};

export function validate(input: unknown) {
  return {
    partial: partialUnknownSchema.parse(input),
    overridden: overriddenSchema.parse(input),
  };
}
