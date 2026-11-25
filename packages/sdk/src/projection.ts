import { performance } from "node:perf_hooks";

import type { EngineClient, RpcRequestOptions } from "./engine-client.ts";
import { registerProvenance, summarizeSyntaxSnippet } from "./evidence.ts";
import {
  VeruleExecutionError,
  getRefSessionToken,
  makeProgramRef,
  type CallRef,
  type DeclarationRef,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type ProgramRef,
  type SignatureRef,
  type SyntaxKind,
  type SyntaxRef,
  type TypeRef,
} from "./types.ts";

export type SelectedKeys<S> = {
  [K in keyof S]-?: undefined extends S[K]
    ? never
    : S[K] extends false
      ? never
      : K;
}[keyof S];

export type TypeSummary = {
  readonly ref: TypeRef;
  readonly typeText: string;
  readonly isUnion: boolean;
  readonly isIntersection: boolean;
  readonly isLiteral: boolean;
  readonly literalValue?: string | number | boolean;
  readonly unionMembers?: readonly string[];
};

export type InstantiatedParameterType = {
  readonly index: number;
  readonly parameterName: string | null;
  readonly isRest: boolean;
  readonly type: TypeSummary;
};

export type SignatureParameterInfo = {
  readonly name: string;
  readonly declaration: DeclarationRef | null;
  readonly optional: boolean;
  readonly rest: boolean;
};

export type TypePropertyInfo = {
  readonly name: string;
  readonly optional: boolean;
  readonly declaration: DeclarationRef | null;
};

export type DeclarationProjectionSelection = {
  readonly name?: true;
  readonly containerName?: true;
  readonly kind?: true;
  readonly location?: true;
  readonly syntax?: true;
  readonly isExternal?: true;
  readonly declarations?: true;
};

export type FullDeclarationProjectionFields = {
  readonly name: string | null;
  readonly containerName: string | null;
  readonly kind: SyntaxKind | null;
  readonly location: DiagnosticLocation | null;
  readonly syntax: SyntaxRef | null;
  readonly isExternal: boolean;
  readonly declarations: readonly DeclarationRef[];
};

export type ProjectedDeclarationRow<
  S extends DeclarationProjectionSelection = Record<never, never>,
> = {
  readonly target: SyntaxRef | CallRef | SignatureRef;
  readonly declaration: DeclarationRef | null;
  readonly gaps: readonly Gap[];
} & Pick<
  FullDeclarationProjectionFields,
  Extract<SelectedKeys<S>, keyof FullDeclarationProjectionFields>
>;

export type CallProjectionSelection = {
  readonly location?: true;
  readonly syntax?: true;
  readonly callee?: true;
  readonly calleeDeclaration?: true;
  readonly signature?: true;
  readonly arguments?: true;
  readonly instantiatedParameterTypes?: readonly number[] | true;
  readonly returnType?: true;
};

export type FullCallProjectionFields = {
  readonly location: DiagnosticLocation;
  readonly syntax: SyntaxRef;
  readonly callee: SyntaxRef | null;
  readonly calleeDeclaration: DeclarationRef | null;
  readonly signature: SignatureRef | null;
  readonly arguments: readonly SyntaxRef[];
  readonly instantiatedParameterTypes: readonly InstantiatedParameterType[];
  readonly returnType: TypeSummary | null;
};

export type ProjectedCallRow<S extends CallProjectionSelection> = {
  readonly call: CallRef;
  readonly gaps: readonly Gap[];
} & Pick<
  FullCallProjectionFields,
  Extract<SelectedKeys<S>, keyof FullCallProjectionFields>
>;

export type ProjectedCallResolutionRow<
  S extends CallProjectionSelection = Record<never, never>,
> = {
  readonly target: SyntaxRef;
  readonly call: CallRef | null;
  readonly gaps: readonly Gap[];
} & Pick<
  FullCallProjectionFields,
  Extract<SelectedKeys<S>, keyof FullCallProjectionFields>
>;

