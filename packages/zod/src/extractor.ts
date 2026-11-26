import {
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type ProjectContext,
  type ProjectSummary,
  type SyntaxRef,
} from "tssae";

import {
  AstRangeStore,
  inspectCallCallee,
  readStaticPropertyName,
  resolveChild,
  sortStringKeys,
  unwrapExpression,
  type AstNode,
  type AstTree,
} from "./ast.ts";
import {
  buildValidatorFieldModifier,
  buildValidatorInputFields,
  buildValidatorOutputFields,
  type ExternalSchemaResolution,
  type ExternalSchemaResolver,
  type ValidatorFieldExtractor,
  type ValidatorFieldInfo,
  type ValidatorFieldModifierResult,
  type ValidatorFieldRequirement,
  type ValidatorInputFieldsResult,
  type ValidatorOutputFieldsResult,
  type ValidatorQueryOptions,
} from "./contracts.ts";

const PROVIDER_ID = "zod";

const VALIDATOR_CALL_METHODS = [
  "parse",
  "safeParse",
  "parseAsync",
  "safeParseAsync",
] as const;

const SCHEMA_COMBINATOR_METHODS = [
  "object",
  "strictObject",
  "looseObject",
  "record",
  "union",
  "discriminatedUnion",
  "intersection",
  "pipe",
  "extend",
  "merge",
  "pick",
  "omit",
  "partial",
  "required",
  "and",
  "transform",
  "refine",
  "superRefine",
  "check",
  "openapi",
  "describe",
  "meta",
  "brand",
  "readonly",
  "passthrough",
  "strict",
  "strip",
  "catchall",
] as const;

const ALL_SCHEMA_DISCOVERY_CALLEES = [
  ...SCHEMA_COMBINATOR_METHODS,
  "createSelectSchema",
] as const;

const ZOD_OBJECT_CONSTRUCTORS = new Set([
  "object",
  "strictObject",
  "looseObject",
]);

const TRANSPARENT_MODIFIER_METHODS = new Set([
  "transform",
  "refine",
  "superRefine",
  "check",
  "openapi",
  "describe",
  "meta",
  "brand",
  "readonly",
]);

const TRANSPARENT_SCHEMA_WRAPPERS = new Set([
  ...TRANSPARENT_MODIFIER_METHODS,
  "passthrough",
  "strict",
  "strip",
  "catchall",
]);

const KNOWN_ZOD_LEAF_CONSTRUCTORS = new Set([
  "string",
  "number",
  "bigint",
  "boolean",
  "date",
  "symbol",
  "undefined",
  "void",
  "never",
  "null",
  "nan",
  "literal",
  "enum",
  "nativeEnum",
  "array",
  "tuple",
  "set",
  "map",
  "object",
  "strictObject",
  "looseObject",
  "record",
  "union",
  "discriminatedUnion",
  "intersection",
  "pipe",
  "function",
  "promise",
  "lazy",
  "custom",
  "instanceof",
  "file",
  "email",
  "url",
  "uuid",
  "cuid",
  "cuid2",
  "ulid",
  "ipv4",
  "ipv6",
  "cidrv4",
  "cidrv6",
  "iso",
  "base64",
  "base64url",
  "jwt",
  "int",
  "float32",
  "float64",
  "stringbool",
  "templateLiteral",
  "json",
  "datetime",
  "time",
  "duration",
]);

type KnownFieldSide = {
  readonly resolved: true;
  readonly requirement: ValidatorFieldRequirement;
  readonly nullable: boolean;
  readonly fromDefault: boolean;
  readonly explicitNullability: boolean;
  readonly dynamicGaps: readonly Gap[];
  readonly evidence: readonly Evidence[];
};

type UnknownFieldSide = {
  readonly resolved: false;
  readonly gap: Gap;
  readonly evidence: readonly Evidence[];
};

type FieldSide = KnownFieldSide | UnknownFieldSide;

type SchemaFieldEntry = {
  readonly name: string;
  readonly input: FieldSide;
  readonly output: FieldSide;
};

type SchemaSideState = {
  readonly fields: ReadonlyMap<string, FieldSide>;
  readonly schemaGaps: readonly Gap[];
  readonly hasDynamicFields: boolean;
  readonly evidence: readonly Evidence[];
};

type SchemaEvaluation = {
  readonly recognized: boolean;
  readonly schemaName?: string;
  readonly schemaSyntax?: SyntaxRef;
  readonly input: SchemaSideState;
  readonly output: SchemaSideState;
};

type ResolvedDeclarationInfo = {
  readonly name: string | null;
  readonly kind: string | null;
  readonly location: DiagnosticLocation | null;
  readonly syntax: SyntaxRef | null;
  readonly isExternal: boolean;
  readonly packageName: string | null;
  readonly exportName: string | null;
};

type CachedProjectState = {
  readonly inputs: readonly ValidatorInputFieldsResult[];
  readonly outputs: readonly ValidatorOutputFieldsResult[];
  readonly byTargetHandle: ReadonlyMap<
    string,
    {
      readonly input: ValidatorInputFieldsResult;
      readonly output: ValidatorOutputFieldsResult;
    }
  >;
};

const projectCache = new WeakMap<
  ProjectSummary,
  Map<string, CachedProjectState>
>();

const UNKNOWN_UNRESOLVED_SIDE = (
  gap: Gap,
  evidence: readonly Evidence[],
): UnknownFieldSide => ({
  resolved: false,
  gap,
  evidence,
});

function resolverCacheKey(
  resolvers: readonly ExternalSchemaResolver[] | undefined,
): string {
  if (resolvers === undefined || resolvers.length === 0) {
    return "none";
  }
  return resolvers.map((r) => r.id).join(",");
}

function emptySideState(): SchemaSideState {
  return {
    fields: new Map(),
    schemaGaps: [],
    hasDynamicFields: false,
    evidence: [],
  };
}

function unresolvedSchemaSide(
  gap: Gap,
  evidence: readonly Evidence[],
): SchemaSideState {
  return {
    fields: new Map(),
    schemaGaps: [gap],
    hasDynamicFields: true,
    evidence,
  };
}

function emptyEvaluation(
  recognized: boolean,
  schemaSyntax?: SyntaxRef,
  schemaName?: string,
): SchemaEvaluation {
  return {
    recognized,
    ...(schemaSyntax !== undefined ? { schemaSyntax } : {}),
    ...(schemaName !== undefined ? { schemaName } : {}),
    input: emptySideState(),
    output: emptySideState(),
  };
}

function isZodRootReceiver(
  tree: AstTree,
  receiverNode: AstNode | undefined,
): boolean {
  const unwrapped = unwrapExpression(tree, receiverNode);
  if (unwrapped === undefined) {
    return false;
  }
  if (unwrapped.kind === "Identifier") {
    const name = unwrapped.text.trim();
    return name === "z" || name === "zod";
  }
  if (unwrapped.kind === "PropertyAccessExpression") {
    const base =
      resolveChild(tree, unwrapped.expression) ?? unwrapped.children[0];
    const prop = resolveChild(tree, unwrapped.name) ?? unwrapped.children[1];
    const propName = prop?.text.trim();
    return (
      (propName === "coerce" || propName === "iso" || propName === "z") &&
      isZodRootReceiver(tree, base)
    );
  }
  return false;
}

function isZodTypeText(typeText: string): boolean {
  return (
    typeText.includes("Zod") ||
    typeText.includes("$Zod") ||
    typeText.startsWith("z.")
  );
}

function mergeSideEvidence(
  fields: ReadonlyMap<string, FieldSide>,
  extra: readonly Evidence[],
): readonly Evidence[] {
  const out: Evidence[] = [...extra];
  for (const field of fields.values()) {
    out.push(...field.evidence);
  }
  return out;
}

function collectSideGaps(side: SchemaSideState): readonly Gap[] {
  const out: Gap[] = [...side.schemaGaps];
  for (const field of side.fields.values()) {
    if (!field.resolved) {
      out.push(field.gap);
    } else {
      out.push(...field.dynamicGaps);
    }
  }
  return out;
}

function computeSideDynamic(
  fields: ReadonlyMap<string, FieldSide>,
  schemaGaps: readonly Gap[],
  baseDynamic: boolean,
): boolean {
  if (baseDynamic || schemaGaps.length > 0) {
    return true;
  }
  for (const field of fields.values()) {
    if (!field.resolved || field.dynamicGaps.length > 0) {
      return true;
    }
  }
  return false;
}

function convertSideFieldsToPublic(
  side: SchemaSideState,
): Readonly<Record<string, ValidatorFieldInfo>> {
  const out: Record<string, ValidatorFieldInfo> = {};
  for (const [name, field] of side.fields) {
    if (field.resolved) {
      out[name] = {
        name,
        requirement: field.requirement,
        nullable: field.nullable,
        ...(field.evidence.length > 0 ? { evidence: field.evidence } : {}),
      };
    }
  }
  return out;
}

function intersectKnownFieldSides(
  a: KnownFieldSide,
  b: KnownFieldSide,
  isOutput: boolean,
): KnownFieldSide {
  let requirement: ValidatorFieldRequirement;
  if (isOutput) {
    if (a.requirement === "required" || b.requirement === "required") {
      requirement = "required";
    } else if (a.requirement === "defaulted" || b.requirement === "defaulted") {
      requirement = "defaulted";
    } else {
      requirement = "optional";
    }
  } else {
    if (a.requirement === "required" || b.requirement === "required") {
      requirement = "required";
    } else if (a.requirement === "defaulted" && b.requirement === "defaulted") {
      requirement = "defaulted";
    } else {
      requirement = "optional";
    }
  }
  return {
    resolved: true,
    requirement,
    nullable: a.nullable && b.nullable,
    fromDefault: a.fromDefault || b.fromDefault,
    explicitNullability: a.explicitNullability || b.explicitNullability,
    dynamicGaps: [...a.dynamicGaps, ...b.dynamicGaps],
    evidence: [...a.evidence, ...b.evidence],
  };
}

