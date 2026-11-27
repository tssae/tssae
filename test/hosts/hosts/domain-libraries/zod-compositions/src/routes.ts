import { basePageSchema, extraMetaSchema } from "./schemas.js";
import { z } from "./zod-stub.js";

const extendedPage = basePageSchema
  .extend({
    password: z.string().nullable().prefault(""),
    customDomain: z.string().nullable(),
  })
  .merge(extraMetaSchema);

const pickedGate = basePageSchema.pick({ slug: true, title: true });
const omittedPage = extraMetaSchema.omit({ secret: true });
const partialGate = pickedGate.partial();
const requiredAgain = partialGate.required({ slug: true });

export function runRoute(input: unknown) {
  const a = extendedPage.parse(input);
  const b = pickedGate.parse(input);
  const c = omittedPage.parse(input);
  const d = partialGate.parse(input);
  const e = requiredAgain.parse(input);
  return { a, b, c, d, e };
}
