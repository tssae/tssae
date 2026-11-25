import { fileURLToPath } from "node:url";

const executableName =
  process.platform === "win32" ? "verule-engine.exe" : "verule-engine";

/** Absolute path to the host executable installed by this package's build. */
export const engineExecutablePath = fileURLToPath(
  new URL(`../dist/bin/${executableName}`, import.meta.url),
);
