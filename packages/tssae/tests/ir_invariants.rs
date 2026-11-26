use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tsrs_api::{Handler, Response, Session, SessionOptions};
use tsrs_ast::{Kind, Node, is_function_like_declaration, is_type_node};
use tsrs_core::{Context, P};
use tsrs_vfs::{FS, bundled, osvfs};
use tssae_analysis::ir::{
    AssignOp, ImplicitAccessorKind, IrCallKind, IrOp, IrPropKey, MemberVisibility,
    TypeGuardSemantics,
};
use tssae_tsrs_boundary::{
    lower_class_default_constructor_to_ir, lower_class_static_init_to_ir, lower_class_to_ir,
    lower_scope_to_ir,
};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn collect_function_nodes(node: P<Node>, out: &mut Vec<P<Node>>) {
    if is_function_like_declaration(Some(node)) && node.body().is_some() {
        out.push(node);
    }
    node.for_each_child(&mut |child| {
        collect_function_nodes(child, out);
        false
    });
}

fn is_value_expression_kind(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::NumericLiteral
            | Kind::BigIntLiteral
            | Kind::StringLiteral
            | Kind::RegularExpressionLiteral
            | Kind::NoSubstitutionTemplateLiteral
            | Kind::TrueKeyword
            | Kind::FalseKeyword
            | Kind::NullKeyword
            | Kind::ThisKeyword
            | Kind::SuperKeyword
            | Kind::MetaProperty
            | Kind::ArrayLiteralExpression
            | Kind::ObjectLiteralExpression
            | Kind::PropertyAccessExpression
            | Kind::ElementAccessExpression
            | Kind::CallExpression
            | Kind::NewExpression
            | Kind::TaggedTemplateExpression
            | Kind::TypeAssertionExpression
            | Kind::ParenthesizedExpression
            | Kind::FunctionExpression
            | Kind::ArrowFunction
            | Kind::DeleteExpression
            | Kind::TypeOfExpression
            | Kind::VoidExpression
            | Kind::AwaitExpression
            | Kind::PrefixUnaryExpression
            | Kind::PostfixUnaryExpression
            | Kind::BinaryExpression
            | Kind::ConditionalExpression
            | Kind::TemplateExpression
            | Kind::YieldExpression
            | Kind::SpreadElement
            | Kind::ClassExpression
            | Kind::AsExpression
            | Kind::NonNullExpression
            | Kind::SatisfiesExpression
            | Kind::JsxElement
            | Kind::JsxSelfClosingElement
            | Kind::JsxFragment
    )
}

fn collect_body_expressions(node: P<Node>, root_fn: P<Node>, out: &mut Vec<P<Node>>) {
    if node != root_fn && is_type_node(node) {
        return;
    }
    if node != root_fn && is_value_expression_kind(node.kind()) {
        out.push(node);
    }
    if node != root_fn
        && (is_function_like_declaration(Some(node))
            || matches!(node.kind(), Kind::ClassDeclaration | Kind::ClassExpression))
    {
        return;
    }
    if node.kind() == Kind::PropertyAccessExpression {
        // PropertyAccessExpression.name() is a member identifier, not an independent value read.
        let pa = node.as_property_access_expression();
        collect_body_expressions(pa.expression(), root_fn, out);
        return;
    }
    node.for_each_child(&mut |child| {
        collect_body_expressions(child, root_fn, out);
        false
    });
}

const CONTROL_FLOW_FIXTURE: &str = r#"
declare function step(v: unknown): void;
declare function risky(): void;
declare function asyncStream(): AsyncIterable<number>;

export function collatz(n: number, ok: boolean): number {
  while (n !== 1) {
    if (n % 2 === 0) {
      n = n / 2;
    } else {
      n = 3 * n + 1;
    }
  }
  if (!ok) {
    return n;
  }
  step(n);
  return n;
}

