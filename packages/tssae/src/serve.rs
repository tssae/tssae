use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::Arc,
    time::Instant,
};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tsrs_api::{Session, SessionOptions, session::SnapshotData};
use tsrs_ast::{Node, Symbol, get_source_file_of_node};
use tsrs_checker::{Signature, Type};
use tsrs_core::P;
use tsrs_project::ID as ProjectId;
use tsrs_vfs::{FS, bundled, osvfs};

use crate::calls_values::ProjectCallGraph;
use crate::cfg::{
    EvidenceItem, FunctionCfg, GapItem, normalize_cfg_scope_node, normalize_function_node,
    resolve_enclosing_function,
};
use crate::directives::{ScannedDirective, scan_snapshot_directives};
use crate::instance_identity::{FieldWriteIndex, InstanceHandleData};
use crate::native_adapter::{
    NativeAdapterObservation, NativeAdapterPackageInput, NativeAdapterRegistry, NativeAdapterStatus,
};
use crate::observe::{EngineRpcMetrics, ObserveCollector, ObserveRequestConfig, elapsed_ms};
use crate::package_impl::PackageImplCache;
use crate::projection::{QueryStep, execute_query_batch};
pub use crate::projection::{SourceLocation, node_location, slice_source_text};
use crate::projects::{
    ProjectSummary, UpdateSnapshotInput, normalize_path, open_projects_snapshot, relative_to_cwd,
    release_tsrs_snapshot, update_projects_snapshot,
};

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
}

impl RpcErr {
    pub const fn new(code: &'static str, message: String) -> Self {
        Self { code, message }
    }
}

