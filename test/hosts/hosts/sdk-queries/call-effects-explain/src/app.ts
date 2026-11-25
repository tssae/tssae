import { createExternalResource, onDidDisconnect } from "@acme/lifecycle";

import { releaseResource } from "./helper.js";

export function setupSession(): void {
  const conn = createExternalResource("conn");
  onDidDisconnect(() => {
    releaseResource(conn);
  });
}
