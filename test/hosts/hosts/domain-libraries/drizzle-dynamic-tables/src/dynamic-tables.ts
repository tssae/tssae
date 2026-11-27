import {
  pgTable,
  serial,
  text,
  timestamp,
  type PgDatabase,
} from "drizzle-orm/pg-core";

const sharedTimestamps = {
  createdAt: timestamp("created_at").defaultNow().notNull(),
};

declare function extraColumns(): Record<string, ReturnType<typeof text>>;
const dynamicColKey: string = "extra";

export const events = pgTable("events", {
  id: serial("id").primaryKey(),
  ...sharedTimestamps,
  ...extraColumns(),
  [dynamicColKey]: text("extra"),
});

declare const db: PgDatabase<never>;
export function queryEvents() {
  return db
    .select({ id: events.id, [dynamicColKey]: events.createdAt })
    .from(events);
}