function intersectFieldSideWithUnknown(
  known: KnownFieldSide,
  unknown: UnknownFieldSide,
  isOutput: boolean,
): FieldSide {
  const pinnedPresence = isOutput
    ? known.requirement === "required" || known.requirement === "defaulted"
    : known.requirement === "required";
  if (!known.nullable && pinnedPresence) {
    return {
      ...known,
      evidence: [...unknown.evidence, ...known.evidence],
    };
  }
  return unknown;
}

function intersectSchemaSide(
  left: SchemaSideState,
  right: SchemaSideState,
  isOutput: boolean,
  extraEvidence: readonly Evidence[],
): SchemaSideState {
  const fields = new Map<string, FieldSide>(left.fields);
  for (const [name, rightField] of right.fields) {
    const leftField = fields.get(name);
    if (leftField === undefined) {
      fields.set(name, rightField);
      continue;
    }
    if (leftField.resolved && rightField.resolved) {
      fields.set(
        name,
        intersectKnownFieldSides(leftField, rightField, isOutput),
      );
    } else if (!leftField.resolved && rightField.resolved) {
      fields.set(
        name,
        intersectFieldSideWithUnknown(rightField, leftField, isOutput),
      );
    } else if (leftField.resolved && !rightField.resolved) {
      fields.set(
        name,
        intersectFieldSideWithUnknown(leftField, rightField, isOutput),
      );
    } else {
      fields.set(name, rightField);
    }
  }
  const schemaGaps = [...left.schemaGaps, ...right.schemaGaps];
  return {
    fields,
    schemaGaps,
    hasDynamicFields: computeSideDynamic(fields, schemaGaps, false),
    evidence: [...left.evidence, ...right.evidence, ...extraEvidence],
  };
}

function extendSchemaSide(
  base: SchemaSideState,
  override: SchemaSideState,
  extraEvidence: readonly Evidence[],
): SchemaSideState {
  const fields = new Map<string, FieldSide>(base.fields);
  for (const [name, field] of override.fields) {
    fields.set(name, field);
  }
  const schemaGaps = [...base.schemaGaps, ...override.schemaGaps];
  return {
    fields,
    schemaGaps,
    hasDynamicFields: computeSideDynamic(fields, schemaGaps, false),
    evidence: [...base.evidence, ...override.evidence, ...extraEvidence],
  };
}

class ZodProjectEvaluator {
  readonly #ctx: ProjectContext;
  readonly #options: ValidatorQueryOptions | undefined;
  readonly #ast: AstRangeStore;
  readonly #declCache = new Map<
    string,
    Promise<ResolvedDeclarationInfo | null>
  >();
  readonly #schemaEvalCache = new Map<string, Promise<SchemaEvaluation>>();
  readonly #objectShapeCache = new Map<
    string,
    Promise<SchemaEvaluation | undefined>
  >();
  readonly #typeTextCache = new Map<string, Promise<string>>();

  constructor(ctx: ProjectContext, options?: ValidatorQueryOptions) {
    this.#ctx = ctx;
    this.#options = options;
    this.#ast = new AstRangeStore(ctx);
  }

  async resolveDeclaration(
    ref: SyntaxRef,
  ): Promise<ResolvedDeclarationInfo | null> {
    const existing = this.#declCache.get(ref.handleId);
    if (existing !== undefined) {
      return existing;
    }
    const pending = this.#ctx
      .resolveDeclarations([ref], {
        select: {
          name: true,
          kind: true,
          location: true,
          syntax: true,
          isExternal: true,
          packageName: true,
          exportName: true,
        },
      })
      .then((rows) => {
        const row = rows[0];
        if (row === undefined || row.declaration === null) {
          return null;
        }
        return {
          name: row.name,
          kind: row.kind,
          location: row.location,
          syntax: row.syntax,
          isExternal: row.isExternal,
          packageName: row.packageName,
          exportName: row.exportName,
        };
      });
    this.#declCache.set(ref.handleId, pending);
    return pending;
  }

  async getTypeText(ref: SyntaxRef): Promise<string> {
    const existing = this.#typeTextCache.get(ref.handleId);
    if (existing !== undefined) {
      return existing;
    }
    const pending = this.#ctx
      .typeAt([ref])
      .then((rows) => rows[0]?.typeText ?? "");
    this.#typeTextCache.set(ref.handleId, pending);
    return pending;
  }

  async evaluateFieldSideFromExternal(
    res: ExternalSchemaResolution,
    at: DiagnosticLocation,
  ): Promise<{
    readonly input: SchemaSideState;
    readonly output: SchemaSideState;
  }> {
    const inputFields = new Map<string, FieldSide>();
    const outputFields = new Map<string, FieldSide>();
    const evidence = res.evidence ?? [
      { kind: "source", message: "Resolved external schema", at },
    ];
    for (const [name, info] of Object.entries(res.fields)) {
      const side: KnownFieldSide = {
        resolved: true,
        requirement: info.requirement,
        nullable: info.nullable,
        fromDefault: info.requirement === "defaulted",
        explicitNullability: info.nullable,
        dynamicGaps: [],
        evidence: info.evidence ?? [],
      };
      inputFields.set(name, side);
      outputFields.set(name, side);
    }
    if (res.unresolvedFields !== undefined) {
      for (const [name, gaps] of Object.entries(res.unresolvedFields)) {
        const gap = gaps[0];
        if (gap === undefined) {
          continue;
        }
        const unknown = UNKNOWN_UNRESOLVED_SIDE(gap, []);
        inputFields.set(name, unknown);
        outputFields.set(name, unknown);
      }
    }
    const schemaGaps = res.gaps ?? [];
    const hasDynamic =
      Boolean(res.hasDynamicFields) ||
      computeSideDynamic(inputFields, schemaGaps, false);
    const sideState: SchemaSideState = {
      fields: inputFields,
      schemaGaps,
      hasDynamicFields: hasDynamic,
      evidence,
    };
    return {
      input: sideState,
      output: {
        ...sideState,
        fields: outputFields,
      },
    };
  }

  async resolveNodeToInitializer(
    ref: SyntaxRef,
    visited: ReadonlySet<string>,
  ): Promise<
    | {
        readonly decl: ResolvedDeclarationInfo;
        readonly tree: AstTree;
        readonly initNode: AstNode;
      }
    | undefined
  > {
    const decl = await this.resolveDeclaration(ref);
    if (
      decl === null ||
      decl.location === null ||
      decl.syntax === null ||
      visited.has(decl.syntax.handleId)
    ) {
      return undefined;
    }
    const loaded = await this.#ast.getNode(decl.syntax, decl.location);
    if (loaded === undefined) {
      return undefined;
    }
    const { tree, node } = loaded;
    if (
      node.kind === "VariableDeclaration" ||
      node.kind === "PropertyAssignment"
    ) {
      const initNode = unwrapExpression(
        tree,
        resolveChild(tree, node.initializer),
      );
      if (initNode !== undefined) {
        return { decl, tree, initNode };
      }
    }
    if (node.kind === "ExportAssignment") {
      const exprNode = unwrapExpression(
        tree,
        resolveChild(tree, node.expression),
      );
      if (exprNode !== undefined) {
        return { decl, tree, initNode: exprNode };
      }
    }
    return undefined;
  }

  async evaluateObjectLiteralShape(
    tree: AstTree,
    objNode: AstNode,
    visited: ReadonlySet<string>,
  ): Promise<SchemaEvaluation> {
    const inputFields = new Map<string, FieldSide>();
    const outputFields = new Map<string, FieldSide>();
    const schemaGaps: Gap[] = [];
    const evidence: Evidence[] = [
      {
        kind: "source",
        message: "Zod object shape definition",
        at: objNode.location,
      },
    ];
    let hasDynamicFields = false;

    for (const child of objNode.children) {
      if (child.kind === "PropertyAssignment") {
        const nameNode = resolveChild(tree, child.name) ?? child.children[0];
        const propName = readStaticPropertyName(nameNode);
        if (propName === undefined) {
          hasDynamicFields = true;
          schemaGaps.push({
            kind: "dynamic-schema-field",
            message:
              "Computed schema property name cannot be statically resolved",
            at: child.location,
          });
          continue;
        }
        const initNode =
          unwrapExpression(tree, resolveChild(tree, child.initializer)) ??
          unwrapExpression(tree, child.children[1]);
        if (initNode === undefined) {
          hasDynamicFields = true;
          const gap: Gap = {
            kind: "unresolved-schema-field",
            message: `Missing initializer for schema field "${propName}"`,
            at: child.location,
          };
          const unknown = UNKNOWN_UNRESOLVED_SIDE(gap, []);
          inputFields.set(propName, unknown);
          outputFields.set(propName, unknown);
          continue;
        }
        const fieldRes = await this.evaluateFieldExpression(
          tree,
          initNode,
          propName,
          child.location,
          visited,
        );
        inputFields.set(propName, fieldRes.input);
        outputFields.set(propName, fieldRes.output);
      } else if (child.kind === "ShorthandPropertyAssignment") {
        const nameNode = resolveChild(tree, child.name) ?? child.children[0];
        const propName = readStaticPropertyName(nameNode);
        if (propName === undefined || nameNode === undefined) {
          hasDynamicFields = true;
          schemaGaps.push({
            kind: "dynamic-schema-field",
            message: "Dynamic shorthand property in schema shape",
            at: child.location,
          });
          continue;
        }
        const fieldRes = await this.evaluateFieldExpression(
          tree,
          nameNode,
          propName,
          child.location,
          visited,
        );
        inputFields.set(propName, fieldRes.input);
        outputFields.set(propName, fieldRes.output);
      } else if (child.kind === "SpreadAssignment") {
        const spreadTarget = unwrapExpression(
          tree,
          resolveChild(tree, child.expression) ?? child.children[0],
        );
        if (spreadTarget !== undefined) {
          const resolvedSpread = await this.tryEvaluatePlainObjectShape(
            tree,
            spreadTarget,
            visited,
          );
          if (resolvedSpread !== undefined) {
            for (const [k, v] of resolvedSpread.input.fields) {
              inputFields.set(k, v);
            }
            for (const [k, v] of resolvedSpread.output.fields) {
              outputFields.set(k, v);
            }
            schemaGaps.push(...resolvedSpread.input.schemaGaps);
            evidence.push(...resolvedSpread.input.evidence);
            if (resolvedSpread.input.hasDynamicFields) {
              hasDynamicFields = true;
            }
            continue;
          }
        }
        hasDynamicFields = true;
        schemaGaps.push({
          kind: "dynamic-schema-spread",
          message: `Dynamic spread in schema shape: ${child.text.trim()}`,
          at: child.location,
        });
      }
    }

    const input: SchemaSideState = {
      fields: inputFields,
      schemaGaps,
      hasDynamicFields: computeSideDynamic(
        inputFields,
        schemaGaps,
        hasDynamicFields,
      ),
      evidence,
    };
    const output: SchemaSideState = {
      fields: outputFields,
      schemaGaps,
      hasDynamicFields: computeSideDynamic(
        outputFields,
        schemaGaps,
        hasDynamicFields,
      ),
      evidence,
    };
    return { recognized: true, schemaSyntax: objNode.ref, input, output };
  }

