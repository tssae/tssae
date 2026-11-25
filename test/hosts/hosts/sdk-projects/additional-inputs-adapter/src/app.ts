declare function validatePayload(payload: { id: string }): void;
export function handle(): void {
  validatePayload({ id: "u1" });
}
