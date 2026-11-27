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

export function runValuesAndCallsScenarios(): void {
  runDestructureDefault();
  runNullishAndOptional();
  runAccessorValue();
  runClosureReassignValue();
  runIteratorValue();
  invokeWorker(new DerivedWorker());
  invokeReassignedClosure();
  invokeOptionalTarget({ ping: optionalPing });
}
