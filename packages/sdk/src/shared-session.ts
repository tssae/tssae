import { mkdirSync, rmSync } from "node:fs";
import {
  createConnection,
  createServer,
  type Server,
  type Socket,
} from "node:net";
import { dirname, resolve } from "node:path";

import {
  VeruleExecutionError,
  type CheckReport,
  type FactConflictDetails,
  type ObserveLimitInput,
  type ObserveMode,
  type ObserveOptions,
  type RequestSharedReportOptions,
  type SharedSession,
  type SharedSessionOptions,
  type SourceTarget,
  type VeruleErrorCode,
} from "./types.ts";
import {
  createWorkspaceController,
  toExecutionErrorInfo,
  type WorkspaceCheckParams,
} from "./watch.ts";

type SharedSessionRequest = {
  readonly id: string;
  readonly method: "check";
  readonly params?: {
    readonly ruleIds?: readonly string[];
    readonly at?: string | SourceTarget;
    readonly observe?: ObserveMode | ObserveOptions;
    readonly observeLimit?: ObserveLimitInput;
  };
};

type SharedSessionResponse =
  | {
      readonly id: string;
      readonly ok: true;
      readonly report: CheckReport;
    }
  | {
      readonly id: string;
      readonly ok: false;
      readonly error: {
        readonly code: VeruleErrorCode;
        readonly message: string;
        readonly conflict?: FactConflictDetails;
      };
    };

function isNamedPipeEndpoint(endpoint: string): boolean {
  return process.platform === "win32" && endpoint.startsWith("\\\\.\\pipe\\");
}

function resolveEndpointPath(cwd: string, endpoint: string): string {
  if (isNamedPipeEndpoint(endpoint)) {
    return endpoint;
  }
  return resolve(cwd, endpoint);
}

function probeSocketActive(endpoint: string): Promise<boolean> {
  return new Promise((resolveProbe) => {
    const socket = createConnection(endpoint);
    const finish = (active: boolean) => {
      socket.removeAllListeners();
      socket.destroy();
      resolveProbe(active);
    };
    socket.setTimeout(250, () => finish(false));
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
  });
}

function bindServerOnce(server: Server, endpoint: string): Promise<void> {
  return new Promise((resolveBind, rejectBind) => {
    const onError = (err: NodeJS.ErrnoException) => {
      server.off("listening", onListening);
      rejectBind(err);
    };
    const onListening = () => {
      server.off("error", onError);
      resolveBind();
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen(endpoint);
  });
}

async function listenEndpoint(server: Server, endpoint: string): Promise<void> {
  if (!isNamedPipeEndpoint(endpoint)) {
    mkdirSync(dirname(endpoint), { recursive: true });
  }
  try {
    await bindServerOnce(server, endpoint);
  } catch (err) {
    const nodeErr = err as NodeJS.ErrnoException;
    if (nodeErr.code !== "EADDRINUSE" || isNamedPipeEndpoint(endpoint)) {
      throw new VeruleExecutionError(
        "ENGINE_ERROR",
        `Failed to listen on shared session endpoint "${endpoint}": ${nodeErr.message}`,
        { cause: err },
      );
    }
    const active = await probeSocketActive(endpoint);
    if (active) {
      throw new VeruleExecutionError(
        "INVALID_CONFIG",
        `Shared session endpoint "${endpoint}" is already in use by an active session.`,
      );
    }
    rmSync(endpoint, { force: true });
    try {
      await bindServerOnce(server, endpoint);
    } catch (retryErr) {
      const msg =
        retryErr instanceof Error ? retryErr.message : String(retryErr);
      throw new VeruleExecutionError(
        "ENGINE_ERROR",
        `Failed to listen on shared session endpoint "${endpoint}": ${msg}`,
        { cause: retryErr },
      );
    }
  }
}

function parseSharedRequest(rawLine: string): SharedSessionRequest {
  let parsed: unknown;
  try {
    parsed = JSON.parse(rawLine);
  } catch (err) {
    throw new VeruleExecutionError(
      "INVALID_CONFIG",
      `Invalid shared session request JSON: ${err instanceof Error ? err.message : String(err)}`,
      { cause: err },
    );
  }
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    typeof (parsed as { readonly id?: unknown }).id !== "string" ||
    (parsed as { readonly method?: unknown }).method !== "check"
  ) {
    throw new VeruleExecutionError(
      "INVALID_CONFIG",
      'Shared session request must include string "id" and method "check".',
    );
  }
  return parsed as SharedSessionRequest;
}

