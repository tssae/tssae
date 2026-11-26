import {
  extractZodFieldModifiers,
  extractZodInputFields,
  extractZodOutputFields,
  type ExternalSchemaResolution,
  type ExternalSchemaResolver,
  type ValidatorFieldExtractor,
  type ValidatorFieldInfo,
  type ValidatorFieldModifierResult,
  type ValidatorQueryOptions,
} from "@tssae/zod";
import {
  type DeclarationRef,
  type DiagnosticLocation,
  type Evidence,
  type Gap,
  type ProjectContext,
  type ProjectSummary,
  type SyntaxNodeResult,
  type SyntaxRef,
} from "tssae";

import {
  AstRangeStore,
  inspectCallCallee,
  readStaticPropertyName,
  readStringLiteral,
  resolveChild,
  sortStringKeys,
  unwrapExpression,
  type AstNode,
  type AstTree,
} from "./ast.ts";
import {
  buildDrizzleTableColumns,
  buildDrizzleZodSchema,
  buildQueryRowProjection,
  type DrizzleColumnInfo,
  type DrizzleTableColumnsOptions,
  type DrizzleTableColumnsResult,
  type DrizzleZodFieldInfo,
  type DrizzleZodQueryOptions,
  type DrizzleZodSchemaResult,
  type QueryRowColumnInfo,
  type QueryRowProjectionExtractor,
  type QueryRowProjectionOptions,
  type QueryRowProjectionResult,
  type QueryRowRelationCardinality,
  type QueryRowRelationInfo,
} from "./contracts.ts";

const PROVIDER_ID = "drizzle";
const DRIZZLE_ORM_PKG = "drizzle-orm";
const DRIZZLE_ZOD_PKG = "drizzle-zod";

const TABLE_CREATOR_CALLEES = new Set([
  "sqliteTable",
  "pgTable",
  "mysqlTable",
  "singlestoreTable",
]);

const TABLE_CREATOR_FACTORIES = new Set([
  "sqliteTableCreator",
  "pgTableCreator",
  "mysqlTableCreator",
  "singlestoreTableCreator",
]);

const TABLE_BUILDER_METHODS = new Set([...TABLE_CREATOR_CALLEES, "table"]);

const TRANSPARENT_COLUMN_METHODS = new Set([
  "references",
  "unique",
  "generatedAlwaysAs",
  "generatedAlwaysAsIdentity",
  "$type",
  "$onUpdate",
  "$onUpdateFn",
]);

const DEFAULT_COLUMN_METHODS = new Set([
  "default",
  "defaultNow",
  "defaultRandom",
  "$defaultFn",
  "$default",
]);

const SELECT_CALL_METHODS = new Set([
  "select",
  "selectDistinct",
  "selectDistinctOn",
]);

const CHAIN_MODIFIER_METHODS = new Set([
  "from",
  "where",
  "leftJoin",
  "rightJoin",
  "innerJoin",
  "fullJoin",
  "crossJoin",
  "orderBy",
  "groupBy",
  "having",
  "limit",
  "offset",
  "get",
  "all",
  "values",
  "execute",
  "prepare",
  "as",
  "$dynamic",
]);

const JOIN_METHODS = new Set([
  "leftJoin",
  "rightJoin",
  "innerJoin",
  "fullJoin",
  "crossJoin",
]);

type ParsedColumnBuilder = {
  readonly sqlName: string;
  readonly nullable: boolean;
  readonly primaryKey: boolean;
  readonly hasDefault: boolean;
};

type RelationEdge = {
  readonly name: string;
  readonly targetTable: string;
  readonly cardinality: QueryRowRelationCardinality;
  readonly at: DiagnosticLocation;
};

type TableIndex = {
  readonly tables: readonly DrizzleTableColumnsResult[];
  readonly byDeclSpan: ReadonlyMap<string, DrizzleTableColumnsResult>;
  readonly byTableName: ReadonlyMap<string, DrizzleTableColumnsResult>;
  readonly bySqlName: ReadonlyMap<string, DrizzleTableColumnsResult>;
};

type CachedDrizzleSnapshot = {
  tableIndexPromise?: Promise<TableIndex>;
  projectionsPromise?: Promise<readonly QueryRowProjectionResult[]>;
  drizzleZodPromises: Map<boolean, Promise<readonly DrizzleZodSchemaResult[]>>;
};

const snapshotCache = new WeakMap<ProjectSummary, CachedDrizzleSnapshot>();

function getSnapshotCache(project: ProjectSummary): CachedDrizzleSnapshot {
  let cached = snapshotCache.get(project);
  if (cached === undefined) {
    cached = { drizzleZodPromises: new Map() };
    snapshotCache.set(project, cached);
  }
  return cached;
}

function spanKey(loc: DiagnosticLocation): string {
  return `${loc.file}:${String(loc.start)}:${String(loc.end)}`;
}

function isSourceFile(file: string): boolean {
  return (
    !file.includes("node_modules") &&
    !/\.d\.[cm]?ts$/u.test(file) &&
    /\.[cm]?tsx?$/u.test(file)
  );
}

function isPackageFile(
  file: string | undefined | null,
  pkgName: string,
): boolean {
  if (file == null) {
    return false;
  }
  const normalized = file.replaceAll("\\", "/");
  return (
    normalized.includes(`/node_modules/${pkgName}/`) ||
    normalized.includes(`/${pkgName}/`)
  );
}

function matchesPackage(
  decl:
    | {
        readonly packageName?: string | null;
        readonly location?: DiagnosticLocation | null;
      }
    | undefined,
  pkgName: string,
): boolean {
  if (decl === undefined) {
    return false;
  }
  return (
    decl.packageName === pkgName || isPackageFile(decl.location?.file, pkgName)
  );
}

function extractVarNameFromDeclText(text: string): string | undefined {
  const eqIdx = text.indexOf("=");
  const lhs = (eqIdx >= 0 ? text.slice(0, eqIdx) : text).trim();
  const colonIdx = lhs.indexOf(":");
  const name = (colonIdx >= 0 ? lhs.slice(0, colonIdx) : lhs).trim();
  return name.length > 0 && /^[A-Za-z_$][\w$]*$/u.test(name) ? name : undefined;
}

function isDrizzleTableTypeText(typeText: string): boolean {
  return (
    typeText.includes("PgTable") ||
    typeText.includes("SQLiteTable") ||
    typeText.includes("MySqlTable") ||
    typeText.includes("SingleStoreTable") ||
    typeText.includes("TableWithColumns")
  );
}

function parseColumnBuilderChain(
  tree: AstTree,
  exprNode: AstNode,
  fallbackColName: string,
): ParsedColumnBuilder | undefined {
  const unwrapped = unwrapExpression(tree, exprNode);
  if (unwrapped === undefined || unwrapped.kind !== "CallExpression") {
    return undefined;
  }

  let nullable = true;
  let primaryKey = false;
  let hasDefault = false;
  let current: AstNode | undefined = unwrapped;

  while (current !== undefined && current.kind === "CallExpression") {
    const { methodName, receiverNode } = inspectCallCallee(tree, current);
    if (methodName === undefined) {
      return undefined;
    }

    if (methodName === "primaryKey") {
      primaryKey = true;
      nullable = false;
      current = unwrapExpression(tree, receiverNode);
      continue;
    }
    if (methodName === "notNull") {
      nullable = false;
      current = unwrapExpression(tree, receiverNode);
      continue;
    }
    if (DEFAULT_COLUMN_METHODS.has(methodName)) {
      hasDefault = true;
      current = unwrapExpression(tree, receiverNode);
      continue;
    }
    if (TRANSPARENT_COLUMN_METHODS.has(methodName)) {
      current = unwrapExpression(tree, receiverNode);
      continue;
    }

    // Reached root column constructor (`serial("id")`, `t.integer("id")`, etc.)
    const receiverUnwrapped = unwrapExpression(tree, receiverNode);
    if (
      receiverUnwrapped !== undefined &&
      receiverUnwrapped.kind !== "Identifier"
    ) {
      // Unrecognized method chain root
      return undefined;
    }
    const args = (current.arguments ?? [])
      .map((r) => unwrapExpression(tree, resolveChild(tree, r)))
      .filter((n): n is AstNode => n !== undefined);
    const sqlName = readStringLiteral(args[0]) ?? fallbackColName;
    return {
      sqlName,
      nullable,
      primaryKey,
      hasDefault,
    };
  }

  return undefined;
}

async function resolveIdentifierToObjectLiteral(
  ctx: ProjectContext,
  astStore: AstRangeStore,
  ref: SyntaxRef,
): Promise<
  | {
      readonly tree: AstTree;
      readonly objNode: AstNode;
    }
  | undefined
> {
  const [decl] = await ctx.resolveDeclarations([ref], {
    select: { location: true, syntax: true, kind: true },
  });
  if (
    decl === undefined ||
    decl.declaration === null ||
    decl.location === null ||
    decl.syntax === null ||
    !isSourceFile(decl.location.file)
  ) {
    return undefined;
  }
  const loaded = await astStore.getNode(decl.syntax, decl.location);
  if (loaded === undefined || loaded.node.kind !== "VariableDeclaration") {
    return undefined;
  }
  const initNode = unwrapExpression(
    loaded.tree,
    resolveChild(loaded.tree, loaded.node.initializer),
  );
  if (initNode === undefined || initNode.kind !== "ObjectLiteralExpression") {
    return undefined;
  }
  return { tree: loaded.tree, objNode: initNode };
}

