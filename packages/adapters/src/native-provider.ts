import {
  defineFactProvider,
  type FactProvider,
  type FactType,
  type ProgramObjectKind,
} from "tssae";

/**
 * Defines a `FactProvider` that forwards query requests straight to a native
 * engine provider with optional target, condition, and explain propagation.
 */
export function defineNativeFactProvider<
  SubjectKind extends ProgramObjectKind,
  Condition,
  Value,
>(
  factType: FactType<SubjectKind, Condition, Value>,
  options: {
    readonly providerId: string;
    readonly reportProviderId?: string;
  },
): FactProvider<SubjectKind, Condition, Value> {
  return defineFactProvider(factType, (ctx, request) =>
    ctx.extractEngineFacts({
      factTypeId: request.factType.id,
      providerId: options.providerId,
      ...(options.reportProviderId !== undefined
        ? { reportProviderId: options.reportProviderId }
        : {}),
      ...(request.targets !== undefined ? { targets: request.targets } : {}),
      ...(request.condition !== undefined
        ? { condition: request.condition }
        : {}),
      ...(request.explain !== undefined ? { explain: request.explain } : {}),
    }),
  );
}
