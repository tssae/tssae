mod support;

use std::{
    fs,
    io::BufReader,
    path::Path,
    process::{ChildStdin, ChildStdout},
};

use serde_json::{Value, json};
use support::{TempDirGuard, finish_engine, normalized, rpc_call, spawn_engine_stdio};

fn open_snapshot(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    cwd: &str,
    entry: &str,
) -> (u64, String, Vec<Value>) {
    let resp = rpc_call(
        stdin,
        reader,
        &json!({
            "id": 1,
            "method": "snapshot.open",
            "params": { "cwd": cwd, "entries": [entry] }
        }),
    );
    assert_eq!(resp["ok"], json!(true), "open failed: {resp}");
    assert!(
        resp.get("metrics").is_none(),
        "metrics must be omitted when observeMode is off: {resp}"
    );
    let snapshot_id = resp["result"]["snapshotId"].as_u64().expect("snapshotId");
    let project_id = resp["result"]["projects"][0]["projectId"]
        .as_str()
        .expect("projectId")
        .to_string();
    let source_files = resp["result"]["projects"][0]["sourceFiles"]
        .as_array()
        .expect("sourceFiles")
        .clone();
    (snapshot_id, project_id, source_files)
}

fn convert_file_to_crlf(path: &Path) {
    let text = fs::read_to_string(path).expect("read file for crlf conversion");
    let crlf_text = text.replace("\r\n", "\n").replace('\n', "\r\n");
    fs::write(path, crlf_text).expect("write crlf file");
}

#[test]
fn serves_narrowed_type_queries_and_utf16_locations() {
    let temp = TempDirGuard::from_host("verule-engine-serve-types", "sample");
    let tsconfig_str = normalized(&temp.path().join("tsconfig.json"));
    let sample_source =
        fs::read_to_string(temp.path().join("src/sample.ts")).expect("read sample.ts");
    let cwd_str = normalized(temp.path());
    let (child, mut stdin, mut reader) = spawn_engine_stdio();

    let (snapshot_id, project_id, source_files) =
        open_snapshot(&mut stdin, &mut reader, &cwd_str, &tsconfig_str);
    assert!(
        source_files
            .iter()
            .any(|v| v.as_str() == Some("src/sample.ts"))
    );
    assert!(
        !source_files
            .iter()
            .any(|v| v.as_str().unwrap_or("").contains("lib.es5.d.ts"))
    );

    let syntax_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 2,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "findSyntax", "kinds": ["CallExpression"] }]
            }
        }),
    );
    assert_eq!(
        syntax_resp["ok"],
        json!(true),
        "findSyntax failed: {syntax_resp}"
    );
    let sink_calls: Vec<&Value> = syntax_resp["result"]["results"][0]["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["text"].as_str().unwrap_or("").starts_with("sink("))
        .collect();
    assert_eq!(sink_calls.len(), 2);

    let first_loc = &sink_calls[0]["location"];
    assert_eq!(first_loc["file"], json!("src/sample.ts"));
    assert_eq!(first_loc["line"], json!(7));
    let line7 = sample_source.lines().nth(6).expect("line 7 in sample.ts");
    let prefix = line7.split("sink(input)").next().expect("prefix");
    let expected_col = u32::try_from(prefix.encode_utf16().count()).expect("fits u32") + 1;
    assert_eq!(first_loc["column"], json!(expected_col));
    assert_eq!(first_loc["column"], json!(20));
    let start_byte = usize::try_from(first_loc["start"].as_u64().expect("start")).expect("usize");
    let end_byte = usize::try_from(first_loc["end"].as_u64().expect("end")).expect("usize");
    assert_eq!(&sample_source[start_byte..end_byte], "sink(input)");

    let arg_if = sink_calls[0]["arguments"][0].as_str().expect("arg0");
    let arg_else = sink_calls[1]["arguments"][0].as_str().expect("arg1");
    let type_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 3,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "typeAtLocation", "targets": [arg_if, arg_else] }]
            }
        }),
    );
    assert_eq!(
        type_resp["ok"],
        json!(true),
        "typeAtLocation failed: {type_resp}"
    );
    let types = type_resp["result"]["results"][0]["items"]
        .as_array()
        .expect("items");
    assert_eq!(types[0]["typeText"], json!("string"));
    assert_eq!(types[1]["typeText"], json!("number"));

    finish_engine(child, stdin);
}