function extractCallbackObjectReturn(
  tree: AstTree,
  fnNode: AstNode,
): AstNode | undefined {
  const body = unwrapExpression(tree, resolveChild(tree, fnNode.body));
  if (body === undefined) {
    return undefined;
  }
  if (body.kind === "ObjectLiteralExpression") {
    return body;
  }
  if (body.kind === "Block") {
    const stmts = body.children.filter(
      (c) => c.kind !== "OpenBraceToken" && c.kind !== "CloseBraceToken",
    );
    if (stmts.length === 1 && stmts[0]?.kind === "ReturnStatement") {
      const ret = stmts[0];
      const retExpr = unwrapExpression(
        tree,
        resolveChild(tree, ret.expression) ??
          ret.children.find((c) => c.kind !== "ReturnKeyword"),
      );
      if (retExpr?.kind === "ObjectLiteralExpression") {
        return retExpr;
      }
    }
  }
  return undefined;
}

async function parseColumnsObjectLiteral(
  ctx: ProjectContext,
  astStore: AstRangeStore,
  tree: AstTree,
  objNode: AstNode,
  tableName: string,
): Promise<{
  readonly columns: Record<string, DrizzleColumnInfo>;
  readonly hasDynamicColumns: boolean;
  readonly gaps: Gap[];
  readonly evidence: Evidence[];
}> {
  const columns: Record<string, DrizzleColumnInfo> = {};
  const gaps: Gap[] = [];
  const evidence: Evidence[] = [];
  let hasDynamicColumns = false;

  for (const child of objNode.children) {
    if (child.kind === "PropertyAssignment") {
      const nameNode = resolveChild(tree, child.name) ?? child.children[0];
      const colName = readStaticPropertyName(nameNode);
      if (colName === undefined) {
        hasDynamicColumns = true;
        gaps.push({
          kind: "dynamic-table-columns",
          message: `Dynamic computed column key in table "${tableName}"`,
          at: child.location,
        });
        continue;
      }
      const initNode =
        unwrapExpression(tree, resolveChild(tree, child.initializer)) ??
        unwrapExpression(tree, child.children[1]);
      if (initNode === undefined) {
        hasDynamicColumns = true;
        gaps.push({
          kind: "dynamic-table-columns",
          message: `Unresolved column initializer "${colName}" in table "${tableName}"`,
          at: child.location,
        });
        continue;
      }
      const parsed = parseColumnBuilderChain(tree, initNode, colName);
      if (parsed === undefined) {
        hasDynamicColumns = true;
        gaps.push({
          kind: "dynamic-table-columns",
          message: `Unrecognized column builder for "${colName}" in table "${tableName}"`,
          at: child.location,
        });
        continue;
      }
      const colEv: Evidence = {
        kind: "source",
        message: `Column "${colName}" (${parsed.sqlName}) on table "${tableName}"`,
        at: child.location,
      };
      evidence.push(colEv);
      columns[colName] = {
        name: colName,
        sqlName: parsed.sqlName,
        nullable: parsed.nullable,
        primaryKey: parsed.primaryKey,
        hasDefault: parsed.hasDefault,
        evidence: [colEv],
      };
    } else if (child.kind === "SpreadAssignment") {
      const spreadExpr = unwrapExpression(
        tree,
        resolveChild(tree, child.expression) ?? child.children[0],
      );
      if (spreadExpr?.kind === "ObjectLiteralExpression") {
        const nested = await parseColumnsObjectLiteral(
          ctx,
          astStore,
          tree,
          spreadExpr,
          tableName,
        );
        Object.assign(columns, nested.columns);
        gaps.push(...nested.gaps);
        evidence.push(...nested.evidence);
        if (nested.hasDynamicColumns) {
          hasDynamicColumns = true;
        }
        continue;
      }
      if (spreadExpr?.kind === "Identifier") {
        const resolved = await resolveIdentifierToObjectLiteral(
          ctx,
          astStore,
          spreadExpr.ref,
        );
        if (resolved !== undefined) {
          const nested = await parseColumnsObjectLiteral(
            ctx,
            astStore,
            resolved.tree,
            resolved.objNode,
            tableName,
          );
          Object.assign(columns, nested.columns);
          gaps.push(...nested.gaps);
          evidence.push(...nested.evidence);
          if (nested.hasDynamicColumns) {
            hasDynamicColumns = true;
          }
          continue;
        }
      }
      hasDynamicColumns = true;
      gaps.push({
        kind: "dynamic-table-columns",
        message: `Dynamic column spread in table "${tableName}": ${child.text.trim()}`,
        at: child.location,
      });
    }
  }

  return { columns, hasDynamicColumns, gaps, evidence };
}

async function parseDrizzleTableFromCall(
  ctx: ProjectContext,
  astStore: AstRangeStore,
  tree: AstTree,
  callNode: AstNode,
  options: {
    readonly target: DeclarationRef | SyntaxRef;
    readonly declaredName?: string;
    readonly declarationLocation?: DiagnosticLocation;
  },
): Promise<DrizzleTableColumnsResult | undefined> {
  const { calleeNode, methodName } = inspectCallCallee(tree, callNode);
  if (calleeNode === undefined) {
    return undefined;
  }

  let isVerifiedDrizzle = false;
  if (methodName !== undefined && TABLE_BUILDER_METHODS.has(methodName)) {
    const [decl] = await ctx.resolveDeclarations([calleeNode.ref], {
      select: { packageName: true, location: true },
    });
    if (matchesPackage(decl, DRIZZLE_ORM_PKG)) {
      isVerifiedDrizzle = true;
    }
  }
  if (!isVerifiedDrizzle) {
    const [typeInfo] = await ctx.typeAt([callNode.ref]);
    if (typeInfo !== undefined && isDrizzleTableTypeText(typeInfo.typeText)) {
      isVerifiedDrizzle = true;
    }
  }
  if (!isVerifiedDrizzle) {
    return undefined;
  }

  const args = (callNode.arguments ?? [])
    .map((r) => unwrapExpression(tree, resolveChild(tree, r)))
    .filter((n): n is AstNode => n !== undefined);
  const sqlTableName = readStringLiteral(args[0]);
  if (sqlTableName === undefined) {
    return undefined;
  }

  const tableName = options.declaredName ?? sqlTableName;
  const colsArg = args[1];
  let objNode: AstNode | undefined;
  if (colsArg?.kind === "ObjectLiteralExpression") {
    objNode = colsArg;
  } else if (
    colsArg?.kind === "ArrowFunction" ||
    colsArg?.kind === "FunctionExpression"
  ) {
    objNode = extractCallbackObjectReturn(tree, colsArg);
  }

  const declLoc = options.declarationLocation ?? callNode.location;
  const rootEv: Evidence = {
    kind: "source",
    message: `Drizzle table "${tableName}" (${sqlTableName})`,
    at: declLoc,
  };

  if (objNode === undefined) {
    return buildDrizzleTableColumns({
      target: options.target,
      at: declLoc,
      tableName,
      sqlTableName,
      columns: {},
      hasDynamicColumns: true,
      evidence: [rootEv],
      gaps: [
        {
          kind: "dynamic-table-columns",
          message: `Dynamic columns definition for Drizzle table "${tableName}"`,
          at: colsArg?.location ?? callNode.location,
        },
      ],
    });
  }

  const parsed = await parseColumnsObjectLiteral(
    ctx,
    astStore,
    tree,
    objNode,
    tableName,
  );
  return buildDrizzleTableColumns({
    target: options.target,
    at: declLoc,
    tableName,
    sqlTableName,
    columns: parsed.columns,
    hasDynamicColumns: parsed.hasDynamicColumns,
    evidence: [rootEv, ...parsed.evidence],
    gaps: parsed.gaps,
  });
}