  async tryEvaluatePlainObjectShape(
    tree: AstTree,
    node: AstNode,
    visited: ReadonlySet<string>,
  ): Promise<SchemaEvaluation | undefined> {
    const unwrapped = unwrapExpression(tree, node);
    if (unwrapped === undefined) {
      return undefined;
    }
    if (unwrapped.kind === "ObjectLiteralExpression") {
      return this.evaluateObjectLiteralShape(tree, unwrapped, visited);
    }
    if (
      unwrapped.kind === "Identifier" ||
      unwrapped.kind === "PropertyAccessExpression"
    ) {
      const cacheKey = unwrapped.ref.handleId;
      const cached = this.#objectShapeCache.get(cacheKey);
      if (cached !== undefined) {
        return cached;
      }
      const pending = (async (): Promise<SchemaEvaluation | undefined> => {
        const resolved = await this.resolveNodeToInitializer(
          unwrapped.ref,
          visited,
        );
        if (resolved === undefined) {
          return undefined;
        }
        const nextVisited = new Set(visited);
        if (resolved.decl.syntax !== null) {
          nextVisited.add(resolved.decl.syntax.handleId);
        }
        if (resolved.initNode.kind === "ObjectLiteralExpression") {
          return this.evaluateObjectLiteralShape(
            resolved.tree,
            resolved.initNode,
            nextVisited,
          );
        }
        if (resolved.initNode.kind === "CallExpression") {
          const { methodName, receiverNode } = inspectCallCallee(
            resolved.tree,
            resolved.initNode,
          );
          if (
            methodName !== undefined &&
            ZOD_OBJECT_CONSTRUCTORS.has(methodName) &&
            isZodRootReceiver(resolved.tree, receiverNode)
          ) {
            const evalRes = await this.evaluateSchemaNode(
              resolved.tree,
              resolved.initNode,
              nextVisited,
            );
            return evalRes.recognized ? evalRes : undefined;
          }
        }
        return undefined;
      })();
      this.#objectShapeCache.set(cacheKey, pending);
      return pending;
    }
    if (unwrapped.kind === "PropertyAccessExpression") {
      // e.g., schema.shape
      const prop = resolveChild(tree, unwrapped.name) ?? unwrapped.children[1];
      if (prop?.text.trim() === "shape") {
        const recv =
          resolveChild(tree, unwrapped.expression) ?? unwrapped.children[0];
        if (recv !== undefined) {
          const schemaEval = await this.evaluateSchemaNode(tree, recv, visited);
          if (schemaEval.recognized) {
            return schemaEval;
          }
        }
      }
    }
    return undefined;
  }

  async evaluateFieldExpression(
    tree: AstTree,
    exprNode: AstNode,
    fieldName: string,
    fieldLocation: DiagnosticLocation,
    visited: ReadonlySet<string>,
  ): Promise<SchemaFieldEntry> {
    const unwrapped = unwrapExpression(tree, exprNode);
    if (unwrapped === undefined) {
      const gap: Gap = {
        kind: "unresolved-schema-field",
        message: `Unresolved field validator for "${fieldName}"`,
        at: fieldLocation,
      };
      const unknown = UNKNOWN_UNRESOLVED_SIDE(gap, []);
      return { name: fieldName, input: unknown, output: unknown };
    }

    const fieldEvidence: Evidence[] = [
      {
        kind: "source",
        message: `Field "${fieldName}" schema definition`,
        at: fieldLocation,
      },
    ];

    if (unwrapped.kind === "CallExpression") {
      const { methodName, receiverNode } = inspectCallCallee(tree, unwrapped);
      const args = (unwrapped.arguments ?? [])
        .map((r) => resolveChild(tree, r))
        .filter((n): n is AstNode => n !== undefined);

      // Modifiers chained onto an existing field schema
      if (methodName === "optional" || methodName === "exactOptional") {
        const inner =
          receiverNode !== undefined
            ? await this.evaluateFieldExpression(
                tree,
                receiverNode,
                fieldName,
                fieldLocation,
                visited,
              )
            : undefined;
        const applyOpt = (side: FieldSide | undefined): KnownFieldSide =>
          side !== undefined && side.resolved
            ? { ...side, requirement: "optional", evidence: fieldEvidence }
            : {
                resolved: true,
                requirement: "optional",
                nullable: false,
                fromDefault: false,
                explicitNullability: false,
                dynamicGaps: [],
                evidence: fieldEvidence,
              };
        return {
          name: fieldName,
          input: applyOpt(inner?.input),
          output: applyOpt(inner?.output),
        };
      }

      if (methodName === "nullable") {
        const inner =
          receiverNode !== undefined
            ? await this.evaluateFieldExpression(
                tree,
                receiverNode,
                fieldName,
                fieldLocation,
                visited,
              )
            : undefined;
        const applyNullable = (side: FieldSide | undefined): FieldSide => {
          if (side !== undefined && side.resolved) {
            return {
              ...side,
              nullable: true,
              explicitNullability: true,
              evidence: fieldEvidence,
            };
          }
          return (
            side ??
            UNKNOWN_UNRESOLVED_SIDE(
              {
                kind: "unresolved-schema-field",
                message: `Unresolved field validator for "${fieldName}"`,
                at: unwrapped.location,
              },
              fieldEvidence,
            )
          );
        };
        return {
          name: fieldName,
          input: applyNullable(inner?.input),
          output: applyNullable(inner?.output),
        };
      }

      if (methodName === "nullish") {
        const inner =
          receiverNode !== undefined
            ? await this.evaluateFieldExpression(
                tree,
                receiverNode,
                fieldName,
                fieldLocation,
                visited,
              )
            : undefined;
        const applyNullish = (side: FieldSide | undefined): KnownFieldSide =>
          side !== undefined && side.resolved
            ? {
                ...side,
                requirement: "optional",
                nullable: true,
                explicitNullability: true,
                evidence: fieldEvidence,
              }
            : {
                resolved: true,
                requirement: "optional",
                nullable: true,
                fromDefault: false,
                explicitNullability: true,
                dynamicGaps: [],
                evidence: fieldEvidence,
              };
        return {
          name: fieldName,
          input: applyNullish(inner?.input),
          output: applyNullish(inner?.output),
        };
      }

      if (
        methodName === "default" ||
        methodName === "prefault" ||
        methodName === "catch"
      ) {
        const inner =
          receiverNode !== undefined
            ? await this.evaluateFieldExpression(
                tree,
                receiverNode,
                fieldName,
                fieldLocation,
                visited,
              )
            : undefined;
        const applyDefault = (side: FieldSide | undefined): KnownFieldSide =>
          side !== undefined && side.resolved
            ? {
                ...side,
                requirement: "defaulted",
                fromDefault: true,
                evidence: fieldEvidence,
              }
            : {
                resolved: true,
                requirement: "defaulted",
                nullable: false,
                fromDefault: true,
                explicitNullability: false,
                dynamicGaps: [],
                evidence: fieldEvidence,
              };
        return {
          name: fieldName,
          input: applyDefault(inner?.input),
          output: applyDefault(inner?.output),
        };
      }

      if (methodName === "nonoptional") {
        const inner =
          receiverNode !== undefined
            ? await this.evaluateFieldExpression(
                tree,
                receiverNode,
                fieldName,
                fieldLocation,
                visited,
              )
            : undefined;
        const applyRequired = (side: FieldSide | undefined): FieldSide => {
          if (side !== undefined && side.resolved) {
            return {
              ...side,
              requirement: "required",
              evidence: fieldEvidence,
            };
          }
          return {
            resolved: true,
            requirement: "required",
            nullable: false,
            fromDefault: false,
            explicitNullability: false,
            dynamicGaps: [],
            evidence: fieldEvidence,
          };
        };
        return {
          name: fieldName,
          input: applyRequired(inner?.input),
          output: applyRequired(inner?.output),
        };
      }

      if (
        methodName !== undefined &&
        TRANSPARENT_MODIFIER_METHODS.has(methodName)
      ) {
        if (receiverNode !== undefined) {
          const inner = await this.evaluateFieldExpression(
            tree,
            receiverNode,
            fieldName,
            fieldLocation,
            visited,
          );
          return {
            name: fieldName,
            input: { ...inner.input, evidence: fieldEvidence },
            output: { ...inner.output, evidence: fieldEvidence },
          };
        }
      }

      if (
        methodName === "pipe" &&
        receiverNode !== undefined &&
        args[0] !== undefined
      ) {
        const inputSide = await this.evaluateFieldExpression(
          tree,
          receiverNode,
          fieldName,
          fieldLocation,
          visited,
        );
        const outputSide = await this.evaluateFieldExpression(
          tree,
          args[0],
          fieldName,
          fieldLocation,
          visited,
        );
        return {
          name: fieldName,
          input: { ...inputSide.input, evidence: fieldEvidence },
          output: { ...outputSide.output, evidence: fieldEvidence },
        };
      }

      if (
        (methodName === "any" || methodName === "unknown") &&
        isZodRootReceiver(tree, receiverNode)
      ) {
        const dynamicSide: KnownFieldSide = {
          resolved: true,
          requirement: "required",
          nullable: false,
          fromDefault: false,
          explicitNullability: false,
          dynamicGaps: [
            {
              kind: "dynamic-schema-field",
              message: `Schema field "${fieldName}" uses dynamic z.${methodName}()`,
              at: fieldLocation,
            },
          ],
          evidence: fieldEvidence,
        };
        return { name: fieldName, input: dynamicSide, output: dynamicSide };
      }

      if (
        methodName === "preprocess" &&
        isZodRootReceiver(tree, receiverNode)
      ) {
        const schemaArg = args[1];
        if (schemaArg !== undefined) {
          return this.evaluateFieldExpression(
            tree,
            schemaArg,
            fieldName,
            fieldLocation,
            visited,
          );
        }
      }

      if (
        methodName !== undefined &&
        KNOWN_ZOD_LEAF_CONSTRUCTORS.has(methodName) &&
        isZodRootReceiver(tree, receiverNode)
      ) {
        const knownLeaf: KnownFieldSide = {
          resolved: true,
          requirement: "required",
          nullable: false,
          fromDefault: false,
          explicitNullability: false,
          dynamicGaps: [],
          evidence: fieldEvidence,
        };
        return { name: fieldName, input: knownLeaf, output: knownLeaf };
      }

      // A composed nested object schema expression (e.g. subSchema.extend(...)) used as a field value
      if (
        methodName !== undefined &&
        SCHEMA_COMBINATOR_METHODS.includes(
          methodName as (typeof SCHEMA_COMBINATOR_METHODS)[number],
        ) &&
        receiverNode !== undefined
      ) {
        const nestedEval = await this.evaluateSchemaNode(
          tree,
          unwrapped,
          visited,
        );
        if (nestedEval.recognized) {
          const leaf: KnownFieldSide = {
            resolved: true,
            requirement: "required",
            nullable: false,
            fromDefault: false,
            explicitNullability: false,
            dynamicGaps: [],
            evidence: fieldEvidence,
          };
          return { name: fieldName, input: leaf, output: leaf };
        }
      }
    }

    // Identifier / PropertyAccess referencing another named field schema variable
    if (
      unwrapped.kind === "Identifier" ||
      unwrapped.kind === "PropertyAccessExpression"
    ) {
      const resolved = await this.resolveNodeToInitializer(
        unwrapped.ref,
        visited,
      );
      if (resolved !== undefined) {
        const nextVisited = new Set(visited);
        if (resolved.decl.syntax !== null) {
          nextVisited.add(resolved.decl.syntax.handleId);
        }
        return this.evaluateFieldExpression(
          resolved.tree,
          resolved.initNode,
          fieldName,
          fieldLocation,
          nextVisited,
        );
      }
    }

    const gap: Gap = {
      kind: "unresolved-schema-field",
      message: `Unresolved field validator for "${fieldName}": ${unwrapped.text.trim()}`,
      at: fieldLocation,
    };
    const unknown = UNKNOWN_UNRESOLVED_SIDE(gap, fieldEvidence);
    return { name: fieldName, input: unknown, output: unknown };
  }