function toCheckParams(
  params: SharedSessionRequest["params"],
): WorkspaceCheckParams | undefined {
  if (!params) return undefined;
  return {
    ...(params.ruleIds !== undefined ? { ruleIds: params.ruleIds } : {}),
    ...(params.at !== undefined ? { at: params.at } : {}),
    ...(params.observe !== undefined ? { observe: params.observe } : {}),
    ...(params.observeLimit !== undefined
      ? { observeLimit: params.observeLimit }
      : {}),
  };
}

/**
 * Starts an explicit long-lived shared analysis session bound to a single configuration and
 * local socket/pipe endpoint. Reuses the same incremental update and rule execution pipeline
 * as `watchProject`, and verifies disk state synchronously on every client query to avoid
 * watcher debounce races.
 */
export async function startSharedSession(
  options: SharedSessionOptions,
): Promise<SharedSession> {
  if (options.signal?.aborted) {
    throw new VeruleExecutionError(
      "SESSION_CLOSED",
      "Shared session startup was aborted.",
    );
  }

  const absCwd = resolve(options.cwd);
  const endpoint = resolveEndpointPath(absCwd, options.endpoint);

  const controller = createWorkspaceController({
    cwd: absCwd,
    ...(options.adapterResolutionRoot !== undefined
      ? { adapterResolutionRoot: options.adapterResolutionRoot }
      : {}),
    ...(options.config !== undefined ? { config: options.config } : {}),
    ...(options.loadConfig !== undefined
      ? { loadConfig: options.loadConfig }
      : {}),
    ...(options.watchFiles !== undefined
      ? { watchFiles: options.watchFiles }
      : {}),
    ...(options.observe !== undefined ? { observe: options.observe } : {}),
    ...(options.observeLimit !== undefined
      ? { observeLimit: options.observeLimit }
      : {}),
    ...(options.debounceMs !== undefined
      ? { debounceMs: options.debounceMs }
      : {}),
    onBatchReady: () => {
      // Keep the engine snapshot warm in the background; errors are captured for client queries.
      void controller.applyPendingBatch().catch(() => {});
    },
  });

  try {
    await controller.openInitial();
  } catch (err) {
    await controller.close();
    throw err;
  }

  const activeSockets = new Set<Socket>();
  let isClosed = false;
  let closePromise: Promise<void> | undefined;
  let resolveClosed!: () => void;
  const closedDone = new Promise<void>((resolveDone) => {
    resolveClosed = resolveDone;
  });

  const server = createServer((socket) => {
    if (isClosed) {
      socket.destroy();
      return;
    }
    activeSockets.add(socket);
    socket.setEncoding("utf8");

    let buffer = "";
    let handled = false;

    const writeResponse = (response: SharedSessionResponse) => {
      if (!socket.destroyed) {
        socket.end(`${JSON.stringify(response)}\n`);
      }
    };

    const handleLine = async (line: string) => {
      if (handled) return;
      handled = true;

      let requestId = "";
      try {
        const request = parseSharedRequest(line);
        requestId = request.id;
        const report = await controller.checkLatest(
          toCheckParams(request.params),
        );
        writeResponse({
          id: requestId,
          ok: true,
          report,
        });
      } catch (err) {
        const info = toExecutionErrorInfo(err);
        writeResponse({
          id: requestId,
          ok: false,
          error: info,
        });
      }
    };

    socket.on("data", (chunk: string) => {
      if (handled) return;
      buffer += chunk;
      const newlineIndex = buffer.indexOf("\n");
      if (newlineIndex === -1) return;
      const line = buffer.slice(0, newlineIndex).trim();
      buffer = buffer.slice(newlineIndex + 1);
      if (line.length > 0) {
        void handleLine(line);
      }
    });

    socket.on("error", () => {
      activeSockets.delete(socket);
      socket.destroy();
    });

    socket.on("close", () => {
      activeSockets.delete(socket);
    });
  });

  try {
    await listenEndpoint(server, endpoint);
  } catch (err) {
    await controller.close();
    throw err;
  }

  const close = async (): Promise<void> => {
    if (closePromise) {
      return closePromise;
    }
    closePromise = (async () => {
      isClosed = true;
      options.signal?.removeEventListener("abort", onAbort);
      for (const socket of activeSockets) {
        socket.destroy();
      }
      activeSockets.clear();
      await new Promise<void>((resolveServerClose) => {
        server.close(() => resolveServerClose());
      });
      if (!isNamedPipeEndpoint(endpoint)) {
        rmSync(endpoint, { force: true });
      }
      await controller.close();
      resolveClosed();
    })();
    return closePromise;
  };

  const onAbort = () => {
    void close();
  };
  options.signal?.addEventListener("abort", onAbort, { once: true });

  return {
    endpoint,
    get snapshotId() {
      return controller.session?.snapshotId ?? 0;
    },
    get projects() {
      return controller.session?.projects ?? [];
    },
    get closed() {
      return isClosed;
    },
    waitUntilClosed: () => closedDone,
    close,
  };
}

