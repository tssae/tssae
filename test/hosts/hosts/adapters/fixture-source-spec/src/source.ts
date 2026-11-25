export function defineFixtureSource(
  name: string,
  fields: Record<string, unknown>,
) {
  return { name, fields };
}

declare function dynamicField(): string;

export const orderSpec = defineFixtureSource("order", {
  id: "required",
  note: "optional",
  extra: dynamicField(),
});