export function allLoopsAndLabeledJumps(items: number[], obj: Record<string, number>): void {
  let acc = 0;
  do {
    acc += 1;
  } while (acc < 2);

  outer: for (let i = 0; i < items.length; i++) {
    for (const k in obj) {
      if (k === "skip") {
        continue outer;
      }
      if (k === "stop") {
        break outer;
      }
      acc += obj[k] ?? 0;
    }
  }

  blockLabel: {
    if (acc > 10) {
      break blockLabel;
    }
    step(acc);
  }
}

export function tryCatchFinallyAndDeadCode(flag: boolean): number {
  let state = { count: 0 };
  try {
    if (flag) {
      risky();
      return 1;
      // dead code after return must still satisfy invariants
      state.count = 99;
      step(state.count);
    }
    if (!flag) {
      throw new Error("explicit");
      // dead code after throw
      state.count = 100;
    }
  } catch (err) {
    try {
      step(err);
    } finally {
      state.count += 1;
    }
  } finally {
    state.count += 2;
  }
  return state.count;
}

export function* generatorWithDelegate(other: Generator<number>): Generator<number> {
  yield;
  yield 1;
  yield* other;
}

export async function asyncAwaitAndForAwait(p: Promise<number>): Promise<void> {
  const val = await p;
  for await (const item of asyncStream()) {
    if (item === val) {
      break;
    }
    step(item);
  }
}

export function dynamicOpsAndPropWrite(obj: { x: number }, key: string): boolean {
  obj.x = 42;
  obj["x"] += 1;
  return key in obj || obj instanceof Object;
}
"#;

#[test]
fn lowered_control_flow_ir_satisfies_invariants_and_zero_unknowns() {
    let temp = TempDir::new("tssae-ir-invariants");
    let src_dir = temp.path.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::write(
        temp.path.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "strict": true,
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "noEmit": true
  },
  "include": ["src/**/*.ts"]
}"#,
    )
    .expect("write tsconfig");

    fs::write(src_dir.join("control_flow.ts"), CONTROL_FLOW_FIXTURE).expect("write source");

    let cwd = temp.path.to_string_lossy().replace('\\', "/");
    let tsconfig = temp
        .path
        .join("tsconfig.json")
        .to_string_lossy()
        .replace('\\', "/");

    let fs_impl: Arc<dyn FS> = Arc::new(bundled::wrap_fs(osvfs::fs()));
    let session = Session::new(SessionOptions::new(
        cwd.clone(),
        bundled::default_lib_path(),
        fs_impl,
        false,
    ));
    let payload = serde_json::to_vec(&serde_json::json!({
        "openProjects": [&tsconfig],
    }))
    .expect("serialize openProjects");
    let Response::Json(resp_text) = session
        .handle_request("createSnapshot", &payload)
        .expect("createSnapshot")
    else {
        panic!("expected JSON response");
    };
    let parsed: serde_json::Value = serde_json::from_str(&resp_text).expect("parse snapshot json");
    let snap_id = parsed["snapshot"].as_u64().expect("snapshot id");
    let snapshot_data = session.snapshot_data(snap_id).expect("snapshot data");
    let pid = snapshot_data
        .snapshot
        .project_collection
        .projects()
        .first()
        .expect("opened project")
        .id();
    let program = snapshot_data.get_program(&pid).expect("program");
    let ctx = Context::background();
    let mut checker = program.get_type_checker(&ctx);

    let mut fn_nodes = Vec::new();
    for sf in program.source_files() {
        if sf.file_name().ends_with("control_flow.ts") {
            collect_function_nodes(sf.as_node(), &mut fn_nodes);
        }
    }
    assert_eq!(fn_nodes.len(), 6);

    for (idx, &fn_node) in fn_nodes.iter().enumerate() {
        let scope_id = u32::try_from(idx).expect("scope id");
        let lowered = lower_scope_to_ir(&cwd, program, &mut checker, fn_node, scope_id)
            .expect("lowering succeeds");
        lowered
            .ir
            .validate_invariants()
            .expect("structural invariants hold");

        for vertex in &lowered.ir.vertices {
            assert!(
                !matches!(vertex.op, IrOp::Unknown { .. }),
                "Unexpected Unknown vertex in control_flow.ts: {:?}",
                vertex.op
            );
        }

        let mut exprs = Vec::new();
        collect_body_expressions(fn_node, fn_node, &mut exprs);
        for expr in exprs {
            let sid = lowered
                .node_to_syntax
                .get(&expr)
                .copied()
                .unwrap_or_else(|| panic!("Missing SyntaxId for expression {:?}", expr.kind()));
            assert!(
                lowered.ir.expr_to_vertex.contains_key(&sid.local_id()),
                "Expression {:?} missing from expr_to_vertex",
                expr.kind()
            );
        }
    }

    drop(checker);
    drop(snapshot_data);
    let release_payload =
        serde_json::to_vec(&serde_json::json!({ "snapshot": snap_id })).expect("release json");
    let _ = session.handle_request("release", &release_payload);
    session.close();
}

