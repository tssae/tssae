import type { SqliteRemoteDatabase } from "drizzle-orm/sqlite-proxy";

import { users } from "./drizzle-schema.ts";
import * as schema from "./drizzle-schema.ts";

declare const db: SqliteRemoteDatabase<typeof schema>;

const dynamicColumnName: string = "fullName";

export function getCompleteUser() {
  return db
    .select({
      id: users.id,
      fullName: users.fullName,
      age: users.age,
      status: users.status,
    })
    .from(users)
    .get();
}

export function getUserBrief() {
  return db
    .select({
      id: users.id,
      fullName: users.fullName,
      status: users.status,
    })
    .from(users)
    .get();
}

export function getDynamicUser() {
  return db
    .select({
      id: users.id,
      [dynamicColumnName]: users.fullName,
    })
    .from(users)
    .get();
}

export function getPageWithWorkspace() {
  return db.query.page.findFirst({
    columns: {
      slug: true,
      customDomain: true,
    },
    with: {
      workspace: true,
    },
  });
}