#[test]
fn rejects_stale_snapshots_and_invalid_project_configs() {
    let temp = TempDirGuard::from_host("verule-engine-serve-errors", "sample");
    let tsconfig_str = normalized(&temp.path().join("tsconfig.json"));
    let cwd_str = normalized(temp.path());
    let (child, mut stdin, mut reader) = spawn_engine_stdio();

    let open_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 1,
            "method": "snapshot.open",
            "params": { "cwd": cwd_str, "entries": [tsconfig_str] }
        }),
    );
    assert_eq!(open_resp["ok"], json!(true));
    let snapshot_id = open_resp["result"]["snapshotId"]
        .as_u64()
        .expect("snapshotId");
    let project_id = open_resp["result"]["projects"][0]["projectId"]
        .as_str()
        .expect("projectId")
        .to_string();

    let release_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 2,
            "method": "snapshot.release",
            "params": { "snapshotId": snapshot_id }
        }),
    );
    assert_eq!(release_resp["ok"], json!(true));

    let stale_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 3,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "findSyntax", "kinds": ["CallExpression"] }]
            }
        }),
    );
    assert_eq!(stale_resp["ok"], json!(false));
    assert_eq!(stale_resp["error"]["code"], json!("STALE_SNAPSHOT"));

    let missing_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 4,
            "method": "snapshot.open",
            "params": { "cwd": cwd_str, "entries": ["nonexistent.tsconfig.json"] }
        }),
    );
    assert_eq!(missing_resp["ok"], json!(false));
    assert_eq!(missing_resp["error"]["code"], json!("PROJECT_LOAD_ERROR"));

    let bad_tsconfig = temp.path().join("bad.tsconfig.json");
    fs::write(&bad_tsconfig, "{ not valid json").expect("write bad tsconfig");
    let bad_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 5,
            "method": "snapshot.open",
            "params": { "cwd": cwd_str, "entries": [normalized(&bad_tsconfig)] }
        }),
    );
    assert_eq!(bad_resp["ok"], json!(false));
    assert_eq!(bad_resp["error"]["code"], json!("PROJECT_LOAD_ERROR"));

    finish_engine(child, stdin);
}

fn collect_cfg_handles(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    snapshot_id: u64,
    project_id: &str,
) -> (Vec<String>, Vec<String>) {
    let syntax_resp = rpc_call(
        stdin,
        reader,
        &json!({
            "id": 2,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [
                    { "op": "findSyntax", "kinds": ["FunctionDeclaration"] },
                    { "op": "findSyntax", "kinds": ["CallExpression"] }
                ]
            }
        }),
    );
    assert_eq!(syntax_resp["ok"], json!(true));
    let body_fns: Vec<String> = syntax_resp["result"]["results"][0]["items"]
        .as_array()
        .expect("fns")
        .iter()
        .filter(|item| !item["text"].as_str().unwrap_or("").starts_with("declare "))
        .map(|item| item["ref"].as_str().expect("fn ref").to_string())
        .collect();
    assert_eq!(body_fns.len(), 3);

    let sink_calls: Vec<String> = syntax_resp["result"]["results"][1]["items"]
        .as_array()
        .expect("calls")
        .iter()
        .filter(|item| item["text"].as_str().unwrap_or("").starts_with("sink("))
        .map(|item| item["ref"].as_str().expect("call ref").to_string())
        .collect();
    assert_eq!(sink_calls.len(), 3);
    (body_fns, sink_calls)
}