async function computeTableIndex(ctx: ProjectContext): Promise<TableIndex> {
  const astStore = new AstRangeStore(ctx);
  const [tableCalls, creatorCalls, importSpecs, varDecls] = await Promise.all([
    ctx.findSyntax({
      kinds: ["CallExpression"],
      calleeNames: [...TABLE_BUILDER_METHODS],
    }),
    ctx.findSyntax({
      kinds: ["CallExpression"],
      calleeNames: [...TABLE_CREATOR_FACTORIES],
    }),
    ctx.findSyntax({
      kinds: ["ImportSpecifier"],
    }),
    ctx.findSyntax({
      kinds: ["VariableDeclaration"],
    }),
  ]);

  const varByInitHandle = new Map<string, SyntaxNodeResult>();
  for (const decl of varDecls) {
    if (decl.initializer !== undefined) {
      varByInitHandle.set(decl.initializer.handleId, decl);
    }
  }

  const seenDeclSpans = new Set<string>();
  const tables: DrizzleTableColumnsResult[] = [];

  const tryAddTableFromVarOrCall = async (
    declLoc: DiagnosticLocation,
    declName: string | undefined,
    callRef: SyntaxRef,
    callLoc: DiagnosticLocation,
    targetRef: DeclarationRef | SyntaxRef,
  ): Promise<void> => {
    const key = spanKey(declLoc);
    if (seenDeclSpans.has(key)) {
      return;
    }
    const loaded = await astStore.getNode(callRef, callLoc);
    if (loaded === undefined || loaded.node.kind !== "CallExpression") {
      return;
    }
    const parsed = await parseDrizzleTableFromCall(
      ctx,
      astStore,
      loaded.tree,
      loaded.node,
      {
        target: targetRef,
        ...(declName !== undefined ? { declaredName: declName } : {}),
        declarationLocation: declLoc,
      },
    );
    if (parsed !== undefined) {
      seenDeclSpans.add(key);
      tables.push(parsed);
    }
  };

  // 1. Direct table constructor calls inside the current project
  await Promise.all(
    tableCalls.map(async (call) => {
      const enclosingVar = varByInitHandle.get(call.ref.handleId);
      const declName =
        enclosingVar !== undefined
          ? extractVarNameFromDeclText(enclosingVar.text)
          : undefined;
      await tryAddTableFromVarOrCall(
        enclosingVar?.location ?? call.location,
        declName,
        call.ref,
        call.location,
        enclosingVar?.ref ?? call.ref,
      );
    }),
  );

  // 2. Creator factory invocations like `pgTableCreator(...)("users", { ... })`
  if (creatorCalls.length > 0) {
    const creatorRows = await Promise.all(
      creatorCalls.map((c) =>
        astStore.getNode(c.ref, {
          file: c.location.file,
          start: Math.max(0, c.location.start),
          end: c.location.end + 4096,
          match: "within",
        }),
      ),
    );
    for (const row of creatorRows) {
      const parent = row?.node.parent;
      if (parent?.kind === "CallExpression") {
        const varDecl =
          parent.parent?.kind === "VariableDeclaration"
            ? parent.parent
            : undefined;
        await tryAddTableFromVarOrCall(
          varDecl?.location ?? parent.location,
          varDecl !== undefined
            ? extractVarNameFromDeclText(varDecl.text)
            : undefined,
          parent.ref,
          parent.location,
          varDecl?.ref ?? parent.ref,
        );
      }
    }
  }

  // 3. Cross-package Drizzle tables referenced by ImportSpecifiers
  if (importSpecs.length > 0) {
    const importDecls = await ctx.resolveDeclarations(
      importSpecs.map((s) => s.ref),
      {
        select: {
          name: true,
          kind: true,
          location: true,
          syntax: true,
          isExternal: true,
        },
      },
    );

    const externalVarDecls = new Map<
      string,
      {
        readonly declaration: DeclarationRef;
        readonly name: string | null;
        readonly location: DiagnosticLocation;
        readonly syntax: SyntaxRef;
      }
    >();
    for (const row of importDecls) {
      if (
        row.declaration !== null &&
        row.location !== null &&
        row.syntax !== null &&
        row.kind === "VariableDeclaration" &&
        isSourceFile(row.location.file) &&
        !seenDeclSpans.has(spanKey(row.location))
      ) {
        externalVarDecls.set(spanKey(row.location), {
          declaration: row.declaration,
          name: row.name,
          location: row.location,
          syntax: row.syntax,
        });
      }
    }

    await Promise.all(
      [...externalVarDecls.values()].map(async (ext) => {
        const loaded = await astStore.getNode(ext.syntax, ext.location);
        if (
          loaded === undefined ||
          loaded.node.kind !== "VariableDeclaration"
        ) {
          return;
        }
        const initNode = unwrapExpression(
          loaded.tree,
          resolveChild(loaded.tree, loaded.node.initializer),
        );
        if (initNode === undefined || initNode.kind !== "CallExpression") {
          return;
        }
        await tryAddTableFromVarOrCall(
          ext.location,
          ext.name ?? extractVarNameFromDeclText(loaded.node.text),
          initNode.ref,
          ext.location,
          ext.declaration,
        );
      }),
    );
  }

  const byDeclSpan = new Map<string, DrizzleTableColumnsResult>();
  const byTableName = new Map<string, DrizzleTableColumnsResult>();
  const bySqlName = new Map<string, DrizzleTableColumnsResult>();

  for (const tbl of tables) {
    byDeclSpan.set(spanKey(tbl.at), tbl);
    byTableName.set(tbl.tableName, tbl);
    bySqlName.set(tbl.sqlTableName, tbl);
  }

  return { tables, byDeclSpan, byTableName, bySqlName };
}

async function getTableIndex(ctx: ProjectContext): Promise<TableIndex> {
  const cache = getSnapshotCache(ctx.project);
  if (cache.tableIndexPromise === undefined) {
    cache.tableIndexPromise = computeTableIndex(ctx);
  }
  return cache.tableIndexPromise;
}

async function resolveExprToTable(
  ctx: ProjectContext,
  astStore: AstRangeStore,
  tableIndex: TableIndex,
  ref: SyntaxRef,
): Promise<DrizzleTableColumnsResult | undefined> {
  const [decl] = await ctx.resolveDeclarations([ref], {
    select: { name: true, kind: true, location: true, syntax: true },
  });
  if (
    decl === undefined ||
    decl.declaration === null ||
    decl.location === null
  ) {
    return undefined;
  }
  const existing = tableIndex.byDeclSpan.get(spanKey(decl.location));
  if (existing !== undefined) {
    return existing;
  }
  if (decl.syntax !== null && isSourceFile(decl.location.file)) {
    const loaded = await astStore.getNode(decl.syntax, decl.location);
    if (loaded?.node.kind === "VariableDeclaration") {
      const initNode = unwrapExpression(
        loaded.tree,
        resolveChild(loaded.tree, loaded.node.initializer),
      );
      if (initNode?.kind === "CallExpression") {
        return parseDrizzleTableFromCall(ctx, astStore, loaded.tree, initNode, {
          target: decl.declaration,
          ...(decl.name !== null ? { declaredName: decl.name } : {}),
          declarationLocation: decl.location,
        });
      }
    }
  }
  return undefined;
}

/**
 * Extracts Drizzle table column definitions (SQL column names, primary keys,
 * nullability, defaults, and cross-package provenance) from `drizzle-orm` tables.
 */
export async function extractDrizzleTableColumns(
  ctx: ProjectContext,
  options?: DrizzleTableColumnsOptions,
): Promise<readonly DrizzleTableColumnsResult[]> {
  ctx.recordGaps([]);
  const tableIndex = await getTableIndex(ctx);
  const astStore = new AstRangeStore(ctx);

  let results: readonly DrizzleTableColumnsResult[];
  if (options?.targets !== undefined) {
    const syntaxRows = await ctx.syntaxOf(options.targets, {
      select: { kind: true, location: true },
    });
    const matched: DrizzleTableColumnsResult[] = [];
    for (let i = 0; i < options.targets.length; i += 1) {
      const target = options.targets[i]!;
      const row = syntaxRows[i];
      if (row === undefined || row.syntax === null || row.location === null) {
        continue;
      }
      const direct = tableIndex.byDeclSpan.get(spanKey(row.location));
      let table: DrizzleTableColumnsResult | undefined = direct;
      if (table === undefined && row.kind !== "VariableDeclaration") {
        table = await resolveExprToTable(ctx, astStore, tableIndex, row.syntax);
      }
      if (table !== undefined) {
        if (
          options.tableName !== undefined &&
          table.tableName !== options.tableName
        ) {
          continue;
        }
        matched.push(
          buildDrizzleTableColumns({
            target,
            at: table.at,
            tableName: table.tableName,
            sqlTableName: table.sqlTableName,
            columns: table.columns,
            hasDynamicColumns: table.hasDynamicColumns,
            evidence: table.evidence,
            gaps: table.gaps,
          }),
        );
      }
    }
    results = matched;
  } else if (options?.tableName !== undefined) {
    results = tableIndex.tables.filter(
      (tbl) => tbl.tableName === options.tableName,
    );
  } else {
    results = tableIndex.tables;
  }

  const allGaps: Gap[] = [];
  for (const item of results) {
    allGaps.push(...item.gaps);
  }
  ctx.recordGaps(allGaps);
  return results;
}

async function computeRelationMap(
  ctx: ProjectContext,
  astStore: AstRangeStore,
  tableIndex: TableIndex,
): Promise<ReadonlyMap<string, ReadonlyMap<string, RelationEdge>>> {
  const relCalls = await ctx.findSyntax({
    kinds: ["CallExpression"],
    calleeNames: ["relations"],
  });
  const bySourceTable = new Map<string, Map<string, RelationEdge>>();
  if (relCalls.length === 0) {
    return bySourceTable;
  }

  const decls = await ctx.resolveDeclarations(
    relCalls.map((c) => c.ref),
    { select: { packageName: true, location: true } },
  );

  await Promise.all(
    relCalls.map(async (call, idx) => {
      if (!matchesPackage(decls[idx], DRIZZLE_ORM_PKG)) {
        return;
      }
      const loaded = await astStore.getNode(call.ref, call.location);
      if (loaded === undefined) {
        return;
      }
      const { tree, node } = loaded;
      const args = (node.arguments ?? [])
        .map((r) => unwrapExpression(tree, resolveChild(tree, r)))
        .filter((n): n is AstNode => n !== undefined);
      const sourceArg = args[0];
      const callbackArg = args[1];
      if (
        sourceArg === undefined ||
        callbackArg === undefined ||
        (callbackArg.kind !== "ArrowFunction" &&
          callbackArg.kind !== "FunctionExpression")
      ) {
        return;
      }
      const sourceTable = await resolveExprToTable(
        ctx,
        astStore,
        tableIndex,
        sourceArg.ref,
      );
      if (sourceTable === undefined) {
        return;
      }
      const obj = extractCallbackObjectReturn(tree, callbackArg);
      if (obj === undefined) {
        return;
      }
      let edgeMap = bySourceTable.get(sourceTable.tableName);
      if (edgeMap === undefined) {
        edgeMap = new Map();
        bySourceTable.set(sourceTable.tableName, edgeMap);
      }

      for (const prop of obj.children) {
        if (prop.kind !== "PropertyAssignment") {
          continue;
        }
        const relName = readStaticPropertyName(
          resolveChild(tree, prop.name) ?? prop.children[0],
        );
        const relInit = unwrapExpression(
          tree,
          resolveChild(tree, prop.initializer) ?? prop.children[1],
        );
        if (relName === undefined || relInit?.kind !== "CallExpression") {
          continue;
        }
        const { methodName } = inspectCallCallee(tree, relInit);
        if (methodName !== "one" && methodName !== "many") {
          continue;
        }
        const targetArg = unwrapExpression(
          tree,
          resolveChild(tree, relInit.arguments?.[0]),
        );
        if (targetArg === undefined) {
          continue;
        }
        const targetTable = await resolveExprToTable(
          ctx,
          astStore,
          tableIndex,
          targetArg.ref,
        );
        if (targetTable !== undefined) {
          edgeMap.set(relName, {
            name: relName,
            targetTable: targetTable.tableName,
            cardinality: methodName === "one" ? "single" : "many",
            at: prop.location,
          });
        }
      }
    }),
  );

  return bySourceTable;
}