/**
 * Queries a running shared analysis session over its local socket/pipe endpoint
 * and returns the latest complete `CheckReport`.
 */
export async function requestSharedReport(
  options: RequestSharedReportOptions,
): Promise<CheckReport> {
  const endpoint = resolveEndpointPath(
    options.cwd ?? process.cwd(),
    options.endpoint,
  );

  return new Promise<CheckReport>((resolveReport, rejectReport) => {
    const socket = createConnection(endpoint);
    socket.setEncoding("utf8");

    let buffer = "";
    let settled = false;

    const finishError = (err: VeruleExecutionError) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      rejectReport(err);
    };

    const finishSuccess = (report: CheckReport) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      resolveReport(report);
    };

    if (options.timeoutMs !== undefined) {
      socket.setTimeout(options.timeoutMs, () => {
        finishError(
          new VeruleExecutionError(
            "ENGINE_ERROR",
            `Timed out waiting for shared session response from "${endpoint}".`,
          ),
        );
      });
    }

    socket.once("connect", () => {
      const request: SharedSessionRequest = {
        id: "1",
        method: "check",
        params: {
          ...(options.ruleIds !== undefined
            ? { ruleIds: options.ruleIds }
            : {}),
          ...(options.at !== undefined ? { at: options.at } : {}),
          ...(options.observe !== undefined
            ? { observe: options.observe }
            : {}),
          ...(options.observeLimit !== undefined
            ? { observeLimit: options.observeLimit }
            : {}),
        },
      };
      socket.write(`${JSON.stringify(request)}\n`);
    });

    socket.on("data", (chunk: string) => {
      if (settled) return;
      buffer += chunk;
      const newlineIndex = buffer.indexOf("\n");
      if (newlineIndex === -1) return;
      const line = buffer.slice(0, newlineIndex).trim();
      if (line.length === 0) return;

      let parsed: SharedSessionResponse;
      try {
        parsed = JSON.parse(line) as SharedSessionResponse;
      } catch (err) {
        finishError(
          new VeruleExecutionError(
            "ENGINE_ERROR",
            `Malformed response from shared session "${endpoint}": ${err instanceof Error ? err.message : String(err)}`,
            { cause: err },
          ),
        );
        return;
      }

      if (!parsed.ok) {
        finishError(
          new VeruleExecutionError(
            parsed.error.code,
            parsed.error.message,
            parsed.error.conflict !== undefined
              ? { conflict: parsed.error.conflict }
              : undefined,
          ),
        );
        return;
      }
      finishSuccess(parsed.report);
    });

    socket.once("end", () => {
      if (!settled) {
        finishError(
          new VeruleExecutionError(
            "ENGINE_ERROR",
            `Shared session at "${endpoint}" closed the connection before sending a report.`,
          ),
        );
      }
    });

    socket.once("error", (err) => {
      finishError(
        new VeruleExecutionError(
          "ENGINE_ERROR",
          `Failed to connect to shared session at "${endpoint}": ${err.message}`,
          { cause: err },
        ),
      );
    });
  });
}
