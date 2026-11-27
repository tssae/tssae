use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    sync::{Arc, RwLock},
    time::Instant,
};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tssae_analysis::{
    branch_coverage::{
        BranchBypassPath, BranchCoverageOutcome, ImplicitExceptionCoverageOutcome,
        evaluate_branch_coverage,
    },
    ir::{FileIrEntry, FunctionIr, SnapshotIrCache},
    summaries::BehaviorSummaryIndex,
};
use tssae_common::{BranchId, EvidenceItem, FileId, GapItem, SummaryConfigPayload, UnknownReason};
use tssae_tsrs_boundary::{
    BoundaryChecker, BoundaryNode, BoundaryProgram, BoundaryProjectFileCatalog, BoundaryProjectId,
    BoundarySession, BoundarySignature, BoundarySnapshotData, BoundarySymbol, BoundaryType,
    FileIdRegistry, FileSyntaxIndex, ProgramHierarchyIndex, ProjectSummary, ResolvedPackageInfo,
    SourceMapCache, SymbolRegistry, UpdateSnapshotInput, lower_scope_to_ir_boundary,
    normalize_path, open_projects_snapshot, release_compiler_snapshot, update_projects_snapshot,
};

use crate::comments::{ScannedComment, scan_snapshot_comments};
use crate::ir_instances::InstanceHandleData;
use crate::observe::{EngineRpcMetrics, ObserveCollector, ObserveRequestConfig, elapsed_ms};
pub use crate::projection::SourceLocation;
use crate::projection::{QueryStep, execute_query_batch};

#[derive(Deserialize)]
struct RpcRequest {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    observe: Option<ObserveRequestConfig>,
}

#[derive(Serialize)]
struct RpcSuccessResponse<'a, T> {
    id: &'a Value,
    ok: bool,
    result: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<EngineRpcMetrics>,
}

#[derive(Serialize)]
struct RpcErrorPayload<'a> {
    code: &'a str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<&'a Value>,
}

#[derive(Serialize)]
struct RpcErrorResponse<'a> {
    id: &'a Value,
    ok: bool,
    error: RpcErrorPayload<'a>,
}

pub struct RpcErr {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl RpcErr {
    pub const fn new(code: &'static str, message: String) -> Self {
        Self {
            code,
            message,
            details: None,
        }
    }

