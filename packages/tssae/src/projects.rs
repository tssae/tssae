use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write as _,
    path::Path,
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use tsrs_api::{Handler, Response, Session, session::SnapshotData};
use tsrs_ast::Diagnostic as TsrsDiagnostic;
use tsrs_compiler::Program;
use tsrs_core::{
    CompilerOptions, JsxEmit, ModuleKind, ModuleResolutionKind, ScriptTarget,
    resolve_project_reference_path,
};
use tsrs_diagnostics::Category;
use tsrs_project::ID as ProjectId;

#[derive(Debug)]
pub struct ProjectOpenError {
    pub code: &'static str,
    pub message: String,
}

impl ProjectOpenError {
    const fn new(code: &'static str, message: String) -> Self {
        Self { code, message }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Mirrors four independent TypeScript compiler boolean flags in JSON output."
)]
pub struct CompilerOptionsSummary {
    pub strict: bool,
    pub allow_js: bool,
    pub check_js: bool,
    pub composite: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jsx: Option<String>,
    pub paths: BTreeMap<String, Vec<String>>,
    pub custom_conditions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub project_id: String,
    pub config_file: String,
    pub entry_config: String,
    pub entry_chain: Vec<String>,
    pub referenced_by: Vec<String>,
    pub project_references: Vec<String>,
    pub compiler_options: CompilerOptionsSummary,
    pub source_files: Vec<String>,
}

pub struct OpenedProjectsSnapshot {
    pub tsrs_snapshot_id: u64,
    pub snapshot_data: Arc<SnapshotData>,
    pub entry_specs: Vec<String>,
    pub opened_configs: Vec<String>,
    pub project_ids: BTreeMap<String, ProjectId>,
    pub summaries: Vec<ProjectSummary>,
    pub workspace_source_files: Vec<String>,
    pub snapshot_digest: String,
}

pub struct UpdateSnapshotInput<'a> {
    pub cwd: &'a str,
    pub old_tsrs_snapshot_id: u64,
    pub old_entry_specs: &'a [String],
    pub old_opened_configs: &'a [String],
    pub old_snapshot_data: &'a SnapshotData,
    pub old_project_ids: &'a BTreeMap<String, ProjectId>,
    pub entries: Option<&'a [String]>,
    pub changed: &'a [String],
    pub created: &'a [String],
    pub deleted: &'a [String],
}

pub struct UpdatedProjectsSnapshot {
    pub opened: OpenedProjectsSnapshot,
    pub recomputed_projects: Vec<String>,
    pub reused_projects: Vec<String>,
    pub recomputed_files: Vec<String>,
    pub reused_files: Vec<String>,
    pub removed_files: Vec<String>,
}

#[derive(Deserialize)]
struct TsrsSnapshotResponse {
    snapshot: u64,
}

struct ProjectProvenance {
    entry_config: String,
    entry_chain: Vec<String>,
    referenced_by: BTreeSet<String>,
}

struct DiscoveryState {
    visited_configs: BTreeSet<String>,
    discovery_order: Vec<String>,
    provenance: BTreeMap<String, ProjectProvenance>,
    project_refs_map: BTreeMap<String, Vec<String>>,
}

pub use tssae_tsrs_boundary::{
    is_project_source_file, normalize_path, relative_to_cwd, resolve_against_cwd,
};

pub fn release_tsrs_snapshot(session: &Session, tsrs_snapshot_id: u64) {
    if let Ok(payload) = serde_json::to_vec(&serde_json::json!({ "snapshot": tsrs_snapshot_id })) {
        let _ = session.handle_request("release", &payload);
    }
}

fn release_if_intermediate(session: &Session, candidate_id: u64, protected_id: u64) {
    if candidate_id != protected_id {
        release_tsrs_snapshot(session, candidate_id);
    }
}