#[test]
fn serves_list_branches_and_branch_coverage_statuses() {
    let temp = TempDirGuard::from_host("verule-engine-serve-cfg", "cfg");
    let tsconfig_str = normalized(&temp.path().join("tsconfig.json"));
    let cwd_str = normalized(temp.path());
    let (child, mut stdin, mut reader) = spawn_engine_stdio();
    let (snapshot_id, project_id, _) =
        open_snapshot(&mut stdin, &mut reader, &cwd_str, &tsconfig_str);
    let (body_fns, sink_calls) =
        collect_cfg_handles(&mut stdin, &mut reader, snapshot_id, &project_id);

    let branches_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 3,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "listBranches", "targets": body_fns }]
            }
        }),
    );
    assert_eq!(
        branches_resp["ok"],
        json!(true),
        "listBranches failed: {branches_resp}"
    );
    let branch_groups = branches_resp["result"]["results"][0]["items"]
        .as_array()
        .expect("branch groups");

    let joint_true_branches: Vec<&str> = branch_groups[0]["branches"]
        .as_array()
        .expect("joint branches")
        .iter()
        .filter(|b| b["direction"] == json!("true"))
        .map(|b| b["ref"].as_str().expect("branch ref"))
        .collect();
    assert_eq!(joint_true_branches.len(), 2);

    let collect_all_refs = |idx: usize| -> Vec<&str> {
        branch_groups[idx]["branches"]
            .as_array()
            .expect("branches")
            .iter()
            .map(|b| b["ref"].as_str().expect("branch ref"))
            .collect()
    };

    let cov_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 4,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [
                    { "op": "branchCoverage", "function": body_fns[0], "branches": [joint_true_branches[0]], "targets": [sink_calls[0]] },
                    { "op": "branchCoverage", "function": body_fns[0], "branches": joint_true_branches, "targets": [sink_calls[0]] },
                    { "op": "branchCoverage", "function": body_fns[1], "branches": collect_all_refs(1), "targets": [sink_calls[1]] },
                    { "op": "branchCoverage", "function": body_fns[2], "branches": collect_all_refs(2), "targets": [sink_calls[2]] }
                ]
            }
        }),
    );
    assert_eq!(
        cov_resp["ok"],
        json!(true),
        "branchCoverage failed: {cov_resp}"
    );
    let results = cov_resp["result"]["results"].as_array().expect("results");
    assert_eq!(results[0]["items"][0]["status"], json!("bypass"));
    assert_eq!(results[1]["items"][0]["status"], json!("covered"));
    assert_eq!(results[2]["items"][0]["status"], json!("unreachable"));
    assert_eq!(results[3]["items"][0]["status"], json!("unknown"));
    assert_eq!(
        results[3]["items"][0]["gaps"][0]["kind"],
        json!("unmodeled-try-catch")
    );

    finish_engine(child, stdin);
}

#[test]
fn projects_overloads_generics_assignability_and_three_state_fields() {
    let temp = TempDirGuard::from_host("verule-engine-serve-projection", "projection");
    let cwd_str = normalized(temp.path());
    let tsconfig_str = normalized(&temp.path().join("tsconfig.json"));
    let (child, mut stdin, mut reader) = spawn_engine_stdio();
    let (snapshot_id, project_id, _) =
        open_snapshot(&mut stdin, &mut reader, &cwd_str, &tsconfig_str);

    // Range-scoped findSyntax on lines 11-13 of src/mod.ts
    let syntax_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 2,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{
                    "op": "findSyntax",
                    "kinds": ["CallExpression"],
                    "range": { "file": "src/mod.ts", "startLine": 11, "endLine": 13 }
                }]
            }
        }),
    );
    assert_eq!(syntax_resp["ok"], json!(true), "{syntax_resp}");
    let call_items = syntax_resp["result"]["results"][0]["items"]
        .as_array()
        .expect("items");
    assert_eq!(call_items.len(), 3);

    let resolve_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 3,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{
                    "op": "resolveCalls",
                    "targets": [
                        call_items[0]["ref"],
                        call_items[1]["ref"],
                        call_items[2]["ref"]
                    ],
                    "select": {
                        "signature": true,
                        "arguments": true,
                        "instantiatedParameterTypes": true,
                        "returnType": true
                    }
                }]
            }
        }),
    );
    assert_eq!(resolve_resp["ok"], json!(true), "{resolve_resp}");
    let rows = resolve_resp["result"]["results"][0]["items"]
        .as_array()
        .expect("resolved call rows");
    assert_eq!(rows.len(), 3);

    // Unrequested field `callee` must not be serialized; requested fields must exist.
    assert!(rows[0].get("callee").is_none());
    assert_eq!(
        rows[0]["instantiatedParameterTypes"][0]["type"]["typeText"],
        json!("string")
    );
    assert_eq!(rows[0]["returnType"]["typeText"], json!("string"));
    assert_eq!(rows[0]["gaps"], json!([]));

    // Empty 0-arg call: `arguments: []`, `instantiatedParameterTypes: []`, `gaps: []`.
    assert_eq!(rows[1]["arguments"], json!([]));
    assert_eq!(rows[1]["instantiatedParameterTypes"], json!([]));
    assert_eq!(rows[1]["returnType"]["typeText"], json!("void"));
    assert_eq!(rows[1]["gaps"], json!([]));

    // Indeterminate call on `any`: signature & returnType are explicit null, gaps non-empty.
    assert_eq!(rows[2]["signature"], json!(null));
    assert_eq!(rows[2]["returnType"], json!(null));
    assert_eq!(rows[2]["gaps"][0]["kind"], json!("unresolved-signature"));

    // Mismatched handle kind rejected with INVALID_REFERENCE_KIND.
    let bad_kind_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 4,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "projectCalls", "targets": [call_items[0]["ref"]], "select": { "location": true } }]
            }
        }),
    );
    assert_eq!(bad_kind_resp["ok"], json!(false));
    assert_eq!(
        bad_kind_resp["error"]["code"],
        json!("INVALID_REFERENCE_KIND")
    );

    finish_engine(child, stdin);
}

