import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@tssae/test-hosts";
import {
  TssaeError,
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
            if (item.status === "bypass") {
              assert.equal(executeCalls[0]!.location.line, 16);
              assert.ok(item.evidence.length >= 1);
            }
          }
        }
      } finally {
        await session.close();
      }

      assert.deepEqual(observed, [
        { fn: "guardedEarlyReturn", status: "covered", evidenceCount: 1 },
        { fn: "bypassPossible", status: "bypass", evidenceCount: 2 },
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
          "multiAuthIfElse:onlyApi": "bypass",
          "multiAuthIfElse:onlySession": "bypass",
          "multiAuthIfElse:both": "covered",
          "switchWithDefault:onlyBearer": "bypass",
          "switchWithDefault:bothCases": "covered",
          "switchFallthrough:onlyBearer": "bypass",
          "switchFallthrough:bothCases": "covered",
          "switchNoDefault:onlyBearer": "bypass",
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
          call_0: "covered",
          call_1: "covered",
          call_2: "covered",
          call_3: "covered",
          call_4: "covered",
          method_wrong_branch: "bypass",
        });
      },
    );
  });

  void it("returns unreachable for dead targets and unknown with Gaps for unmodeled control flow", async () => {
    await withTempHost(
      { host: "sdk-queries/branch-unreachable-unknown" },
      async (host) => {
        await host.typecheck();

        const statuses: Record<string, string> = {};
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
            allGaps.push(...res!.gaps);
          }

          const tryCase = findFn("function tryCatchHandler(");
          tryBranchCount = tryCase.group.branches.length;
          const [tryRes] = await ctx.branchCoverage({
            function: tryCase.fn.ref,
            branches: tryCase.group.branches.map((b) => b.ref),
            targets: [tryCase.executeCall.ref],
          });
          statuses["tryCatch"] = tryRes!.status;
          gapKinds["tryCatch"] = tryRes!.gaps.map((g) => g.kind);
          allGaps.push(...tryRes!.gaps);

          const loopCase = findFn("function loopHandler(");
          const [loopRes] = await ctx.branchCoverage({
            function: loopCase.fn.ref,
            branches: loopCase.group.branches.map((b) => b.ref),
            targets: [loopCase.executeCall.ref],
          });
          statuses["loop"] = loopRes!.status;
          gapKinds["loop"] = loopRes!.gaps.map((g) => g.kind);
          allGaps.push(...loopRes!.gaps);

          const genCase = findFn("function* generatorHandler(");
          const [genRes] = await ctx.branchCoverage({
            function: genCase.fn.ref,
            branches: genCase.group.branches.map((b) => b.ref),
            targets: [genCase.executeCall.ref],
          });
          statuses["generator"] = genRes!.status;
          gapKinds["generator"] = genRes!.gaps.map((g) => g.kind);
          allGaps.push(...genRes!.gaps);

          // Outer function querying target inside nested closure must not confuse scopes
          const outerCase = findFn("function outerWithClosure(");
          const [outerRes] = await ctx.branchCoverage({
            function: outerCase.fn.ref,
            branches: [],
            targets: [outerCase.executeCall.ref],
          });
          statuses["outerForInnerTarget"] = outerRes!.status;
          gapKinds["outerForInnerTarget"] = outerRes!.gaps.map((g) => g.kind);
          allGaps.push(...outerRes!.gaps);

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
          deadAfterReturn: "unreachable",
          deadIfFalse: "unreachable",
          deadAfterSwitch: "unreachable",
          tryCatch: "unknown",
          loop: "unknown",
          generator: "unknown",
          outerForInnerTarget: "unknown",
          innerDirect: "bypass",
        });
        assert.deepEqual(gapKinds, {
          tryCatch: ["unmodeled-try-catch"],
          loop: ["unmodeled-loop"],
          generator: ["unmodeled-generator"],
          outerForInnerTarget: ["target-outside-cfg"],
        });
        assert.ok(allGaps.length >= 4);
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
        foreignGapKind = res!.gaps[0]?.kind;
        assert.equal(foreignResultStatus, "unknown");
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
              localBranches!.gaps.length + localCov!.gaps.length;
          }
        } finally {
          await session.close();
        }

        assert.deepEqual(wholeFnStatuses, {
          loopWithBypass: "unknown",
          tryLoopWithGuard: "unknown",
          loopWithContinueGuard: "unknown",
        });
        assert.deepEqual(localStatuses, {
          loopWithBypass: "bypass",
          tryLoopWithGuard: "covered",
          loopWithContinueGuard: "covered",
        });
        assert.deepEqual(localGapCounts, {
          loopWithBypass: 0,
          tryLoopWithGuard: 0,
          loopWithContinueGuard: 0,
        });
      },
    );
  });
});
