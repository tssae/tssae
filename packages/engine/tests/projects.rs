mod support;

use std::{
    fs,
    io::{BufReader, Write as _},
    path::Path,
    process::{ChildStdin, ChildStdout},
};

use serde_json::{Value, json};
use support::{
    TempDirGuard, copy_dir_recursive, copy_host_scenario, finish_engine, normalized, rpc_call,
    spawn_engine_stdio,
};

fn materialize_vendor_packages(root: &Path) {
    let vendor_pkg = root.join("vendor/ext-pkg");
    let node_modules_pkg = root.join("node_modules/ext-pkg");
    copy_dir_recursive(&vendor_pkg, &node_modules_pkg);
    fs::remove_dir_all(root.join("vendor")).expect("remove vendor dir after materializing");
}

fn assert_runtime_literal_type(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    snapshot_id: u64,
    proj: &Value,
    req_id: u64,
    expected_literal: &str,
) {
    let project_id = proj["projectId"].as_str().expect("projectId");
    let syntax_resp = rpc_call(
        stdin,
        reader,
        &json!({
            "id": req_id,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{
                    "op": "findSyntax",
                    "files": ["./packages/shared/shared.ts"],
                    "kinds": ["CallExpression"]
                }]
            }
        }),
    );
    assert_eq!(
        syntax_resp["ok"],
        json!(true),
        "findSyntax failed: {syntax_resp}"
    );
    let items = syntax_resp["result"]["results"][0]["items"]
        .as_array()
        .expect("items");
    assert_eq!(items.len(), 1, "expected 1 call in shared.ts");
    let arg_ref = items[0]["arguments"][0].as_str().expect("arg0 ref");

    let type_resp = rpc_call(
        stdin,
        reader,
        &json!({
            "id": req_id + 10,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{
                    "op": "typeAtLocation",
                    "targets": [arg_ref]
                }]
            }
        }),
    );
    assert_eq!(
        type_resp["ok"],
        json!(true),
        "typeAtLocation failed: {type_resp}"
    );
    let ty = &type_resp["result"]["results"][0]["items"][0];
    assert_eq!(ty["isLiteral"], json!(true));
    assert_eq!(ty["literalValue"], json!(expected_literal));
    assert_eq!(ty["typeText"], json!(format!("\"{expected_literal}\"")));
}

#[test]
fn discovers_references_recursively_tracks_provenance_and_isolates_types() {
    let temp = TempDirGuard::from_host("verule-engine-projects-agg", "multi-project");
    materialize_vendor_packages(temp.path());
    let cwd_str = normalized(temp.path());
    let (child, mut stdin, mut reader) = spawn_engine_stdio();

    let open_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 1,
            "method": "snapshot.open",
            "params": { "cwd": cwd_str, "entries": ["tsconfig.json"] }
        }),
    );
    assert_eq!(open_resp["ok"], json!(true), "open failed: {open_resp}");

    let snapshot_id = open_resp["result"]["snapshotId"]
        .as_u64()
        .expect("snapshotId");
    let projects = open_resp["result"]["projects"]
        .as_array()
        .expect("projects");
    assert_eq!(
        projects.len(),
        3,
        "expected root + 2 subprojects: {open_resp}"
    );

    let root_proj = projects
        .iter()
        .find(|p| p["configFile"] == json!("tsconfig.json"))
        .expect("root project");
    assert_eq!(root_proj["entryConfig"], json!("tsconfig.json"));
    assert_eq!(root_proj["entryChain"], json!(["tsconfig.json"]));
    assert_eq!(root_proj["sourceFiles"], json!([]));
    assert_eq!(
        root_proj["projectReferences"],
        json!(["packages/web/tsconfig.json", "packages/node/tsconfig.json"])
    );

    let web_proj = projects
        .iter()
        .find(|p| p["configFile"] == json!("packages/web/tsconfig.json"))
        .expect("web project");
    assert_eq!(web_proj["entryConfig"], json!("tsconfig.json"));
    assert_eq!(
        web_proj["entryChain"],
        json!(["tsconfig.json", "packages/web/tsconfig.json"])
    );
    assert_eq!(web_proj["referencedBy"], json!(["tsconfig.json"]));
    assert_eq!(web_proj["compilerOptions"]["strict"], json!(true));
    assert_eq!(
        web_proj["compilerOptions"]["paths"]["@runtime"],
        json!(["./src/web.ts"])
    );
    assert_eq!(
        web_proj["sourceFiles"],
        json!(["packages/shared/shared.ts", "packages/web/src/web.ts"])
    );

    let node_proj = projects
        .iter()
        .find(|p| p["configFile"] == json!("packages/node/tsconfig.json"))
        .expect("node project");
    assert_eq!(node_proj["entryConfig"], json!("tsconfig.json"));
    assert_eq!(
        node_proj["entryChain"],
        json!(["tsconfig.json", "packages/node/tsconfig.json"])
    );
    assert_eq!(node_proj["referencedBy"], json!(["tsconfig.json"]));
    assert_eq!(node_proj["compilerOptions"]["strict"], json!(false));
    assert_eq!(node_proj["compilerOptions"]["allowJs"], json!(true));
    assert_eq!(
        node_proj["compilerOptions"]["paths"]["@runtime"],
        json!(["./src/node.ts"])
    );
    assert_eq!(
        node_proj["sourceFiles"],
        json!(["packages/node/src/node.ts", "packages/shared/shared.ts"])
    );

    assert_runtime_literal_type(&mut stdin, &mut reader, snapshot_id, web_proj, 2, "web");
    assert_runtime_literal_type(&mut stdin, &mut reader, snapshot_id, node_proj, 3, "node");

    finish_engine(child, stdin);
}

