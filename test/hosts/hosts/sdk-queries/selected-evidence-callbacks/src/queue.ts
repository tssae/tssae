interface Job {
  flags: number;
  safe: number;
}
function invokeWithCallback(cb: () => void): void {
  cb();
}
export function flush(job: Job): void {
  job.safe = 1;
  invokeWithCallback(() => {
    job.flags = 0;
  });
}
