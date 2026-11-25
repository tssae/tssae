declare module "react/jsx-runtime" {
  export function jsx(type: unknown, props: unknown): unknown;
  export function jsxs(type: unknown, props: unknown): unknown;
  export const Fragment: unique symbol;
}

declare namespace JSX {
  interface IntrinsicElements {
    div: Record<string, unknown>;
  }
}
