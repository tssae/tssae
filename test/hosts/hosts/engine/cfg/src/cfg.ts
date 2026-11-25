declare function hasKey(req: string): boolean;
declare function hasToken(req: string): boolean;
declare function verify(): void;
declare function sink(req: string): void;

export function joint(req: string): void {
  if (hasKey(req)) {
    verify();
  } else if (hasToken(req)) {
    verify();
  } else {
    return;
  }
  sink(req);
}

export function dead(req: string): void {
  if (hasKey(req)) {
    verify();
  }
  return;
  // oxlint-disable-next-line no-unreachable
  sink(req);
}

export function unmodeled(req: string): void {
  try {
    if (hasKey(req)) {
      sink(req);
    }
  } catch {
    // intentionally unmodeled try-catch
  }
}
