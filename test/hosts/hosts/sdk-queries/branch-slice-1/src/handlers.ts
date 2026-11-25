declare function isAuthorized(req: string): boolean;
declare function audit(req: string): void;
declare function execute(req: string): void;

export function guardedEarlyReturn(req: string): void {
  if (!isAuthorized(req)) {
    return;
  }
  execute(req);
}

export function bypassPossible(req: string): void {
  if (isAuthorized(req)) {
    audit(req);
  }
  execute(req);
}