export type SignatureProjectionSelection = {
  readonly signatureText?: true;
  readonly declaration?: true;
  readonly location?: true;
  readonly syntax?: true;
  readonly parameters?: true;
  readonly minArgumentCount?: true;
  readonly hasRestParameter?: true;
  readonly instantiatedParameterTypes?: readonly number[] | true;
  readonly returnType?: true;
  readonly isInstantiated?: true;
};

export type FullSignatureProjectionFields = {
  readonly signatureText: string;
  readonly declaration: DeclarationRef | null;
  readonly location: DiagnosticLocation | null;
  readonly syntax: SyntaxRef | null;
  readonly parameters: readonly SignatureParameterInfo[];
  readonly minArgumentCount: number;
  readonly hasRestParameter: boolean;
  readonly instantiatedParameterTypes: readonly InstantiatedParameterType[];
  readonly returnType: TypeSummary;
  readonly isInstantiated: boolean;
};

export type ProjectedSignatureRow<S extends SignatureProjectionSelection> = {
  readonly signature: SignatureRef;
  readonly gaps: readonly Gap[];
} & Pick<
  FullSignatureProjectionFields,
  Extract<SelectedKeys<S>, keyof FullSignatureProjectionFields>
>;

export type TypeProjectionSelection = {
  readonly typeText?: true;
  readonly isUnion?: true;
  readonly isIntersection?: true;
  readonly isLiteral?: true;
  readonly literalValue?: true;
  readonly unionMembers?: true;
  readonly unionMemberTypes?: true;
  readonly properties?: true;
  readonly typeArguments?: true;
};

export type FullTypeProjectionFields = {
  readonly typeText: string;
  readonly isUnion: boolean;
  readonly isIntersection: boolean;
  readonly isLiteral: boolean;
  readonly literalValue: string | number | boolean | null;
  readonly unionMembers: readonly string[];
  readonly unionMemberTypes: readonly TypeRef[];
  readonly properties: readonly TypePropertyInfo[];
  readonly typeArguments: readonly TypeRef[];
};

export type ProjectedTypeRow<S extends TypeProjectionSelection> = {
  readonly type: TypeRef;
  readonly gaps: readonly Gap[];
} & Pick<
  FullTypeProjectionFields,
  Extract<SelectedKeys<S>, keyof FullTypeProjectionFields>
>;

export type SyntaxLookupSelection = {
  readonly kind?: true;
  readonly text?: true;
  readonly location?: true;
  readonly expression?: true;
  readonly arguments?: true;
  readonly name?: true;
  readonly initializer?: true;
  readonly body?: true;
  readonly parameters?: true;
};

export type FullSyntaxLookupFields = {
  readonly kind: (SyntaxKind | (string & {})) | null;
  readonly text: string | null;
  readonly location: DiagnosticLocation | null;
  readonly expression: SyntaxRef | null;
  readonly arguments: readonly SyntaxRef[];
  readonly name: SyntaxRef | null;
  readonly initializer: SyntaxRef | null;
  readonly body: SyntaxRef | null;
  readonly parameters: readonly SyntaxRef[];
};

export type ProjectedSyntaxLookupRow<
  S extends SyntaxLookupSelection = Record<never, never>,
> = {
  readonly target: SyntaxRef | DeclarationRef | CallRef | SignatureRef;
  readonly syntax: SyntaxRef | null;
  readonly gaps: readonly Gap[];
} & Pick<
  FullSyntaxLookupFields,
  Extract<SelectedKeys<S>, keyof FullSyntaxLookupFields>
>;

export type AssignabilityPair = {
  readonly source: TypeRef;
  readonly target: TypeRef;
};

export type AssignabilityResult = {
  readonly source: TypeRef;
  readonly target: TypeRef;
  readonly assignable: boolean;
  readonly gaps: readonly Gap[];
};

type RawTypeSummary = {
  readonly ref: string;
  readonly typeText: string;
  readonly isUnion: boolean;
  readonly isIntersection: boolean;
  readonly isLiteral: boolean;
  readonly literalValue?: string | number | boolean;
  readonly unionMembers?: readonly string[];
};

type RawInstantiatedParameter = {
  readonly index: number;
  readonly parameterName: string | null;
  readonly isRest: boolean;
  readonly type: RawTypeSummary;
};

