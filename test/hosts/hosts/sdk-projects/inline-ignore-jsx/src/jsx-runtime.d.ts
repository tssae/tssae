declare module "react/jsx-runtime" {
  export namespace JSX {
    type Element = unknown;
    interface IntrinsicElements {
      [elemName: string]: Record<string, unknown>;
    }
  }
  export function jsx(...args: unknown[]): unknown;
  export function jsxs(...args: unknown[]): unknown;
}
