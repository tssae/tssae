import {
  enterCallSite,
  exitCallSite,
  recordCallee,
  sink,
  source,
} from "./probe.ts";

export function runDestructureDefault(): void {
  const token = source("val-destructure-default", { kind: "destructure" });
  const empty: { current?: { kind: string } } = {};
  const { current = token } = empty;
  sink("val-destructure-default", current);
}

export function runNullishAndOptional(): void {
  const fallback = source("val-nullish-optional", { kind: "nullish" });
  const box: { inner?: { value?: { kind: string } } } = {};
  const picked = box.inner?.value ?? fallback;
  sink("val-nullish-optional", picked);
}

class TokenBox {
  #stored: { kind: string } | undefined;

  get current(): { kind: string } | undefined {
    recordCallee("TokenBox.get");
    return this.#stored;
  }

  set current(next: { kind: string } | undefined) {
    recordCallee("TokenBox.set");
    this.#stored = next;
  }
}

export function runAccessorValue(): void {
  const token = source("val-accessor", { kind: "accessor" });
  const box = new TokenBox();
  box.current = token;
  enterCallSite("callee-accessor-get");
  let readBack: { kind: string } | undefined;
  try {
    readBack = box.current;
  } finally {
    exitCallSite();
  }
  sink("val-accessor", readBack);
}

export function runClosureReassignValue(): void {
  let holder: { kind: string } = { kind: "initial" };
  const token = source("val-closure-reassign", { kind: "closure" });
  const update = (): void => {
    holder = token;
  };
  update();
  sink("val-closure-reassign", holder);
}

function* yieldSingle<T>(item: T): Generator<T> {
  yield item;
}

export function runIteratorValue(): void {
  const token = source("val-iterator", { kind: "iterator" });
  for (const item of yieldSingle(token)) {
    sink("val-iterator", item);
  }
}

export function runGeneratorNextValue(): void {
  const token = source("val-generator-next-value", { kind: "gen-next" });
  sink("val-generator-next-value", yieldSingle(token).next().value);
}

async function* yieldMappedArray<T>(item: T): AsyncGenerator<T[]> {
  yield [item].map((x) => x);
}

export async function runAsyncGeneratorContainerValue(): Promise<void> {
  const token = source("val-async-generator-array-elem", {
    kind: "async-gen-array",
  });
  for await (const arr of yieldMappedArray(token)) {
    sink("val-async-generator-array-elem", arr[0]);
  }
}

async function* yieldWrappedPromise<T>(item: T): AsyncGenerator<T> {
  yield Promise.resolve(item);
}

function* syncYieldMappedArray<T>(item: T): Generator<T[]> {
  yield [item].map((x) => x);
}

async function* delegateSyncMappedArray<T>(item: T): AsyncGenerator<T[]> {
  yield* syncYieldMappedArray(item);
}

function asMaybeAsyncStep<R>(step: R): R | Promise<R> {
  return step;
}

export async function runAsyncGeneratorYieldPromiseScenarios(): Promise<void> {
  const tokenForAwait = source("val-async-gen-yield-promise", {
    kind: "async-gen-yield-promise",
  });
  for await (const val of yieldWrappedPromise(tokenForAwait)) {
    sink("val-async-gen-yield-promise", val);
  }

  const tokenNext = source("val-async-gen-yield-promise-next", {
    kind: "async-gen-yield-promise-next",
  });
  const nextItem = (await yieldWrappedPromise(tokenNext).next()).value;
  sink("val-async-gen-yield-promise-next", nextItem);

  const tokenDelegated = source("val-async-delegate-sync-gen-array", {
    kind: "async-delegate-sync-gen-array",
  });
  for await (const arr of delegateSyncMappedArray(tokenDelegated)) {
    sink("val-async-delegate-sync-gen-array", arr[0]);
  }

  const tokenAwaitSyncStep = source("val-await-sync-gen-next-value", {
    kind: "await-sync-gen-next-value",
  });
  const syncStep = await asMaybeAsyncStep(
    yieldSingle(tokenAwaitSyncStep).next(),
  );
  sink("val-await-sync-gen-next-value", syncStep.value);
}

export class BaseWorker {
  execute(): void {
    recordCallee("BaseWorker.execute");
  }
}

export class DerivedWorker extends BaseWorker {
  override execute(): void {
    recordCallee("DerivedWorker.execute");
  }
}

export function invokeWorker(worker: BaseWorker): void {
  enterCallSite("callee-polymorphic");
  try {
    worker.execute();
  } finally {
    exitCallSite();
  }
}

function initialTarget(): void {
  recordCallee("initialTarget");
}

function reassignedTarget(): void {
  recordCallee("reassignedTarget");
}

export function invokeReassignedClosure(): void {
  let handler = initialTarget;
  const swap = (): void => {
    handler = reassignedTarget;
  };
  swap();
  enterCallSite("callee-closure-reassign");
  try {
    handler();
  } finally {
    exitCallSite();
  }
}

export function invokeOptionalTarget(helper?: { ping: () => void }): void {
  enterCallSite("callee-optional-present");
  try {
    helper?.ping();
  } finally {
    exitCallSite();
  }
}

function optionalPing(): void {
  recordCallee("optionalPing");
}

export async function runValuesAndCallsScenarios(): Promise<void> {
  runDestructureDefault();
  runNullishAndOptional();
  runAccessorValue();
  runClosureReassignValue();
  runIteratorValue();
  runGeneratorNextValue();
  await runAsyncGeneratorContainerValue();
  await runAsyncGeneratorYieldPromiseScenarios();
  invokeWorker(new DerivedWorker());
  invokeReassignedClosure();
  invokeOptionalTarget({ ping: optionalPing });
}
