use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    time::Instant,
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tsrs_ast::{Kind, Node, SourceFile, Symbol, SymbolFlags, get_source_file_of_node};
use tsrs_checker::TypeFormatFlags;
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::{P, tspath};
use verule_adapter_api::{
    AdapterError, DeclarationInfo, Evidence, FactExplainTrace, Gap, HostAccess, HostFactQuery,
    HostQueriedFact, HostValue, NativeAdapterInputAttribution, NativeAdapterOutput,
    NativeContribution, NativeFactRequest, NativeLibrary, NodeKind, NodeOperation, SourceLocation,
};

use crate::calls_values::{
    canonical_symbol_for_node, node_location_rel, node_text, unwrap_expr_wrappers,
};
use crate::observe::{EngineStepDetail, ObserveCollector, elapsed_ms};
use crate::projection::{safe_expression, safe_initializer};
use crate::projects::{is_project_source_file, relative_to_cwd, resolve_against_cwd};
use crate::serve::{HandlePayload, RpcErr, SnapshotState};

pub type NativeAdapterRegistry = BTreeMap<String, Rc<NativeLibrary>>;

/// Whether this snapshot loaded adapter libraries or retained the prior registry.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeAdapterStatus {
    Loaded,
    Reused,
}

/// User-facing identity and load state for one native adapter in the registry.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeAdapterObservation {
    /// Package name supplied by project configuration.
    pub package_name: String,
    /// Provider ID declared by the validated adapter manifest.
    pub provider_id: String,
    /// Whether the active snapshot loaded or retained this library.
    pub status: NativeAdapterStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeAdapterPackageInput {
    package_name: String,
    adapter_id: String,
    library_path: String,
}

/// Summarizes the active registry without exposing resolved library paths.
pub fn observe_registry(
    registry: &NativeAdapterRegistry,
    status: NativeAdapterStatus,
) -> Vec<NativeAdapterObservation> {
    registry
        .iter()
        .map(|(provider_id, library)| NativeAdapterObservation {
            package_name: library.package_name().to_string(),
            provider_id: provider_id.clone(),
            status,
        })
        .collect()
}

