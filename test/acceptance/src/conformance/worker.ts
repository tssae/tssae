import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

import type {
  BranchRef,
  DiagnosticLocation,
  Gap,
  InstanceRef,
  OpenProjectOptions,
  ProjectContext,
  ProjectSession,
  SyntaxRef,
} from "tssae";

import type {
  ConformanceBatchWorkerConfig,
  ConformanceBatchWorkerResult,
  ConformanceViolation,
} from "./types.ts";

type TssaeModule = {
  readonly openProject: (
    options: OpenProjectOptions,
  ) => Promise<ProjectSession>;
};

const FUNCTION_BODY_KINDS = new Set([
  "FunctionDeclaration",
  "FunctionExpression",
  "ArrowFunction",
  "MethodDeclaration",
  "Constructor",
  "GetAccessor",
  "SetAccessor",
]);

const EXTRA_SCOPE_KINDS = new Set([
  "ClassStaticBlockDeclaration",
  "ModuleBlock",
]);

const TOP_LEVEL_STATEMENT_KINDS = new Set([
  "Block",
  "ExpressionStatement",
  "VariableStatement",
  "IfStatement",
  "SwitchStatement",
  "TryStatement",
  "ForStatement",
  "ForOfStatement",
  "ForInStatement",
  "WhileStatement",
  "DoStatement",
  "WithStatement",
  "LabeledStatement",
  "ReturnStatement",
  "ThrowStatement",
  "ClassDeclaration",
]);

const CALL_NODE_KINDS = new Set([
  "CallExpression",
  "NewExpression",
  "TaggedTemplateExpression",
]);

const INVARIANT_VALIDATION_PREFIX = "Scope analysis validation failed:";
const LIST_BRANCHES_CHUNK_SIZE = 256;
const MAX_REPRESENTATIVE_CALLS_PER_BATCH = 32;
const MAX_REPRESENTATIVE_VARS_PER_BATCH = 32;
const MAX_REPRESENTATIVE_BRANCH_FNS_PER_BATCH = 16;
const MAX_REPRESENTATIVE_SCOPES_PER_BATCH = 16;

function isInsideSpan(
  inner: DiagnosticLocation,
  outer: DiagnosticLocation,
): boolean {
  return inner.start >= outer.start && inner.end <= outer.end;
}

function resolveImportSpecifier(entryPath: string): string {
  if (entryPath === "tssae" || entryPath.startsWith("file://")) {
    return entryPath;
  }
  return pathToFileURL(resolve(entryPath)).href;
}

function collectInvariantGaps(
  gaps: readonly Gap[],
  query: string,
  fallbackFile: string,
  violations: ConformanceViolation[],
  corpusId: string,
  allowAnyUnmodeled: boolean,
): void {
  for (const gap of gaps) {
    if (gap.kind !== "unmodeled-construct") {
      continue;
    }
    if (
      !allowAnyUnmodeled &&
      !gap.message.startsWith(INVARIANT_VALIDATION_PREFIX)
    ) {
      continue;
    }
    violations.push({
      corpusId,
      kind: "invariant-violation",
      query,
      file: gap.at?.file ?? fallbackFile,
      message: gap.message,
      ...(gap.at !== undefined ? { at: gap.at } : {}),
    });
  }
}