type RawDeclarationRow = {
  readonly target: string;
  readonly declaration: string | null;
  readonly gaps: readonly Gap[];
  readonly name?: string | null;
  readonly containerName?: string | null;
  readonly kind?: SyntaxKind | null;
  readonly location?: DiagnosticLocation | null;
  readonly syntax?: string | null;
  readonly isExternal?: boolean;
  readonly declarations?: readonly string[];
};

type RawCallRow = {
  readonly target?: string;
  readonly call: string | null;
  readonly gaps: readonly Gap[];
  readonly location?: DiagnosticLocation;
  readonly syntax?: string;
  readonly callee?: string | null;
  readonly calleeDeclaration?: string | null;
  readonly signature?: string | null;
  readonly arguments?: readonly string[];
  readonly instantiatedParameterTypes?: readonly RawInstantiatedParameter[];
  readonly returnType?: RawTypeSummary | null;
};

type RawSignatureRow = {
  readonly signature: string;
  readonly gaps: readonly Gap[];
  readonly signatureText?: string;
  readonly declaration?: string | null;
  readonly location?: DiagnosticLocation | null;
  readonly syntax?: string | null;
  readonly parameters?: readonly {
    readonly name: string;
    readonly declaration: string | null;
    readonly optional: boolean;
    readonly rest: boolean;
  }[];
  readonly minArgumentCount?: number;
  readonly hasRestParameter?: boolean;
  readonly instantiatedParameterTypes?: readonly RawInstantiatedParameter[];
  readonly returnType?: RawTypeSummary;
  readonly isInstantiated?: boolean;
};

type RawTypeRow = {
  readonly type: string;
  readonly gaps: readonly Gap[];
  readonly typeText?: string;
  readonly isUnion?: boolean;
  readonly isIntersection?: boolean;
  readonly isLiteral?: boolean;
  readonly literalValue?: string | number | boolean | null;
  readonly unionMembers?: readonly string[];
  readonly unionMemberTypes?: readonly string[];
  readonly properties?: readonly {
    readonly name: string;
    readonly optional: boolean;
    readonly declaration: string | null;
  }[];
  readonly typeArguments?: readonly string[];
};

type RawAssignabilityRow = {
  readonly source: string;
  readonly target: string;
  readonly assignable: boolean;
  readonly gaps: readonly Gap[];
};

type RawSyntaxLookupRow = {
  readonly target: string;
  readonly syntax: string | null;
  readonly gaps: readonly Gap[];
  readonly kind?: SyntaxKind | null;
  readonly text?: string | null;
  readonly location?: DiagnosticLocation | null;
  readonly expression?: string | null;
  readonly arguments?: readonly string[];
  readonly name?: string | null;
  readonly initializer?: string | null;
  readonly body?: string | null;
  readonly parameters?: readonly string[];
};

type EngineBatchResult = {
  readonly results: readonly {
    readonly items: readonly unknown[];
  }[];
};

