import { handler, validate, type RawPayload } from "./validator.js";

export function unsafeRoute(raw: RawPayload): void {
  const parsed = validate(raw);
  if (!parsed.ok) {
    return;
  }
  handler(raw);
}

export function safeRoute(raw: RawPayload): void {
  const parsed = validate(raw);
  if (!parsed.ok) {
    return;
  }
  handler(parsed.data);
}
