export type RawPayload = { readonly untrusted: string };
export type ParsedPayload = { readonly clean: string };
export type ValidationResult =
  | { readonly ok: true; readonly data: ParsedPayload }
  | { readonly ok: false; readonly error: string };

export function validate(raw: RawPayload): ValidationResult {
  const trimmed = raw.untrusted.trim();
  if (trimmed.length === 0) {
    return { ok: false, error: "empty" };
  }
  return { ok: true, data: { clean: trimmed } };
}

export function handler(_input: RawPayload | ParsedPayload): void {}
