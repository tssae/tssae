import { fileURLToPath } from "node:url";

import { typecheckHostProjects } from "../src/typecheck.ts";

const hostsDir = fileURLToPath(new URL("../hosts", import.meta.url));

try {
  await typecheckHostProjects(hostsDir);
} catch (error) {
  process.stderr.write(
    `${error instanceof Error ? error.message : String(error)}\n`,
  );
  process.exitCode = 1;
}
