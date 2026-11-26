import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { performance } from "node:perf_hooks";
import {
  createInterface,
  type Interface as ReadlineInterface,
} from "node:readline";

import { engineExecutablePath } from "./binary.ts";
import type { EngineCallIoMetrics, EngineRpcMetrics } from "./observation.ts";
import type { SummaryConflictDetails } from "./summaries.ts";
import { TssaeError, type ObserveMode, type TssaeErrorCode } from "./types.ts";

export type EngineObserveRequest = {
  readonly mode: ObserveMode;
  readonly maxItems?: number;
  readonly maxBytes?: number;
};

export type RpcRequestOptions = {
  readonly observe?: EngineObserveRequest;
  readonly onMetrics?: (metrics: EngineCallIoMetrics) => void;
};

type RpcResponse =
  | {
      readonly id: number;
      readonly ok: true;
      readonly result: unknown;
      readonly metrics?: EngineRpcMetrics;
    }
  | {
      readonly id: number;
      readonly ok: false;
      readonly error: {
        readonly code: string;
        readonly message: string;
        readonly details?: unknown;
      };
    };

type PendingCall = {
  readonly isQueryBatch: boolean;
  readonly bytesSent: number;
  readonly encodeMs: number;
  readonly getWriteMs: () => number;
  readonly onMetrics: ((metrics: EngineCallIoMetrics) => void) | undefined;
  readonly resolve: (value: unknown) => void;
  readonly reject: (reason: Error) => void;
};

function mapEngineErrorCode(code: string): TssaeErrorCode {
  switch (code) {
    case "PROJECT_LOAD_ERROR":
    case "STALE_REFERENCE":
    case "CROSS_PROJECT_REFERENCE":
    case "STALE_SNAPSHOT":
    case "NATIVE_ADAPTER_LOAD":
    case "NATIVE_ADAPTER_EXTRACT":
    case "FACT_CONFLICT":
    case "SUMMARY_CONFLICT":
      return code;
    default:
      return "ENGINE_ERROR";
  }
}

/** Manages a single `tssae-engine serve --stdio` subprocess over NDJSON RPC. */
export class EngineClient {
  readonly #child: ChildProcessWithoutNullStreams;
  readonly #lines: ReadlineInterface;
  readonly #pending = new Map<number, PendingCall>();
  readonly #stderrChunks: string[] = [];
  #nextId = 1;
  #closed = false;

  constructor() {
    this.#child = spawn(engineExecutablePath, ["serve", "--stdio"], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.#lines = createInterface({
      input: this.#child.stdout,
      crlfDelay: Infinity,
    });

    this.#child.stderr.setEncoding("utf8");
    this.#child.stderr.on("data", (chunk: string) => {
      this.#stderrChunks.push(chunk);
    });

    this.#lines.on("line", (line) => {
      if (line.trim() === "") return;
      let parsed: RpcResponse;
      try {
        parsed = JSON.parse(line) as RpcResponse;
      } catch (err) {
        this.#failAll(
          new TssaeError(
            "ENGINE_ERROR",
            `Malformed engine response: ${String(err)}`,
          ),
        );
        return;
      }

      const call = this.#pending.get(parsed.id);
      if (!call) return;
      this.#pending.delete(parsed.id);

      if (call.onMetrics) {
        const bytesReceived = Buffer.byteLength(line, "utf8") + 1;
        call.onMetrics({
          isQueryBatch: call.isQueryBatch,
          bytesSent: call.bytesSent,
          bytesReceived,
          encodeMs: call.encodeMs,
          writeMs: call.getWriteMs(),
          ...(parsed.ok && parsed.metrics !== undefined
            ? { engineMetrics: parsed.metrics }
            : {}),
        });
      }

      if (parsed.ok) {
        call.resolve(parsed.result);
      } else {
        const mappedCode = mapEngineErrorCode(parsed.error.code);
        call.reject(
          new TssaeError(
            mappedCode,
            parsed.error.message,
            mappedCode === "SUMMARY_CONFLICT" &&
              parsed.error.details !== undefined
              ? {
                  summaryConflict: parsed.error
                    .details as SummaryConflictDetails,
                }
              : undefined,
          ),
        );
      }
    });

    this.#child.on("error", (err) => {
      this.#failAll(
        new TssaeError(
          "ENGINE_ERROR",
          `Failed to start analysis engine: ${err.message}`,
          {
            cause: err,
          },
        ),
      );
    });

    this.#child.on("exit", (code, signal) => {
      if (!this.#closed || this.#pending.size > 0) {
        const stderrText = this.#stderrChunks.join("").trim();
        const detail = stderrText !== "" ? `: ${stderrText}` : "";
        this.#failAll(
          new TssaeError(
            "ENGINE_ERROR",
            `Analysis engine exited unexpectedly (code=${String(code)}, signal=${String(signal)})${detail}`,
          ),
        );
      }
    });
  }

  #failAll(error: TssaeError): void {
    for (const pending of this.#pending.values()) {
      pending.reject(error);
    }
    this.#pending.clear();
  }

  request<T>(
    method: string,
    params: Record<string, unknown>,
    options?: RpcRequestOptions,
  ): Promise<T> {
    if (this.#closed) {
      return Promise.reject(
        new TssaeError("SESSION_CLOSED", "Analysis engine session is closed."),
      );
    }

    const id = this.#nextId++;
    const trackMetrics = options?.onMetrics !== undefined;
    const observePayload =
      options?.observe && options.observe.mode !== "off"
        ? options.observe
        : undefined;

    const encodeStart = trackMetrics ? performance.now() : 0;
    const payload = observePayload
      ? { id, method, params, observe: observePayload }
      : { id, method, params };
    const line = `${JSON.stringify(payload)}\n`;
    const encodeMs = trackMetrics ? performance.now() - encodeStart : 0;
    const bytesSent = trackMetrics ? Buffer.byteLength(line, "utf8") : 0;

    return new Promise<T>((resolve, reject) => {
      let writeMs = 0;
      const writeStart = trackMetrics ? performance.now() : 0;

      this.#pending.set(id, {
        isQueryBatch: method === "query.batch",
        bytesSent,
        encodeMs,
        getWriteMs: () => writeMs,
        onMetrics: options?.onMetrics,
        resolve: (value) => resolve(value as T),
        reject,
      });

      this.#child.stdin.write(line, "utf8", (err) => {
        if (trackMetrics) {
          writeMs = performance.now() - writeStart;
        }
        if (err) {
          this.#pending.delete(id);
          reject(
            new TssaeError(
              "ENGINE_ERROR",
              `Failed to write request to analysis engine: ${err.message}`,
              { cause: err },
            ),
          );
        }
      });
    });
  }

  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    this.#lines.close();

    if (this.#child.exitCode !== null || this.#child.signalCode !== null) {
      return;
    }

    await new Promise<void>((resolve) => {
      const timer = setTimeout(() => {
        this.#child.kill("SIGKILL");
        resolve();
      }, 3_000);
      timer.unref();

      this.#child.once("exit", () => {
        clearTimeout(timer);
        resolve();
      });

      this.#child.stdin.end();
    });
  }
}
