import assert from "node:assert/strict";
import { it } from "node:test";

import { getEngineVersion } from "tssae";

void it("runs under the package-managed Node 24.0.0 runtime", () => {
  assert.equal(process.versions.node, "24.0.0");
});

void it("returns the version reported by the packaged engine", async () => {
  assert.match(await getEngineVersion(), /^\d+\.\d+\.\d+(?:[-+][\w.+-]+)?$/u);
});