/// Opens all entry `tsconfig.json`/`jsconfig.json` files and recursively discovers `references`
/// via BFS, preventing cycles and releasing intermediate transition snapshots.
pub fn open_projects_snapshot(
    session: &Arc<Session>,
    cwd: &str,
    entries: &[String],
) -> Result<OpenedProjectsSnapshot, ProjectOpenError> {
    let (initial_entries, mut state) = collect_initial_entries(cwd, entries)?;
    let mut tsrs_snapshot_id = create_initial_snapshot(session, &initial_entries)?;
    let mut pending_inspection: VecDeque<String> = initial_entries.into_iter().collect();

    loop {
        let snapshot_data = match session.snapshot_data(tsrs_snapshot_id) {
            Ok(data) => data,
            Err(err) => {
                release_tsrs_snapshot(session, tsrs_snapshot_id);
                return Err(ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()));
            }
        };

        let newly_discovered = match inspect_pending_projects(
            cwd,
            &snapshot_data,
            &mut pending_inspection,
            &mut state,
        ) {
            Ok(next) => next,
            Err(err) => {
                drop(snapshot_data);
                release_tsrs_snapshot(session, tsrs_snapshot_id);
                return Err(err);
            }
        };

        if newly_discovered.is_empty() {
            let result = build_opened_snapshot(
                tsrs_snapshot_id,
                cwd,
                entries.to_vec(),
                snapshot_data,
                &state,
            );
            if result.is_err() {
                release_tsrs_snapshot(session, tsrs_snapshot_id);
            }
            return result;
        }

        drop(snapshot_data);
        let prev_snapshot_id = tsrs_snapshot_id;
        let next_snapshot_id =
            match update_snapshot_open_projects(session, prev_snapshot_id, &newly_discovered) {
                Ok(id) => id,
                Err(err) => {
                    release_tsrs_snapshot(session, prev_snapshot_id);
                    return Err(err);
                }
            };
        release_tsrs_snapshot(session, prev_snapshot_id);
        tsrs_snapshot_id = next_snapshot_id;
        pending_inspection.extend(newly_discovered);
    }
}

/// Incrementally updates an active snapshot with file notifications and optional entry changes,
/// preserving `old_tsrs_snapshot_id` untouched on failure so watch sessions can recover cleanly.
pub fn update_projects_snapshot(
    session: &Arc<Session>,
    input: &UpdateSnapshotInput<'_>,
) -> Result<UpdatedProjectsSnapshot, ProjectOpenError> {
    let effective_entries: Vec<String> = input
        .entries
        .map_or_else(|| input.old_entry_specs.to_vec(), <[String]>::to_vec);
    let (initial_entries, mut state) = collect_initial_entries(input.cwd, &effective_entries)?;

    let mut tsrs_snapshot_id = send_initial_incremental_update(session, input, &initial_entries)?;

    let snapshot_data = discover_updated_references(
        session,
        input.cwd,
        input.old_tsrs_snapshot_id,
        &mut tsrs_snapshot_id,
        initial_entries,
        &mut state,
    )?;

    let (tsrs_snapshot_id, snapshot_data) = close_unreachable_projects(
        session,
        input.old_tsrs_snapshot_id,
        tsrs_snapshot_id,
        snapshot_data,
        input.old_opened_configs,
        &state.visited_configs,
    )?;

    let opened = match build_opened_snapshot(
        tsrs_snapshot_id,
        input.cwd,
        effective_entries,
        snapshot_data,
        &state,
    ) {
        Ok(opened) => opened,
        Err(err) => {
            release_if_intermediate(session, tsrs_snapshot_id, input.old_tsrs_snapshot_id);
            return Err(err);
        }
    };

    let diff = compute_recomputation_scope(
        input.cwd,
        input.old_snapshot_data,
        input.old_project_ids,
        &opened.snapshot_data,
        &opened.project_ids,
    );

    Ok(UpdatedProjectsSnapshot {
        opened,
        recomputed_projects: diff.recomputed_projects,
        reused_projects: diff.reused_projects,
        recomputed_files: diff.recomputed_files,
        reused_files: diff.reused_files,
        removed_files: diff.removed_files,
    })
}

