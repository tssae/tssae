export type ProbeSnapshot = {
  readonly arrivedPairs: ReadonlyArray<{
    readonly sinkTag: string;
    readonly sourceTags: readonly string[];
  }>;
  readonly callsBySite: Readonly<Record<string, readonly string[]>>;
  readonly writesBySite: Readonly<Record<string, readonly string[]>>;
  readonly disposedResources: Readonly<Record<string, boolean>>;
};

const sources: Array<{ readonly tag: string; readonly value: unknown }> = [];
const arrivedPairs: Array<{
  readonly sinkTag: string;
  readonly sourceTags: readonly string[];
}> = [];
const callSiteStack: string[] = [];
const callsBySite = new Map<string, Set<string>>();
const writesBySite = new Map<string, Set<string>>();
const disposedResources = new Map<string, boolean>();

export function resetProbeLedger(): void {
  sources.length = 0;
  arrivedPairs.length = 0;
  callSiteStack.length = 0;
  callsBySite.clear();
  writesBySite.clear();
  disposedResources.clear();
}

export function getProbeSnapshot(): ProbeSnapshot {
  const callsRecord: Record<string, readonly string[]> = {};
  for (const [site, targets] of callsBySite) {
    callsRecord[site] = [...targets];
  }
  const writesRecord: Record<string, readonly string[]> = {};
  for (const [site, props] of writesBySite) {
    writesRecord[site] = [...props];
  }
  const resourcesRecord: Record<string, boolean> = {};
  for (const [tag, disposed] of disposedResources) {
    resourcesRecord[tag] = disposed;
  }
  return {
    arrivedPairs: [...arrivedPairs],
    callsBySite: callsRecord,
    writesBySite: writesRecord,
    disposedResources: resourcesRecord,
  };
}

export function source<T>(tag: string, value: T): T {
  sources.push({ tag, value });
  return value;
}

export function sink<T>(tag: string, value: T): T {
  const matched: string[] = [];
  for (const entry of sources) {
    if (Object.is(entry.value, value)) {
      matched.push(entry.tag);
    }
  }
  arrivedPairs.push({ sinkTag: tag, sourceTags: matched });
  return value;
}

export function enterCallSite(callSiteTag: string): void {
  callSiteStack.push(callSiteTag);
}

export function exitCallSite(): void {
  callSiteStack.pop();
}

export function recordCallee(targetId: string): void {
  const activeSite = callSiteStack.at(-1);
  if (activeSite === undefined) {
    return;
  }
  let targets = callsBySite.get(activeSite);
  if (!targets) {
    targets = new Set<string>();
    callsBySite.set(activeSite, targets);
  }
  targets.add(targetId);
}

export function watchWrites<T extends object>(
  callSiteTag: string,
  target: T,
): T {
  let props = writesBySite.get(callSiteTag);
  if (!props) {
    props = new Set<string>();
    writesBySite.set(callSiteTag, props);
  }
  return new Proxy(target, {
    set(obj, prop, value, receiver): boolean {
      if (typeof prop === "string") {
        props.add(prop);
      }
      return Reflect.set(obj, prop, value, receiver);
    },
  });
}

export class TrackedHandle implements Disposable {
  readonly #tag: string;

  constructor(tag: string) {
    this.#tag = tag;
    disposedResources.set(tag, false);
  }

  dispose(): void {
    disposedResources.set(this.#tag, true);
  }

  [Symbol.dispose](): void {
    this.dispose();
  }
}
