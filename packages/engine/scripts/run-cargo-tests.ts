import { spawnSync } from "node:child_process";

import { resolveHostDir } from "@verule/test-hosts";

const hostsDir = resolveHostDir("engine");
const result = spawnSync(
  "cargo",
  ["test", "--locked", "--workspace", ...process.argv.slice(2)],
  {
    stdio: "inherit",
    env: {
      ...process.env,
      VERULE_ENGINE_TEST_HOSTS_DIR: hostsDir,
    },
  },
);

if (result.error !== undefined) {
  throw result.error;
}

process.exitCode = result.status ?? 1;
