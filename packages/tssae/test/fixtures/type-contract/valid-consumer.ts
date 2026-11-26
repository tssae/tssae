import {
  createAdditionalInputRegistry,
  defineSummaries,
  isSameInstance,
  extractBasisEvidence,
  openProject,
  registerProvenance,
  type AdditionalInputEntry,
  type AdditionalInputRegistrar,
  type AdditionalInputSource,
  type CallEffectsResult,
  type CallRef,
  type DeclarationRef,
  type ExplainCleanupPath,
  type InstanceCleanupScopeResult,
  type InstanceRef,
  type ProvenanceCarrier,
  type SignatureRef,
  type SyntaxRef,
  type TypeRef,
} from "tssae";

const extSummaries = defineSummaries({
  id: "ext-summaries",
  entries: [
    {
      match: { memberPath: ["IMenuService", "prototype", "createMenu"] },
      instances: {
        completeness: "complete",
        outcome: { kind: "allocates-fresh-instance" },
      },
    },
    {
      match: { memberPath: ["Disposable", "prototype", "_register"] },
      instances: {
        completeness: "complete",
        outcome: { kind: "forwards-instance", from: { argIndex: 0 } },
      },
    },
    {
      match: { memberPath: ["dispose"] },
      effects: {
        completeness: "complete",
        resourceOps: [{ kind: "disposes-target", target: "receiver" }],
      },
    },
    {
      match: { memberPath: ["MutableDisposable", "prototype", "value"] },
      effects: {
        completeness: "complete",
        resourceOps: [{ kind: "replaces-previous-value", target: "receiver" }],
      },
    },
    {
      match: { memberPath: ["onDidDisconnect"] },
      calls: {
        completeness: "complete",
        invokesCallbacks: [
          {
            callbackArgIndex: 0,
            timing: "lifecycle-end",
          },
        ],
      },
    },
  ],
});

const registrar: AdditionalInputRegistrar = createAdditionalInputRegistry({
  cwd: "/tmp/workspace",
});
const ruleSource: AdditionalInputSource = {
  kind: "rule",
  id: "demo-rule",
};
const regEntry: AdditionalInputEntry = registrar.registerAdditionalInput(
  "allowed-packages.json",
  ruleSource,
);
void regEntry;

const domainCarrier: ProvenanceCarrier<{ readonly name: string }> =
  registerProvenance({ name: "demo" }, []);
void extractBasisEvidence([domainCarrier]);

export const sampleExplainPath: ExplainCleanupPath | undefined = undefined;
export function inspectExplainSummaryIds(
  path: ExplainCleanupPath | undefined,
): readonly string[] | undefined {
  return path?.summaryIds;
}

export async function runExternalConsumer(cwd: string): Promise<void> {
  const session = await openProject({
    cwd,
    tsconfig: "tsconfig.json",
    additionalInputs: ["allowed-packages.json"],
    summaries: { packs: [extSummaries] },
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
    ctx.recordGaps([]);
    const declRows = await ctx.resolveDeclarations(
      syntaxNodes.map((n) => n.ref),
      { select: { packageName: true, exportName: true } },
    );
    void declRows[0]?.packageName;
    void declRows[0]?.exportName;
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
      if (instRow.status === "proved" && instRow.instance !== null) {
        instRefs.push(instRow.instance);
      } else if (instRow.status === "inconclusive") {
        void instRow.gaps.length;
      } else {
        void instRow.evidence.length;
      }
    }
    const projectedInstances = await ctx.projectInstances(instRefs, {
      select: {
        instanceKey: true,
        originKind: true,
        allocationLocation: true,
        originSyntax: true,
        summaryId: true,
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
              const firstCaller = callers[0];
              if (firstCaller !== undefined) {
                void extractBasisEvidence([firstCaller]);
                if (firstCaller.completeness === "incomplete") {
                  void firstCaller.gaps.length;
                }
              }
              const firstCallee = callees[0];
              if (
                firstCallee !== undefined &&
                firstCallee.completeness === "incomplete"
              ) {
                void firstCallee.gaps.length;
              }
              const firstProp = prop.results[0];
              if (firstProp !== undefined) {
                if (firstProp.status === "proved") {
                  void firstProp.paths[0].steps.length;
                } else if (firstProp.status === "inconclusive") {
                  void firstProp.gaps.length;
                } else {
                  void firstProp.evidence.length;
                }
              }
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
    const firstEffResult = effectsResults[0];
    if (
      firstEffResult !== undefined &&
      firstEffResult.completeness === "incomplete"
    ) {
      void firstEffResult.gaps.length;
    }
    const firstEffect = firstEffResult?.fieldWrites[0];
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
        if (cov.implicitExceptions.status === "disproved") {
          void cov.implicitExceptions.mayThrowSites.length;
          void cov.implicitExceptions.counterexamplePath.steps.length;
        } else if (cov.implicitExceptions.status === "inconclusive") {
          void cov.implicitExceptions.mayThrowSites.length;
          void cov.implicitExceptions.gaps.length;
        } else {
          void cov.implicitExceptions.evidence.length;
        }
      }
      await ctx.resolveInstances([recvSyn]);
    }
    const firstCleanup = cleanupResults[0];
    if (firstCleanup !== undefined) {
      const disposedInst: InstanceRef | undefined =
        firstCleanup.disposedInstances[0]?.instance;
      void disposedInst;
      const firstTarget = firstCleanup.targetResults[0];
      if (firstTarget !== undefined) {
        if (firstTarget.status === "proved") {
          void firstTarget.cleanupSites[0].location;
        } else if (firstTarget.status === "disproved") {
          void firstTarget.cleanedOnAnyPath;
          void firstTarget.uncleanedPath.length;
        } else {
          void firstTarget.gaps.length;
        }
        if (firstTarget.implicitExceptions.status === "disproved") {
          void firstTarget.implicitExceptions.mayThrowSites.length;
          void firstTarget.implicitExceptions.counterexamplePath.length;
        } else if (firstTarget.implicitExceptions.status === "inconclusive") {
          void firstTarget.implicitExceptions.mayThrowSites.length;
          void firstTarget.implicitExceptions.gaps.length;
        } else {
          void firstTarget.implicitExceptions.cleanupSites.length;
        }
      }
    }
  } finally {
    await session.close();
  }
}