async function resolveProjectionObject(
  ctx: ProjectContext,
  astStore: AstRangeStore,
  tableIndex: TableIndex,
  tree: AstTree,
  objNode: AstNode,
  fallbackTable: DrizzleTableColumnsResult | undefined,
): Promise<{
  readonly columns: Record<string, QueryRowColumnInfo>;
  readonly hasDynamicColumns: boolean;
  readonly gaps: Gap[];
  readonly evidence: Evidence[];
}> {
  const columns: Record<string, QueryRowColumnInfo> = {};
  const gaps: Gap[] = [];
  const evidence: Evidence[] = [];
  let hasDynamicColumns = false;

  for (const child of objNode.children) {
    if (child.kind === "PropertyAssignment") {
      const alias = readStaticPropertyName(
        resolveChild(tree, child.name) ?? child.children[0],
      );
      if (alias === undefined) {
        hasDynamicColumns = true;
        gaps.push({
          kind: "dynamic-query-projection",
          message: `Dynamic computed projection key: ${child.text.trim()}`,
          at: child.location,
        });
        continue;
      }
      const valNode = unwrapExpression(
        tree,
        resolveChild(tree, child.initializer) ?? child.children[1],
      );
      if (valNode?.kind === "PropertyAccessExpression") {
        const tblExpr = unwrapExpression(
          tree,
          resolveChild(tree, valNode.expression) ?? valNode.children[0],
        );
        const colProp = resolveChild(tree, valNode.name) ?? valNode.children[1];
        const colName = colProp?.text.trim();
        if (tblExpr !== undefined && colName !== undefined) {
          const tbl =
            (await resolveExprToTable(
              ctx,
              astStore,
              tableIndex,
              tblExpr.ref,
            )) ?? fallbackTable;
          const colInfo = tbl?.columns[colName];
          if (tbl !== undefined && colInfo !== undefined) {
            const projEv: Evidence = {
              kind: "source",
              message: `Projected column "${alias}" from ${tbl.tableName}.${colName}`,
              at: child.location,
            };
            const combinedEv = [...(colInfo.evidence ?? []), projEv];
            evidence.push(...combinedEv);
            columns[alias] = {
              name: alias,
              kind: "tableColumn",
              tableName: tbl.tableName,
              columnName: colName,
              sqlName: colInfo.sqlName,
              nullable: colInfo.nullable,
              hasDefault: colInfo.hasDefault,
              evidence: combinedEv,
            };
            continue;
          }
        }
      }

      const compEv: Evidence = {
        kind: "source",
        message: `Projected computed column "${alias}"`,
        at: child.location,
      };
      evidence.push(compEv);
      columns[alias] = {
        name: alias,
        kind: "computed",
        evidence: [compEv],
      };
    } else if (child.kind === "SpreadAssignment") {
      const spreadExpr = unwrapExpression(
        tree,
        resolveChild(tree, child.expression) ?? child.children[0],
      );
      if (spreadExpr?.kind === "CallExpression") {
        const { methodName } = inspectCallCallee(tree, spreadExpr);
        const arg0 = unwrapExpression(
          tree,
          resolveChild(tree, spreadExpr.arguments?.[0]),
        );
        if (methodName === "getTableColumns" && arg0 !== undefined) {
          const tbl = await resolveExprToTable(
            ctx,
            astStore,
            tableIndex,
            arg0.ref,
          );
          if (tbl !== undefined) {
            for (const [colName, col] of Object.entries(tbl.columns)) {
              columns[colName] = {
                name: colName,
                kind: "tableColumn",
                tableName: tbl.tableName,
                columnName: colName,
                sqlName: col.sqlName,
                nullable: col.nullable,
                hasDefault: col.hasDefault,
                ...(col.evidence !== undefined
                  ? { evidence: col.evidence }
                  : {}),
              };
            }
            evidence.push(...tbl.evidence);
            if (tbl.hasDynamicColumns) {
              hasDynamicColumns = true;
              gaps.push(...tbl.gaps);
            }
            continue;
          }
        }
      }
      hasDynamicColumns = true;
      gaps.push({
        kind: "dynamic-query-projection",
        message: `Dynamic spread in query projection: ${child.text.trim()}`,
        at: child.location,
      });
    }
  }

  return { columns, hasDynamicColumns, gaps, evidence };
}

function projectAllTableColumns(
  table: DrizzleTableColumnsResult,
  at: DiagnosticLocation,
): {
  readonly columns: Record<string, QueryRowColumnInfo>;
  readonly hasDynamicColumns: boolean;
  readonly gaps: Gap[];
  readonly evidence: Evidence[];
} {
  const columns: Record<string, QueryRowColumnInfo> = {};
  for (const [colName, col] of Object.entries(table.columns)) {
    columns[colName] = {
      name: colName,
      kind: "tableColumn",
      tableName: table.tableName,
      columnName: colName,
      sqlName: col.sqlName,
      nullable: col.nullable,
      hasDefault: col.hasDefault,
      ...(col.evidence !== undefined ? { evidence: col.evidence } : {}),
    };
  }
  return {
    columns,
    hasDynamicColumns: table.hasDynamicColumns,
    gaps: [...table.gaps],
    evidence: [
      ...table.evidence,
      {
        kind: "source",
        message: `Full table projection from "${table.tableName}"`,
        at,
      },
    ],
  };
}

function projectRelationalConfig(
  table: DrizzleTableColumnsResult,
  configObj: AstNode | undefined,
  tree: AstTree,
  tableIndex: TableIndex,
  relationMap: ReadonlyMap<string, ReadonlyMap<string, RelationEdge>>,
  visitedTables: ReadonlySet<string>,
): {
  readonly columns: Record<string, QueryRowColumnInfo>;
  readonly relations: Record<string, QueryRowRelationInfo>;
  readonly hasDynamicColumns: boolean;
  readonly gaps: Gap[];
  readonly evidence: Evidence[];
} {
  const gaps: Gap[] = [...table.gaps];
  const evidence: Evidence[] = [...table.evidence];
  let hasDynamicColumns = table.hasDynamicColumns;

  let columnsProp: AstNode | undefined;
  let withProp: AstNode | undefined;

  if (configObj !== undefined && configObj.kind === "ObjectLiteralExpression") {
    for (const child of configObj.children) {
      if (child.kind !== "PropertyAssignment") {
        continue;
      }
      const propName = readStaticPropertyName(
        resolveChild(tree, child.name) ?? child.children[0],
      );
      const valNode = unwrapExpression(
        tree,
        resolveChild(tree, child.initializer) ?? child.children[1],
      );
      if (propName === "columns") {
        columnsProp = valNode;
      } else if (propName === "with") {
        withProp = valNode;
      }
    }
  }

  let selectedColNames: readonly string[] = table.columnNames;
  if (columnsProp !== undefined) {
    if (columnsProp.kind !== "ObjectLiteralExpression") {
      hasDynamicColumns = true;
      gaps.push({
        kind: "dynamic-query-projection",
        message: `Dynamic relational columns config on "${table.tableName}"`,
        at: columnsProp.location,
      });
    } else {
      const included: string[] = [];
      const excluded = new Set<string>();
      for (const prop of columnsProp.children) {
        if (prop.kind !== "PropertyAssignment") {
          hasDynamicColumns = true;
          gaps.push({
            kind: "dynamic-query-projection",
            message: `Dynamic entry in relational columns config on "${table.tableName}"`,
            at: prop.location,
          });
          continue;
        }
        const key = readStaticPropertyName(
          resolveChild(tree, prop.name) ?? prop.children[0],
        );
        const val = unwrapExpression(
          tree,
          resolveChild(tree, prop.initializer) ?? prop.children[1],
        );
        if (key === undefined || val === undefined) {
          hasDynamicColumns = true;
          gaps.push({
            kind: "dynamic-query-projection",
            message: `Dynamic relational column selector on "${table.tableName}"`,
            at: prop.location,
          });
          continue;
        }
        if (val.kind === "TrueKeyword" || val.text.trim() === "true") {
          included.push(key);
        } else if (val.kind === "FalseKeyword" || val.text.trim() === "false") {
          excluded.add(key);
        } else {
          hasDynamicColumns = true;
          gaps.push({
            kind: "dynamic-query-projection",
            message: `Non-boolean column flag for "${key}" on "${table.tableName}"`,
            at: prop.location,
          });
        }
      }
      if (included.length > 0) {
        selectedColNames = included.filter((name) => name in table.columns);
      } else if (excluded.size > 0) {
        selectedColNames = table.columnNames.filter(
          (name) => !excluded.has(name),
        );
      }
    }
  }

  const columns: Record<string, QueryRowColumnInfo> = {};
  for (const colName of selectedColNames) {
    const col = table.columns[colName];
    if (col !== undefined) {
      columns[colName] = {
        name: colName,
        kind: "tableColumn",
        tableName: table.tableName,
        columnName: colName,
        sqlName: col.sqlName,
        nullable: col.nullable,
        hasDefault: col.hasDefault,
        ...(col.evidence !== undefined ? { evidence: col.evidence } : {}),
      };
    }
  }

  const relations: Record<string, QueryRowRelationInfo> = {};
  if (withProp !== undefined) {
    if (withProp.kind !== "ObjectLiteralExpression") {
      hasDynamicColumns = true;
      gaps.push({
        kind: "dynamic-query-projection",
        message: `Dynamic relational "with" option on "${table.tableName}"`,
        at: withProp.location,
      });
    } else {
      const tableEdges = relationMap.get(table.tableName);
      for (const prop of withProp.children) {
        if (prop.kind !== "PropertyAssignment") {
          hasDynamicColumns = true;
          gaps.push({
            kind: "dynamic-query-projection",
            message: `Dynamic relation entry in "with" on "${table.tableName}"`,
            at: prop.location,
          });
          continue;
        }
        const relName = readStaticPropertyName(
          resolveChild(tree, prop.name) ?? prop.children[0],
        );
        const relVal = unwrapExpression(
          tree,
          resolveChild(tree, prop.initializer) ?? prop.children[1],
        );
        if (relName === undefined || relVal === undefined) {
          continue;
        }
        if (relVal.kind === "FalseKeyword" || relVal.text.trim() === "false") {
          continue;
        }
        const edge = tableEdges?.get(relName);
        const targetTable =
          edge !== undefined
            ? tableIndex.byTableName.get(edge.targetTable)
            : undefined;
        if (edge === undefined || targetTable === undefined) {
          hasDynamicColumns = true;
          gaps.push({
            kind: "unresolved-drizzle-relation",
            message: `Unresolved Drizzle relation "${table.tableName}.${relName}"`,
            at: prop.location,
          });
          continue;
        }
        evidence.push({
          kind: "source",
          message: `Relation "${relName}" -> "${targetTable.tableName}" (${edge.cardinality})`,
          at: edge.at,
        });
        if (visitedTables.has(targetTable.tableName)) {
          relations[relName] = {
            name: relName,
            targetTable: targetTable.tableName,
            cardinality: edge.cardinality,
            columns: [],
            hasDynamicColumns: true,
            relations: {},
          };
          continue;
        }
        const nextVisited = new Set(visitedTables);
        nextVisited.add(targetTable.tableName);
        const nested = projectRelationalConfig(
          targetTable,
          relVal.kind === "ObjectLiteralExpression" ? relVal : undefined,
          tree,
          tableIndex,
          relationMap,
          nextVisited,
        );
        gaps.push(...nested.gaps);
        evidence.push(...nested.evidence);
        relations[relName] = {
          name: relName,
          targetTable: targetTable.tableName,
          cardinality: edge.cardinality,
          columns: sortStringKeys(Object.keys(nested.columns)),
          hasDynamicColumns: nested.hasDynamicColumns,
          relations: nested.relations,
          evidence: nested.evidence,
        };
      }
    }
  }

  return { columns, relations, hasDynamicColumns, gaps, evidence };
}