    pub const fn with_details(code: &'static str, message: String, details: Value) -> Self {
        Self {
            code,
            message,
            details: Some(details),
        }
    }
}

#[derive(Deserialize)]
struct SnapshotOpenParams {
    cwd: String,
    entries: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotUpdateParams {
    snapshot_id: u64,
    #[serde(default)]
    entries: Option<Vec<String>>,
    #[serde(default)]
    changed: Vec<String>,
    #[serde(default)]
    created: Vec<String>,
    #[serde(default)]
    deleted: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotReleaseParams {
    snapshot_id: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotSetSummariesParams {
    snapshot_id: u64,
    summaries: SummaryConfigPayload,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotSetSummariesResult {
    updated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPackagePayload {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_root: Option<String>,
    pub project_dirs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryBatchParams {
    snapshot_id: u64,
    project_id: String,
    steps: Vec<QueryStep>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentsScanParams {
    snapshot_id: u64,
    #[serde(default)]
    files: Option<Vec<String>>,
}

#[derive(Serialize)]
struct CommentsScanResult {
    comments: Vec<ScannedComment>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotOpenResult {
    snapshot_id: u64,
    snapshot_digest: String,
    projects: Vec<ProjectSummary>,
    workspace_source_files: Vec<String>,
    resolved_packages: Vec<ResolvedPackagePayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_rss_bytes: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotUpdateResult {
    snapshot_id: u64,
    snapshot_digest: String,
    projects: Vec<ProjectSummary>,
    workspace_source_files: Vec<String>,
    resolved_packages: Vec<ResolvedPackagePayload>,
    recomputed_projects: Vec<String>,
    reused_projects: Vec<String>,
    recomputed_files: Vec<String>,
    reused_files: Vec<String>,
    removed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_rss_bytes: Option<u64>,
}

#[derive(Serialize)]
struct SnapshotReleaseResult {
    released: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchEdgeItem {
    #[serde(rename = "ref")]
    branch_ref: String,
    kind: &'static str,
    direction: &'static str,
    condition: String,
    condition_text: String,
    location: SourceLocation,
    statement_location: SourceLocation,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionBranchesItem {
    pub target: String,
    pub branches: Vec<BranchEdgeItem>,
    pub gaps: Vec<GapItem>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum BranchCoverageItem {
    #[serde(rename_all = "camelCase")]
    Proved {
        target: String,
        target_reachable: bool,
        implicit_exceptions: ImplicitExceptionCoverageOutcome,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename_all = "camelCase")]
    Disproved {
        target: String,
        target_reachable: bool,
        implicit_exceptions: ImplicitExceptionCoverageOutcome,
        counterexample_path: BranchBypassPath,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename_all = "camelCase")]
    Inconclusive {
        target: String,
        target_reachable: bool,
        implicit_exceptions: ImplicitExceptionCoverageOutcome,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

impl BranchCoverageItem {
    #[must_use]
    pub fn target(&self) -> &str {
        match self {
            Self::Proved { target, .. }
            | Self::Disproved { target, .. }
            | Self::Inconclusive { target, .. } => target,
        }
    }

    #[must_use]
    pub const fn status_str(&self) -> &'static str {
        match self {
            Self::Proved { .. } => "proved",
            Self::Disproved { .. } => "disproved",
            Self::Inconclusive { .. } => "inconclusive",
        }
    }
}

#[derive(Clone, Copy)]
pub struct BranchHandle {
    pub function_node: BoundaryNode,
    pub edge_index: usize,
}

#[derive(Clone, Copy)]
pub enum HandlePayload {
    Syntax(BoundaryNode),
    Declaration {
        node: BoundaryNode,
        symbol: Option<BoundarySymbol>,
    },
    Call(BoundaryNode),
    Signature {
        sig: BoundarySignature,
        call_node: Option<BoundaryNode>,
    },
    Type {
        ty: BoundaryType,
        enclosing: Option<BoundaryNode>,
    },
    Branch(BranchHandle),
    Instance(usize),
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum HandleInternKey {
    Syntax(String, BoundaryNode),
    Declaration {
        project_id: String,
        node: BoundaryNode,
    },
    Call(String, BoundaryNode),
    Signature {
        project_id: String,
        sig: BoundarySignature,
        call_node: Option<BoundaryNode>,
    },
    Type {
        project_id: String,
        ty: BoundaryType,
        enclosing: Option<BoundaryNode>,
    },
    Branch {
        project_id: String,
        function_node: BoundaryNode,
        edge_index: usize,
    },
}

pub struct HandleEntry {
    pub project_id: String,
    pub payload: HandlePayload,
}

pub struct SnapshotState {
    pub snapshot_id: u64,
    pub compiler_snapshot_id: u64,
    pub cwd: String,
    pub entry_specs: Vec<String>,
    pub opened_configs: Vec<String>,
    pub snapshot_data: BoundarySnapshotData,
    pub project_ids: BTreeMap<String, BoundaryProjectId>,
    pub handles: FxHashMap<String, HandleEntry>,
    pub handle_intern: FxHashMap<HandleInternKey, String>,
    pub instances: Vec<InstanceHandleData>,
    pub instance_handles: FxHashMap<(String, String), String>,
    pub file_ids: Arc<RwLock<FileIdRegistry>>,
    pub ir_cache: SnapshotIrCache,
    pub file_syntax_indices: FxHashMap<FileId, Arc<FileSyntaxIndex>>,
    pub symbol_registry: SymbolRegistry,
    pub ir_call_graph_cache: FxHashMap<String, Arc<tssae_analysis::call_resolution::IrCallGraph>>,
    pub ir_call_graph_shards:
        FxHashMap<(String, FileId), Arc<tssae_analysis::call_resolution::IrCallGraph>>,
    pub ir_field_write_cache:
        FxHashMap<String, Arc<tssae_analysis::instance_identity::IrFieldWriteIndex>>,
    pub ir_field_write_shards:
        FxHashMap<(String, FileId), Arc<tssae_analysis::instance_identity::IrFieldWriteIndex>>,
    pub hierarchy_cache: FxHashMap<String, Arc<ProgramHierarchyIndex>>,
    pub project_file_catalog_cache: FxHashMap<String, Arc<BoundaryProjectFileCatalog>>,
    pub sourcemap_cache: SourceMapCache,
    pub resolved_package_versions: FxHashMap<String, Option<String>>,
    pub summary_config: SummaryConfigPayload,
    pub summary_index: Arc<BehaviorSummaryIndex>,
    pub next_handle_id: u64,
}

impl SnapshotState {
    #[must_use]
    pub fn workspace_programs(&self) -> Vec<BoundaryProgram> {
        self.snapshot_data.workspace_programs()
    }

    fn alloc_handle(&mut self, prefix: &str, project_id: &str, payload: HandlePayload) -> String {
        self.next_handle_id += 1;
        let id = format!("{prefix}:{}:{}", self.snapshot_id, self.next_handle_id);
        self.handles.insert(
            id.clone(),
            HandleEntry {
                project_id: project_id.to_string(),
                payload,
            },
        );
        id
    }

    fn alloc_interned_handle(
        &mut self,
        prefix: &str,
        project_id: &str,
        key: HandleInternKey,
        payload: HandlePayload,
    ) -> String {
        if let Some(existing) = self.handle_intern.get(&key) {
            return existing.clone();
        }
        let id = self.alloc_handle(prefix, project_id, payload);
        self.handle_intern.insert(key, id.clone());
        id
    }

    pub fn alloc_syntax(&mut self, project_id: &str, node: BoundaryNode) -> String {
        self.alloc_interned_handle(
            "syn",
            project_id,
            HandleInternKey::Syntax(project_id.to_string(), node),
            HandlePayload::Syntax(node),
        )
    }

    pub fn alloc_declaration(
        &mut self,
        project_id: &str,
        node: BoundaryNode,
        symbol: Option<BoundarySymbol>,
    ) -> String {
        let key = HandleInternKey::Declaration {
            project_id: project_id.to_string(),
            node,
        };
        if let Some(existing_id) = self.handle_intern.get(&key).cloned() {
            if let Some(sym) = symbol
                && let Some(entry) = self.handles.get_mut(&existing_id)
                && matches!(
                    entry.payload,
                    HandlePayload::Declaration { symbol: None, .. }
                )
            {
                entry.payload = HandlePayload::Declaration {
                    node,
                    symbol: Some(sym),
                };
            }
            return existing_id;
        }
        let id = self.alloc_handle(
            "dec",
            project_id,
            HandlePayload::Declaration { node, symbol },
        );
        self.handle_intern.insert(key, id.clone());
        id
    }

    pub fn alloc_call(&mut self, project_id: &str, node: BoundaryNode) -> String {
        self.alloc_interned_handle(
            "cal",
            project_id,
            HandleInternKey::Call(project_id.to_string(), node),
            HandlePayload::Call(node),
        )
    }

    pub fn alloc_signature(
        &mut self,
        project_id: &str,
        sig: BoundarySignature,
        call_node: Option<BoundaryNode>,
    ) -> String {
        self.alloc_interned_handle(
            "sig",
            project_id,
            HandleInternKey::Signature {
                project_id: project_id.to_string(),
                sig,
                call_node,
            },
            HandlePayload::Signature { sig, call_node },
        )
    }

    pub fn alloc_type(
        &mut self,
        project_id: &str,
        ty: BoundaryType,
        enclosing: Option<BoundaryNode>,
    ) -> String {
        self.alloc_interned_handle(
            "typ",
            project_id,
            HandleInternKey::Type {
                project_id: project_id.to_string(),
                ty,
                enclosing,
            },
            HandlePayload::Type { ty, enclosing },
        )
    }

    pub fn alloc_branch(&mut self, project_id: &str, branch: BranchHandle) -> String {
        self.alloc_interned_handle(
            "brn",
            project_id,
            HandleInternKey::Branch {
                project_id: project_id.to_string(),
                function_node: branch.function_node,
                edge_index: branch.edge_index,
            },
            HandlePayload::Branch(branch),
        )
    }

    pub fn alloc_instance(&mut self, project_id: &str, data: InstanceHandleData) -> String {
        let key = (project_id.to_string(), data.instance_key.clone());
        if let Some(existing) = self.instance_handles.get(&key) {
            return existing.clone();
        }
        let idx = self.instances.len();
        self.instances.push(data);
        let handle_id = self.alloc_handle("ins", project_id, HandlePayload::Instance(idx));
        self.instance_handles.insert(key, handle_id.clone());
        handle_id
    }

    pub fn get_instance(
        &self,
        project_id: &str,
        handle_id: &str,
    ) -> Result<&InstanceHandleData, RpcErr> {
        let entry = self.get_entry(project_id, handle_id)?;
        let HandlePayload::Instance(idx) = entry.payload else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {handle_id} is not an instance reference."),
            ));
        };
        self.instances.get(idx).ok_or_else(|| {
            RpcErr::new(
                "STALE_REFERENCE",
                format!("Instance payload for {handle_id} is missing."),
            )
        })
    }

    pub fn get_entry(&self, project_id: &str, handle_id: &str) -> Result<&HandleEntry, RpcErr> {
        let Some(entry) = self.handles.get(handle_id) else {
            return Err(RpcErr::new(
                "STALE_REFERENCE",
                format!("Program reference {handle_id} is expired or unknown."),
            ));
        };
        if entry.project_id != project_id {
            return Err(RpcErr::new(
                "CROSS_PROJECT_REFERENCE",
                format!(
                    "Program reference {handle_id} belongs to project {}, not {project_id}.",
                    entry.project_id
                ),
            ));
        }
        Ok(entry)
    }

    pub fn get_syntax_node(
        &self,
        project_id: &str,
        handle_id: &str,
    ) -> Result<BoundaryNode, RpcErr> {
        let entry = self.get_entry(project_id, handle_id)?;
        let HandlePayload::Syntax(node) = entry.payload else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {handle_id} is not a syntax reference."),
            ));
        };
        Ok(node)
    }

    pub fn get_call_node(&self, project_id: &str, handle_id: &str) -> Result<BoundaryNode, RpcErr> {
        let entry = self.get_entry(project_id, handle_id)?;
        let HandlePayload::Call(node) = entry.payload else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {handle_id} is not a call reference."),
            ));
        };
        Ok(node)
    }

    pub fn get_signature(
        &self,
        project_id: &str,
        handle_id: &str,
    ) -> Result<(BoundarySignature, Option<BoundaryNode>), RpcErr> {
        let entry = self.get_entry(project_id, handle_id)?;
        let HandlePayload::Signature { sig, call_node } = entry.payload else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {handle_id} is not a signature reference."),
            ));
        };
        Ok((sig, call_node))
    }