fn send_initial_incremental_update(
    session: &Session,
    input: &UpdateSnapshotInput<'_>,
    initial_entries: &[String],
) -> Result<u64, ProjectOpenError> {
    let old_open_set: BTreeSet<&str> = input
        .old_opened_configs
        .iter()
        .map(String::as_str)
        .collect();
    let open_projects: Vec<&str> = initial_entries
        .iter()
        .map(String::as_str)
        .filter(|cfg| !old_open_set.contains(cfg))
        .collect();
    let close_projects: Vec<&str> = input
        .old_opened_configs
        .iter()
        .map(String::as_str)
        .filter(|cfg| !Path::new(cfg).is_file())
        .collect();
    let resolve_list = |paths: &[String]| -> Vec<String> {
        paths
            .iter()
            .map(|item| resolve_against_cwd(input.cwd, item))
            .collect()
    };

    let payload = serde_json::to_vec(&serde_json::json!({
        "snapshot": input.old_tsrs_snapshot_id,
        "changes": {
            "openProjects": open_projects,
            "closeProjects": close_projects,
            "ensurePrograms": true,
            "fileNotifications": {
                "changed": resolve_list(input.changed),
                "created": resolve_list(input.created),
                "deleted": resolve_list(input.deleted),
            }
        }
    }))
    .map_err(|err| ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()))?;

    let response = session
        .handle_request("updateSnapshot", &payload)
        .map_err(|err| ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()))?;
    parse_tsrs_snapshot_id(response)
}

fn discover_updated_references(
    session: &Session,
    cwd: &str,
    protected_snapshot_id: u64,
    tsrs_snapshot_id: &mut u64,
    initial_entries: Vec<String>,
    state: &mut DiscoveryState,
) -> Result<Arc<SnapshotData>, ProjectOpenError> {
    let mut pending_inspection: VecDeque<String> = initial_entries.into_iter().collect();

    loop {
        let snapshot_data = match session.snapshot_data(*tsrs_snapshot_id) {
            Ok(data) => data,
            Err(err) => {
                release_if_intermediate(session, *tsrs_snapshot_id, protected_snapshot_id);
                return Err(ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()));
            }
        };

        let mut unopened = Vec::new();
        while !pending_inspection.is_empty() {
            let newly_discovered =
                match inspect_pending_projects(cwd, &snapshot_data, &mut pending_inspection, state)
                {
                    Ok(next) => next,
                    Err(err) => {
                        drop(snapshot_data);
                        release_if_intermediate(session, *tsrs_snapshot_id, protected_snapshot_id);
                        return Err(err);
                    }
                };
            let programs_by_config = collect_snapshot_programs(&snapshot_data);
            for child_abs in newly_discovered {
                if programs_by_config.contains_key(&child_abs) {
                    pending_inspection.push_back(child_abs);
                } else {
                    unopened.push(child_abs);
                }
            }
        }

        if unopened.is_empty() {
            return Ok(snapshot_data);
        }

        drop(snapshot_data);
        let prev_id = *tsrs_snapshot_id;
        let next_id = match update_snapshot_open_projects(session, prev_id, &unopened) {
            Ok(id) => id,
            Err(err) => {
                release_if_intermediate(session, prev_id, protected_snapshot_id);
                return Err(err);
            }
        };
        release_if_intermediate(session, prev_id, protected_snapshot_id);
        *tsrs_snapshot_id = next_id;
        pending_inspection.extend(unopened);
    }
}

fn close_unreachable_projects(
    session: &Session,
    protected_snapshot_id: u64,
    tsrs_snapshot_id: u64,
    snapshot_data: Arc<SnapshotData>,
    old_opened_configs: &[String],
    visited_configs: &BTreeSet<String>,
) -> Result<(u64, Arc<SnapshotData>), ProjectOpenError> {
    let stale_configs: Vec<&str> = old_opened_configs
        .iter()
        .map(String::as_str)
        .filter(|cfg| Path::new(cfg).is_file() && !visited_configs.contains(*cfg))
        .collect();
    if stale_configs.is_empty() {
        return Ok((tsrs_snapshot_id, snapshot_data));
    }

    drop(snapshot_data);
    let payload = serde_json::to_vec(&serde_json::json!({
        "snapshot": tsrs_snapshot_id,
        "changes": {
            "closeProjects": stale_configs,
            "ensurePrograms": true,
        },
    }))
    .map_err(|err| {
        release_if_intermediate(session, tsrs_snapshot_id, protected_snapshot_id);
        ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string())
    })?;

    let next_id = match session.handle_request("updateSnapshot", &payload) {
        Ok(resp) => match parse_tsrs_snapshot_id(resp) {
            Ok(id) => id,
            Err(err) => {
                release_if_intermediate(session, tsrs_snapshot_id, protected_snapshot_id);
                return Err(err);
            }
        },
        Err(err) => {
            release_if_intermediate(session, tsrs_snapshot_id, protected_snapshot_id);
            return Err(ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()));
        }
    };
    release_if_intermediate(session, tsrs_snapshot_id, protected_snapshot_id);

    match session.snapshot_data(next_id) {
        Ok(data) => Ok((next_id, data)),
        Err(err) => {
            release_if_intermediate(session, next_id, protected_snapshot_id);
            Err(ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()))
        }
    }
}

