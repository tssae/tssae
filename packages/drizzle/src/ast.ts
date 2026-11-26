import type {
  DiagnosticLocation,
  ProjectContext,
  SourceRange,
  SyntaxNodeResult,
  SyntaxRef,
} from "tssae";

/** Reconstructed AST node with parent/child links built from `findSyntax({ range })`. */
export type AstNode = {
  readonly ref: SyntaxRef;
  readonly kind: string;
  readonly text: string;
  readonly location: DiagnosticLocation;
  readonly expression?: SyntaxRef;
  readonly arguments?: readonly SyntaxRef[];
  readonly name?: SyntaxRef;
  readonly initializer?: SyntaxRef;
  readonly body?: SyntaxRef;
  readonly parameters?: readonly SyntaxRef[];
  readonly children: AstNode[];
  parent?: AstNode;
};

/** Pre-indexed AST tree for a fetched source range. */
export type AstTree = {
  readonly root: AstNode;
  readonly byHandleId: ReadonlyMap<string, AstNode>;
  readonly nodes: readonly AstNode[];
};

const WRAPPER_SYNTAX_KINDS = new Set([
  "ParenthesizedExpression",
  "AsExpression",
  "SatisfiesExpression",
  "TypeAssertionExpression",
  "NonNullExpression",
]);

function locationKey(loc: {
  readonly file: string;
  readonly start: number;
  readonly end: number;
}): string {
  return `${loc.file}:${String(loc.start)}:${String(loc.end)}`;
}

/** Reconstructs an AST tree from flat `findSyntax({ range })` output. */
export function buildAstTree(
  rawNodes: readonly SyntaxNodeResult[],
): AstTree | undefined {
  if (rawNodes.length === 0) {
    return undefined;
  }

  const sorted = [...rawNodes].sort((a, b) => {
    if (a.location.start !== b.location.start) {
      return a.location.start - b.location.start;
    }
    return b.location.end - a.location.end;
  });

  const nodes: AstNode[] = sorted.map((raw) => ({
    ref: raw.ref,
    kind: raw.kind,
    text: raw.text,
    location: raw.location,
    ...(raw.expression !== undefined ? { expression: raw.expression } : {}),
    ...(raw.arguments !== undefined ? { arguments: raw.arguments } : {}),
    ...(raw.name !== undefined ? { name: raw.name } : {}),
    ...(raw.initializer !== undefined ? { initializer: raw.initializer } : {}),
    ...(raw.body !== undefined ? { body: raw.body } : {}),
    ...(raw.parameters !== undefined ? { parameters: raw.parameters } : {}),
    children: [],
  }));

  const byHandleId = new Map<string, AstNode>();
  const stack: AstNode[] = [];

  for (const node of nodes) {
    byHandleId.set(node.ref.handleId, node);
    while (stack.length > 0) {
      const top = stack[stack.length - 1]!;
      if (
        node.location.start >= top.location.start &&
        node.location.end <= top.location.end
      ) {
        break;
      }
      stack.pop();
    }
    const parent = stack[stack.length - 1];
    if (parent !== undefined) {
      node.parent = parent;
      parent.children.push(node);
    }
    stack.push(node);
  }

  return {
    root: nodes[0]!,
    byHandleId,
    nodes,
  };
}

/** Snapshot-scoped AST range fetcher with per-range deduplication. */
export class AstRangeStore {
  readonly #ctx: ProjectContext;
  readonly #cache = new Map<string, Promise<AstTree | undefined>>();

  constructor(ctx: ProjectContext) {
    this.#ctx = ctx;
  }

  async getTree(
    range: Pick<DiagnosticLocation, "file" | "start" | "end"> &
      Partial<Pick<SourceRange, "match">>,
  ): Promise<AstTree | undefined> {
    const key = locationKey(range);
    const existing = this.#cache.get(key);
    if (existing !== undefined) {
      return existing;
    }
    const pending = this.#ctx
      .findSyntax({
        range: {
          file: range.file,
          start: range.start,
          end: range.end,
          match: range.match ?? "within",
        },
      })
      .then((nodes) => buildAstTree(nodes));
    this.#cache.set(key, pending);
    return pending;
  }

