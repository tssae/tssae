use std::sync::Arc;

use rustc_hash::FxHashSet;
use serde::Serialize;
use tssae_analysis::{
    branch_coverage::ControlFlowPathStep,
    call_effects::{
        CallEffectsBatchOutcome, CallEffectsOptions, CallEffectsOutcome, CallEffectsTargetSpec,
        CallbackInvocationData, DeclTargetRef, FieldEffectData, GuardConditionData,
        analyze_call_effects_on_ir,
    },
    call_resolution::IrCallGraph,
    instance_cleanup::{
        CleanupScopeSpec, CleanupSiteData, CleanupTargetSpec, ImplicitExceptionCleanupOutcome,
        InstanceCleanupBatchOutcome, TargetCleanupStatusData, evaluate_instance_cleanup_on_ir,
    },
    instance_identity::{InstanceOriginKind, IrFieldWriteIndex, ResolvedInstanceOrigin},
};
use tssae_common::{
    EvidenceItem, FileId, GapItem, LocalSyntaxId, SemanticModel, SummarySource, SyntaxId,
};
use tssae_tsrs_boundary::{
    BoundaryChecker, BoundaryNode, BoundaryProgram, BoundarySymbol, normalize_path,
};

use crate::ir_calls::SnapshotIrEnv;
use crate::ir_instances::InstanceHandleData;
use crate::observe::ObserveCollector;
use crate::serve::{HandlePayload, RpcErr, SnapshotState, SourceLocation};

