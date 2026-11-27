import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

import {
  defineSummaries,
  openProject,
  type BranchEdgeInfo,
  type CallRef,
  type DiagnosticLocation,
  type ProjectContext,
  type SyntaxNodeResult,
  type SyntaxRef,
} from "tssae";

import { collectPreciseCoverage } from "./coverage.ts";
import {
  checkSoundness,
  type BranchCoverageSoundnessCase,
  type CallEffectsSoundnessCase,
  type CalleesOfSoundnessCase,
  type InstanceCleanupSoundnessCase,
  type SoundnessCheckInput,
  type SoundnessCheckReport,
  type ValuePropagationSoundnessCase,
} from "./oracle.ts";

export type SoundnessReconciliationOptions = {
  readonly projectDir: string;
  readonly tsconfig?: string | undefined;
};

export type SoundnessReconciliationResult = {
  readonly snapshotCount: number;
  readonly input: SoundnessCheckInput;
  readonly report: SoundnessCheckReport;
  readonly queriesEvaluated: {
    readonly branchCoverage: number;
    readonly valuePropagation: number;
    readonly calleesOf: number;
    readonly callEffects: number;
    readonly instanceCleanup: number;
  };
};

type ProbeSnapshot = {
  readonly arrivedPairs: ReadonlyArray<{
    readonly sinkTag: string;
    readonly sourceTags: readonly string[];
  }>;
  readonly callsBySite: Readonly<Record<string, readonly string[]>>;
  readonly writesBySite: Readonly<Record<string, readonly string[]>>;
  readonly disposedResources: Readonly<Record<string, boolean>>;
};

type ProbeModule = {
  resetProbeLedger: () => void;
  getProbeSnapshot: () => ProbeSnapshot;
};

type ControlFlowModule = {
  runControlFlowScenarios: () => void;
};

type ValuesAndCallsModule = {
  runValuesAndCallsScenarios: () => Promise<void> | void;
};

type EffectsAndCleanupModule = {
  runEffectsAndCleanupScenarios: () => void;
};

function insideSpan(
  inner: DiagnosticLocation,
  outer: DiagnosticLocation,
): boolean {
  return (
    inner.file === outer.file &&
    inner.start >= outer.start &&
    inner.end <= outer.end
  );
}

function formatLocation(loc: DiagnosticLocation): string {
  return `${loc.file}:${loc.line}:${loc.column}`;
}

function findEnclosingFunction(
  nodes: readonly SyntaxNodeResult[],
  fnName: string,
): SyntaxNodeResult {
  const found = nodes.find(
    (node) =>
      node.kind === "FunctionDeclaration" &&
      node.text.includes(`function ${fnName}(`),
  );
  if (!found) {
    throw new Error(`Missing FunctionDeclaration "${fnName}" in AST.`);
  }
  return found;
}

function findCallInside(
  nodes: readonly SyntaxNodeResult[],
  enclosing: DiagnosticLocation,
  prefix: string,
  kind: SyntaxNodeResult["kind"] = "CallExpression",
  enclosingExprPrefix?: string,
): SyntaxNodeResult {
  const scopeLoc =
    enclosingExprPrefix === undefined
      ? enclosing
      : (nodes.find(
          (node) =>
            insideSpan(node.location, enclosing) &&
            node.text.startsWith(enclosingExprPrefix),
        )?.location ?? enclosing);
  const found = nodes.find(
    (node) =>
      node.kind === kind &&
      insideSpan(node.location, scopeLoc) &&
      node.text.startsWith(prefix),
  );
  if (!found) {
    throw new Error(
      `Missing ${kind} starting with "${prefix}" inside ${formatLocation(scopeLoc)}.`,
    );
  }
  return found;
}

function findNewInside(
  nodes: readonly SyntaxNodeResult[],
  enclosing: DiagnosticLocation,
  prefix: string,
): SyntaxNodeResult {
  const found = nodes.find(
    (node) =>
      node.kind === "NewExpression" &&
      insideSpan(node.location, enclosing) &&
      node.text.startsWith(prefix),
  );
  if (!found) {
    throw new Error(
      `Missing NewExpression starting with "${prefix}" inside ${formatLocation(enclosing)}.`,
    );
  }
  return found;
}

function selectBranchEdges(
  branches: readonly BranchEdgeInfo[],
  kind: BranchEdgeInfo["kind"],
  predicate: (branch: BranchEdgeInfo) => boolean,
): readonly BranchEdgeInfo[] {
  const matched = branches.filter((b) => b.kind === kind && predicate(b));
  if (matched.length === 0) {
    throw new Error(`Expected at least one "${kind}" branch edge.`);
  }
  return matched;
}

