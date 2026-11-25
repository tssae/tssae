import dual = require("dual-pkg");
import { transformSub } from "dual-pkg/sub";

export function runCjs(): void {
  const input = { id: "cjs" };
  const mainOut = dual.transformPayload(input);
  const subOut = transformSub(input);
  void [mainOut, subOut];
}
