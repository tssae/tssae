import { transformPayload, type Payload } from "dual-pkg";
import { transformSub } from "dual-pkg/sub";

export function runEsm(): void {
  const input: Payload = { id: "esm" };
  const mainOut = transformPayload(input);
  const subOut = transformSub(input);
  void [mainOut, subOut];
}
