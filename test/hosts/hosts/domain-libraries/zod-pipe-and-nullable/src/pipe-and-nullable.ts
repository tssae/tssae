import { z } from "zod";

declare function untypedFactory(): unknown;
declare function runtimeFieldValidator(): z.ZodType<number>;

export const methodPipeUnrecognized = z
  .object({ rawId: z.string() })
  .pipe(untypedFactory() as never);

export const staticPipeUnrecognized = z.pipe(
  z.object({ rawId: z.string() }),
  untypedFactory() as never,
);

export const andNullableOverUnknown = z
  .object({ id: z.string(), age: runtimeFieldValidator() })
  .and(z.object({ age: z.number().nullable() }));

export function validatePipeAndNullable(input: unknown) {
  return {
    methodPipe: methodPipeUnrecognized.parse(input),
    staticPipe: staticPipeUnrecognized.parse(input),
    andNullable: andNullableOverUnknown.parse(input),
  };
}
