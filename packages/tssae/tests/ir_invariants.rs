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
use tssae_analysis::ir::IrOp;
use tssae_tsrs_boundary::lower_scope_to_ir;

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
    )
}

fn collect_body_expressions(node: P<Node>, root_fn: P<Node>, out: &mut Vec<P<Node>>) {
    if node != root_fn
        && (is_function_like_declaration(Some(node))
            || matches!(node.kind(), Kind::ClassDeclaration | Kind::ClassExpression)
            || is_type_node(node))
    {
        return;
    }
    if is_value_expression_kind(node.kind()) {
        out.push(node);
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
