use std::{cell::Cell, sync::Arc};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use tsrs_ast::{Node, SourceFile, Symbol};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;
use tssae_analysis::{
    IrProvider,
    call_resolution::{
        CallParameterBindingData, CallTargetsOutcome, CallerSiteData, CallersOutcome, IrCallGraph,
        ResolvedCallTargetData, build_ir_call_graph_file_shard, find_callers_on_ir,
        merge_ir_call_graph_shards, resolve_call_targets_with_summaries,
    },
    instance_identity::{
        IrFieldWriteIndex, build_ir_field_write_file_shard, merge_ir_field_write_shards,
    },
    ir::{FunctionIr, IrScopeKind, ScopeCatalogEntry},
    summaries::BehaviorSummaryIndex,
};
use tssae_common::{
    DeclarationIdentity, DeclarationSummaryData, DiagnosticLocation, EvidenceItem, FileId, GapItem,
    ImplementationFact, ImplementationSourceKind, MemberTargetHierarchy, SemanticModel,
    SignatureId, SummaryLookupStats, SummarySource, SymbolId, SyntaxCatalog, SyntaxId,
    SyntaxScopeInfo, TypeId, TypeSummaryData, UnknownReason, WriteSyntaxInfo,
};
use tssae_tsrs_boundary::{
    FirstPartyResolutionSource, SemanticModelBridge, normalize_path, resolve_enclosing_cfg_owner,
};

use crate::ir_impls::ResolvedImplTargetPayload;
use crate::observe::ObserveCollector;
use crate::projects::{is_project_source_file, relative_to_cwd};
use crate::serve::{HandlePayload, RpcErr, SnapshotState, SourceLocation};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSiteParameterBindingPayload {
    pub argument_index: usize,
    pub argument: String,
    pub parameter_name: Option<String>,
    pub parameter_declaration: Option<String>,
    pub is_rest: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectCallInfoPayload {
    pub call: String,
    pub syntax: String,
    pub location: SourceLocation,
    pub caller_declaration: Option<String>,
    pub parameter_bindings: Vec<CallSiteParameterBindingPayload>,
    pub return_target: String,
}

#[derive(Serialize)]
#[serde(tag = "completeness", rename_all = "camelCase")]
pub enum DeclarationCallersRow {
    #[serde(rename = "complete", rename_all = "camelCase")]
    Complete {
        declaration: String,
        calls: Vec<String>,
        call_sites: Vec<DirectCallInfoPayload>,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "incomplete", rename_all = "camelCase")]
    Incomplete {
        declaration: String,
        calls: Vec<String>,
        call_sites: Vec<DirectCallInfoPayload>,
        evidence: Vec<EvidenceItem>,
        gaps: Vec<GapItem>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallTargetInfoPayload {
    pub declaration: String,
    pub name: Option<String>,
    pub location: Option<SourceLocation>,
    pub is_external: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation: Option<ResolvedImplTargetPayload>,
    pub parameter_bindings: Vec<CallSiteParameterBindingPayload>,
    pub return_expressions: Vec<String>,
}

#[derive(Serialize)]
#[serde(tag = "completeness", rename_all = "camelCase")]
pub enum CallTargetsRow {
    #[serde(rename = "complete", rename_all = "camelCase")]
    Complete {
        call: String,
        targets: Vec<String>,
        resolved_targets: Vec<CallTargetInfoPayload>,
        return_target: String,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "incomplete", rename_all = "camelCase")]
    Incomplete {
        call: String,
        targets: Vec<String>,
        resolved_targets: Vec<CallTargetInfoPayload>,
        return_target: String,
        evidence: Vec<EvidenceItem>,
        gaps: Vec<GapItem>,
    },
}

pub struct SnapshotProjectFileCatalog {
    pub sf_by_file_id: FxHashMap<FileId, P<SourceFile>>,
    pub rel_paths_by_file_id: FxHashMap<FileId, String>,
    pub project_files: Vec<FileId>,
}

fn build_project_file_catalog(
    cwd: &str,
    program: &'static Program,
    workspace_programs: &[&'static Program],
    file_ids: &Arc<std::sync::RwLock<tssae_tsrs_boundary::FileIdRegistry>>,
) -> SnapshotProjectFileCatalog {
    let mut sf_by_file_id = FxHashMap::default();
    let mut rel_paths_by_file_id = FxHashMap::default();
    let mut project_files = Vec::new();
    let mut seen_rel = FxHashSet::default();

    let mut reg = file_ids.write().ok();
    let mut register_sf = |sf: P<SourceFile>, is_proj: bool| {
        let rel = normalize_path(&relative_to_cwd(cwd, sf.file_name()));
        let Some(ref mut reg) = reg else {
            return;
        };
        let fid = reg.intern(&rel);
        sf_by_file_id.entry(fid).or_insert(sf);
        rel_paths_by_file_id
            .entry(fid)
            .or_insert_with(|| rel.clone());
        if is_proj && seen_rel.insert(rel) {
            project_files.push(fid);
        }
    };

    for &sf in program.get_source_files() {
        if !program.is_source_file_default_library(sf.path()) {
            register_sf(sf, is_project_source_file(program, sf));
        }
    }
    for &wp in workspace_programs {
        for &sf in wp.get_source_files() {
            if !wp.is_source_file_default_library(sf.path()) {
                register_sf(sf, false);
            }
        }
    }
    drop(reg);
    project_files.sort_by(|a, b| {
        rel_paths_by_file_id
            .get(a)
            .cmp(&rel_paths_by_file_id.get(b))
    });

    SnapshotProjectFileCatalog {
        sf_by_file_id,
        rel_paths_by_file_id,
        project_files,
    }
}

pub struct SnapshotIrEnv<'a> {
    pub semantic: SemanticModelBridge<'a>,
    ir_cache: &'a tssae_analysis::ir::SnapshotIrCache,
    catalog: Arc<SnapshotProjectFileCatalog>,
    project_id: String,
    hierarchy_cache: &'a mut FxHashMap<String, Arc<tssae_tsrs_boundary::ProgramHierarchyIndex>>,
    ir_call_graph_cache: &'a mut FxHashMap<String, Arc<IrCallGraph>>,
    ir_call_graph_shards: &'a mut FxHashMap<(String, FileId), Arc<IrCallGraph>>,
    ir_field_write_cache: &'a mut FxHashMap<String, Arc<IrFieldWriteIndex>>,
    ir_field_write_shards: &'a mut FxHashMap<(String, FileId), Arc<IrFieldWriteIndex>>,
    pub summary_stats: Cell<SummaryLookupStats>,
}

impl Drop for SnapshotIrEnv<'_> {
    fn drop(&mut self) {
        if let Some(h) = self.semantic.cached_hierarchy() {
            self.hierarchy_cache
                .entry(self.project_id.clone())
                .or_insert(h);
        }
    }
}