    pub fn get_type_entry(
        &self,
        project_id: &str,
        handle_id: &str,
    ) -> Result<(BoundaryType, Option<BoundaryNode>), RpcErr> {
        let entry = self.get_entry(project_id, handle_id)?;
        let HandlePayload::Type { ty, enclosing } = entry.payload else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {handle_id} is not a type reference."),
            ));
        };
        Ok((ty, enclosing))
    }

    pub fn get_branch(&self, project_id: &str, handle_id: &str) -> Result<BranchHandle, RpcErr> {
        let entry = self.get_entry(project_id, handle_id)?;
        let HandlePayload::Branch(branch) = entry.payload else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {handle_id} is not a branch reference."),
            ));
        };
        Ok(branch)
    }

    pub fn get_or_build_ir(
        &mut self,
        program: BoundaryProgram,
        checker: &mut BoundaryChecker<'_>,
        scope_node: BoundaryNode,
        observer: &mut ObserveCollector,
    ) -> Result<(Arc<FunctionIr>, Arc<FileSyntaxIndex>), GapItem> {
        let Some(sf) = scope_node.source_file() else {
            return Err(GapItem::from_reason(
                UnknownReason::MissingSourceFile,
                "Function node has no associated source file.".to_string(),
                None,
            ));
        };
        let rel_file = sf.rel_path(&self.cwd);
        let file_id = self
            .file_ids
            .write()
            .map_or(FileId(0), |mut reg| reg.intern(&rel_file));
        let source_file_addr = sf.addr();
        let file_index = self
            .file_syntax_indices
            .entry(file_id)
            .or_insert_with(|| Arc::new(sf.build_syntax_index(file_id, rel_file.clone())))
            .clone();
        let Some(local_syntax) = file_index.boundary_local_id_for_node(scope_node) else {
            return Err(GapItem::from_reason(
                UnknownReason::InvalidCfgScope,
                "Scope node was not indexed in its source file.".to_string(),
                Some(scope_node.location(sf, &rel_file)),
            ));
        };

        let file_entry = self.ir_cache.get_or_insert_file(file_id, || {
            FileIrEntry::new(
                file_id,
                rel_file.clone(),
                source_file_addr,
                file_index.scope_catalog.clone(),
            )
        });

        let mut lowering_duration_ms = 0.0;
        let cwd = &self.cwd;
        let (result, is_hit) = file_entry.get_or_init_scope(local_syntax, |local_scope| {
            let started = Instant::now();
            let lowered = lower_scope_to_ir_boundary(
                cwd,
                program,
                checker,
                scope_node,
                local_scope,
                Some(&file_index),
            );
            lowering_duration_ms = elapsed_ms(started);
            lowered
        });

        if is_hit {
            observer.record_ir_cache_hit();
        } else {
            let vertices_created = result.as_ref().map_or(0, |ir| ir.vertices.len());
            observer.record_ir_lowering(vertices_created, lowering_duration_ms, result.is_ok());
        }

        result.map(|ir| (ir, file_index))
    }
}

