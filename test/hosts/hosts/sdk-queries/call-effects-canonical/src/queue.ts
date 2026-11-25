import { invokeSafely } from "./helper.js";

export interface Job {
  (): void;
  flags: number;
}

const queue: Job[] = [];

export function runDirectAndIndirect(skip: boolean): void {
  try {
    for (let i = 0; i < queue.length; i++) {
      const cb = queue[i];
      if (cb) {
        if (skip) {
          continue;
        }
        cb();
        if (!(cb.flags & 4)) {
          cb.flags &= ~1;
        }
      }
    }
    for (let j = 0; j < queue.length; j++) {
      const job = queue[j];
      if (job) {
        invokeSafely(job);
        job.flags &= ~1;
      }
    }
  } finally {
    queue.length = 0;
  }
}

export function runWithConcreteCallback(job: Job): void {
  const onDone = (): void => {
    job.flags |= 1;
  };
  invokeSafely(onDone);
}