const EXPRESSIONS_CLASSES_FIXTURE: &str = r#"
declare function sink(v: unknown): void;
declare function tag(strings: TemplateStringsArray, ...vals: unknown[]): string;
declare function makeDisposable(): { [Symbol.dispose](): void };
declare function makeAsyncDisposable(): { [Symbol.asyncDispose](): Promise<void> };
declare function decorate(target: unknown, ctx?: unknown): any;

export function allAssignmentsAndDestructuring(
  input: { a?: number; b: { c: number[] }; extra?: string; dynKey?: number },
  pair: [number, number],
  obj: { x: number; get prop(): number; set prop(v: number) },
  dynKey: string,
  { p = 1, ...pRest }: { p?: number; q: number },
  [first = 10, ...restArr]: number[] = [1, 2, 3],
): number {
  let acc = 0;
  acc = 1;
  acc += 2;
  acc++;
  --acc;
  obj.x++;
  obj.prop = acc;
  const g = obj.prop;
  acc += g;

  let maybe: number | undefined = undefined;
  maybe ??= first;
  let truthy = true;
  truthy &&= false;
  let falsy = false;
  falsy ||= true;

  const { a = 5, b: { c: [head, ...tail] }, [dynKey]: dynVal, ...objRest } = input;
  let x1 = 0;
  let x2 = 0;
  let rObj: Record<string, unknown> = {};
  ({ a: x1 = 7, ...rObj } = input);
  [x1, , x2 = 9] = pair;

  for (const [k, v] of [[1, 2]]) {
    acc += k + v;
  }
  for (const key in rObj) {
    sink(key);
  }
  try {
    sink(objRest);
  } catch ({ message }: any) {
    sink(message);
  }

  return acc + a + head + tail.length + p + pRest.q + restArr.length + x1 + x2 + Number(Boolean(dynVal));
}

export async function usingAndOptionalChain(
  nested: { a?: { b: { c(x: number): number } } } | undefined,
  arr: number[],
): Promise<number> {
  using d = makeDisposable();
  await using ad = makeAsyncDisposable();
  void d;
  void ad;

  const tagged = tag`hello ${1 + 2}`;
  const chained = nested?.a?.b.c(42);
  const spreadArr = [...arr, 99];
  const constObj = { flag: true } as const;
  const constArr = <const>[1, 2];
  sink([tagged, chained, spreadArr, constObj, constArr]);
  return chained ?? 0;
}

export function closuresAndClassExpr(seed: number) {
  let counter = seed;
  const inc = (delta: number) => {
    counter += delta;
    return counter;
  };
  const LocalExpr = class extends Object {
    #secret = seed;
    readonly tag = "local";
    method(): number {
      return this.#secret;
    }
  };
  return { inc, LocalExpr };
}

export class BaseWidget {
  readonly kind = "base";
  baseMethod(): number {
    return 1;
  }
}