struct EngineServer {
    session: Option<(String, BoundarySession)>,
    snapshots: BTreeMap<u64, SnapshotState>,
    file_ids: Arc<RwLock<FileIdRegistry>>,
    next_snapshot_id: u64,
}

impl EngineServer {
    fn new() -> Self {
        Self {
            session: None,
            snapshots: BTreeMap::new(),
            file_ids: Arc::new(RwLock::new(FileIdRegistry::new())),
            next_snapshot_id: 0,
        }
    }

    fn ensure_session(&mut self, cwd: &str) -> BoundarySession {
        if let Some((existing_cwd, existing_session)) = &self.session
            && existing_cwd == cwd
        {
            return existing_session.clone();
        }
        if let Some((_, old_session)) = self.session.take() {
            self.snapshots.clear();
            old_session.close();
        }
        self.file_ids = Arc::new(RwLock::new(FileIdRegistry::new()));
        let session = BoundarySession::new(cwd);
        self.session = Some((cwd.to_string(), session.clone()));
        session
    }

    fn handle_line(&mut self, line: &str) -> String {
        let request: RpcRequest = match serde_json::from_str(line) {
            Ok(req) => req,
            Err(err) => {
                return serialize_error(
                    &Value::Null,
                    "INVALID_REQUEST",
                    format!("Invalid JSON request: {err}"),
                    None,
                );
            }
        };

        let mut observer = ObserveCollector::from_request(request.observe);
        match self.dispatch(&request.method, request.params, &mut observer) {
            Ok(result) => {
                if observer.is_active() {
                    let encode_start = Instant::now();
                    let encoded = serde_json::to_string(&result);
                    observer.add_encode_ms(elapsed_ms(encode_start));
                    if let Err(err) = encoded {
                        return serialize_error(
                            &request.id,
                            "SERIALIZATION_ERROR",
                            err.to_string(),
                            None,
                        );
                    }
                }
                let metrics = observer.finish();
                serde_json::to_string(&RpcSuccessResponse {
                    id: &request.id,
                    ok: true,
                    result,
                    metrics,
                })
                .unwrap_or_else(|err| {
                    serialize_error(&request.id, "SERIALIZATION_ERROR", err.to_string(), None)
                })
            }
            Err(err) => serialize_error(&request.id, err.code, err.message, err.details.as_ref()),
        }
    }

    fn dispatch(
        &mut self,
        method: &str,
        params: Value,
        observer: &mut ObserveCollector,
    ) -> Result<Value, RpcErr> {
        match method {
            "snapshot.open" => {
                let parsed: SnapshotOpenParams = serde_json::from_value(params)
                    .map_err(|err| RpcErr::new("INVALID_PARAMS", err.to_string()))?;
                let result = self.open_snapshot(&parsed)?;
                serde_json::to_value(result)
                    .map_err(|err| RpcErr::new("SERIALIZATION_ERROR", err.to_string()))
            }
            "snapshot.update" => {
                let parsed: SnapshotUpdateParams = serde_json::from_value(params)
                    .map_err(|err| RpcErr::new("INVALID_PARAMS", err.to_string()))?;
                let result = self.update_snapshot(&parsed, observer)?;
                serde_json::to_value(result)
                    .map_err(|err| RpcErr::new("SERIALIZATION_ERROR", err.to_string()))
            }
            "snapshot.setSummaries" => {
                let parsed: SnapshotSetSummariesParams = serde_json::from_value(params)
                    .map_err(|err| RpcErr::new("INVALID_PARAMS", err.to_string()))?;
                let result = self.set_snapshot_summaries(parsed)?;
                serde_json::to_value(result)
                    .map_err(|err| RpcErr::new("SERIALIZATION_ERROR", err.to_string()))
            }
            "snapshot.release" => {
                let parsed: SnapshotReleaseParams = serde_json::from_value(params)
                    .map_err(|err| RpcErr::new("INVALID_PARAMS", err.to_string()))?;
                let result = self.release_snapshot(parsed.snapshot_id)?;
                serde_json::to_value(result)
                    .map_err(|err| RpcErr::new("SERIALIZATION_ERROR", err.to_string()))
            }
            "query.batch" => {
                let parsed: QueryBatchParams = serde_json::from_value(params)
                    .map_err(|err| RpcErr::new("INVALID_PARAMS", err.to_string()))?;
                self.query_batch(parsed, observer)
            }
            "comments.scan" => {
                let parsed: CommentsScanParams = serde_json::from_value(params)
                    .map_err(|err| RpcErr::new("INVALID_PARAMS", err.to_string()))?;
                self.scan_comments(&parsed)
            }
            _ => Err(RpcErr::new(
                "METHOD_NOT_FOUND",
                format!("Unknown method: {method}"),
            )),
        }
    }

