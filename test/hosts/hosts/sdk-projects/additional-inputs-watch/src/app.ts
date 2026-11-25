import { helper } from "./helper.js";
declare function usePackage(name: string): void;
export function run(): string {
  usePackage("zod");
  return helper();
}
