import { defineAdapter } from "tssae";

import { defineNativeFactProvider } from "../native-provider.ts";
import {
  ValidatorFieldFact,
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
} from "./facts.ts";

/** JavaScript query bridge loaded by the package's native adapter entry. */
const zodAdapter = defineAdapter({
  id: "zod",
  version: "0.1.0",
  description: "Static Zod schema and validator field extractor.",
  provides: [
    defineNativeFactProvider(ValidatorInputFieldsFact, {
      providerId: "zod",
      reportProviderId: "zod",
    }),
    defineNativeFactProvider(ValidatorOutputFieldsFact, {
      providerId: "zod",
      reportProviderId: "zod",
    }),
    defineNativeFactProvider(ValidatorFieldFact, {
      providerId: "zod",
      reportProviderId: "zod",
    }),
  ],
});

export default zodAdapter;
