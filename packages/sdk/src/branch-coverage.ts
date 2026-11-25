import { registerProvenance } from "./evidence.ts";
import {
  VeruleExecutionError,
  getRefSessionToken,
  makeProgramRef,
  type BranchRef,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type ProgramRef,
  type SyntaxRef,
} from "./types.ts";

export type BranchKind =
  | "if"
  | "switchCase"
  | "switchDefault"
  | "conditional"
  | "logicalAnd"
  | "logicalOr"
  | "nullishCoalescing";

export type BranchDirection =
  | "true"
  | "false"
  | "case"
  | "default"
  | "shortCircuit"
  | "continue";

export type BranchCoverageStatus =
  | "covered"
  | "bypass"
  | "unreachable"
  | "unknown";

/** A single control-flow branch edge discovered within a function body. */
export type BranchEdgeInfo = {
  readonly ref: BranchRef;
  readonly kind: BranchKind;
  readonly direction: BranchDirection;
  readonly condition: SyntaxRef;
  readonly conditionText: string;
  readonly location: DiagnosticLocation;
  readonly statementLocation: DiagnosticLocation;
};

/** Branch edges and any control-flow modeling gaps discovered for a function node. */
export type FunctionBranchesResult = {
  readonly target: SyntaxRef;
  readonly branches: readonly BranchEdgeInfo[];
  readonly gaps: readonly Gap[];
};

/** Parameters for checking whether every modeled path from function or local block scope entry to each target passes selected branches. */
export type BranchCoverageOptions = {
  readonly function?: SyntaxRef;
  readonly scope?: SyntaxRef;
  readonly branches: readonly BranchRef[];
  readonly targets: readonly SyntaxRef[];
};

/** Result of a `branchCoverage` query for a single target syntax node. */
export type BranchCoverageItem = {
  readonly target: SyntaxRef;
  readonly status: BranchCoverageStatus;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

export type EngineBranchEdgeItem = {
  readonly ref: string;
  readonly kind: BranchKind;
  readonly direction: BranchDirection;
  readonly condition: string;
  readonly conditionText: string;
  readonly location: DiagnosticLocation;
  readonly statementLocation: DiagnosticLocation;
};

export type EngineFunctionBranchesItem = {
  readonly target: string;
  readonly branches: readonly EngineBranchEdgeItem[];
  readonly gaps: readonly Gap[];
};

export type EngineBranchCoverageItem = {
  readonly target: string;
  readonly status: BranchCoverageStatus;
  readonly evidence: readonly Evidence[];
  readonly gaps: readonly Gap[];
};

/** Validates that a program handle belongs to the active session, snapshot, and project. */
export function assertValidProgramRef(
  ref: ProgramRef,
  sessionToken: symbol,
  snapshotId: number,
  projectId: string,
): void {
  if (
    getRefSessionToken(ref) !== sessionToken ||
    ref.snapshotId !== snapshotId
  ) {
    throw new VeruleExecutionError(
      "STALE_REFERENCE",
      `Program reference "${ref.handleId}" belongs to a closed or stale snapshot.`,
    );
  }
  if (ref.projectId !== projectId) {
    throw new VeruleExecutionError(
      "CROSS_PROJECT_REFERENCE",
      `Program reference "${ref.handleId}" belongs to project "${ref.projectId}", not "${projectId}".`,
    );
  }
}

export function toFunctionBranchesResult(
  item: EngineFunctionBranchesItem,
  target: SyntaxRef,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
): FunctionBranchesResult {
  return {
    target,
    branches: item.branches.map((branch) =>
      registerProvenance(
        {
          ref: makeProgramRef(
            "branch",
            sessionToken,
            projectId,
            snapshotId,
            branch.ref,
          ),
          kind: branch.kind,
          direction: branch.direction,
          condition: makeProgramRef(
            "syntax",
            sessionToken,
            projectId,
            snapshotId,
            branch.condition,
          ),
          conditionText: branch.conditionText,
          location: branch.location,
          statementLocation: branch.statementLocation,
        },
        [
          {
            kind: "source",
            message: `Branch (${branch.kind} ${branch.direction}): ${branch.conditionText}`,
            at: branch.location,
          },
        ],
      ),
    ),
    gaps: item.gaps,
  };
}

export function toBranchCoverageItem(
  item: EngineBranchCoverageItem,
  target: SyntaxRef,
): BranchCoverageItem {
  return registerProvenance(
    {
      target,
      status: item.status,
      evidence: item.evidence,
      gaps: item.gaps,
    },
    item.evidence,
  );
}
