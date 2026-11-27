import { runConformanceCli } from "@tssae/acceptance";

const exitCode = await runConformanceCli(process.argv.slice(2));
process.exitCode = exitCode;
