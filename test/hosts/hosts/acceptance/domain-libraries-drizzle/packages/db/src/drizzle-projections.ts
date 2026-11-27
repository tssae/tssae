import type { SqliteRemoteDatabase } from "drizzle-orm/sqlite-proxy";

import { users } from "./drizzle-schema.ts";
import * as schema from "./drizzle-schema.ts";

declare const db: SqliteRemoteDatabase<typeof schema>;

const dynamicColumnName: string = "fullName";

export function getUserBrief() {
  return db
    .select({
      id: users.id,
      displayName: users.fullName,
    })
    .from(users)
    .get();
}

export function getUserWithWorkspace() {
  return db.query.users.findFirst({
    columns: { id: true, fullName: true },
    with: { workspace: true },
  });
}

export function getUserWithDynamicColumn() {
  return db
    .select({
      id: users.id,
      [dynamicColumnName]: users.fullName,
    })
    .from(users)
    .get();
}