pub fn load_configured_adapters(
    inputs: &[NativeAdapterPackageInput],
) -> Result<NativeAdapterRegistry, RpcErr> {
    let mut adapters = BTreeMap::new();
    for input in inputs {
        let library = NativeLibrary::load(&input.package_name, &input.library_path)
            .map_err(|error| RpcErr::new("NATIVE_ADAPTER_LOAD", error.to_string()))?;
        let id = library.manifest().id.clone();
        if id != input.adapter_id {
            return Err(RpcErr::new(
                "NATIVE_ADAPTER_LOAD",
                format!(
                    "Failed to load configured adapter package \"{}\": its JavaScript bridge declares adapter id \"{}\", but its native library manifest declares adapter id \"{}\".",
                    input.package_name, input.adapter_id, id
                ),
            ));
        }
        if let Some(existing) = adapters.get(&id) {
            let existing: &Rc<NativeLibrary> = existing;
            return Err(RpcErr::new(
                "NATIVE_ADAPTER_LOAD",
                format!(
                    "Adapter package \"{}\" duplicates provider id \"{}\" already loaded from \"{}\".",
                    input.package_name,
                    id,
                    existing.package_name()
                ),
            ));
        }
        adapters.insert(id, Rc::new(library));
    }
    Ok(adapters)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum CacheTargetKey {
    Full,
    Targets(Vec<P<Node>>),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct NativeFactCacheKey {
    project_id: String,
    provider_id: String,
    fact_type_id: String,
    condition_key: String,
    targets: CacheTargetKey,
    explain: bool,
}

#[derive(Clone)]
struct CachedContribution {
    subject_node: P<Node>,
    subject_location: Option<SourceLocation>,
    value: Value,
    evidence: Vec<Evidence>,
    gaps: Vec<Gap>,
    explain_trace: Option<FactExplainTrace>,
}

#[derive(Clone)]
struct CachedNativeExtraction {
    contributions: Vec<CachedContribution>,
    additional_inputs: Vec<String>,
    adapter_inputs: Vec<NativeAdapterInputAttribution>,
    additional_input_snapshots: Vec<(String, Option<Vec<u8>>)>,
    sub_queries: Vec<NativeSubQueryTrace>,
}

/// Per-snapshot cache of native adapter extractions, invalidated automatically on `snapshot.update`.
#[derive(Default)]
pub struct NativeFactCache {
    entries: FxHashMap<NativeFactCacheKey, CachedNativeExtraction>,
}

/// Observation trace for a nested `Context::query_facts` call made by a native adapter.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSubQueryTrace {
    pub fact_type_id: String,
    pub providers: Vec<String>,
    pub cache_status: &'static str,
    pub candidate_count: usize,
    pub duration_ms: f64,
    pub subject_locations: Vec<SourceLocation>,
}

/// Output of one top-level native extraction step, including nested sub-query traces.
pub struct NativeExtractionOutcome {
    pub output: NativeAdapterOutput,
    pub sub_queries: Vec<NativeSubQueryTrace>,
}

#[derive(Clone)]
struct ActiveExtraction {
    provider_id: String,
    fact_type_id: String,
    targets: Option<FxHashSet<P<Node>>>,
}

fn canonical_condition_key(condition: Option<&Value>) -> String {
    serde_json::to_string(condition.unwrap_or(&Value::Null)).unwrap_or_else(|_| "null".to_string())
}

fn normalize_target_key(targets: Option<&[P<Node>]>) -> CacheTargetKey {
    let Some(nodes) = targets else {
        return CacheTargetKey::Full;
    };
    let mut normalized = nodes.to_vec();
    normalized.sort_unstable_by_key(|node| (&raw const **node).addr());
    normalized.dedup();
    CacheTargetKey::Targets(normalized)
}

fn additional_inputs_unchanged(cwd: &str, snapshots: &[(String, Option<Vec<u8>>)]) -> bool {
    snapshots.iter().all(|(path, expected)| {
        let resolved = resolve_against_cwd(cwd, path);
        let current = std::fs::read(&resolved).ok();
        current.as_ref() == expected.as_ref()
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Connects the native provider request to one engine snapshot and its observer."
)]
pub fn extract(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    provider_id: &str,
    report_provider_id: Option<&str>,
    fact_type_id: &str,
    target_handles: &[String],
    has_targets: bool,
    condition: Option<&Value>,
    explain: bool,
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<NativeExtractionOutcome, RpcErr> {
    let targets = resolve_targets(snapshot, project_id, target_handles, has_targets)?;
    let target_nodes: Option<Vec<P<Node>>> = targets
        .as_ref()
        .map(|items| items.iter().map(|item| item.node).collect());
    let aliases_by_primary_handle = target_aliases(targets.as_deref());
    let primary_handles_by_node = primary_target_handles(targets.as_deref());

    let mut candidates_cache: Option<Rc<NativeAdapterCandidates>> = None;
    let mut active_extractions = Vec::new();
    let mut pending_conflict: Option<String> = None;

    let (cached, cache_status, manifest_id) = extract_provider_cached(
        snapshot,
        project_id,
        program,
        checker,
        provider_id,
        fact_type_id,
        condition,
        explain,
        target_nodes.as_deref(),
        targets.as_deref(),
        &mut candidates_cache,
        &mut active_extractions,
        &mut pending_conflict,
        observer,
    )
    .map_err(|err| {
        pending_conflict.map_or(err, |conflict_msg| {
            RpcErr::new("FACT_CONFLICT", conflict_msg)
        })
    })?;

    let contributions = materialize_contributions(
        snapshot,
        project_id,
        &cached.contributions,
        &primary_handles_by_node,
        &aliases_by_primary_handle,
    );
    let output = NativeAdapterOutput {
        contributions,
        additional_inputs: cached.additional_inputs,
        adapter_inputs: cached.adapter_inputs,
    };

    let details = if observer.is_detail() {
        output
            .contributions
            .iter()
            .map(|contribution| EngineStepDetail {
                kind: "nativeAdapterFact",
                file: contribution
                    .subject_location
                    .as_ref()
                    .map(|location| location.file.clone()),
                line: contribution
                    .subject_location
                    .as_ref()
                    .map(|location| location.line),
                column: contribution
                    .subject_location
                    .as_ref()
                    .map(|location| location.column),
                summary: format!("{fact_type_id} from {manifest_id}"),
            })
            .collect()
    } else {
        Vec::new()
    };
    observer.record_provider_step_with_cache(
        "extractFacts",
        report_provider_id.unwrap_or(&manifest_id),
        provider_id,
        cache_status,
        step_start,
        output.contributions.len(),
        details,
    );
    Ok(NativeExtractionOutcome {
        output,
        sub_queries: cached.sub_queries,
    })
}

fn primary_target_handles(targets: Option<&[ResolvedNativeTarget]>) -> FxHashMap<P<Node>, String> {
    let mut map = FxHashMap::default();
    for target in targets.into_iter().flatten() {
        map.entry(target.node)
            .or_insert_with(|| target.handle_id.clone());
    }
    map
}

fn materialize_contributions(
    snapshot: &mut SnapshotState,
    project_id: &str,
    cached: &[CachedContribution],
    primary_handles_by_node: &FxHashMap<P<Node>, String>,
    aliases_by_primary_handle: &FxHashMap<String, Vec<String>>,
) -> Vec<NativeContribution> {
    let mut contributions = Vec::with_capacity(cached.len());
    for item in cached {
        let primary = primary_handles_by_node
            .get(&item.subject_node)
            .cloned()
            .unwrap_or_else(|| snapshot.alloc_syntax(project_id, item.subject_node));
        if let Some(handles) = aliases_by_primary_handle.get(&primary) {
            for handle_id in handles {
                contributions.push(NativeContribution {
                    subject: handle_id.clone(),
                    subject_location: item.subject_location.clone(),
                    value: item.value.clone(),
                    evidence: item.evidence.clone(),
                    gaps: item.gaps.clone(),
                    explain_trace: item.explain_trace.clone(),
                });
            }
        } else {
            contributions.push(NativeContribution {
                subject: primary,
                subject_location: item.subject_location.clone(),
                value: item.value.clone(),
                evidence: item.evidence.clone(),
                gaps: item.gaps.clone(),
                explain_trace: item.explain_trace.clone(),
            });
        }
    }
    contributions
}

#[expect(
    clippy::too_many_arguments,
    reason = "Coordinates cache lookup, cycle detection, and recursive native extraction."
)]
fn extract_provider_cached(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    provider_id: &str,
    fact_type_id: &str,
    condition: Option<&Value>,
    explain: bool,
    target_nodes: Option<&[P<Node>]>,
    resolved_targets: Option<&[ResolvedNativeTarget]>,
    candidates_cache: &mut Option<Rc<NativeAdapterCandidates>>,
    active_extractions: &mut Vec<ActiveExtraction>,
    pending_conflict: &mut Option<String>,
    observer: &mut ObserveCollector,
) -> Result<(CachedNativeExtraction, &'static str, String), RpcErr> {
    let adapter = snapshot
        .native_adapters
        .get(provider_id)
        .cloned()
        .ok_or_else(|| {
            RpcErr::new(
                "UNKNOWN_ENGINE_PROVIDER",
                format!("No configured native adapter provides provider id \"{provider_id}\"."),
            )
        })?;
    let manifest_id = adapter.manifest().id.clone();

    let requested_set: Option<FxHashSet<P<Node>>> =
        target_nodes.map(|nodes| nodes.iter().copied().collect());
    if let Some(cycle_msg) = detect_cycle(
        active_extractions,
        provider_id,
        fact_type_id,
        requested_set.as_ref(),
    ) {
        return Err(RpcErr::new("NATIVE_ADAPTER_EXTRACT", cycle_msg));
    }

    let cache_key = NativeFactCacheKey {
        project_id: project_id.to_string(),
        provider_id: provider_id.to_string(),
        fact_type_id: fact_type_id.to_string(),
        condition_key: canonical_condition_key(condition),
        targets: normalize_target_key(target_nodes),
        explain,
    };
    if let Some(cached) = snapshot.native_fact_cache.entries.get(&cache_key)
        && additional_inputs_unchanged(&snapshot.cwd, &cached.additional_input_snapshots)
    {
        let mut reused = cached.clone();
        for sub in &mut reused.sub_queries {
            sub.cache_status = "reused";
        }
        return Ok((reused, "reused", manifest_id));
    }

    let candidates = if let Some(existing) = candidates_cache {
        Rc::clone(existing)
    } else {
        let collected = Rc::new(collect_native_adapter_nodes(
            &snapshot.cwd,
            program,
            checker,
        ));
        *candidates_cache = Some(Rc::clone(&collected));
        collected
    };

    run_uncached_provider_extraction(
        snapshot,
        project_id,
        program,
        checker,
        &adapter,
        provider_id,
        fact_type_id,
        condition,
        explain,
        target_nodes,
        resolved_targets,
        requested_set,
        cache_key,
        &candidates,
        active_extractions,
        pending_conflict,
        observer,
    )
    .map(|cached| (cached, "computed", manifest_id))
}

#[expect(
    clippy::too_many_arguments,
    reason = "Executes one uncached native adapter invocation and populates the snapshot cache."
)]
fn run_uncached_provider_extraction(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    adapter: &NativeLibrary,
    provider_id: &str,
    fact_type_id: &str,
    condition: Option<&Value>,
    explain: bool,
    target_nodes: Option<&[P<Node>]>,
    resolved_targets: Option<&[ResolvedNativeTarget]>,
    requested_set: Option<FxHashSet<P<Node>>>,
    cache_key: NativeFactCacheKey,
    candidates: &Rc<NativeAdapterCandidates>,
    active_extractions: &mut Vec<ActiveExtraction>,
    pending_conflict: &mut Option<String>,
    observer: &mut ObserveCollector,
) -> Result<CachedNativeExtraction, RpcErr> {
    let mut table = NodeTable::default();
    let call_ids = table.list(candidates.calls.iter().copied());
    let variable_ids = table.list(candidates.variables.iter().copied());
    let target_ids = match (resolved_targets, target_nodes) {
        (Some(resolved), _) => Some(
            resolved
                .iter()
                .map(|target| table.handle_with_program_ref(target.node, target.handle_id.clone()))
                .collect::<Vec<_>>(),
        ),
        (None, Some(nodes)) => Some(table.list(nodes.iter().copied())),
        (None, None) => None,
    };

    active_extractions.push(ActiveExtraction {
        provider_id: provider_id.to_string(),
        fact_type_id: fact_type_id.to_string(),
        targets: requested_set,
    });

    let cwd = snapshot.cwd.clone();
    let mut host = SnapshotAdapterHost {
        cwd: cwd.clone(),
        project_id,
        program,
        checker,
        snapshot,
        table,
        candidates: Rc::clone(candidates),
        active_extractions,
        observer,
        explain,
        recorded_input_snapshots: Vec::new(),
        child_additional_inputs: Vec::new(),
        child_adapter_inputs: Vec::new(),
        child_input_snapshots: Vec::new(),
        sub_queries: Vec::new(),
        pending_conflict: None,
    };

    let request = NativeFactRequest {
        fact_type_id,
        provider_id,
        project_id,
        condition,
        calls: &call_ids,
        variables: &variable_ids,
        targets: target_ids.as_deref(),
        explain,
    };
    let extract_result = adapter.extract(&request, &mut host);
    let host_conflict = host.pending_conflict.take();
    let recorded_input_snapshots = std::mem::take(&mut host.recorded_input_snapshots);
    let child_additional_inputs = std::mem::take(&mut host.child_additional_inputs);
    let child_adapter_inputs = std::mem::take(&mut host.child_adapter_inputs);
    let child_input_snapshots = std::mem::take(&mut host.child_input_snapshots);
    let sub_queries = std::mem::take(&mut host.sub_queries);
    let reverse_refs = std::mem::take(&mut host.table.reverse_refs);
    drop(host);
    active_extractions.pop();

    if let Some(conflict_msg) = host_conflict {
        *pending_conflict = Some(conflict_msg.clone());
        return Err(RpcErr::new("FACT_CONFLICT", conflict_msg));
    }

    let output = extract_result.map_err(|error| native_extract_error(adapter, error))?;
    let cached_contributions = resolve_cached_contributions(
        snapshot,
        project_id,
        &adapter.manifest().id,
        &reverse_refs,
        output.contributions,
    )?;

    let additional_inputs = merge_unique_strings(output.additional_inputs, child_additional_inputs);
    let adapter_inputs = merge_adapter_inputs(output.adapter_inputs, child_adapter_inputs);
    let additional_input_snapshots = merge_input_snapshots(
        &cwd,
        &additional_inputs,
        recorded_input_snapshots,
        child_input_snapshots,
    );

    let cached = CachedNativeExtraction {
        contributions: cached_contributions,
        additional_inputs,
        adapter_inputs,
        additional_input_snapshots,
        sub_queries,
    };
    snapshot
        .native_fact_cache
        .entries
        .insert(cache_key, cached.clone());

    Ok(cached)
}