impl<'a> SnapshotIrEnv<'a> {
    pub fn from_snapshot(
        snapshot: &'a mut SnapshotState,
        project_id: &str,
        program: &'static Program,
        workspace_programs: &'a [&'static Program],
        checker: &'a mut CheckerHandle,
    ) -> Self {
        let file_ids = Arc::clone(&snapshot.file_ids);
        let catalog = if let Some(cached) = snapshot.project_file_catalog_cache.get(project_id) {
            Arc::clone(cached)
        } else {
            let built = Arc::new(build_project_file_catalog(
                &snapshot.cwd,
                program,
                workspace_programs,
                &file_ids,
            ));
            snapshot
                .project_file_catalog_cache
                .insert(project_id.to_string(), Arc::clone(&built));
            built
        };
        let cached_hierarchy = snapshot.hierarchy_cache.get(project_id).cloned();

        let semantic = SemanticModelBridge::with_snapshot_store(
            &snapshot.cwd,
            program,
            workspace_programs,
            checker,
            file_ids,
            &mut snapshot.file_syntax_indices,
            &mut snapshot.symbol_registry,
            &snapshot.sourcemap_cache,
        );
        if let Some(h) = cached_hierarchy {
            semantic.set_cached_hierarchy(h);
        }

        Self {
            semantic,
            ir_cache: &snapshot.ir_cache,
            catalog,
            project_id: project_id.to_string(),
            hierarchy_cache: &mut snapshot.hierarchy_cache,
            ir_call_graph_cache: &mut snapshot.ir_call_graph_cache,
            ir_call_graph_shards: &mut snapshot.ir_call_graph_shards,
            ir_field_write_cache: &mut snapshot.ir_field_write_cache,
            ir_field_write_shards: &mut snapshot.ir_field_write_shards,
            summary_stats: Cell::new(SummaryLookupStats::default()),
        }
    }

    #[must_use]
    pub fn project_files(&self) -> &[FileId] {
        &self.catalog.project_files
    }

    pub fn ensure_call_graph(
        &mut self,
        summary_index: &BehaviorSummaryIndex,
        target_files: &[FileId],
        is_full_project: bool,
    ) -> Arc<IrCallGraph> {
        if let Some(cached) = self.ir_call_graph_cache.get(&self.project_id) {
            return Arc::clone(cached);
        }

        let mut missing = Vec::new();
        let mut seen_missing = FxHashSet::default();
        for &fid in target_files {
            if !self
                .ir_call_graph_shards
                .contains_key(&(self.project_id.clone(), fid))
                && seen_missing.insert(fid)
            {
                missing.push(fid);
            }
        }

        let mut newly_built = Vec::with_capacity(missing.len());
        for fid in missing {
            let shard = Arc::new(build_ir_call_graph_file_shard(
                fid,
                self,
                self,
                self,
                summary_index,
            ));
            newly_built.push((fid, shard));
        }

        for (fid, shard) in newly_built {
            self.ir_call_graph_shards
                .insert((self.project_id.clone(), fid), shard);
        }

        let shard_refs: Vec<&Arc<IrCallGraph>> = target_files
            .iter()
            .filter_map(|&fid| {
                self.ir_call_graph_shards
                    .get(&(self.project_id.clone(), fid))
            })
            .collect();
        let merged = Arc::new(merge_ir_call_graph_shards(shard_refs));
        if is_full_project && merged.conflict.is_none() {
            self.ir_call_graph_cache
                .insert(self.project_id.clone(), Arc::clone(&merged));
        }
        merged
    }

    pub fn ensure_field_write_index(
        &mut self,
        target_files: &[FileId],
        is_full_project: bool,
    ) -> Arc<IrFieldWriteIndex> {
        if let Some(cached) = self.ir_field_write_cache.get(&self.project_id) {
            return Arc::clone(cached);
        }

        let mut missing = Vec::new();
        let mut seen_missing = FxHashSet::default();
        for &fid in target_files {
            if !self
                .ir_field_write_shards
                .contains_key(&(self.project_id.clone(), fid))
                && seen_missing.insert(fid)
            {
                missing.push(fid);
            }
        }

        let mut newly_built = Vec::with_capacity(missing.len());
        for fid in missing {
            let shard = Arc::new(build_ir_field_write_file_shard(fid, self, self, self));
            newly_built.push((fid, shard));
        }

        for (fid, shard) in newly_built {
            self.ir_field_write_shards
                .insert((self.project_id.clone(), fid), shard);
        }

        let shard_refs: Vec<&Arc<IrFieldWriteIndex>> = target_files
            .iter()
            .filter_map(|&fid| {
                self.ir_field_write_shards
                    .get(&(self.project_id.clone(), fid))
            })
            .collect();
        let merged = Arc::new(merge_ir_field_write_shards(shard_refs));
        if is_full_project {
            self.ir_field_write_cache
                .insert(self.project_id.clone(), Arc::clone(&merged));
        }
        merged
    }

    fn ensure_file_ir_entry(&self, file: FileId) -> Option<Arc<tssae_analysis::ir::FileIrEntry>> {
        if let Some(existing) = self.ir_cache.get_file(file) {
            return Some(existing);
        }
        let &sf = self.catalog.sf_by_file_id.get(&file)?;
        let (fid, file_index) = self.semantic.ensure_source_file_index(sf)?;
        let rel_file = self
            .catalog
            .rel_paths_by_file_id
            .get(&fid)
            .cloned()
            .unwrap_or_else(|| file_index.rel_file.clone());
        let sf_addr = std::ptr::from_ref(&*sf) as usize;
        Some(self.ir_cache.get_or_insert_file(fid, || {
            tssae_analysis::ir::FileIrEntry::new(
                fid,
                rel_file,
                sf_addr,
                file_index.scope_catalog.clone(),
            )
        }))
    }
}