fn collect_initial_entries(
    cwd: &str,
    entries: &[String],
) -> Result<(Vec<String>, DiscoveryState), ProjectOpenError> {
    if entries.is_empty() {
        return Err(ProjectOpenError::new(
            "PROJECT_LOAD_ERROR",
            "At least one tsconfig or jsconfig entry is required.".to_string(),
        ));
    }

    let mut initial_entries = Vec::new();
    let mut visited_configs = BTreeSet::new();
    let mut discovery_order = Vec::new();
    let mut provenance = BTreeMap::new();

    for entry in entries {
        let resolved = resolve_against_cwd(cwd, entry);
        if !Path::new(&resolved).is_file() {
            return Err(ProjectOpenError::new(
                "PROJECT_LOAD_ERROR",
                format!("Project configuration file not found: {resolved}"),
            ));
        }
        if visited_configs.insert(resolved.clone()) {
            let rel = relative_to_cwd(cwd, &resolved);
            provenance.insert(
                resolved.clone(),
                ProjectProvenance {
                    entry_config: rel.clone(),
                    entry_chain: vec![rel],
                    referenced_by: BTreeSet::new(),
                },
            );
            discovery_order.push(resolved.clone());
            initial_entries.push(resolved);
        }
    }

    Ok((
        initial_entries,
        DiscoveryState {
            visited_configs,
            discovery_order,
            provenance,
            project_refs_map: BTreeMap::new(),
        },
    ))
}

fn inspect_pending_projects(
    cwd: &str,
    snapshot_data: &SnapshotData,
    pending: &mut VecDeque<String>,
    state: &mut DiscoveryState,
) -> Result<Vec<String>, ProjectOpenError> {
    let programs_by_config = collect_snapshot_programs(snapshot_data);
    let mut newly_discovered = Vec::new();

    while let Some(config_path) = pending.pop_front() {
        let Some((_, program)) = programs_by_config.get(&config_path) else {
            return Err(ProjectOpenError::new(
                "PROJECT_LOAD_ERROR",
                format!("Opened project missing program for {config_path}"),
            ));
        };

        if let Some(config_err) = first_config_error(program) {
            return Err(ProjectOpenError::new("PROJECT_LOAD_ERROR", config_err));
        }

        let parent_rel = relative_to_cwd(cwd, &config_path);
        let parent_prov = state
            .provenance
            .get(&config_path)
            .map(|p| (p.entry_config.clone(), p.entry_chain.clone()));

        let mut refs_for_project = Vec::new();
        for reference in program.command_line().project_references() {
            let raw_child = resolve_project_reference_path(reference);
            let child_abs = resolve_against_cwd(cwd, &raw_child);
            let child_rel = relative_to_cwd(cwd, &child_abs);
            if !refs_for_project.contains(&child_rel) {
                refs_for_project.push(child_rel.clone());
            }

            if state.visited_configs.insert(child_abs.clone()) {
                if !Path::new(&child_abs).is_file() {
                    return Err(ProjectOpenError::new(
                        "PROJECT_LOAD_ERROR",
                        format!(
                            "Referenced project configuration file not found: {child_abs} (referenced from {parent_rel})"
                        ),
                    ));
                }

                let (entry_config, mut entry_chain) = parent_prov
                    .clone()
                    .unwrap_or_else(|| (parent_rel.clone(), vec![parent_rel.clone()]));
                entry_chain.push(child_rel);

                let mut referenced_by = BTreeSet::new();
                referenced_by.insert(parent_rel.clone());

                state.provenance.insert(
                    child_abs.clone(),
                    ProjectProvenance {
                        entry_config,
                        entry_chain,
                        referenced_by,
                    },
                );
                state.discovery_order.push(child_abs.clone());
                newly_discovered.push(child_abs);
            } else if let Some(existing_prov) = state.provenance.get_mut(&child_abs) {
                existing_prov.referenced_by.insert(parent_rel.clone());
            }
        }
        state.project_refs_map.insert(config_path, refs_for_project);
    }

    Ok(newly_discovered)
}

