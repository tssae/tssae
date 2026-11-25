import { z } from "zod";

declare function runtimeFieldValidator(): z.ZodOptional<z.ZodNumber>;

export const modifierOverrideSchema = z.object({
  id: z.string(),
  age: runtimeFieldValidator().default(21),
});

export const andBaseSchema = z.object({
  id: z.string(),
  age: runtimeFieldValidator(),
});

export const andDefaultedSchema = andBaseSchema.and(
  z.object({ age: z.number().default(21) }),
);

export const andRequiredSchema = andBaseSchema.and(
  z.object({ age: z.number() }),
);

export const pipeOverrideSchema = andBaseSchema.pipe(
  z.object({ id: z.string(), age: z.number().default(21) }),
);

export const staticIntersectionSchema = z.intersection(
  andBaseSchema,
  z.object({ age: z.number().default(21) }),
);

export const staticPipeSchema = z.pipe(
  andBaseSchema,
  z.object({ id: z.string(), age: z.number().default(21) }),
);

export function validateCombinators(input: unknown) {
  return {
    modifier: modifierOverrideSchema.parse(input),
    andDefaulted: andDefaultedSchema.parse(input),
    andRequired: andRequiredSchema.parse(input),
    pipe: pipeOverrideSchema.parse(input),
    zIntersection: staticIntersectionSchema.parse(input),
    zPipe: staticPipeSchema.parse(input),
  };
}
