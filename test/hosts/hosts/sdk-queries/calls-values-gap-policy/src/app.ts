function knownSink(v: string): string {
  return v;
}
function makeSecret(): string {
  return "secret";
}
export function run(useDynamic: boolean, cb: (v: string) => string): string {
  const secret = makeSecret();
  const safe = knownSink(secret);
  if (useDynamic) {
    return cb(secret);
  }
  return safe;
}
