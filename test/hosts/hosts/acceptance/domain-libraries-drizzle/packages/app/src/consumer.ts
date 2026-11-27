import {
  accounts,
  getUserBrief,
  getUserWithWorkspace,
  refinedWorkspaceSchema,
} from "@acceptance-host/db";

declare function inspectTable(table: unknown): void;

export function runDrizzleConsumer() {
  inspectTable(accounts);
  return {
    brief: getUserBrief(),
    withWorkspace: getUserWithWorkspace(),
    refinedWorkspaceSchema,
  };
}
