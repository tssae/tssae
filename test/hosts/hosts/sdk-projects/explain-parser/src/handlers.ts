interface ExternalParser {
  parse(value: unknown): unknown;
}
declare const parser: ExternalParser;
declare function makeInput(): { id: number };

export function runFlow() {
  const row = makeInput();
  return parser.parse(row);
}