async function sweepContext(
  ctx: ProjectContext,
  corpusId: string,
  violations: ConformanceViolation[],
): Promise<{
  readonly filesScanned: number;
  readonly scopesChecked: number;
  readonly queriesExecuted: number;
}> {
  let filesScanned = 0;
  let scopesChecked = 0;
  let queriesExecuted = 0;

  const repCallRefs: { readonly ref: SyntaxRef; readonly file: string }[] = [];
  const repVarInits: { readonly ref: SyntaxRef; readonly file: string }[] = [];
  const repScopes: { readonly ref: SyntaxRef; readonly file: string }[] = [];
  const repBranchCases: {
    readonly target: SyntaxRef;
    readonly isFunction: boolean;
    readonly branches: readonly BranchRef[];
    readonly targets: readonly SyntaxRef[];
    readonly file: string;
  }[] = [];

  for (const relFile of ctx.project.sourceFiles) {
    filesScanned += 1;
    const nodes = await ctx.findSyntax({ range: { file: relFile } });
    queriesExecuted += 1;
    if (nodes.length === 0) {
      continue;
    }

    const fileScopes: {
      readonly ref: SyntaxRef;
      readonly isFunction: boolean;
    }[] = [];
    const seenHandles = new Set<string>();
    const addScope = (ref: SyntaxRef, isFunction: boolean): void => {
      if (!seenHandles.has(ref.handleId)) {
        seenHandles.add(ref.handleId);
        fileScopes.push({ ref, isFunction });
      }
    };

    const fileTargets: SyntaxRef[] = [];
    const enclosingScopeSpans: DiagnosticLocation[] = [];

    for (const node of nodes) {
      if (FUNCTION_BODY_KINDS.has(node.kind) && node.body !== undefined) {
        addScope(node.ref, true);
        enclosingScopeSpans.push(node.location);
      } else if (EXTRA_SCOPE_KINDS.has(node.kind)) {
        addScope(node.ref, false);
        enclosingScopeSpans.push(node.location);
      }

      if (CALL_NODE_KINDS.has(node.kind) && fileTargets.length < 4) {
        fileTargets.push(node.ref);
        if (repCallRefs.length < MAX_REPRESENTATIVE_CALLS_PER_BATCH) {
          repCallRefs.push({ ref: node.ref, file: relFile });
        }
      } else if (
        (node.kind === "VariableDeclaration" ||
          node.kind === "PropertyDeclaration") &&
        node.initializer !== undefined &&
        repVarInits.length < MAX_REPRESENTATIVE_VARS_PER_BATCH
      ) {
        repVarInits.push({ ref: node.initializer, file: relFile });
      }
    }

    const rootTopLevelSpans: DiagnosticLocation[] = [];
    for (const node of nodes) {
      if (!TOP_LEVEL_STATEMENT_KINDS.has(node.kind)) {
        continue;
      }
      if (
        enclosingScopeSpans.some((scopeSpan) =>
          isInsideSpan(node.location, scopeSpan),
        )
      ) {
        continue;
      }
      if (
        rootTopLevelSpans.some((stmtSpan) =>
          isInsideSpan(node.location, stmtSpan),
        )
      ) {
        continue;
      }
      rootTopLevelSpans.push(node.location);
      addScope(node.ref, false);
    }

    if (
      fileScopes.length > 0 &&
      repScopes.length < MAX_REPRESENTATIVE_SCOPES_PER_BATCH
    ) {
      repScopes.push({ ref: fileScopes[0]!.ref, file: relFile });
    }

    const scopeKindsByHandle = new Map(
      fileScopes.map((s) => [s.ref.handleId, s.isFunction]),
    );

    for (let i = 0; i < fileScopes.length; i += LIST_BRANCHES_CHUNK_SIZE) {
      const chunk = fileScopes
        .slice(i, i + LIST_BRANCHES_CHUNK_SIZE)
        .map((s) => s.ref);
      scopesChecked += chunk.length;
      const branchResults = await ctx.listBranches(chunk);
      queriesExecuted += 1;
      for (const row of branchResults) {
        collectInvariantGaps(
          row.gaps,
          "listBranches",
          relFile,
          violations,
          corpusId,
          true,
        );
        if (
          row.branches.length > 0 &&
          repBranchCases.length < MAX_REPRESENTATIVE_BRANCH_FNS_PER_BATCH
        ) {
          const targets =
            fileTargets.length > 0 ? fileTargets : [nodes[0]!.ref];
          repBranchCases.push({
            target: row.target,
            isFunction: scopeKindsByHandle.get(row.target.handleId) ?? false,
            branches: row.branches.slice(0, 4).map((b) => b.ref),
            targets,
            file: relFile,
          });
        }
      }
    }
  }

  for (const item of repBranchCases) {
    const coverageRows = await ctx.branchCoverage(
      item.isFunction
        ? {
            function: item.target,
            branches: item.branches,
            targets: item.targets,
          }
        : {
            scope: item.target,
            branches: item.branches,
            targets: item.targets,
          },
    );
    queriesExecuted += 1;
    for (const row of coverageRows) {
      if (row.status === "inconclusive") {
        collectInvariantGaps(
          row.gaps,
          "branchCoverage",
          item.file,
          violations,
          corpusId,
          false,
        );
      }
      if (row.implicitExceptions.status === "inconclusive") {
        collectInvariantGaps(
          row.implicitExceptions.gaps,
          "branchCoverage.implicitExceptions",
          item.file,
          violations,
          corpusId,
          false,
        );
      }
    }
  }

  if (repCallRefs.length > 0) {
    const fallbackFile = repCallRefs[0]!.file;
    const callSyntaxRefs = repCallRefs.map((item) => item.ref);
    const resolvedCalls = await ctx.resolveCalls(callSyntaxRefs, {
      select: { signature: true, calleeDeclaration: true, arguments: true },
    });
    queriesExecuted += 1;

    const validCalls = resolvedCalls
      .map((row) => row.call)
      .filter((ref): ref is NonNullable<typeof ref> => ref !== null);

    if (validCalls.length > 0) {
      const callees = await ctx.calleesOf(validCalls);
      queriesExecuted += 1;
      const declRefs = callees
        .flatMap((row) => {
          if (row.completeness === "incomplete") {
            collectInvariantGaps(
              row.gaps,
              "calleesOf",
              fallbackFile,
              violations,
              corpusId,
              false,
            );
          }
          return row.resolvedTargets.map((target) => target.declaration);
        })
        .slice(0, 8);

      if (declRefs.length > 0) {
        const callers = await ctx.callersOf(declRefs);
        queriesExecuted += 1;
        for (const row of callers) {
          if (row.completeness === "incomplete") {
            collectInvariantGaps(
              row.gaps,
              "callersOf",
              fallbackFile,
              violations,
              corpusId,
              false,
            );
          }
        }
      }

      const impls = await ctx.resolveImplementations(validCalls.slice(0, 16));
      queriesExecuted += 1;
      for (const row of impls) {
        if (row.status === "inconclusive") {
          collectInvariantGaps(
            row.gaps,
            "resolveImplementations",
            fallbackFile,
            violations,
            corpusId,
            false,
          );
        }
      }

      const effects = await ctx.callEffects({
        targets: validCalls.slice(0, 12),
        followCallbacks: true,
        includeReads: true,
        includeWrites: true,
      });
      queriesExecuted += 1;
      for (const row of effects) {
        if (row.completeness === "incomplete") {
          collectInvariantGaps(
            row.gaps,
            "callEffects",
            fallbackFile,
            violations,
            corpusId,
            false,
          );
        }
      }
    }
  }

  let resolvedInstanceRefs: InstanceRef[] = [];
  if (repVarInits.length > 0) {
    const fallbackFile = repVarInits[0]!.file;
    const initRefs = repVarInits.map((item) => item.ref);
    const instances = await ctx.resolveInstances(initRefs.slice(0, 16));
    queriesExecuted += 1;
    for (const row of instances) {
      if (row.status === "proved" && row.instance !== null) {
        resolvedInstanceRefs.push(row.instance);
      } else if (row.status === "inconclusive") {
        collectInvariantGaps(
          row.gaps,
          "resolveInstances",
          fallbackFile,
          violations,
          corpusId,
          false,
        );
        for (const inst of row.instances) {
          resolvedInstanceRefs.push(inst.ref);
        }
      }
    }
    resolvedInstanceRefs = resolvedInstanceRefs.slice(0, 8);

    if (initRefs.length >= 2) {
      const mid = Math.ceil(initRefs.length / 2);
      const propagation = await ctx.valuePropagation({
        propagation: "value",
        from: initRefs.slice(0, Math.min(mid, 6)),
        to: initRefs.slice(mid, Math.min(mid + 6, initRefs.length)),
        includeMemberAccess: true,
      });
      queriesExecuted += 1;
      for (const row of propagation.results) {
        if (row.status === "inconclusive") {
          collectInvariantGaps(
            row.gaps,
            "valuePropagation",
            fallbackFile,
            violations,
            corpusId,
            false,
          );
        }
      }
    }
  }

  if (repScopes.length > 0) {
    const fallbackFile = repScopes[0]!.file;
    const cleanupRows = await ctx.instanceCleanup({
      scopes: repScopes.map((item) => item.ref),
      ...(resolvedInstanceRefs.length > 0
        ? { targets: resolvedInstanceRefs }
        : {}),
    });
    queriesExecuted += 1;
    for (const row of cleanupRows) {
      collectInvariantGaps(
        row.gaps,
        "instanceCleanup",
        fallbackFile,
        violations,
        corpusId,
        false,
      );
      for (const tr of row.targetResults) {
        if (tr.status === "inconclusive") {
          collectInvariantGaps(
            tr.gaps,
            "instanceCleanup.target",
            fallbackFile,
            violations,
            corpusId,
            false,
          );
        }
        if (tr.implicitExceptions.status === "inconclusive") {
          collectInvariantGaps(
            tr.implicitExceptions.gaps,
            "instanceCleanup.implicitExceptions",
            fallbackFile,
            violations,
            corpusId,
            false,
          );
        }
      }
    }
  }

  return { filesScanned, scopesChecked, queriesExecuted };
}