    fn open_snapshot(&mut self, params: &SnapshotOpenParams) -> Result<SnapshotOpenResult, RpcErr> {
        let cwd = normalize_path(&params.cwd);
        let session = self.ensure_session(&cwd);
        let opened = open_projects_snapshot(&session, &cwd, &params.entries)
            .map_err(|err| RpcErr::new(err.code, err.message))?;
        let (resolved_packages, resolved_package_versions) =
            collect_snapshot_resolved_packages(&cwd, &opened.snapshot_data);
        let summary_config = SummaryConfigPayload::default();
        let summary_index = Arc::new(BehaviorSummaryIndex::from_config(
            &summary_config,
            &resolved_package_versions,
        ));

        self.next_snapshot_id += 1;
        let snapshot_id = self.next_snapshot_id;
        self.snapshots.insert(
            snapshot_id,
            SnapshotState {
                snapshot_id,
                compiler_snapshot_id: opened.compiler_snapshot_id,
                cwd,
                entry_specs: opened.entry_specs,
                opened_configs: opened.opened_configs,
                snapshot_data: opened.snapshot_data,
                project_ids: opened.project_ids,
                handles: FxHashMap::default(),
                handle_intern: FxHashMap::default(),
                instances: Vec::new(),
                instance_handles: FxHashMap::default(),
                file_ids: Arc::clone(&self.file_ids),
                ir_cache: SnapshotIrCache::new(),
                file_syntax_indices: FxHashMap::default(),
                symbol_registry: SymbolRegistry::new(),
                ir_call_graph_cache: FxHashMap::default(),
                ir_call_graph_shards: FxHashMap::default(),
                ir_field_write_cache: FxHashMap::default(),
                ir_field_write_shards: FxHashMap::default(),
                hierarchy_cache: FxHashMap::default(),
                project_file_catalog_cache: FxHashMap::default(),
                sourcemap_cache: SourceMapCache::default(),
                resolved_package_versions,
                summary_config,
                summary_index,
                next_handle_id: 0,
            },
        );

        Ok(SnapshotOpenResult {
            snapshot_id,
            snapshot_digest: opened.snapshot_digest,
            projects: opened.summaries,
            workspace_source_files: opened.workspace_source_files,
            resolved_packages,
            engine_rss_bytes: read_process_rss_bytes(),
        })
    }

    fn set_snapshot_summaries(
        &mut self,
        params: SnapshotSetSummariesParams,
    ) -> Result<SnapshotSetSummariesResult, RpcErr> {
        let Some(snapshot) = self.snapshots.get_mut(&params.snapshot_id) else {
            return Err(RpcErr::new(
                "STALE_SNAPSHOT",
                format!("Snapshot {} is expired or unknown.", params.snapshot_id),
            ));
        };
        let index = Arc::new(BehaviorSummaryIndex::from_config(
            &params.summaries,
            &snapshot.resolved_package_versions,
        ));
        snapshot.summary_config = params.summaries;
        snapshot.summary_index = index;
        snapshot.ir_call_graph_cache.clear();
        snapshot.ir_call_graph_shards.clear();
        snapshot.ir_field_write_cache.clear();
        snapshot.ir_field_write_shards.clear();
        snapshot.hierarchy_cache.clear();
        snapshot.project_file_catalog_cache.clear();
        Ok(SnapshotSetSummariesResult { updated: true })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Validates parameters, updates snapshot, inherits IR cache, and builds result."
    )]
    fn update_snapshot(
        &mut self,
        params: &SnapshotUpdateParams,
        observer: &mut ObserveCollector,
    ) -> Result<SnapshotUpdateResult, RpcErr> {
        let Some(session) = self.session.as_ref().map(|(_, s)| s.clone()) else {
            return Err(RpcErr::new(
                "STALE_SNAPSHOT",
                "Analysis session is not initialized.".to_string(),
            ));
        };

        let (
            cwd,
            old_compiler_snapshot_id,
            old_entry_specs,
            old_opened_configs,
            old_snapshot_data,
            old_project_ids,
            old_summary_config,
        ) = {
            let Some(old_state) = self.snapshots.get(&params.snapshot_id) else {
                return Err(RpcErr::new(
                    "STALE_SNAPSHOT",
                    format!("Snapshot {} is expired or unknown.", params.snapshot_id),
                ));
            };
            (
                old_state.cwd.clone(),
                old_state.compiler_snapshot_id,
                old_state.entry_specs.clone(),
                old_state.opened_configs.clone(),
                old_state.snapshot_data.clone(),
                old_state.project_ids.clone(),
                old_state.summary_config.clone(),
            )
        };

        let updated = update_projects_snapshot(
            &session,
            &UpdateSnapshotInput {
                cwd: &cwd,
                old_compiler_snapshot_id,
                old_entry_specs: &old_entry_specs,
                old_opened_configs: &old_opened_configs,
                old_snapshot_data: &old_snapshot_data,
                old_project_ids: &old_project_ids,
                entries: params.entries.as_deref(),
                changed: &params.changed,
                created: &params.created,
                deleted: &params.deleted,
            },
        )
        .map_err(|err| RpcErr::new(err.code, err.message))?;

        drop(old_snapshot_data);
        let old_state = self.snapshots.remove(&params.snapshot_id);
        release_compiler_snapshot(&session, old_compiler_snapshot_id);

        self.next_snapshot_id += 1;
        let snapshot_id = self.next_snapshot_id;
        let opened = updated.opened;

        let (ir_cache, file_syntax_indices, files_reused, files_invalidated) =
            inherit_snapshot_caches(
                &cwd,
                old_state.as_ref(),
                &updated.recomputed_files,
                &updated.removed_files,
                &opened.snapshot_data,
            );
        observer.record_ir_file_reuse(files_reused, files_invalidated);
        drop(old_state);

        let (resolved_packages, resolved_package_versions) =
            collect_snapshot_resolved_packages(&cwd, &opened.snapshot_data);
        let summary_index = Arc::new(BehaviorSummaryIndex::from_config(
            &old_summary_config,
            &resolved_package_versions,
        ));

        self.snapshots.insert(
            snapshot_id,
            SnapshotState {
                snapshot_id,
                compiler_snapshot_id: opened.compiler_snapshot_id,
                cwd,
                entry_specs: opened.entry_specs,
                opened_configs: opened.opened_configs,
                snapshot_data: opened.snapshot_data,
                project_ids: opened.project_ids,
                handles: FxHashMap::default(),
                handle_intern: FxHashMap::default(),
                instances: Vec::new(),
                instance_handles: FxHashMap::default(),
                file_ids: Arc::clone(&self.file_ids),
                ir_cache,
                file_syntax_indices,
                symbol_registry: SymbolRegistry::new(),
                ir_call_graph_cache: FxHashMap::default(),
                ir_call_graph_shards: FxHashMap::default(),
                ir_field_write_cache: FxHashMap::default(),
                ir_field_write_shards: FxHashMap::default(),
                hierarchy_cache: FxHashMap::default(),
                project_file_catalog_cache: FxHashMap::default(),
                sourcemap_cache: SourceMapCache::default(),
                resolved_package_versions,
                summary_config: old_summary_config,
                summary_index,
                next_handle_id: 0,
            },
        );

        Ok(SnapshotUpdateResult {
            snapshot_id,
            snapshot_digest: opened.snapshot_digest,
            projects: opened.summaries,
            workspace_source_files: opened.workspace_source_files,
            resolved_packages,
            recomputed_projects: updated.recomputed_projects,
            reused_projects: updated.reused_projects,
            recomputed_files: updated.recomputed_files,
            reused_files: updated.reused_files,
            removed_files: updated.removed_files,
            engine_rss_bytes: read_process_rss_bytes(),
        })
    }

    fn release_snapshot(&mut self, snapshot_id: u64) -> Result<SnapshotReleaseResult, RpcErr> {
        let Some(state) = self.snapshots.remove(&snapshot_id) else {
            return Err(RpcErr::new(
                "STALE_SNAPSHOT",
                format!("Snapshot {snapshot_id} is not active."),
            ));
        };
        let compiler_snapshot_id = state.compiler_snapshot_id;
        drop(state);
        if let Some((_, session)) = &self.session {
            release_compiler_snapshot(session, compiler_snapshot_id);
        }
        Ok(SnapshotReleaseResult { released: true })
    }

    fn query_batch(
        &mut self,
        params: QueryBatchParams,
        observer: &mut ObserveCollector,
    ) -> Result<Value, RpcErr> {
        let Some(snapshot) = self.snapshots.get_mut(&params.snapshot_id) else {
            return Err(RpcErr::new(
                "STALE_SNAPSHOT",
                format!("Snapshot {} is expired or unknown.", params.snapshot_id),
            ));
        };

        let Some(pid) = snapshot.project_ids.get(&params.project_id).cloned() else {
            return Err(RpcErr::new(
                "UNKNOWN_PROJECT",
                format!(
                    "Project {} is not part of snapshot {}.",
                    params.project_id, params.snapshot_id
                ),
            ));
        };

        let program = snapshot
            .snapshot_data
            .get_program(&pid)
            .map_err(|err| RpcErr::new("ENGINE_ERROR", err))?;

        let result = execute_query_batch(
            snapshot,
            &params.project_id,
            program,
            params.steps,
            observer,
        )?;
        serde_json::to_value(result)
            .map_err(|err| RpcErr::new("SERIALIZATION_ERROR", err.to_string()))
    }

    fn scan_comments(&self, params: &CommentsScanParams) -> Result<Value, RpcErr> {
        let Some(snapshot) = self.snapshots.get(&params.snapshot_id) else {
            return Err(RpcErr::new(
                "STALE_SNAPSHOT",
                format!("Snapshot {} is expired or unknown.", params.snapshot_id),
            ));
        };
        let comments = scan_snapshot_comments(snapshot, params.files.as_deref());
        serde_json::to_value(CommentsScanResult { comments })
            .map_err(|err| RpcErr::new("SERIALIZATION_ERROR", err.to_string()))
    }

    fn shutdown(mut self) {
        let snapshots = std::mem::take(&mut self.snapshots);
        if let Some((_, session)) = self.session.take() {
            for state in snapshots.into_values() {
                let id = state.compiler_snapshot_id;
                drop(state);
                release_compiler_snapshot(&session, id);
            }
            session.close();
        }
    }
}