fn resolve_cached_contributions(
    snapshot: &SnapshotState,
    project_id: &str,
    adapter_id: &str,
    reverse_refs: &FxHashMap<String, P<Node>>,
    contributions: Vec<NativeContribution>,
) -> Result<Vec<CachedContribution>, RpcErr> {
    let mut cached = Vec::with_capacity(contributions.len());
    for contrib in contributions {
        let subject_node = reverse_refs
            .get(&contrib.subject)
            .copied()
            .or_else(|| resolve_handle_node(snapshot, project_id, &contrib.subject))
            .ok_or_else(|| {
                RpcErr::new(
                    "NATIVE_ADAPTER_EXTRACT",
                    format!(
                        "Adapter \"{adapter_id}\" returned unknown subject handle \"{}\".",
                        contrib.subject
                    ),
                )
            })?;
        cached.push(CachedContribution {
            subject_node,
            subject_location: contrib.subject_location,
            value: contrib.value,
            evidence: contrib.evidence,
            gaps: contrib.gaps,
            explain_trace: contrib.explain_trace,
        });
    }
    Ok(cached)
}

fn resolve_handle_node(
    snapshot: &SnapshotState,
    project_id: &str,
    handle_id: &str,
) -> Option<P<Node>> {
    let entry = snapshot.get_entry(project_id, handle_id).ok()?;
    match entry.payload {
        HandlePayload::Syntax(node)
        | HandlePayload::Call(node)
        | HandlePayload::Declaration { node, .. } => Some(node),
        _ => None,
    }
}

fn detect_cycle(
    active: &[ActiveExtraction],
    provider_id: &str,
    fact_type_id: &str,
    requested: Option<&FxHashSet<P<Node>>>,
) -> Option<String> {
    let overlaps = active.iter().any(|frame| {
        frame.provider_id == provider_id
            && frame.fact_type_id == fact_type_id
            && match (&frame.targets, requested) {
                (None, _) | (_, None) => true,
                (Some(existing), Some(req)) => req.iter().any(|node| existing.contains(node)),
            }
    });
    if !overlaps {
        return None;
    }
    let mut chain: Vec<String> = active
        .iter()
        .map(|frame| format!("{}({})", frame.provider_id, frame.fact_type_id))
        .collect();
    chain.push(format!("{provider_id}({fact_type_id})"));
    Some(format!(
        "Cyclic fact query detected: {}",
        chain.join(" -> ")
    ))
}

fn merge_unique_strings(primary: Vec<String>, secondary: Vec<String>) -> Vec<String> {
    let mut set = BTreeSet::new();
    set.extend(primary);
    set.extend(secondary);
    set.into_iter().collect()
}

fn merge_adapter_inputs(
    primary: Vec<NativeAdapterInputAttribution>,
    secondary: Vec<NativeAdapterInputAttribution>,
) -> Vec<NativeAdapterInputAttribution> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for item in primary.into_iter().chain(secondary) {
        let key = (item.adapter_id.clone(), item.path.clone());
        if seen.insert(key) {
            out.push(item);
        }
    }
    out.sort_by(|a, b| a.adapter_id.cmp(&b.adapter_id).then(a.path.cmp(&b.path)));
    out
}

fn merge_input_snapshots(
    cwd: &str,
    all_paths: &[String],
    recorded: Vec<(String, Option<Vec<u8>>)>,
    child: Vec<(String, Option<Vec<u8>>)>,
) -> Vec<(String, Option<Vec<u8>>)> {
    let mut by_path: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();
    for (path, bytes) in recorded.into_iter().chain(child) {
        by_path.entry(path).or_insert(bytes);
    }
    for path in all_paths {
        by_path.entry(path.clone()).or_insert_with(|| {
            let resolved = resolve_against_cwd(cwd, path);
            std::fs::read(&resolved).ok()
        });
    }
    by_path.into_iter().collect()
}