impl SyntaxCatalog for SnapshotIrEnv<'_> {
    fn syntax_location(&self, syntax: SyntaxId) -> Option<DiagnosticLocation> {
        self.semantic.syntax_location(syntax)
    }

    fn syntax_text(&self, syntax: SyntaxId) -> Option<String> {
        self.semantic.syntax_text(syntax)
    }

    fn syntax_kind(&self, syntax: SyntaxId) -> Option<String> {
        self.semantic.syntax_kind(syntax)
    }

    fn file_path(&self, file: FileId) -> Option<&str> {
        self.catalog
            .rel_paths_by_file_id
            .get(&file)
            .map(String::as_str)
    }
}

impl SemanticModel for SnapshotIrEnv<'_> {
    fn symbol_at_syntax(&self, syntax: SyntaxId) -> Option<SymbolId> {
        self.semantic.symbol_at_syntax(syntax)
    }

    fn type_at_syntax(&self, syntax: SyntaxId) -> Option<TypeSummaryData> {
        self.semantic.type_at_syntax(syntax)
    }

    fn signature_at_syntax(&self, syntax: SyntaxId) -> Option<SignatureId> {
        self.semantic.signature_at_syntax(syntax)
    }

    fn resolve_declaration(&self, syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        self.semantic.resolve_declaration(syntax)
    }

    fn resolve_declaration_by_symbol(&self, symbol: SymbolId) -> Option<DeclarationSummaryData> {
        self.semantic.resolve_declaration_by_symbol(symbol)
    }

    fn is_assignable_to(&self, source: TypeId, target: TypeId) -> bool {
        self.semantic.is_assignable_to(source, target)
    }

    fn base_types(&self, ty: TypeId) -> Vec<TypeId> {
        self.semantic.base_types(ty)
    }

    fn is_external_syntax(&self, syntax: SyntaxId) -> bool {
        self.semantic.is_external_syntax(syntax)
    }

    fn member_target_hierarchy(
        &self,
        receiver_syntax: SyntaxId,
        member_name: Option<&str>,
        static_receiver: bool,
    ) -> Option<MemberTargetHierarchy> {
        self.semantic
            .member_target_hierarchy(receiver_syntax, member_name, static_receiver)
    }

    fn class_hierarchy_for_symbol(&self, class_symbol: SymbolId) -> Option<MemberTargetHierarchy> {
        self.semantic.class_hierarchy_for_symbol(class_symbol)
    }

    fn resolve_class_constructor(&self, class_syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        self.semantic.resolve_class_constructor(class_syntax)
    }

    fn resolve_super_target(&self, call_syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        self.semantic.resolve_super_target(call_syntax)
    }

    fn resolve_implementation_for_syntax(
        &self,
        decl_or_node_syntax: SyntaxId,
        call_site_syntax: Option<SyntaxId>,
    ) -> ImplementationFact {
        self.semantic
            .resolve_implementation_for_syntax(decl_or_node_syntax, call_site_syntax)
    }

    fn check_call_missing_package_gap(&self, call_syntax: SyntaxId) -> Option<GapItem> {
        self.semantic.check_call_missing_package_gap(call_syntax)
    }

    fn declaration_identity(&self, syntax: SyntaxId) -> Option<DeclarationIdentity> {
        self.semantic.declaration_identity(syntax)
    }

    fn syntax_scope_info(&self, syntax: SyntaxId) -> Option<SyntaxScopeInfo> {
        self.semantic.syntax_scope_info(syntax)
    }

    fn branch_statement_syntax(&self, condition_syntax: SyntaxId) -> Option<SyntaxId> {
        self.semantic.branch_statement_syntax(condition_syntax)
    }

    fn write_syntax_info(&self, write_syntax: SyntaxId) -> Option<WriteSyntaxInfo> {
        self.semantic.write_syntax_info(write_syntax)
    }

    fn record_summary_lookup(&self, source: Option<SummarySource>) {
        let mut stats = self.summary_stats.get();
        stats.record(source);
        self.summary_stats.set(stats);
    }
}