function findEnclosingQueryChainRoot(
  tree: AstTree,
  callNode: AstNode,
): AstNode {
  let current = callNode;
  while (
    current.parent?.kind === "PropertyAccessExpression" &&
    current.parent.parent?.kind === "CallExpression" &&
    current.parent.parent.expression?.handleId === current.parent.ref.handleId
  ) {
    const nextCall = current.parent.parent;
    const { methodName } = inspectCallCallee(tree, nextCall);
    if (methodName !== undefined && CHAIN_MODIFIER_METHODS.has(methodName)) {
      current = nextCall;
      continue;
    }
    break;
  }
  return current;
}

function walkChainFromOuter(
  tree: AstTree,
  outerCall: AstNode,
): readonly { readonly call: AstNode; readonly methodName: string }[] {
  const chain: { readonly call: AstNode; readonly methodName: string }[] = [];
  let current: AstNode | undefined = outerCall;
  while (current !== undefined && current.kind === "CallExpression") {
    const { methodName, receiverNode } = inspectCallCallee(tree, current);
    if (methodName === undefined) {
      break;
    }
    chain.push({ call: current, methodName });
    current = unwrapExpression(tree, receiverNode);
  }
  chain.reverse();
  return chain;
}

async function computeProjections(
  ctx: ProjectContext,
): Promise<readonly QueryRowProjectionResult[]> {
  const tableIndex = await getTableIndex(ctx);
  const astStore = new AstRangeStore(ctx);
  const relationMap = await computeRelationMap(ctx, astStore, tableIndex);

  const [selectCalls, returningCalls, relQueryCalls] = await Promise.all([
    ctx.findSyntax({
      kinds: ["CallExpression"],
      calleeNames: ["select", "selectDistinct", "selectDistinctOn"],
    }),
    ctx.findSyntax({
      kinds: ["CallExpression"],
      calleeNames: ["returning"],
    }),
    ctx.findSyntax({
      kinds: ["CallExpression"],
      calleeNames: ["findFirst", "findMany"],
    }),
  ]);

  const allStarts = [...selectCalls, ...returningCalls, ...relQueryCalls];
  if (allStarts.length === 0) {
    return [];
  }

  const decls = await ctx.resolveDeclarations(
    allStarts.map((c) => c.ref),
    { select: { packageName: true, location: true } },
  );

  const results: QueryRowProjectionResult[] = [];
  const seenRoots = new Set<string>();

  // Helper to fetch an enlarged range covering enclosing method chains in the same file
  const loadChainContext = async (
    call: SyntaxNodeResult,
  ): Promise<
    { readonly tree: AstTree; readonly node: AstNode } | undefined
  > => {
    const loaded = await astStore.getNode(call.ref, {
      file: call.location.file,
      start: Math.max(0, call.location.start - 512),
      end: call.location.end + 8192,
      match: "within",
    });
    if (loaded === undefined) {
      return undefined;
    }
    const exact = loaded.tree.byHandleId.get(call.ref.handleId);
    if (exact === undefined) {
      return undefined;
    }
    return { tree: loaded.tree, node: exact };
  };

  let offset = 0;

  // 1. select / selectDistinct / selectDistinctOn chains
  await Promise.all(
    selectCalls.map(async (call, idx) => {
      if (!matchesPackage(decls[offset + idx], DRIZZLE_ORM_PKG)) {
        return;
      }
      const loaded = await loadChainContext(call);
      if (loaded === undefined) {
        return;
      }
      const { tree, node: selectNode } = loaded;
      const rootNode = findEnclosingQueryChainRoot(tree, selectNode);
      const rootKey = spanKey(rootNode.location);
      if (seenRoots.has(rootKey)) {
        return;
      }
      seenRoots.add(rootKey);

      const chain = walkChainFromOuter(tree, rootNode);
      const selectStep = chain.find((s) =>
        SELECT_CALL_METHODS.has(s.methodName),
      );
      const fromStep = chain.find((s) => s.methodName === "from");
      if (selectStep === undefined) {
        return;
      }

      let fromTable: DrizzleTableColumnsResult | undefined;
      const fromArg =
        fromStep !== undefined
          ? unwrapExpression(
              tree,
              resolveChild(tree, fromStep.call.arguments?.[0]),
            )
          : undefined;
      if (fromArg !== undefined) {
        fromTable = await resolveExprToTable(
          ctx,
          astStore,
          tableIndex,
          fromArg.ref,
        );
      }

      // Also check joins for column lookup fallbacks
      for (const step of chain) {
        if (JOIN_METHODS.has(step.methodName)) {
          const joinArg = unwrapExpression(
            tree,
            resolveChild(tree, step.call.arguments?.[0]),
          );
          if (joinArg !== undefined && fromTable === undefined) {
            fromTable = await resolveExprToTable(
              ctx,
              astStore,
              tableIndex,
              joinArg.ref,
            );
          }
        }
      }

      const projArgIdx = selectStep.methodName === "selectDistinctOn" ? 1 : 0;
      const projArg = unwrapExpression(
        tree,
        resolveChild(tree, selectStep.call.arguments?.[projArgIdx]),
      );

      const queryEv: Evidence = {
        kind: "source",
        message: `Drizzle .${selectStep.methodName}() query`,
        at: rootNode.location,
      };

      if (projArg === undefined) {
        if (fromTable === undefined) {
          results.push(
            buildQueryRowProjection({
              library: PROVIDER_ID,
              queryKind: "select",
              target: rootNode.ref,
              at: rootNode.location,
              columns: {},
              relations: {},
              hasDynamicColumns: true,
              evidence: [queryEv],
              gaps: [
                {
                  kind: "unresolved-drizzle-table",
                  message:
                    "Could not resolve .from() table for Drizzle select()",
                  at: rootNode.location,
                },
              ],
            }),
          );
          return;
        }
        const full = projectAllTableColumns(fromTable, rootNode.location);
        results.push(
          buildQueryRowProjection({
            library: PROVIDER_ID,
            queryKind: "select",
            target: rootNode.ref,
            at: rootNode.location,
            tableName: fromTable.tableName,
            columns: full.columns,
            relations: {},
            hasDynamicColumns: full.hasDynamicColumns,
            evidence: [queryEv, ...full.evidence],
            gaps: full.gaps,
          }),
        );
        return;
      }

      if (projArg.kind === "ObjectLiteralExpression") {
        const proj = await resolveProjectionObject(
          ctx,
          astStore,
          tableIndex,
          tree,
          projArg,
          fromTable,
        );
        results.push(
          buildQueryRowProjection({
            library: PROVIDER_ID,
            queryKind: "select",
            target: rootNode.ref,
            at: rootNode.location,
            ...(fromTable !== undefined
              ? { tableName: fromTable.tableName }
              : {}),
            columns: proj.columns,
            relations: {},
            hasDynamicColumns: proj.hasDynamicColumns,
            evidence: [queryEv, ...proj.evidence],
            gaps: proj.gaps,
          }),
        );
        return;
      }

      if (projArg.kind === "CallExpression") {
        const { methodName } = inspectCallCallee(tree, projArg);
        const arg0 = unwrapExpression(
          tree,
          resolveChild(tree, projArg.arguments?.[0]),
        );
        if (methodName === "getTableColumns" && arg0 !== undefined) {
          const tbl = await resolveExprToTable(
            ctx,
            astStore,
            tableIndex,
            arg0.ref,
          );
          if (tbl !== undefined) {
            const full = projectAllTableColumns(tbl, rootNode.location);
            results.push(
              buildQueryRowProjection({
                library: PROVIDER_ID,
                queryKind: "select",
                target: rootNode.ref,
                at: rootNode.location,
                tableName: tbl.tableName,
                columns: full.columns,
                relations: {},
                hasDynamicColumns: full.hasDynamicColumns,
                evidence: [queryEv, ...full.evidence],
                gaps: full.gaps,
              }),
            );
            return;
          }
        }
      }

      results.push(
        buildQueryRowProjection({
          library: PROVIDER_ID,
          queryKind: "select",
          target: rootNode.ref,
          at: rootNode.location,
          ...(fromTable !== undefined
            ? { tableName: fromTable.tableName }
            : {}),
          columns: {},
          relations: {},
          hasDynamicColumns: true,
          evidence: [queryEv],
          gaps: [
            {
              kind: "dynamic-query-projection",
              message: `Dynamic Drizzle projection expression: ${projArg.text.trim()}`,
              at: projArg.location,
            },
          ],
        }),
      );
    }),
  );

  offset += selectCalls.length;

  // 2. insert / update / delete ...returning(...)
  await Promise.all(
    returningCalls.map(async (call, idx) => {
      if (!matchesPackage(decls[offset + idx], DRIZZLE_ORM_PKG)) {
        return;
      }
      const loaded = await loadChainContext(call);
      if (loaded === undefined) {
        return;
      }
      const { tree, node: retNode } = loaded;
      const rootNode = findEnclosingQueryChainRoot(tree, retNode);
      const rootKey = spanKey(rootNode.location);
      if (seenRoots.has(rootKey)) {
        return;
      }
      seenRoots.add(rootKey);

      const chain = walkChainFromOuter(tree, rootNode);
      const mutationStep = chain.find(
        (s) =>
          s.methodName === "insert" ||
          s.methodName === "update" ||
          s.methodName === "delete",
      );
      const tableArg =
        mutationStep !== undefined
          ? unwrapExpression(
              tree,
              resolveChild(tree, mutationStep.call.arguments?.[0]),
            )
          : undefined;
      const targetTable =
        tableArg !== undefined
          ? await resolveExprToTable(ctx, astStore, tableIndex, tableArg.ref)
          : undefined;

      const retArg = unwrapExpression(
        tree,
        resolveChild(tree, retNode.arguments?.[0]),
      );
      const queryEv: Evidence = {
        kind: "source",
        message: `Drizzle .returning() query`,
        at: rootNode.location,
      };

      if (retArg === undefined) {
        if (targetTable === undefined) {
          results.push(
            buildQueryRowProjection({
              library: PROVIDER_ID,
              queryKind: "returning",
              target: rootNode.ref,
              at: rootNode.location,
              columns: {},
              relations: {},
              hasDynamicColumns: true,
              evidence: [queryEv],
              gaps: [
                {
                  kind: "unresolved-drizzle-table",
                  message: "Could not resolve target table for .returning()",
                  at: rootNode.location,
                },
              ],
            }),
          );
          return;
        }
        const full = projectAllTableColumns(targetTable, rootNode.location);
        results.push(
          buildQueryRowProjection({
            library: PROVIDER_ID,
            queryKind: "returning",
            target: rootNode.ref,
            at: rootNode.location,
            tableName: targetTable.tableName,
            columns: full.columns,
            relations: {},
            hasDynamicColumns: full.hasDynamicColumns,
            evidence: [queryEv, ...full.evidence],
            gaps: full.gaps,
          }),
        );
        return;
      }

      if (retArg.kind === "ObjectLiteralExpression") {
        const proj = await resolveProjectionObject(
          ctx,
          astStore,
          tableIndex,
          tree,
          retArg,
          targetTable,
        );
        results.push(
          buildQueryRowProjection({
            library: PROVIDER_ID,
            queryKind: "returning",
            target: rootNode.ref,
            at: rootNode.location,
            ...(targetTable !== undefined
              ? { tableName: targetTable.tableName }
              : {}),
            columns: proj.columns,
            relations: {},
            hasDynamicColumns: proj.hasDynamicColumns,
            evidence: [queryEv, ...proj.evidence],
            gaps: proj.gaps,
          }),
        );
        return;
      }

      results.push(
        buildQueryRowProjection({
          library: PROVIDER_ID,
          queryKind: "returning",
          target: rootNode.ref,
          at: rootNode.location,
          ...(targetTable !== undefined
            ? { tableName: targetTable.tableName }
            : {}),
          columns: {},
          relations: {},
          hasDynamicColumns: true,
          evidence: [queryEv],
          gaps: [
            {
              kind: "dynamic-query-projection",
              message: `Dynamic .returning() projection: ${retArg.text.trim()}`,
              at: retArg.location,
            },
          ],
        }),
      );
    }),
  );

  offset += returningCalls.length;

  // 3. Relational queries: db.query.<table>.findFirst(...) / findMany(...)
  await Promise.all(
    relQueryCalls.map(async (call, idx) => {
      if (!matchesPackage(decls[offset + idx], DRIZZLE_ORM_PKG)) {
        return;
      }
      const loaded = await astStore.getNode(call.ref, call.location);
      if (loaded === undefined) {
        return;
      }
      const { tree, node: relCallNode } = loaded;
      const { methodName, receiverNode } = inspectCallCallee(tree, relCallNode);
      const recvUnwrapped = unwrapExpression(tree, receiverNode);
      if (
        recvUnwrapped === undefined ||
        recvUnwrapped.kind !== "PropertyAccessExpression"
      ) {
        return;
      }
      const queryContainer = unwrapExpression(
        tree,
        resolveChild(tree, recvUnwrapped.expression) ??
          recvUnwrapped.children[0],
      );
      const tableProp =
        resolveChild(tree, recvUnwrapped.name) ?? recvUnwrapped.children[1];
      const tableKey = tableProp?.text.trim();
      if (queryContainer === undefined || tableKey === undefined) {
        return;
      }
      // Verify `...query.<tableKey>` pattern
      const isQueryProp =
        queryContainer.kind === "PropertyAccessExpression" &&
        (
          resolveChild(tree, queryContainer.name) ?? queryContainer.children[1]
        )?.text.trim() === "query";
      if (!isQueryProp) {
        return;
      }

      const sourceTable =
        tableIndex.byTableName.get(tableKey) ??
        tableIndex.bySqlName.get(tableKey);
      const queryEv: Evidence = {
        kind: "source",
        message: `Drizzle relational .${methodName ?? "find"}() query on "${tableKey}"`,
        at: relCallNode.location,
      };
      if (sourceTable === undefined) {
        results.push(
          buildQueryRowProjection({
            library: PROVIDER_ID,
            queryKind: "relational",
            target: relCallNode.ref,
            at: relCallNode.location,
            tableName: tableKey,
            columns: {},
            relations: {},
            hasDynamicColumns: true,
            evidence: [queryEv],
            gaps: [
              {
                kind: "unresolved-drizzle-table",
                message: `Could not resolve relational query table "${tableKey}"`,
                at: relCallNode.location,
              },
            ],
          }),
        );
        return;
      }

      const configArg = unwrapExpression(
        tree,
        resolveChild(tree, relCallNode.arguments?.[0]),
      );
      const projected = projectRelationalConfig(
        sourceTable,
        configArg,
        tree,
        tableIndex,
        relationMap,
        new Set([sourceTable.tableName]),
      );
      results.push(
        buildQueryRowProjection({
          library: PROVIDER_ID,
          queryKind: "relational",
          target: relCallNode.ref,
          at: relCallNode.location,
          tableName: sourceTable.tableName,
          columns: projected.columns,
          relations: projected.relations,
          hasDynamicColumns: projected.hasDynamicColumns,
          evidence: [queryEv, ...projected.evidence],
          gaps: projected.gaps,
        }),
      );
    }),
  );

  return results;
}

