function internalMake(tag: string): { tag: string } {
  return { tag };
}
export const publicMake = internalMake;
export default function defaultFactory(x: string): string {
  return x;
}
export class BaseBox {
  constructor(public readonly label: string) {}
  ping(): string {
    return this.label;
  }
}