impl IrProvider for SnapshotIrEnv<'_> {
    fn function_ir(&self, scope_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
        let scope_node = self.semantic.node_for_syntax(scope_syntax).ok_or_else(|| {
            GapItem::from_reason(
                UnknownReason::MissingSourceFile,
                "Scope syntax node is not indexed in snapshot.".to_string(),
                None,
            )
        })?;
        let entry = self
            .ensure_file_ir_entry(scope_syntax.file_id())
            .ok_or_else(|| {
                GapItem::from_reason(
                    UnknownReason::MissingSourceFile,
                    "Source file for scope is unavailable.".to_string(),
                    None,
                )
            })?;
        let (res, _) = entry.get_or_init_scope(scope_syntax.local_id(), |local_scope| {
            self.semantic.lower_scope_to_ir(scope_node, local_scope)
        });
        res
    }

    fn enclosing_scope_ir(&self, node_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
        let node = self.semantic.node_for_syntax(node_syntax).ok_or_else(|| {
            GapItem::from_reason(
                UnknownReason::MissingSourceFile,
                "Node syntax handle is not indexed in snapshot.".to_string(),
                None,
            )
        })?;
        let scope_node = resolve_enclosing_cfg_owner(node).ok_or_else(|| {
            GapItem::from_reason(
                UnknownReason::NoEnclosingFunction,
                "Node has no enclosing function or module scope.".to_string(),
                None,
            )
        })?;
        let scope_syn = self.semantic.intern_syntax(scope_node).ok_or_else(|| {
            GapItem::from_reason(
                UnknownReason::InvalidCfgScope,
                "Enclosing scope node could not be interned.".to_string(),
                None,
            )
        })?;
        self.function_ir(scope_syn)
    }

    fn file_scope_ir(&self, file: FileId) -> Option<Arc<FunctionIr>> {
        let entry = self.ensure_file_ir_entry(file)?;
        let root_syn = entry
            .scope_catalog
            .iter()
            .find(|s| s.kind == IrScopeKind::ModuleTopLevel)?
            .root_syntax;
        self.function_ir(root_syn).ok()
    }

    fn scopes_in_file(&self, file: FileId) -> Vec<ScopeCatalogEntry> {
        self.ensure_file_ir_entry(file)
            .map_or_default(|entry| entry.scope_catalog.clone())
    }
}

