import { loadPublic, loadRestricted, sink } from "./sources.ts";

export function handle(id: string): void {
  const restricted = loadRestricted(id);
  const pub = loadPublic(id);
  sink(restricted);
  sink(pub);
}