/** Validates that every reference belongs to the active session, snapshot, and project. */
export function validateProgramRefs(
  refs: readonly ProgramRef[],
  sessionToken: symbol,
  snapshotId: number,
  projectId: string,
): void {
  for (const ref of refs) {
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
}

export function toTypeSummary(
  raw: RawTypeSummary,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
): TypeSummary {
  return {
    ref: makeProgramRef("type", sessionToken, projectId, snapshotId, raw.ref),
    typeText: raw.typeText,
    isUnion: raw.isUnion,
    isIntersection: raw.isIntersection,
    isLiteral: raw.isLiteral,
    ...(raw.literalValue !== undefined
      ? { literalValue: raw.literalValue }
      : {}),
    ...(raw.unionMembers !== undefined
      ? { unionMembers: raw.unionMembers }
      : {}),
  };
}

function toInstantiatedParams(
  rawList: readonly RawInstantiatedParameter[],
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
): readonly InstantiatedParameterType[] {
  return rawList.map((item) => ({
    index: item.index,
    parameterName: item.parameterName,
    isRest: item.isRest,
    type: toTypeSummary(item.type, sessionToken, projectId, snapshotId),
  }));
}

function assignCallProjectionFields(
  targetObj: Record<string, unknown>,
  raw: RawCallRow,
  select: CallProjectionSelection | undefined,
  sessionToken: symbol,
  projectId: string,
  snapshotId: number,
): void {
  if (!select) return;
  if (select.location === true) {
    targetObj.location = raw.location;
  }
  if (select.syntax === true) {
    targetObj.syntax = makeProgramRef(
      "syntax",
      sessionToken,
      projectId,
      snapshotId,
      raw.syntax!,
    );
  }
  if (select.callee === true) {
    targetObj.callee =
      raw.callee != null
        ? makeProgramRef(
            "syntax",
            sessionToken,
            projectId,
            snapshotId,
            raw.callee,
          )
        : null;
  }
  if (select.calleeDeclaration === true) {
    targetObj.calleeDeclaration =
      raw.calleeDeclaration != null
        ? makeProgramRef(
            "declaration",
            sessionToken,
            projectId,
            snapshotId,
            raw.calleeDeclaration,
          )
        : null;
  }
  if (select.signature === true) {
    targetObj.signature =
      raw.signature != null
        ? makeProgramRef(
            "signature",
            sessionToken,
            projectId,
            snapshotId,
            raw.signature,
          )
        : null;
  }
  if (select.arguments === true) {
    targetObj.arguments = (raw.arguments ?? []).map((id) =>
      makeProgramRef("syntax", sessionToken, projectId, snapshotId, id),
    );
  }
  if (select.instantiatedParameterTypes !== undefined) {
    targetObj.instantiatedParameterTypes = toInstantiatedParams(
      raw.instantiatedParameterTypes ?? [],
      sessionToken,
      projectId,
      snapshotId,
    );
  }
  if (select.returnType === true) {
    targetObj.returnType =
      raw.returnType != null
        ? toTypeSummary(raw.returnType, sessionToken, projectId, snapshotId)
        : null;
  }
}

export function createProjectionQueries(deps: {
  readonly client: EngineClient;
  readonly sessionToken: symbol;
  readonly snapshotId: number;
  readonly projectId: string;
  readonly assertOpen: () => void;
  readonly recordGaps: (gaps: readonly Gap[]) => void;
  readonly getRpcOptions?: () => RpcRequestOptions | undefined;
  readonly onQueryCompleted?: (
    op: string,
    candidateCount: number,
    durationMs: number,
  ) => void;
}) {
  const {
    client,
    sessionToken,
    snapshotId,
    projectId,
    assertOpen,
    recordGaps,
    getRpcOptions,
    onQueryCompleted,
  } = deps;

  async function runStep<T>(
    step: Record<string, unknown>,
  ): Promise<readonly T[]> {
    const start = onQueryCompleted ? performance.now() : 0;
    const batch = await client.request<EngineBatchResult>(
      "query.batch",
      {
        snapshotId,
        projectId,
        steps: [step],
      },
      getRpcOptions?.(),
    );
    const items = (batch.results[0]?.items ?? []) as readonly T[];
    if (onQueryCompleted && typeof step.op === "string") {
      onQueryCompleted(step.op, items.length, performance.now() - start);
    }
    return items;
  }

  return {
    resolveDeclarations: async <
      const S extends DeclarationProjectionSelection = Record<never, never>,
    >(
      targets: readonly (SyntaxRef | CallRef | SignatureRef)[],
      options?: { readonly select?: S },
    ): Promise<readonly ProjectedDeclarationRow<S>[]> => {
      assertOpen();
      if (targets.length === 0) return [];
      validateProgramRefs(targets, sessionToken, snapshotId, projectId);
      const select = options?.select;
      const items = await runStep<RawDeclarationRow>({
        op: "resolveDeclarations",
        targets: targets.map((t) => t.handleId),
        ...(select !== undefined ? { select } : {}),
      });

      return items.map((raw, index) => {
        recordGaps(raw.gaps);
        const row: Record<string, unknown> = {
          target: targets[index]!,
          declaration:
            raw.declaration !== null
              ? makeProgramRef(
                  "declaration",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.declaration,
                )
              : null,
          gaps: raw.gaps,
        };
        if (select?.name === true) row.name = raw.name ?? null;
        if (select?.containerName === true) {
          row.containerName = raw.containerName ?? null;
        }
        if (select?.kind === true) row.kind = raw.kind ?? null;
        if (select?.location === true) row.location = raw.location ?? null;
        if (select?.syntax === true) {
          row.syntax =
            raw.syntax != null
              ? makeProgramRef(
                  "syntax",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.syntax,
                )
              : null;
        }
        if (select?.isExternal === true) {
          row.isExternal = raw.isExternal ?? false;
        }
        if (select?.declarations === true) {
          row.declarations = (raw.declarations ?? []).map((id) =>
            makeProgramRef(
              "declaration",
              sessionToken,
              projectId,
              snapshotId,
              id,
            ),
          );
        }
        const declEvidence: Evidence[] = [];
        if (select?.location === true && raw.location != null) {
          declEvidence.push({
            kind: "source",
            message:
              select.name === true && raw.name != null
                ? `Declaration: ${raw.name}`
                : "Declaration",
            at: raw.location,
            ...(select.name === true && raw.name != null
              ? { declaration: raw.name }
              : {}),
          });
        } else if (select?.name === true && raw.name != null) {
          declEvidence.push({
            kind: "source",
            message: `Declaration: ${raw.name}`,
            declaration: raw.name,
          });
        }
        return registerProvenance(
          row as ProjectedDeclarationRow<S>,
          declEvidence,
        );
      });
    },

    resolveCalls: async <
      const S extends CallProjectionSelection = Record<never, never>,
    >(
      targets: readonly SyntaxRef[],
      options?: { readonly select?: S },
    ): Promise<readonly ProjectedCallResolutionRow<S>[]> => {
      assertOpen();
      if (targets.length === 0) return [];
      validateProgramRefs(targets, sessionToken, snapshotId, projectId);
      const select = options?.select;
      const items = await runStep<RawCallRow>({
        op: "resolveCalls",
        targets: targets.map((t) => t.handleId),
        ...(select !== undefined ? { select } : {}),
      });

      return items.map((raw, index) => {
        recordGaps(raw.gaps);
        const row: Record<string, unknown> = {
          target: targets[index]!,
          call:
            raw.call !== null
              ? makeProgramRef(
                  "call",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.call,
                )
              : null,
          gaps: raw.gaps,
        };
        assignCallProjectionFields(
          row,
          raw,
          select,
          sessionToken,
          projectId,
          snapshotId,
        );
        return registerProvenance(
          row as ProjectedCallResolutionRow<S>,
          select?.location === true && raw.location != null
            ? [
                {
                  kind: "source",
                  message: "Call expression",
                  at: raw.location,
                },
              ]
            : [],
        );
      });
    },

    projectCalls: async <const S extends CallProjectionSelection>(
      calls: readonly CallRef[],
      options: { readonly select: S },
    ): Promise<readonly ProjectedCallRow<S>[]> => {
      assertOpen();
      if (calls.length === 0) return [];
      validateProgramRefs(calls, sessionToken, snapshotId, projectId);
      const select = options.select;
      const items = await runStep<RawCallRow>({
        op: "projectCalls",
        targets: calls.map((c) => c.handleId),
        select,
      });

      return items.map((raw, index) => {
        recordGaps(raw.gaps);
        const row: Record<string, unknown> = {
          call: calls[index]!,
          gaps: raw.gaps,
        };
        assignCallProjectionFields(
          row,
          raw,
          select,
          sessionToken,
          projectId,
          snapshotId,
        );
        return registerProvenance(
          row as ProjectedCallRow<S>,
          select.location === true && raw.location != null
            ? [
                {
                  kind: "source",
                  message: "Call expression",
                  at: raw.location,
                },
              ]
            : [],
        );
      });
    },

    projectSignatures: async <const S extends SignatureProjectionSelection>(
      signatures: readonly SignatureRef[],
      options: { readonly select: S },
    ): Promise<readonly ProjectedSignatureRow<S>[]> => {
      assertOpen();
      if (signatures.length === 0) return [];
      validateProgramRefs(signatures, sessionToken, snapshotId, projectId);
      const select = options.select;
      const items = await runStep<RawSignatureRow>({
        op: "projectSignatures",
        targets: signatures.map((s) => s.handleId),
        select,
      });

      return items.map((raw, index) => {
        recordGaps(raw.gaps);
        const row: Record<string, unknown> = {
          signature: signatures[index]!,
          gaps: raw.gaps,
        };
        if (select.signatureText === true) {
          row.signatureText = raw.signatureText ?? "";
        }
        if (select.declaration === true) {
          row.declaration =
            raw.declaration != null
              ? makeProgramRef(
                  "declaration",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.declaration,
                )
              : null;
        }
        if (select.location === true) row.location = raw.location ?? null;
        if (select.syntax === true) {
          row.syntax =
            raw.syntax != null
              ? makeProgramRef(
                  "syntax",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.syntax,
                )
              : null;
        }
        if (select.parameters === true) {
          row.parameters = (raw.parameters ?? []).map((p) => ({
            name: p.name,
            declaration:
              p.declaration !== null
                ? makeProgramRef(
                    "declaration",
                    sessionToken,
                    projectId,
                    snapshotId,
                    p.declaration,
                  )
                : null,
            optional: p.optional,
            rest: p.rest,
          }));
        }
        if (select.minArgumentCount === true) {
          row.minArgumentCount = raw.minArgumentCount ?? 0;
        }
        if (select.hasRestParameter === true) {
          row.hasRestParameter = raw.hasRestParameter ?? false;
        }
        if (select.instantiatedParameterTypes !== undefined) {
          row.instantiatedParameterTypes = toInstantiatedParams(
            raw.instantiatedParameterTypes ?? [],
            sessionToken,
            projectId,
            snapshotId,
          );
        }
        if (select.returnType === true) {
          row.returnType = toTypeSummary(
            raw.returnType!,
            sessionToken,
            projectId,
            snapshotId,
          );
        }
        if (select.isInstantiated === true) {
          row.isInstantiated = raw.isInstantiated ?? false;
        }
        return row as ProjectedSignatureRow<S>;
      });
    },

    projectTypes: async <const S extends TypeProjectionSelection>(
      types: readonly TypeRef[],
      options: { readonly select: S },
    ): Promise<readonly ProjectedTypeRow<S>[]> => {
      assertOpen();
      if (types.length === 0) return [];
      validateProgramRefs(types, sessionToken, snapshotId, projectId);
      const select = options.select;
      const items = await runStep<RawTypeRow>({
        op: "projectTypes",
        targets: types.map((t) => t.handleId),
        select,
      });

      return items.map((raw, index) => {
        recordGaps(raw.gaps);
        const row: Record<string, unknown> = {
          type: types[index]!,
          gaps: raw.gaps,
        };
        if (select.typeText === true) row.typeText = raw.typeText ?? "";
        if (select.isUnion === true) row.isUnion = raw.isUnion ?? false;
        if (select.isIntersection === true) {
          row.isIntersection = raw.isIntersection ?? false;
        }
        if (select.isLiteral === true) row.isLiteral = raw.isLiteral ?? false;
        if (select.literalValue === true) {
          row.literalValue = raw.literalValue ?? null;
        }
        if (select.unionMembers === true) {
          row.unionMembers = raw.unionMembers ?? [];
        }
        if (select.unionMemberTypes === true) {
          row.unionMemberTypes = (raw.unionMemberTypes ?? []).map((id) =>
            makeProgramRef("type", sessionToken, projectId, snapshotId, id),
          );
        }
        if (select.properties === true) {
          row.properties = (raw.properties ?? []).map((prop) => ({
            name: prop.name,
            optional: prop.optional,
            declaration:
              prop.declaration !== null
                ? makeProgramRef(
                    "declaration",
                    sessionToken,
                    projectId,
                    snapshotId,
                    prop.declaration,
                  )
                : null,
          }));
        }
        if (select.typeArguments === true) {
          row.typeArguments = (raw.typeArguments ?? []).map((id) =>
            makeProgramRef("type", sessionToken, projectId, snapshotId, id),
          );
        }
        return row as ProjectedTypeRow<S>;
      });
    },

    isAssignableTo: async (
      pairs: readonly AssignabilityPair[],
    ): Promise<readonly AssignabilityResult[]> => {
      assertOpen();
      if (pairs.length === 0) return [];
      const allRefs: TypeRef[] = [];
      for (const pair of pairs) {
        allRefs.push(pair.source, pair.target);
      }
      validateProgramRefs(allRefs, sessionToken, snapshotId, projectId);
      const items = await runStep<RawAssignabilityRow>({
        op: "isAssignableTo",
        pairs: pairs.map((pair) => ({
          source: pair.source.handleId,
          target: pair.target.handleId,
        })),
      });

      return items.map((raw, index) => {
        recordGaps(raw.gaps);
        return {
          source: pairs[index]!.source,
          target: pairs[index]!.target,
          assignable: raw.assignable,
          gaps: raw.gaps,
        };
      });
    },

    syntaxOf: async <
      const S extends SyntaxLookupSelection = Record<never, never>,
    >(
      targets: readonly (SyntaxRef | DeclarationRef | CallRef | SignatureRef)[],
      options?: { readonly select?: S },
    ): Promise<readonly ProjectedSyntaxLookupRow<S>[]> => {
      assertOpen();
      if (targets.length === 0) return [];
      validateProgramRefs(targets, sessionToken, snapshotId, projectId);
      const select = options?.select;
      const items = await runStep<RawSyntaxLookupRow>({
        op: "syntaxOf",
        targets: targets.map((t) => t.handleId),
        ...(select !== undefined ? { select } : {}),
      });

      return items.map((raw, index) => {
        recordGaps(raw.gaps);
        const row: Record<string, unknown> = {
          target: targets[index]!,
          syntax:
            raw.syntax !== null
              ? makeProgramRef(
                  "syntax",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.syntax,
                )
              : null,
          gaps: raw.gaps,
        };
        if (select?.kind === true) row.kind = raw.kind ?? null;
        if (select?.text === true) row.text = raw.text ?? null;
        if (select?.location === true) row.location = raw.location ?? null;
        if (select?.expression === true) {
          row.expression =
            raw.expression != null
              ? makeProgramRef(
                  "syntax",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.expression,
                )
              : null;
        }
        if (select?.arguments === true) {
          row.arguments = (raw.arguments ?? []).map((id) =>
            makeProgramRef("syntax", sessionToken, projectId, snapshotId, id),
          );
        }
        if (select?.name === true) {
          row.name =
            raw.name != null
              ? makeProgramRef(
                  "syntax",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.name,
                )
              : null;
        }
        if (select?.initializer === true) {
          row.initializer =
            raw.initializer != null
              ? makeProgramRef(
                  "syntax",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.initializer,
                )
              : null;
        }
        if (select?.body === true) {
          row.body =
            raw.body != null
              ? makeProgramRef(
                  "syntax",
                  sessionToken,
                  projectId,
                  snapshotId,
                  raw.body,
                )
              : null;
        }
        if (select?.parameters === true) {
          row.parameters = (raw.parameters ?? []).map((id) =>
            makeProgramRef("syntax", sessionToken, projectId, snapshotId, id),
          );
        }
        const syntaxEvidence: Evidence[] = [];
        if (select?.location === true && raw.location != null) {
          const kindPrefix =
            select.kind === true && raw.kind != null ? raw.kind : "Syntax node";
          const snippet =
            select.text === true && raw.text != null
              ? `: ${summarizeSyntaxSnippet(raw.text)}`
              : "";
          syntaxEvidence.push({
            kind: "source",
            message: `${kindPrefix}${snippet}`,
            at: raw.location,
          });
        }
        return registerProvenance(
          row as ProjectedSyntaxLookupRow<S>,
          syntaxEvidence,
        );
      });
    },
  };
}
