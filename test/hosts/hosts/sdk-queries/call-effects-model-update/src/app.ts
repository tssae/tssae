import { createExternalResource } from "@acme/lifecycle";
export function run(): void {
  const item = createExternalResource("x");
  const close = () => {
    item.dispose();
  };
  void close;
}