async function runWorker(): Promise<void> {
  const configPath = process.argv[2];
  if (configPath === undefined) {
    throw new Error("Conformance worker requires a JSON config path argument.");
  }
  const config = JSON.parse(
    readFileSync(resolve(configPath), "utf8"),
  ) as ConformanceBatchWorkerConfig;

  const tssae = (await import(
    resolveImportSpecifier(config.tssaeEntryPath)
  )) as TssaeModule;

  // Keep temporary tsconfig.batch.json outside the cached corpus checkout.
  const batchTempDir = mkdtempSync(join(tmpdir(), "tssae-conformance-batch-"));
  const tsconfigPath = join(batchTempDir, "tsconfig.batch.json");
  let session: ProjectSession | undefined;

  try {
    writeFileSync(
      tsconfigPath,
      `${JSON.stringify(
        {
          compilerOptions: {
            allowJs: true,
            checkJs: false,
            noEmit: true,
            noResolve: true,
            types: [],
            target: "esnext",
            module: "preserve",
            jsx: "preserve",
          },
          files: config.files.map((relPath) =>
            resolve(config.corpusDir, relPath),
          ),
        },
        null,
        2,
      )}\n`,
      "utf8",
    );

    session = await tssae.openProject({
      cwd: config.corpusDir,
      tsconfig: tsconfigPath,
      summaries: { discovery: ["builtin"] },
    });

    let filesScanned = 0;
    let scopesChecked = 0;
    let queriesExecuted = 0;
    const violations: ConformanceViolation[] = [];

    for (const ctx of session.projectContexts) {
      const counts = await sweepContext(ctx, config.corpusId, violations);
      filesScanned += counts.filesScanned;
      scopesChecked += counts.scopesChecked;
      queriesExecuted += counts.queriesExecuted;
    }

    const result: ConformanceBatchWorkerResult = {
      filesScanned,
      scopesChecked,
      queriesExecuted,
      violations,
    };
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } finally {
    try {
      await session?.close();
    } finally {
      rmSync(batchTempDir, { recursive: true, force: true });
    }
  }
}

void runWorker().catch((error: unknown) => {
  process.stderr.write(
    `${error instanceof Error ? (error.stack ?? error.message) : String(error)}\n`,
  );
  process.exitCode = 1;
});
