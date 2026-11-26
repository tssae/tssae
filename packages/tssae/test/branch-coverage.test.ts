import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { writeHostFiles, withTempHost } from "@tssae/test-hosts";
import {
  TssaeError,
  evidenceOf,
  openProject,
  type BranchEdgeInfo,
  type BranchRef,
  type ProjectContext,
  type SyntaxRef,
} from "tssae";

function verifyBranchTypeContracts(
  ctx: ProjectContext,
  syntaxRef: SyntaxRef,
  branchRef: BranchRef,
): void {
  void ctx.branchCoverage({
    // @ts-expect-error SyntaxRef cannot be passed where BranchRef is required
    branches: [syntaxRef],
    targets: [syntaxRef],
  });
  void ctx.branchCoverage({
    branches: [branchRef],
    // @ts-expect-error BranchRef cannot be passed where SyntaxRef is required
    targets: [branchRef],
  });
  void ctx.branchCoverage({
    // @ts-expect-error BranchRef cannot be passed as local scope
    scope: branchRef,
    branches: [branchRef],
    targets: [syntaxRef],
  });
  // @ts-expect-error BranchRef cannot be passed to listBranches
  void ctx.listBranches([branchRef]);
}

void verifyBranchTypeContracts;

function insideSpan(
  loc: { readonly start: number; readonly end: number },
  fnLoc: { readonly start: number; readonly end: number },
): boolean {
  return loc.start >= fnLoc.start && loc.end <= fnLoc.end;
}