  async evaluateMaskKeys(
    tree: AstTree,
    maskNode: AstNode | undefined,
  ): Promise<
    | { readonly kind: "keys"; readonly keys: ReadonlySet<string> }
    | { readonly kind: "dynamic"; readonly gap: Gap }
  > {
    const unwrapped = unwrapExpression(tree, maskNode);
    if (
      unwrapped === undefined ||
      unwrapped.kind !== "ObjectLiteralExpression"
    ) {
      return {
        kind: "dynamic",
        gap: {
          kind: "dynamic-schema-mask",
          message: "Dynamic mask argument in schema combinator",
          ...(unwrapped !== undefined ? { at: unwrapped.location } : {}),
        },
      };
    }

    const keys = new Set<string>();
    for (const child of unwrapped.children) {
      if (child.kind !== "PropertyAssignment") {
        return {
          kind: "dynamic",
          gap: {
            kind: "dynamic-schema-mask",
            message: `Unsupported mask entry: ${child.text.trim()}`,
            at: child.location,
          },
        };
      }
      const key = readStaticPropertyName(
        resolveChild(tree, child.name) ?? child.children[0],
      );
      const valNode = unwrapExpression(
        tree,
        resolveChild(tree, child.initializer) ?? child.children[1],
      );
      if (key === undefined || valNode === undefined) {
        return {
          kind: "dynamic",
          gap: {
            kind: "dynamic-schema-mask",
            message: "Computed mask key in schema combinator",
            at: child.location,
          },
        };
      }
      if (valNode.kind === "TrueKeyword" || valNode.text.trim() === "true") {
        keys.add(key);
      } else if (
        valNode.kind !== "FalseKeyword" &&
        valNode.text.trim() !== "false"
      ) {
        return {
          kind: "dynamic",
          gap: {
            kind: "dynamic-schema-mask",
            message: `Non-literal mask value for "${key}"`,
            at: child.location,
          },
        };
      }
    }
    return { kind: "keys", keys };
  }