struct MaterializedBinding {
    argument_index: usize,
    argument_node: P<Node>,
    parameter_name: Option<String>,
    parameter_decl_node: Option<(P<Node>, Option<P<Symbol>>)>,
    is_rest: bool,
}

struct MaterializedImplTarget {
    file: String,
    location: DiagnosticLocation,
    decl_node: P<Node>,
    decl_symbol: Option<P<Symbol>>,
    export_name: Option<String>,
    package_name: Option<String>,
    resolution_source: FirstPartyResolutionSource,
    declaration_file: Option<String>,
}

struct MaterializedTarget {
    decl_node: P<Node>,
    decl_symbol: Option<P<Symbol>>,
    name: Option<String>,
    location: Option<DiagnosticLocation>,
    is_external: bool,
    implementation: Option<MaterializedImplTarget>,
    parameter_bindings: Vec<MaterializedBinding>,
    return_expr_nodes: Vec<P<Node>>,
}

struct MaterializedCalleesRow {
    call_id: String,
    call_node: P<Node>,
    targets: Vec<MaterializedTarget>,
    evidence: Vec<EvidenceItem>,
    gaps: Vec<GapItem>,
    is_complete: bool,
}

struct MaterializedCallerSite {
    call_node: P<Node>,
    location: DiagnosticLocation,
    caller_decl: Option<(P<Node>, Option<P<Symbol>>)>,
    parameter_bindings: Vec<MaterializedBinding>,
}

struct MaterializedCallersRow {
    decl_id: String,
    call_sites: Vec<MaterializedCallerSite>,
    evidence: Vec<EvidenceItem>,
    gaps: Vec<GapItem>,
    is_complete: bool,
}

fn materialize_bindings(
    bindings: Vec<CallParameterBindingData>,
    env: &SnapshotIrEnv<'_>,
) -> Vec<MaterializedBinding> {
    bindings
        .into_iter()
        .filter_map(|b| {
            let arg_node = env.semantic.node_for_syntax(b.argument_syntax)?;
            let param_decl_node = b.parameter_decl_syntax.and_then(|ps| {
                let pnode = env.semantic.node_for_syntax(ps)?;
                let psym = env
                    .semantic
                    .symbol_at_syntax(ps)
                    .and_then(|sid| env.semantic.symbol_for_id(sid));
                Some((pnode, psym))
            });
            Some(MaterializedBinding {
                argument_index: b.argument_index,
                argument_node: arg_node,
                parameter_name: b.parameter_name,
                parameter_decl_node: param_decl_node,
                is_rest: b.is_rest,
            })
        })
        .collect()
}

