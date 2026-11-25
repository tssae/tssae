import assert from "node:assert/strict";
import { it } from "node:test";

import { getEngineVersion } from "tssae";

void it("returns the version reported by the packaged engine", async () => {
  assert.match(await getEngineVersion(), /^\d+\.\d+\.\d+(?:[-+][\w.+-]+)?$/u);
});
