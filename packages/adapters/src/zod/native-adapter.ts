import { defineAdapter, defineFactProvider } from "tssae";

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
    defineFactProvider(ValidatorInputFieldsFact, (ctx, request) =>
      ctx.extractEngineFacts({
        factTypeId: request.factType.id,
        providerId: "zod",
        reportProviderId: "zod",
        ...(request.targets !== undefined ? { targets: request.targets } : {}),
        ...(request.condition !== undefined
          ? { condition: request.condition }
          : {}),
        ...(request.explain !== undefined ? { explain: request.explain } : {}),
      }),
    ),
    defineFactProvider(ValidatorOutputFieldsFact, (ctx, request) =>
      ctx.extractEngineFacts({
        factTypeId: request.factType.id,
        providerId: "zod",
        reportProviderId: "zod",
        ...(request.targets !== undefined ? { targets: request.targets } : {}),
        ...(request.condition !== undefined
          ? { condition: request.condition }
          : {}),
        ...(request.explain !== undefined ? { explain: request.explain } : {}),
      }),
    ),
    defineFactProvider(ValidatorFieldFact, (ctx, request) =>
      ctx.extractEngineFacts({
        factTypeId: request.factType.id,
        providerId: "zod",
        reportProviderId: "zod",
        ...(request.targets !== undefined ? { targets: request.targets } : {}),
        ...(request.explain !== undefined ? { explain: request.explain } : {}),
      }),
    ),
  ],
});

export default zodAdapter;