fn native_extract_error(adapter: &NativeLibrary, error: AdapterError) -> RpcErr {
    let message = match error {
        AdapterError::UnsupportedFactType { fact_type_id, .. } => format!(
            "Configured native adapter package \"{}\" (adapter id \"{}\") does not provide fact type \"{}\".",
            adapter.package_name(),
            adapter.manifest().id,
            fact_type_id
        ),
        error => error.to_string(),
    };
    RpcErr::new("NATIVE_ADAPTER_EXTRACT", message)
}

fn target_aliases(targets: Option<&[ResolvedNativeTarget]>) -> FxHashMap<String, Vec<String>> {
    let mut handles_by_node: FxHashMap<P<Node>, Vec<String>> = FxHashMap::default();
    for target in targets.into_iter().flatten() {
        let handles = handles_by_node.entry(target.node).or_default();
        if !handles.contains(&target.handle_id) {
            handles.push(target.handle_id.clone());
        }
    }
    handles_by_node
        .into_values()
        .filter(|handles| handles.len() > 1)
        .map(|handles| (handles[0].clone(), handles))
        .collect()
}

struct NativeAdapterCandidates {
    calls: Vec<P<Node>>,
    variables: Vec<P<Node>>,
}

/// Collect syntax candidates for native adapters, including nearby workspace declarations reached from imports and calls.
fn collect_native_adapter_nodes(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
) -> NativeAdapterCandidates {
    let project_files: Vec<P<SourceFile>> = program
        .get_source_files()
        .iter()
        .copied()
        .filter(|&source_file| is_project_source_file(program, source_file))
        .collect();

    let mut calls = Vec::new();
    let mut variables = Vec::new();
    let mut imports = Vec::new();
    for &source_file in &project_files {
        collect_file_syntax(source_file, &mut calls, &mut variables, &mut imports);
    }

    let extra_files = discover_workspace_files(cwd, checker, &project_files, &calls, &imports);
    for source_file in extra_files {
        let mut unused_imports = Vec::new();
        collect_file_syntax(source_file, &mut calls, &mut variables, &mut unused_imports);
    }

    NativeAdapterCandidates { calls, variables }
}

fn collect_file_syntax(
    source_file: P<SourceFile>,
    calls: &mut Vec<P<Node>>,
    variables: &mut Vec<P<Node>>,
    imports: &mut Vec<P<Node>>,
) {
    fn walk(
        node: P<Node>,
        calls: &mut Vec<P<Node>>,
        variables: &mut Vec<P<Node>>,
        imports: &mut Vec<P<Node>>,
    ) {
        match node.kind() {
            Kind::CallExpression => calls.push(node),
            Kind::VariableDeclaration => variables.push(node),
            Kind::ImportSpecifier => imports.push(node),
            _ => {}
        }
        node.for_each_child(&mut |child| {
            walk(child, calls, variables, imports);
            false
        });
    }

    walk(source_file.as_node(), calls, variables, imports);
}

fn discover_workspace_files(
    cwd: &str,
    checker: &mut CheckerHandle,
    project_files: &[P<SourceFile>],
    initial_calls: &[P<Node>],
    initial_imports: &[P<Node>],
) -> Vec<P<SourceFile>> {
    let mut visited_files: FxHashSet<String> = project_files
        .iter()
        .map(|source_file| relative_to_cwd(cwd, source_file.file_name()))
        .collect();
    let mut discovered_files = Vec::new();
    let mut frontier_calls = initial_calls.to_vec();
    let mut frontier_imports = initial_imports.to_vec();

    // Follow workspace entrypoints only two hops so adapters can inspect package sources without walking dependencies.
    for _ in 0..2 {
        let mut references = Vec::with_capacity(frontier_imports.len() + frontier_calls.len() * 3);
        references.extend(frontier_imports.iter().copied());
        for &call in &frontier_calls {
            if let Some(expression) = safe_expression(call) {
                references.push(expression);
                if let Some(inner_expression) = safe_expression(expression) {
                    references.push(inner_expression);
                }
            }
            references.extend(call.arguments().iter().copied());
        }
        if references.is_empty() {
            break;
        }

        let mut next_files = Vec::new();
        for node in references {
            let Some((_, declaration)) = resolve_decl_for_ref(cwd, checker, node) else {
                continue;
            };
            let Some(source_file) = get_source_file_of_node(declaration) else {
                continue;
            };
            let relative_path = relative_to_cwd(cwd, source_file.file_name());
            if is_declaration_file(&relative_path) || is_node_modules_file(&relative_path) {
                continue;
            }
            if visited_files.insert(relative_path) {
                discovered_files.push(source_file);
                next_files.push(source_file);
            }
        }
        if next_files.is_empty() {
            break;
        }

        frontier_calls.clear();
        frontier_imports.clear();
        for source_file in next_files {
            let mut unused_variables = Vec::new();
            collect_file_syntax(
                source_file,
                &mut frontier_calls,
                &mut unused_variables,
                &mut frontier_imports,
            );
        }
    }

    discovered_files
}

fn is_declaration_file(path: &str) -> bool {
    tspath::is_declaration_file_name(path)
}

fn is_node_modules_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.starts_with("node_modules/") || normalized.contains("/node_modules/")
}

fn can_follow_declaration(cwd: &str, declaration: P<Node>) -> bool {
    let Some(source_file) = get_source_file_of_node(declaration) else {
        return false;
    };
    if source_file.is_declaration_file() {
        return false;
    }
    let relative_path = relative_to_cwd(cwd, source_file.file_name());
    !is_declaration_file(&relative_path) && !is_node_modules_file(&relative_path)
}

fn resolve_decl_raw(
    checker: &mut CheckerHandle,
    node: P<Node>,
) -> Option<(Option<String>, P<Node>)> {
    if matches!(node.kind(), Kind::CallExpression | Kind::NewExpression) {
        let signature = checker.get_resolved_signature_exported(node);
        let signature_declaration = (signature != checker.get_unknown_signature())
            .then(|| {
                signature
                    .declaration()
                    .or_else(|| signature.target().and_then(|target| target.declaration()))
            })
            .flatten();
        let callee_symbol = node
            .expression()
            .and_then(|expression| checker.get_symbol_at_location_exported(expression))
            .map(|symbol| unwrap_alias(checker, symbol))
            .filter(|&symbol| symbol != checker.get_unknown_symbol());
        let primary = signature_declaration.or_else(|| {
            callee_symbol.and_then(|symbol| {
                symbol
                    .value_declaration()
                    .or_else(|| symbol.declarations().first().copied())
            })
        })?;
        let name = callee_symbol.map(|symbol| symbol.name().to_string());
        return Some((name, primary));
    }

    let symbol = canonical_symbol_for_node(checker, node)?;
    let primary = symbol
        .value_declaration()
        .or_else(|| symbol.declarations().first().copied())?;
    let name = Some(symbol.name().to_string());
    Some((name, primary))
}

