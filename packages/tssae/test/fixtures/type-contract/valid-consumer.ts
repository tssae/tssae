import {
  TssaeError,
  createAdditionalInputRegistry,
  defineAdapter,
  defineFactProvider,
  defineFactType,
  defineModel,
  isSameInstance,
  openProject,
  type AdditionalInputEntry,
  type AdditionalInputRegistrar,
  type AdditionalInputSource,
  type CallEffectsResult,
  type CallRef,
  type DeclarationRef,
  type ExplainCleanupPath,
  type FactConflictDetails,
  type FactConflictParty,
  type InstanceCleanupScopeResult,
  type InstanceRef,
  type SignatureRef,
  type SyntaxRef,
  type TypeRef,
} from "tssae";

const extModel = defineModel({
  id: "ext-model",
  instanceBehaviors: [
    {
      kind: "allocates-fresh-instance",
      match: { container: "IMenuService", member: "createMenu" },
    },
    {
      kind: "forwards-argument",
      match: { container: "Disposable", member: "_register" },
      argumentIndex: 0,
    },
  ],
  cleanupOperations: [
    {
      kind: "disposes-target",
      match: { member: "dispose" },
      target: "receiver",
    },
    {
      kind: "replaces-previous-value",
      match: { container: "MutableDisposable", member: "value" },
      target: "receiver",
    },
  ],
  lifecycleEvents: [
    {
      kind: "lifecycle-end-callback",
      match: { symbol: "onDidDisconnect" },
      callbackArgumentIndex: 0,
    },
  ],
});

const registrar: AdditionalInputRegistrar = createAdditionalInputRegistry({
  cwd: "/tmp/workspace",
});
const adapterSource: AdditionalInputSource = {
  kind: "adapter",
  id: "demo-adapter",
};
const regEntry: AdditionalInputEntry = registrar.registerAdditionalInput(
  "allowed-packages.json",
  adapterSource,
);
void regEntry;

const sampleFact = defineFactType<
  "syntax",
  { readonly strict?: boolean } | undefined,
  readonly string[]
>({
  id: "demo.sample-fact",
  subjectKind: "syntax",
  merge: (existing, incoming) => {
    if (existing.value[0] === incoming.value[0]) {
      return { ok: true, value: [...existing.value, ...incoming.value] };
    }
    return {
      ok: false,
      conflict: {
        message: "Mismatched head tag",
        existingConclusion: existing.value[0] ?? "none",
        incomingConclusion: incoming.value[0] ?? "none",
      },
    };
  },
});

function inspectConflict(
  err: TssaeError,
): readonly FactConflictParty[] | undefined {
  const details: FactConflictDetails | undefined = err.conflict;
  return details?.parties;
}
void inspectConflict;

const sampleAdapter = defineAdapter({
  id: "demo-adapter",
  provides: [
    defineFactProvider(sampleFact, async (ctx) => {
      const adapterInput: AdditionalInputEntry = ctx.registerAdditionalInput(
        "allowed-packages.json",
      );
      if (adapterInput.exists) {
        const raw: string = ctx.readAdditionalInput("allowed-packages.json");
        void raw;
      }
      const nodes = await ctx.findSyntax({ kinds: ["CallExpression"] });
      return nodes.map((n) => ({ subject: n.ref, value: ["ok"] }));
    }),
  ],
});

export const sampleExplainPath: ExplainCleanupPath | undefined = undefined;