/**
 * Extracts Drizzle row projection shapes across `select`, `selectDistinct`,
 * `selectDistinctOn`, `.returning()`, and relational `db.query.<table>.findFirst`/`findMany` calls.
 */
export async function extractDrizzleRowProjections(
  ctx: ProjectContext,
  options?: QueryRowProjectionOptions,
): Promise<readonly QueryRowProjectionResult[]> {
  ctx.recordGaps([]);
  if (options?.library !== undefined && options.library !== PROVIDER_ID) {
    return [];
  }
  const cache = getSnapshotCache(ctx.project);
  if (cache.projectionsPromise === undefined) {
    cache.projectionsPromise = computeProjections(ctx);
  }
  const all = await cache.projectionsPromise;
  let results = all;
  if (options?.targets !== undefined) {
    const wanted = new Set(options.targets.map((t) => t.handleId));
    results = all.filter((item) => wanted.has(item.target.handleId));
  }

  const allGaps: Gap[] = [];
  for (const item of results) {
    allGaps.push(...item.gaps);
  }
  ctx.recordGaps(allGaps);
  return results;
}

/** Standard `QueryRowProjectionExtractor` implementation backed by `@tssae/drizzle`. */
export const drizzleQueryProjection: QueryRowProjectionExtractor = {
  id: PROVIDER_ID,
  extractRowProjections: extractDrizzleRowProjections,
};