fn resolve_decl_for_ref(
    cwd: &str,
    checker: &mut CheckerHandle,
    node: P<Node>,
) -> Option<(Option<String>, P<Node>)> {
    let (name, primary) = resolve_decl_raw(checker, node)?;
    can_follow_declaration(cwd, primary).then_some((name, primary))
}

fn unwrap_alias(checker: &mut CheckerHandle, mut symbol: P<Symbol>) -> P<Symbol> {
    let mut seen = Vec::new();
    while symbol.flags().intersects(SymbolFlags::Alias) && !seen.contains(&symbol) {
        seen.push(symbol);
        let next = checker.get_aliased_symbol(symbol);
        if next == symbol || next == checker.get_unknown_symbol() {
            break;
        }
        symbol = next;
    }
    symbol
}

struct ResolvedNativeTarget {
    node: P<Node>,
    handle_id: String,
}

fn resolve_targets(
    snapshot: &SnapshotState,
    project_id: &str,
    target_handles: &[String],
    has_targets: bool,
) -> Result<Option<Vec<ResolvedNativeTarget>>, RpcErr> {
    if !has_targets {
        return Ok(None);
    }
    let mut nodes = Vec::with_capacity(target_handles.len());
    for handle_id in target_handles {
        let entry = snapshot.get_entry(project_id, handle_id)?;
        let (HandlePayload::Syntax(node)
        | HandlePayload::Call(node)
        | HandlePayload::Declaration { node, .. }) = entry.payload
        else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!(
                    "Program reference {handle_id} cannot be used as a native adapter fact target."
                ),
            ));
        };
        nodes.push(ResolvedNativeTarget {
            node,
            handle_id: handle_id.clone(),
        });
    }
    Ok(Some(nodes))
}

#[derive(Default)]
struct NodeTable {
    nodes: Vec<P<Node>>,
    ids: FxHashMap<P<Node>, u64>,
    program_refs: FxHashMap<P<Node>, String>,
    reverse_refs: FxHashMap<String, P<Node>>,
}

impl NodeTable {
    fn handle(&mut self, node: P<Node>) -> u64 {
        if let Some(id) = self.ids.get(&node) {
            return *id;
        }
        self.nodes.push(node);
        let id = self.nodes.len() as u64;
        self.ids.insert(node, id);
        id
    }

    fn handle_with_program_ref(&mut self, node: P<Node>, handle_id: String) -> u64 {
        let id = self.handle(node);
        self.reverse_refs.insert(handle_id.clone(), node);
        self.program_refs.entry(node).or_insert(handle_id);
        id
    }

    fn list(&mut self, nodes: impl IntoIterator<Item = P<Node>>) -> Vec<u64> {
        nodes.into_iter().map(|node| self.handle(node)).collect()
    }

    fn get(&self, id: u64) -> Option<P<Node>> {
        id.checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| self.nodes.get(index))
            .copied()
    }
}

struct SnapshotAdapterHost<'a> {
    cwd: String,
    project_id: &'a str,
    program: &'static Program,
    checker: &'a mut CheckerHandle,
    snapshot: &'a mut SnapshotState,
    table: NodeTable,
    candidates: Rc<NativeAdapterCandidates>,
    active_extractions: &'a mut Vec<ActiveExtraction>,
    observer: &'a mut ObserveCollector,
    explain: bool,
    recorded_input_snapshots: Vec<(String, Option<Vec<u8>>)>,
    child_additional_inputs: Vec<String>,
    child_adapter_inputs: Vec<NativeAdapterInputAttribution>,
    child_input_snapshots: Vec<(String, Option<Vec<u8>>)>,
    sub_queries: Vec<NativeSubQueryTrace>,
    pending_conflict: Option<String>,
}

impl SnapshotAdapterHost<'_> {
    fn read_declaration_info(&mut self, node: P<Node>) -> Option<DeclarationInfo> {
        resolve_decl_raw(self.checker, node).map(|(name, declaration)| {
            let source_file = get_source_file_of_node(declaration);
            let provenance = crate::package_impl::resolve_declaration_provenance(
                self.program,
                self.checker,
                node,
                declaration,
            );
            let syntax = can_follow_declaration(&self.cwd, declaration)
                .then(|| self.table.handle(declaration));
            DeclarationInfo {
                name,
                kind: Some(format!("{:?}", declaration.kind())),
                location: node_location_rel(&self.cwd, declaration).map(to_api_location),
                syntax,
                is_external: source_file.is_some_and(|file| {
                    file.is_declaration_file()
                        || !crate::projects::is_project_source_file(self.program, file)
                }),
                package_name: provenance.package_name,
                export_name: provenance.export_name,
            }
        })
    }

    fn program_reference(&mut self, node: P<Node>) -> HostValue {
        let handle = self
            .table
            .program_refs
            .get(&node)
            .cloned()
            .unwrap_or_else(|| {
                let allocated = self.snapshot.alloc_syntax(self.project_id, node);
                self.table.reverse_refs.insert(allocated.clone(), node);
                allocated
            });
        HostValue::Text(Some(handle))
    }
}

