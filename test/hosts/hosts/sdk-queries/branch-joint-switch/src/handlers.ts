declare function hasApiKey(req: string): boolean;
declare function hasSession(req: string): boolean;
declare function verifyApiKey(req: string): void;
declare function verifySession(req: string): void;
declare function verifyBearer(req: string): void;
declare function execute(req: string): void;

export function multiAuthIfElse(req: string): void {
  if (hasApiKey(req)) {
    verifyApiKey(req);
  } else if (hasSession(req)) {
    verifySession(req);
  } else {
    return;
  }
  execute(req);
}

export function switchWithDefault(authType: "bearer" | "hmac" | "none"): void {
  switch (authType) {
    case "bearer":
      verifyBearer("bearer");
      break;
    case "hmac":
      verifyApiKey("hmac");
      break;
    default:
      throw new Error("unsupported");
  }
  execute(authType);
}

export function switchFallthrough(
  authType: "legacy" | "bearer" | "none",
): void {
  switch (authType) {
    case "legacy":
    case "bearer":
      verifyBearer("ok");
      break;
    default:
      return;
  }
  execute(authType);
}

export function switchNoDefault(authType: "bearer" | "other"): void {
  switch (authType) {
    case "bearer":
      verifyBearer("bearer");
      break;
  }
  execute(authType);
}
