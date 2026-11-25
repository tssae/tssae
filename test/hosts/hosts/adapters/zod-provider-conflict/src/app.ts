declare function validateUser(input: unknown): unknown;
export function handle(input: unknown) {
  return validateUser(input);
}
