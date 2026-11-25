export interface Payload {
  readonly id: string;
}
export declare function transformPayload<T extends Payload>(input: T): T;