void describe("tssae branchCoverage", () => {
  void it("distinguishes covered early-return guard from bypass path via listBranches and branchCoverage", async () => {
    await withTempHost({ host: "sdk-queries/branch-slice-1" }, async (host) => {
      await host.typecheck();

      const observed: Array<{
        fn: string;
        status: string;
        evidenceCount: number;
      }> = [];

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project();
        const fns = await ctx.findSyntax({ kinds: ["FunctionDeclaration"] });
        const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
        const branchGroups = await ctx.listBranches(fns.map((fn) => fn.ref));

        for (const [index, fn] of fns.entries()) {
          const executeCalls = calls.filter(
            (call) =>
              call.text.startsWith("execute(") &&
              insideSpan(call.location, fn.location),
          );
          if (executeCalls.length === 0) continue;

          const branches = branchGroups[index]!.branches;
          const selected: BranchRef[] = branches
            .filter(
              (branch) =>
                (branch.kind === "if" &&
                  branch.conditionText === "!isAuthorized(req)" &&
                  branch.direction === "false") ||
                (branch.kind === "if" &&
                  branch.conditionText === "isAuthorized(req)" &&
                  branch.direction === "true"),
            )
            .map((branch) => branch.ref);

          const targets: SyntaxRef[] = executeCalls.map((call) => call.ref);
          const coverage = await ctx.branchCoverage({
            function: fn.ref,
            branches: selected,
            targets,
          });

          for (const item of coverage) {
            observed.push({
              fn: fn.text.includes("guardedEarlyReturn")
                ? "guardedEarlyReturn"
                : "bypassPossible",
              status: item.status,
              evidenceCount: item.evidence.length,
            });
            if (item.status === "disproved") {
              assert.equal(executeCalls[0]!.location.line, 16);
              assert.ok(item.evidence.length >= 1);
              assert.ok(item.counterexamplePath.steps.length >= 1);
            }
          }
        }
      } finally {
        await session.close();
      }

      assert.deepEqual(observed, [
        { fn: "guardedEarlyReturn", status: "proved", evidenceCount: 1 },
        { fn: "bypassPossible", status: "disproved", evidenceCount: 2 },
      ]);
    });
  });

  void it("handles multi-branch joint coverage and switch/case/default fallthrough semantics", async () => {
    await withTempHost(
      { host: "sdk-queries/branch-joint-switch" },
      async (host) => {
        await host.typecheck();

        const results: Record<string, string> = {};
        const witnessMessages: Record<string, readonly string[]> = {};

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const fns = await ctx.findSyntax({
            kinds: ["FunctionDeclaration"],
          });
          const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
          const branchGroups = await ctx.listBranches(fns.map((fn) => fn.ref));

          const fnByName = (name: string) => {
            const idx = fns.findIndex((fn) =>
              fn.text.includes(`function ${name}(`),
            );
            assert.ok(idx >= 0, `expected function ${name}`);
            const fn = fns[idx]!;
            const executeCall = calls.find(
              (call) =>
                call.text.startsWith("execute(") &&
                insideSpan(call.location, fn.location),
            )!;
            return {
              fn,
              branches: branchGroups[idx]!.branches,
              executeCall,
            };
          };

          // 1. multiAuthIfElse: single branch bypasses, combined pair covers
          {
            const { fn, branches, executeCall } = fnByName("multiAuthIfElse");
            const apiKeyTrue = branches.find(
              (b) =>
                b.conditionText === "hasApiKey(req)" && b.direction === "true",
            )!;
            const sessionTrue = branches.find(
              (b) =>
                b.conditionText === "hasSession(req)" && b.direction === "true",
            )!;

            const [onlyApi] = await ctx.branchCoverage({
              function: fn.ref,
              branches: [apiKeyTrue.ref],
              targets: [executeCall.ref],
            });
            results["multiAuthIfElse:onlyApi"] = onlyApi!.status;
            witnessMessages["multiAuthIfElse:onlyApi"] = onlyApi!.evidence.map(
              (e) => e.message,
            );

            const [onlySession] = await ctx.branchCoverage({
              function: fn.ref,
              branches: [sessionTrue.ref],
              targets: [executeCall.ref],
            });
            results["multiAuthIfElse:onlySession"] = onlySession!.status;

            const [both] = await ctx.branchCoverage({
              function: fn.ref,
              branches: [apiKeyTrue.ref, sessionTrue.ref],
              targets: [executeCall.ref],
            });
            results["multiAuthIfElse:both"] = both!.status;
          }

          // 2. switchWithDefault
          {
            const { fn, branches, executeCall } = fnByName("switchWithDefault");
            const bearerCase = branches.find(
              (b) => b.kind === "switchCase" && b.conditionText === '"bearer"',
            )!;
            const hmacCase = branches.find(
              (b) => b.kind === "switchCase" && b.conditionText === '"hmac"',
            )!;
            const [onlyBearer] = await ctx.branchCoverage({
              function: fn.ref,
              branches: [bearerCase.ref],
              targets: [executeCall.ref],
            });
            results["switchWithDefault:onlyBearer"] = onlyBearer!.status;

            const [bothCases] = await ctx.branchCoverage({
              function: fn.ref,
              branches: [bearerCase.ref, hmacCase.ref],
              targets: [executeCall.ref],
            });
            results["switchWithDefault:bothCases"] = bothCases!.status;
          }

          // 3. switchFallthrough
          {
            const { fn, branches, executeCall } = fnByName("switchFallthrough");
            const legacyCase = branches.find(
              (b) => b.kind === "switchCase" && b.conditionText === '"legacy"',
            )!;
            const bearerCase = branches.find(
              (b) => b.kind === "switchCase" && b.conditionText === '"bearer"',
            )!;
            const [onlyBearer] = await ctx.branchCoverage({
              function: fn.ref,
              branches: [bearerCase.ref],
              targets: [executeCall.ref],
            });
            results["switchFallthrough:onlyBearer"] = onlyBearer!.status;

            const [bothCases] = await ctx.branchCoverage({
              function: fn.ref,
              branches: [legacyCase.ref, bearerCase.ref],
              targets: [executeCall.ref],
            });
            results["switchFallthrough:bothCases"] = bothCases!.status;
          }

          // 4. switchNoDefault
          {
            const { fn, branches, executeCall } = fnByName("switchNoDefault");
            const bearerCase = branches.find(
              (b) => b.kind === "switchCase" && b.conditionText === '"bearer"',
            )!;
            const [onlyBearer] = await ctx.branchCoverage({
              function: fn.ref,
              branches: [bearerCase.ref],
              targets: [executeCall.ref],
            });
            results["switchNoDefault:onlyBearer"] = onlyBearer!.status;
            witnessMessages["switchNoDefault:onlyBearer"] =
              onlyBearer!.evidence.map((e) => e.message);
          }
        } finally {
          await session.close();
        }

        assert.deepEqual(results, {
          "multiAuthIfElse:onlyApi": "disproved",
          "multiAuthIfElse:onlySession": "disproved",
          "multiAuthIfElse:both": "proved",
          "switchWithDefault:onlyBearer": "disproved",
          "switchWithDefault:bothCases": "proved",
          "switchFallthrough:onlyBearer": "disproved",
          "switchFallthrough:bothCases": "proved",
          "switchNoDefault:onlyBearer": "disproved",
        });
        assert.ok(
          witnessMessages["multiAuthIfElse:onlyApi"].some((msg) =>
            msg.includes("hasSession(req)"),
          ),
        );
        assert.ok(
          witnessMessages["switchNoDefault:onlyBearer"].some((msg) =>
            msg.includes("no default clause"),
          ),
        );
      },
    );
  });

  void it("supports methods, arrow functions, function expressions, ternary, and short-circuit operators", async () => {
    await withTempHost(
      { host: "sdk-queries/branch-expressions" },
      async (host) => {
        await host.typecheck();

        const statuses: Record<string, string> = {};
        let conditionTypeText: string | undefined;

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const fns = await ctx.findSyntax({
            kinds: ["MethodDeclaration", "ArrowFunction", "FunctionExpression"],
          });
          const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
          const branchGroups = await ctx.listBranches(fns.map((fn) => fn.ref));

          const condTypeResults = await ctx.typeAt([
            branchGroups[0]!.branches[0]!.condition,
          ]);
          conditionTypeText = condTypeResults[0]?.typeText;

          // Omitting `function` resolves enclosing function automatically for each target.
          const allBranches: BranchEdgeInfo[] = branchGroups.flatMap(
            (g) => g.branches,
          );
          const executeCalls = calls.filter((call) =>
            call.text.startsWith("execute("),
          );
          assert.equal(executeCalls.length, 5);

          const coveringBranches = allBranches
            .filter(
              (b) =>
                (b.kind === "conditional" && b.direction === "true") ||
                (b.kind === "logicalAnd" && b.direction === "continue") ||
                (b.kind === "logicalOr" && b.direction === "continue") ||
                (b.kind === "nullishCoalescing" && b.direction === "continue"),
            )
            .map((b) => b.ref);

          const coverage = await ctx.branchCoverage({
            branches: coveringBranches,
            targets: executeCalls.map((c) => c.ref),
          });
          for (const [idx, item] of coverage.entries()) {
            statuses[`call_${idx}`] = item.status;
          }

          // Wrong branch direction yields bypass
          const wrongBranches = allBranches
            .filter((b) => b.kind === "conditional" && b.direction === "false")
            .map((b) => b.ref);
          const [methodBypass] = await ctx.branchCoverage({
            branches: wrongBranches,
            targets: [executeCalls[0]!.ref],
          });
          statuses["method_wrong_branch"] = methodBypass!.status;
        } finally {
          await session.close();
        }

        assert.equal(conditionTypeText, "boolean");
        assert.deepEqual(statuses, {
          call_0: "proved",
          call_1: "proved",
          call_2: "proved",
          call_3: "proved",
          call_4: "proved",
          method_wrong_branch: "disproved",
        });
      },
    );
  });

  void it("distinguishes unreachable targets (proved with targetReachable=false) and inconclusive critical-path gaps", async () => {
    await withTempHost(
      { host: "sdk-queries/branch-unreachable-unknown" },
      async (host) => {
        await host.typecheck();

        const statuses: Record<string, string> = {};
        const reachability: Record<string, boolean> = {};
        const gapKinds: Record<string, readonly string[]> = {};
        const allGaps: Array<{
          readonly at?: unknown;
          readonly message: string;
        }> = [];
        let tryBranchCount = 0;

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const fns = await ctx.findSyntax({
            kinds: ["FunctionDeclaration", "ArrowFunction"],
          });
          const calls = await ctx.findSyntax({ kinds: ["CallExpression"] });
          const branchGroups = await ctx.listBranches(fns.map((fn) => fn.ref));

          const findFn = (prefix: string) => {
            const idx = fns.findIndex((fn) => fn.text.includes(prefix));
            assert.ok(idx >= 0, `expected fn ${prefix}`);
            const fn = fns[idx]!;
            const executeCall = calls.find(
              (call) =>
                call.text.startsWith("execute(") &&
                insideSpan(call.location, fn.location),
            )!;
            return {
              fn,
              group: branchGroups[idx]!,
              executeCall,
            };
          };

          for (const [label, fnName] of [
            ["deadAfterReturn", "deadAfterReturn"],
            ["deadIfFalse", "deadIfFalse"],
            ["deadAfterSwitch", "deadAfterSwitch"],
          ] as const) {
            const { fn, group, executeCall } = findFn(`function ${fnName}(`);
            const [res] = await ctx.branchCoverage({
              function: fn.ref,
              branches: group.branches.map((b) => b.ref),
              targets: [executeCall.ref],
            });
            statuses[label] = res!.status;
            reachability[label] = res!.targetReachable;
            if (res!.status === "inconclusive") {
              allGaps.push(...res!.gaps);
            }
          }

          const tryCase = findFn("function tryCatchHandler(");
          tryBranchCount = tryCase.group.branches.length;
          // With guard branches selected, `if (isAuthorized(req))` still dominates `execute(req)`
          const [tryGuardedRes] = await ctx.branchCoverage({
            function: tryCase.fn.ref,
            branches: tryCase.group.branches.map((b) => b.ref),
            targets: [tryCase.executeCall.ref],
          });
          statuses["tryCatchGuarded"] = tryGuardedRes!.status;
          reachability["tryCatchGuarded"] = tryGuardedRes!.targetReachable;

          // Without guard branches selected, the bypass path goes through `try/catch`
          const [tryRes] = await ctx.branchCoverage({
            function: tryCase.fn.ref,
            branches: [],
            targets: [tryCase.executeCall.ref],
          });
          statuses["tryCatch"] = tryRes!.status;
          if (tryRes!.status === "inconclusive") {
            gapKinds["tryCatch"] = tryRes!.gaps.map((g) => g.kind);
            allGaps.push(...tryRes!.gaps);
          }

          const loopCase = findFn("function loopHandler(");
          const [loopGuardedRes] = await ctx.branchCoverage({
            function: loopCase.fn.ref,
            branches: loopCase.group.branches.map((b) => b.ref),
            targets: [loopCase.executeCall.ref],
          });
          statuses["loopGuarded"] = loopGuardedRes!.status;

          const [loopRes] = await ctx.branchCoverage({
            function: loopCase.fn.ref,
            branches: [],
            targets: [loopCase.executeCall.ref],
          });
          statuses["loop"] = loopRes!.status;
          if (loopRes!.status === "inconclusive") {
            gapKinds["loop"] = loopRes!.gaps.map((g) => g.kind);
            allGaps.push(...loopRes!.gaps);
          }

          const genCase = findFn("function* generatorHandler(");
          const [genGuardedRes] = await ctx.branchCoverage({
            function: genCase.fn.ref,
            branches: genCase.group.branches.map((b) => b.ref),
            targets: [genCase.executeCall.ref],
          });
          statuses["generatorGuarded"] = genGuardedRes!.status;

          const [genRes] = await ctx.branchCoverage({
            function: genCase.fn.ref,
            branches: [],
            targets: [genCase.executeCall.ref],
          });
          statuses["generator"] = genRes!.status;
          if (genRes!.status === "inconclusive") {
            gapKinds["generator"] = genRes!.gaps.map((g) => g.kind);
            allGaps.push(...genRes!.gaps);
          }

          // Outer function querying target inside nested closure must not confuse scopes
          const outerCase = findFn("function outerWithClosure(");
          const [outerRes] = await ctx.branchCoverage({
            function: outerCase.fn.ref,
            branches: [],
            targets: [outerCase.executeCall.ref],
          });
          statuses["outerForInnerTarget"] = outerRes!.status;
          if (outerRes!.status === "inconclusive") {
            gapKinds["outerForInnerTarget"] = outerRes!.gaps.map((g) => g.kind);
            allGaps.push(...outerRes!.gaps);
          }

          // Querying the inner arrow function directly works
          const innerArrow = fns.find(
            (fn) =>
              fn.kind === "ArrowFunction" && fn.text.includes("execute(req)"),
          )!;
          const [innerRes] = await ctx.branchCoverage({
            function: innerArrow.ref,
            branches: [],
            targets: [outerCase.executeCall.ref],
          });
          statuses["innerDirect"] = innerRes!.status;
        } finally {
          await session.close();
        }

        assert.equal(tryBranchCount, 2);
        assert.deepEqual(statuses, {
          deadAfterReturn: "proved",
          deadIfFalse: "proved",
          deadAfterSwitch: "proved",
          tryCatchGuarded: "proved",
          tryCatch: "disproved",
          loopGuarded: "proved",
          loop: "disproved",
          generatorGuarded: "proved",
          generator: "disproved",
          outerForInnerTarget: "inconclusive",
          innerDirect: "disproved",
        });
        assert.deepEqual(reachability, {
          deadAfterReturn: false,
          deadIfFalse: false,
          deadAfterSwitch: false,
          tryCatchGuarded: true,
        });
        assert.deepEqual(gapKinds, {
          outerForInnerTarget: ["target-outside-cfg"],
        });
        assert.equal(allGaps.length, 1);
        assert.ok(
          allGaps.every(
            (gap) => gap.at !== undefined && gap.message.length > 0,
          ),
        );
      },
    );
  });

  void it("rejects stale BranchRefs across sessions and foreign BranchRefs when function is explicit", async () => {
    await withTempHost({ host: "sdk-queries/branch-slice-1" }, async (host) => {
      let capturedBranchRef: BranchRef | undefined;

      const session1 = await openProject({ cwd: host.rootDir });
      let foreignResultStatus: string | undefined;
      let foreignGapKind: string | undefined;
      try {
        const ctx = session1.project();
        const fns = (
          await ctx.findSyntax({
            kinds: ["FunctionDeclaration"],
          })
        ).filter((fn) => !fn.text.startsWith("declare "));
        const calls = await ctx.findSyntax({
          kinds: ["CallExpression"],
        });
        const groups = await ctx.listBranches(fns.map((fn) => fn.ref));
        capturedBranchRef = groups[0]!.branches[0]!.ref;

        const secondFn = fns[1]!;
        const secondExecute = calls.find(
          (c) =>
            c.text.startsWith("execute(") &&
            insideSpan(c.location, secondFn.location),
        )!;
        const [res] = await ctx.branchCoverage({
          function: secondFn.ref,
          branches: [capturedBranchRef],
          targets: [secondExecute.ref],
        });
        foreignResultStatus = res!.status;
        assert.equal(res!.status, "inconclusive");
        foreignGapKind = res!.gaps[0]?.kind;
        assert.equal(foreignResultStatus, "inconclusive");
        assert.equal(foreignGapKind, "foreign-branch-reference");
      } finally {
        await session1.close();
      }

      assert.ok(capturedBranchRef);
      const session2 = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session2.project();
        const calls = await ctx.findSyntax({
          kinds: ["CallExpression"],
        });
        await assert.rejects(
          () =>
            ctx.branchCoverage({
              branches: [capturedBranchRef],
              targets: [calls[0]!.ref],
            }),
          (err: unknown) =>
            err instanceof TssaeError && err.code === "STALE_REFERENCE",
        );
      } finally {
        await session2.close();
      }
    });
  });

  void it("evaluates local-scope branchCoverage inside loop and try/finally bodies without degrading on outer control flow", async () => {
    await withTempHost(
      { host: "sdk-queries/branch-local-scope" },
      async (host) => {
        await host.typecheck();

        const localStatuses: Record<string, string> = {};
        const localGapCounts: Record<string, number> = {};
        const wholeFnStatuses: Record<string, string> = {};

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();
          const fns = await ctx.findSyntax({
            kinds: ["FunctionDeclaration"],
          });
          const blocks = await ctx.findSyntax({ kinds: ["Block"] });
          const writes = (
            await ctx.findSyntax({ kinds: ["BinaryExpression"] })
          ).filter((expr) => expr.text.includes("&="));

          for (const fnName of [
            "loopWithBypass",
            "tryLoopWithGuard",
            "loopWithContinueGuard",
          ] as const) {
            const fn = fns.find((f) => f.text.includes(`function ${fnName}(`))!;
            const fnWrites = writes.filter((w) =>
              insideSpan(w.location, fn.location),
            );
            const postClear = fnWrites.at(-1)!;

            // Whole-function query preserves unmodeled outer loop/try gaps
            const [wholeFnCov] = await ctx.branchCoverage({
              function: fn.ref,
              branches: [],
              targets: [postClear.ref],
            });
            wholeFnStatuses[fnName] = wholeFnCov!.status;

            // Pick the loop body block (outermost block inside the loop/try that has no loop/try keywords of its own)
            const candidateBlocks = blocks
              .filter(
                (b) =>
                  insideSpan(b.location, fn.location) &&
                  insideSpan(postClear.location, b.location) &&
                  !b.text.includes("for (") &&
                  !b.text.includes("try {") &&
                  b.text.includes("ALLOW_RECURSE"),
              )
              .toSorted(
                (a, b) =>
                  b.location.end -
                  b.location.start -
                  (a.location.end - a.location.start),
              );
            const localBlock = candidateBlocks[0]!;

            const [localBranches] = await ctx.listBranches([localBlock.ref]);
            const guardBranches = localBranches!.branches.filter(
              (b) =>
                b.conditionText.includes("ALLOW_RECURSE") &&
                ((b.conditionText.startsWith("!") && b.direction === "true") ||
                  (!b.conditionText.startsWith("!") &&
                    b.direction === "false")),
            );

            const [localCov] = await ctx.branchCoverage({
              scope: localBlock.ref,
              branches: guardBranches.map((b) => b.ref),
              targets: [postClear.ref],
            });
            localStatuses[fnName] = localCov!.status;
            localGapCounts[fnName] =
              localBranches!.gaps.length +
              (localCov!.status === "inconclusive" ? localCov!.gaps.length : 0);
          }
        } finally {
          await session.close();
        }

        assert.deepEqual(wholeFnStatuses, {
          loopWithBypass: "disproved",
          tryLoopWithGuard: "disproved",
          loopWithContinueGuard: "disproved",
        });
        assert.deepEqual(localStatuses, {
          loopWithBypass: "disproved",
          tryLoopWithGuard: "proved",
          loopWithContinueGuard: "proved",
        });
        assert.deepEqual(localGapCounts, {
          loopWithBypass: 0,
          tryLoopWithGuard: 0,
          loopWithContinueGuard: 0,
        });
      },
    );
  });

  void it("excludes unrelated unknown constructs from gaps, proves guarded targets despite off-path unknowns, and reports known bypasses over parallel unknowns", async () => {
    await withTempHost({}, async (host) => {
      writeHostFiles(host.rootDir, {
        "package.json": JSON.stringify(
          { name: "ir-unrelated-gaps", private: true, type: "module" },
          null,
          2,
        ),
        "tsconfig.json": JSON.stringify(
          {
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              jsx: "preserve",
              noEmit: true,
            },
            include: ["src/**/*.tsx", "src/**/*.d.ts"],
          },
          null,
          2,
        ),
        "src/jsx-ambient.d.ts": `declare namespace JSX {
  interface IntrinsicElements {
    tag: Record<string, never>;
  }
}
`,
        "src/cases.tsx": `declare function isAuthorized(req: string): boolean;
declare function log(msg: string): void;
declare function execute(req: string): void;
declare function consume(v: unknown): void;

export function unrelatedUnknownAroundGuard(
  req: string,
  debug: boolean,
): void {
  if (debug) {
    consume(<tag />);
    return;
  }
  if (!isAuthorized(req)) {
    return;
  }
  execute(req);
  consume(<tag />);
}

export function knownBypassWithParallelUnknown(
  req: string,
  useJsx: boolean,
): void {
  if (useJsx) {
    consume(<tag />);
  } else if (isAuthorized(req)) {
    log(req);
  }
  execute(req);
}

export function criticalVsOffPathUnknown(
  req: string,
  debug: boolean,
): void {
  if (debug) {
    try {
      log(req);
    } catch {
      log("err");
    }
    return;
  }
  if (isAuthorized(req)) {
    consume(<tag />);
  } else {
    return;
  }
  execute(req);
}

export function optionalChainShortCircuit(
  cfg: { readonly run?: (req: string) => void } | undefined,
  req: string,
): void {
  cfg?.run?.(req);
}
`,
      });
      await host.typecheck();

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project();
        const fns = await ctx.findSyntax({
          files: ["src/cases.tsx"],
          kinds: ["FunctionDeclaration"],
        });
        const calls = await ctx.findSyntax({
          files: ["src/cases.tsx"],
          kinds: ["CallExpression"],
        });
        const branchGroups = await ctx.listBranches(fns.map((f) => f.ref));

        const findCase = (name: string) => {
          const idx = fns.findIndex((f) =>
            f.text.includes(`function ${name}(`),
          );
          assert.ok(idx >= 0, `expected ${name}`);
          const fn = fns[idx]!;
          const target = calls.find(
            (c) =>
              c.text.startsWith("execute(") &&
              insideSpan(c.location, fn.location),
          );
          return { fn, branches: branchGroups[idx]!.branches, target };
        };

        // 1. Unrelated unknown in returning branch + unknown after target do NOT degrade proved
        {
          const c = findCase("unrelatedUnknownAroundGuard");
          const authFalse = c.branches.find(
            (b) =>
              b.conditionText === "!isAuthorized(req)" &&
              b.direction === "false",
          )!;
          const [res] = await ctx.branchCoverage({
            function: c.fn.ref,
            branches: [authFalse.ref],
            targets: [c.target!.ref],
          });
          assert.equal(res!.status, "proved");
          assert.equal(res!.targetReachable, true);
        }

        // 2. Known explicit bypass alongside parallel unknown yields "disproved" with counterexamplePath
        {
          const c = findCase("knownBypassWithParallelUnknown");
          const [res] = await ctx.branchCoverage({
            function: c.fn.ref,
            branches: [],
            targets: [c.target!.ref],
          });
          assert.ok(res);
          assert.equal(res.status, "disproved");
          if (res.status === "disproved") {
            assert.ok(res.counterexamplePath.steps.length >= 2);
            assert.ok(res.counterexamplePath.evidence.length >= 2);
          }
        }

        // 3. Critical-path unknown yields "inconclusive" with ONLY critical-path unmodeled-construct gaps
        {
          const c = findCase("criticalVsOffPathUnknown");
          const authFalse = c.branches.find(
            (b) =>
              b.conditionText === "isAuthorized(req)" &&
              b.direction === "false",
          );
          const [res] = await ctx.branchCoverage({
            function: c.fn.ref,
            branches: authFalse ? [authFalse.ref] : [],
            targets: [c.target!.ref],
          });
          assert.ok(res);
          assert.equal(res.status, "inconclusive");
          if (res.status === "inconclusive") {
            assert.ok(res.gaps.length >= 1);
            assert.ok(res.gaps.every((g) => g.kind === "unmodeled-construct"));
            assert.ok(res.gaps.every((g) => insideSpan(g.at!, c.fn.location)));
          }
        }

        // 4. Optional chaining `?.` exposes optionalChain continue/shortCircuit branches
        {
          const c = findCase("optionalChainShortCircuit");
          const optBranches = c.branches.filter(
            (b) => b.kind === "optionalChain",
          );
          assert.ok(
            optBranches.some((b) => b.direction === "continue") &&
              optBranches.some((b) => b.direction === "shortCircuit"),
          );
        }
      } finally {
        await session.close();
      }
    });
  });

  void it("models loops, labeled jumps, try/catch/finally, yield/await, explicit throw, and implicit mayThrow conclusions", async () => {
    await withTempHost({}, async (host) => {
      writeHostFiles(host.rootDir, {
        "package.json": JSON.stringify(
          { name: "ir-control-flow-ticket-04", private: true, type: "module" },
          null,
          2,
        ),
        "tsconfig.json": JSON.stringify(
          {
            compilerOptions: {
              strict: true,
              target: "ES2022",
              module: "NodeNext",
              moduleResolution: "NodeNext",
              jsx: "preserve",
              noEmit: true,
            },
            include: ["src/**/*.tsx", "src/**/*.d.ts"],
          },
          null,
          2,
        ),
        "src/jsx-ambient.d.ts": `declare namespace JSX {
  interface IntrinsicElements {
    tag: Record<string, never>;
  }
}
`,
        "src/control-flow.tsx": `declare function execute(tag: string): void;
declare function risky(): void;
declare function mightThrow(): void;
declare function cleanup(): void;
declare function consume(v: unknown): void;
declare function asyncStream(): AsyncIterable<string>;

export function collatzGuarded(n: number, ok: boolean): void {
  while (n !== 1) {
    if (n % 2 === 0) {
      n = n / 2;
    } else {
      n = 3 * n + 1;
    }
  }
  if (!ok) {
    return;
  }
  execute("collatz");
}

export function explicitThrowIntoCatch(ok: boolean): void {
  try {
    if (!ok) {
      throw "explicit";
    }
    return;
  } catch {
    execute("caught-explicit");
  }
}

export function explicitThrowSwallowedBypass(ok: boolean): void {
  try {
    if (!ok) {
      throw "swallowed";
    }
    return;
  } catch {
    // swallowed
  }
  execute("after-explicit-catch");
}

export function implicitThrowToCatchOnly(): void {
  try {
    risky();
  } catch {
    execute("caught-implicit");
  }
}

export function implicitThrowBeforeGuard(ok: boolean): void {
  try {
    mightThrow();
    if (!ok) {
      return;
    }
  } catch {
    execute("catch-bypass");
  }
}

export function guardedImplicitThrowInTryBeforeTarget(ok: boolean): void {
  try {
    if (!ok) {
      mightThrow();
      return;
    }
  } catch {}
  execute("after-try-implicit");
}

export function guardedBeforeTryImplicitThrow(ok: boolean): void {
  if (!ok) {
    return;
  }
  try {
    mightThrow();
  } catch {}
  execute("safe-after-try");
}

export function tryFinallyReturnRouting(ok: boolean): void {
  try {
    if (!ok) {
      return;
    }
  } finally {
    cleanup();
  }
  execute("after-finally");
}

export function tryFinallyOverrideReturn(ok: boolean): void {
  try {
    if (!ok) {
      return;
    }
  } finally {
    return;
  }
  // oxlint-disable-next-line no-unreachable
  execute("unreachable-after-finally-return");
}

export function labeledBreakWithFinally(ok: boolean): void {
  outer: for (let i = 0; i < 2; i++) {
    try {
      if (!ok) {
        break outer;
      }
    } finally {
      cleanup();
    }
    execute("inside-outer");
  }
}

export function labeledContinueOuterLoop(ok: boolean): void {
  outer: for (let i = 0; i < 2; i++) {
    let j = 0;
    do {
      if (ok) {
        continue outer;
      }
      j += 1;
    } while (j < 2);
    execute("after-inner-loop");
  }
}

export function labeledBlockBreak(ok: boolean): void {
  blockExit: {
    if (!ok) {
      break blockExit;
    }
    execute("inside-labeled-block");
  }
}

export async function awaitRejectionBypass(p: Promise<void>): Promise<void> {
  try {
    await p;
  } catch {
    execute("caught-await");
  }
}

export function* generatorYieldVariants(
  other: Generator<number>,
  ok: boolean,
): Generator<number> {
  yield 1;
  try {
    yield* other;
  } catch {
    execute("caught-yield-star");
  }
  if (!ok) {
    return;
  }
  execute("after-yield");
}

export async function forAwaitAndDoWhile(items: Record<string, number>): Promise<void> {
  let count = 0;
  do {
    count += 1;
  } while (count < 2);
  for (const key in items) {
    if (key === "skip") {
      continue;
    }
  }
  for await (const item of asyncStream()) {
    if (item === "ok") {
      execute("for-await");
    }
  }
}

export function implicitBypassThroughUnknown(flag: boolean): void {
  try {
    if (flag) {
      consume(<tag />);
      risky();
    }
  } catch {
    execute("caught-unknown-implicit");
  }
}
`,
      });
      await host.typecheck();

      const session = await openProject({
        cwd: host.rootDir,
        tsconfig: "tsconfig.json",
      });
      try {
        const ctx = session.project();
        const fns = await ctx.findSyntax({
          files: ["src/control-flow.tsx"],
          kinds: ["FunctionDeclaration"],
        });
        const calls = await ctx.findSyntax({
          files: ["src/control-flow.tsx"],
          kinds: ["CallExpression"],
        });
        const exprStmts = await ctx.findSyntax({
          files: ["src/control-flow.tsx"],
          kinds: ["ExpressionStatement"],
        });
        const branchGroups = await ctx.listBranches(fns.map((f) => f.ref));

        const getCase = (name: string) => {
          const idx = fns.findIndex(
            (f) =>
              f.text.includes(`function ${name}(`) ||
              f.text.includes(`function* ${name}(`),
          );
          assert.ok(idx >= 0, `expected function ${name}`);
          const fn = fns[idx]!;
          const group = branchGroups[idx]!;
          const fnCalls = calls.filter((c) =>
            insideSpan(c.location, fn.location),
          );
          const execCalls = fnCalls.filter((c) =>
            c.text.startsWith("execute("),
          );
          return { fn, group, fnCalls, execCalls };
        };

        // 1. Collatz loop + guard -> both proved, loop branches exposed
        {
          const c = getCase("collatzGuarded");
          assert.equal(c.group.gaps.length, 0);
          assert.ok(
            c.group.branches.some(
              (b) => b.kind === "loop" && b.conditionText === "n !== 1",
            ),
          );
          const guardFalse = c.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [guarded] = await ctx.branchCoverage({
            function: c.fn.ref,
            branches: [guardFalse.ref],
            targets: [c.execCalls[0]!.ref],
          });
          assert.equal(guarded!.status, "proved");
          assert.equal(guarded!.targetReachable, true);
          assert.equal(guarded!.implicitExceptions.status, "proved");

          const [unguarded] = await ctx.branchCoverage({
            function: c.fn.ref,
            branches: [],
            targets: [c.execCalls[0]!.ref],
          });
          assert.equal(unguarded!.status, "disproved");
        }

        // 2. Explicit throw bypassing guard -> main status disproved with counterexamplePath
        {
          const c1 = getCase("explicitThrowIntoCatch");
          const guardFalse = c1.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [res1] = await ctx.branchCoverage({
            function: c1.fn.ref,
            branches: [guardFalse.ref],
            targets: [c1.execCalls[0]!.ref],
          });
          assert.equal(res1!.status, "disproved");
          assert.equal(res1!.targetReachable, true);
          assert.equal(res1!.implicitExceptions.status, "proved");
          if (res1!.status === "disproved") {
            assert.ok(
              res1!.counterexamplePath.steps.some((s) =>
                s.description.includes("Explicit throw"),
              ),
            );
          }

          const c2 = getCase("explicitThrowSwallowedBypass");
          const guardFalse2 = c2.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [res2] = await ctx.branchCoverage({
            function: c2.fn.ref,
            branches: [guardFalse2.ref],
            targets: [c2.execCalls[0]!.ref],
          });
          assert.equal(res2!.status, "disproved");
          assert.equal(res2!.implicitExceptions.status, "proved");
        }

        // 3. Catch reachable ONLY via implicit exception -> main proved (unreachable), implicit disproved with mayThrowSites
        {
          const c = getCase("implicitThrowToCatchOnly");
          const riskyCall = c.fnCalls.find((x) => x.text === "risky()")!;
          const [res] = await ctx.branchCoverage({
            function: c.fn.ref,
            branches: [],
            targets: [c.execCalls[0]!.ref],
          });
          assert.equal(res!.status, "proved");
          assert.equal(res!.targetReachable, false);
          assert.equal(res!.implicitExceptions.status, "disproved");
          if (res!.implicitExceptions.status === "disproved") {
            assert.ok(
              res!.implicitExceptions.mayThrowSites.some(
                (loc) => loc.line === riskyCall.location.line,
              ),
            );
            assert.ok(
              res!.implicitExceptions.evidence.some(
                (ev) => ev.at?.line === riskyCall.location.line,
              ),
            );
            assert.ok(
              evidenceOf(res!.implicitExceptions).some(
                (ev) => ev.at?.line === riskyCall.location.line,
              ),
            );
          }
        }

        // 4. Implicit throw before guard vs guarded in try vs guarded before try
        {
          const cBefore = getCase("implicitThrowBeforeGuard");
          const mightCall = cBefore.fnCalls.find(
            (x) => x.text === "mightThrow()",
          )!;
          const guardFalse = cBefore.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [resBefore] = await ctx.branchCoverage({
            function: cBefore.fn.ref,
            branches: [guardFalse.ref],
            targets: [cBefore.execCalls[0]!.ref],
          });
          assert.equal(resBefore!.status, "proved");
          assert.equal(resBefore!.targetReachable, false);
          assert.equal(resBefore!.implicitExceptions.status, "disproved");
          if (resBefore!.implicitExceptions.status === "disproved") {
            assert.ok(
              resBefore!.implicitExceptions.mayThrowSites.some(
                (loc) => loc.line === mightCall.location.line,
              ),
            );
          }

          const cInTry = getCase("guardedImplicitThrowInTryBeforeTarget");
          const mightInTry = cInTry.fnCalls.find(
            (x) => x.text === "mightThrow()",
          )!;
          const guardInTry = cInTry.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [resInTry] = await ctx.branchCoverage({
            function: cInTry.fn.ref,
            branches: [guardInTry.ref],
            targets: [cInTry.execCalls[0]!.ref],
          });
          assert.equal(resInTry!.status, "proved");
          assert.equal(resInTry!.targetReachable, true);
          assert.equal(resInTry!.implicitExceptions.status, "disproved");
          if (resInTry!.implicitExceptions.status === "disproved") {
            assert.ok(
              resInTry!.implicitExceptions.mayThrowSites.some(
                (loc) => loc.line === mightInTry.location.line,
              ),
            );
          }

          const cSafe = getCase("guardedBeforeTryImplicitThrow");
          const guardSafe = cSafe.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [resSafe] = await ctx.branchCoverage({
            function: cSafe.fn.ref,
            branches: [guardSafe.ref],
            targets: [cSafe.execCalls[0]!.ref],
          });
          assert.equal(resSafe!.status, "proved");
          assert.equal(resSafe!.targetReachable, true);
          assert.equal(resSafe!.implicitExceptions.status, "proved");
        }

        // 5. finally completion routing, override return, labeled break/continue, labeled block
        {
          const cFinally = getCase("tryFinallyReturnRouting");
          const guardFalse = cFinally.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [resFinally] = await ctx.branchCoverage({
            function: cFinally.fn.ref,
            branches: [guardFalse.ref],
            targets: [cFinally.execCalls[0]!.ref],
          });
          assert.equal(resFinally!.status, "proved");
          assert.equal(resFinally!.targetReachable, true);
          assert.equal(resFinally!.implicitExceptions.status, "proved");

          const cOverride = getCase("tryFinallyOverrideReturn");
          const [resOverride] = await ctx.branchCoverage({
            function: cOverride.fn.ref,
            branches: [],
            targets: [cOverride.execCalls[0]!.ref],
          });
          assert.equal(resOverride!.status, "proved");
          assert.equal(resOverride!.targetReachable, false);

          const cLblBreak = getCase("labeledBreakWithFinally");
          const breakGuardFalse = cLblBreak.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [resLblBreak] = await ctx.branchCoverage({
            function: cLblBreak.fn.ref,
            branches: [breakGuardFalse.ref],
            targets: [cLblBreak.execCalls[0]!.ref],
          });
          assert.equal(resLblBreak!.status, "proved");
          assert.equal(resLblBreak!.implicitExceptions.status, "proved");

          const cLblCont = getCase("labeledContinueOuterLoop");
          const contGuardFalse = cLblCont.group.branches.find(
            (b) => b.conditionText === "ok" && b.direction === "false",
          )!;
          const [resLblCont] = await ctx.branchCoverage({
            function: cLblCont.fn.ref,
            branches: [contGuardFalse.ref],
            targets: [cLblCont.execCalls[0]!.ref],
          });
          assert.equal(resLblCont!.status, "proved");

          const cLblBlock = getCase("labeledBlockBreak");
          const blkGuardFalse = cLblBlock.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [resLblBlock] = await ctx.branchCoverage({
            function: cLblBlock.fn.ref,
            branches: [blkGuardFalse.ref],
            targets: [cLblBlock.execCalls[0]!.ref],
          });
          assert.equal(resLblBlock!.status, "proved");
        }

        // 6. await, yield/yield*, do...while, for...in, for await...of have zero gaps and model rejections
        {
          const cAwait = getCase("awaitRejectionBypass");
          assert.equal(cAwait.group.gaps.length, 0);
          const awaitStmt = exprStmts.find(
            (s) =>
              s.text.startsWith("await ") &&
              insideSpan(s.location, cAwait.fn.location),
          )!;
          const [resAwait] = await ctx.branchCoverage({
            function: cAwait.fn.ref,
            branches: [],
            targets: [cAwait.execCalls[0]!.ref],
          });
          assert.equal(resAwait!.status, "proved");
          assert.equal(resAwait!.targetReachable, false);
          assert.equal(resAwait!.implicitExceptions.status, "disproved");
          if (resAwait!.implicitExceptions.status === "disproved") {
            assert.ok(
              resAwait!.implicitExceptions.mayThrowSites.some(
                (loc) => loc.line === awaitStmt.location.line,
              ),
            );
          }

          const cGen = getCase("generatorYieldVariants");
          assert.equal(cGen.group.gaps.length, 0);
          const yieldStarStmt = exprStmts.find(
            (s) =>
              s.text.startsWith("yield*") &&
              insideSpan(s.location, cGen.fn.location),
          )!;
          const genGuardFalse = cGen.group.branches.find(
            (b) => b.conditionText === "!ok" && b.direction === "false",
          )!;
          const [resCaughtYield, resAfterYield] = await ctx.branchCoverage({
            function: cGen.fn.ref,
            branches: [genGuardFalse.ref],
            targets: [cGen.execCalls[0]!.ref, cGen.execCalls[1]!.ref],
          });
          assert.equal(resCaughtYield!.status, "proved");
          assert.equal(resCaughtYield!.targetReachable, false);
          assert.equal(resCaughtYield!.implicitExceptions.status, "disproved");
          if (resCaughtYield!.implicitExceptions.status === "disproved") {
            assert.ok(
              resCaughtYield!.implicitExceptions.mayThrowSites.some(
                (loc) => loc.line === yieldStarStmt.location.line,
              ),
            );
          }
          assert.equal(resAfterYield!.status, "proved");
          assert.equal(resAfterYield!.targetReachable, true);
          assert.equal(resAfterYield!.implicitExceptions.status, "proved");

          const cForAwait = getCase("forAwaitAndDoWhile");
          assert.equal(cForAwait.group.gaps.length, 0);
          const itemOkBranch = cForAwait.group.branches.find(
            (b) =>
              b.conditionText === 'item === "ok"' && b.direction === "true",
          )!;
          const [resForAwait] = await ctx.branchCoverage({
            function: cForAwait.fn.ref,
            branches: [itemOkBranch.ref],
            targets: [cForAwait.execCalls[0]!.ref],
          });
          assert.equal(resForAwait!.status, "proved");
          assert.equal(resForAwait!.targetReachable, true);
        }

        // 7. Implicit exception path passing through Unknown -> inconclusive with gaps + mayThrowSites
        {
          const cUnk = getCase("implicitBypassThroughUnknown");
          const riskyCall = cUnk.fnCalls.find((x) => x.text === "risky()")!;
          const [resUnk] = await ctx.branchCoverage({
            function: cUnk.fn.ref,
            branches: [],
            targets: [cUnk.execCalls[0]!.ref],
          });
          assert.equal(resUnk!.status, "proved");
          assert.equal(resUnk!.targetReachable, false);
          assert.equal(resUnk!.implicitExceptions.status, "inconclusive");
          if (resUnk!.implicitExceptions.status === "inconclusive") {
            assert.ok(resUnk!.implicitExceptions.gaps.length >= 1);
            assert.ok(
              resUnk!.implicitExceptions.gaps.every(
                (g) => g.kind === "unmodeled-construct",
              ),
            );
            assert.ok(
              resUnk!.implicitExceptions.mayThrowSites.some(
                (loc) => loc.line === riskyCall.location.line,
              ),
            );
          }
        }
      } finally {
        await session.close();
      }
    });
  });
});