#[derive(Deserialize)]
struct SnapshotOpenParams {
    cwd: String,
    entries: Vec<String>,
    #[serde(default, rename = "nativeAdapters")]
    native_adapters: Vec<NativeAdapterPackageInput>,
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
    #[serde(default, rename = "nativeAdapters")]
    native_adapters: Option<Vec<NativeAdapterPackageInput>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotReleaseParams {
    snapshot_id: u64,
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
struct DirectivesScanParams {
    snapshot_id: u64,
}

#[derive(Serialize)]
struct DirectivesScanResult {
    directives: Vec<ScannedDirective>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotOpenResult {
    snapshot_id: u64,
    snapshot_digest: String,
    projects: Vec<ProjectSummary>,
    workspace_source_files: Vec<String>,
    native_adapters: Vec<NativeAdapterObservation>,
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
    native_adapters: Vec<NativeAdapterObservation>,
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
#[serde(rename_all = "camelCase")]
pub struct BranchCoverageItem {
    pub target: String,
    pub status: &'static str,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

#[derive(Clone, Copy)]
pub struct BranchHandle {
    pub function_node: P<Node>,
    pub edge_index: usize,
}

#[derive(Clone, Copy)]
pub enum HandlePayload {
    Syntax(P<Node>),
    Declaration {
        node: P<Node>,
        symbol: Option<P<Symbol>>,
    },
    Call(P<Node>),
    Signature {
        sig: P<Signature>,
        call_node: Option<P<Node>>,
    },
    Type {
        ty: P<Type>,
        enclosing: Option<P<Node>>,
    },
    Branch(BranchHandle),
    Instance(usize),
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum HandleInternKey {
    Syntax(String, P<Node>),
    Declaration {
        project_id: String,
        node: P<Node>,
    },
    Call(String, P<Node>),
    Signature {
        project_id: String,
        sig: P<Signature>,
        call_node: Option<P<Node>>,
    },
    Type {
        project_id: String,
        ty: P<Type>,
        enclosing: Option<P<Node>>,
    },
    Branch {
        project_id: String,
        function_node: P<Node>,
        edge_index: usize,
    },
}

pub struct HandleEntry {
    pub project_id: String,
    pub payload: HandlePayload,
}

pub struct SnapshotState {
    pub snapshot_id: u64,
    pub tsrs_snapshot_id: u64,
    pub cwd: String,
    pub entry_specs: Vec<String>,
    pub opened_configs: Vec<String>,
    pub snapshot_data: Arc<SnapshotData>,
    pub project_ids: BTreeMap<String, ProjectId>,
    pub handles: FxHashMap<String, HandleEntry>,
    pub handle_intern: FxHashMap<HandleInternKey, String>,
    pub instances: Vec<InstanceHandleData>,
    pub instance_handles: FxHashMap<(String, String), String>,
    pub cfg_cache: FxHashMap<(String, P<Node>), Result<Arc<FunctionCfg>, GapItem>>,
    pub call_graph_cache: FxHashMap<String, Arc<ProjectCallGraph>>,
    pub field_write_index: FxHashMap<String, Arc<FieldWriteIndex>>,
    pub package_impl_cache: PackageImplCache,
    pub native_adapters: NativeAdapterRegistry,
    pub native_fact_cache: crate::native_adapter::NativeFactCache,
    pub next_handle_id: u64,
}

impl SnapshotState {
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

    pub fn alloc_syntax(&mut self, project_id: &str, node: P<Node>) -> String {
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
        node: P<Node>,
        symbol: Option<P<Symbol>>,
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

    pub fn alloc_call(&mut self, project_id: &str, node: P<Node>) -> String {
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
        sig: P<Signature>,
        call_node: Option<P<Node>>,
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
        ty: P<Type>,
        enclosing: Option<P<Node>>,
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

    pub fn get_syntax_node(&self, project_id: &str, handle_id: &str) -> Result<P<Node>, RpcErr> {
        let entry = self.get_entry(project_id, handle_id)?;
        let HandlePayload::Syntax(node) = entry.payload else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {handle_id} is not a syntax reference."),
            ));
        };
        Ok(node)
    }

    pub fn get_call_node(&self, project_id: &str, handle_id: &str) -> Result<P<Node>, RpcErr> {
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
    ) -> Result<(P<Signature>, Option<P<Node>>), RpcErr> {
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
    ) -> Result<(P<Type>, Option<P<Node>>), RpcErr> {
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

    pub fn get_or_build_cfg(
        &mut self,
        project_id: &str,
        fn_node: P<Node>,
    ) -> Result<Arc<FunctionCfg>, GapItem> {
        let key = (project_id.to_string(), fn_node);
        if let Some(cached) = self.cfg_cache.get(&key) {
            return cached.clone();
        }
        let Some(sf) = get_source_file_of_node(fn_node) else {
            return Err(GapItem {
                kind: "missing-source-file",
                message: "Function node has no associated source file.".to_string(),
                at: None,
            });
        };
        let rel_file = relative_to_cwd(&self.cwd, sf.file_name());
        let built = FunctionCfg::build_scope(sf, &rel_file, fn_node).map(Arc::new);
        self.cfg_cache.insert(key, built.clone());
        built
    }
}

struct EngineServer {
    session: Option<(String, Arc<Session>)>,
    snapshots: BTreeMap<u64, SnapshotState>,
    next_snapshot_id: u64,
}

impl EngineServer {
    const fn new() -> Self {
        Self {
            session: None,
            snapshots: BTreeMap::new(),
            next_snapshot_id: 0,
        }
    }

    fn ensure_session(&mut self, cwd: &str) -> Arc<Session> {
        if let Some((existing_cwd, existing_session)) = &self.session
            && existing_cwd == cwd
        {
            return Arc::clone(existing_session);
        }
        if let Some((_, old_session)) = self.session.take() {
            self.snapshots.clear();
            old_session.close();
        }
        let fs: Arc<dyn FS> = Arc::new(bundled::wrap_fs(osvfs::fs()));
        let session = Session::new(SessionOptions::new(
            cwd.to_string(),
            bundled::default_lib_path(),
            fs,
            false,
        ));
        self.session = Some((cwd.to_string(), Arc::clone(&session)));
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
                    serialize_error(&request.id, "SERIALIZATION_ERROR", err.to_string())
                })
            }
            Err(err) => serialize_error(&request.id, err.code, err.message),
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
                let result = self.update_snapshot(&parsed)?;
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
            "directives.scan" => {
                let parsed: DirectivesScanParams = serde_json::from_value(params)
                    .map_err(|err| RpcErr::new("INVALID_PARAMS", err.to_string()))?;
                self.scan_directives(parsed.snapshot_id)
            }
            _ => Err(RpcErr::new(
                "METHOD_NOT_FOUND",
                format!("Unknown method: {method}"),
            )),
        }
    }

    fn open_snapshot(&mut self, params: &SnapshotOpenParams) -> Result<SnapshotOpenResult, RpcErr> {
        let native_adapters =
            crate::native_adapter::load_configured_adapters(&params.native_adapters)?;
        let native_adapter_observations =
            crate::native_adapter::observe_registry(&native_adapters, NativeAdapterStatus::Loaded);
        let cwd = normalize_path(&params.cwd);
        let session = self.ensure_session(&cwd);
        let opened = open_projects_snapshot(&session, &cwd, &params.entries)
            .map_err(|err| RpcErr::new(err.code, err.message))?;

        self.next_snapshot_id += 1;
        let snapshot_id = self.next_snapshot_id;
        self.snapshots.insert(
            snapshot_id,
            SnapshotState {
                snapshot_id,
                tsrs_snapshot_id: opened.tsrs_snapshot_id,
                cwd,
                entry_specs: opened.entry_specs,
                opened_configs: opened.opened_configs,
                snapshot_data: opened.snapshot_data,
                project_ids: opened.project_ids,
                handles: FxHashMap::default(),
                handle_intern: FxHashMap::default(),
                instances: Vec::new(),
                instance_handles: FxHashMap::default(),
                cfg_cache: FxHashMap::default(),
                call_graph_cache: FxHashMap::default(),
                field_write_index: FxHashMap::default(),
                package_impl_cache: PackageImplCache::default(),
                native_adapters,
                native_fact_cache: crate::native_adapter::NativeFactCache::default(),
                next_handle_id: 0,
            },
        );

        Ok(SnapshotOpenResult {
            snapshot_id,
            snapshot_digest: opened.snapshot_digest,
            projects: opened.summaries,
            workspace_source_files: opened.workspace_source_files,
            native_adapters: native_adapter_observations,
            engine_rss_bytes: read_process_rss_bytes(),
        })
    }

    fn update_snapshot(
        &mut self,
        params: &SnapshotUpdateParams,
    ) -> Result<SnapshotUpdateResult, RpcErr> {
        let Some(session) = self.session.as_ref().map(|(_, s)| Arc::clone(s)) else {
            return Err(RpcErr::new(
                "STALE_SNAPSHOT",
                "Analysis session is not initialized.".to_string(),
            ));
        };

        let (
            cwd,
            old_tsrs_snapshot_id,
            old_entry_specs,
            old_opened_configs,
            old_snapshot_data,
            old_project_ids,
            old_native_adapters,
        ) = {
            let Some(old_state) = self.snapshots.get(&params.snapshot_id) else {
                return Err(RpcErr::new(
                    "STALE_SNAPSHOT",
                    format!("Snapshot {} is expired or unknown.", params.snapshot_id),
                ));
            };
            (
                old_state.cwd.clone(),
                old_state.tsrs_snapshot_id,
                old_state.entry_specs.clone(),
                old_state.opened_configs.clone(),
                Arc::clone(&old_state.snapshot_data),
                old_state.project_ids.clone(),
                old_state.native_adapters.clone(),
            )
        };

        let (native_adapters, native_adapter_status) = match params.native_adapters.as_deref() {
            Some(inputs) => (
                crate::native_adapter::load_configured_adapters(inputs)?,
                NativeAdapterStatus::Loaded,
            ),
            None => (old_native_adapters, NativeAdapterStatus::Reused),
        };
        let native_adapter_observations =
            crate::native_adapter::observe_registry(&native_adapters, native_adapter_status);

        let updated = update_projects_snapshot(
            &session,
            &UpdateSnapshotInput {
                cwd: &cwd,
                old_tsrs_snapshot_id,
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
        drop(self.snapshots.remove(&params.snapshot_id));
        release_tsrs_snapshot(&session, old_tsrs_snapshot_id);

        self.next_snapshot_id += 1;
        let snapshot_id = self.next_snapshot_id;
        let opened = updated.opened;
        self.snapshots.insert(
            snapshot_id,
            SnapshotState {
                snapshot_id,
                tsrs_snapshot_id: opened.tsrs_snapshot_id,
                cwd,
                entry_specs: opened.entry_specs,
                opened_configs: opened.opened_configs,
                snapshot_data: opened.snapshot_data,
                project_ids: opened.project_ids,
                handles: FxHashMap::default(),
                handle_intern: FxHashMap::default(),
                instances: Vec::new(),
                instance_handles: FxHashMap::default(),
                cfg_cache: FxHashMap::default(),
                call_graph_cache: FxHashMap::default(),
                field_write_index: FxHashMap::default(),
                package_impl_cache: PackageImplCache::default(),
                native_adapters,
                native_fact_cache: crate::native_adapter::NativeFactCache::default(),
                next_handle_id: 0,
            },
        );

        Ok(SnapshotUpdateResult {
            snapshot_id,
            snapshot_digest: opened.snapshot_digest,
            projects: opened.summaries,
            workspace_source_files: opened.workspace_source_files,
            native_adapters: native_adapter_observations,
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
        let tsrs_snapshot_id = state.tsrs_snapshot_id;
        drop(state);
        if let Some((_, session)) = &self.session {
            release_tsrs_snapshot(session, tsrs_snapshot_id);
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
            .map_err(|err| RpcErr::new("ENGINE_ERROR", err.to_string()))?;

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

    fn scan_directives(&self, snapshot_id: u64) -> Result<Value, RpcErr> {
        let Some(snapshot) = self.snapshots.get(&snapshot_id) else {
            return Err(RpcErr::new(
                "STALE_SNAPSHOT",
                format!("Snapshot {snapshot_id} is expired or unknown."),
            ));
        };
        let directives = scan_snapshot_directives(snapshot);
        serde_json::to_value(DirectivesScanResult { directives })
            .map_err(|err| RpcErr::new("SERIALIZATION_ERROR", err.to_string()))
    }

    fn shutdown(mut self) {
        let snapshots = std::mem::take(&mut self.snapshots);
        if let Some((_, session)) = self.session.take() {
            for state in snapshots.into_values() {
                let id = state.tsrs_snapshot_id;
                drop(state);
                release_tsrs_snapshot(&session, id);
            }
            session.close();
        }
    }
}

pub fn run_list_branches(
    snapshot: &mut SnapshotState,
    project_id: &str,
    targets: &[String],
) -> Result<Vec<FunctionBranchesItem>, RpcErr> {
    let mut fn_nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_syntax_node(project_id, handle_id)?;
        let Some(scope_node) = normalize_cfg_scope_node(node) else {
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
    for (target_id, fn_node) in fn_nodes {
        match snapshot.get_or_build_cfg(project_id, fn_node) {
            Ok(cfg) => {
                let mut branch_items = Vec::with_capacity(cfg.branches.len());
                for (edge_index, branch) in cfg.branches.iter().enumerate() {
                    let branch_ref = snapshot.alloc_branch(
                        project_id,
                        BranchHandle {
                            function_node: fn_node,
                            edge_index,
                        },
                    );
                    let condition = snapshot.alloc_syntax(project_id, branch.condition_node);
                    branch_items.push(BranchEdgeItem {
                        branch_ref,
                        kind: branch.kind,
                        direction: branch.direction,
                        condition,
                        condition_text: branch.condition_text.clone(),
                        location: branch.location.clone(),
                        statement_location: branch.statement_location.clone(),
                    });
                }
                items.push(FunctionBranchesItem {
                    target: target_id,
                    branches: branch_items,
                    gaps: cfg.gaps.clone(),
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

pub fn run_branch_coverage(
    snapshot: &mut SnapshotState,
    project_id: &str,
    function_handle: Option<&str>,
    scope_handle: Option<&str>,
    branch_handles: &[String],
    target_handles: &[String],
) -> Result<Vec<BranchCoverageItem>, RpcErr> {
    if function_handle.is_some() && scope_handle.is_some() {
        return Err(RpcErr::new(
            "INVALID_CONFIG",
            "branchCoverage accepts either 'function' or 'scope', not both.".to_string(),
        ));
    }

    let explicit_scope = if let Some(fn_id) = function_handle {
        let node = snapshot.get_syntax_node(project_id, fn_id)?;
        let Some(fn_node) = normalize_function_node(node) else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {fn_id} is not a function-like syntax node."),
            ));
        };
        Some(fn_node)
    } else if let Some(scope_id) = scope_handle {
        let node = snapshot.get_syntax_node(project_id, scope_id)?;
        let Some(scope_node) = normalize_cfg_scope_node(node) else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!(
                    "Program reference {scope_id} is not a function, block, or statement syntax node."
                ),
            ));
        };
        Some(scope_node)
    } else {
        None
    };

    let mut resolved_branches = Vec::with_capacity(branch_handles.len());
    for branch_id in branch_handles {
        resolved_branches.push(snapshot.get_branch(project_id, branch_id)?);
    }

    let mut resolved_targets = Vec::with_capacity(target_handles.len());
    for target_id in target_handles {
        let node = snapshot.get_syntax_node(project_id, target_id)?;
        let Some(sf) = get_source_file_of_node(node) else {
            return Err(RpcErr::new(
                "ENGINE_ERROR",
                format!("Target syntax node {target_id} has no source file."),
            ));
        };
        let rel_file = relative_to_cwd(&snapshot.cwd, sf.file_name());
        let loc = node_location(sf, &rel_file, node);
        resolved_targets.push((target_id.clone(), node, loc));
    }

    let mut items = Vec::with_capacity(resolved_targets.len());
    for (target_id, target_node, target_loc) in resolved_targets {
        let scope_fn = explicit_scope.or_else(|| resolve_enclosing_function(target_node));
        let Some(fn_node) = scope_fn else {
            items.push(BranchCoverageItem {
                target: target_id,
                status: "unknown",
                evidence: Vec::new(),
                gaps: vec![GapItem {
                    kind: "no-enclosing-function",
                    message: "Target is not inside a function body.".to_string(),
                    at: Some(target_loc),
                }],
            });
            continue;
        };

        let cfg = match snapshot.get_or_build_cfg(project_id, fn_node) {
            Ok(cfg) => cfg,
            Err(gap) => {
                items.push(BranchCoverageItem {
                    target: target_id,
                    status: "unknown",
                    evidence: Vec::new(),
                    gaps: vec![gap],
                });
                continue;
            }
        };

        let mut selected_edges = Vec::new();
        let mut extra_gaps = Vec::new();
        for branch in &resolved_branches {
            if branch.function_node == fn_node {
                selected_edges.push(branch.edge_index);
            } else if explicit_scope.is_some() {
                extra_gaps.push(GapItem {
                    kind: "foreign-branch-reference",
                    message:
                        "Branch reference belongs to a different function than the target scope."
                            .to_string(),
                    at: Some(target_loc.clone()),
                });
            }
        }

        let outcome = cfg.evaluate_coverage(target_node, &target_loc, &selected_edges, &extra_gaps);
        items.push(BranchCoverageItem {
            target: target_id,
            status: outcome.status.as_str(),
            evidence: outcome.evidence,
            gaps: outcome.gaps,
        });
    }

    Ok(items)
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

fn serialize_error(id: &Value, code: &str, message: String) -> String {
    serde_json::to_string(&RpcErrorResponse {
        id,
        ok: false,
        error: RpcErrorPayload { code, message },
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