export async function runExternalConsumer(cwd: string): Promise<void> {
  const session = await openProject({
    cwd,
    tsconfig: "tsconfig.json",
    adapters: [sampleAdapter],
    additionalInputs: ["allowed-packages.json"],
    models: [extModel],
  });
  try {
    const ctx = session.project();
    const inputEntry: AdditionalInputEntry = ctx.registerAdditionalInput(
      "allowed-packages.json",
    );
    if (inputEntry.exists) {
      const raw: string = ctx.readAdditionalInput("allowed-packages.json");
      void raw;
    }
    const syntaxNodes = await ctx.findSyntax({
      range: { file: "src/app.ts", startLine: 1, endLine: 20 },
      kinds: ["CallExpression"],
    });
    const facts = await ctx.facts(sampleFact, {
      targets: syntaxNodes.map((n) => n.ref),
      providerId: "demo-adapter",
      condition: { strict: true },
    });
    void facts;
    const callResolutions = await ctx.resolveCalls(
      syntaxNodes.map((n) => n.ref),
      { select: { signature: true, location: true } },
    );
    const callRefs: CallRef[] = [];
    for (const row of callResolutions) {
      if (row.call !== null) {
        callRefs.push(row.call);
      }
    }
    const resolvedInstances = await ctx.resolveInstances(callRefs);
    const instRefs: InstanceRef[] = [];
    for (const instRow of resolvedInstances) {
      if (instRow.instance !== null) {
        instRefs.push(instRow.instance);
      }
    }
    const projectedInstances = await ctx.projectInstances(instRefs, {
      select: {
        instanceKey: true,
        originKind: true,
        allocationLocation: true,
        originSyntax: true,
        modelId: true,
        evidence: true,
      },
    });
    const firstInst = projectedInstances[0];
    if (firstInst !== undefined && firstInst.originSyntax !== null) {
      const originSyn: SyntaxRef = firstInst.originSyntax;
      void originSyn;
      void isSameInstance(firstInst.instance, instRefs[0]);
    }
    const callRows = await ctx.projectCalls(callRefs, {
      select: { signature: true, returnType: true },
    });
    const sigRefs: SignatureRef[] = [];
    for (const row of callRows) {
      if (row.signature !== null) {
        sigRefs.push(row.signature);
      }
    }
    const sigRows = await ctx.projectSignatures(sigRefs, {
      select: {
        instantiatedParameterTypes: [0],
        returnType: true,
        declaration: true,
      },
    });
    const firstSig = sigRows[0];
    if (firstSig !== undefined) {
      const param0 = firstSig.instantiatedParameterTypes[0];
      if (param0 !== undefined) {
        const sourceType: TypeRef = param0.type.ref;
        const targetType: TypeRef = firstSig.returnType.ref;
        const [assign] = await ctx.isAssignableTo([
          { source: sourceType, target: targetType },
        ]);
        if (assign?.assignable) {
          const decl: DeclarationRef | null = firstSig.declaration;
          if (decl !== null) {
            const [lookedUp] = await ctx.syntaxOf([decl], {
              select: { kind: true, location: true },
            });
            const syn: SyntaxRef | null = lookedUp?.syntax ?? null;
            if (syn !== null && lookedUp?.location != null) {
              const callers = await ctx.callersOf([decl]);
              const callees = await ctx.calleesOf(callRefs);
              const prop = await ctx.valuePropagation({
                propagation: "value",
                from: syn,
                to: [decl, ...callRefs],
              });
              void callers;
              void callees;
              void prop.results[0]?.reachable;
            }
          }
        }
      }
    }
    const cleanupResults: readonly InstanceCleanupScopeResult[] =
      await ctx.instanceCleanup({
        scopes: callRefs,
        targets: instRefs,
      });
    const effectsResults: readonly CallEffectsResult[] = await ctx.callEffects({
      targets: callRefs,
    });
    const firstEffect = effectsResults[0]?.fieldWrites[0];
    if (firstEffect !== undefined) {
      const writeSyn: SyntaxRef = firstEffect.syntax;
      const recvSyn: SyntaxRef = firstEffect.receiverSyntax;
      const [cov] = await ctx.branchCoverage({
        branches: [],
        targets: [writeSyn],
      });
      if (cov !== undefined) {
        if (cov.status === "disproved") {
          void cov.counterexamplePath.steps.length;
        } else if (cov.status === "inconclusive") {
          void cov.gaps.length;
        } else {
          void cov.targetReachable;
        }
      }
      await ctx.resolveInstances([recvSyn]);
    }
    const firstCleanup = cleanupResults[0];
    if (firstCleanup !== undefined) {
      const disposedInst: InstanceRef | undefined =
        firstCleanup.disposedInstances[0]?.instance;
      void disposedInst;
    }
  } finally {
    await session.close();
  }
}