pub fn run_list_branches(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
    observer: &mut ObserveCollector,
) -> Result<Vec<FunctionBranchesItem>, RpcErr> {
    let mut fn_nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_syntax_node(project_id, handle_id)?;
        let Some(scope_node) = node.normalize_cfg_scope() else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!(
                    "Program reference {handle_id} is not a function, block, or statement syntax node."
                ),
            ));
        };
        fn_nodes.push((handle_id.clone(), scope_node));
    }

    let mut items = Vec::with_capacity(fn_nodes.len());
    for (target_id, scope_node) in fn_nodes {
        match snapshot.get_or_build_ir(program, checker, scope_node, observer) {
            Ok((ir, file_index)) => {
                let mut branch_items = Vec::with_capacity(ir.branches.len());
                for branch in &ir.branches {
                    let branch_ref = snapshot.alloc_branch(
                        project_id,
                        BranchHandle {
                            function_node: scope_node,
                            edge_index: usize::try_from(branch.id.0).unwrap_or(0),
                        },
                    );
                    let cond_node = file_index
                        .boundary_node_for_syntax_id(branch.condition_syntax)
                        .unwrap_or(scope_node);
                    let condition = snapshot.alloc_syntax(project_id, cond_node);
                    branch_items.push(BranchEdgeItem {
                        branch_ref,
                        kind: branch.kind.as_str(),
                        direction: branch.direction.as_str(),
                        condition,
                        condition_text: branch.condition_text.clone(),
                        location: branch.location.clone(),
                        statement_location: branch.statement_location.clone(),
                    });
                }
                items.push(FunctionBranchesItem {
                    target: target_id,
                    branches: branch_items,
                    gaps: Vec::new(),
                });
            }
            Err(gap) => {
                items.push(FunctionBranchesItem {
                    target: target_id,
                    branches: Vec::new(),
                    gaps: vec![gap],
                });
            }
        }
    }

    Ok(items)
}

pub struct BranchCoverageRequest<'a> {
    pub project_id: &'a str,
    pub program: BoundaryProgram,
    pub function_handle: Option<&'a str>,
    pub scope_handle: Option<&'a str>,
    pub branch_handles: &'a [String],
    pub target_handles: &'a [String],
}