  async evaluateSchemaNode(
    tree: AstTree,
    node: AstNode,
    visited: ReadonlySet<string> = new Set(),
  ): Promise<SchemaEvaluation> {
    const unwrapped = unwrapExpression(tree, node);
    if (unwrapped === undefined) {
      return emptyEvaluation(false);
    }

    const cacheKey = unwrapped.ref.handleId;
    if (visited.size === 0) {
      const cached = this.#schemaEvalCache.get(cacheKey);
      if (cached !== undefined) {
        return cached;
      }
    }

    const compute = async (): Promise<SchemaEvaluation> => {
      if (
        unwrapped.kind === "Identifier" ||
        unwrapped.kind === "PropertyAccessExpression"
      ) {
        if (visited.has(unwrapped.ref.handleId)) {
          return emptyEvaluation(false);
        }
        const resolved = await this.resolveNodeToInitializer(
          unwrapped.ref,
          visited,
        );
        if (resolved !== undefined) {
          const nextVisited = new Set(visited);
          nextVisited.add(unwrapped.ref.handleId);
          if (resolved.decl.syntax !== null) {
            nextVisited.add(resolved.decl.syntax.handleId);
          }
          const evalRes = await this.evaluateSchemaNode(
            resolved.tree,
            resolved.initNode,
            nextVisited,
          );
          if (evalRes.recognized) {
            const schemaName =
              evalRes.schemaName ??
              resolved.decl.name ??
              (unwrapped.kind === "Identifier"
                ? unwrapped.text.trim()
                : undefined);
            return {
              ...evalRes,
              ...(schemaName !== undefined ? { schemaName } : {}),
            };
          }
        }
        const typeText = await this.getTypeText(unwrapped.ref);
        if (isZodTypeText(typeText)) {
          const gap: Gap = {
            kind: "unresolved-schema",
            message: `Unresolved external schema: ${unwrapped.text.trim()}`,
            at: unwrapped.location,
          };
          const ev: Evidence = {
            kind: "source",
            message: `Unresolved Zod schema reference "${unwrapped.text.trim()}"`,
            at: unwrapped.location,
          };
          const side = unresolvedSchemaSide(gap, [ev]);
          return {
            recognized: true,
            schemaSyntax: unwrapped.ref,
            ...(unwrapped.kind === "Identifier"
              ? { schemaName: unwrapped.text.trim() }
              : {}),
            input: side,
            output: side,
          };
        }
        return emptyEvaluation(false);
      }

      if (unwrapped.kind !== "CallExpression") {
        return emptyEvaluation(false);
      }

      const { calleeNode, methodName, receiverNode } = inspectCallCallee(
        tree,
        unwrapped,
      );
      const args = (unwrapped.arguments ?? [])
        .map((r) => resolveChild(tree, r))
        .filter((n): n is AstNode => n !== undefined);

      // 1. z.object / z.strictObject / z.looseObject
      if (
        methodName !== undefined &&
        ZOD_OBJECT_CONSTRUCTORS.has(methodName) &&
        isZodRootReceiver(tree, receiverNode)
      ) {
        const shapeArg = unwrapExpression(tree, args[0]);
        if (
          shapeArg === undefined ||
          shapeArg.kind !== "ObjectLiteralExpression"
        ) {
          const gap: Gap = {
            kind: "dynamic-schema-shape",
            message: `Dynamic object shape in z.${methodName}()`,
            at: unwrapped.location,
          };
          const ev: Evidence = {
            kind: "source",
            message: `z.${methodName}() call`,
            at: unwrapped.location,
          };
          const side = unresolvedSchemaSide(gap, [ev]);
          return {
            recognized: true,
            schemaSyntax: unwrapped.ref,
            input: side,
            output: side,
          };
        }
        const shapeEval = await this.evaluateObjectLiteralShape(
          tree,
          shapeArg,
          visited,
        );
        return {
          ...shapeEval,
          schemaSyntax: unwrapped.ref,
        };
      }

      // 2. z.record(...)
      if (methodName === "record" && isZodRootReceiver(tree, receiverNode)) {
        const gap: Gap = {
          kind: "dynamic-schema-shape",
          message: "z.record() defines dynamic key-value pairs",
          at: unwrapped.location,
        };
        const ev: Evidence = {
          kind: "source",
          message: "z.record() dynamic shape",
          at: unwrapped.location,
        };
        const side = unresolvedSchemaSide(gap, [ev]);
        return {
          recognized: true,
          schemaSyntax: unwrapped.ref,
          input: side,
          output: side,
        };
      }

      // 3. z.union / z.discriminatedUnion
      if (
        (methodName === "union" || methodName === "discriminatedUnion") &&
        isZodRootReceiver(tree, receiverNode)
      ) {
        const listArg =
          methodName === "union"
            ? unwrapExpression(tree, args[0])
            : unwrapExpression(tree, args[1]);
        if (listArg?.kind === "ArrayLiteralExpression") {
          const memberEvals = await Promise.all(
            listArg.children.map((el) =>
              this.evaluateSchemaNode(tree, el, visited),
            ),
          );
          if (
            memberEvals.length > 0 &&
            memberEvals.every((m) => m.recognized)
          ) {
            const combineUnion = (isOutput: boolean): SchemaSideState => {
              const allNames = new Set<string>();
              const schemaGaps: Gap[] = [];
              const evidence: Evidence[] = [
                {
                  kind: "source",
                  message: `z.${methodName}() union schema`,
                  at: unwrapped.location,
                },
              ];
              let hasDynamic = false;
              for (const m of memberEvals) {
                const s = isOutput ? m.output : m.input;
                for (const key of s.fields.keys()) {
                  allNames.add(key);
                }
                schemaGaps.push(...s.schemaGaps);
                evidence.push(...s.evidence);
                if (s.hasDynamicFields) {
                  hasDynamic = true;
                }
              }
              const fields = new Map<string, FieldSide>();
              for (const name of allNames) {
                const sides = memberEvals.map((m) =>
                  (isOutput ? m.output : m.input).fields.get(name),
                );
                if (sides.some((s) => s !== undefined && !s.resolved)) {
                  const unk = sides.find(
                    (s): s is UnknownFieldSide =>
                      s !== undefined && !s.resolved,
                  )!;
                  fields.set(name, unk);
                  continue;
                }
                const presentInAll = sides.every((s) => s !== undefined);
                const resolvedSides = sides.filter(
                  (s): s is KnownFieldSide => s !== undefined && s.resolved,
                );
                let requirement: ValidatorFieldRequirement = "optional";
                if (presentInAll) {
                  if (
                    resolvedSides.every((s) => s.requirement === "required")
                  ) {
                    requirement = "required";
                  } else if (
                    resolvedSides.every(
                      (s) =>
                        s.requirement === "defaulted" ||
                        (isOutput && s.requirement === "required"),
                    )
                  ) {
                    requirement = resolvedSides.some(
                      (s) => s.requirement === "defaulted",
                    )
                      ? "defaulted"
                      : "required";
                  }
                }
                fields.set(name, {
                  resolved: true,
                  requirement,
                  nullable: resolvedSides.some((s) => s.nullable),
                  fromDefault: resolvedSides.every((s) => s.fromDefault),
                  explicitNullability: resolvedSides.some(
                    (s) => s.explicitNullability,
                  ),
                  dynamicGaps: resolvedSides.flatMap((s) => s.dynamicGaps),
                  evidence: resolvedSides.flatMap((s) => s.evidence),
                });
              }
              return {
                fields,
                schemaGaps,
                hasDynamicFields: computeSideDynamic(
                  fields,
                  schemaGaps,
                  hasDynamic,
                ),
                evidence,
              };
            };
            return {
              recognized: true,
              schemaSyntax: unwrapped.ref,
              input: combineUnion(false),
              output: combineUnion(true),
            };
          }
        }
      }

      // 4. Static z.intersection(a, b) & z.pipe(a, b)
      if (
        methodName === "intersection" &&
        isZodRootReceiver(tree, receiverNode) &&
        args[0] !== undefined &&
        args[1] !== undefined
      ) {
        const [left, right] = await Promise.all([
          this.evaluateSchemaNode(tree, args[0], visited),
          this.evaluateSchemaNode(tree, args[1], visited),
        ]);
        if (left.recognized && right.recognized) {
          const ev: Evidence = {
            kind: "source",
            message: "z.intersection() combinator",
            at: unwrapped.location,
          };
          return {
            recognized: true,
            schemaSyntax: unwrapped.ref,
            input: intersectSchemaSide(left.input, right.input, false, [ev]),
            output: intersectSchemaSide(left.output, right.output, true, [ev]),
          };
        }
      }

      if (
        methodName === "pipe" &&
        isZodRootReceiver(tree, receiverNode) &&
        args[0] !== undefined &&
        args[1] !== undefined
      ) {
        return this.evaluatePipePair(
          tree,
          unwrapped,
          args[0],
          args[1],
          visited,
        );
      }

      // 5. Method combinators on a receiver schema
      if (receiverNode !== undefined && methodName !== undefined) {
        if (methodName === "extend" && args[0] !== undefined) {
          const base = await this.evaluateSchemaNode(
            tree,
            receiverNode,
            visited,
          );
          if (base.recognized) {
            const override = await this.evaluateExtensionShape(
              tree,
              args[0],
              visited,
            );
            const ev: Evidence = {
              kind: "source",
              message: "Schema .extend() composition",
              at: unwrapped.location,
            };
            return {
              recognized: true,
              schemaSyntax: unwrapped.ref,
              input: extendSchemaSide(base.input, override.input, [ev]),
              output: extendSchemaSide(base.output, override.output, [ev]),
            };
          }
        }

        if (methodName === "merge" && args[0] !== undefined) {
          const [base, other] = await Promise.all([
            this.evaluateSchemaNode(tree, receiverNode, visited),
            this.evaluateSchemaNode(tree, args[0], visited),
          ]);
          if (base.recognized) {
            const ev: Evidence = {
              kind: "source",
              message: "Schema .merge() composition",
              at: unwrapped.location,
            };
            const rhs = other.recognized
              ? other
              : {
                  input: unresolvedSchemaSide(
                    {
                      kind: "unresolved-schema",
                      message: `Unresolved .merge() schema argument: ${args[0].text.trim()}`,
                      at: args[0].location,
                    },
                    [ev],
                  ),
                  output: unresolvedSchemaSide(
                    {
                      kind: "unresolved-schema",
                      message: `Unresolved .merge() schema argument: ${args[0].text.trim()}`,
                      at: args[0].location,
                    },
                    [ev],
                  ),
                };
            return {
              recognized: true,
              schemaSyntax: unwrapped.ref,
              input: extendSchemaSide(base.input, rhs.input, [ev]),
              output: extendSchemaSide(base.output, rhs.output, [ev]),
            };
          }
        }

        if (
          (methodName === "pick" || methodName === "omit") &&
          args[0] !== undefined
        ) {
          const base = await this.evaluateSchemaNode(
            tree,
            receiverNode,
            visited,
          );
          if (base.recognized) {
            const mask = await this.evaluateMaskKeys(tree, args[0]);
            const ev: Evidence = {
              kind: "source",
              message: `Schema .${methodName}() filter`,
              at: unwrapped.location,
            };
            const applyFilter = (side: SchemaSideState): SchemaSideState => {
              if (mask.kind === "dynamic") {
                const schemaGaps = [...side.schemaGaps, mask.gap];
                return {
                  ...side,
                  schemaGaps,
                  hasDynamicFields: true,
                  evidence: [...side.evidence, ev],
                };
              }
              const fields = new Map<string, FieldSide>();
              for (const [k, v] of side.fields) {
                const keep =
                  methodName === "pick" ? mask.keys.has(k) : !mask.keys.has(k);
                if (keep) {
                  fields.set(k, v);
                }
              }
              return {
                fields,
                schemaGaps: side.schemaGaps,
                hasDynamicFields: computeSideDynamic(
                  fields,
                  side.schemaGaps,
                  side.schemaGaps.length > 0,
                ),
                evidence: [...side.evidence, ev],
              };
            };
            return {
              recognized: true,
              schemaSyntax: unwrapped.ref,
              input: applyFilter(base.input),
              output: applyFilter(base.output),
            };
          }
        }

        if (methodName === "partial" || methodName === "required") {
          const base = await this.evaluateSchemaNode(
            tree,
            receiverNode,
            visited,
          );
          if (base.recognized) {
            const mask =
              args[0] !== undefined
                ? await this.evaluateMaskKeys(tree, args[0])
                : undefined;
            const ev: Evidence = {
              kind: "source",
              message: `Schema .${methodName}() modifier`,
              at: unwrapped.location,
            };
            const targetRequirement: ValidatorFieldRequirement =
              methodName === "partial" ? "optional" : "required";
            const applyModifier = (side: SchemaSideState): SchemaSideState => {
              if (mask?.kind === "dynamic") {
                return {
                  ...side,
                  schemaGaps: [...side.schemaGaps, mask.gap],
                  hasDynamicFields: true,
                  evidence: [...side.evidence, ev],
                };
              }
              const fields = new Map<string, FieldSide>();
              for (const [k, v] of side.fields) {
                if (v.resolved && (mask === undefined || mask.keys.has(k))) {
                  fields.set(k, {
                    ...v,
                    requirement: targetRequirement,
                  });
                } else {
                  fields.set(k, v);
                }
              }
              return {
                fields,
                schemaGaps: side.schemaGaps,
                hasDynamicFields: computeSideDynamic(
                  fields,
                  side.schemaGaps,
                  false,
                ),
                evidence: [...side.evidence, ev],
              };
            };
            return {
              recognized: true,
              schemaSyntax: unwrapped.ref,
              input: applyModifier(base.input),
              output: applyModifier(base.output),
            };
          }
        }

        if (methodName === "and" && args[0] !== undefined) {
          const [base, other] = await Promise.all([
            this.evaluateSchemaNode(tree, receiverNode, visited),
            this.evaluateSchemaNode(tree, args[0], visited),
          ]);
          if (base.recognized) {
            const ev: Evidence = {
              kind: "source",
              message: "Schema .and() intersection",
              at: unwrapped.location,
            };
            const rhs = other.recognized
              ? other
              : {
                  input: unresolvedSchemaSide(
                    {
                      kind: "unresolved-schema",
                      message: `Unresolved .and() schema argument: ${args[0].text.trim()}`,
                      at: args[0].location,
                    },
                    [ev],
                  ),
                  output: unresolvedSchemaSide(
                    {
                      kind: "unresolved-schema",
                      message: `Unresolved .and() schema argument: ${args[0].text.trim()}`,
                      at: args[0].location,
                    },
                    [ev],
                  ),
                };
            return {
              recognized: true,
              schemaSyntax: unwrapped.ref,
              input: intersectSchemaSide(base.input, rhs.input, false, [ev]),
              output: intersectSchemaSide(base.output, rhs.output, true, [ev]),
            };
          }
        }

        if (methodName === "pipe" && args[0] !== undefined) {
          return this.evaluatePipePair(
            tree,
            unwrapped,
            receiverNode,
            args[0],
            visited,
          );
        }

        if (TRANSPARENT_SCHEMA_WRAPPERS.has(methodName)) {
          const base = await this.evaluateSchemaNode(
            tree,
            receiverNode,
            visited,
          );
          if (base.recognized) {
            const ev: Evidence = {
              kind: "source",
              message: `Schema .${methodName}() wrapper`,
              at: unwrapped.location,
            };
            return {
              ...base,
              schemaSyntax: unwrapped.ref,
              input: {
                ...base.input,
                evidence: [...base.input.evidence, ev],
              },
              output: {
                ...base.output,
                evidence: [...base.output.evidence, ev],
              },
            };
          }
        }
      }

      // 6. External schema resolvers (e.g. @tssae/drizzle's drizzleZodSchemaResolver)
      const externalResolvers = this.#options?.externalSchemaResolvers;
      if (externalResolvers !== undefined && externalResolvers.length > 0) {
        for (const resolver of externalResolvers) {
          const resolved = await resolver.resolveSchemaCall(
            this.#ctx,
            unwrapped.ref,
            this.#options,
          );
          if (resolved !== undefined) {
            const sides = await this.evaluateFieldSideFromExternal(
              resolved,
              unwrapped.location,
            );
            return {
              recognized: true,
              schemaSyntax: unwrapped.ref,
              input: sides.input,
              output: sides.output,
            };
          }
        }
      }

      // 7. Unresolved external schema call (e.g. createSelectSchema without drizzle resolver, or factory returning ZodType)
      const [decl, typeText] = await Promise.all([
        calleeNode !== undefined
          ? this.resolveDeclaration(calleeNode.ref)
          : Promise.resolve(null),
        this.getTypeText(unwrapped.ref),
      ]);
      const looksForeignZod =
        decl?.packageName === "drizzle-zod" || isZodTypeText(typeText);
      if (looksForeignZod) {
        const gap: Gap = {
          kind: "unresolved-schema",
          message: `Unresolved external schema: ${unwrapped.text.trim()}`,
          at: unwrapped.location,
        };
        const ev: Evidence = {
          kind: "source",
          message: `Unresolved schema expression: ${unwrapped.text.trim()}`,
          at: unwrapped.location,
        };
        const side = unresolvedSchemaSide(gap, [ev]);
        return {
          recognized: true,
          schemaSyntax: unwrapped.ref,
          input: side,
          output: side,
        };
      }

      return emptyEvaluation(false);
    };

    const promise = compute();
    if (visited.size === 0) {
      this.#schemaEvalCache.set(cacheKey, promise);
    }
    return promise;
  }

