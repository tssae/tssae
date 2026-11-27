import type {
  CallRef,
  DeclarationRef,
  DiagnosticLocation,
  InstanceRef,
  ProjectContext,
  ProjectSession,
  SyntaxNodeResult,
  SyntaxRef,
} from "tssae";

import type {
  CollectQueryDigestOptions,
  DigestSample,
  NormalizedLocation,
  QueryChannelCounts,
  QueryDigest,
  SemanticOutcomeChannel,
  SemanticOutcomeStatus,
  SemanticQueryName,
} from "./types.ts";

const FUNCTION_SYNTAX_KINDS = [
  "FunctionDeclaration",
  "MethodDeclaration",
  "FunctionExpression",
  "ArrowFunction",
  "Constructor",
] as const;

const CALL_SYNTAX_KINDS = ["CallExpression", "NewExpression"] as const;

const FILE_BATCH_SIZE = 50;
const QUERY_BATCH_SIZE = 150;

function compareStrings(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function compareLocations(
  a: DiagnosticLocation,
  b: DiagnosticLocation,
): number {
  const fileCmp = compareStrings(a.file, b.file);
  if (fileCmp !== 0) {
    return fileCmp;
  }
  if (a.start !== b.start) {
    return a.start - b.start;
  }
  return a.end - b.end;
}

function compareSyntaxNodes(a: SyntaxNodeResult, b: SyntaxNodeResult): number {
  const locCmp = compareLocations(a.location, b.location);
  if (locCmp !== 0) {
    return locCmp;
  }
  return compareStrings(a.kind, b.kind);
}

function isDeclarationFile(filePath: string): boolean {
  return (
    filePath.endsWith(".d.ts") ||
    filePath.endsWith(".d.mts") ||
    filePath.endsWith(".d.cts")
  );
}

function normalizeLocation(loc: DiagnosticLocation): NormalizedLocation {
  return {
    file: loc.file,
    line: loc.line,
    column: loc.column,
    endLine: loc.endLine,
    endColumn: loc.endColumn,
    start: loc.start,
    end: loc.end,
  };
}

function formatPos(loc: DiagnosticLocation): string {
  return `${String(loc.line)}:${String(loc.column)}-${String(loc.endLine)}:${String(loc.endColumn)}`;
}

function chunkArray<T>(
  items: readonly T[],
  chunkSize: number,
): readonly (readonly T[])[] {
  const chunks: T[][] = [];
  for (let i = 0; i < items.length; i += chunkSize) {
    chunks.push(items.slice(i, i + chunkSize));
  }
  return chunks;
}

function groupByFile(
  nodes: readonly SyntaxNodeResult[],
): Map<string, SyntaxNodeResult[]> {
  const grouped = new Map<string, SyntaxNodeResult[]>();
  for (const node of nodes) {
    const existing = grouped.get(node.location.file);
    if (existing !== undefined) {
      existing.push(node);
    } else {
      grouped.set(node.location.file, [node]);
    }
  }
  return grouped;
}

function limitPerFile(
  nodes: readonly SyntaxNodeResult[],
  maxPerFile: number | undefined,
): readonly SyntaxNodeResult[] {
  if (maxPerFile === undefined) {
    return nodes;
  }
  const counts = new Map<string, number>();
  const kept: SyntaxNodeResult[] = [];
  for (const node of nodes) {
    const count = counts.get(node.location.file) ?? 0;
    if (count < maxPerFile) {
      counts.set(node.location.file, count + 1);
      kept.push(node);
    }
  }
  return kept;
}

function buildCounts(
  samples: readonly DigestSample[],
): readonly QueryChannelCounts[] {
  const buckets = new Map<
    string,
    {
      readonly query: SemanticQueryName;
      readonly channel: SemanticOutcomeChannel;
      total: number;
      readonly statusCounts: Record<string, number>;
    }
  >();

  for (const sample of samples) {
    const key = `${sample.query}\0${sample.channel}`;
    let bucket = buckets.get(key);
    if (bucket === undefined) {
      bucket = {
        query: sample.query,
        channel: sample.channel,
        total: 0,
        statusCounts: {},
      };
      buckets.set(key, bucket);
    }
    bucket.total += 1;
    bucket.statusCounts[sample.status] =
      (bucket.statusCounts[sample.status] ?? 0) + 1;
  }

  return [...buckets.values()]
    .sort((a, b) => {
      const q = compareStrings(a.query, b.query);
      return q !== 0 ? q : compareStrings(a.channel, b.channel);
    })
    .map((bucket) => ({
      query: bucket.query,
      channel: bucket.channel,
      total: bucket.total,
      statusCounts: Object.fromEntries(
        Object.entries(bucket.statusCounts).sort(([a], [b]) =>
          compareStrings(a, b),
        ),
      ),
    }));
}

function isProjectSession(
  input: ProjectContext | ProjectSession,
): input is ProjectSession {
  return "projectContexts" in input;
}

async function collectFromProjectContext(
  ctx: ProjectContext,
  options: CollectQueryDigestOptions,
  sampleMap: Map<string, DigestSample>,
): Promise<number> {
  const maxFunctionsPerFile = options.maxFunctionsPerFile ?? 5;
  const maxCallsPerFile = options.maxCallsPerFile ?? 6;
  const maxBranchesPerFunction = options.maxBranchesPerFunction ?? 3;
  const maxBranchTargetsPerFunction = options.maxBranchTargetsPerFunction ?? 3;
  const maxValueSourcesPerFile = options.maxValueSourcesPerFile ?? 4;
  const maxValueTargetsPerFile = options.maxValueTargetsPerFile ?? 4;
  const maxInstanceTargetsPerFile = options.maxInstanceTargetsPerFile ?? 5;
  const maxCleanupScopesPerFile = options.maxCleanupScopesPerFile ?? 4;
  const maxCleanupTargetsPerFile = options.maxCleanupTargetsPerFile ?? 4;

  const allSourceFiles = ctx.project.sourceFiles
    .filter((file) => !isDeclarationFile(file))
    .sort(compareStrings);
  const files =
    options.maxFiles !== undefined
      ? allSourceFiles.slice(0, options.maxFiles)
      : allSourceFiles;

  if (files.length === 0) {
    return 0;
  }

  const addSample = (
    query: SemanticQueryName,
    channel: SemanticOutcomeChannel,
    location: DiagnosticLocation,
    targetId: string,
    status: SemanticOutcomeStatus,
  ): void => {
    const normalized = normalizeLocation(location);
    const dedupKey = `${query}\0${channel}\0${normalized.file}\0${targetId}`;
    if (!sampleMap.has(dedupKey)) {
      sampleMap.set(dedupKey, {
        query,
        channel,
        file: normalized.file,
        location: normalized,
        targetId,
        status,
      });
    }
  };

  const fnNodesRaw: SyntaxNodeResult[] = [];
  const callNodesRaw: SyntaxNodeResult[] = [];
  const varNodesRaw: SyntaxNodeResult[] = [];
  const returnNodesRaw: SyntaxNodeResult[] = [];

  for (const fileChunk of chunkArray(files, FILE_BATCH_SIZE)) {
    const [fns, calls, vars, returns] = await Promise.all([
      ctx.findSyntax({ files: fileChunk, kinds: [...FUNCTION_SYNTAX_KINDS] }),
      ctx.findSyntax({ files: fileChunk, kinds: [...CALL_SYNTAX_KINDS] }),
      ctx.findSyntax({ files: fileChunk, kinds: ["VariableDeclaration"] }),
      ctx.findSyntax({ files: fileChunk, kinds: ["ReturnStatement"] }),
    ]);
    for (const node of fns) {
      if (node.body !== undefined) {
        fnNodesRaw.push(node);
      }
    }
    callNodesRaw.push(...calls);
    for (const node of vars) {
      if (node.initializer !== undefined) {
        varNodesRaw.push(node);
      }
    }
    returnNodesRaw.push(...returns);
  }

  fnNodesRaw.sort(compareSyntaxNodes);
  callNodesRaw.sort(compareSyntaxNodes);
  varNodesRaw.sort(compareSyntaxNodes);
  returnNodesRaw.sort(compareSyntaxNodes);

  const fnNodes = limitPerFile(fnNodesRaw, maxFunctionsPerFile);
  const callNodes = limitPerFile(callNodesRaw, maxCallsPerFile);

  // 1. calleesOf, resolveImplementations, callEffects
  const resolvedCallEntries: {
    readonly ref: CallRef;
    readonly location: DiagnosticLocation;
  }[] = [];

  for (const callChunk of chunkArray(callNodes, QUERY_BATCH_SIZE)) {
    const resolvedRows = await ctx.resolveCalls(
      callChunk.map((node) => node.ref),
      { select: { location: true } },
    );
    for (const [index, row] of resolvedRows.entries()) {
      const sourceNode = callChunk[index];
      if (row.call !== null && sourceNode !== undefined) {
        resolvedCallEntries.push({
          ref: row.call,
          location: row.location ?? sourceNode.location,
        });
      }
    }
  }

  for (const callChunk of chunkArray(resolvedCallEntries, QUERY_BATCH_SIZE)) {
    const refs = callChunk.map((entry) => entry.ref);
    const [calleesResults, implResults, effectResults] = await Promise.all([
      ctx.calleesOf(refs),
      ctx.resolveImplementations(refs),
      ctx.callEffects({
        targets: refs,
        followCallbacks: true,
        includeReads: true,
        includeWrites: true,
      }),
    ]);

    for (const [index, row] of calleesResults.entries()) {
      const entry = callChunk[index];
      if (entry !== undefined) {
        addSample(
          "calleesOf",
          "primary",
          entry.location,
          `calleesOf@${formatPos(entry.location)}`,
          row.completeness,
        );
      }
    }

    for (const [index, row] of implResults.entries()) {
      const entry = callChunk[index];
      if (entry !== undefined) {
        addSample(
          "resolveImplementations",
          "primary",
          entry.location,
          `resolveImplementations@${formatPos(entry.location)}`,
          row.status,
        );
      }
    }

    for (const [index, row] of effectResults.entries()) {
      const entry = callChunk[index];
      if (entry !== undefined) {
        addSample(
          "callEffects",
          "primary",
          entry.location,
          `callEffects@${formatPos(entry.location)}`,
          row.completeness,
        );
      }
    }
  }

  // 2. callersOf
  const uniqueDecls: {
    readonly ref: DeclarationRef;
    readonly location: DiagnosticLocation;
  }[] = [];
  const seenDeclKeys = new Set<string>();

  for (const fnChunk of chunkArray(fnNodes, QUERY_BATCH_SIZE)) {
    const declRows = await ctx.resolveDeclarations(
      fnChunk.map((node) => node.ref),
      { select: { location: true } },
    );
    for (const [index, row] of declRows.entries()) {
      const fnNode = fnChunk[index];
      if (row.declaration === null || fnNode === undefined) {
        continue;
      }
      const declLoc = row.location ?? fnNode.location;
      const declKey = `${declLoc.file}:${String(declLoc.start)}..${String(declLoc.end)}`;
      if (!seenDeclKeys.has(declKey)) {
        seenDeclKeys.add(declKey);
        uniqueDecls.push({ ref: row.declaration, location: declLoc });
      }
    }
  }

  for (const declChunk of chunkArray(uniqueDecls, QUERY_BATCH_SIZE)) {
    const callersResults = await ctx.callersOf(
      declChunk.map((entry) => entry.ref),
    );
    for (const [index, row] of callersResults.entries()) {
      const entry = declChunk[index];
      if (entry !== undefined) {
        addSample(
          "callersOf",
          "primary",
          entry.location,
          `callersOf@${formatPos(entry.location)}`,
          row.completeness,
        );
      }
    }
  }

  // 3. branchCoverage (primary + implicitExceptions)
  const callsByFile = groupByFile(callNodesRaw);
  const returnsByFile = groupByFile(returnNodesRaw);

  for (const fnChunk of chunkArray(fnNodes, QUERY_BATCH_SIZE)) {
    const branchLists = await ctx.listBranches(fnChunk.map((fn) => fn.ref));
    for (const [index, fnBranchRow] of branchLists.entries()) {
      const fnNode = fnChunk[index];
      if (fnNode === undefined || fnBranchRow.branches.length === 0) {
        continue;
      }

      const fileReturns = returnsByFile.get(fnNode.location.file) ?? [];
      const fileCalls = callsByFile.get(fnNode.location.file) ?? [];
      const candidateTargets = [...fileReturns, ...fileCalls]
        .filter(
          (node) =>
            node.location.start >= fnNode.location.start &&
            node.location.end <= fnNode.location.end,
        )
        .sort(compareSyntaxNodes);

      const seenTargetSpan = new Set<string>();
      const selectedTargets: SyntaxNodeResult[] = [];
      for (const candidate of candidateTargets) {
        const spanKey = `${String(candidate.location.start)}..${String(candidate.location.end)}`;
        if (!seenTargetSpan.has(spanKey)) {
          seenTargetSpan.add(spanKey);
          selectedTargets.push(candidate);
          if (selectedTargets.length >= maxBranchTargetsPerFunction) {
            break;
          }
        }
      }
      if (selectedTargets.length === 0) {
        continue;
      }

      const sortedBranches = [...fnBranchRow.branches].sort((a, b) => {
        const locCmp = compareLocations(a.location, b.location);
        if (locCmp !== 0) {
          return locCmp;
        }
        const kindCmp = compareStrings(a.kind, b.kind);
        return kindCmp !== 0
          ? kindCmp
          : compareStrings(a.direction, b.direction);
      });
      const selectedBranches = sortedBranches.slice(0, maxBranchesPerFunction);
      const branchSpec = selectedBranches
        .map((b) => `${b.kind}:${b.direction}@${formatPos(b.location)}`)
        .join(",");

      const targetLocByHandle = new Map<string, DiagnosticLocation>();
      for (const target of selectedTargets) {
        targetLocByHandle.set(target.ref.handleId, target.location);
      }

      const coverageRows = await ctx.branchCoverage({
        function: fnNode.ref,
        branches: selectedBranches.map((b) => b.ref),
        targets: selectedTargets.map((t) => t.ref),
      });

      for (const [covIndex, item] of coverageRows.entries()) {
        const targetLoc =
          targetLocByHandle.get(item.target.handleId) ??
          selectedTargets[covIndex]?.location;
        if (targetLoc === undefined) {
          continue;
        }
        const targetId = `branchCoverage@fn(${formatPos(fnNode.location)})->target(${formatPos(targetLoc)})[${branchSpec}]`;
        addSample(
          "branchCoverage",
          "primary",
          targetLoc,
          targetId,
          item.status,
        );
        addSample(
          "branchCoverage",
          "implicitExceptions",
          targetLoc,
          targetId,
          item.implicitExceptions.status,
        );
      }
    }
  }

  // 4. valuePropagation, resolveInstances, instanceCleanup
  const fnsByFile = groupByFile(fnNodes);
  const varsByFile = groupByFile(varNodesRaw);

  for (const file of files) {
    const fileVars = varsByFile.get(file) ?? [];
    const fileCalls = callsByFile.get(file) ?? [];
    const fileReturns = returnsByFile.get(file) ?? [];
    const fileFns = fnsByFile.get(file) ?? [];

    // 4a. valuePropagation
    const fromEntries: {
      readonly ref: SyntaxRef;
      readonly location: DiagnosticLocation;
    }[] = [];
    for (const v of fileVars) {
      if (v.initializer !== undefined) {
        fromEntries.push({ ref: v.initializer, location: v.location });
        if (fromEntries.length >= maxValueSourcesPerFile) {
          break;
        }
      }
    }

    const toEntries: {
      readonly ref: SyntaxRef;
      readonly location: DiagnosticLocation;
    }[] = [];
    const toCandidateNodes = [...fileReturns, ...fileCalls].sort(
      compareSyntaxNodes,
    );
    for (const node of toCandidateNodes) {
      toEntries.push({ ref: node.ref, location: node.location });
      if (toEntries.length >= maxValueTargetsPerFile) {
        break;
      }
    }

    if (fromEntries.length > 0 && toEntries.length > 0) {
      const endpointLocByHandle = new Map<string, DiagnosticLocation>();
      for (const entry of [...fromEntries, ...toEntries]) {
        endpointLocByHandle.set(entry.ref.handleId, entry.location);
      }

      const vpResult = await ctx.valuePropagation({
        propagation: "value",
        includeMemberAccess: true,
        from: fromEntries.map((e) => e.ref),
        to: toEntries.map((e) => e.ref),
        lookupFiles: [file],
      });

      for (const row of vpResult.results) {
        const fromLoc = endpointLocByHandle.get(row.from.handleId);
        const toLoc = endpointLocByHandle.get(row.to.handleId);
        if (fromLoc === undefined || toLoc === undefined) {
          continue;
        }
        addSample(
          "valuePropagation",
          "primary",
          toLoc,
          `valuePropagation@${formatPos(fromLoc)}->${formatPos(toLoc)}`,
          row.status,
        );
      }
    }

    // 4b. resolveInstances -> instanceCleanup (primary + implicitExceptions)
    const instanceTargetCandidates: {
      readonly ref: SyntaxRef;
      readonly location: DiagnosticLocation;
    }[] = [];
    const seenInstSpan = new Set<string>();

    for (const call of fileCalls) {
      if (call.kind === "NewExpression") {
        const spanKey = `${String(call.location.start)}..${String(call.location.end)}`;
        if (!seenInstSpan.has(spanKey)) {
          seenInstSpan.add(spanKey);
          instanceTargetCandidates.push({
            ref: call.ref,
            location: call.location,
          });
        }
      }
    }
    for (const v of fileVars) {
      if (v.initializer !== undefined) {
        const spanKey = `${String(v.location.start)}..${String(v.location.end)}`;
        if (!seenInstSpan.has(spanKey)) {
          seenInstSpan.add(spanKey);
          instanceTargetCandidates.push({
            ref: v.initializer,
            location: v.location,
          });
        }
      }
    }

    const selectedInstanceTargets = instanceTargetCandidates
      .sort((a, b) => compareLocations(a.location, b.location))
      .slice(0, maxInstanceTargetsPerFile);

    if (selectedInstanceTargets.length > 0) {
      const instanceResults = await ctx.resolveInstances(
        selectedInstanceTargets.map((entry) => entry.ref),
        { lookupFiles: [file] },
      );

      const resolvedInstancesByKey = new Map<string, InstanceRef>();
      for (const [index, res] of instanceResults.entries()) {
        const entry = selectedInstanceTargets[index];
        if (entry === undefined) {
          continue;
        }
        addSample(
          "resolveInstances",
          "primary",
          entry.location,
          `resolveInstances@${formatPos(entry.location)}`,
          res.status,
        );

        if (res.status === "proved") {
          const primaryRef = res.instance ?? res.instances[0].ref;
          const primaryKey = res.instanceKey ?? res.instances[0].instanceKey;
          if (!resolvedInstancesByKey.has(primaryKey)) {
            resolvedInstancesByKey.set(primaryKey, primaryRef);
          }
        } else if (res.status === "inconclusive") {
          for (const inst of res.instances) {
            if (!resolvedInstancesByKey.has(inst.instanceKey)) {
              resolvedInstancesByKey.set(inst.instanceKey, inst.ref);
            }
          }
        }
      }

      if (fileFns.length > 0 && resolvedInstancesByKey.size > 0) {
        const sortedInstanceKeys = [...resolvedInstancesByKey.keys()]
          .sort(compareStrings)
          .slice(0, maxCleanupTargetsPerFile);
        const cleanupTargets = sortedInstanceKeys.map((key) =>
          resolvedInstancesByKey.get(key)!,
        );
        const cleanupScopes = fileFns.slice(0, maxCleanupScopesPerFile);

        const cleanupResults = await ctx.instanceCleanup({
          scopes: cleanupScopes.map((fn) => fn.ref),
          targets: cleanupTargets,
          lookupFiles: [file],
        });

        for (const [scopeIdx, scopeRes] of cleanupResults.entries()) {
          const scopeNode = cleanupScopes[scopeIdx];
          if (scopeNode === undefined) {
            continue;
          }
          for (const tr of scopeRes.targetResults) {
            const targetId = `instanceCleanup@${formatPos(scopeNode.location)}->${tr.instanceKey}`;
            addSample(
              "instanceCleanup",
              "primary",
              scopeNode.location,
              targetId,
              tr.status,
            );
            addSample(
              "instanceCleanup",
              "implicitExceptions",
              scopeNode.location,
              targetId,
              tr.implicitExceptions.status,
            );
          }
        }
      }
    }
  }

  return files.length;
}

/**
 * Deterministically samples all 8 public TSSAE query categories across a project
 * context or session, stripping temporary `snapshotId`/`handleId` handles.
 */
export async function collectQueryDigest(
  target: ProjectContext | ProjectSession,
  options: CollectQueryDigestOptions = {},
): Promise<QueryDigest> {
  const contexts = isProjectSession(target)
    ? [...target.projectContexts].sort((a, b) =>
        compareStrings(a.project.configFile, b.project.configFile),
      )
    : [target];

  const sampleMap = new Map<string, DigestSample>();
  let totalFiles = 0;

  for (const ctx of contexts) {
    totalFiles += await collectFromProjectContext(ctx, options, sampleMap);
  }

  const samples = [...sampleMap.values()].sort((a, b) => {
    const queryCmp = compareStrings(a.query, b.query);
    if (queryCmp !== 0) return queryCmp;
    const channelCmp = compareStrings(a.channel, b.channel);
    if (channelCmp !== 0) return channelCmp;
    const fileCmp = compareStrings(a.file, b.file);
    if (fileCmp !== 0) return fileCmp;
    if (a.location.start !== b.location.start) {
      return a.location.start - b.location.start;
    }
    if (a.location.end !== b.location.end) {
      return a.location.end - b.location.end;
    }
    return compareStrings(a.targetId, b.targetId);
  });

  return {
    schemaVersion: 1,
    projectCount: contexts.length,
    fileCount: totalFiles,
    sampleCount: samples.length,
    counts: buildCounts(samples),
    samples,
  };
}
