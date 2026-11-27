export type ZodAnyType = {
  optional: () => ZodAnyType;
  nullish: () => ZodAnyType;
  nullable: () => ZodAnyType;
  default: (v: unknown) => ZodAnyType;
  prefault: (v: unknown) => ZodAnyType;
  catch: (v: unknown) => ZodAnyType;
  nonoptional: () => ZodAnyType;
  transform: (fn: (v: never) => unknown) => ZodAnyType;
  refine: (fn: (v: never) => boolean) => ZodAnyType;
  superRefine: (fn: (v: never, ctx: unknown) => void) => ZodAnyType;
  pipe: (target: ZodAnyType) => ZodAnyType;
  openapi: (meta: unknown) => ZodAnyType;
  extend: (shape: Record<string, unknown>) => ZodObjectType;
  merge: (other: ZodObjectType) => ZodObjectType;
  pick: (mask: Record<string, boolean>) => ZodObjectType;
  omit: (mask: Record<string, boolean>) => ZodObjectType;
  partial: (mask?: Record<string, boolean>) => ZodObjectType;
  required: (mask?: Record<string, boolean>) => ZodObjectType;
  parse: (v: unknown) => unknown;
  safeParse: (v: unknown) => { success: boolean; data?: unknown };
  parseAsync: (v: unknown) => Promise<unknown>;
  safeParseAsync: (v: unknown) => Promise<{ success: boolean; data?: unknown }>;
  shape: Record<string, ZodAnyType>;
};
export type ZodObjectType = ZodAnyType;
declare const zodNode: ZodAnyType;
export const z = {
  string: () => zodNode,
  number: () => zodNode,
  boolean: () => zodNode,
  enum: (_values: readonly string[]) => zodNode,
  array: (_item: ZodAnyType) => zodNode,
  any: () => zodNode,
  unknown: () => zodNode,
  record: (_k: ZodAnyType, _v?: ZodAnyType) => zodNode,
  object: (_shape: Record<string, unknown>) => zodNode,
  strictObject: (_shape: Record<string, unknown>) => zodNode,
  looseObject: (_shape: Record<string, unknown>) => zodNode,
  union: (_items: readonly ZodAnyType[]) => zodNode,
  discriminatedUnion: (_k: string, _items: readonly ZodAnyType[]) => zodNode,
  intersection: (_a: ZodAnyType, _b: ZodAnyType) => zodNode,
};
