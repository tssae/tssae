import { forwardBridge } from "./bridge.js";

function createToken(): string {
  return "secret";
}

async function asyncPass(v: string): Promise<string> {
  return v;
}

function idFn(v: string): string {
  return v;
}

function fetchRow(): { id: string; tag: string } {
  return { id: "1", tag: "ok" };
}

export async function run(
  cond: boolean,
  cb: (x: string) => string,
): Promise<string> {
  const raw = createToken();
  const unwrapped = (await asyncPass(raw)) as string;
  const row = { secret: unwrapped, extra: "ok" };
  const { extra, ...rest } = row;
  const delivered = forwardBridge("alpha", rest.secret);
  const { secret: extracted, ...withoutSecret } = row;
  const isolatedA = idFn(raw);
  const isolatedB = idFn("safe");
  const templated = `prefix:${raw}`;
  let overwritten = raw;
  overwritten = "clean";
  const opaque = fetchRow();
  const { id: _id, ...restRow } = opaque;
  const mixedOut = (cond ? idFn : cb)(raw);
  return `${delivered}:${extra}:${extracted}:${Object.keys(withoutSecret).length}:${isolatedA}:${isolatedB}:${templated}:${overwritten}:${restRow.tag}:${mixedOut}`;
}
