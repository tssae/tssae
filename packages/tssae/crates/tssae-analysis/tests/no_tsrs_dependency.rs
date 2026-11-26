use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::Path,
};

fn parse_cargo_lock(lock_content: &str) -> BTreeMap<String, Vec<String>> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current_name: Option<String> = None;
    let mut current_deps: Vec<String> = Vec::new();
    let mut in_deps = false;

    for raw_line in lock_content.lines() {
        let line = raw_line.trim();
        if line == "[[package]]" {
            if let Some(name) = current_name.take() {
                graph.entry(name).or_default().append(&mut current_deps);
            }
            current_deps.clear();
            in_deps = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            current_name = Some(strip_quotes(rest));
            continue;
        }
        if line == "dependencies = [" {
            in_deps = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("dependencies = [") {
            let trimmed = rest.trim_end_matches(']').trim();
            if !trimmed.is_empty() {
                for item in trimmed.split(',') {
                    let dep = extract_dep_name(&strip_quotes(item.trim()));
                    if !dep.is_empty() {
                        current_deps.push(dep);
                    }
                }
            }
            in_deps = !rest.ends_with(']');
            continue;
        }
        if in_deps {
            if line == "]" {
                in_deps = false;
                continue;
            }
            let cleaned = line.trim_end_matches(',');
            let dep = extract_dep_name(&strip_quotes(cleaned));
            if !dep.is_empty() {
                current_deps.push(dep);
            }
        }
    }

    if let Some(name) = current_name.take() {
        graph.entry(name).or_default().append(&mut current_deps);
    }

    graph
}

fn strip_quotes(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

fn extract_dep_name(entry: &str) -> String {
    entry
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn collect_transitive_deps(graph: &BTreeMap<String, Vec<String>>, root: &str) -> BTreeSet<String> {
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(root.to_string());

    while let Some(pkg) = queue.pop_front() {
        if !visited.insert(pkg.clone()) {
            continue;
        }
        if let Some(deps) = graph.get(&pkg) {
            for dep in deps {
                if !visited.contains(dep) {
                    queue.push_back(dep.clone());
                }
            }
        }
    }

    visited.remove(root);
    visited
}

fn assert_manifest_has_no_tsrs_deps(manifest_path: &Path) {
    let content = fs::read_to_string(manifest_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest_path.display()));
    let mut in_dependency_section = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            in_dependency_section = line.contains("dependencies");
            continue;
        }
        if !in_dependency_section || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let dep_key = line.split('=').next().unwrap_or("").trim();
        assert!(
            !dep_key.starts_with("tsrs"),
            "{} must not depend on any tsrs crate, found `{dep_key}`",
            manifest_path.display()
        );
    }
}

#[test]
fn tssae_analysis_and_common_have_zero_tsrs_dependencies() {
    let analysis_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tssae_pkg_dir = analysis_dir
        .parent()
        .and_then(Path::parent)
        .expect("packages/tssae directory");
    let common_manifest = tssae_pkg_dir.join("crates/tssae-common/Cargo.toml");
    let analysis_manifest = analysis_dir.join("Cargo.toml");
    let boundary_manifest = tssae_pkg_dir.join("crates/tssae-tsrs-boundary/Cargo.toml");
    let cargo_lock_path = tssae_pkg_dir.join("Cargo.lock");

    assert_manifest_has_no_tsrs_deps(&common_manifest);
    assert_manifest_has_no_tsrs_deps(&analysis_manifest);
    assert!(
        boundary_manifest.is_file(),
        "boundary manifest must exist at {}",
        boundary_manifest.display()
    );

    let lock_content = fs::read_to_string(&cargo_lock_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", cargo_lock_path.display()));
    let dep_graph = parse_cargo_lock(&lock_content);

    for crate_name in ["tssae-common", "tssae-analysis"] {
        assert!(
            dep_graph.contains_key(crate_name),
            "Cargo.lock must contain workspace crate {crate_name}"
        );
        let transitive = collect_transitive_deps(&dep_graph, crate_name);
        let tsrs_deps: Vec<&String> = transitive
            .iter()
            .filter(|name| name.starts_with("tsrs"))
            .collect();
        assert!(
            tsrs_deps.is_empty(),
            "{crate_name} must have zero transitive tsrs dependencies, found: {tsrs_deps:?}"
        );
    }

    let boundary_transitive = collect_transitive_deps(&dep_graph, "tssae-tsrs-boundary");
    assert!(
        boundary_transitive
            .iter()
            .any(|name| name.starts_with("tsrs")),
        "tssae-tsrs-boundary is expected to encapsulate tsrs dependencies"
    );
}
