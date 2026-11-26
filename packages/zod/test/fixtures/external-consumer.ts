import {
  buildValidatorInputFields,
  extractZodFieldModifiers,
  extractZodFields,
  extractZodInputFields,
  extractZodOutputFields,
  zodValidator,
  type ExternalSchemaResolver,
  type ValidatorFieldExtractor,
  type ValidatorFieldModifierResult,
  type ValidatorInputFieldsResult,
  type ValidatorOutputFieldsResult,
} from "@tssae/zod";
import {
  TssaeError,
  evidenceOf,
  extractBasisEvidence,
  openProject,
  type Gap,
  type ProjectContext,
  type SyntaxRef,
} from "tssae";

export async function inspectValidators(
  cwd: string,
  extractor: ValidatorFieldExtractor = zodValidator,
): Promise<{
  readonly inputs: readonly ValidatorInputFieldsResult[];
  readonly outputs: readonly ValidatorOutputFieldsResult[];
  readonly both: {
    readonly inputs: readonly ValidatorInputFieldsResult[];
    readonly outputs: readonly ValidatorOutputFieldsResult[];
  };
  readonly modifiers: readonly ValidatorFieldModifierResult[];
  readonly gaps: readonly Gap[];
}> {
  const gaps: Gap[] = [];
  const session = await openProject({ cwd });
  try {
    const ctx: ProjectContext = session.project(undefined, {
      observer: {
        onGaps: (reported) => gaps.push(...reported),
      },
    });
    const customResolver: ExternalSchemaResolver = {
      id: "noop",
      resolveSchemaCall: async () => undefined,
    };
    const inputs = await extractor.extractInputFields(ctx, {
      scope: "calls",
      externalSchemaResolvers: [customResolver],
    });
    const outputs = await extractor.extractOutputFields(ctx, {
      scope: "schemas",
    });
    const both = await extractZodFields(ctx, { scope: "all" });
    const refs: SyntaxRef[] = inputs.map((item) => item.target);
    const directInputs = await extractZodInputFields(ctx, { targets: refs });
    const directOutputs = await extractZodOutputFields(ctx, { targets: refs });
    const modifiers = await extractZodFieldModifiers(ctx, refs);

    const synthetic = buildValidatorInputFields({
      siteKind: "call",
      target: inputs[0]!.target,
      at: inputs[0]!.at,
      providerId: "custom",
      fields: {
        id: { name: "id", requirement: "required", nullable: false },
      },
      hasDynamicFields: false,
      evidence: [],
      gaps: [],
    });
    const combinedEvidence = extractBasisEvidence([
      ...directInputs,
      ...directOutputs,
      synthetic,
    ]);
    const itemEvidence = directInputs[0] ? evidenceOf(directInputs[0]) : [];
    if (combinedEvidence.length < itemEvidence.length) {
      throw new Error("Expected extractBasisEvidence to preserve evidence");
    }

    return { inputs, outputs, both, modifiers, gaps };
  } catch (error) {
    if (error instanceof TssaeError) {
      throw error;
    }
    throw error;
  } finally {
    await session.close();
  }
}