impl HostAccess for SnapshotAdapterHost<'_> {
    fn read_node(&mut self, node_id: u64, operation: NodeOperation) -> Result<HostValue, String> {
        let node = self
            .table
            .get(node_id)
            .ok_or_else(|| format!("unknown syntax handle {node_id}"))?;
        let value = match operation {
            NodeOperation::Kind => HostValue::Kind(map_node_kind(node.kind())),
            NodeOperation::KindLabel => HostValue::Text(Some(format!("{:?}", node.kind()))),
            NodeOperation::Name => {
                HostValue::Node(node.name().map(|child| self.table.handle(child)))
            }
            NodeOperation::Expression => {
                HostValue::Node(node.expression().map(|child| self.table.handle(child)))
            }
            NodeOperation::Body => {
                HostValue::Node(node.body().map(|child| self.table.handle(child)))
            }
            NodeOperation::Initializer => {
                HostValue::Node(node.initializer().map(|child| self.table.handle(child)))
            }
            NodeOperation::SafeInitializer => {
                HostValue::Node(safe_initializer(node).map(|child| self.table.handle(child)))
            }
            NodeOperation::UnwrapExpression => {
                HostValue::Node(Some(self.table.handle(unwrap_expr_wrappers(node))))
            }
            NodeOperation::Text => HostValue::Text(node_text(node)),
            NodeOperation::TypeText => {
                let ty = self.checker.get_type_at_location(node);
                HostValue::Text(Some(self.checker.type_to_string_ex_exported(
                    ty,
                    Some(node),
                    TypeFormatFlags::None,
                    None,
                )))
            }
            NodeOperation::Location => {
                HostValue::Location(node_location_rel(&self.cwd, node).map(to_api_location))
            }
            NodeOperation::Declaration => HostValue::Declaration(self.read_declaration_info(node)),
            NodeOperation::DeclarationNode => {
                let declaration = resolve_decl_for_ref(&self.cwd, self.checker, node)
                    .map(|(_, declaration)| declaration);
                HostValue::Node(declaration.map(|declaration| self.table.handle(declaration)))
            }
            NodeOperation::Arguments => {
                let children = if matches!(node.kind(), Kind::CallExpression | Kind::NewExpression)
                {
                    node.arguments()
                        .iter()
                        .map(|child| self.table.handle(*child))
                        .collect()
                } else {
                    Vec::new()
                };
                HostValue::Nodes(children)
            }
            NodeOperation::Properties => {
                let children = if node.kind() == Kind::ObjectLiteralExpression {
                    node.as_object_literal_expression()
                        .properties()
                        .nodes()
                        .iter()
                        .map(|child| self.table.handle(*child))
                        .collect()
                } else {
                    Vec::new()
                };
                HostValue::Nodes(children)
            }
            NodeOperation::Children => {
                let mut children = Vec::new();
                node.for_each_child(&mut |child| {
                    children.push(self.table.handle(child));
                    false
                });
                HostValue::Nodes(children)
            }
            NodeOperation::ProgramReference => self.program_reference(node),
        };
        Ok(value)
    }

    fn read_additional_input(&mut self, path: &str) -> Result<String, String> {
        let resolved = resolve_against_cwd(&self.cwd, path);
        let bytes = std::fs::read(&resolved).map_err(|error| {
            format!("failed to read adapter input \"{path}\" from \"{resolved}\": {error}")
        })?;
        let text = String::from_utf8(bytes.clone()).map_err(|error| {
            format!("failed to decode adapter input \"{path}\" from \"{resolved}\": {error}")
        })?;
        self.recorded_input_snapshots
            .push((path.to_string(), Some(bytes)));
        Ok(text)
    }

    fn register_additional_input(&mut self, path: &str) -> Result<(), String> {
        if path.trim().is_empty() {
            return Err("additional input path must not be empty".to_string());
        }
        let resolved = resolve_against_cwd(&self.cwd, path);
        let bytes = std::fs::read(&resolved).ok();
        self.recorded_input_snapshots
            .push((path.to_string(), bytes));
        Ok(())
    }

    fn query_facts(&mut self, query: HostFactQuery) -> Result<Vec<HostQueriedFact>, String> {
        let started_at = Instant::now();
        let target_nodes = match resolve_query_target_nodes(&self.table, query.targets.as_deref())?
        {
            QueryTargets::Empty => {
                self.record_empty_sub_query(query.fact_type_id, started_at);
                return Ok(Vec::new());
            }
            QueryTargets::Resolved(nodes) => nodes,
        };
        let matched_providers = select_matching_providers(
            &self.snapshot.native_adapters,
            &query.fact_type_id,
            query.provider_id.as_deref(),
        );
        if matched_providers.is_empty() {
            self.record_empty_sub_query(query.fact_type_id, started_at);
            return Ok(Vec::new());
        }
        self.execute_matched_sub_queries(
            &query.fact_type_id,
            query.condition.as_ref(),
            target_nodes.as_deref(),
            &matched_providers,
            started_at,
        )
    }
}

enum QueryTargets {
    Empty,
    Resolved(Option<Vec<P<Node>>>),
}

fn resolve_query_target_nodes(
    table: &NodeTable,
    target_ids: Option<&[u64]>,
) -> Result<QueryTargets, String> {
    let Some(ids) = target_ids else {
        return Ok(QueryTargets::Resolved(None));
    };
    if ids.is_empty() {
        return Ok(QueryTargets::Empty);
    }
    let mut resolved = Vec::with_capacity(ids.len());
    for &id in ids {
        let node = table
            .get(id)
            .ok_or_else(|| format!("unknown syntax handle {id}"))?;
        resolved.push(node);
    }
    Ok(QueryTargets::Resolved(Some(resolved)))
}

fn select_matching_providers(
    adapters: &NativeAdapterRegistry,
    fact_type_id: &str,
    provider_id: Option<&str>,
) -> Vec<String> {
    provider_id.map_or_else(
        || {
            adapters
                .iter()
                .filter(|(_, adapter)| {
                    adapter
                        .manifest()
                        .fact_type_ids
                        .iter()
                        .any(|id| id == fact_type_id)
                })
                .map(|(id, _)| id.clone())
                .collect()
        },
        |pid| {
            adapters
                .get(pid)
                .filter(|adapter| {
                    adapter
                        .manifest()
                        .fact_type_ids
                        .iter()
                        .any(|id| id == fact_type_id)
                })
                .map_or_default(|_| vec![pid.to_string()])
        },
    )
}