fn materialize_target(
    t: ResolvedCallTargetData,
    env: &SnapshotIrEnv<'_>,
) -> Option<MaterializedTarget> {
    let decl_node = env.semantic.node_for_syntax(t.declaration_syntax)?;
    let decl_symbol = t.symbol.and_then(|s| env.semantic.symbol_for_id(s));
    let implementation = t.implementation.and_then(|imp| {
        let imp_decl_node = env.semantic.node_for_syntax(imp.declaration_syntax)?;
        let imp_decl_symbol = imp
            .declaration_symbol
            .and_then(|s| env.semantic.symbol_for_id(s));
        let source = match imp.resolution_source {
            ImplementationSourceKind::CurrentProject => FirstPartyResolutionSource::CurrentProject,
            ImplementationSourceKind::ProjectReference => {
                FirstPartyResolutionSource::ProjectReference
            }
            ImplementationSourceKind::CustomConditions => {
                FirstPartyResolutionSource::CustomConditions
            }
            ImplementationSourceKind::DeclarationMap => FirstPartyResolutionSource::DeclarationMap,
        };
        Some(MaterializedImplTarget {
            file: imp.file,
            location: imp.location,
            decl_node: imp_decl_node,
            decl_symbol: imp_decl_symbol,
            export_name: imp.export_name,
            package_name: imp.package_name,
            resolution_source: source,
            declaration_file: imp.declaration_file,
        })
    });
    let parameter_bindings = materialize_bindings(t.parameter_bindings, env);
    let return_expr_nodes = t
        .return_expressions
        .into_iter()
        .filter_map(|rs| env.semantic.node_for_syntax(rs))
        .collect();
    Some(MaterializedTarget {
        decl_node,
        decl_symbol,
        name: t.name,
        location: t.location,
        is_external: t.is_external,
        implementation,
        parameter_bindings,
        return_expr_nodes,
    })
}

fn materialize_callers_site(
    s: CallerSiteData,
    env: &SnapshotIrEnv<'_>,
) -> Option<MaterializedCallerSite> {
    let call_node = env.semantic.node_for_syntax(s.call_syntax)?;
    let caller_decl = s.caller_decl_syntax.and_then(|cd| {
        let cnode = env.semantic.node_for_syntax(cd)?;
        let csym = s
            .caller_symbol
            .and_then(|sid| env.semantic.symbol_for_id(sid));
        Some((cnode, csym))
    });
    let parameter_bindings = materialize_bindings(s.parameter_bindings, env);
    Some(MaterializedCallerSite {
        call_node,
        location: s.location,
        caller_decl,
        parameter_bindings,
    })
}

fn alloc_materialized_bindings(
    snapshot: &mut SnapshotState,
    project_id: &str,
    bindings: Vec<MaterializedBinding>,
) -> Vec<CallSiteParameterBindingPayload> {
    bindings
        .into_iter()
        .map(|b| CallSiteParameterBindingPayload {
            argument_index: b.argument_index,
            argument: snapshot.alloc_syntax(project_id, b.argument_node),
            parameter_name: b.parameter_name,
            parameter_declaration: b
                .parameter_decl_node
                .map(|(n, sym)| snapshot.alloc_declaration(project_id, n, sym)),
            is_rest: b.is_rest,
        })
        .collect()
}

