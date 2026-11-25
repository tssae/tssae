import {
  ValidatorInputFieldsFact,
  ValidatorOutputFieldsFact,
  zodAdapter,
} from "@verule/adapter-zod";
import { openProject } from "tssae";

export async function runExternalZodConsumer(cwd: string): Promise<void> {
  const session = await openProject({
    cwd,
    tsconfig: "tsconfig.json",
    adapters: [zodAdapter],
  });
  try {
    const ctx = session.project();
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
  } finally {
    await session.close();
  }
}