async function resolveCallHandles(
  ctx: ProjectContext,
  calls: readonly SyntaxNodeResult[],
): Promise<readonly CallRef[]> {
  const resolved = await ctx.resolveCalls(calls.map((c) => c.ref));
  return resolved.map((row, idx) => {
    if (!row.call) {
      throw new Error(
        `resolveCalls returned null for ${formatLocation(calls[idx]!.location)}`,
      );
    }
    return row.call;
  });
}

function buildProbeCleanupSummaries() {
  return defineSummaries({
    id: "soundness-probe-cleanup-summaries",
    entries: [
      {
        match: {
          module: "probe.ts",
          memberPath: ["TrackedHandle", "prototype", "dispose"],
        },
        effects: {
          completeness: "complete",
          resourceOps: [{ kind: "disposes-target", target: "receiver" }],
        },
      },
      {
        match: {
          module: "probe.ts",
          memberPath: ["TrackedHandle", "prototype", "[Symbol.dispose]"],
        },
        effects: {
          completeness: "complete",
          resourceOps: [{ kind: "disposes-target", target: "receiver" }],
        },
      },
    ],
  });
}

/**
 * Executes controlled TypeScript fixtures natively in Node under V8 precise
 * byte coverage and plain runtime probes (zero AST rewriting), queries the
 * exact same unmodified files in a single `openProject` snapshot, and verifies
 * all five unilateral soundness invariants via `checkSoundness`.
 */