#[test]
fn updates_snapshots_and_invalidates_stale_handles() {
    let temp = TempDirGuard::from_host("verule-engine-serve-update", "sample");
    let tsconfig_str = normalized(&temp.path().join("tsconfig.json"));
    let unrelated_path = temp.path().join("src").join("unrelated.ts");
    fs::write(&unrelated_path, "export const untouched = 1;\n").expect("write unrelated.ts");
    let cwd_str = normalized(temp.path());
    let (child, mut stdin, mut reader) = spawn_engine_stdio();

    let (snap1, project_id, files1) =
        open_snapshot(&mut stdin, &mut reader, &cwd_str, &tsconfig_str);
    assert!(files1.iter().any(|v| v.as_str() == Some("src/sample.ts")));
    assert!(
        files1
            .iter()
            .any(|v| v.as_str() == Some("src/unrelated.ts"))
    );

    let syn_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 2,
            "method": "query.batch",
            "params": {
                "snapshotId": snap1,
                "projectId": project_id,
                "steps": [{ "op": "findSyntax", "files": ["src/sample.ts"], "kinds": ["CallExpression"] }]
            }
        }),
    );
    assert_eq!(syn_resp["ok"], json!(true));
    let old_ref = syn_resp["result"]["results"][0]["items"][0]["ref"]
        .as_str()
        .expect("old ref")
        .to_string();

    fs::write(&unrelated_path, "export const untouched = 2;\n").expect("update unrelated.ts");
    let update1 = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 3,
            "method": "snapshot.update",
            "params": { "snapshotId": snap1, "changed": ["src/unrelated.ts"] }
        }),
    );
    assert_eq!(update1["ok"], json!(true), "update1 failed: {update1}");
    let snap2 = update1["result"]["snapshotId"].as_u64().expect("snap2");
    assert_ne!(snap2, snap1);
    assert!(
        update1["result"]["snapshotDigest"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(
        update1["result"]["recomputedFiles"]
            .as_array()
            .expect("recomputedFiles")
            .iter()
            .any(|v| v.as_str() == Some("src/unrelated.ts"))
    );
    assert!(
        update1["result"]["reusedFiles"]
            .as_array()
            .expect("reusedFiles")
            .iter()
            .any(|v| v.as_str() == Some("src/sample.ts"))
    );
    if let Some(rss) = update1["result"]["engineRssBytes"].as_u64() {
        assert!(rss > 0);
    }

    assert_stale_type_query(
        &mut stdin,
        &mut reader,
        4,
        snap1,
        &project_id,
        &old_ref,
        "STALE_SNAPSHOT",
    );
    assert_stale_type_query(
        &mut stdin,
        &mut reader,
        5,
        snap2,
        &project_id,
        &old_ref,
        "STALE_REFERENCE",
    );

    finish_engine(child, stdin);
}

#[test]
fn recovers_from_broken_tsconfig_on_snapshot_update() {
    let temp = TempDirGuard::from_host("verule-engine-serve-recover", "sample");
    let tsconfig_str = normalized(&temp.path().join("tsconfig.json"));
    let cwd_str = normalized(temp.path());
    let (child, mut stdin, mut reader) = spawn_engine_stdio();

    let (snap1, _, _) = open_snapshot(&mut stdin, &mut reader, &cwd_str, &tsconfig_str);
    let tsconfig_path = temp.path().join("tsconfig.json");
    let valid_tsconfig = fs::read_to_string(&tsconfig_path).expect("read tsconfig");

    fs::write(&tsconfig_path, "{ broken json").expect("break tsconfig");
    let failed_update = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 2,
            "method": "snapshot.update",
            "params": { "snapshotId": snap1, "changed": ["tsconfig.json"] }
        }),
    );
    assert_eq!(failed_update["ok"], json!(false));
    assert_eq!(failed_update["error"]["code"], json!("PROJECT_LOAD_ERROR"));

    fs::write(&tsconfig_path, valid_tsconfig).expect("restore tsconfig");
    let recovered_update = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 3,
            "method": "snapshot.update",
            "params": { "snapshotId": snap1, "changed": ["tsconfig.json"] }
        }),
    );
    assert_eq!(
        recovered_update["ok"],
        json!(true),
        "recovery failed: {recovered_update}"
    );
    assert_ne!(
        recovered_update["result"]["snapshotId"]
            .as_u64()
            .expect("snap2"),
        snap1
    );

    finish_engine(child, stdin);
}

