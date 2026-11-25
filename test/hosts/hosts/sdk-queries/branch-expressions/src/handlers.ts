declare function isAuthorized(req: string): boolean;
declare function isGuest(req: string): boolean;
declare function deny(req: string): void;
declare function execute(req: string): void;

export class HandlerService {
  handle(req: string): void {
    // oxlint-disable-next-line no-unused-expressions
    isAuthorized(req) ? execute(req) : deny(req);
  }
}

export const conciseAnd = (req: string): void => {
  // oxlint-disable-next-line no-unused-expressions
  isAuthorized(req) && execute(req);
};

export const conciseExprBody = (req: string) =>
  isAuthorized(req) && execute(req);

export const fnExprOrNullish = function (
  req: string,
  cached: string | null,
): void {
  // oxlint-disable-next-line no-unused-expressions
  isGuest(req) || execute(req);
  const _v = cached ?? execute(req);
};
