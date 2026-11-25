export function loadRestricted(id: string): { id: string; secret: string } {
  return { id, secret: "s3cr3t" };
}

export function loadPublic(id: string): { id: string; secret: string } {
  return { id, secret: "public" };
}

export function sink(_value: unknown): void {}