  async evaluateExtensionShape(
    tree: AstTree,
    shapeArg: AstNode,
    visited: ReadonlySet<string>,
  ): Promise<SchemaEvaluation> {
    const unwrapped = unwrapExpression(tree, shapeArg);
    if (unwrapped?.kind === "ObjectLiteralExpression") {
      return this.evaluateObjectLiteralShape(tree, unwrapped, visited);
    }
    const evaluated = await this.evaluateSchemaNode(tree, shapeArg, visited);
    if (evaluated.recognized) {
      return evaluated;
    }
    const gap: Gap = {
      kind: "dynamic-schema-shape",
      message: `Dynamic extension shape: ${shapeArg.text.trim()}`,
      at: shapeArg.location,
    };
    const side = unresolvedSchemaSide(gap, [
      {
        kind: "source",
        message: "Dynamic .extend() argument",
        at: shapeArg.location,
      },
    ]);
    return { recognized: true, input: side, output: side };
  }

  async evaluatePipePair(
    tree: AstTree,
    pipeCall: AstNode,
    baseNode: AstNode,
    targetNode: AstNode,
    visited: ReadonlySet<string>,
  ): Promise<SchemaEvaluation> {
    const [base, target] = await Promise.all([
      this.evaluateSchemaNode(tree, baseNode, visited),
      this.evaluateSchemaNode(tree, targetNode, visited),
    ]);
    if (!base.recognized) {
      return emptyEvaluation(false);
    }
    const ev: Evidence = {
      kind: "source",
      message: "Schema .pipe() composition",
      at: pipeCall.location,
    };
    const outputSide = target.recognized
      ? {
          ...target.output,
          evidence: [...base.output.evidence, ...target.output.evidence, ev],
        }
      : unresolvedSchemaSide(
          {
            kind: "unresolved-schema",
            message: `Unresolved .pipe() target schema: ${targetNode.text.trim()}`,
            at: targetNode.location,
          },
          [
            ...base.output.evidence,
            ev,
            {
              kind: "source",
              message: `Unrecognized .pipe() target at ${targetNode.location.file}:${String(targetNode.location.line)}`,
              at: targetNode.location,
            },
          ],
        );
    return {
      recognized: true,
      schemaSyntax: pipeCall.ref,
      input: {
        ...base.input,
        evidence: [...base.input.evidence, ev],
      },
      output: outputSide,
    };
  }
}

function buildPairResults(
  evalRes: SchemaEvaluation,
  options: {
    readonly siteKind: "call" | "schema";
    readonly validatorMethod?: string;
    readonly schemaName?: string;
    readonly target: SyntaxRef;
    readonly at: DiagnosticLocation;
    readonly extraEvidence: readonly Evidence[];
  },
): {
  readonly input: ValidatorInputFieldsResult;
  readonly output: ValidatorOutputFieldsResult;
} {
  const inputEvidence = mergeSideEvidence(evalRes.input.fields, [
    ...evalRes.input.evidence,
    ...options.extraEvidence,
  ]);
  const outputEvidence = mergeSideEvidence(evalRes.output.fields, [
    ...evalRes.output.evidence,
    ...options.extraEvidence,
  ]);
  const schemaName = options.schemaName ?? evalRes.schemaName;

  const input = buildValidatorInputFields({
    siteKind: options.siteKind,
    ...(options.validatorMethod !== undefined
      ? { validatorMethod: options.validatorMethod }
      : {}),
    ...(schemaName !== undefined ? { schemaName } : {}),
    target: options.target,
    ...(evalRes.schemaSyntax !== undefined
      ? { schemaSyntax: evalRes.schemaSyntax }
      : {}),
    at: options.at,
    providerId: PROVIDER_ID,
    fields: convertSideFieldsToPublic(evalRes.input),
    hasDynamicFields: evalRes.input.hasDynamicFields,
    evidence: inputEvidence,
    gaps: collectSideGaps(evalRes.input),
  });

  const output = buildValidatorOutputFields({
    siteKind: options.siteKind,
    ...(options.validatorMethod !== undefined
      ? { validatorMethod: options.validatorMethod }
      : {}),
    ...(schemaName !== undefined ? { schemaName } : {}),
    target: options.target,
    ...(evalRes.schemaSyntax !== undefined
      ? { schemaSyntax: evalRes.schemaSyntax }
      : {}),
    at: options.at,
    providerId: PROVIDER_ID,
    fields: convertSideFieldsToPublic(evalRes.output),
    hasDynamicFields: evalRes.output.hasDynamicFields,
    evidence: outputEvidence,
    gaps: collectSideGaps(evalRes.output),
  });

  return { input, output };
}

function containsLocation(
  outer: DiagnosticLocation,
  inner: DiagnosticLocation,
): boolean {
  return (
    outer.file === inner.file &&
    outer.start <= inner.start &&
    outer.end >= inner.end &&
    (outer.start !== inner.start || outer.end !== inner.end)
  );
}