async function verifyCallIsDrizzleZodCreateSelectSchema(
  ctx: ProjectContext,
  astStore: AstRangeStore,
  tree: AstTree,
  callNode: AstNode,
): Promise<boolean> {
  const { calleeNode, methodName, receiverNode } = inspectCallCallee(
    tree,
    callNode,
  );
  if (calleeNode === undefined) {
    return false;
  }

  // Case A: `createSelectSchema(...)` / `aliasedSelect(...)` / `barrelSelect(...)` / `destructuredSelect(...)`
  if (calleeNode.kind === "Identifier") {
    const [decl] = await ctx.resolveDeclarations([calleeNode.ref], {
      select: {
        packageName: true,
        exportName: true,
        name: true,
        kind: true,
        location: true,
        syntax: true,
      },
    });
    if (decl === undefined || decl.declaration === null) {
      return false;
    }
    const declKind: string | null = decl.kind;
    if (matchesPackage(decl, DRIZZLE_ZOD_PKG)) {
      return (
        decl.exportName === "createSelectSchema" ||
        decl.name === "createSelectSchema" ||
        declKind === "BindingElement"
      );
    }
    if (
      declKind === "BindingElement" &&
      decl.location !== null &&
      decl.syntax !== null &&
      isSourceFile(decl.location.file)
    ) {
      const loaded = await astStore.getNode(decl.syntax, {
        file: decl.location.file,
        start: Math.max(0, decl.location.start - 128),
        end: decl.location.end + 512,
        match: "within",
      });
      const binding =
        loaded?.tree.byHandleId.get(decl.syntax.handleId) ?? loaded?.node;
      const objPattern = binding?.parent;
      const varDecl =
        objPattern?.kind === "ObjectBindingPattern"
          ? objPattern.parent
          : undefined;
      if (varDecl?.kind === "VariableDeclaration" && loaded !== undefined) {
        const initNode = unwrapExpression(
          loaded.tree,
          resolveChild(loaded.tree, varDecl.initializer),
        );
        if (initNode?.kind === "CallExpression") {
          return verifyCreateSchemaFactoryCall(ctx, loaded.tree, initNode);
        }
      }
    }
    return false;
  }

  // Case B: `dz.createSelectSchema(...)` / `createSchemaFactory().createSelectSchema(...)` / `factory.createSelectSchema(...)`
  if (
    calleeNode.kind === "PropertyAccessExpression" &&
    methodName === "createSelectSchema" &&
    receiverNode !== undefined
  ) {
    const recvUnwrapped = unwrapExpression(tree, receiverNode);
    if (recvUnwrapped === undefined) {
      return false;
    }
    if (recvUnwrapped.kind === "CallExpression") {
      return verifyCreateSchemaFactoryCall(ctx, tree, recvUnwrapped);
    }
    if (recvUnwrapped.kind === "Identifier") {
      const [recvDecl] = await ctx.resolveDeclarations([recvUnwrapped.ref], {
        select: {
          packageName: true,
          kind: true,
          location: true,
          syntax: true,
        },
      });
      if (recvDecl === undefined || recvDecl.declaration === null) {
        return false;
      }
      // Namespace or direct module import (`import * as dz from "drizzle-zod"`)
      const recvKind: string | null = recvDecl.kind;
      if (recvKind !== "VariableDeclaration") {
        return matchesPackage(recvDecl, DRIZZLE_ZOD_PKG);
      }
      if (
        recvDecl.kind === "VariableDeclaration" &&
        recvDecl.location !== null &&
        recvDecl.syntax !== null &&
        isSourceFile(recvDecl.location.file)
      ) {
        const loaded = await astStore.getNode(
          recvDecl.syntax,
          recvDecl.location,
        );
        if (loaded === undefined) {
          return false;
        }
        // IMPORTANT: Do NOT use unwrapExpression before checking if it was `{} as ...`;
        // checking unwrapped initializer distinguishes real `createSchemaFactory()` vs `{}`!
        const initNode = unwrapExpression(
          loaded.tree,
          resolveChild(loaded.tree, loaded.node.initializer),
        );
        if (initNode?.kind === "CallExpression") {
          return verifyCreateSchemaFactoryCall(ctx, loaded.tree, initNode);
        }
        return false;
      }
    }
  }

  return false;
}

async function verifyCreateSchemaFactoryCall(
  ctx: ProjectContext,
  tree: AstTree,
  callNode: AstNode,
): Promise<boolean> {
  const { calleeNode } = inspectCallCallee(tree, callNode);
  if (calleeNode === undefined) {
    return false;
  }
  const [decl] = await ctx.resolveDeclarations([calleeNode.ref], {
    select: { packageName: true, exportName: true, name: true, location: true },
  });
  return (
    matchesPackage(decl, DRIZZLE_ZOD_PKG) &&
    (decl?.exportName === "createSchemaFactory" ||
      decl?.name === "createSchemaFactory")
  );
}

async function evaluateDrizzleZodCallNode(
  ctx: ProjectContext,
  astStore: AstRangeStore,
  tableIndex: TableIndex,
  tree: AstTree,
  callNode: AstNode,
  refineFields: boolean,
  schemaName?: string,
): Promise<DrizzleZodSchemaResult | undefined> {
  const isReal = await verifyCallIsDrizzleZodCreateSelectSchema(
    ctx,
    astStore,
    tree,
    callNode,
  );
  if (!isReal) {
    return undefined;
  }

  const args = (callNode.arguments ?? [])
    .map((r) => unwrapExpression(tree, resolveChild(tree, r)))
    .filter((n): n is AstNode => n !== undefined);
  const tableArg = args[0];
  const refineArg = args[1];

  const table =
    tableArg !== undefined
      ? await resolveExprToTable(ctx, astStore, tableIndex, tableArg.ref)
      : undefined;

  const evidence: Evidence[] = [
    {
      kind: "source",
      message: `drizzle-zod createSelectSchema(${tableArg?.text.trim() ?? ""})`,
      at: callNode.location,
    },
  ];
  const gaps: Gap[] = [];
  let hasDynamicFields = false;

  const fields: Record<string, DrizzleZodFieldInfo> = {};
  if (table !== undefined) {
    evidence.push(...table.evidence);
    gaps.push(...table.gaps);
    if (table.hasDynamicColumns) {
      hasDynamicFields = true;
    }
    for (const [colName, col] of Object.entries(table.columns)) {
      fields[colName] = {
        name: colName,
        resolved: true,
        requirement: "required",
        nullable: col.nullable,
        ...(col.evidence !== undefined ? { evidence: col.evidence } : {}),
      };
    }
  } else {
    hasDynamicFields = true;
    gaps.push({
      kind: "unresolved-drizzle-table",
      message: `Could not resolve Drizzle table in createSelectSchema(${tableArg?.text.trim() ?? ""})`,
      at: tableArg?.location ?? callNode.location,
    });
  }

  if (refineArg !== undefined) {
    if (refineArg.kind !== "ObjectLiteralExpression") {
      hasDynamicFields = true;
      gaps.push({
        kind: "dynamic-schema-shape",
        message: `Dynamic refinement argument in createSelectSchema()`,
        at: refineArg.location,
      });
    } else {
      const refinedEntries: {
        readonly colName: string;
        readonly prop: AstNode;
        readonly initNode: AstNode;
      }[] = [];
      for (const prop of refineArg.children) {
        if (prop.kind === "PropertyAssignment") {
          const colName = readStaticPropertyName(
            resolveChild(tree, prop.name) ?? prop.children[0],
          );
          if (colName === undefined) {
            hasDynamicFields = true;
            gaps.push({
              kind: "dynamic-schema-field",
              message: "Computed field key in createSelectSchema refinement",
              at: prop.location,
            });
            continue;
          }
          const initNode =
            unwrapExpression(tree, resolveChild(tree, prop.initializer)) ??
            unwrapExpression(tree, prop.children[1]);
          if (initNode === undefined) {
            continue;
          }
          refinedEntries.push({ colName, prop, initNode });
        } else if (prop.kind === "SpreadAssignment") {
          hasDynamicFields = true;
          gaps.push({
            kind: "dynamic-schema-spread",
            message: `Dynamic spread in createSelectSchema refinement: ${prop.text.trim()}`,
            at: prop.location,
          });
        }
      }

      if (refinedEntries.length > 0) {
        let modifiers: readonly ValidatorFieldModifierResult[] = [];
        if (refineFields) {
          modifiers = await extractZodFieldModifiers(
            ctx,
            refinedEntries.map((e) => e.initNode.ref),
          );
        }

        for (let i = 0; i < refinedEntries.length; i += 1) {
          const { colName, prop } = refinedEntries[i]!;
          const baseCol = fields[colName];
          if (!refineFields) {
            hasDynamicFields = true;
            const gap: Gap = {
              kind: "unresolved-schema-field",
              message: `Refined field "${colName}" requires @tssae/zod refinement resolution`,
              at: prop.location,
            };
            gaps.push(gap);
            fields[colName] = {
              name: colName,
              resolved: false,
              ...(baseCol?.evidence !== undefined
                ? { evidence: baseCol.evidence }
                : {}),
              gaps: [gap],
            };
            continue;
          }

          const mod = modifiers[i];
          if (
            mod !== undefined &&
            mod.resolved &&
            mod.requirement !== undefined
          ) {
            const nullable =
              mod.mode === "callback" && !mod.nullabilityExplicit
                ? (baseCol?.nullable ?? false)
                : (mod.nullable ?? false);
            const fieldEv = [...(baseCol?.evidence ?? []), ...mod.evidence];
            evidence.push(...mod.evidence);
            gaps.push(...mod.gaps);
            if (mod.gaps.length > 0) {
              hasDynamicFields = true;
            }
            fields[colName] = {
              name: colName,
              resolved: true,
              requirement: mod.requirement,
              nullable,
              evidence: fieldEv,
              ...(mod.gaps.length > 0 ? { gaps: mod.gaps } : {}),
            };
          } else {
            hasDynamicFields = true;
            const fieldGaps: readonly Gap[] =
              mod !== undefined && mod.gaps.length > 0
                ? mod.gaps
                : [
                    {
                      kind: "unresolved-schema-field",
                      message: `Unresolved refinement for field "${colName}"`,
                      at: prop.location,
                    },
                  ];
            gaps.push(...fieldGaps);
            fields[colName] = {
              name: colName,
              resolved: false,
              ...(baseCol?.evidence !== undefined
                ? { evidence: baseCol.evidence }
                : {}),
              gaps: fieldGaps,
            };
          }
        }
      }
    }
  }

  return buildDrizzleZodSchema({
    target: callNode.ref,
    at: callNode.location,
    ...(schemaName !== undefined ? { schemaName } : {}),
    ...(table !== undefined ? { tableName: table.tableName } : {}),
    fields,
    hasDynamicFields,
    evidence,
    gaps,
  });
}