fn resolve_explicit_scope(
    snapshot: &SnapshotState,
    project_id: &str,
    function_handle: Option<&str>,
    scope_handle: Option<&str>,
) -> Result<Option<BoundaryNode>, RpcErr> {
    if function_handle.is_some() && scope_handle.is_some() {
        return Err(RpcErr::new(
            "INVALID_CONFIG",
            "branchCoverage accepts either 'function' or 'scope', not both.".to_string(),
        ));
    }
    if let Some(fn_id) = function_handle {
        let node = snapshot.get_syntax_node(project_id, fn_id)?;
        let Some(fn_node) = node.normalize_function() else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {fn_id} is not a function-like syntax node."),
            ));
        };
        return Ok(Some(fn_node));
    }
    if let Some(scope_id) = scope_handle {
        let node = snapshot.get_syntax_node(project_id, scope_id)?;
        let Some(scope_node) = node.normalize_cfg_scope() else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!(
                    "Program reference {scope_id} is not a function, block, or statement syntax node."
                ),
            ));
        };
        return Ok(Some(scope_node));
    }
    Ok(None)
}

fn outcome_to_branch_coverage_item(
    target: String,
    outcome: BranchCoverageOutcome,
) -> BranchCoverageItem {
    match outcome {
        BranchCoverageOutcome::Proved {
            target_reachable,
            implicit_exceptions,
            evidence,
        } => BranchCoverageItem::Proved {
            target,
            target_reachable,
            implicit_exceptions,
            evidence,
        },
        BranchCoverageOutcome::Disproved {
            target_reachable,
            implicit_exceptions,
            counterexample_path,
            evidence,
        } => BranchCoverageItem::Disproved {
            target,
            target_reachable,
            implicit_exceptions,
            counterexample_path,
            evidence,
        },
        BranchCoverageOutcome::Inconclusive {
            target_reachable,
            implicit_exceptions,
            gaps,
            evidence,
        } => BranchCoverageItem::Inconclusive {
            target,
            target_reachable,
            implicit_exceptions,
            gaps,
            evidence,
        },
    }
}

pub fn run_branch_coverage(
    snapshot: &mut SnapshotState,
    checker: &mut BoundaryChecker<'_>,
    req: &BranchCoverageRequest<'_>,
    observer: &mut ObserveCollector,
) -> Result<Vec<BranchCoverageItem>, RpcErr> {
    let explicit_scope = resolve_explicit_scope(
        snapshot,
        req.project_id,
        req.function_handle,
        req.scope_handle,
    )?;

    let mut resolved_branches = Vec::with_capacity(req.branch_handles.len());
    for branch_id in req.branch_handles {
        resolved_branches.push(snapshot.get_branch(req.project_id, branch_id)?);
    }

    let mut resolved_targets = Vec::with_capacity(req.target_handles.len());
    for target_id in req.target_handles {
        let node = snapshot.get_syntax_node(req.project_id, target_id)?;
        let Some(sf) = node.source_file() else {
            return Err(RpcErr::new(
                "ENGINE_ERROR",
                format!("Target syntax node {target_id} has no source file."),
            ));
        };
        let rel_file = sf.rel_path(&snapshot.cwd);
        let loc = node.location(sf, &rel_file);
        resolved_targets.push((target_id.clone(), node, loc));
    }

    let mut items = Vec::with_capacity(resolved_targets.len());
    for (target_id, target_node, target_loc) in resolved_targets {
        let scope_fn = explicit_scope.or_else(|| target_node.resolve_enclosing_function());
        let Some(scope_node) = scope_fn else {
            let gap = GapItem::from_reason(
                UnknownReason::NoEnclosingFunction,
                "Target is not inside a function body.".to_string(),
                Some(target_loc),
            );
            let evidence = vec![EvidenceItem::simple(
                "note",
                gap.message.clone(),
                gap.at.clone(),
            )];
            items.push(BranchCoverageItem::Inconclusive {
                target: target_id,
                target_reachable: false,
                implicit_exceptions: ImplicitExceptionCoverageOutcome::Inconclusive {
                    may_throw_sites: Vec::new(),
                    gaps: vec![gap.clone()],
                    evidence: evidence.clone(),
                },
                gaps: vec![gap],
                evidence,
            });
            continue;
        };

        let (ir, file_index) =
            match snapshot.get_or_build_ir(req.program, checker, scope_node, observer) {
                Ok(pair) => pair,
                Err(gap) => {
                    let evidence = vec![EvidenceItem::simple(
                        "note",
                        gap.message.clone(),
                        gap.at.clone(),
                    )];
                    items.push(BranchCoverageItem::Inconclusive {
                        target: target_id,
                        target_reachable: false,
                        implicit_exceptions: ImplicitExceptionCoverageOutcome::Inconclusive {
                            may_throw_sites: Vec::new(),
                            gaps: vec![gap.clone()],
                            evidence: evidence.clone(),
                        },
                        gaps: vec![gap],
                        evidence,
                    });
                    continue;
                }
            };

        let mut selected_branches = Vec::new();
        let mut extra_gaps = Vec::new();
        for branch in &resolved_branches {
            if branch.function_node == scope_node {
                let bid = BranchId(u32::try_from(branch.edge_index).unwrap_or(u32::MAX));
                selected_branches.push(bid);
            } else if explicit_scope.is_some() {
                extra_gaps.push(GapItem::from_reason(
                    UnknownReason::ForeignBranchReference,
                    "Branch reference belongs to a different function than the target scope."
                        .to_string(),
                    Some(target_loc.clone()),
                ));
            }
        }

        let target_syntax = file_index.boundary_syntax_id_for_node(target_node);
        let outcome = evaluate_branch_coverage(
            &ir,
            target_syntax,
            &target_loc,
            &selected_branches,
            &extra_gaps,
        );
        items.push(outcome_to_branch_coverage_item(target_id, outcome));
    }

    Ok(items)
}