fn assert_stale_type_query(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    id: u64,
    snapshot_id: u64,
    project_id: &str,
    handle_ref: &str,
    expected_code: &str,
) {
    let resp = rpc_call(
        stdin,
        reader,
        &json!({
            "id": id,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "typeAtLocation", "targets": [handle_ref] }]
            }
        }),
    );
    assert_eq!(resp["ok"], json!(false));
    assert_eq!(resp["error"]["code"], json!(expected_code));
}

#[test]
fn emits_observation_metrics_in_summary_and_omits_when_off() {
    let temp = TempDirGuard::from_host("verule-engine-serve-observe-summary", "sample");
    let tsconfig_str = normalized(&temp.path().join("tsconfig.json"));
    let cwd_str = normalized(temp.path());
    let (child, mut stdin, mut reader) = spawn_engine_stdio();

    let open_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 1,
            "method": "snapshot.open",
            "params": { "cwd": cwd_str, "entries": [tsconfig_str] },
            "observe": { "mode": "summary" }
        }),
    );
    assert_eq!(open_resp["ok"], json!(true), "open failed: {open_resp}");
    assert!(open_resp["metrics"]["durationMs"].as_f64().is_some());
    assert!(open_resp["metrics"]["encodeMs"].as_f64().is_some());
    let snapshot_id = open_resp["result"]["snapshotId"]
        .as_u64()
        .expect("snapshotId");
    let project_id = open_resp["result"]["projects"][0]["projectId"]
        .as_str()
        .expect("projectId")
        .to_string();

    let summary_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 2,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "findSyntax", "kinds": ["CallExpression"] }]
            },
            "observe": { "mode": "summary" }
        }),
    );
    assert_eq!(summary_resp["ok"], json!(true));
    let summary_step = &summary_resp["metrics"]["steps"][0];
    assert_eq!(summary_step["op"], json!("findSyntax"));
    assert_eq!(summary_step["provider"], json!("engine"));
    assert_eq!(summary_step["cacheStatus"], json!("computed"));
    assert!(summary_step["candidateCount"].as_u64().unwrap_or(0) >= 2);
    assert!(summary_step.get("details").is_none());
    assert_eq!(summary_resp["metrics"]["truncated"], json!(false));
    assert_eq!(summary_resp["metrics"]["droppedCount"], json!(0));

    let off_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 3,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "findSyntax", "kinds": ["CallExpression"] }]
            },
            "observe": { "mode": "off" }
        }),
    );
    assert_eq!(off_resp["ok"], json!(true));
    assert!(off_resp.get("metrics").is_none());

    finish_engine(child, stdin);
}

