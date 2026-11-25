declare function isAuthorized(req: string): boolean;
declare function verify(req: string): void;
declare function execute(req: string): void;
declare function log(err: unknown): void;

export function deadAfterReturn(req: string): void {
  if (isAuthorized(req)) {
    verify(req);
  }
  return;
  // oxlint-disable-next-line no-unreachable
  execute(req);
}

export function deadIfFalse(req: string): void {
  // oxlint-disable-next-line no-constant-condition
  if (false) {
    execute(req);
  }
}

export function deadAfterSwitch(mode: "a" | "b"): void {
  switch (mode) {
    case "a":
      return;
    default:
      return;
  }
  // oxlint-disable-next-line no-unreachable
  execute(mode);
}

export function tryCatchHandler(req: string): void {
  try {
    if (isAuthorized(req)) {
      execute(req);
    }
  } catch (err) {
    log(err);
  }
}

export function loopHandler(items: readonly string[]): void {
  for (const item of items) {
    if (isAuthorized(item)) {
      execute(item);
    }
  }
}

export function* generatorHandler(req: string): Generator<string> {
  yield req;
  if (isAuthorized(req)) {
    execute(req);
  }
}

export function outerWithClosure(req: string): () => void {
  const inner = () => {
    execute(req);
  };
  return inner;
}
