import { readFileSync } from "node:fs";
import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { basename } from "node:path";

import type { PackedArtifact } from "./artifacts.ts";

export type ArtifactRegistryServer = {
  readonly url: string;
  readonly port: number;
  readonly scopes: readonly string[];
  readonly unscopedPackages: readonly string[];
  close(): Promise<void>;
};

export type StartArtifactRegistryOptions = {
  /**
   * Upstream npm registry URL used to proxy packages not in `artifacts` (for example
   * `typescript` or `@types/node`) when routing unscoped packages through the loopback
   * registry. Pass `null` to disable upstream forwarding.
   */
  readonly upstreamRegistryUrl?: string | null;
};

const syntheticReleaseTimestamp = "2020-01-01T00:00:00.000Z";
const tarballRoutePrefix = "/-/tarball/";
const defaultUpstreamRegistry = "https://registry.npmjs.org/";

export function extractPackageScope(packageName: string): string | undefined {
  if (!packageName.startsWith("@")) {
    return undefined;
  }
  const slashIndex = packageName.indexOf("/");
  return slashIndex > 1 ? packageName.slice(0, slashIndex) : undefined;
}

function sendJson(
  res: ServerResponse,
  statusCode: number,
  payload: unknown,
): void {
  const body = JSON.stringify(payload);
  res.writeHead(statusCode, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(body),
  });
  res.end(body);
}

async function proxyUpstreamRequest(
  req: IncomingMessage,
  res: ServerResponse,
  upstreamBaseUrl: string,
): Promise<void> {
  const targetUrl = new URL(req.url ?? "/", upstreamBaseUrl);
  const headers: Record<string, string> = {};
  if (typeof req.headers.accept === "string") {
    headers.accept = req.headers.accept;
  }
  const upstreamResponse = await fetch(targetUrl, {
    method: req.method ?? "GET",
    headers,
  });
  const body =
    req.method === "HEAD"
      ? Buffer.alloc(0)
      : Buffer.from(await upstreamResponse.arrayBuffer());
  const responseHeaders: Record<string, string | number> = {
    "content-length": body.length,
  };
  const contentType = upstreamResponse.headers.get("content-type");
  if (contentType !== null) {
    responseHeaders["content-type"] = contentType;
  }
  res.writeHead(upstreamResponse.status, responseHeaders);
  res.end(req.method === "HEAD" ? undefined : body);
}

/**
 * Starts an in-process loopback HTTP npm registry serving packuments and `.tgz` tarballs
 * for both scoped and unscoped local artifacts, while optionally forwarding external
 * dependencies to an upstream npm registry.
 */
export async function startArtifactRegistry(
  artifacts: readonly PackedArtifact[],
  options: StartArtifactRegistryOptions = {},
): Promise<ArtifactRegistryServer> {
  const byName = new Map<string, PackedArtifact>();
  const byTarball = new Map<string, PackedArtifact>();
  const scopeSet = new Set<string>();
  const unscopedSet = new Set<string>();

  for (const artifact of artifacts) {
    byName.set(artifact.name, artifact);
    byTarball.set(basename(artifact.filename), artifact);
    const scope = extractPackageScope(artifact.name);
    if (scope !== undefined) {
      scopeSet.add(scope);
    } else {
      unscopedSet.add(artifact.name);
    }
  }

  const upstreamRegistryUrl =
    options.upstreamRegistryUrl === null
      ? undefined
      : (options.upstreamRegistryUrl ??
        process.env.npm_config_registry ??
        defaultUpstreamRegistry);

  let port = 0;

  async function handleRequest(
    req: IncomingMessage,
    res: ServerResponse,
  ): Promise<void> {
    const rawUrl = req.url ?? "/";
    const pathname = rawUrl.split("?")[0] ?? "/";
    if (req.method !== "GET" && req.method !== "HEAD") {
      sendJson(res, 405, { error: "method_not_allowed" });
      return;
    }

    if (pathname.startsWith(tarballRoutePrefix)) {
      const encodedName = pathname.slice(tarballRoutePrefix.length);
      const tarballName = decodeURIComponent(encodedName);
      const artifact = byTarball.get(tarballName);
      if (artifact === undefined) {
        sendJson(res, 404, { error: "tarball_not_found" });
        return;
      }
      const bytes = readFileSync(artifact.filename);
      res.writeHead(200, {
        "content-type": "application/octet-stream",
        "content-length": bytes.length,
      });
      res.end(req.method === "HEAD" ? undefined : bytes);
      return;
    }

    const packageName = decodeURIComponent(pathname.slice(1));
    const artifact = byName.get(packageName);
    if (artifact !== undefined) {
      const tarballUrl = `http://127.0.0.1:${String(port)}${tarballRoutePrefix}${encodeURIComponent(basename(artifact.filename))}`;
      sendJson(res, 200, {
        name: artifact.name,
        "dist-tags": { latest: artifact.version },
        time: { [artifact.version]: syntheticReleaseTimestamp },
        versions: {
          [artifact.version]: {
            ...artifact.manifest,
            dist: {
              shasum: artifact.sha1,
              integrity: artifact.integrity,
              tarball: tarballUrl,
            },
          },
        },
      });
      return;
    }

    if (upstreamRegistryUrl !== undefined) {
      try {
        await proxyUpstreamRequest(req, res, upstreamRegistryUrl);
        return;
      } catch (error) {
        sendJson(res, 502, {
          error: "upstream_proxy_failed",
          message: error instanceof Error ? error.message : String(error),
        });
        return;
      }
    }

    sendJson(res, 404, { error: "package_not_found" });
  }

  const server = createServer((req, res) => {
    void handleRequest(req, res).catch((error: unknown) => {
      if (!res.headersSent) {
        sendJson(res, 500, {
          error: "internal_registry_error",
          message: error instanceof Error ? error.message : String(error),
        });
      } else {
        res.destroy();
      }
    });
  });

  await new Promise<void>((resolveListen, rejectListen) => {
    server.once("error", rejectListen);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", rejectListen);
      resolveListen();
    });
  });

  const address = server.address();
  if (address === null || typeof address === "string") {
    await new Promise<void>((resolveClose) =>
      server.close(() => resolveClose()),
    );
    throw new Error("Expected TCP address for loopback artifact registry");
  }
  port = address.port;

  return {
    url: `http://127.0.0.1:${String(port)}`,
    port,
    scopes: [...scopeSet],
    unscopedPackages: [...unscopedSet],
    async close(): Promise<void> {
      server.closeAllConnections();
      await new Promise<void>((resolveClose, rejectClose) => {
        server.close((error) => {
          if (error !== undefined && error !== null) {
            rejectClose(error);
            return;
          }
          resolveClose();
        });
      });
    },
  };
}