async function computeProjectState(
  ctx: ProjectContext,
  options?: ValidatorQueryOptions,
): Promise<CachedProjectState> {
  const evaluator = new ZodProjectEvaluator(ctx, options);
  const astStore = new AstRangeStore(ctx);

  const [validatorCalls, schemaCalls, varDecls] = await Promise.all([
    ctx.findSyntax({
      kinds: ["CallExpression"],
      calleeNames: VALIDATOR_CALL_METHODS,
    }),
    ctx.findSyntax({
      kinds: ["CallExpression"],
      calleeNames: ALL_SCHEMA_DISCOVERY_CALLEES,
    }),
    ctx.findSyntax({
      kinds: ["VariableDeclaration"],
    }),
  ]);

  const varNameByInitializerId = new Map<string, string>();
  for (const decl of varDecls) {
    if (decl.initializer !== undefined && decl.name !== undefined) {
      // Extract declared identifier text concisely from declaration text `foo = ...`
      const eqIdx = decl.text.indexOf("=");
      const lhs = (eqIdx >= 0 ? decl.text.slice(0, eqIdx) : decl.text).trim();
      const colonIdx = lhs.indexOf(":");
      const varName = (colonIdx >= 0 ? lhs.slice(0, colonIdx) : lhs).trim();
      if (varName.length > 0 && /^[A-Za-z_$][\w$]*$/u.test(varName)) {
        varNameByInitializerId.set(decl.initializer.handleId, varName);
      }
    }
  }

  const allOuterCalls = [...validatorCalls, ...schemaCalls];
  const rootSchemaCalls = schemaCalls.filter(
    (candidate) =>
      !allOuterCalls.some((other) =>
        containsLocation(other.location, candidate.location),
      ),
  );

  // Batch AST range fetches for validation calls and root schema calls
  const [callTrees, schemaTrees] = await Promise.all([
    Promise.all(
      validatorCalls.map((call) => astStore.getNode(call.ref, call.location)),
    ),
    Promise.all(
      rootSchemaCalls.map((call) => astStore.getNode(call.ref, call.location)),
    ),
  ]);

  const inputs: ValidatorInputFieldsResult[] = [];
  const outputs: ValidatorOutputFieldsResult[] = [];
  const byTargetHandle = new Map<
    string,
    {
      readonly input: ValidatorInputFieldsResult;
      readonly output: ValidatorOutputFieldsResult;
    }
  >();

  // 1. Evaluate validation call sites (.parse / .safeParse / .parseAsync / .safeParseAsync)
  const callPairs = await Promise.all(
    validatorCalls.map(async (call, idx) => {
      const loaded = callTrees[idx];
      if (loaded === undefined) {
        return undefined;
      }
      const { methodName, receiverNode } = inspectCallCallee(
        loaded.tree,
        loaded.node,
      );
      if (methodName === undefined || receiverNode === undefined) {
        return undefined;
      }
      const evalRes = await evaluator.evaluateSchemaNode(
        loaded.tree,
        receiverNode,
      );
      if (!evalRes.recognized) {
        return undefined;
      }
      const callEvidence: Evidence = {
        kind: "source",
        message: `Zod .${methodName}() validation call`,
        at: call.location,
      };
      return {
        targetHandle: call.ref.handleId,
        pair: buildPairResults(evalRes, {
          siteKind: "call",
          validatorMethod: methodName,
          target: call.ref,
          at: call.location,
          extraEvidence: [callEvidence],
        }),
      };
    }),
  );

  for (const item of callPairs) {
    if (item !== undefined) {
      inputs.push(item.pair.input);
      outputs.push(item.pair.output);
      byTargetHandle.set(item.targetHandle, item.pair);
    }
  }

  // 2. Evaluate root schema sites
  const schemaPairs = await Promise.all(
    rootSchemaCalls.map(async (call, idx) => {
      const loaded = schemaTrees[idx];
      if (loaded === undefined) {
        return undefined;
      }
      const evalRes = await evaluator.evaluateSchemaNode(
        loaded.tree,
        loaded.node,
      );
      if (!evalRes.recognized) {
        return undefined;
      }
      const inferredName =
        varNameByInitializerId.get(call.ref.handleId) ?? evalRes.schemaName;
      return {
        targetHandle: call.ref.handleId,
        pair: buildPairResults(evalRes, {
          siteKind: "schema",
          ...(inferredName !== undefined ? { schemaName: inferredName } : {}),
          target: call.ref,
          at: call.location,
          extraEvidence: [],
        }),
      };
    }),
  );

  for (const item of schemaPairs) {
    if (item !== undefined) {
      inputs.push(item.pair.input);
      outputs.push(item.pair.output);
      byTargetHandle.set(item.targetHandle, item.pair);
    }
  }

  return { inputs, outputs, byTargetHandle };
}

async function getOrComputeProjectState(
  ctx: ProjectContext,
  options?: ValidatorQueryOptions,
): Promise<CachedProjectState> {
  const resolvers = options?.externalSchemaResolvers;
  // Bypass snapshot cache if custom non-static resolvers are provided with identical IDs
  // to preserve isolation while caching standard single-resolver / no-resolver runs cleanly.
  const cacheKey = resolverCacheKey(resolvers);
  let perResolver = projectCache.get(ctx.project);
  if (perResolver === undefined) {
    perResolver = new Map();
    projectCache.set(ctx.project, perResolver);
  }
  const hit = perResolver.get(cacheKey);
  if (hit !== undefined && (resolvers === undefined || resolvers.length <= 1)) {
    return hit;
  }
  const computed = await computeProjectState(ctx, options);
  if (resolvers === undefined || resolvers.length <= 1) {
    perResolver.set(cacheKey, computed);
  }
  return computed;
}

async function evaluateExplicitTargets(
  ctx: ProjectContext,
  state: CachedProjectState,
  targets: readonly SyntaxRef[],
  scope: "all" | "calls" | "schemas",
  options?: ValidatorQueryOptions,
): Promise<
  readonly {
    readonly input: ValidatorInputFieldsResult;
    readonly output: ValidatorOutputFieldsResult;
  }[]
> {
  const results: {
    readonly input: ValidatorInputFieldsResult;
    readonly output: ValidatorOutputFieldsResult;
  }[] = [];
  const missingTargets: SyntaxRef[] = [];

  for (const target of targets) {
    const cached = state.byTargetHandle.get(target.handleId);
    if (cached !== undefined) {
      if (scope === "all" || cached.input.siteKind === scope.slice(0, -1)) {
        results.push(cached);
      }
    } else {
      missingTargets.push(target);
    }
  }

  if (missingTargets.length === 0) {
    return results;
  }

  const evaluator = new ZodProjectEvaluator(ctx, options);
  const astStore = new AstRangeStore(ctx);
  const syntaxRows = await ctx.syntaxOf(missingTargets, {
    select: { kind: true, location: true, initializer: true },
  });

  for (const row of syntaxRows) {
    if (row.syntax === null || row.location === null) {
      continue;
    }
    const loaded = await astStore.getNode(row.syntax, row.location);
    if (loaded === undefined) {
      continue;
    }
    const { tree, node } = loaded;

    if (scope !== "schemas" && node.kind === "CallExpression") {
      const { methodName, receiverNode } = inspectCallCallee(tree, node);
      if (
        methodName !== undefined &&
        VALIDATOR_CALL_METHODS.includes(
          methodName as (typeof VALIDATOR_CALL_METHODS)[number],
        ) &&
        receiverNode !== undefined
      ) {
        const evalRes = await evaluator.evaluateSchemaNode(tree, receiverNode);
        if (evalRes.recognized) {
          results.push(
            buildPairResults(evalRes, {
              siteKind: "call",
              validatorMethod: methodName,
              target: row.syntax,
              at: row.location,
              extraEvidence: [
                {
                  kind: "source",
                  message: `Zod .${methodName}() validation call`,
                  at: row.location,
                },
              ],
            }),
          );
        }
        continue;
      }
    }

    if (scope !== "calls") {
      const evalTarget =
        node.kind === "VariableDeclaration"
          ? (resolveChild(tree, node.initializer) ?? node)
          : node;
      const evalRes = await evaluator.evaluateSchemaNode(tree, evalTarget);
      if (evalRes.recognized) {
        results.push(
          buildPairResults(evalRes, {
            siteKind: "schema",
            target: row.syntax,
            at: row.location,
            extraEvidence: [],
          }),
        );
      }
    }
  }

  return results;
}

function filterByScope<T extends { readonly siteKind: "call" | "schema" }>(
  items: readonly T[],
  scope: "all" | "calls" | "schemas",
): readonly T[] {
  if (scope === "all") {
    return items;
  }
  const wanted = scope === "calls" ? "call" : "schema";
  return items.filter((item) => item.siteKind === wanted);
}

function recordResultGaps(
  ctx: ProjectContext,
  items: readonly { readonly gaps: readonly Gap[] }[],
): void {
  const allGaps: Gap[] = [];
  for (const item of items) {
    allGaps.push(...item.gaps);
  }
  ctx.recordGaps(allGaps);
}

/**
 * Extracts Zod schema input-side field requirements across validation call sites
 * (`.parse`, `.safeParse`, `.parseAsync`, `.safeParseAsync`) and schema declarations.
 */
export async function extractZodInputFields(
  ctx: ProjectContext,
  options?: ValidatorQueryOptions,
): Promise<readonly ValidatorInputFieldsResult[]> {
  ctx.recordGaps([]);
  const scope = options?.scope ?? "all";
  const state = await getOrComputeProjectState(ctx, options);
  const items =
    options?.targets !== undefined
      ? (
          await evaluateExplicitTargets(
            ctx,
            state,
            options.targets,
            scope,
            options,
          )
        ).map((pair) => pair.input)
      : filterByScope(state.inputs, scope);
  recordResultGaps(ctx, items);
  return items;
}

