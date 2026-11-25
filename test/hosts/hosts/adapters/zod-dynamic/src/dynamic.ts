import { z } from "./zod-stub.js";

declare function dynamicShape(): Record<string, typeof z.string>;
const dynKey = Math.random() > 0.5 ? "a" : "b";

export const mixedSchema = z.object({
  id: z.string(),
  ...dynamicShape(),
  [dynKey]: z.string(),
  extra: z.any(),
  meta: z.unknown(),
});

export const dictSchema = z.record(z.string(), z.number());

export function parseDynamic(input: unknown) {
  return {
    mixed: mixedSchema.parse(input),
    dict: dictSchema.parse(input),
  };
}
