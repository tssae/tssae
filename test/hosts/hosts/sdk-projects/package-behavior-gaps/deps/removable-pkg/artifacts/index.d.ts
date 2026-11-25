export interface Item {
  readonly name: string;
}
export declare function modeledOp<T extends Item>(item: T): T;
export declare function unmodeledOp<T extends Item>(item: T): T;
