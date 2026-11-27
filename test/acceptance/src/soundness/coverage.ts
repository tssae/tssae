import { readFileSync } from "node:fs";
import { Session } from "node:inspector/promises";
import type { Profiler } from "node:inspector/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export type ExecutionCountOptions = {
  /**
   * When true, a composite expression (e.g. `obj?.fn()` or `a && b()`) whose
   * outer span executed is reported as `0` if any strictly inner subrange
   * (such as `?.fn()` or `&& b()`) has V8 execution count `0`.
   */
  readonly shortCircuitSubrangeZeroAsSkipped?: boolean | undefined;
};

export type PreciseCoverageSnapshot = {
  readonly getExecutionCount: (
    filePath: string,
    utf8Start: number,
    utf8End: number,
    options?: ExecutionCountOptions,
  ) => number;
};

type IndexedRange = {
  readonly startOffset: number;
  readonly endOffset: number;
  readonly count: number;
  readonly fnSpan: number;
};

type IndexedScript = {
  readonly utf8Bytes: Buffer;
  readonly ranges: readonly IndexedRange[];
};

function indexScriptCoverage(
  filePath: string,
  script: Profiler.ScriptCoverage | undefined,
): IndexedScript {
  const utf8Bytes = readFileSync(filePath);
  if (!script) {
    return { utf8Bytes, ranges: [] };
  }

  const ranges: IndexedRange[] = [];
  for (const fn of script.functions) {
    const rootRange = fn.ranges[0];
    const fnSpan = rootRange
      ? rootRange.endOffset - rootRange.startOffset
      : Number.MAX_SAFE_INTEGER;
    for (const r of fn.ranges) {
      ranges.push({
        startOffset: r.startOffset,
        endOffset: r.endOffset,
        count: r.count,
        fnSpan,
      });
    }
  }
  return { utf8Bytes, ranges };
}

function utf8ToUtf16Offset(utf8Bytes: Buffer, utf8ByteOffset: number): number {
  const clamped = Math.max(0, Math.min(utf8ByteOffset, utf8Bytes.length));
  return utf8Bytes.subarray(0, clamped).toString("utf8").length;
}

/**
 * Executes `execute()` under V8 `Profiler.takePreciseCoverage` without altering
 * source bytes, and maps TSSAE UTF-8 byte offsets to V8 UTF-16 code-unit spans.
 */
export async function collectPreciseCoverage(
  filePaths: readonly string[],
  execute: () => Promise<void> | void,
): Promise<PreciseCoverageSnapshot> {
  const inspector = new Session();
  inspector.connect();
  let result: readonly Profiler.ScriptCoverage[] = [];

  try {
    await inspector.post("Profiler.enable");
    await inspector.post("Profiler.startPreciseCoverage", {
      callCount: true,
      detailed: true,
    });
    try {
      await execute();
      const taken = await inspector.post("Profiler.takePreciseCoverage");
      result = taken.result;
    } finally {
      await inspector.post("Profiler.stopPreciseCoverage");
      await inspector.post("Profiler.disable");
    }
  } finally {
    inspector.disconnect();
  }

  const byUrl = new Map<string, Profiler.ScriptCoverage>();
  for (const entry of result) {
    if (entry.url.length > 0) {
      byUrl.set(entry.url, entry);
    }
  }

  const indexedByPath = new Map<string, IndexedScript>();
  for (const filePath of filePaths) {
    const absPath = resolve(filePath);
    const url = pathToFileURL(absPath).href;
    indexedByPath.set(absPath, indexScriptCoverage(absPath, byUrl.get(url)));
  }

  return {
    getExecutionCount(
      filePath: string,
      utf8Start: number,
      utf8End: number,
      options?: ExecutionCountOptions,
    ): number {
      const absPath = resolve(filePath);
      const indexed = indexedByPath.get(absPath);
      if (!indexed) {
        return 0;
      }

      const utf16Start = utf8ToUtf16Offset(indexed.utf8Bytes, utf8Start);
      const utf16End = utf8ToUtf16Offset(indexed.utf8Bytes, utf8End);
      const targetSpan = utf16End - utf16Start;

      let bestRange: IndexedRange | undefined;
      let bestSpan = Number.MAX_SAFE_INTEGER;
      let bestFnSpan = Number.MAX_SAFE_INTEGER;

      for (const r of indexed.ranges) {
        if (r.startOffset <= utf16Start && utf16End <= r.endOffset) {
          const span = r.endOffset - r.startOffset;
          if (span < bestSpan || (span === bestSpan && r.fnSpan < bestFnSpan)) {
            bestRange = r;
            bestSpan = span;
            bestFnSpan = r.fnSpan;
          }
        }
      }

      if (!bestRange) {
        return 0;
      }

      if (
        options?.shortCircuitSubrangeZeroAsSkipped === true &&
        bestRange.count > 0
      ) {
        for (const r of indexed.ranges) {
          const span = r.endOffset - r.startOffset;
          if (
            r.startOffset >= utf16Start &&
            r.endOffset <= utf16End &&
            span < targetSpan &&
            r.count === 0
          ) {
            return 0;
          }
        }
      }

      return bestRange.count;
    },
  };
}
