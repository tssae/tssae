import cond = require("cond-custom");

export function runCjs(): void {
  const base = { kind: "b" };
  const viaCond = cond.adapt(base);
  void viaCond;
}