/**
 * Extracts Zod schema output-side field presence and defaults across validation
 * call sites and schema declarations.
 */
export async function extractZodOutputFields(
  ctx: ProjectContext,
  options?: ValidatorQueryOptions,
): Promise<readonly ValidatorOutputFieldsResult[]> {
  ctx.recordGaps([]);
  const scope = options?.scope ?? "all";
  const state = await getOrComputeProjectState(ctx, options);
  const items =
    options?.targets !== undefined
      ? (
          await evaluateExplicitTargets(
            ctx,
            state,
            options.targets,
            scope,
            options,
          )
        ).map((pair) => pair.output)
      : filterByScope(state.outputs, scope);
  recordResultGaps(ctx, items);
  return items;
}

/**
 * Convenience query returning both input-side and output-side Zod field views.
 */
export async function extractZodFields(
  ctx: ProjectContext,
  options?: ValidatorQueryOptions,
): Promise<{
  readonly inputs: readonly ValidatorInputFieldsResult[];
  readonly outputs: readonly ValidatorOutputFieldsResult[];
}> {
  ctx.recordGaps([]);
  const scope = options?.scope ?? "all";
  const state = await getOrComputeProjectState(ctx, options);
  if (options?.targets !== undefined) {
    const pairs = await evaluateExplicitTargets(
      ctx,
      state,
      options.targets,
      scope,
      options,
    );
    const inputs = pairs.map((p) => p.input);
    const outputs = pairs.map((p) => p.output);
    recordResultGaps(ctx, [...inputs, ...outputs]);
    return { inputs, outputs };
  }
  const inputs = filterByScope(state.inputs, scope);
  const outputs = filterByScope(state.outputs, scope);
  recordResultGaps(ctx, [...inputs, ...outputs]);
  return { inputs, outputs };
}

/**
 * Evaluates targeted field-level Zod expressions or callback refinements
 * (for example `z.number().default(18)` or `(s) => s.optional()`).
 */
export async function extractZodFieldModifiers(
  ctx: ProjectContext,
  targets: readonly SyntaxRef[],
): Promise<readonly ValidatorFieldModifierResult[]> {
  ctx.recordGaps([]);
  if (targets.length === 0) {
    return [];
  }

  const evaluator = new ZodProjectEvaluator(ctx);
  const astStore = new AstRangeStore(ctx);
  const rows = await ctx.syntaxOf(targets, {
    select: { kind: true, location: true },
  });

  const results: ValidatorFieldModifierResult[] = [];
  for (const row of rows) {
    if (row.syntax === null || row.location === null) {
      continue;
    }
    const loaded = await astStore.getNode(row.syntax, row.location);
    if (loaded === undefined) {
      continue;
    }
    const { tree, node } = loaded;
    const unwrapped = unwrapExpression(tree, node);
    if (unwrapped === undefined) {
      continue;
    }

    if (
      unwrapped.kind === "ArrowFunction" ||
      unwrapped.kind === "FunctionExpression"
    ) {
      const paramName = extractCallbackParamName(tree, unwrapped);
      const returnExpr = extractCallbackReturnExpr(tree, unwrapped);
      if (paramName !== undefined && returnExpr !== undefined) {
        const cbState = evaluateCallbackChain(tree, returnExpr, paramName);
        if (cbState.resolved) {
          results.push(
            buildValidatorFieldModifier({
              target: row.syntax,
              at: row.location,
              providerId: PROVIDER_ID,
              resolved: true,
              mode: "callback",
              requirement: cbState.requirement,
              nullable: cbState.nullable,
              fromDefault: cbState.fromDefault,
              nullabilityExplicit: cbState.nullabilityExplicit,
              evidence: [
                {
                  kind: "source",
                  message: "Zod callback field refinement",
                  at: row.location,
                },
              ],
              gaps: [],
            }),
          );
          continue;
        }
      }
      const gap: Gap = {
        kind: "unresolved-schema-field",
        message: `Unresolved callback field refinement: ${unwrapped.text.trim()}`,
        at: row.location,
      };
      results.push(
        buildValidatorFieldModifier({
          target: row.syntax,
          at: row.location,
          providerId: PROVIDER_ID,
          resolved: false,
          mode: "callback",
          fromDefault: false,
          nullabilityExplicit: false,
          evidence: [
            {
              kind: "source",
              message: "Unresolved Zod callback refinement",
              at: row.location,
            },
          ],
          gaps: [gap],
        }),
      );
      continue;
    }

    const fieldEval = await evaluator.evaluateFieldExpression(
      tree,
      unwrapped,
      "field",
      row.location,
      new Set(),
    );
    if (fieldEval.input.resolved) {
      results.push(
        buildValidatorFieldModifier({
          target: row.syntax,
          at: row.location,
          providerId: PROVIDER_ID,
          resolved: true,
          mode: "direct",
          requirement: fieldEval.input.requirement,
          nullable: fieldEval.input.nullable,
          fromDefault: fieldEval.input.fromDefault,
          nullabilityExplicit: fieldEval.input.explicitNullability,
          evidence: fieldEval.input.evidence,
          gaps: fieldEval.input.dynamicGaps,
        }),
      );
    } else {
      results.push(
        buildValidatorFieldModifier({
          target: row.syntax,
          at: row.location,
          providerId: PROVIDER_ID,
          resolved: false,
          mode: "direct",
          fromDefault: false,
          nullabilityExplicit: false,
          evidence: fieldEval.input.evidence,
          gaps: [fieldEval.input.gap],
        }),
      );
    }
  }

  recordResultGaps(ctx, results);
  return results;
}

function extractCallbackParamName(
  tree: AstTree,
  fnNode: AstNode,
): string | undefined {
  const firstParamRef = fnNode.parameters?.[0];
  const paramNode = resolveChild(tree, firstParamRef);
  if (paramNode === undefined) {
    return undefined;
  }
  const paramNameNode =
    resolveChild(tree, paramNode.name) ??
    paramNode.children.find((c) => c.kind === "Identifier");
  const text = (paramNameNode ?? paramNode).text.trim();
  const identMatch = /^[A-Za-z_$][\w$]*/u.exec(text);
  return identMatch?.[0];
}

function extractCallbackReturnExpr(
  tree: AstTree,
  fnNode: AstNode,
): AstNode | undefined {
  const bodyNode =
    unwrapExpression(tree, resolveChild(tree, fnNode.body)) ??
    fnNode.children.find((c) => c.kind === "Block");
  if (bodyNode === undefined) {
    return undefined;
  }
  if (bodyNode.kind !== "Block") {
    return bodyNode;
  }
  const statements = bodyNode.children.filter(
    (c) => c.kind !== "OpenBraceToken" && c.kind !== "CloseBraceToken",
  );
  if (statements.length !== 1 || statements[0]?.kind !== "ReturnStatement") {
    return undefined;
  }
  const ret = statements[0];
  const retExpr =
    resolveChild(tree, ret.expression) ??
    ret.children.find((c) => c.kind !== "ReturnKeyword");
  return unwrapExpression(tree, retExpr);
}

type CallbackChainState =
  | {
      readonly resolved: true;
      readonly requirement: ValidatorFieldRequirement;
      readonly nullable: boolean;
      readonly fromDefault: boolean;
      readonly nullabilityExplicit: boolean;
    }
  | { readonly resolved: false };

function evaluateCallbackChain(
  tree: AstTree,
  exprNode: AstNode,
  paramName: string,
): CallbackChainState {
  const unwrapped = unwrapExpression(tree, exprNode);
  if (unwrapped === undefined) {
    return { resolved: false };
  }
  if (unwrapped.kind === "Identifier" && unwrapped.text.trim() === paramName) {
    return {
      resolved: true,
      requirement: "required",
      nullable: false,
      fromDefault: false,
      nullabilityExplicit: false,
    };
  }
  if (unwrapped.kind !== "CallExpression") {
    return { resolved: false };
  }
  const { methodName, receiverNode } = inspectCallCallee(tree, unwrapped);
  if (methodName === undefined || receiverNode === undefined) {
    return { resolved: false };
  }
  const inner = evaluateCallbackChain(tree, receiverNode, paramName);
  if (!inner.resolved) {
    return { resolved: false };
  }
  if (methodName === "optional" || methodName === "exactOptional") {
    return { ...inner, requirement: "optional" };
  }
  if (methodName === "nullable") {
    return { ...inner, nullable: true, nullabilityExplicit: true };
  }
  if (methodName === "nullish") {
    return {
      ...inner,
      requirement: "optional",
      nullable: true,
      nullabilityExplicit: true,
    };
  }
  if (
    methodName === "default" ||
    methodName === "prefault" ||
    methodName === "catch"
  ) {
    return { ...inner, requirement: "defaulted", fromDefault: true };
  }
  if (methodName === "nonoptional") {
    return { ...inner, requirement: "required" };
  }
  if (TRANSPARENT_MODIFIER_METHODS.has(methodName)) {
    return inner;
  }
  return { resolved: false };
}

/** Standard `ValidatorFieldExtractor` implementation backed by `@tssae/zod`. */
export const zodValidator: ValidatorFieldExtractor = {
  id: PROVIDER_ID,
  extractInputFields: extractZodInputFields,
  extractOutputFields: extractZodOutputFields,
};

export function sortValidatorFieldKeys(
  fields: Readonly<Record<string, ValidatorFieldInfo>>,
): readonly string[] {
  return sortStringKeys(Object.keys(fields));
}