fn create_initial_snapshot(
    session: &Session,
    open_projects: &[String],
) -> Result<u64, ProjectOpenError> {
    let payload = serde_json::to_vec(&serde_json::json!({
        "openProjects": open_projects,
    }))
    .map_err(|err| ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()))?;

    let response = session
        .handle_request("createSnapshot", &payload)
        .map_err(|err| ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()))?;
    parse_tsrs_snapshot_id(response)
}

fn update_snapshot_open_projects(
    session: &Session,
    snapshot_id: u64,
    open_projects: &[String],
) -> Result<u64, ProjectOpenError> {
    let payload = serde_json::to_vec(&serde_json::json!({
        "snapshot": snapshot_id,
        "changes": {
            "openProjects": open_projects,
            "ensurePrograms": true,
        },
    }))
    .map_err(|err| ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()))?;

    let response = session
        .handle_request("updateSnapshot", &payload)
        .map_err(|err| ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()))?;
    parse_tsrs_snapshot_id(response)
}

fn parse_tsrs_snapshot_id(response: Response) -> Result<u64, ProjectOpenError> {
    let Response::Json(json_text) = response else {
        return Err(ProjectOpenError::new(
            "PROJECT_LOAD_ERROR",
            "Unexpected binary response from tsrs session.".to_string(),
        ));
    };
    let parsed: TsrsSnapshotResponse = serde_json::from_str(&json_text)
        .map_err(|err| ProjectOpenError::new("PROJECT_LOAD_ERROR", err.to_string()))?;
    Ok(parsed.snapshot)
}

fn collect_snapshot_programs(
    snapshot_data: &SnapshotData,
) -> BTreeMap<String, (ProjectId, &'static Program)> {
    let mut map = BTreeMap::new();
    for proj in snapshot_data.snapshot.project_collection.projects() {
        let Some(program) = proj.get_program() else {
            continue;
        };
        let config_norm = normalize_path(proj.config_file_name());
        map.insert(config_norm, (proj.id(), program));
    }
    map
}

