import { runPerfCli } from "@tssae/acceptance";

const exitCode = await runPerfCli(process.argv.slice(2), "medium");
process.exitCode = exitCode;
