import {
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
  zodAdapter,
} from "@verule/adapter-zod";
import { defineConfig, defineRule } from "@verule/sdk";

const rule = defineRule({
  id: "consumer-zod-rule",
  run: async (ctx) => {
    const inputs = await ctx.facts(ValidatorInputFieldsFact, {
      providerId: "zod",
    });
    const outputs = await ctx.facts(ValidatorOutputFieldsFact);
    for (const item of inputs) {
      void item.value.requiredFields;
      void item.value.defaultedFields;
    }
    for (const item of outputs) {
      void item.value.presentFields;
    }
  },
});

export default defineConfig({
  tsconfig: "tsconfig.json",
  adapters: [zodAdapter],
  rules: [rule],
});
