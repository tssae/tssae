export const v = {
  schema: <T>(s: T) => s,
  string: () => ({}),
  number: () => ({}),
  parse: (_schema: unknown, input: unknown) => input,
};