async function computeDrizzleZodSchemas(
  ctx: ProjectContext,
  refineFields: boolean,
): Promise<readonly DrizzleZodSchemaResult[]> {
  const tableIndex = await getTableIndex(ctx);
  const astStore = new AstRangeStore(ctx);

  const [allCalls, varDecls] = await Promise.all([
    ctx.findSyntax({ kinds: ["CallExpression"] }),
    ctx.findSyntax({ kinds: ["VariableDeclaration"] }),
  ]);
  if (allCalls.length === 0) {
    return [];
  }

  const varByInitHandle = new Map<string, SyntaxNodeResult>();
  for (const decl of varDecls) {
    if (decl.initializer !== undefined) {
      varByInitHandle.set(decl.initializer.handleId, decl);
    }
  }

  const callDecls = await ctx.resolveDeclarations(
    allCalls.map((c) => c.ref),
    { select: { packageName: true, location: true, kind: true } },
  );

  const candidateCalls = allCalls.filter((_, idx) => {
    const decl = callDecls[idx];
    if (decl === undefined) {
      return false;
    }
    const kindStr: string | null = decl.kind;
    return (
      matchesPackage(decl, DRIZZLE_ZOD_PKG) || kindStr === "BindingElement"
    );
  });
  if (candidateCalls.length === 0) {
    return [];
  }

  const results: DrizzleZodSchemaResult[] = [];
  for (const call of candidateCalls) {
    const loaded = await astStore.getNode(call.ref, call.location);
    if (loaded === undefined) {
      continue;
    }
    const varDecl = varByInitHandle.get(call.ref.handleId);
    const schemaName =
      varDecl !== undefined
        ? extractVarNameFromDeclText(varDecl.text)
        : undefined;
    const schema = await evaluateDrizzleZodCallNode(
      ctx,
      astStore,
      tableIndex,
      loaded.tree,
      loaded.node,
      refineFields,
      schemaName,
    );
    if (schema !== undefined) {
      results.push(schema);
    }
  }

  return results;
}

const onDemandDrizzleZodCache = new WeakMap<
  ProjectSummary,
  Map<string, Promise<DrizzleZodSchemaResult | undefined>>
>();

async function resolveOnDemandDrizzleZodCall(
  ctx: ProjectContext,
  callSyntax: SyntaxRef,
  refineFields: boolean,
): Promise<DrizzleZodSchemaResult | undefined> {
  let perCall = onDemandDrizzleZodCache.get(ctx.project);
  if (perCall === undefined) {
    perCall = new Map();
    onDemandDrizzleZodCache.set(ctx.project, perCall);
  }
  const cacheKey = `${String(refineFields)}:${callSyntax.handleId}`;
  const existing = perCall.get(cacheKey);
  if (existing !== undefined) {
    return existing;
  }

  const pending = (async (): Promise<DrizzleZodSchemaResult | undefined> => {
    const [row] = await ctx.syntaxOf([callSyntax], {
      select: { kind: true, location: true },
    });
    if (
      row === undefined ||
      row.syntax === null ||
      row.location === null ||
      row.kind !== "CallExpression"
    ) {
      return undefined;
    }
    const [decl] = await ctx.resolveDeclarations([row.syntax], {
      select: { packageName: true, location: true, kind: true },
    });
    const declKind: string | null = decl?.kind ?? null;
    if (
      !matchesPackage(decl, DRIZZLE_ZOD_PKG) &&
      declKind !== "BindingElement"
    ) {
      return undefined;
    }
    const astStore = new AstRangeStore(ctx);
    const loaded = await astStore.getNode(row.syntax, row.location);
    if (loaded === undefined) {
      return undefined;
    }
    const tableIndex = await getTableIndex(ctx);
    return evaluateDrizzleZodCallNode(
      ctx,
      astStore,
      tableIndex,
      loaded.tree,
      loaded.node,
      refineFields,
    );
  })();

  perCall.set(cacheKey, pending);
  return pending;
}

/**
 * Extracts schemas created by verified `drizzle-zod` calls (`createSelectSchema`,
 * `createSchemaFactory().createSelectSchema`), resolving underlying Drizzle tables
 * and second-argument callback/direct refinements via `@tssae/zod`.
 */
export async function extractDrizzleZodSchemas(
  ctx: ProjectContext,
  options?: DrizzleZodQueryOptions,
): Promise<readonly DrizzleZodSchemaResult[]> {
  ctx.recordGaps([]);
  const refineFields = options?.refineFields ?? true;
  const cache = getSnapshotCache(ctx.project);
  let pending = cache.drizzleZodPromises.get(refineFields);
  if (pending === undefined) {
    pending = computeDrizzleZodSchemas(ctx, refineFields);
    cache.drizzleZodPromises.set(refineFields, pending);
  }
  const all = await pending;
  let results: readonly DrizzleZodSchemaResult[];
  if (options?.targets !== undefined) {
    const byHandle = new Map(all.map((item) => [item.target.handleId, item]));
    const matched: DrizzleZodSchemaResult[] = [];
    for (const target of options.targets) {
      const hit =
        byHandle.get(target.handleId) ??
        (await resolveOnDemandDrizzleZodCall(ctx, target, refineFields));
      if (hit !== undefined) {
        matched.push(hit);
      }
    }
    results = matched;
  } else {
    results = all;
  }

  const allGaps: Gap[] = [];
  for (const item of results) {
    allGaps.push(...item.gaps);
  }
  ctx.recordGaps(allGaps);
  return results;
}

function toExternalSchemaResolution(
  schema: DrizzleZodSchemaResult,
): ExternalSchemaResolution {
  const fields: Record<string, ValidatorFieldInfo> = {};
  const unresolvedFields: Record<string, readonly Gap[]> = {};
  for (const [name, info] of Object.entries(schema.fields)) {
    if (info.resolved && info.requirement !== undefined) {
      fields[name] = {
        name,
        requirement: info.requirement,
        nullable: info.nullable ?? false,
        ...(info.evidence !== undefined ? { evidence: info.evidence } : {}),
      };
    } else if (info.gaps !== undefined && info.gaps.length > 0) {
      unresolvedFields[name] = info.gaps;
    }
  }
  return {
    fields,
    hasDynamicFields: schema.hasDynamicFields,
    ...(Object.keys(unresolvedFields).length > 0 ? { unresolvedFields } : {}),
    evidence: schema.evidence,
    gaps: schema.gaps,
  };
}

/**
 * `@tssae/zod` `ExternalSchemaResolver` hook backed by `@tssae/drizzle`'s
 * `drizzle-zod` schema extractor.
 */
export const drizzleZodSchemaResolver: ExternalSchemaResolver = {
  id: "drizzle-zod",
  resolveSchemaCall: async (ctx, callSyntax) => {
    const schemas = await extractDrizzleZodSchemas(ctx, { refineFields: true });
    const match =
      schemas.find((s) => s.target.handleId === callSyntax.handleId) ??
      (await resolveOnDemandDrizzleZodCall(ctx, callSyntax, true));
    return match !== undefined ? toExternalSchemaResolution(match) : undefined;
  },
};

function mergeExternalResolvers(
  options?: ValidatorQueryOptions,
): ValidatorQueryOptions {
  const existing = options?.externalSchemaResolvers ?? [];
  const hasDrizzleZod = existing.some(
    (r) => r.id === drizzleZodSchemaResolver.id,
  );
  return {
    ...options,
    externalSchemaResolvers: hasDrizzleZod
      ? existing
      : [drizzleZodSchemaResolver, ...existing],
  };
}

/**
 * Convenience `@tssae/zod` + `@tssae/drizzle` validator extractor that automatically
 * attaches `drizzleZodSchemaResolver` when querying validator input/output fields.
 */
export const drizzleZodValidator: ValidatorFieldExtractor = {
  id: "drizzle-zod",
  extractInputFields: (ctx, options) =>
    extractZodInputFields(ctx, mergeExternalResolvers(options)),
  extractOutputFields: (ctx, options) =>
    extractZodOutputFields(ctx, mergeExternalResolvers(options)),
};
