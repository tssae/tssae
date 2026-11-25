import lintConfiguration from "@verule/tooling-config/oxlint";
import { defineConfig } from "oxlint";

export default defineConfig({
  extends: [lintConfiguration],
  options: { typeAware: true },
});
