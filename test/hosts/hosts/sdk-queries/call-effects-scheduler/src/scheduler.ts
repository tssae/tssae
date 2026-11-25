import { callWithErrorHandling } from "./errorHandling.js";

export const SchedulerJobFlags = {
  QUEUED: 1 << 0,
  PRE: 1 << 1,
  ALLOW_RECURSE: 1 << 2,
} as const;

export interface SchedulerJob {
  (): void;
  flags: number;
}

const queue: SchedulerJob[] = [];

export function queueJob(job: SchedulerJob): void {
  if (!(job.flags! & SchedulerJobFlags.QUEUED)) {
    job.flags! |= SchedulerJobFlags.QUEUED;
    queue.push(job);
  }
}

export function flushJobBuggy(job: SchedulerJob): void {
  if (job.flags! & SchedulerJobFlags.ALLOW_RECURSE) {
    job.flags! &= ~SchedulerJobFlags.QUEUED;
  }
  callWithErrorHandling(() => {
    queueJob(job);
  });
  job.flags! &= ~SchedulerJobFlags.QUEUED;
}

export function flushJobFixed(job: SchedulerJob): void {
  if (job.flags! & SchedulerJobFlags.ALLOW_RECURSE) {
    job.flags! &= ~SchedulerJobFlags.QUEUED;
  }
  callWithErrorHandling(() => {
    queueJob(job);
  });
  if (!(job.flags! & SchedulerJobFlags.ALLOW_RECURSE)) {
    job.flags! &= ~SchedulerJobFlags.QUEUED;
  }
}

export function flushJobDynamic(job: SchedulerJob): void {
  if (job.flags! & SchedulerJobFlags.ALLOW_RECURSE) {
    job.flags! &= ~SchedulerJobFlags.QUEUED;
  }
  callWithErrorHandling(job);
  job.flags! &= ~SchedulerJobFlags.QUEUED;
}