const DEFAULT_MAX_EFFECT_CALL_DEPTH: usize = 12;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardConditionPayload {
    pub branch_syntax: String,
    pub condition: String,
    pub condition_text: String,
    pub direction: &'static str,
    pub location: SourceLocation,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldEffectPayload {
    pub kind: &'static str,
    pub property_name: String,
    pub operator: Option<&'static str>,
    pub syntax: String,
    pub target_syntax: String,
    pub receiver_syntax: String,
    pub receiver_declaration: Option<String>,
    pub value_syntax: Option<String>,
    pub value_text: Option<String>,
    pub location: SourceLocation,
    pub via_call: Option<String>,
    pub enclosing_function: Option<String>,
    pub enclosing_block: Option<String>,
    pub cfg_scope: Option<String>,
    pub guards: Vec<GuardConditionPayload>,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallbackInvocationPayload {
    pub kind: &'static str,
    pub call: String,
    pub outer_call: String,
    pub syntax: String,
    pub callback_argument: String,
    pub parameter_index: Option<usize>,
    pub target_declaration: Option<String>,
    pub helper_declaration: Option<String>,
    pub resolved_targets: Vec<String>,
    pub enclosing_function: Option<String>,
    pub enclosing_block: Option<String>,
    pub cfg_scope: Option<String>,
    pub location: SourceLocation,
    pub outer_call_location: SourceLocation,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Serialize)]
#[serde(tag = "completeness", rename_all = "camelCase")]
pub enum CallEffectsRow {
    #[serde(rename = "complete", rename_all = "camelCase")]
    Complete {
        target: String,
        field_reads: Vec<FieldEffectPayload>,
        field_writes: Vec<FieldEffectPayload>,
        effects: Vec<FieldEffectPayload>,
        callback_invocations: Vec<CallbackInvocationPayload>,
        calls: Vec<String>,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "incomplete", rename_all = "camelCase")]
    Incomplete {
        target: String,
        field_reads: Vec<FieldEffectPayload>,
        field_writes: Vec<FieldEffectPayload>,
        effects: Vec<FieldEffectPayload>,
        callback_invocations: Vec<CallbackInvocationPayload>,
        calls: Vec<String>,
        evidence: Vec<EvidenceItem>,
        gaps: Vec<GapItem>,
    },
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupSitePayload {
    pub instance: String,
    pub instance_key: String,
    pub allocation_location: Option<SourceLocation>,
    pub operation_kind: String,
    pub syntax: String,
    pub location: SourceLocation,
    pub via_call: Option<String>,
    pub via_call_location: Option<SourceLocation>,
    pub summary_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_source: Option<SummarySource>,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ImplicitExceptionCleanupPayload {
    #[serde(rename = "proved", rename_all = "camelCase")]
    Proved {
        cleanup_sites: Vec<CleanupSitePayload>,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "disproved", rename_all = "camelCase")]
    Disproved {
        may_throw_sites: Vec<SourceLocation>,
        counterexample_path: Vec<ControlFlowPathStep>,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "inconclusive", rename_all = "camelCase")]
    Inconclusive {
        may_throw_sites: Vec<SourceLocation>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TargetInstanceCleanupRow {
    #[serde(rename = "proved", rename_all = "camelCase")]
    Proved {
        instance: String,
        instance_key: String,
        allocation_location: Option<SourceLocation>,
        implicit_exceptions: ImplicitExceptionCleanupPayload,
        cleanup_sites: Vec<CleanupSitePayload>,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "disproved", rename_all = "camelCase")]
    Disproved {
        instance: String,
        instance_key: String,
        allocation_location: Option<SourceLocation>,
        implicit_exceptions: ImplicitExceptionCleanupPayload,
        cleaned_on_any_path: bool,
        cleanup_sites: Vec<CleanupSitePayload>,
        uncleaned_path: Vec<ControlFlowPathStep>,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "inconclusive", rename_all = "camelCase")]
    Inconclusive {
        instance: String,
        instance_key: String,
        allocation_location: Option<SourceLocation>,
        implicit_exceptions: ImplicitExceptionCleanupPayload,
        cleanup_sites: Vec<CleanupSitePayload>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceCleanupScopeRow {
    pub scope: String,
    pub resolved_functions: Vec<String>,
    pub target_results: Vec<TargetInstanceCleanupRow>,
    pub disposed_instances: Vec<CleanupSitePayload>,
    pub registered_instances: Vec<CleanupSitePayload>,
    pub detached_instances: Vec<CleanupSitePayload>,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

type MaterializedDecl = (BoundaryNode, Option<BoundarySymbol>);

struct MaterializedGuard {
    branch_syntax: Option<BoundaryNode>,
    condition: Option<BoundaryNode>,
    condition_text: String,
    direction: &'static str,
    location: SourceLocation,
}

struct MaterializedFieldEffect {
    kind: &'static str,
    property_name: String,
    operator: Option<&'static str>,
    syntax: Option<BoundaryNode>,
    target_syntax: Option<BoundaryNode>,
    receiver_syntax: Option<BoundaryNode>,
    receiver_declaration: Option<MaterializedDecl>,
    value_syntax: Option<BoundaryNode>,
    value_text: Option<String>,
    location: SourceLocation,
    via_call: Option<BoundaryNode>,
    enclosing_function: Option<BoundaryNode>,
    enclosing_block: Option<BoundaryNode>,
    cfg_scope: Option<BoundaryNode>,
    guards: Vec<MaterializedGuard>,
    evidence: Vec<EvidenceItem>,
}

struct MaterializedCallback {
    kind: &'static str,
    call: Option<BoundaryNode>,
    outer_call: Option<BoundaryNode>,
    syntax: Option<BoundaryNode>,
    callback_argument: Option<BoundaryNode>,
    parameter_index: Option<usize>,
    target_declaration: Option<MaterializedDecl>,
    helper_declaration: Option<MaterializedDecl>,
    resolved_targets: Vec<MaterializedDecl>,
    enclosing_function: Option<BoundaryNode>,
    enclosing_block: Option<BoundaryNode>,
    cfg_scope: Option<BoundaryNode>,
    location: SourceLocation,
    outer_call_location: SourceLocation,
    evidence: Vec<EvidenceItem>,
}

enum MaterializedCallEffects {
    Complete {
        target: String,
        reads: Vec<MaterializedFieldEffect>,
        writes: Vec<MaterializedFieldEffect>,
        effects: Vec<MaterializedFieldEffect>,
        callbacks: Vec<MaterializedCallback>,
        calls: Vec<BoundaryNode>,
        evidence: Vec<EvidenceItem>,
    },
    Incomplete {
        target: String,
        reads: Vec<MaterializedFieldEffect>,
        writes: Vec<MaterializedFieldEffect>,
        effects: Vec<MaterializedFieldEffect>,
        callbacks: Vec<MaterializedCallback>,
        calls: Vec<BoundaryNode>,
        evidence: Vec<EvidenceItem>,
        gaps: Vec<GapItem>,
    },
}

struct MaterializedOrigin {
    instance_key: String,
    origin_kind: &'static str,
    allocation_location: Option<SourceLocation>,
    origin_node: Option<BoundaryNode>,
    summary_id: Option<String>,
    summary_source: Option<SummarySource>,
    evidence: Vec<EvidenceItem>,
}

struct MaterializedCleanupSite {
    origin: MaterializedOrigin,
    operation_kind: String,
    syntax: Option<BoundaryNode>,
    location: SourceLocation,
    via_call: Option<BoundaryNode>,
    via_call_location: Option<SourceLocation>,
    summary_id: Option<String>,
    summary_source: Option<SummarySource>,
    evidence: Vec<EvidenceItem>,
}

enum MaterializedImplicitOutcome {
    Proved {
        cleanup_sites: Vec<MaterializedCleanupSite>,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        may_throw_sites: Vec<SourceLocation>,
        counterexample_path: Vec<ControlFlowPathStep>,
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        may_throw_sites: Vec<SourceLocation>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

enum MaterializedTargetStatus {
    Proved {
        cleanup_sites: Vec<MaterializedCleanupSite>,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        cleaned_on_any_path: bool,
        cleanup_sites: Vec<MaterializedCleanupSite>,
        uncleaned_path: Vec<ControlFlowPathStep>,
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        cleanup_sites: Vec<MaterializedCleanupSite>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

struct MaterializedTargetCleanup {
    handle_id: String,
    instance_key: String,
    allocation_location: Option<SourceLocation>,
    status: MaterializedTargetStatus,
    implicit_exceptions: MaterializedImplicitOutcome,
}

struct MaterializedScopeCleanup {
    scope_handle: String,
    resolved_functions: Vec<BoundaryNode>,
    target_results: Vec<MaterializedTargetCleanup>,
    disposed_instances: Vec<MaterializedCleanupSite>,
    registered_instances: Vec<MaterializedCleanupSite>,
    detached_instances: Vec<MaterializedCleanupSite>,
    evidence: Vec<EvidenceItem>,
    gaps: Vec<GapItem>,
}

fn conflict_rpc_err(conflict: &tssae_common::SummaryConflictError) -> RpcErr {
    let details = serde_json::to_value(conflict).unwrap_or(serde_json::Value::Null);
    RpcErr::with_details("SUMMARY_CONFLICT", conflict.format_message(), details)
}

fn resolve_target_node_and_symbol(
    snapshot: &SnapshotState,
    project_id: &str,
    handle_id: &str,
) -> Result<(BoundaryNode, Option<BoundarySymbol>), RpcErr> {
    let entry = snapshot.get_entry(project_id, handle_id)?;
    match entry.payload {
        HandlePayload::Syntax(node) | HandlePayload::Call(node) => Ok((node, None)),
        HandlePayload::Declaration { node, symbol } => Ok((node, symbol)),
        HandlePayload::Signature { .. }
        | HandlePayload::Type { .. }
        | HandlePayload::Branch(_)
        | HandlePayload::Instance(_) => Err(RpcErr::new(
            "INVALID_REFERENCE_KIND",
            format!("Program reference {handle_id} cannot be analyzed for call effects."),
        )),
    }
}

fn materialize_decl(env: &SnapshotIrEnv<'_>, decl: DeclTargetRef) -> Option<MaterializedDecl> {
    let node = env.semantic.node_for_syntax(decl.0)?;
    let sym = decl.1.and_then(|s| env.semantic.symbol_for_id(s));
    Some((node, sym))
}

fn materialize_guard(env: &SnapshotIrEnv<'_>, g: GuardConditionData) -> MaterializedGuard {
    MaterializedGuard {
        branch_syntax: env.semantic.node_for_syntax(g.branch_syntax),
        condition: env.semantic.node_for_syntax(g.condition_syntax),
        condition_text: g.condition_text,
        direction: g.direction,
        location: g.location,
    }
}

fn materialize_field_effect(
    env: &SnapshotIrEnv<'_>,
    eff: FieldEffectData,
) -> MaterializedFieldEffect {
    MaterializedFieldEffect {
        kind: eff.kind,
        property_name: eff.property_name,
        operator: eff.operator,
        syntax: env.semantic.node_for_syntax(eff.syntax),
        target_syntax: env.semantic.node_for_syntax(eff.target_syntax),
        receiver_syntax: env.semantic.node_for_syntax(eff.receiver_syntax),
        receiver_declaration: eff
            .receiver_declaration
            .and_then(|d| materialize_decl(env, d)),
        value_syntax: eff
            .value_syntax
            .and_then(|s| env.semantic.node_for_syntax(s)),
        value_text: eff.value_text,
        location: eff.location,
        via_call: eff.via_call.and_then(|s| env.semantic.node_for_syntax(s)),
        enclosing_function: eff
            .enclosing_function
            .and_then(|s| env.semantic.node_for_syntax(s)),
        enclosing_block: eff
            .enclosing_block
            .and_then(|s| env.semantic.node_for_syntax(s)),
        cfg_scope: env
            .semantic
            .node_for_syntax(eff.cfg_scope.unwrap_or(eff.syntax)),
        guards: eff
            .guards
            .into_iter()
            .map(|g| materialize_guard(env, g))
            .collect(),
        evidence: eff.evidence,
    }
}

fn materialize_callback(
    env: &SnapshotIrEnv<'_>,
    cb: CallbackInvocationData,
) -> MaterializedCallback {
    MaterializedCallback {
        kind: cb.kind,
        call: env.semantic.node_for_syntax(cb.call_syntax),
        outer_call: env.semantic.node_for_syntax(cb.outer_call_syntax),
        syntax: env.semantic.node_for_syntax(cb.syntax),
        callback_argument: env.semantic.node_for_syntax(cb.callback_argument_syntax),
        parameter_index: cb.parameter_index,
        target_declaration: cb.target_declaration.and_then(|d| materialize_decl(env, d)),
        helper_declaration: cb.helper_declaration.and_then(|d| materialize_decl(env, d)),
        resolved_targets: cb
            .resolved_targets
            .into_iter()
            .filter_map(|d| materialize_decl(env, d))
            .collect(),
        enclosing_function: cb
            .enclosing_function
            .and_then(|s| env.semantic.node_for_syntax(s)),
        enclosing_block: cb
            .enclosing_block
            .and_then(|s| env.semantic.node_for_syntax(s)),
        cfg_scope: cb.cfg_scope.and_then(|s| env.semantic.node_for_syntax(s)),
        location: cb.location,
        outer_call_location: cb.outer_call_location,
        evidence: cb.evidence,
    }
}

fn materialize_origin(
    env: &SnapshotIrEnv<'_>,
    origin: ResolvedInstanceOrigin,
) -> MaterializedOrigin {
    MaterializedOrigin {
        instance_key: origin.instance_key,
        origin_kind: origin.origin_kind.as_str(),
        allocation_location: origin.allocation_location,
        origin_node: origin
            .origin_syntax
            .and_then(|s| env.semantic.node_for_syntax(s)),
        summary_id: origin.summary_id,
        summary_source: origin.summary_source,
        evidence: origin.evidence,
    }
}

fn materialize_cleanup_site(
    env: &SnapshotIrEnv<'_>,
    site: CleanupSiteData,
) -> MaterializedCleanupSite {
    MaterializedCleanupSite {
        origin: materialize_origin(env, site.origin),
        operation_kind: site.operation_kind,
        syntax: env.semantic.node_for_syntax(site.site_syntax),
        location: site.location,
        via_call: site.via_call.and_then(|s| env.semantic.node_for_syntax(s)),
        via_call_location: site.via_call_location,
        summary_id: site.summary_id,
        summary_source: site.summary_source,
        evidence: site.evidence,
    }
}

fn alloc_field_effect_payload(
    snapshot: &mut SnapshotState,
    project_id: &str,
    fallback_node: BoundaryNode,
    mat: MaterializedFieldEffect,
) -> FieldEffectPayload {
    let syn_node = mat.syntax.unwrap_or(fallback_node);
    let syntax = snapshot.alloc_syntax(project_id, syn_node);
    let target_syntax = snapshot.alloc_syntax(project_id, mat.target_syntax.unwrap_or(syn_node));
    let receiver_syntax =
        snapshot.alloc_syntax(project_id, mat.receiver_syntax.unwrap_or(syn_node));
    let receiver_declaration = mat
        .receiver_declaration
        .map(|(n, s)| snapshot.alloc_declaration(project_id, n, s));
    let value_syntax = mat
        .value_syntax
        .map(|v| snapshot.alloc_syntax(project_id, v));
    let via_call = mat.via_call.map(|c| snapshot.alloc_call(project_id, c));
    let enclosing_function = mat
        .enclosing_function
        .map(|f| snapshot.alloc_syntax(project_id, f));
    let enclosing_block = mat
        .enclosing_block
        .map(|b| snapshot.alloc_syntax(project_id, b));
    let cfg_scope = mat.cfg_scope.map(|s| snapshot.alloc_syntax(project_id, s));
    let guards = mat
        .guards
        .into_iter()
        .map(|g| GuardConditionPayload {
            branch_syntax: snapshot.alloc_syntax(project_id, g.branch_syntax.unwrap_or(syn_node)),
            condition: snapshot.alloc_syntax(project_id, g.condition.unwrap_or(syn_node)),
            condition_text: g.condition_text,
            direction: g.direction,
            location: g.location,
        })
        .collect();

    FieldEffectPayload {
        kind: mat.kind,
        property_name: mat.property_name,
        operator: mat.operator,
        syntax,
        target_syntax,
        receiver_syntax,
        receiver_declaration,
        value_syntax,
        value_text: mat.value_text,
        location: mat.location,
        via_call,
        enclosing_function,
        enclosing_block,
        cfg_scope,
        guards,
        evidence: mat.evidence,
    }
}

fn alloc_callback_payload(
    snapshot: &mut SnapshotState,
    project_id: &str,
    fallback_node: BoundaryNode,
    cb: MaterializedCallback,
) -> CallbackInvocationPayload {
    let call_node = cb.call.unwrap_or(fallback_node);
    let outer_node = cb.outer_call.unwrap_or(call_node);
    let syn_node = cb.syntax.unwrap_or(call_node);
    let arg_node = cb.callback_argument.unwrap_or(call_node);
    CallbackInvocationPayload {
        kind: cb.kind,
        call: snapshot.alloc_call(project_id, call_node),
        outer_call: snapshot.alloc_call(project_id, outer_node),
        syntax: snapshot.alloc_syntax(project_id, syn_node),
        callback_argument: snapshot.alloc_syntax(project_id, arg_node),
        parameter_index: cb.parameter_index,
        target_declaration: cb
            .target_declaration
            .map(|(n, s)| snapshot.alloc_declaration(project_id, n, s)),
        helper_declaration: cb
            .helper_declaration
            .map(|(n, s)| snapshot.alloc_declaration(project_id, n, s)),
        resolved_targets: cb
            .resolved_targets
            .into_iter()
            .map(|(n, s)| snapshot.alloc_declaration(project_id, n, s))
            .collect(),
        enclosing_function: cb
            .enclosing_function
            .map(|f| snapshot.alloc_syntax(project_id, f)),
        enclosing_block: cb
            .enclosing_block
            .map(|b| snapshot.alloc_syntax(project_id, b)),
        cfg_scope: cb.cfg_scope.map(|s| snapshot.alloc_syntax(project_id, s)),
        location: cb.location,
        outer_call_location: cb.outer_call_location,
        evidence: cb.evidence,
    }
}

fn alloc_cleanup_site_payload(
    snapshot: &mut SnapshotState,
    project_id: &str,
    fallback_node: BoundaryNode,
    site: MaterializedCleanupSite,
) -> CleanupSitePayload {
    let inst_ref = snapshot.alloc_instance(
        project_id,
        InstanceHandleData {
            instance_key: site.origin.instance_key.clone(),
            origin_kind: site.origin.origin_kind,
            allocation_location: site.origin.allocation_location.clone(),
            origin_syntax: site.origin.origin_node,
            summary_id: site.origin.summary_id,
            summary_source: site.origin.summary_source,
            evidence: site.origin.evidence,
        },
    );
    let syn_node = site.syntax.unwrap_or(fallback_node);
    let syntax = snapshot.alloc_syntax(project_id, syn_node);
    let via_call = site.via_call.map(|c| snapshot.alloc_call(project_id, c));
    CleanupSitePayload {
        instance: inst_ref,
        instance_key: site.origin.instance_key,
        allocation_location: site.origin.allocation_location,
        operation_kind: site.operation_kind,
        syntax,
        location: site.location,
        via_call,
        via_call_location: site.via_call_location,
        summary_id: site.summary_id,
        summary_source: site.summary_source,
        evidence: site.evidence,
    }
}

type CallEffectsBatchMaterialized = (
    Vec<(BoundaryNode, MaterializedCallEffects)>,
    Option<tssae_common::SummaryConflictError>,
);

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Executes IR-based callEffects analysis with summary lookup reporting and handle allocation."
)]
pub fn run_call_effects(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
    follow_callbacks: bool,
    include_reads: bool,
    include_writes: bool,
    max_depth: Option<usize>,
    observer: &mut ObserveCollector,
) -> Result<Vec<CallEffectsRow>, RpcErr> {
    let mut target_inputs = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let (node, symbol) = resolve_target_node_and_symbol(snapshot, project_id, handle_id)?;
        target_inputs.push((handle_id.clone(), node, symbol));
    }

    if target_inputs.is_empty() {
        return Ok(Vec::new());
    }

    let workspace_programs = snapshot.workspace_programs();
    let cached_graph = snapshot.ir_call_graph_cache.get(project_id).cloned();
    let cached_field_index = snapshot.ir_field_write_cache.get(project_id).cloned();
    let summary_index = Arc::clone(&snapshot.summary_index);

    let (materialized, conflict_opt): CallEffectsBatchMaterialized = {
        let env = SnapshotIrEnv::from_snapshot(
            snapshot,
            project_id,
            program,
            &workspace_programs,
            checker,
        );
        if let Some(g) = cached_graph.as_deref()
            && let Some(conflict) = &g.conflict
        {
            let stats = env.summary_stats.get();
            observer.record_summary_lookup_stats(&stats);
            let err = conflict_rpc_err(conflict);
            drop(env);
            return Err(err);
        }

        let specs: Vec<CallEffectsTargetSpec> = target_inputs
            .iter()
            .map(|(_, node, symbol)| {
                env.semantic.intern_syntax(*node).map_or_else(
                    || CallEffectsTargetSpec {
                        syntax: SyntaxId::from_parts(FileId(u32::MAX), LocalSyntaxId(u32::MAX)),
                        symbol: None,
                    },
                    |syn| {
                        let sym = symbol
                            .map(|s| env.semantic.intern_symbol(s))
                            .or_else(|| env.symbol_at_syntax(syn));
                        CallEffectsTargetSpec {
                            syntax: syn,
                            symbol: sym,
                        }
                    },
                )
            })
            .collect();

        let opts = CallEffectsOptions {
            follow_callbacks,
            include_reads,
            include_writes,
            max_depth: max_depth.unwrap_or(DEFAULT_MAX_EFFECT_CALL_DEPTH),
        };

        let (batch, _, _): (
            CallEffectsBatchOutcome,
            Option<IrCallGraph>,
            Option<IrFieldWriteIndex>,
        ) = analyze_call_effects_on_ir(
            &specs,
            opts,
            env.project_files(),
            &env,
            &env,
            &env,
            &summary_index,
            cached_graph.as_deref(),
            cached_field_index.as_deref(),
        );
        let stats = env.summary_stats.get();
        observer.record_summary_lookup_stats(&stats);

        let mat_rows = target_inputs
            .into_iter()
            .zip(batch.results)
            .map(|((handle_id, fallback_node, _), outcome)| {
                let mat = match outcome {
                    CallEffectsOutcome::Complete {
                        field_reads,
                        field_writes,
                        effects,
                        callback_invocations,
                        calls,
                        evidence,
                    } => MaterializedCallEffects::Complete {
                        target: handle_id,
                        reads: field_reads
                            .into_iter()
                            .map(|e| materialize_field_effect(&env, e))
                            .collect(),
                        writes: field_writes
                            .into_iter()
                            .map(|e| materialize_field_effect(&env, e))
                            .collect(),
                        effects: effects
                            .into_iter()
                            .map(|e| materialize_field_effect(&env, e))
                            .collect(),
                        callbacks: callback_invocations
                            .into_iter()
                            .map(|c| materialize_callback(&env, c))
                            .collect(),
                        calls: calls
                            .into_iter()
                            .filter_map(|s| env.semantic.node_for_syntax(s))
                            .collect(),
                        evidence,
                    },
                    CallEffectsOutcome::Incomplete {
                        field_reads,
                        field_writes,
                        effects,
                        callback_invocations,
                        calls,
                        evidence,
                        gaps,
                    } => MaterializedCallEffects::Incomplete {
                        target: handle_id,
                        reads: field_reads
                            .into_iter()
                            .map(|e| materialize_field_effect(&env, e))
                            .collect(),
                        writes: field_writes
                            .into_iter()
                            .map(|e| materialize_field_effect(&env, e))
                            .collect(),
                        effects: effects
                            .into_iter()
                            .map(|e| materialize_field_effect(&env, e))
                            .collect(),
                        callbacks: callback_invocations
                            .into_iter()
                            .map(|c| materialize_callback(&env, c))
                            .collect(),
                        calls: calls
                            .into_iter()
                            .filter_map(|s| env.semantic.node_for_syntax(s))
                            .collect(),
                        evidence,
                        gaps,
                    },
                };
                (fallback_node, mat)
            })
            .collect();

        drop(env);
        (mat_rows, batch.conflict)
    };

    if let Some(conflict) = conflict_opt {
        return Err(conflict_rpc_err(&conflict));
    }

    let mut rows = Vec::with_capacity(materialized.len());
    for (fallback_node, mat) in materialized {
        match mat {
            MaterializedCallEffects::Complete {
                target,
                reads,
                writes,
                effects,
                callbacks,
                calls,
                evidence,
            } => {
                rows.push(CallEffectsRow::Complete {
                    target,
                    field_reads: reads
                        .into_iter()
                        .map(|e| alloc_field_effect_payload(snapshot, project_id, fallback_node, e))
                        .collect(),
                    field_writes: writes
                        .into_iter()
                        .map(|e| alloc_field_effect_payload(snapshot, project_id, fallback_node, e))
                        .collect(),
                    effects: effects
                        .into_iter()
                        .map(|e| alloc_field_effect_payload(snapshot, project_id, fallback_node, e))
                        .collect(),
                    callback_invocations: callbacks
                        .into_iter()
                        .map(|c| alloc_callback_payload(snapshot, project_id, fallback_node, c))
                        .collect(),
                    calls: calls
                        .into_iter()
                        .map(|c| snapshot.alloc_call(project_id, c))
                        .collect(),
                    evidence,
                });
            }
            MaterializedCallEffects::Incomplete {
                target,
                reads,
                writes,
                effects,
                callbacks,
                calls,
                evidence,
                gaps,
            } => {
                rows.push(CallEffectsRow::Incomplete {
                    target,
                    field_reads: reads
                        .into_iter()
                        .map(|e| alloc_field_effect_payload(snapshot, project_id, fallback_node, e))
                        .collect(),
                    field_writes: writes
                        .into_iter()
                        .map(|e| alloc_field_effect_payload(snapshot, project_id, fallback_node, e))
                        .collect(),
                    effects: effects
                        .into_iter()
                        .map(|e| alloc_field_effect_payload(snapshot, project_id, fallback_node, e))
                        .collect(),
                    callback_invocations: callbacks
                        .into_iter()
                        .map(|c| alloc_callback_payload(snapshot, project_id, fallback_node, c))
                        .collect(),
                    calls: calls
                        .into_iter()
                        .map(|c| snapshot.alloc_call(project_id, c))
                        .collect(),
                    evidence,
                    gaps,
                });
            }
        }
    }

    Ok(rows)
}

type CleanupBatchMaterialized = (
    Vec<(BoundaryNode, MaterializedScopeCleanup)>,
    Option<tssae_common::SummaryConflictError>,
);

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Executes IR-based dual tri-state instanceCleanup analysis with call graph caching and handle allocation."
)]
pub fn run_instance_cleanup(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    scopes: &[String],
    targets: &[String],
    lookup_files: Option<&[String]>,
    observer: &mut ObserveCollector,
) -> Result<Vec<InstanceCleanupScopeRow>, RpcErr> {
    let mut scope_inputs = Vec::with_capacity(scopes.len());
    for handle_id in scopes {
        let (node, _) = resolve_target_node_and_symbol(snapshot, project_id, handle_id)?;
        scope_inputs.push((handle_id.clone(), node));
    }

    let mut target_inputs = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let data = snapshot.get_instance(project_id, handle_id)?.clone();
        target_inputs.push((handle_id.clone(), data));
    }

    if scope_inputs.is_empty() {
        return Ok(Vec::new());
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

    let (materialized, conflict_opt): CleanupBatchMaterialized = {
        let mut env = SnapshotIrEnv::from_snapshot(
            snapshot,
            project_id,
            program,
            &workspace_programs,
            checker,
        );

        let scope_specs: Vec<CleanupScopeSpec> = scope_inputs
            .iter()
            .map(|(_, node)| CleanupScopeSpec {
                syntax: env.semantic.intern_syntax(*node).unwrap_or_else(|| {
                    SyntaxId::from_parts(FileId(u32::MAX), LocalSyntaxId(u32::MAX))
                }),
            })
            .collect();

        let target_specs: Vec<CleanupTargetSpec> = target_inputs
            .iter()
            .map(|(_, data)| CleanupTargetSpec {
                instance_key: data.instance_key.clone(),
                origin_kind: match data.origin_kind {
                    "summary-allocation" => InstanceOriginKind::SummaryAllocation,
                    "parameter" => InstanceOriginKind::Parameter,
                    _ => InstanceOriginKind::Allocation,
                },
                allocation_location: data.allocation_location.clone(),
                origin_syntax: data
                    .origin_syntax
                    .and_then(|n| env.semantic.intern_syntax(n)),
                summary_id: data.summary_id.clone(),
                summary_source: data.summary_source,
                evidence: data.evidence.clone(),
            })
            .collect();

        let is_full_project = allowed_files.is_none();
        let call_target_files: Vec<FileId> = allowed_files.as_ref().map_or_else(
            || env.project_files().to_vec(),
            |allowed| {
                env.project_files()
                    .iter()
                    .copied()
                    .filter(|f| allowed.contains(f))
                    .collect()
            },
        );
        let cached_graph =
            env.ensure_call_graph(&summary_index, &call_target_files, is_full_project);
        if let Some(conflict) = &cached_graph.conflict {
            let stats = env.summary_stats.get();
            observer.record_summary_lookup_stats(&stats);
            let err = conflict_rpc_err(conflict);
            drop(env);
            return Err(err);
        }

        let extra_field_files: FxHashSet<FileId> = scope_specs
            .iter()
            .map(|s| s.syntax.file_id())
            .chain(
                target_specs
                    .iter()
                    .filter_map(|t| t.origin_syntax.map(SyntaxId::file_id)),
            )
            .collect();
        let field_target_files: Vec<FileId> = allowed_files.as_ref().map_or_else(
            || env.project_files().to_vec(),
            |allowed| {
                env.project_files()
                    .iter()
                    .copied()
                    .filter(|f| allowed.contains(f) || extra_field_files.contains(f))
                    .collect()
            },
        );
        let cached_field_index = env.ensure_field_write_index(&field_target_files, is_full_project);

        let (batch, _, _): (
            InstanceCleanupBatchOutcome,
            Option<IrCallGraph>,
            Option<IrFieldWriteIndex>,
        ) = evaluate_instance_cleanup_on_ir(
            &scope_specs,
            &target_specs,
            env.project_files(),
            allowed_files.as_ref(),
            &env,
            &env,
            &env,
            &summary_index,
            Some(cached_graph.as_ref()),
            Some(cached_field_index.as_ref()),
        );
        let stats = env.summary_stats.get();
        observer.record_summary_lookup_stats(&stats);

        let mat_scopes = scope_inputs
            .into_iter()
            .zip(batch.results)
            .map(|((scope_handle, scope_node), scope_res)| {
                let target_results = target_inputs
                    .iter()
                    .zip(scope_res.target_results)
                    .map(|((target_handle, _), tr)| {
                        let verdict_status = match tr.status {
                            TargetCleanupStatusData::Proved {
                                cleanup_sites,
                                evidence,
                            } => MaterializedTargetStatus::Proved {
                                cleanup_sites: cleanup_sites
                                    .into_iter()
                                    .map(|s| materialize_cleanup_site(&env, s))
                                    .collect(),
                                evidence,
                            },
                            TargetCleanupStatusData::Disproved {
                                cleaned_on_any_path,
                                cleanup_sites,
                                uncleaned_path,
                                evidence,
                            } => MaterializedTargetStatus::Disproved {
                                cleaned_on_any_path,
                                cleanup_sites: cleanup_sites
                                    .into_iter()
                                    .map(|s| materialize_cleanup_site(&env, s))
                                    .collect(),
                                uncleaned_path,
                                evidence,
                            },
                            TargetCleanupStatusData::Inconclusive {
                                cleanup_sites,
                                gaps,
                                evidence,
                            } => MaterializedTargetStatus::Inconclusive {
                                cleanup_sites: cleanup_sites
                                    .into_iter()
                                    .map(|s| materialize_cleanup_site(&env, s))
                                    .collect(),
                                gaps,
                                evidence,
                            },
                        };
                        let implicit_exceptions = match tr.implicit_exceptions {
                            ImplicitExceptionCleanupOutcome::Proved {
                                cleanup_sites,
                                evidence,
                            } => MaterializedImplicitOutcome::Proved {
                                cleanup_sites: cleanup_sites
                                    .into_iter()
                                    .map(|s| materialize_cleanup_site(&env, s))
                                    .collect(),
                                evidence,
                            },
                            ImplicitExceptionCleanupOutcome::Disproved {
                                may_throw_sites,
                                counterexample_path,
                                evidence,
                            } => MaterializedImplicitOutcome::Disproved {
                                may_throw_sites,
                                counterexample_path,
                                evidence,
                            },
                            ImplicitExceptionCleanupOutcome::Inconclusive {
                                may_throw_sites,
                                gaps,
                                evidence,
                            } => MaterializedImplicitOutcome::Inconclusive {
                                may_throw_sites,
                                gaps,
                                evidence,
                            },
                        };
                        MaterializedTargetCleanup {
                            handle_id: target_handle.clone(),
                            instance_key: tr.instance_key,
                            allocation_location: tr.allocation_location,
                            status: verdict_status,
                            implicit_exceptions,
                        }
                    })
                    .collect();

                let mat = MaterializedScopeCleanup {
                    scope_handle,
                    resolved_functions: scope_res
                        .resolved_functions
                        .into_iter()
                        .filter_map(|s| env.semantic.node_for_syntax(s))
                        .collect(),
                    target_results,
                    disposed_instances: scope_res
                        .disposed_instances
                        .into_iter()
                        .map(|s| materialize_cleanup_site(&env, s))
                        .collect(),
                    registered_instances: scope_res
                        .registered_instances
                        .into_iter()
                        .map(|s| materialize_cleanup_site(&env, s))
                        .collect(),
                    detached_instances: scope_res
                        .detached_instances
                        .into_iter()
                        .map(|s| materialize_cleanup_site(&env, s))
                        .collect(),
                    evidence: scope_res.evidence,
                    gaps: scope_res.gaps,
                };
                (scope_node, mat)
            })
            .collect();

        drop(env);
        (mat_scopes, batch.conflict)
    };

    if let Some(conflict) = conflict_opt {
        return Err(conflict_rpc_err(&conflict));
    }

    let mut rows = Vec::with_capacity(materialized.len());
    for (scope_node, mat) in materialized {
        let target_rows = mat
            .target_results
            .into_iter()
            .map(|tr| {
                let implicit_exceptions = match tr.implicit_exceptions {
                    MaterializedImplicitOutcome::Proved {
                        cleanup_sites,
                        evidence,
                    } => ImplicitExceptionCleanupPayload::Proved {
                        cleanup_sites: cleanup_sites
                            .into_iter()
                            .map(|s| {
                                alloc_cleanup_site_payload(snapshot, project_id, scope_node, s)
                            })
                            .collect(),
                        evidence,
                    },
                    MaterializedImplicitOutcome::Disproved {
                        may_throw_sites,
                        counterexample_path,
                        evidence,
                    } => ImplicitExceptionCleanupPayload::Disproved {
                        may_throw_sites,
                        counterexample_path,
                        evidence,
                    },
                    MaterializedImplicitOutcome::Inconclusive {
                        may_throw_sites,
                        gaps,
                        evidence,
                    } => ImplicitExceptionCleanupPayload::Inconclusive {
                        may_throw_sites,
                        gaps,
                        evidence,
                    },
                };
                match tr.status {
                    MaterializedTargetStatus::Proved {
                        cleanup_sites,
                        evidence,
                    } => TargetInstanceCleanupRow::Proved {
                        instance: tr.handle_id,
                        instance_key: tr.instance_key,
                        allocation_location: tr.allocation_location,
                        implicit_exceptions,
                        cleanup_sites: cleanup_sites
                            .into_iter()
                            .map(|s| {
                                alloc_cleanup_site_payload(snapshot, project_id, scope_node, s)
                            })
                            .collect(),
                        evidence,
                    },
                    MaterializedTargetStatus::Disproved {
                        cleaned_on_any_path,
                        cleanup_sites,
                        uncleaned_path,
                        evidence,
                    } => TargetInstanceCleanupRow::Disproved {
                        instance: tr.handle_id,
                        instance_key: tr.instance_key,
                        allocation_location: tr.allocation_location,
                        implicit_exceptions,
                        cleaned_on_any_path,
                        cleanup_sites: cleanup_sites
                            .into_iter()
                            .map(|s| {
                                alloc_cleanup_site_payload(snapshot, project_id, scope_node, s)
                            })
                            .collect(),
                        uncleaned_path,
                        evidence,
                    },
                    MaterializedTargetStatus::Inconclusive {
                        cleanup_sites,
                        gaps,
                        evidence,
                    } => TargetInstanceCleanupRow::Inconclusive {
                        instance: tr.handle_id,
                        instance_key: tr.instance_key,
                        allocation_location: tr.allocation_location,
                        implicit_exceptions,
                        cleanup_sites: cleanup_sites
                            .into_iter()
                            .map(|s| {
                                alloc_cleanup_site_payload(snapshot, project_id, scope_node, s)
                            })
                            .collect(),
                        gaps,
                        evidence,
                    },
                }
            })
            .collect();

        rows.push(InstanceCleanupScopeRow {
            scope: mat.scope_handle,
            resolved_functions: mat
                .resolved_functions
                .into_iter()
                .map(|f| snapshot.alloc_syntax(project_id, f))
                .collect(),
            target_results: target_rows,
            disposed_instances: mat
                .disposed_instances
                .into_iter()
                .map(|s| alloc_cleanup_site_payload(snapshot, project_id, scope_node, s))
                .collect(),
            registered_instances: mat
                .registered_instances
                .into_iter()
                .map(|s| alloc_cleanup_site_payload(snapshot, project_id, scope_node, s))
                .collect(),
            detached_instances: mat
                .detached_instances
                .into_iter()
                .map(|s| alloc_cleanup_site_payload(snapshot, project_id, scope_node, s))
                .collect(),
            evidence: mat.evidence,
            gaps: mat.gaps,
        });
    }

    Ok(rows)
}