@decorate
export class DerivedWidget extends BaseWidget {
  #privateVal = 10;
  public instanceField = 20;
  static staticCount = 0;
  static {
    DerivedWidget.staticCount += 1;
  }

  constructor(public readonly id: string, private label: string) {
    super();
    this.instanceField += this.#privateVal;
  }

  override baseMethod(): number {
    return super.baseMethod() + this.label.length;
  }
}

export class DefaultCtorClass extends BaseWidget {
  #token = "abc";
  count = 42;
  readToken(): string {
    return this.#token;
  }
}

export function isWidget(value: unknown): value is BaseWidget {
  return value instanceof BaseWidget;
}

export function assertWidget(value: unknown): asserts value is BaseWidget {
  if (!(value instanceof BaseWidget)) {
    throw new Error("not widget");
  }
}
"#;

const JSX_FIXTURE: &str = r#"
declare namespace JSX {
  interface IntrinsicElements {
    div: { children?: unknown; title?: string; disabled?: boolean };
  }
}
declare function Card(props: { count: number; children?: unknown }): unknown;

export function renderView(count: number, extra: Record<string, unknown>) {
  return (
    <div title="hi" disabled {...extra}>
      <>
        <Card count={count + 1}>{count}</Card>
      </>
    </div>
  );
}
"#;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Integration invariant test covering Ticket 05 expressions, classes, JSX, and caches."
)]
fn lowered_expressions_classes_jsx_and_caches_satisfy_ticket05_contracts() {
    let temp = TempDir::new("tssae-ir-ticket05");
    let src_dir = temp.path.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::write(
        temp.path.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "strict": true,
    "target": "ES2024",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "jsx": "preserve",
    "noEmit": true
  },
  "include": ["src/**/*.ts", "src/**/*.tsx"]
}"#,
    )
    .expect("write tsconfig");

    fs::write(src_dir.join("expr_classes.ts"), EXPRESSIONS_CLASSES_FIXTURE)
        .expect("write expr_classes");
    fs::write(src_dir.join("view.tsx"), JSX_FIXTURE).expect("write jsx");

    let cwd = temp.path.to_string_lossy().replace('\\', "/");
    let tsconfig = temp
        .path
        .join("tsconfig.json")
        .to_string_lossy()
        .replace('\\', "/");

    let fs_impl: Arc<dyn FS> = Arc::new(bundled::wrap_fs(osvfs::fs()));
    let session = Session::new(SessionOptions::new(
        cwd.clone(),
        bundled::default_lib_path(),
        fs_impl,
        false,
    ));
    let payload = serde_json::to_vec(&serde_json::json!({
        "openProjects": [&tsconfig],
    }))
    .expect("serialize openProjects");
    let Response::Json(resp_text) = session
        .handle_request("createSnapshot", &payload)
        .expect("createSnapshot")
    else {
        panic!("expected JSON response");
    };
    let parsed: serde_json::Value = serde_json::from_str(&resp_text).expect("parse snapshot json");
    let snap_id = parsed["snapshot"].as_u64().expect("snapshot id");
    let snapshot_data = session.snapshot_data(snap_id).expect("snapshot data");
    let pid = snapshot_data
        .snapshot
        .project_collection
        .projects()
        .first()
        .expect("opened project")
        .id();
    let program = snapshot_data.get_program(&pid).expect("program");
    let ctx = Context::background();
    let mut checker = program.get_type_checker(&ctx);

    let mut fn_nodes = Vec::new();
    let mut class_nodes = Vec::new();
    for sf in program.source_files() {
        if sf.file_name().ends_with("expr_classes.ts") || sf.file_name().ends_with("view.tsx") {
            collect_function_nodes(sf.as_node(), &mut fn_nodes);
            sf.as_node().for_each_child(&mut |child| {
                if child.kind() == Kind::ClassDeclaration {
                    class_nodes.push(child);
                }
                false
            });
        }
    }

    for (idx, &fn_node) in fn_nodes.iter().enumerate() {
        let scope_id = u32::try_from(idx).expect("scope id");
        let lowered = lower_scope_to_ir(&cwd, program, &mut checker, fn_node, scope_id)
            .expect("lowering succeeds");
        lowered
            .ir
            .validate_invariants()
            .expect("structural invariants hold");

        for vertex in &lowered.ir.vertices {
            assert!(
                !matches!(vertex.op, IrOp::Unknown { .. }),
                "Unexpected Unknown vertex in {}: {:?}",
                lowered.ir.scope_location.file,
                vertex.op
            );
        }

        let mut exprs = Vec::new();
        collect_body_expressions(fn_node, fn_node, &mut exprs);
        for expr in exprs {
            let sid = lowered
                .node_to_syntax
                .get(&expr)
                .copied()
                .unwrap_or_else(|| panic!("Missing SyntaxId for expression {:?}", expr.kind()));
            assert!(
                lowered.ir.expr_to_vertex.contains_key(&sid.local_id()),
                "Expression {:?} missing from expr_to_vertex in {:?}",
                expr.kind(),
                lowered.ir.scope_location.file,
            );
        }
    }

    // Verify specific Ticket 05 semantics:
    let mut find_fn = |needle: &str| {
        fn_nodes
            .iter()
            .copied()
            .enumerate()
            .find(|(_, n)| {
                n.name()
                    .and_then(tssae_tsrs_boundary::node_text)
                    .is_some_and(|t| t == needle)
            })
            .map(|(idx, n)| {
                let scope_id = u32::try_from(idx).expect("scope idx fits u32");
                lower_scope_to_ir(&cwd, program, &mut checker, n, scope_id).expect("lower")
            })
            .expect("function exists")
    };

    let assign_ir = find_fn("allAssignmentsAndDestructuring");
    assert!(assign_ir.ir.vertices.iter().any(|v| matches!(
        v.op,
        IrOp::AssignLocal {
            op: AssignOp::Increment,
            ..
        }
    )));
    assert!(assign_ir.ir.vertices.iter().any(|v| matches!(
        v.op,
        IrOp::PropWrite {
            implicit_call_kind: Some(ImplicitAccessorKind::Setter),
            ..
        }
    )));
    assert!(assign_ir.ir.vertices.iter().any(|v| matches!(
        v.op,
        IrOp::Call {
            call_kind: IrCallKind::ImplicitGetter,
            ..
        }
    )));
    assert!(assign_ir.ir.vertices.iter().any(|v| matches!(
        v.op,
        IrOp::Call {
            call_kind: IrCallKind::ImplicitSetter,
            ..
        }
    )));
    assert!(
        assign_ir
            .ir
            .vertices
            .iter()
            .any(|v| matches!(v.op, IrOp::ObjectRest { .. }))
    );
    assert!(
        assign_ir
            .ir
            .vertices
            .iter()
            .any(|v| matches!(v.op, IrOp::IteratorRest { .. }))
    );

    let using_ir = find_fn("usingAndOptionalChain");
    assert!(using_ir.ir.vertices.iter().any(|v| matches!(
        v.op,
        IrOp::Call {
            call_kind: IrCallKind::ImplicitDispose { is_async: false },
            ..
        }
    )));
    assert!(using_ir.ir.vertices.iter().any(|v| matches!(
        v.op,
        IrOp::Call {
            call_kind: IrCallKind::ImplicitDispose { is_async: true },
            ..
        }
    )));
    assert!(using_ir.ir.vertices.iter().any(|v| matches!(
        v.op,
        IrOp::Call {
            call_kind: IrCallKind::TaggedTemplate,
            ..
        }
    )));
    assert!(
        using_ir
            .ir
            .vertices
            .iter()
            .any(|v| matches!(v.op, IrOp::ObjectAlloc { as_const: true, .. }))
    );
    assert!(
        using_ir
            .ir
            .vertices
            .iter()
            .any(|v| matches!(v.op, IrOp::ArrayAlloc { as_const: true, .. }))
    );

    let closure_ir = find_fn("closuresAndClassExpr");
    assert!(closure_ir.ir.vertices.iter().any(|v| matches!(
        &v.op,
        IrOp::FunctionLiteral { captures, .. }
            if captures.iter().any(|c| c.name == "counter" && c.is_written_in_closure)
    )));

    let is_widget_ir = find_fn("isWidget");
    assert!(matches!(
        is_widget_ir.ir.type_guard,
        Some(TypeGuardSemantics::ParamPredicate {
            is_asserts: false,
            ..
        })
    ));
    let assert_widget_ir = find_fn("assertWidget");
    assert!(matches!(
        assert_widget_ir.ir.type_guard,
        Some(TypeGuardSemantics::ParamPredicate {
            is_asserts: true,
            ..
        })
    ));

    let jsx_ir = find_fn("renderView");
    assert!(
        jsx_ir
            .ir
            .vertices
            .iter()
            .any(|v| matches!(v.op, IrOp::JsxElement { .. }))
    );

    // Verify class IR, static init, and default constructor synthesis
    let derived_class = class_nodes
        .iter()
        .copied()
        .find(|c| {
            c.name()
                .and_then(tssae_tsrs_boundary::node_text)
                .is_some_and(|t| t == "DerivedWidget")
        })
        .expect("DerivedWidget");
    let class_ir =
        lower_class_to_ir(&cwd, program, &mut checker, derived_class, None).expect("class ir");
    assert!(class_ir.has_extends);
    assert!(class_ir.has_explicit_constructor);
    assert!(class_ir.has_static_init);
    assert!(class_ir.instance_fields.iter().any(|f| {
        f.name == "#privateVal" && f.visibility == Some(MemberVisibility::EcmaPrivate)
    }));
    assert!(class_ir.instance_fields.iter().any(|f| {
        f.name == "id" && f.visibility == Some(MemberVisibility::Public) && f.is_readonly
    }));
    assert!(
        class_ir
            .instance_fields
            .iter()
            .any(|f| { f.name == "label" && f.visibility == Some(MemberVisibility::TsPrivate) })
    );

    let static_init =
        lower_class_static_init_to_ir(&cwd, program, &mut checker, derived_class, 99, None)
            .expect("static init");
    static_init
        .ir
        .validate_invariants()
        .expect("static init invariants");
    assert!(static_init.ir.vertices.iter().any(|v| matches!(
        &v.op,
        IrOp::PropWrite {
            key: IrPropKey::Named(k),
            ..
        } if k == "staticCount"
    )));

    let default_ctor_class = class_nodes
        .iter()
        .copied()
        .find(|c| {
            c.name()
                .and_then(tssae_tsrs_boundary::node_text)
                .is_some_and(|t| t == "DefaultCtorClass")
        })
        .expect("DefaultCtorClass");
    let default_ctor = lower_class_default_constructor_to_ir(
        &cwd,
        program,
        &mut checker,
        default_ctor_class,
        100,
        None,
    )
    .expect("default ctor");
    default_ctor
        .ir
        .validate_invariants()
        .expect("default ctor invariants");
    assert!(default_ctor.ir.vertices.iter().any(|v| matches!(
        v.op,
        IrOp::Call {
            call_kind: IrCallKind::SuperCall,
            ..
        }
    )));
    assert!(default_ctor.ir.vertices.iter().any(|v| matches!(
        &v.op,
        IrOp::PropWrite {
            key: IrPropKey::PrivateIdent(k),
            ..
        } if k == "#token"
    )));

    drop(checker);
    drop(snapshot_data);
    let release_payload =
        serde_json::to_vec(&serde_json::json!({ "snapshot": snap_id })).expect("release json");
    let _ = session.handle_request("release", &release_payload);
    session.close();
}