  async getNode(
    ref: SyntaxRef,
    range: Pick<DiagnosticLocation, "file" | "start" | "end"> &
      Partial<Pick<SourceRange, "match">>,
  ): Promise<
    | {
        readonly tree: AstTree;
        readonly node: AstNode;
      }
    | undefined
  > {
    const tree = await this.getTree(range);
    if (tree === undefined) {
      return undefined;
    }
    const node =
      tree.byHandleId.get(ref.handleId) ??
      tree.nodes.find(
        (n) => n.location.start === range.start && n.location.end === range.end,
      );
    if (node === undefined) {
      return undefined;
    }
    return { tree, node };
  }
}

export function resolveChild(
  tree: AstTree,
  ref: SyntaxRef | undefined,
): AstNode | undefined {
  if (ref === undefined) {
    return undefined;
  }
  return tree.byHandleId.get(ref.handleId);
}

/** Strips transparent TypeScript expression wrappers (`(...)`, `as`, `satisfies`, `!`). */
export function unwrapExpression(
  tree: AstTree,
  node: AstNode | undefined,
): AstNode | undefined {
  let current = node;
  while (current !== undefined && WRAPPER_SYNTAX_KINDS.has(current.kind)) {
    const inner = resolveChild(tree, current.expression) ?? current.children[0];
    if (inner === undefined || inner === current) {
      break;
    }
    current = inner;
  }
  return current;
}

/** Extracts unquoted string content from a StringLiteral or NoSubstitutionTemplateLiteral node. */
export function readStringLiteral(
  node: AstNode | undefined,
): string | undefined {
  if (node === undefined) {
    return undefined;
  }
  if (
    node.kind === "StringLiteral" ||
    node.kind === "NoSubstitutionTemplateLiteral"
  ) {
    const raw = node.text.trim();
    if (raw.length >= 2) {
      const quote = raw[0];
      if (
        (quote === '"' || quote === "'" || quote === "`") &&
        raw.endsWith(quote)
      ) {
        return raw.slice(1, -1);
      }
    }
  }
  return undefined;
}

/** Extracts a static property key name from an Identifier or string/template literal node. */
export function readStaticPropertyName(
  nameNode: AstNode | undefined,
): string | undefined {
  if (nameNode === undefined) {
    return undefined;
  }
  if (nameNode.kind === "Identifier") {
    return nameNode.text.trim();
  }
  const str = readStringLiteral(nameNode);
  if (str !== undefined) {
    return str;
  }
  if (nameNode.kind === "ComputedPropertyName") {
    return readStringLiteral(nameNode.children[0]);
  }
  return undefined;
}

/** Extracts callee method/function name and receiver from a CallExpression node. */
export function inspectCallCallee(
  tree: AstTree,
  callNode: AstNode,
): {
  readonly calleeNode: AstNode | undefined;
  readonly methodName: string | undefined;
  readonly receiverNode: AstNode | undefined;
} {
  const calleeNode = unwrapExpression(
    tree,
    resolveChild(tree, callNode.expression),
  );
  if (calleeNode === undefined) {
    return {
      calleeNode: undefined,
      methodName: undefined,
      receiverNode: undefined,
    };
  }
  if (calleeNode.kind === "Identifier") {
    return {
      calleeNode,
      methodName: calleeNode.text.trim(),
      receiverNode: undefined,
    };
  }
  if (calleeNode.kind === "PropertyAccessExpression") {
    const receiverNode =
      resolveChild(tree, calleeNode.expression) ?? calleeNode.children[0];
    const nameNode =
      resolveChild(tree, calleeNode.name) ?? calleeNode.children[1];
    return {
      calleeNode,
      methodName: nameNode?.text.trim(),
      receiverNode,
    };
  }
  return {
    calleeNode,
    methodName: undefined,
    receiverNode: undefined,
  };
}

export function sortStringKeys(keys: Iterable<string>): readonly string[] {
  return [...keys].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
}