#[test]
fn truncates_observation_details_by_items_and_bytes() {
    let temp = TempDirGuard::from_host("verule-engine-serve-observe-limit", "sample");
    let tsconfig_str = normalized(&temp.path().join("tsconfig.json"));
    let cwd_str = normalized(temp.path());
    let (child, mut stdin, mut reader) = spawn_engine_stdio();

    let (snapshot_id, project_id, _) =
        open_snapshot(&mut stdin, &mut reader, &cwd_str, &tsconfig_str);

    let item_limited_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 2,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "findSyntax", "kinds": ["CallExpression"] }]
            },
            "observe": { "mode": "detail", "maxItems": 1 }
        }),
    );
    assert_eq!(item_limited_resp["ok"], json!(true));
    assert_eq!(item_limited_resp["metrics"]["truncated"], json!(true));
    assert!(
        item_limited_resp["metrics"]["droppedCount"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    let details = item_limited_resp["metrics"]["steps"][0]["details"]
        .as_array()
        .expect("detail items array");
    assert_eq!(details.len(), 1);

    let byte_limited_resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 3,
            "method": "query.batch",
            "params": {
                "snapshotId": snapshot_id,
                "projectId": project_id,
                "steps": [{ "op": "findSyntax", "kinds": ["CallExpression"] }]
            },
            "observe": { "mode": "detail", "maxBytes": 120 }
        }),
    );
    assert_eq!(byte_limited_resp["ok"], json!(true));
    assert_eq!(byte_limited_resp["metrics"]["truncated"], json!(true));
    assert!(
        byte_limited_resp["metrics"]["droppedCount"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );

    finish_engine(child, stdin);
}

#[test]
fn scans_inline_ignore_directives_across_comments_jsx_crlf_and_excludes_jsdoc_and_strings() {
    let temp = TempDirGuard::from_host("verule-engine-serve-directives", "inline-ignore");
    convert_file_to_crlf(&temp.path().join("src/app.ts"));

    let tsconfig_str = normalized(&temp.path().join("tsconfig.json"));
    let cwd_str = normalized(temp.path());
    let (child, mut stdin, mut reader) = spawn_engine_stdio();
    let (snapshot_id, _, _) = open_snapshot(&mut stdin, &mut reader, &cwd_str, &tsconfig_str);

    let resp = rpc_call(
        &mut stdin,
        &mut reader,
        &json!({
            "id": 2,
            "method": "directives.scan",
            "params": { "snapshotId": snapshot_id }
        }),
    );
    assert_eq!(resp["ok"], json!(true), "directives.scan failed: {resp}");
    let directives = resp["result"]["directives"]
        .as_array()
        .expect("directives array");

    assert_eq!(directives.len(), 5, "unexpected directives: {resp}");
    assert_eq!(directives[0]["kind"], json!("valid"));
    assert_eq!(directives[0]["ruleId"], json!("r1"));
    assert_eq!(directives[0]["targetLine"], json!(5));
    assert_eq!(directives[1]["kind"], json!("valid"));
    assert_eq!(directives[1]["ruleId"], json!("r2"));
    assert_eq!(directives[1]["reason"], json!("Block reason"));
    assert_eq!(directives[1]["targetLine"], json!(10));
    assert_eq!(directives[2]["kind"], json!("valid"));
    assert_eq!(directives[2]["ruleId"], json!("r3"));
    assert_eq!(directives[2]["targetLine"], Value::Null);
    assert_eq!(directives[3]["kind"], json!("malformed"));
    assert_eq!(directives[3]["at"]["line"], json!(14));
    assert_eq!(directives[4]["kind"], json!("valid"));
    assert_eq!(directives[4]["ruleId"], json!("r-jsx"));
    assert_eq!(directives[4]["at"]["file"], json!("src/view.tsx"));
    assert_eq!(directives[4]["targetLine"], json!(7));

    finish_engine(child, stdin);
}