fn inherit_snapshot_caches(
    cwd: &str,
    old_state: Option<&SnapshotState>,
    recomputed_files: &[String],
    removed_files: &[String],
    new_snapshot_data: &BoundarySnapshotData,
) -> (
    SnapshotIrCache,
    FxHashMap<FileId, Arc<FileSyntaxIndex>>,
    usize,
    usize,
) {
    let Some(prev) = old_state else {
        return (SnapshotIrCache::new(), FxHashMap::default(), 0, 0);
    };
    let recomputed_set: BTreeSet<&str> = recomputed_files.iter().map(String::as_str).collect();
    let removed_set: BTreeSet<&str> = removed_files.iter().map(String::as_str).collect();
    let new_source_file_addrs = new_snapshot_data.source_file_addrs(cwd);
    let (ir_cache, files_reused, files_invalidated) =
        SnapshotIrCache::inherit_from(&prev.ir_cache, |entry| {
            !recomputed_set.contains(entry.rel_file.as_str())
                && !removed_set.contains(entry.rel_file.as_str())
                && new_source_file_addrs.get(entry.rel_file.as_str())
                    == Some(&entry.source_file_addr)
        });
    let mut file_syntax_indices = FxHashMap::default();
    for (&fid, idx) in &prev.file_syntax_indices {
        if !recomputed_set.contains(idx.rel_file.as_str())
            && !removed_set.contains(idx.rel_file.as_str())
            && new_source_file_addrs.get(idx.rel_file.as_str()) == Some(&idx.source_file_addr)
        {
            file_syntax_indices.insert(fid, Arc::clone(idx));
        }
    }
    (
        ir_cache,
        file_syntax_indices,
        files_reused,
        files_invalidated,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SeenPackageVersion {
    NoneYet,
    Unique(String),
    Multiple,
}

fn record_package_version(
    seen: &mut FxHashMap<String, SeenPackageVersion>,
    name: &str,
    version: Option<&str>,
) {
    match version {
        Some(ver) => match seen.get_mut(name) {
            None => {
                seen.insert(
                    name.to_string(),
                    SeenPackageVersion::Unique(ver.to_string()),
                );
            }
            Some(entry @ SeenPackageVersion::NoneYet) => {
                *entry = SeenPackageVersion::Unique(ver.to_string());
            }
            Some(entry @ SeenPackageVersion::Unique(_)) => {
                if let SeenPackageVersion::Unique(existing) = entry
                    && existing.as_str() != ver
                {
                    *entry = SeenPackageVersion::Multiple;
                }
            }
            Some(SeenPackageVersion::Multiple) => {}
        },
        None => {
            seen.entry(name.to_string())
                .or_insert(SeenPackageVersion::NoneYet);
        }
    }
}

fn finalize_package_versions(
    seen: FxHashMap<String, SeenPackageVersion>,
) -> FxHashMap<String, Option<String>> {
    seen.into_iter()
        .map(|(name, state)| {
            let ver = match state {
                SeenPackageVersion::Unique(v) => Some(v),
                SeenPackageVersion::NoneYet | SeenPackageVersion::Multiple => None,
            };
            (name, ver)
        })
        .collect()
}

fn collect_snapshot_resolved_packages(
    cwd: &str,
    snapshot_data: &BoundarySnapshotData,
) -> (
    Vec<ResolvedPackagePayload>,
    FxHashMap<String, Option<String>>,
) {
    let mut merged: BTreeMap<(String, String), (ResolvedPackageInfo, BTreeSet<String>)> =
        BTreeMap::new();
    let mut seen_versions: FxHashMap<String, SeenPackageVersion> = FxHashMap::default();

    for (prog, proj_dir) in snapshot_data.project_program_dirs(cwd) {
        for pkg in prog.collect_resolved_packages(cwd) {
            record_package_version(&mut seen_versions, &pkg.name, pkg.version.as_deref());
            let key = (
                pkg.name.clone(),
                pkg.package_root.clone().unwrap_or_default(),
            );
            let entry = merged
                .entry(key)
                .or_insert_with(|| (pkg.clone(), BTreeSet::new()));
            if entry.0.version.is_none() && pkg.version.is_some() {
                entry.0.version = pkg.version;
            }
            entry.1.insert(proj_dir.clone());
        }
    }

    let payloads = merged
        .into_values()
        .map(|(pkg, dirs)| ResolvedPackagePayload {
            name: pkg.name,
            version: pkg.version,
            package_root: pkg.package_root,
            project_dirs: dirs.into_iter().collect(),
        })
        .collect();
    (payloads, finalize_package_versions(seen_versions))
}

fn read_process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb_str = rest.split_whitespace().next()?;
            let kb: u64 = kb_str.parse().ok()?;
            return kb.checked_mul(1024);
        }
    }
    None
}

fn serialize_error(id: &Value, code: &str, message: String, details: Option<&Value>) -> String {
    serde_json::to_string(&RpcErrorResponse {
        id,
        ok: false,
        error: RpcErrorPayload {
            code,
            message,
            details,
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":null,"ok":false,"error":{"code":"SERIALIZATION_ERROR","message":"Failed to serialize error."}}"#.to_string()
    })
}

pub fn run_stdio() -> io::Result<()> {
    let mut server = EngineServer::new();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = stdin.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = server.handle_line(trimmed);
        let mut out = stdout.lock();
        writeln!(out, "{response}")?;
        out.flush()?;
    }

    server.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_package_version_map_clears_ambiguous_multi_version_fallback() {
        let mut seen = FxHashMap::default();
        record_package_version(&mut seen, "single-pkg", Some("1.2.0"));
        record_package_version(&mut seen, "single-pkg", Some("1.2.0"));
        record_package_version(&mut seen, "single-pkg", None);

        record_package_version(&mut seen, "multi-pkg", Some("1.2.0"));
        record_package_version(&mut seen, "multi-pkg", Some("2.1.0"));

        record_package_version(&mut seen, "none-then-some", None);
        record_package_version(&mut seen, "none-then-some", Some("3.0.0"));

        record_package_version(&mut seen, "none-only", None);

        let finalized = finalize_package_versions(seen);
        assert_eq!(
            finalized.get("single-pkg"),
            Some(&Some("1.2.0".to_string()))
        );
        assert_eq!(finalized.get("multi-pkg"), Some(&None));
        assert_eq!(
            finalized.get("none-then-some"),
            Some(&Some("3.0.0".to_string()))
        );
        assert_eq!(finalized.get("none-only"), Some(&None));
    }
}