#[test]
fn handles_nested_and_cyclic_references_without_looping_and_opens_jsconfig() {
    let temp = TempDirGuard::new("verule-engine-projects-cycles");
    let root = temp.path();
    copy_host_scenario("jsapp", &root.join("jsapp"));

    // Intentional circular tsconfig graph placeholders kept inline so checked hosts stay acyclic.
    for (dir, file, val, peer) in [
        ("a", "a.ts", "export const aVal = 1;\n", "../b"),
        ("b", "b.ts", "export const bVal = 2;\n", "../a"),
    ] {
        fs::create_dir_all(root.join(dir)).expect("create cyclic dir");
        fs::write(root.join(dir).join(file), val).expect("write placeholder ts");
        let mut tsconfig =
            fs::File::create(root.join(dir).join("tsconfig.json")).expect("create cyclic tsconfig");
        let config_json = serde_json::to_string_pretty(&json!({
            "compilerOptions": { "composite": true, "strict": true },
            "include": [format!("./{file}")],
            "references": [{ "path": peer, "circular": true }]
        }))
        .expect("serialize cyclic tsconfig");
        tsconfig
            .write_all(config_json.as_bytes())
            .expect("write cyclic tsconfig");
    }

    let cwd_str = normalized(root);
    let (child, mut stdin, mut reader) = spawn_engine_stdio();

    let open_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 1,
            "method": "snapshot.open",
            "params": {
                "cwd": cwd_str,
                "entries": ["a/tsconfig.json", "jsapp/jsconfig.json"]
            }
        }),
    );
    assert_eq!(open_resp["ok"], json!(true), "open failed: {open_resp}");

    let projects = open_resp["result"]["projects"]
        .as_array()
        .expect("projects");
    assert_eq!(projects.len(), 3, "expected a, jsapp, b: {open_resp}");

    let proj_b = projects
        .iter()
        .find(|p| p["configFile"] == json!("b/tsconfig.json"))
        .expect("b project");
    assert_eq!(proj_b["entryConfig"], json!("a/tsconfig.json"));
    assert_eq!(
        proj_b["entryChain"],
        json!(["a/tsconfig.json", "b/tsconfig.json"])
    );
    assert_eq!(proj_b["referencedBy"], json!(["a/tsconfig.json"]));
    assert_eq!(proj_b["projectReferences"], json!(["a/tsconfig.json"]));

    let proj_js = projects
        .iter()
        .find(|p| p["configFile"] == json!("jsapp/jsconfig.json"))
        .expect("jsapp project");
    assert_eq!(proj_js["entryConfig"], json!("jsapp/jsconfig.json"));
    assert_eq!(proj_js["entryChain"], json!(["jsapp/jsconfig.json"]));
    assert_eq!(proj_js["sourceFiles"], json!(["jsapp/index.js"]));
    assert_eq!(proj_js["compilerOptions"]["allowJs"], json!(true));

    finish_engine(child, stdin);
}