export async function runSoundnessReconciliationSuite(
  options: SoundnessReconciliationOptions,
): Promise<SoundnessReconciliationResult> {
  const projectDir = resolve(options.projectDir);
  const tsconfig = options.tsconfig ?? "tsconfig.json";

  const probePath = join(projectDir, "probe.ts");
  const controlFlowPath = join(projectDir, "control-flow.ts");
  const valuesAndCallsPath = join(projectDir, "values-and-calls.ts");
  const effectsAndCleanupPath = join(projectDir, "effects-and-cleanup.ts");

  const probeMod = (await import(pathToFileURL(probePath).href)) as ProbeModule;
  probeMod.resetProbeLedger();

  const coverage = await collectPreciseCoverage(
    [controlFlowPath, valuesAndCallsPath, effectsAndCleanupPath],
    async () => {
      const cfMod = (await import(
        pathToFileURL(controlFlowPath).href
      )) as ControlFlowModule;
      const vcMod = (await import(
        pathToFileURL(valuesAndCallsPath).href
      )) as ValuesAndCallsModule;
      const ecMod = (await import(
        pathToFileURL(effectsAndCleanupPath).href
      )) as EffectsAndCleanupModule;

      cfMod.runControlFlowScenarios();
      await vcMod.runValuesAndCallsScenarios();
      ecMod.runEffectsAndCleanupScenarios();
    },
  );

  const probeSnapshot = probeMod.getProbeSnapshot();

  const session = await openProject({
    cwd: projectDir,
    tsconfig,
    summaries: { packs: [buildProbeCleanupSummaries()] },
  });

  try {
    const ctx = session.project();

    const [cfNodes, vcNodes, ecNodes] = await Promise.all([
      ctx.findSyntax({ range: { file: "control-flow.ts" } }),
      ctx.findSyntax({ range: { file: "values-and-calls.ts" } }),
      ctx.findSyntax({ range: { file: "effects-and-cleanup.ts" } }),
    ]);

    // 1. branchCoverage cases (control-flow.ts)
    const branchSpecs: ReadonlyArray<{
      readonly id: string;
      readonly fnName: string;
      readonly guardKind: BranchEdgeInfo["kind"];
      readonly guardPredicate: (b: BranchEdgeInfo) => boolean;
      readonly guardSpanNode: (fnLoc: DiagnosticLocation) => {
        readonly loc: DiagnosticLocation;
        readonly shortCircuitSubrangeZeroAsSkipped?: boolean;
      };
      readonly targetLabel: string;
      readonly reachedViaCaughtException: boolean;
    }> = [
      {
        id: "branch-normal-if-bypass",
        fnName: "normalIfBypass",
        guardKind: "if",
        guardPredicate: (b) => b.direction === "true",
        guardSpanNode: (fnLoc) => ({
          loc: findCallInside(cfNodes, fnLoc, "guardStep(").location,
        }),
        targetLabel: "normal-if-bypass",
        reachedViaCaughtException: false,
      },
      {
        id: "branch-short-circuit-and",
        fnName: "shortCircuitAndBypass",
        guardKind: "logicalAnd",
        guardPredicate: (b) => b.direction !== "shortCircuit",
        guardSpanNode: (fnLoc) => ({
          loc: findCallInside(cfNodes, fnLoc, "guardStep(").location,
        }),
        targetLabel: "short-circuit-and",
        reachedViaCaughtException: false,
      },
      {
        id: "branch-optional-chain",
        fnName: "optionalChainBypass",
        guardKind: "optionalChain",
        guardPredicate: (b) => b.direction !== "shortCircuit",
        guardSpanNode: (fnLoc) => ({
          loc: findCallInside(cfNodes, fnLoc, "helper?.check(").location,
          shortCircuitSubrangeZeroAsSkipped: true,
        }),
        targetLabel: "optional-chain",
        reachedViaCaughtException: false,
      },
      {
        id: "branch-caught-exception-bypass",
        fnName: "caughtExceptionBypass",
        guardKind: "if",
        guardPredicate: (b) => b.direction === "true",
        guardSpanNode: (fnLoc) => ({
          loc: findCallInside(cfNodes, fnLoc, "guardStep(").location,
        }),
        targetLabel: "caught-exception",
        reachedViaCaughtException: true,
      },
    ];

    const branchFns = branchSpecs.map((spec) =>
      findEnclosingFunction(cfNodes, spec.fnName),
    );
    const branchGroups = await ctx.listBranches(branchFns.map((fn) => fn.ref));
    const branchCases: BranchCoverageSoundnessCase[] = [];

    for (const [idx, spec] of branchSpecs.entries()) {
      const fnNode = branchFns[idx]!;
      const edges = selectBranchEdges(
        branchGroups[idx]!.branches,
        spec.guardKind,
        spec.guardPredicate,
      );
      const targetCall = findCallInside(
        cfNodes,
        fnNode.location,
        `targetWork("${spec.targetLabel}")`,
      );
      const [covItem] = await ctx.branchCoverage({
        function: fnNode.ref,
        branches: edges.map((e) => e.ref),
        targets: [targetCall.ref],
      });
      if (!covItem) {
        throw new Error(`Missing branchCoverage item for ${spec.id}`);
      }

      const guardSpan = spec.guardSpanNode(fnNode.location);
      const guardCount = coverage.getExecutionCount(
        controlFlowPath,
        guardSpan.loc.start,
        guardSpan.loc.end,
        guardSpan.shortCircuitSubrangeZeroAsSkipped
          ? { shortCircuitSubrangeZeroAsSkipped: true }
          : undefined,
      );
      const targetCount = coverage.getExecutionCount(
        controlFlowPath,
        targetCall.location.start,
        targetCall.location.end,
      );

      branchCases.push({
        id: spec.id,
        location: formatLocation(targetCall.location),
        targetExecutedCount: targetCount,
        selectedBranchExecutedCounts: [guardCount],
        reachedViaCaughtException: spec.reachedViaCaughtException,
        staticStatus: covItem.status,
        implicitExceptionsStatus: covItem.implicitExceptions.status,
      });
    }

    // 2. valuePropagation cases (values-and-calls.ts)
    const valueTags = [
      "val-destructure-default",
      "val-nullish-optional",
      "val-accessor",
      "val-closure-reassign",
      "val-iterator",
      "val-generator-next-value",
      "val-async-generator-array-elem",
      "val-async-gen-yield-promise",
      "val-async-gen-yield-promise-next",
      "val-async-delegate-sync-gen-array",
      "val-await-sync-gen-next-value",
    ] as const;

    const valueEndpoints = valueTags.map((tag) => {
      const sourceCall = vcNodes.find(
        (node) =>
          node.kind === "CallExpression" &&
          node.text.startsWith(`source("${tag}"`),
      );
      const sinkCall = vcNodes.find(
        (node) =>
          node.kind === "CallExpression" &&
          node.text.startsWith(`sink("${tag}"`),
      );
      const sinkArg = sinkCall?.arguments?.[1];
      if (!sourceCall || !sinkCall || !sinkArg) {
        throw new Error(`Missing source/sink pair for "${tag}".`);
      }
      return { tag, from: sourceCall.ref, to: sinkArg, loc: sinkCall.location };
    });

    const propResult = await ctx.valuePropagation({
      propagation: "value",
      from: valueEndpoints.map((e) => e.from),
      to: valueEndpoints.map((e) => e.to),
    });

    const valueCases: ValuePropagationSoundnessCase[] = valueEndpoints.map(
      (endpoint) => {
        const match = propResult.results.find(
          (r) =>
            r.from.handleId === endpoint.from.handleId &&
            r.to.handleId === endpoint.to.handleId,
        );
        if (!match) {
          throw new Error(
            `Missing valuePropagation result for "${endpoint.tag}".`,
          );
        }
        const runtimeArrived = probeSnapshot.arrivedPairs.some(
          (pair) =>
            pair.sinkTag === endpoint.tag &&
            pair.sourceTags.includes(endpoint.tag),
        );
        return {
          id: endpoint.tag,
          location: formatLocation(endpoint.loc),
          runtimeArrived,
          staticStatus: match.status,
        };
      },
    );

    // 3. calleesOf cases (values-and-calls.ts)
    const calleeDeclarationKinds = new Set([
      "FunctionDeclaration",
      "MethodDeclaration",
      "GetAccessor",
      "SetAccessor",
    ]);
    const recordCalleeCalls = vcNodes.filter(
      (n) => n.kind === "CallExpression" && n.text.startsWith('recordCallee("'),
    );
    const declSpanByTargetId = new Map<string, DiagnosticLocation>();
    for (const call of recordCalleeCalls) {
      const match = /^recordCallee\("([^"]+)"\)/u.exec(call.text);
      if (!match) continue;
      const targetId = match[1]!;
      const candidates = vcNodes.filter(
        (n) =>
          calleeDeclarationKinds.has(n.kind) &&
          insideSpan(call.location, n.location),
      );
      // Pick the innermost enclosing declaration.
      candidates.sort(
        (a, b) =>
          a.location.end -
          a.location.start -
          (b.location.end - b.location.start),
      );
      if (candidates[0]) {
        declSpanByTargetId.set(targetId, candidates[0].location);
      }
    }

    const calleeSiteSpecs: ReadonlyArray<{
      readonly siteTag: string;
      readonly fnName: string;
      readonly callPrefix: string;
      readonly kind?: SyntaxNodeResult["kind"];
      readonly enclosingExprPrefix?: string;
    }> = [
      {
        siteTag: "callee-accessor-get",
        fnName: "runAccessorValue",
        callPrefix: "box.current",
        kind: "PropertyAccessExpression",
        enclosingExprPrefix: "readBack = box.current",
      },
      {
        siteTag: "callee-polymorphic",
        fnName: "invokeWorker",
        callPrefix: "worker.execute(",
      },
      {
        siteTag: "callee-closure-reassign",
        fnName: "invokeReassignedClosure",
        callPrefix: "handler(",
      },
      {
        siteTag: "callee-optional-present",
        fnName: "invokeOptionalTarget",
        callPrefix: "helper?.ping(",
      },
    ];

    const calleeCallNodes = calleeSiteSpecs.map((spec) => {
      const fn = findEnclosingFunction(vcNodes, spec.fnName);
      return findCallInside(
        vcNodes,
        fn.location,
        spec.callPrefix,
        spec.kind,
        spec.enclosingExprPrefix,
      );
    });
    const calleeCallRefs = await resolveCallHandles(ctx, calleeCallNodes);
    const calleesResults = await ctx.calleesOf(calleeCallRefs);

    const calleesCases: CalleesOfSoundnessCase[] = calleeSiteSpecs.map(
      (spec, idx) => {
        const row = calleesResults[idx]!;
        const callNode = calleeCallNodes[idx]!;
        const staticTargets: string[] = [];

        for (const target of row.resolvedTargets) {
          const candidateLocs = [
            target.location,
            target.implementation?.location ?? null,
          ].filter((loc): loc is DiagnosticLocation => loc !== null);

          for (const [targetId, declSpan] of declSpanByTargetId) {
            if (candidateLocs.some((loc) => insideSpan(loc, declSpan))) {
              staticTargets.push(targetId);
            }
          }
        }

        return {
          id: spec.siteTag,
          location: formatLocation(callNode.location),
          runtimeCalledTargets: probeSnapshot.callsBySite[spec.siteTag] ?? [],
          staticCompleteness: row.completeness,
          staticTargets,
        };
      },
    );

    // 4. callEffects cases (effects-and-cleanup.ts)
    const effectSiteSpecs = [
      {
        siteTag: "effect-direct-write",
        fnName: "runDirectFieldWrite",
        callPrefix: "markQueued(",
      },
      {
        siteTag: "effect-polymorphic-write",
        fnName: "invokeWriter",
        callPrefix: "writer.apply(",
      },
      {
        siteTag: "effect-setter-write",
        fnName: "runSetterWrite",
        callPrefix: "assignLevel(",
      },
    ] as const;

    const effectCallNodes = effectSiteSpecs.map((spec) => {
      const fn = findEnclosingFunction(ecNodes, spec.fnName);
      return findCallInside(ecNodes, fn.location, spec.callPrefix);
    });
    const effectCallRefs = await resolveCallHandles(ctx, effectCallNodes);
    const effectResults = await ctx.callEffects({
      targets: effectCallRefs,
      includeWrites: true,
      includeReads: false,
    });

    const callEffectsCases: CallEffectsSoundnessCase[] = effectSiteSpecs.map(
      (spec, idx) => {
        const row = effectResults[idx]!;
        const callNode = effectCallNodes[idx]!;
        return {
          id: spec.siteTag,
          location: formatLocation(callNode.location),
          runtimeWrittenProperties:
            probeSnapshot.writesBySite[spec.siteTag] ?? [],
          staticCompleteness: row.completeness,
          staticFieldWrites: row.fieldWrites.map((w) => w.propertyName),
        };
      },
    );

    // 5. instanceCleanup cases (effects-and-cleanup.ts)
    const cleanupSpecs = [
      {
        tag: "cleanup-using-throw",
        fnName: "cleanupWithUsing",
        reachedViaCaughtException: true,
      },
      {
        tag: "cleanup-try-finally-throw",
        fnName: "cleanupWithTryFinally",
        reachedViaCaughtException: true,
      },
      {
        tag: "cleanup-unprotected-throw",
        fnName: "cleanupUnprotectedThrow",
        reachedViaCaughtException: true,
      },
      {
        tag: "cleanup-uncleaned-branch",
        fnName: "cleanupUncleanedBranch",
        reachedViaCaughtException: false,
      },
    ] as const;

    const cleanupFnNodes = cleanupSpecs.map((spec) =>
      findEnclosingFunction(ecNodes, spec.fnName),
    );
    const allocNodes = cleanupFnNodes.map((fn) =>
      findNewInside(ecNodes, fn.location, "new TrackedHandle("),
    );
    const resolvedInstances = await ctx.resolveInstances(
      allocNodes.map((n): SyntaxRef => n.ref),
    );

    const instanceCleanupCases: InstanceCleanupSoundnessCase[] = [];
    for (const [idx, spec] of cleanupSpecs.entries()) {
      const fnNode = cleanupFnNodes[idx]!;
      const instRes = resolvedInstances[idx]!;
      if (instRes.status !== "proved" || !instRes.instance) {
        throw new Error(
          `Expected resolveInstances to resolve TrackedHandle for ${spec.tag}, got ${instRes.status}.`,
        );
      }

      const [cleanupScope] = await ctx.instanceCleanup({
        scopes: [fnNode.ref],
        targets: [instRes.instance],
      });
      const targetRes = cleanupScope?.targetResults[0];
      if (!targetRes) {
        throw new Error(
          `Missing instanceCleanup targetResult for ${spec.tag}.`,
        );
      }

      const runtimeCleanedUp =
        probeSnapshot.disposedResources[spec.tag] === true;

      instanceCleanupCases.push({
        id: spec.tag,
        location: formatLocation(fnNode.location),
        runtimeCleanedUp,
        reachedViaCaughtException: spec.reachedViaCaughtException,
        staticStatus: targetRes.status,
        implicitExceptionsStatus: targetRes.implicitExceptions.status,
      });
    }

    const input: SoundnessCheckInput = {
      branchCoverage: branchCases,
      valuePropagation: valueCases,
      calleesOf: calleesCases,
      callEffects: callEffectsCases,
      instanceCleanup: instanceCleanupCases,
    };
    const report = checkSoundness(input);

    return {
      snapshotCount: 1,
      input,
      report,
      queriesEvaluated: {
        branchCoverage: branchCases.length,
        valuePropagation: valueCases.length,
        calleesOf: calleesCases.length,
        callEffects: callEffectsCases.length,
        instanceCleanup: instanceCleanupCases.length,
      },
    };
  } finally {
    await session.close();
  }
}