fn build_opened_snapshot(
    tsrs_snapshot_id: u64,
    cwd: &str,
    entry_specs: Vec<String>,
    snapshot_data: Arc<SnapshotData>,
    state: &DiscoveryState,
) -> Result<OpenedProjectsSnapshot, ProjectOpenError> {
    let programs_by_config = collect_snapshot_programs(&snapshot_data);
    let mut project_ids = BTreeMap::new();
    let mut summaries = Vec::with_capacity(state.discovery_order.len());
    let mut digest_entries = Vec::with_capacity(state.discovery_order.len());

    for config_abs in &state.discovery_order {
        let Some(&(ref tsrs_pid, program)) = programs_by_config.get(config_abs) else {
            return Err(ProjectOpenError::new(
                "PROJECT_LOAD_ERROR",
                format!("Opened project missing program for {config_abs}"),
            ));
        };

        let config_rel = relative_to_cwd(cwd, config_abs);
        let project_id = config_rel.clone();
        let mut files_with_text = collect_project_files_with_text(cwd, program);
        files_with_text.sort_by(|a, b| a.0.cmp(&b.0));
        files_with_text.dedup_by(|a, b| a.0 == b.0);
        let mut workspace_files_with_text = collect_workspace_files_with_text(cwd, program);
        workspace_files_with_text.sort_by(|a, b| a.0.cmp(&b.0));
        workspace_files_with_text.dedup_by(|a, b| a.0 == b.0);

        let prov = state.provenance.get(config_abs);
        let entry_config = prov.map_or_else(|| config_rel.clone(), |p| p.entry_config.clone());
        let entry_chain = prov.map_or_else(|| vec![config_rel.clone()], |p| p.entry_chain.clone());
        let referenced_by =
            prov.map_or_else(Vec::new, |p| p.referenced_by.iter().cloned().collect());
        let project_references = state
            .project_refs_map
            .get(config_abs)
            .cloned()
            .unwrap_or_default();
        let compiler_options = summarize_compiler_options(&program.options());
        let source_files: Vec<String> =
            files_with_text.iter().map(|(rel, _)| rel.clone()).collect();

        let summary = ProjectSummary {
            project_id: project_id.clone(),
            config_file: config_rel,
            entry_config,
            entry_chain,
            referenced_by,
            project_references,
            compiler_options,
            source_files,
        };
        digest_entries.push(ProjectDigestEntry {
            config_abs: config_abs.clone(),
            summary: summary.clone(),
            files_with_text,
            workspace_files_with_text,
        });
        project_ids.insert(project_id, tsrs_pid.clone());
        summaries.push(summary);
    }

    if summaries.is_empty() {
        return Err(ProjectOpenError::new(
            "PROJECT_LOAD_ERROR",
            "No compiler projects were opened.".to_string(),
        ));
    }

    let workspace_source_files = digest_entries
        .iter()
        .flat_map(|entry| {
            entry
                .workspace_files_with_text
                .iter()
                .map(|(path, _)| path.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let snapshot_digest = compute_snapshot_digest(&mut digest_entries);

    Ok(OpenedProjectsSnapshot {
        tsrs_snapshot_id,
        snapshot_data,
        entry_specs,
        opened_configs: state.discovery_order.clone(),
        project_ids,
        summaries,
        workspace_source_files,
        snapshot_digest,
    })
}

fn collect_workspace_files_with_text(
    cwd: &str,
    program: &'static Program,
) -> Vec<(String, &'static str)> {
    let workspace_root = Path::new(cwd)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(cwd).to_path_buf());
    let workspace_root = workspace_root.to_string_lossy();

    // These files affect cross-package references and snapshot identity but stay outside public source coverage.
    let mut files = Vec::new();
    for &source_file in program.get_source_files() {
        if program.is_source_file_default_library(source_file.path())
            || !program.is_source_file_from_external_library(source_file)
        {
            continue;
        }
        let Ok(real_path) = Path::new(source_file.file_name()).canonicalize() else {
            continue;
        };
        if !real_path.starts_with(Path::new(workspace_root.as_ref()))
            || real_path
                .components()
                .any(|component| component.as_os_str() == "node_modules")
        {
            continue;
        }
        let Some(real_path) = real_path.to_str() else {
            continue;
        };
        files.push((
            relative_to_cwd(workspace_root.as_ref(), real_path),
            source_file.text(),
        ));
    }

    files
}

struct ProjectDigestEntry {
    config_abs: String,
    summary: ProjectSummary,
    files_with_text: Vec<(String, &'static str)>,
    workspace_files_with_text: Vec<(String, &'static str)>,
}

fn collect_project_files_with_text(
    cwd: &str,
    program: &'static Program,
) -> Vec<(String, &'static str)> {
    program
        .get_source_files()
        .iter()
        .copied()
        .filter(|&sf| is_project_source_file(program, sf))
        .map(|sf| (relative_to_cwd(cwd, sf.file_name()), sf.text()))
        .collect()
}

fn compute_snapshot_digest(entries: &mut [ProjectDigestEntry]) -> String {
    entries.sort_by(|a, b| a.summary.project_id.cmp(&b.summary.project_id));
    let mut hasher = Sha256::new();
    for entry in entries {
        let config_bytes = std::fs::read(&entry.config_abs).unwrap_or_default();
        let summary_bytes = serde_json::to_vec(&entry.summary).unwrap_or_default();
        hasher.update_len_prefixed(entry.summary.project_id.as_bytes());
        hasher.update_len_prefixed(&config_bytes);
        hasher.update_len_prefixed(&summary_bytes);
        for (rel_file, text) in &entry.files_with_text {
            hasher.update_len_prefixed(rel_file.as_bytes());
            hasher.update_len_prefixed(text.as_bytes());
        }
        for (rel_file, text) in &entry.workspace_files_with_text {
            hasher.update_len_prefixed(rel_file.as_bytes());
            hasher.update_len_prefixed(text.as_bytes());
        }
    }
    hasher.finish_hex()
}

struct RecomputationDiff {
    recomputed_projects: Vec<String>,
    reused_projects: Vec<String>,
    recomputed_files: Vec<String>,
    reused_files: Vec<String>,
    removed_files: Vec<String>,
}

fn compute_recomputation_scope(
    cwd: &str,
    old_snapshot: &SnapshotData,
    old_projects: &BTreeMap<String, ProjectId>,
    new_snapshot: &SnapshotData,
    new_projects: &BTreeMap<String, ProjectId>,
) -> RecomputationDiff {
    let mut old_all_files = BTreeSet::new();
    for pid in old_projects.values() {
        if let Ok(prog) = old_snapshot.get_program(pid) {
            for &sf in prog.get_source_files() {
                if is_project_source_file(prog, sf) {
                    old_all_files.insert(relative_to_cwd(cwd, sf.file_name()));
                }
            }
        }
    }

    let mut recomputed_projects = Vec::new();
    let mut reused_projects = Vec::new();
    let mut recomputed_files_set = BTreeSet::new();
    let mut reused_files_set = BTreeSet::new();
    let mut new_all_files = BTreeSet::new();

    for (id_str, pid) in new_projects {
        let Ok(new_prog) = new_snapshot.get_program(pid) else {
            continue;
        };
        let old_prog = old_projects
            .get(id_str)
            .and_then(|old_pid| old_snapshot.get_program(old_pid).ok());

        let project_reused = old_prog.is_some_and(|old| std::ptr::eq(old, new_prog));
        if project_reused {
            reused_projects.push(id_str.clone());
        } else {
            recomputed_projects.push(id_str.clone());
        }

        for &sf in new_prog.get_source_files() {
            if !is_project_source_file(new_prog, sf) {
                continue;
            }
            let rel = relative_to_cwd(cwd, sf.file_name());
            new_all_files.insert(rel.clone());

            let file_reused = old_prog
                .and_then(|old| old.get_source_file(sf.file_name()))
                .is_some_and(|old_sf| std::ptr::eq(&raw const *old_sf, &raw const *sf));

            if file_reused {
                reused_files_set.insert(rel);
            } else {
                recomputed_files_set.insert(rel);
            }
        }
    }

    let removed_files: Vec<String> = old_all_files.difference(&new_all_files).cloned().collect();
    let reused_files: Vec<String> = reused_files_set
        .difference(&recomputed_files_set)
        .cloned()
        .collect();
    let recomputed_files: Vec<String> = recomputed_files_set.into_iter().collect();

    RecomputationDiff {
        recomputed_projects,
        reused_projects,
        recomputed_files,
        reused_files,
        removed_files,
    }
}

fn summarize_compiler_options(opts: &CompilerOptions) -> CompilerOptionsSummary {
    let target = (opts.target != ScriptTarget::NONE).then(|| format!("{:?}", opts.target));
    let module = (opts.module != ModuleKind::NONE).then(|| format!("{:?}", opts.module));
    let module_resolution = (opts.module_resolution != ModuleResolutionKind::UNKNOWN)
        .then(|| format!("{:?}", opts.module_resolution));
    let jsx = (opts.jsx != JsxEmit::NONE).then(|| format!("{:?}", opts.jsx));

    let mut paths = BTreeMap::new();
    if let Some(raw_paths) = &opts.paths {
        for (pattern, targets) in raw_paths.iter() {
            paths.insert(pattern.clone(), targets.clone().unwrap_or_default());
        }
    }

    CompilerOptionsSummary {
        strict: opts.strict.is_true(),
        allow_js: opts.allow_js.is_true(),
        check_js: opts.check_js.is_true(),
        composite: opts.composite.is_true(),
        target,
        module,
        module_resolution,
        jsx,
        paths,
        custom_conditions: opts.custom_conditions.clone().unwrap_or_default(),
    }
}

fn first_config_error(program: &Program) -> Option<String> {
    for diag in program.get_config_file_parsing_diagnostics() {
        if diag.category() == Category::Error {
            return Some(format_diagnostic(&diag));
        }
    }
    None
}

fn format_diagnostic(diag: &TsrsDiagnostic) -> String {
    let text = diag.message_text();
    if !text.is_empty() {
        return text.to_string();
    }
    if let Some(msg) = diag.message() {
        let args: Vec<&dyn std::fmt::Display> = diag
            .message_args()
            .iter()
            .map(|s| s as &dyn std::fmt::Display)
            .collect();
        return msg.format(&args);
    }
    format!("TS{}", diag.code())
}

const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

struct Sha256 {
    state: [u32; 8],
    bit_len: u64,
    buffer: [u8; 64],
    buf_len: usize,
}

impl Sha256 {
    const fn new() -> Self {
        Self {
            state: [
                0x6a09_e667,
                0xbb67_ae85,
                0x3c6e_f372,
                0xa54f_f53a,
                0x510e_527f,
                0x9b05_688c,
                0x1f83_d9ab,
                0x5be0_cd19,
            ],
            bit_len: 0,
            buffer: [0; 64],
            buf_len: 0,
        }
    }

    fn update_len_prefixed(&mut self, data: &[u8]) {
        let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
        self.update(&len.to_be_bytes());
        self.update(data);
    }

    fn update(&mut self, data: &[u8]) {
        let mut offset = 0;
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buffer[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            offset += take;
            if self.buf_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        while offset + 64 <= data.len() {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[offset..offset + 64]);
            self.compress(&block);
            offset += 64;
        }
        if offset < data.len() {
            let rem = data.len() - offset;
            self.buffer[..rem].copy_from_slice(&data[offset..]);
            self.buf_len = rem;
        }
        let bits = u64::try_from(data.len()).unwrap_or(0).saturating_mul(8);
        self.bit_len = self.bit_len.wrapping_add(bits);
    }

    fn finish_hex(mut self) -> String {
        let bit_len = self.bit_len;
        self.update(&[0x80]);
        if self.buf_len > 56 {
            while self.buf_len < 64 {
                self.buffer[self.buf_len] = 0;
                self.buf_len += 1;
            }
            let block = self.buffer;
            self.compress(&block);
            self.buf_len = 0;
        }
        while self.buf_len < 56 {
            self.buffer[self.buf_len] = 0;
            self.buf_len += 1;
        }
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut out = String::with_capacity(64);
        for word in self.state {
            let _ = write!(out, "{word:08x}");
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut schedule = [0u32; 64];
        for (idx, word) in schedule.iter_mut().enumerate().take(16) {
            let off = idx * 4;
            *word =
                u32::from_be_bytes([block[off], block[off + 1], block[off + 2], block[off + 3]]);
        }
        for idx in 16..64 {
            let s0 = schedule[idx - 15].rotate_right(7)
                ^ schedule[idx - 15].rotate_right(18)
                ^ (schedule[idx - 15] >> 3);
            let s1 = schedule[idx - 2].rotate_right(17)
                ^ schedule[idx - 2].rotate_right(19)
                ^ (schedule[idx - 2] >> 10);
            schedule[idx] = schedule[idx - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[idx - 7])
                .wrapping_add(s1);
        }

        let [
            mut s0,
            mut s1,
            mut s2,
            mut s3,
            mut s4,
            mut s5,
            mut s6,
            mut s7,
        ] = self.state;
        for (idx, &k_val) in SHA256_K.iter().enumerate() {
            let sigma1 = s4.rotate_right(6) ^ s4.rotate_right(11) ^ s4.rotate_right(25);
            let choice = (s4 & s5) ^ ((!s4) & s6);
            let temp1 = s7
                .wrapping_add(sigma1)
                .wrapping_add(choice)
                .wrapping_add(k_val)
                .wrapping_add(schedule[idx]);
            let sigma0 = s0.rotate_right(2) ^ s0.rotate_right(13) ^ s0.rotate_right(22);
            let majority = (s0 & s1) ^ (s0 & s2) ^ (s1 & s2);
            let temp2 = sigma0.wrapping_add(majority);

            s7 = s6;
            s6 = s5;
            s5 = s4;
            s4 = s3.wrapping_add(temp1);
            s3 = s2;
            s2 = s1;
            s1 = s0;
            s0 = temp1.wrapping_add(temp2);
        }

        for (slot, delta) in self.state.iter_mut().zip([s0, s1, s2, s3, s4, s5, s6, s7]) {
            *slot = slot.wrapping_add(delta);
        }
    }
}
