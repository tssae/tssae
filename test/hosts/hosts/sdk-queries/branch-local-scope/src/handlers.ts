const enum JobFlags {
  QUEUED = 1 << 0,
  ALLOW_RECURSE = 1 << 2,
}
interface Job {
  (): void;
  flags?: number;
}
declare function shouldSkip(job: Job): boolean;
declare function runWithHandling(job: Job): void;
declare const queue: Job[];

export function loopWithBypass(): void {
  for (let i = 0; i < queue.length; i++) {
    const job = queue[i];
    if (job) {
      if (shouldSkip(job)) {
        continue;
      }
      if (job.flags! & JobFlags.ALLOW_RECURSE) {
        job.flags! &= ~JobFlags.QUEUED;
      }
      job();
      job.flags! &= ~JobFlags.QUEUED;
    }
  }
}

export function tryLoopWithGuard(): void {
  try {
    for (let i = 0; i < queue.length; i++) {
      const job = queue[i];
      if (job) {
        if (shouldSkip(job)) {
          continue;
        }
        if (job.flags! & JobFlags.ALLOW_RECURSE) {
          job.flags! &= ~JobFlags.QUEUED;
        }
        runWithHandling(job);
        if (!(job.flags! & JobFlags.ALLOW_RECURSE)) {
          job.flags! &= ~JobFlags.QUEUED;
        }
      }
    }
  } finally {
    queue.length = 0;
  }
}

export function loopWithContinueGuard(): void {
  for (let i = 0; i < queue.length; i++) {
    const job = queue[i];
    if (job) {
      if (job.flags! & JobFlags.ALLOW_RECURSE) {
        continue;
      }
      job.flags! &= ~JobFlags.QUEUED;
    }
  }
}
