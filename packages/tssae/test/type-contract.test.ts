import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  installPackedWorkspacePackages,
  withTempHost,
} from "@tssae/test-hosts";

const sdkRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const tscBin = join(sdkRoot, "node_modules", ".bin", "tsc");
const validConsumerFixturePath = fileURLToPath(
  new URL("./fixtures/type-contract/valid-consumer.ts", import.meta.url),
);

async function prepareExternalConsumer(root: string): Promise<void> {
  await installPackedWorkspacePackages(root, ["tssae"], {
    cwd: sdkRoot,
  });

  writeFileSync(
    join(root, "tsconfig.json"),
    JSON.stringify(
      {
        compilerOptions: {
          strict: true,
          exactOptionalPropertyTypes: true,
          noUncheckedIndexedAccess: true,
          target: "ES2022",
          module: "NodeNext",
          moduleResolution: "NodeNext",
          types: [],
          noEmit: true,
        },
        include: ["src/**/*.ts"],
      },
      null,
      2,
    ),
    "utf8",
  );
}

function runConsumerTsc(
  root: string,
  sourceCode: string,
): { readonly ok: boolean; readonly output: string } {
  const srcDir = join(root, "src");
  rmSync(srcDir, { recursive: true, force: true });
  mkdirSync(srcDir, { recursive: true });
  writeFileSync(join(srcDir, "consumer.ts"), sourceCode, "utf8");

  try {
    const stdout = execFileSync(
      tscBin,
      ["-p", join(root, "tsconfig.json"), "--pretty", "false"],
      { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
    );
    return { ok: true, output: stdout };
  } catch (err: unknown) {
    const execErr = err as {
      readonly stdout?: string;
      readonly stderr?: string;
    };
    return {
      ok: false,
      output: `${execErr.stdout ?? ""}\n${execErr.stderr ?? ""}`.trim(),
    };
  }
}

void describe("tssae external TypeScript consumer contract", () => {
  void it("compiles valid call -> signature -> instantiated parameter composition against built .d.ts", async () => {
    await withTempHost({}, async (host) => {
      await prepareExternalConsumer(host.rootDir);
      const validCode = readFileSync(validConsumerFixturePath, "utf8");
      const result = runConsumerTsc(host.rootDir, validCode);
      assert.equal(result.ok, true, `Expected tsc to pass:\n${result.output}`);
    });
  });

  void it("rejects mismatched ProgramRef kinds and reads of unselected projection fields at compile time", async () => {
    await withTempHost({}, async (host) => {
      await prepareExternalConsumer(host.rootDir);

      const negativeCases: readonly {
        readonly name: string;
        readonly body: string;
        readonly expectedPattern: RegExp;
      }[] = [
        {
          name: "DeclarationRef passed to projectCalls",
          body: `
            declare const declRef: DeclarationRef;
            await ctx.projectCalls([declRef], { select: { location: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "SyntaxRef passed to projectCalls",
          body: `
            declare const syntaxRef: SyntaxRef;
            await ctx.projectCalls([syntaxRef], { select: { location: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "TypeRef passed to projectCalls",
          body: `
            declare const typeRef: TypeRef;
            await ctx.projectCalls([typeRef], { select: { location: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "CallRef passed to projectSignatures",
          body: `
            declare const callRef: CallRef;
            await ctx.projectSignatures([callRef], { select: { returnType: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "DeclarationRef passed to projectSignatures",
          body: `
            declare const declRef: DeclarationRef;
            await ctx.projectSignatures([declRef], { select: { returnType: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "SyntaxRef passed to projectSignatures",
          body: `
            declare const syntaxRef: SyntaxRef;
            await ctx.projectSignatures([syntaxRef], { select: { returnType: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "TypeRef passed to projectSignatures",
          body: `
            declare const typeRef: TypeRef;
            await ctx.projectSignatures([typeRef], { select: { returnType: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "CallRef passed to isAssignableTo",
          body: `
            declare const callRef: CallRef;
            declare const typeRef: TypeRef;
            await ctx.isAssignableTo([{ source: callRef, target: typeRef }]);
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "TypeRef passed to syntaxOf",
          body: `
            declare const typeRef: TypeRef;
            await ctx.syntaxOf([typeRef], { select: { location: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "unselected returnType read on projectCalls result",
          body: `
            declare const callRef: CallRef;
            const [row] = await ctx.projectCalls([callRef], { select: { signature: true } });
            void row?.returnType;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "unselected returnType read on projectSignatures result",
          body: `
            declare const sigRef: SignatureRef;
            const [row] = await ctx.projectSignatures([sigRef], { select: { instantiatedParameterTypes: [0] } });
            void row?.returnType;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "unselected location read on resolveDeclarations result",
          body: `
            declare const syntaxRef: SyntaxRef;
            const [row] = await ctx.resolveDeclarations([syntaxRef], { select: { name: true } });
            void row?.location;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "unselected location read on syntaxOf without select",
          body: `
            declare const declRef: DeclarationRef;
            const [row] = await ctx.syntaxOf([declRef]);
            void row?.location;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "SyntaxRef passed to calleesOf",
          body: `
            declare const syntaxRef: SyntaxRef;
            await ctx.calleesOf([syntaxRef]);
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "CallRef passed to callersOf",
          body: `
            declare const callRef: CallRef;
            await ctx.callersOf([callRef]);
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "TypeRef passed to valuePropagation",
          body: `
            declare const typeRef: TypeRef;
            declare const syntaxRef: SyntaxRef;
            await ctx.valuePropagation({ propagation: "value", from: typeRef, to: [syntaxRef] });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "SyntaxRef passed to projectInstances",
          body: `
            const syntaxRef = undefined as unknown as SyntaxRef;
            await ctx.projectInstances([syntaxRef], { select: { instanceKey: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "DeclarationRef passed to projectInstances",
          body: `
            const declRef = undefined as unknown as DeclarationRef;
            await ctx.projectInstances([declRef], { select: { instanceKey: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "CallRef passed to projectInstances",
          body: `
            const callRef = undefined as unknown as CallRef;
            await ctx.projectInstances([callRef], { select: { instanceKey: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "InstanceRef passed to projectCalls",
          body: `
            const instRef = undefined as unknown as InstanceRef;
            await ctx.projectCalls([instRef], { select: { location: true } });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "InstanceRef passed to resolveInstances",
          body: `
            const instRef = undefined as unknown as InstanceRef;
            await ctx.resolveInstances([instRef]);
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "TypeRef passed to resolveInstances",
          body: `
            const typeRef = undefined as unknown as TypeRef;
            await ctx.resolveInstances([typeRef]);
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "unselected allocationLocation read on projectInstances result",
          body: `
            const instRef = undefined as unknown as InstanceRef;
            const [row] = await ctx.projectInstances([instRef], { select: { instanceKey: true } });
            void row?.allocationLocation;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "unselected originKind read on projectInstances without select",
          body: `
            const instRef = undefined as unknown as InstanceRef;
            const [row] = await ctx.projectInstances([instRef]);
            void row?.originKind;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "InstanceRef passed to callEffects targets",
          body: `
            const instRef = undefined as unknown as InstanceRef;
            await ctx.callEffects({ targets: [instRef] });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "SyntaxRef passed to instanceCleanup targets (requires InstanceRef)",
          body: `
            const synRef = undefined as unknown as SyntaxRef;
            await ctx.instanceCleanup({ scopes: [synRef], targets: [synRef] });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "TypeRef passed to instanceCleanup scopes",
          body: `
            const typeRef = undefined as unknown as TypeRef;
            await ctx.instanceCleanup({ scopes: [typeRef] });
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "CallEffects fieldWrite syntax cannot be passed as InstanceRef",
          body: `
            const callRef = undefined as unknown as CallRef;
            const [res] = await ctx.callEffects({ targets: [callRef] });
            const w = res?.fieldWrites[0];
            if (w) {
              await ctx.projectInstances([w.syntax], { select: { instanceKey: true } });
            }
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "aggregate ValuePropagationResult cannot be passed to extractBasisEvidence",
          body: `
            declare const synRef: SyntaxRef;
            const prop = await ctx.valuePropagation({ from: synRef, to: [synRef] });
            extractBasisEvidence([prop]);
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "aggregate CallEffectsResult cannot be passed to extractBasisEvidence",
          body: `
            declare const callRef: CallRef;
            const [eff] = await ctx.callEffects({ targets: [callRef] });
            if (eff) {
              extractBasisEvidence([eff]);
            }
          `,
          expectedPattern: /TS2322|TS2345/u,
        },
        {
          name: "counterexamplePath cannot be read on BranchCoverageItem before narrowing to disproved",
          body: `
            declare const synRef: SyntaxRef;
            const [cov] = await ctx.branchCoverage({ branches: [], targets: [synRef] });
            void cov?.counterexamplePath;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "gaps cannot be read on BranchCoverageItem before narrowing to inconclusive",
          body: `
            declare const synRef: SyntaxRef;
            const [cov] = await ctx.branchCoverage({ branches: [], targets: [synRef] });
            void cov?.gaps;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "mayThrowSites cannot be read on implicitExceptions before narrowing",
          body: `
            declare const synRef: SyntaxRef;
            const [cov] = await ctx.branchCoverage({ branches: [], targets: [synRef] });
            void cov?.implicitExceptions.mayThrowSites;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "counterexamplePath cannot be read on implicitExceptions before narrowing to disproved",
          body: `
            declare const synRef: SyntaxRef;
            const [cov] = await ctx.branchCoverage({ branches: [], targets: [synRef] });
            void cov?.implicitExceptions.counterexamplePath;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "gaps cannot be read on implicitExceptions before narrowing to inconclusive",
          body: `
            declare const synRef: SyntaxRef;
            const [cov] = await ctx.branchCoverage({ branches: [], targets: [synRef] });
            void cov?.implicitExceptions.gaps;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "gaps cannot be read on CallTargetsResult before narrowing to incomplete",
          body: `
            declare const callRef: CallRef;
            const [res] = await ctx.calleesOf([callRef]);
            void res?.gaps;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "gaps cannot be read on DeclarationCallersResult before narrowing to incomplete",
          body: `
            declare const declRef: DeclarationRef;
            const [res] = await ctx.callersOf([declRef]);
            void res?.gaps;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "paths cannot be read on ValuePropagationTargetResult before narrowing to proved",
          body: `
            declare const synRef: SyntaxRef;
            const res = await ctx.valuePropagation({ from: synRef, to: [synRef] });
            void res.results[0]?.paths;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "gaps cannot be read on ValuePropagationTargetResult before narrowing to inconclusive",
          body: `
            declare const synRef: SyntaxRef;
            const res = await ctx.valuePropagation({ from: synRef, to: [synRef] });
            void res.results[0]?.gaps;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "instance cannot be read on InstanceResolutionResult before narrowing to proved",
          body: `
            declare const synRef: SyntaxRef;
            const [res] = await ctx.resolveInstances([synRef]);
            void res?.instance;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "gaps cannot be read on InstanceResolutionResult before narrowing to inconclusive",
          body: `
            declare const synRef: SyntaxRef;
            const [res] = await ctx.resolveInstances([synRef]);
            void res?.gaps;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "gaps cannot be read on CallEffectsResult before narrowing to incomplete",
          body: `
            declare const callRef: CallRef;
            const [res] = await ctx.callEffects({ targets: [callRef] });
            void res?.gaps;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "uncleanedPath cannot be read on TargetInstanceCleanupResult before narrowing to disproved",
          body: `
            declare const synRef: SyntaxRef;
            const [scope] = await ctx.instanceCleanup({ scopes: [synRef] });
            void scope?.targetResults[0]?.uncleanedPath;
          `,
          expectedPattern: /TS2339/u,
        },
        {
          name: "mayThrowSites cannot be read on TargetInstanceCleanupResult implicitExceptions before narrowing",
          body: `
            declare const synRef: SyntaxRef;
            const [scope] = await ctx.instanceCleanup({ scopes: [synRef] });
            void scope?.targetResults[0]?.implicitExceptions.mayThrowSites;
          `,
          expectedPattern: /TS2339/u,
        },
      ];

      for (const testCase of negativeCases) {
        const snippet = [
          'import { extractBasisEvidence, type CallRef, type DeclarationRef, type InstanceRef, type ProjectContext, type SignatureRef, type SyntaxRef, type TypeRef } from "tssae";',
          "",
          "export async function checkContract(ctx: ProjectContext): Promise<void> {",
          testCase.body,
          "}",
          "",
        ].join("\n");
        const result = runConsumerTsc(host.rootDir, snippet);
        assert.equal(
          result.ok,
          false,
          `Expected tsc to reject case "${testCase.name}", but it compiled cleanly.`,
        );
        assert.match(
          result.output,
          testCase.expectedPattern,
          `Unexpected diagnostic for "${testCase.name}":\n${result.output}`,
        );
      }
    });
  });
});