pub fn run_callees_of(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
    observer: &mut ObserveCollector,
) -> Result<Vec<CallTargetsRow>, RpcErr> {
    let mut call_nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_call_node(project_id, handle_id)?;
        call_nodes.push((handle_id.clone(), node));
    }

    let workspace_programs = snapshot.workspace_programs();
    let summary_index = Arc::clone(&snapshot.summary_index);
    let materialized_rows = collect_materialized_callees(
        snapshot,
        project_id,
        program,
        &workspace_programs,
        checker,
        &summary_index,
        call_nodes,
        observer,
    )?;

    let mut rows = Vec::with_capacity(materialized_rows.len());
    for row in materialized_rows {
        let return_target = snapshot.alloc_syntax(project_id, row.call_node);
        let mut target_refs = Vec::with_capacity(row.targets.len());
        let mut resolved_targets = Vec::with_capacity(row.targets.len());

        for t in row.targets {
            let decl_ref = snapshot.alloc_declaration(project_id, t.decl_node, t.decl_symbol);
            let impl_payload = t.implementation.map(|imp| ResolvedImplTargetPayload {
                file: imp.file,
                location: imp.location,
                declaration: snapshot.alloc_declaration(project_id, imp.decl_node, imp.decl_symbol),
                export_name: imp.export_name,
                package_name: imp.package_name,
                resolution_source: imp.resolution_source,
                declaration_file: imp.declaration_file,
            });
            let parameter_bindings =
                alloc_materialized_bindings(snapshot, project_id, t.parameter_bindings);
            let return_expressions = t
                .return_expr_nodes
                .into_iter()
                .map(|expr| snapshot.alloc_syntax(project_id, expr))
                .collect();
            target_refs.push(decl_ref.clone());
            resolved_targets.push(CallTargetInfoPayload {
                declaration: decl_ref,
                name: t.name,
                location: t.location,
                is_external: t.is_external,
                implementation: impl_payload,
                parameter_bindings,
                return_expressions,
            });
        }

        if row.is_complete {
            rows.push(CallTargetsRow::Complete {
                call: row.call_id,
                targets: target_refs,
                resolved_targets,
                return_target,
                evidence: row.evidence,
            });
        } else {
            rows.push(CallTargetsRow::Incomplete {
                call: row.call_id,
                targets: target_refs,
                resolved_targets,
                return_target,
                evidence: row.evidence,
                gaps: row.gaps,
            });
        }
    }

    Ok(rows)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Coordinates snapshot IR env setup, summary lookup metrics, and callee materialization."
)]
fn collect_materialized_callees(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    workspace_programs: &[&'static Program],
    checker: &mut CheckerHandle,
    summary_index: &BehaviorSummaryIndex,
    call_nodes: Vec<(String, P<Node>)>,
    observer: &mut ObserveCollector,
) -> Result<Vec<MaterializedCalleesRow>, RpcErr> {
    let env =
        SnapshotIrEnv::from_snapshot(snapshot, project_id, program, workspace_programs, checker);
    let mut out = Vec::with_capacity(call_nodes.len());
    for (call_id, call_node) in call_nodes {
        let outcome: CallTargetsOutcome = env.semantic.intern_syntax(call_node).map_or_else(
            || CallTargetsOutcome {
                targets: Vec::new(),
                gaps: vec![GapItem::from_reason(
                    UnknownReason::MissingSourceFile,
                    "Call node has no indexed source file.".to_string(),
                    None,
                )],
                is_complete: false,
                evidence: Vec::new(),
                conflict: None,
            },
            |call_syn| {
                resolve_call_targets_with_summaries(call_syn, &env, &env, &env, summary_index)
            },
        );
        if let Some(conflict) = outcome.conflict {
            let stats = env.summary_stats.get();
            observer.record_summary_lookup_stats(&stats);
            drop(env);
            let details = serde_json::to_value(&conflict).unwrap_or(serde_json::Value::Null);
            return Err(RpcErr::with_details(
                "SUMMARY_CONFLICT",
                conflict.format_message(),
                details,
            ));
        }
        let mut mtargets: Vec<MaterializedTarget> = outcome
            .targets
            .into_iter()
            .filter_map(|t| materialize_target(t, &env))
            .collect();
        mtargets.sort_by(|a, b| {
            let a_key = a
                .location
                .as_ref()
                .map(|l| (l.file.clone(), l.start, l.end));
            let b_key = b
                .location
                .as_ref()
                .map(|l| (l.file.clone(), l.start, l.end));
            a_key.cmp(&b_key).then_with(|| a.name.cmp(&b.name))
        });
        out.push(MaterializedCalleesRow {
            call_id,
            call_node,
            targets: mtargets,
            evidence: outcome.evidence,
            gaps: outcome.gaps,
            is_complete: outcome.is_complete,
        });
    }
    let stats = env.summary_stats.get();
    observer.record_summary_lookup_stats(&stats);
    drop(env);
    Ok(out)
}

pub fn run_callers_of(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
    lookup_files: Option<&[String]>,
    observer: &mut ObserveCollector,
) -> Result<Vec<DeclarationCallersRow>, RpcErr> {
    let mut decl_inputs = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let entry = snapshot.get_entry(project_id, handle_id)?;
        let HandlePayload::Declaration { node, symbol } = entry.payload else {
            return Err(RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {handle_id} is not a declaration reference."),
            ));
        };
        decl_inputs.push((handle_id.clone(), node, symbol));
    }

    let workspace_programs = snapshot.workspace_programs();
    let summary_index = Arc::clone(&snapshot.summary_index);
    let file_ids = Arc::clone(&snapshot.file_ids);
    let allowed_files: Option<FxHashSet<FileId>> = lookup_files.map(|files| {
        let mut set = FxHashSet::default();
        if let Ok(mut reg) = file_ids.write() {
            for f in files {
                set.insert(reg.intern(&normalize_path(f)));
            }
        }
        set
    });

    let materialized_rows = collect_materialized_callers(
        snapshot,
        project_id,
        program,
        &workspace_programs,
        checker,
        &summary_index,
        decl_inputs,
        allowed_files.as_ref(),
        observer,
    )?;

    let mut rows = Vec::with_capacity(materialized_rows.len());
    for row in materialized_rows {
        let mut calls = Vec::with_capacity(row.call_sites.len());
        let mut call_sites = Vec::with_capacity(row.call_sites.len());

        for site in row.call_sites {
            let call_ref = snapshot.alloc_call(project_id, site.call_node);
            let syntax_ref = snapshot.alloc_syntax(project_id, site.call_node);
            let caller_declaration = site
                .caller_decl
                .map(|(fn_node, sym)| snapshot.alloc_declaration(project_id, fn_node, sym));
            let parameter_bindings =
                alloc_materialized_bindings(snapshot, project_id, site.parameter_bindings);

            calls.push(call_ref.clone());
            call_sites.push(DirectCallInfoPayload {
                call: call_ref,
                syntax: syntax_ref.clone(),
                location: site.location,
                caller_declaration,
                parameter_bindings,
                return_target: syntax_ref,
            });
        }

        if row.is_complete {
            rows.push(DeclarationCallersRow::Complete {
                declaration: row.decl_id,
                calls,
                call_sites,
                evidence: row.evidence,
            });
        } else {
            rows.push(DeclarationCallersRow::Incomplete {
                declaration: row.decl_id,
                calls,
                call_sites,
                evidence: row.evidence,
                gaps: row.gaps,
            });
        }
    }

    Ok(rows)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Coordinates snapshot IR call graph caching, summary lookup metrics, and caller filtering."
)]
fn collect_materialized_callers(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    workspace_programs: &[&'static Program],
    checker: &mut CheckerHandle,
    summary_index: &BehaviorSummaryIndex,
    decl_inputs: Vec<(String, P<Node>, Option<P<Symbol>>)>,
    allowed_files: Option<&FxHashSet<FileId>>,
    observer: &mut ObserveCollector,
) -> Result<Vec<MaterializedCallersRow>, RpcErr> {
    let mut env =
        SnapshotIrEnv::from_snapshot(snapshot, project_id, program, workspace_programs, checker);
    let is_full_project = allowed_files.is_none();
    let target_files: Vec<FileId> = allowed_files.map_or_else(
        || env.project_files().to_vec(),
        |allowed| {
            env.project_files()
                .iter()
                .copied()
                .filter(|f| allowed.contains(f))
                .collect()
        },
    );
    let graph_arc = env.ensure_call_graph(summary_index, &target_files, is_full_project);
    if let Some(conflict) = &graph_arc.conflict {
        let stats = env.summary_stats.get();
        observer.record_summary_lookup_stats(&stats);
        let msg = conflict.format_message();
        let details = serde_json::to_value(conflict).unwrap_or(serde_json::Value::Null);
        drop(env);
        return Err(RpcErr::with_details("SUMMARY_CONFLICT", msg, details));
    }

    let mut out = Vec::with_capacity(decl_inputs.len());
    for (decl_id, decl_node, sym_opt) in decl_inputs {
        let decl_syn = env.semantic.intern_syntax(decl_node);
        let sym_id = sym_opt
            .map(|s| env.semantic.intern_symbol(s))
            .or_else(|| decl_syn.and_then(|ds| env.symbol_at_syntax(ds)));

        let outcome: CallersOutcome = decl_syn.map_or_else(
            || CallersOutcome {
                call_sites: Vec::new(),
                gaps: vec![GapItem::from_reason(
                    UnknownReason::UnresolvedDeclaration,
                    "Target declaration could not be mapped into snapshot syntax.".to_string(),
                    None,
                )],
                is_complete: false,
                evidence: Vec::new(),
            },
            |ds| find_callers_on_ir(ds, sym_id, &graph_arc, &env, &env, &env, allowed_files),
        );

        let mut sites: Vec<MaterializedCallerSite> = outcome
            .call_sites
            .into_iter()
            .filter_map(|s| materialize_callers_site(s, &env))
            .collect();
        sites.sort_by(|a, b| {
            a.location
                .file
                .cmp(&b.location.file)
                .then_with(|| a.location.start.cmp(&b.location.start))
                .then_with(|| a.location.end.cmp(&b.location.end))
        });

        out.push(MaterializedCallersRow {
            decl_id,
            call_sites: sites,
            evidence: outcome.evidence,
            gaps: outcome.gaps,
            is_complete: outcome.is_complete,
        });
    }
    let stats = env.summary_stats.get();
    observer.record_summary_lookup_stats(&stats);
    drop(env);
    Ok(out)
}