impl SnapshotAdapterHost<'_> {
    fn record_empty_sub_query(&mut self, fact_type_id: String, started_at: Instant) {
        self.sub_queries.push(NativeSubQueryTrace {
            fact_type_id,
            providers: Vec::new(),
            cache_status: "computed",
            candidate_count: 0,
            duration_ms: elapsed_ms(started_at),
            subject_locations: Vec::new(),
        });
    }

    fn execute_matched_sub_queries(
        &mut self,
        fact_type_id: &str,
        condition: Option<&Value>,
        target_nodes: Option<&[P<Node>]>,
        matched_providers: &[String],
        started_at: Instant,
    ) -> Result<Vec<HostQueriedFact>, String> {
        let mut candidates_cache = Some(Rc::clone(&self.candidates));
        let mut all_reused = true;
        let mut per_provider: Vec<(String, Vec<CachedContribution>)> =
            Vec::with_capacity(matched_providers.len());

        for pid in matched_providers {
            let step_start = self.observer.start_step();
            let (cached, cache_status, manifest_id) = extract_provider_cached(
                self.snapshot,
                self.project_id,
                self.program,
                self.checker,
                pid,
                fact_type_id,
                condition,
                self.explain,
                target_nodes,
                None,
                &mut candidates_cache,
                self.active_extractions,
                &mut self.pending_conflict,
                self.observer,
            )
            .map_err(|rpc_err| rpc_err.message)?;

            if cache_status != "reused" {
                all_reused = false;
            }

            let details = if self.observer.is_detail() {
                cached
                    .contributions
                    .iter()
                    .map(|contribution| EngineStepDetail {
                        kind: "nativeAdapterFact",
                        file: contribution
                            .subject_location
                            .as_ref()
                            .map(|location| location.file.clone()),
                        line: contribution
                            .subject_location
                            .as_ref()
                            .map(|location| location.line),
                        column: contribution
                            .subject_location
                            .as_ref()
                            .map(|location| location.column),
                        summary: format!("{fact_type_id} from {manifest_id}"),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            self.observer.record_provider_step_with_cache(
                "extractFacts",
                &manifest_id,
                pid,
                cache_status,
                step_start,
                cached.contributions.len(),
                details,
            );

            self.child_additional_inputs
                .extend(cached.additional_inputs);
            self.child_adapter_inputs.extend(cached.adapter_inputs);
            self.child_input_snapshots
                .extend(cached.additional_input_snapshots);
            self.sub_queries.extend(cached.sub_queries);
            per_provider.push((pid.clone(), cached.contributions));
        }

        let merged = match merge_queried_contributions(fact_type_id, per_provider) {
            Ok(items) => items,
            Err(conflict_msg) => {
                self.pending_conflict = Some(conflict_msg.clone());
                return Err(conflict_msg);
            }
        };

        let mut subject_locations = Vec::new();
        let mut out = Vec::with_capacity(merged.len());
        for item in merged {
            if let Some(loc) = item.subject_location {
                subject_locations.push(loc);
            }
            let subject_node_id = self.table.handle(item.subject_node);
            out.push(HostQueriedFact {
                subject_node_id,
                value: item.value,
                evidence: item.evidence,
                gaps: item.gaps,
                providers: item.providers,
                explain_trace: item.explain_trace,
            });
        }

        self.sub_queries.push(NativeSubQueryTrace {
            fact_type_id: fact_type_id.to_string(),
            providers: matched_providers.to_vec(),
            cache_status: if all_reused { "reused" } else { "computed" },
            candidate_count: out.len(),
            duration_ms: elapsed_ms(started_at),
            subject_locations,
        });

        Ok(out)
    }
}

struct MergedQueriedItem {
    subject_node: P<Node>,
    subject_location: Option<SourceLocation>,
    value: Value,
    evidence: Vec<Evidence>,
    gaps: Vec<Gap>,
    providers: Vec<String>,
    explain_trace: Option<FactExplainTrace>,
}

fn merge_queried_contributions(
    fact_type_id: &str,
    per_provider: Vec<(String, Vec<CachedContribution>)>,
) -> Result<Vec<MergedQueriedItem>, String> {
    let mut ordered_nodes: Vec<P<Node>> = Vec::new();
    let mut by_node: FxHashMap<P<Node>, MergedQueriedItem> = FxHashMap::default();

    for (provider_id, contributions) in per_provider {
        for contrib in contributions {
            if let Some(existing) = by_node.get_mut(&contrib.subject_node) {
                merge_json_values(
                    fact_type_id,
                    existing.subject_location.as_ref(),
                    &existing.providers,
                    &mut existing.value,
                    &provider_id,
                    &contrib.value,
                    &existing.evidence,
                    &contrib.evidence,
                )?;
                if !existing.providers.contains(&provider_id) {
                    existing.providers.push(provider_id.clone());
                    existing.providers.sort();
                }
                existing.evidence = dedupe_evidence(
                    existing
                        .evidence
                        .drain(..)
                        .chain(contrib.evidence)
                        .collect(),
                );
                existing.gaps = dedupe_gaps(existing.gaps.drain(..).chain(contrib.gaps).collect());
                if let Some(incoming_trace) = contrib.explain_trace {
                    let target = existing
                        .explain_trace
                        .get_or_insert_with(FactExplainTrace::default);
                    target.local_gaps.extend(incoming_trace.local_gaps);
                }
                if existing.subject_location.is_none() {
                    existing.subject_location = contrib.subject_location;
                }
            } else {
                ordered_nodes.push(contrib.subject_node);
                by_node.insert(
                    contrib.subject_node,
                    MergedQueriedItem {
                        subject_node: contrib.subject_node,
                        subject_location: contrib.subject_location,
                        value: contrib.value,
                        evidence: dedupe_evidence(contrib.evidence),
                        gaps: dedupe_gaps(contrib.gaps),
                        providers: vec![provider_id.clone()],
                        explain_trace: contrib.explain_trace,
                    },
                );
            }
        }
    }

    Ok(ordered_nodes
        .into_iter()
        .filter_map(|node| by_node.remove(&node))
        .collect())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Formats detailed conflict diagnostics naming both providers and their evidence."
)]
fn merge_json_values(
    fact_type_id: &str,
    location: Option<&SourceLocation>,
    existing_providers: &[String],
    existing_value: &mut Value,
    incoming_provider: &str,
    incoming_value: &Value,
    existing_evidence: &[Evidence],
    incoming_evidence: &[Evidence],
) -> Result<(), String> {
    if existing_value == incoming_value {
        return Ok(());
    }
    if let (Value::Object(existing_map), Value::Object(incoming_map)) =
        (existing_value.clone(), incoming_value)
    {
        let mut merged = existing_map;
        let mut conflict_key: Option<String> = None;
        for (key, incoming_val) in incoming_map {
            if let Some(existing_val) = merged.get(key) {
                if existing_val != incoming_val {
                    conflict_key = Some(key.clone());
                    break;
                }
            } else {
                merged.insert(key.clone(), incoming_val.clone());
            }
        }
        if let Some(key) = conflict_key {
            return Err(format_native_conflict(
                fact_type_id,
                location,
                existing_providers,
                existing_value,
                incoming_provider,
                incoming_value,
                existing_evidence,
                incoming_evidence,
                &format!("providers disagreed on field \"{key}\""),
            ));
        }
        *existing_value = Value::Object(merged);
        return Ok(());
    }

    Err(format_native_conflict(
        fact_type_id,
        location,
        existing_providers,
        existing_value,
        incoming_provider,
        incoming_value,
        existing_evidence,
        incoming_evidence,
        "providers produced differing definite conclusions",
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "Produces deterministic conflict error message naming both parties and evidence."
)]
fn format_native_conflict(
    fact_type_id: &str,
    location: Option<&SourceLocation>,
    existing_providers: &[String],
    existing_value: &Value,
    incoming_provider: &str,
    incoming_value: &Value,
    existing_evidence: &[Evidence],
    incoming_evidence: &[Evidence],
    reason: &str,
) -> String {
    let existing_label = existing_providers.join(",");
    let loc_suffix = location.map_or_else(String::new, |loc| {
        format!(" at {}:{}:{}", loc.file, loc.line, loc.column)
    });
    let format_ev = |items: &[Evidence]| -> String {
        if items.is_empty() {
            return "none".to_string();
        }
        items
            .iter()
            .map(|ev| {
                ev.at.as_ref().map_or_else(
                    || ev.message.clone(),
                    |at| format!("{} ({}:{}:{})", ev.message, at.file, at.line, at.column),
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let (left_id, left_val, left_ev, right_id, right_val, right_ev) =
        if existing_label.as_str() <= incoming_provider {
            (
                existing_label.as_str(),
                existing_value,
                existing_evidence,
                incoming_provider,
                incoming_value,
                incoming_evidence,
            )
        } else {
            (
                incoming_provider,
                incoming_value,
                incoming_evidence,
                existing_label.as_str(),
                existing_value,
                existing_evidence,
            )
        };
    format!(
        "Fact conflict for \"{fact_type_id}\"{loc_suffix} between [{left_id}] and [{right_id}]: {reason}. [{left_id}] concluded {left_val} (evidence: {}). [{right_id}] concluded {right_val} (evidence: {}).",
        format_ev(left_ev),
        format_ev(right_ev),
    )
}

fn dedupe_evidence(items: Vec<Evidence>) -> Vec<Evidence> {
    let mut seen = FxHashSet::default();
    let mut out = Vec::new();
    for item in items {
        let key = serde_json::to_string(&item).unwrap_or_default();
        if seen.insert(key) {
            out.push(item);
        }
    }
    out
}

fn dedupe_gaps(items: Vec<Gap>) -> Vec<Gap> {
    let mut seen = FxHashSet::default();
    let mut out = Vec::new();
    for item in items {
        let key = serde_json::to_string(&item).unwrap_or_default();
        if seen.insert(key) {
            out.push(item);
        }
    }
    out
}

fn to_api_location(location: crate::projection::SourceLocation) -> SourceLocation {
    SourceLocation {
        file: location.file,
        line: location.line,
        column: location.column,
        end_line: location.end_line,
        end_column: location.end_column,
        start: location.start,
        end: location.end,
    }
}

const fn map_node_kind(kind: Kind) -> NodeKind {
    match kind {
        Kind::ArrowFunction => NodeKind::ArrowFunction,
        Kind::Block => NodeKind::Block,
        Kind::CallExpression => NodeKind::CallExpression,
        Kind::ComputedPropertyName => NodeKind::ComputedPropertyName,
        Kind::FunctionExpression => NodeKind::FunctionExpression,
        Kind::Identifier => NodeKind::Identifier,
        Kind::ImportSpecifier => NodeKind::ImportSpecifier,
        Kind::NewExpression => NodeKind::NewExpression,
        Kind::NoSubstitutionTemplateLiteral => NodeKind::NoSubstitutionTemplateLiteral,
        Kind::NumericLiteral => NodeKind::NumericLiteral,
        Kind::ObjectLiteralExpression => NodeKind::ObjectLiteralExpression,
        Kind::PropertyAccessExpression => NodeKind::PropertyAccessExpression,
        Kind::PropertyAssignment => NodeKind::PropertyAssignment,
        Kind::ReturnStatement => NodeKind::ReturnStatement,
        Kind::ShorthandPropertyAssignment => NodeKind::ShorthandPropertyAssignment,
        Kind::SpreadAssignment => NodeKind::SpreadAssignment,
        Kind::StringLiteral => NodeKind::StringLiteral,
        Kind::UndefinedKeyword => NodeKind::UndefinedKeyword,
        Kind::VariableDeclaration => NodeKind::VariableDeclaration,
        Kind::ArrayLiteralExpression => NodeKind::ArrayLiteralExpression,
        Kind::ElementAccessExpression => NodeKind::ElementAccessExpression,
        Kind::TrueKeyword => NodeKind::TrueKeyword,
        Kind::FalseKeyword => NodeKind::FalseKeyword,
        Kind::NullKeyword => NodeKind::NullKeyword,
        Kind::PropertyDeclaration => NodeKind::PropertyDeclaration,
        Kind::FunctionDeclaration => NodeKind::FunctionDeclaration,
        Kind::Parameter => NodeKind::Parameter,
        Kind::BindingElement => NodeKind::BindingElement,
        Kind::AsExpression => NodeKind::AsExpression,
        Kind::NonNullExpression => NodeKind::NonNullExpression,
        Kind::ParenthesizedExpression => NodeKind::ParenthesizedExpression,
        Kind::SatisfiesExpression => NodeKind::SatisfiesExpression,
        Kind::BinaryExpression => NodeKind::BinaryExpression,
        Kind::ConditionalExpression => NodeKind::ConditionalExpression,
        Kind::PrefixUnaryExpression => NodeKind::PrefixUnaryExpression,
        Kind::TemplateExpression => NodeKind::TemplateExpression,
        Kind::TemplateHead => NodeKind::TemplateHead,
        Kind::TemplateSpan => NodeKind::TemplateSpan,
        _ => NodeKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_condition_key_is_order_independent() {
        let a = json!({ "b": 2, "a": { "d": 4, "c": 3 } });
        let b = json!({ "a": { "c": 3, "d": 4 }, "b": 2 });
        assert_eq!(
            canonical_condition_key(Some(&a)),
            canonical_condition_key(Some(&b))
        );
        assert_eq!(canonical_condition_key(None), "null");
    }

    #[test]
    fn detect_cycle_catches_reentrant_provider_fact_type() {
        let active = vec![
            ActiveExtraction {
                provider_id: "b".to_string(),
                fact_type_id: "b.fact".to_string(),
                targets: None,
            },
            ActiveExtraction {
                provider_id: "a".to_string(),
                fact_type_id: "a.fact".to_string(),
                targets: None,
            },
        ];
        let cycle = detect_cycle(&active, "b", "b.fact", None).expect("cycle expected");
        assert!(
            cycle.contains("b(b.fact) -> a(a.fact) -> b(b.fact)"),
            "{cycle}"
        );
        assert!(detect_cycle(&active, "c", "c.fact", None).is_none());
    }

    #[test]
    fn merge_json_values_merges_complementary_and_rejects_contradictory_keys() {
        let mut merged = json!({ "a": 1 });
        let incoming_ok = json!({ "b": 2 });
        merge_json_values(
            "demo.fact",
            None,
            &["provider-a".to_string()],
            &mut merged,
            "provider-b",
            &incoming_ok,
            &[],
            &[],
        )
        .expect("disjoint keys should merge");
        assert_eq!(merged, json!({ "a": 1, "b": 2 }));

        let mut conflict_left = json!({ "mode": "required" });
        let conflict_right = json!({ "mode": "optional" });
        let ev_left = vec![Evidence {
            kind: "source".to_string(),
            message: "left evidence".to_string(),
            at: None,
            model_id: None,
            declaration: None,
        }];
        let ev_right = vec![Evidence {
            kind: "source".to_string(),
            message: "right evidence".to_string(),
            at: None,
            model_id: None,
            declaration: None,
        }];
        let err = merge_json_values(
            "demo.fact",
            None,
            &["provider-b".to_string()],
            &mut conflict_left,
            "provider-a",
            &conflict_right,
            &ev_left,
            &ev_right,
        )
        .expect_err("disagreeing keys should conflict");
        assert!(err.contains("[provider-a] and [provider-b]"), "{err}");
        assert!(err.contains("left evidence"), "{err}");
        assert!(err.contains("right evidence"), "{err}");
    }

    #[test]
    fn additional_inputs_unchanged_detects_disk_edits() {
        let dir =
            std::env::temp_dir().join(format!("verule-native-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let file = dir.join("input.json");
        std::fs::write(&file, b"\"v1\"").expect("write v1");

        let cwd = dir.to_string_lossy().replace('\\', "/");
        let snapshots = vec![("input.json".to_string(), Some(b"\"v1\"".to_vec()))];
        assert!(additional_inputs_unchanged(&cwd, &snapshots));

        std::fs::write(&file, b"\"v2\"").expect("write v2");
        assert!(!additional_inputs_unchanged(&cwd, &snapshots));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
