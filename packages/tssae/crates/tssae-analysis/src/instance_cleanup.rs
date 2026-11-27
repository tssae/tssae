use std::{
    collections::{BTreeSet, HashSet, VecDeque},
    hash::BuildHasher,
    sync::Arc,
};

use rustc_hash::{FxHashMap, FxHashSet};
use tssae_common::{
    CallbackTiming, DeclarationIdentity, DiagnosticLocation, EffectAspectSummary, EvidenceItem,
    FileId, FunctionIrId, GapItem, ResourceOpSpec, SemanticModel, SummaryCompleteness,
    SummaryConflictError, SummarySlotRef, SummarySource, SyntaxCatalog, SyntaxId, UnknownReason,
    ValueId, VertexId,
};

use crate::{
    branch_coverage::{ControlFlowPathStep, StackArena, StackId},
    call_resolution::{IrCallGraph, resolve_call_targets_with_summaries},
    instance_identity::{
        HostOrigin, InstanceOriginKind, IrCallFrame, IrFieldWriteIndex, IrInstanceResolver,
        ReceiverBinding, ResolvedInstanceOrigin, find_call_vertex_by_syntax,
        find_value_producer_vertex,
    },
    ir::{
        CfgVertex, ExplicitEdgeKind, FunctionIr, IrCallArg, IrCallKind, IrOp, IrProvider,
        IrScopeKind, MayThrowReason,
    },
    summaries::{BehaviorSummaryIndex, MatchedSummaryAspect},
};

const MAX_CLEANUP_CALL_DEPTH: usize = 12;
const MAX_CONTAINER_BITS: usize = 32;

/// Target instance specification passed into [`evaluate_instance_cleanup_on_ir`].
#[derive(Clone, Debug)]
pub struct CleanupTargetSpec {
    pub instance_key: String,
    pub origin_kind: InstanceOriginKind,
    pub allocation_location: Option<DiagnosticLocation>,
    pub origin_syntax: Option<SyntaxId>,
    pub summary_id: Option<String>,
    pub summary_source: Option<SummarySource>,
    pub evidence: Vec<EvidenceItem>,
}

/// Scope target passed into [`evaluate_instance_cleanup_on_ir`].
#[derive(Clone, Copy, Debug)]
pub struct CleanupScopeSpec {
    pub syntax: SyntaxId,
}

/// Recorded disposal, registration, replacement, or detachment site for an instance.
#[derive(Clone, Debug)]
pub struct CleanupSiteData {
    pub origin: ResolvedInstanceOrigin,
    pub operation_kind: String,
    pub site_syntax: SyntaxId,
    pub location: DiagnosticLocation,
    pub via_call: Option<SyntaxId>,
    pub via_call_location: Option<DiagnosticLocation>,
    pub summary_id: Option<String>,
    pub summary_source: Option<SummarySource>,
    pub evidence: Vec<EvidenceItem>,
}

/// Independent tri-state outcome evaluating implicit `mayThrow` exception cleanup paths.
#[derive(Clone, Debug)]
pub enum ImplicitExceptionCleanupOutcome {
    Proved {
        cleanup_sites: Vec<CleanupSiteData>,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        may_throw_sites: Vec<DiagnosticLocation>,
        counterexample_path: Vec<ControlFlowPathStep>,
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        may_throw_sites: Vec<DiagnosticLocation>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

/// Tri-state explicit-path outcome for a single target instance inside a scope.
#[derive(Clone, Debug)]
pub enum TargetCleanupStatusData {
    Proved {
        cleanup_sites: Vec<CleanupSiteData>,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        cleaned_on_any_path: bool,
        cleanup_sites: Vec<CleanupSiteData>,
        uncleaned_path: Vec<ControlFlowPathStep>,
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        cleanup_sites: Vec<CleanupSiteData>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

/// Combined explicit + implicit-exception cleanup verdict for one target instance.
#[derive(Clone, Debug)]
pub struct TargetInstanceCleanupResultData {
    pub instance_key: String,
    pub allocation_location: Option<DiagnosticLocation>,
    pub status: TargetCleanupStatusData,
    pub implicit_exceptions: ImplicitExceptionCleanupOutcome,
}

/// Scope-level `instanceCleanup` result over IR.
#[derive(Clone, Debug)]
pub struct InstanceCleanupScopeOutcome {
    pub resolved_functions: Vec<SyntaxId>,
    pub target_results: Vec<TargetInstanceCleanupResultData>,
    pub disposed_instances: Vec<CleanupSiteData>,
    pub registered_instances: Vec<CleanupSiteData>,
    pub detached_instances: Vec<CleanupSiteData>,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

/// Batch output returned by [`evaluate_instance_cleanup_on_ir`].
#[derive(Clone, Debug)]
pub struct InstanceCleanupBatchOutcome {
    pub results: Vec<InstanceCleanupScopeOutcome>,
    pub conflict: Option<SummaryConflictError>,
}

#[derive(Clone)]
struct RegistrationEntry {
    scope_ir: Arc<FunctionIr>,
    vertex_id: VertexId,
    location: Option<DiagnosticLocation>,
    container_slot: SummarySlotRef,
    item_slot: SummarySlotRef,
    summary_id: String,
    summary_source: SummarySource,
    label: String,
}

#[derive(Clone)]
struct SlotWriteEntry {
    scope_ir: Arc<FunctionIr>,
    vertex_id: VertexId,
    write_syntax: SyntaxId,
    location: Option<DiagnosticLocation>,
    object_val: ValueId,
    rhs_val: ValueId,
    summary_id: String,
    summary_source: SummarySource,
    label: String,
    file: FileId,
    start: u32,
}

#[derive(Default)]
struct IrCleanupProjectIndex {
    registrations: Vec<RegistrationEntry>,
    slot_writes: Vec<SlotWriteEntry>,
}

#[derive(Clone)]
struct ResolvedEntryFn {
    ir: Arc<FunctionIr>,
    captured_stack: Vec<IrCallFrame>,
}

/// Collected semantic operations occurring at a single vertex inside a CFG.
#[derive(Clone, Default)]
struct VertexCleanupEffect {
    direct_disposals: Vec<(String, CleanupSiteData)>,
    container_disposals: Vec<(String, Vec<IrCallFrame>, CleanupSiteData)>,
    registrations: Vec<(String, String, CleanupSiteData)>,
    detachments: Vec<(String, String, CleanupSiteData)>,
    touched_instances: FxHashSet<String>,
    dynamic_gap: Option<GapItem>,
    unmodeled_external_gap: Option<GapItem>,
    explicit_callees: Vec<CalleeInvocation>,
    summary_sync_callbacks: Vec<CalleeInvocation>,
    /// True when this vertex is an `ImplicitDispose` or direct cleanup call.
    is_direct_cleanup_call: bool,
}

#[derive(Clone)]
struct CalleeInvocation {
    callee_ir: Arc<FunctionIr>,
    call_syntax: SyntaxId,
    call_location: Option<DiagnosticLocation>,
    next_stack: Vec<IrCallFrame>,
}

/// Summary of how a `(fn_ir, call_stack)` behaves with respect to a specific target instance.
#[derive(Clone)]
struct FunctionTargetSummary {
    explicit_always_cleans: bool,
    explicit_any_cleans: bool,
    implicit_always_cleans: bool,
    cleanup_sites: Vec<CleanupSiteData>,
    explicit_gaps: Vec<GapItem>,
    implicit_may_throw_sites: Vec<DiagnosticLocation>,
    implicit_gaps: Vec<GapItem>,
    implicit_counterexample: Vec<ControlFlowPathStep>,
    uncleaned_path: Vec<ControlFlowPathStep>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum TargetLifeState {
    PreAlloc,
    LiveUncleaned,
    Cleaned,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CfgExploreKey {
    vid: VertexId,
    stack: StackId,
    life: TargetLifeState,
    attached_mask: u32,
    used_may_throw: bool,
    has_unknown: bool,
}

#[derive(Clone)]
enum CfgStepKind {
    Explicit(ExplicitEdgeKind),
    MayThrow(MayThrowReason),
    Alloc,
    Cleanup,
    HelperUncleaned,
}

struct CfgExploreNode {
    key: CfgExploreKey,
    parent: Option<(usize, VertexId, CfgStepKind)>,
}

/// Evaluates dual explicit + implicit-exception `instanceCleanup` over IR for all `scopes` and `targets`.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Receives scopes, target instances, file allowlist, IR environment, summary index, and call graph/field write caches."
)]
pub fn evaluate_instance_cleanup_on_ir<S: BuildHasher>(
    scopes: &[CleanupScopeSpec],
    targets: &[CleanupTargetSpec],
    project_files: &[FileId],
    allowed_files: Option<&HashSet<FileId, S>>,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    summary_index: &BehaviorSummaryIndex,
    cached_call_graph: Option<&IrCallGraph>,
    cached_field_index: Option<&IrFieldWriteIndex>,
) -> (
    InstanceCleanupBatchOutcome,
    Option<IrCallGraph>,
    Option<IrFieldWriteIndex>,
) {
    let owned_allowed: Option<FxHashSet<FileId>> =
        allowed_files.map(|set| set.iter().copied().collect());
    let extra_field_files: FxHashSet<FileId> = scopes
        .iter()
        .map(|s| s.syntax.file_id())
        .chain(
            targets
                .iter()
                .filter_map(|t| t.origin_syntax.map(SyntaxId::file_id)),
        )
        .collect();
    let mut owned_graph: Option<IrCallGraph> = None;
    let mut engine = CleanupAnalyzer {
        targets,
        project_files,
        allowed_files: owned_allowed.as_ref(),
        extra_field_files,
        semantic,
        syntax,
        ir_provider,
        summary_index,
        cached_call_graph,
        owned_graph: &mut owned_graph,
        cached_field_index,
        owned_field_index: None,
        cleanup_index: None,
        vertex_effect_cache: FxHashMap::default(),
        conflict: None,
    };

    let mut results = Vec::with_capacity(scopes.len());
    for &scope in scopes {
        if engine.conflict.is_some() {
            break;
        }
        results.push(engine.analyze_scope(scope));
    }

    let conflict = engine.conflict.take();
    let owned_field_index = engine.owned_field_index.take();
    drop(engine);

    let ret_graph = if allowed_files.is_none() {
        owned_graph
    } else {
        None
    };
    let ret_field_index = if allowed_files.is_none() {
        owned_field_index
    } else {
        None
    };

    (
        InstanceCleanupBatchOutcome { results, conflict },
        ret_graph,
        ret_field_index,
    )
}

type VertexCacheKey = (FunctionIrId, VertexId, Vec<SyntaxId>);

struct CleanupAnalyzer<'a> {
    targets: &'a [CleanupTargetSpec],
    project_files: &'a [FileId],
    allowed_files: Option<&'a FxHashSet<FileId>>,
    extra_field_files: FxHashSet<FileId>,
    semantic: &'a dyn SemanticModel,
    syntax: &'a dyn SyntaxCatalog,
    ir_provider: &'a dyn IrProvider,
    summary_index: &'a BehaviorSummaryIndex,
    cached_call_graph: Option<&'a IrCallGraph>,
    owned_graph: &'a mut Option<IrCallGraph>,
    cached_field_index: Option<&'a IrFieldWriteIndex>,
    owned_field_index: Option<IrFieldWriteIndex>,
    cleanup_index: Option<IrCleanupProjectIndex>,
    vertex_effect_cache: FxHashMap<VertexCacheKey, VertexCleanupEffect>,
    conflict: Option<SummaryConflictError>,
}

impl CleanupAnalyzer<'_> {
    fn ensure_cleanup_index(&mut self) -> &IrCleanupProjectIndex {
        if self.cleanup_index.is_none() {
            let mut index = IrCleanupProjectIndex::default();
            let files: Vec<FileId> = self.allowed_files.map_or_else(
                || self.project_files.to_vec(),
                |set| {
                    self.project_files
                        .iter()
                        .copied()
                        .filter(|f| set.contains(f))
                        .collect()
                },
            );

            for file in files {
                for scope in self.ir_provider.scopes_in_file(file) {
                    let Ok(ir) = self.ir_provider.function_ir(scope.root_syntax) else {
                        continue;
                    };
                    self.index_cleanup_ops_in_ir(&ir, &mut index);
                    if self.conflict.is_some() {
                        break;
                    }
                }
                if self.conflict.is_some() {
                    break;
                }
            }
            self.cleanup_index = Some(index);
        }
        self.cleanup_index.as_ref().expect("initialized")
    }

    fn index_cleanup_ops_in_ir(&mut self, ir: &Arc<FunctionIr>, index: &mut IrCleanupProjectIndex) {
        for v in &ir.vertices {
            let Some(syn) = v.syntax else {
                continue;
            };
            match &v.op {
                IrOp::Call { call_kind, .. } => {
                    let Some(matched) = self.lookup_effects_summary(syn, *call_kind) else {
                        if self.conflict.is_some() {
                            return;
                        }
                        continue;
                    };
                    for op in &matched.aspect.resource_ops {
                        if let ResourceOpSpec::RegistersIntoContainer { item, container } = op {
                            index.registrations.push(RegistrationEntry {
                                scope_ir: Arc::clone(ir),
                                vertex_id: v.id,
                                location: v.location.clone(),
                                container_slot: container.clone(),
                                item_slot: item.clone(),
                                summary_id: matched.pack_id.clone(),
                                summary_source: matched.source,
                                label: format!("container registration via '{}'", matched.pack_id),
                            });
                        }
                    }
                }
                IrOp::PropWrite { object, value, .. } => {
                    let Some(matched) = self.lookup_prop_write_effects_summary(syn) else {
                        if self.conflict.is_some() {
                            return;
                        }
                        continue;
                    };
                    if matched
                        .aspect
                        .resource_ops
                        .iter()
                        .any(|op| matches!(op, ResourceOpSpec::ReplacesPreviousValue { .. }))
                    {
                        let start = v.location.as_ref().map_or(0, |l| l.start);
                        index.slot_writes.push(SlotWriteEntry {
                            scope_ir: Arc::clone(ir),
                            vertex_id: v.id,
                            write_syntax: syn,
                            location: v.location.clone(),
                            object_val: *object,
                            rhs_val: *value,
                            summary_id: matched.pack_id.clone(),
                            summary_source: matched.source,
                            label: format!("slot assignment via '{}'", matched.pack_id),
                            file: syn.file_id(),
                            start,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn analyze_scope(&mut self, scope: CleanupScopeSpec) -> InstanceCleanupScopeOutcome {
        let (entry_fns, scope_evidence, scope_gaps) = self.resolve_scope_functions(scope.syntax);
        if self.conflict.is_some() {
            return InstanceCleanupScopeOutcome {
                resolved_functions: Vec::new(),
                target_results: Vec::new(),
                disposed_instances: Vec::new(),
                registered_instances: Vec::new(),
                detached_instances: Vec::new(),
                evidence: scope_evidence,
                gaps: scope_gaps,
            };
        }

        let resolved_functions: Vec<SyntaxId> =
            entry_fns.iter().map(|e| e.ir.root_syntax).collect();

        // Collect scope-level disposed/registered/detached sites and dynamic gaps.
        let mut scope_disposals = Vec::new();
        let mut scope_registrations = Vec::new();
        let mut scope_detachments = Vec::new();
        let mut all_scope_gaps = scope_gaps.clone();
        let mut all_scope_evidence = scope_evidence.clone();
        let mut visited_collect = FxHashSet::default();

        for entry in &entry_fns {
            self.collect_scope_actions(
                &entry.ir,
                &entry.captured_stack,
                None,
                None,
                &scope_evidence,
                0,
                &mut visited_collect,
                &mut scope_disposals,
                &mut scope_registrations,
                &mut scope_detachments,
                &mut all_scope_gaps,
            );
            if self.conflict.is_some() {
                break;
            }
        }
        for d in &scope_disposals {
            for ev in &d.evidence {
                push_unique_evidence(&mut all_scope_evidence, ev.clone());
            }
        }

        // Precompute initial container memberships registered outside/before the scope.
        let mut target_results = Vec::with_capacity(self.targets.len());
        for target in self.targets {
            if self.conflict.is_some() {
                break;
            }
            let res = self.evaluate_target_across_entries(
                target,
                &entry_fns,
                &scope_evidence,
                &scope_gaps,
            );
            target_results.push(res);
        }

        InstanceCleanupScopeOutcome {
            resolved_functions,
            target_results,
            disposed_instances: dedup_sites(scope_disposals),
            registered_instances: dedup_sites(scope_registrations),
            detached_instances: dedup_sites(scope_detachments),
            evidence: all_scope_evidence,
            gaps: all_scope_gaps,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Resolves function literals, declarations, regular calls, and BehaviorSummary lifecycle-end callback calls."
    )]
    fn resolve_scope_functions(
        &mut self,
        scope_syntax: SyntaxId,
    ) -> (Vec<ResolvedEntryFn>, Vec<EvidenceItem>, Vec<GapItem>) {
        // 1. Direct function-like scope IR.
        if let Ok(ir) = self.ir_provider.function_ir(scope_syntax)
            && !matches!(
                ir.scope_kind,
                IrScopeKind::ModuleTopLevel | IrScopeKind::LocalBlock
            )
        {
            return (
                vec![ResolvedEntryFn {
                    ir,
                    captured_stack: Vec::new(),
                }],
                Vec::new(),
                Vec::new(),
            );
        }

        // 2. Call site (check `lifecycle-end` callback summary first, then regular call resolution).
        if let Ok(enclosing_ir) = self.ir_provider.enclosing_scope_ir(scope_syntax) {
            if let Some(call_v) = find_call_vertex_by_syntax(&enclosing_ir, scope_syntax) {
                let call_loc = call_v
                    .location
                    .clone()
                    .or_else(|| self.syntax.syntax_location(scope_syntax));
                if let Some((identity, matched)) = self.lookup_calls_summary_with_id(scope_syntax)
                    && let Some(cb_spec) = matched
                        .aspect
                        .invokes_callbacks
                        .iter()
                        .find(|cb| cb.timing == CallbackTiming::LifecycleEnd)
                {
                    self.semantic.record_summary_lookup(Some(matched.source));
                    let mut evidence = vec![EvidenceItem::summary(
                        format!(
                            "Summary '{}' declares '{}' argument #{} as lifecycle-end callback.",
                            matched.pack_id,
                            describe_identity(&identity),
                            cb_spec.callback_arg_index
                        ),
                        call_loc.clone(),
                        matched.source.as_str().to_string(),
                        matched.pack_id.clone(),
                        Some(describe_identity(&identity)),
                    )];
                    let args = match &call_v.op {
                        IrOp::Call { args, .. } | IrOp::Construct { args, .. } => args.as_slice(),
                        _ => &[],
                    };
                    let Some(cb_arg) = args.get(cb_spec.callback_arg_index).copied() else {
                        return (
                            Vec::new(),
                            evidence,
                            vec![GapItem::from_reason(
                                UnknownReason::ParameterOutOfBounds,
                                format!(
                                    "Lifecycle event '{}' expected callback at argument #{}.",
                                    describe_identity(&identity),
                                    cb_spec.callback_arg_index
                                ),
                                call_loc,
                            )],
                        );
                    };
                    let arg_val = arg_value_id(cb_arg);
                    let (entries, mut gaps) =
                        self.resolve_callable_value(&enclosing_ir, arg_val, Some(call_v.id), &[]);
                    if entries.is_empty() {
                        push_unique_gap(
                            &mut gaps,
                            GapItem::from_reason(
                                UnknownReason::DynamicCallee,
                                format!(
                                    "Lifecycle-end callback for '{}' could not be statically resolved to a function body.",
                                    describe_identity(&identity)
                                ),
                                call_loc,
                            ),
                        );
                    } else if let Some(first) = entries.first() {
                        evidence.push(EvidenceItem::simple(
                            "source",
                            "Resolved lifecycle-end callback function.".to_string(),
                            Some(first.ir.scope_location.clone()),
                        ));
                    }
                    return (entries, evidence, gaps);
                }
                if self.conflict.is_some() {
                    return (Vec::new(), Vec::new(), Vec::new());
                }

                // Regular call passed as cleanup scope: enter callee(s) with call frame.
                let res = resolve_call_targets_with_summaries(
                    scope_syntax,
                    self.semantic,
                    self.syntax,
                    self.ir_provider,
                    self.summary_index,
                );
                if let Some(conflict) = res.conflict {
                    self.conflict = Some(conflict);
                    return (Vec::new(), Vec::new(), Vec::new());
                }
                let (recv, args) = match &call_v.op {
                    IrOp::Call { receiver, args, .. } => (*receiver, args.clone()),
                    IrOp::Construct { args, .. } => (None, args.clone()),
                    _ => (None, Vec::new()),
                };
                let mut entries = Vec::new();
                for t in res.targets {
                    if !t.is_external
                        && !t.is_default_library
                        && let Some(fn_syn) = t.fn_scope_syntax
                        && let Ok(callee_ir) = self.ir_provider.function_ir(fn_syn)
                    {
                        let frame = IrCallFrame {
                            call_syntax: scope_syntax,
                            call_location: call_loc.clone(),
                            caller_ir: Arc::clone(&enclosing_ir),
                            callee_ir: Arc::clone(&callee_ir),
                            receiver: recv.map(ReceiverBinding::Value),
                            args: args.clone(),
                            is_construct: false,
                            closure_env: Vec::new(),
                        };
                        entries.push(ResolvedEntryFn {
                            ir: callee_ir,
                            captured_stack: vec![frame],
                        });
                    }
                }
                return (entries, Vec::new(), res.gaps);
            }

            if scope_syntax.file_id() == enclosing_ir.root_syntax.file_id()
                && let Some(&val) = enclosing_ir.expr_to_value.get(&scope_syntax.local_id())
            {
                let hint = enclosing_ir
                    .expr_to_vertex
                    .get(&scope_syntax.local_id())
                    .copied();
                let (entries, gaps) = self.resolve_callable_value(&enclosing_ir, val, hint, &[]);
                return (entries, Vec::new(), gaps);
            }
        }

        // 3. Declaration pointing to callable.
        if let Some(decl) = self.semantic.resolve_declaration(scope_syntax)
            && let Some(fn_syn) = decl.callable_syntax.or(decl.initializer_syntax)
            && fn_syn != scope_syntax
        {
            return self.resolve_scope_functions(fn_syn);
        }

        (
            Vec::new(),
            Vec::new(),
            vec![GapItem::from_reason(
                UnknownReason::MissingFunctionBody,
                "Scope target could not be resolved to an analyzable function body.".to_string(),
                self.syntax.syntax_location(scope_syntax),
            )],
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Collects scope-level disposed/registered/detached instances and dynamic gaps across reachable calls."
    )]
    fn collect_scope_actions(
        &mut self,
        ir: &Arc<FunctionIr>,
        call_stack: &[IrCallFrame],
        top_site: Option<SyntaxId>,
        via_call: Option<SyntaxId>,
        chain_evidence: &[EvidenceItem],
        depth: usize,
        visited: &mut FxHashSet<(FunctionIrId, Vec<SyntaxId>)>,
        disposals: &mut Vec<CleanupSiteData>,
        registrations: &mut Vec<CleanupSiteData>,
        detachments: &mut Vec<CleanupSiteData>,
        gaps: &mut Vec<GapItem>,
    ) {
        if self.conflict.is_some() || depth >= MAX_CLEANUP_CALL_DEPTH {
            return;
        }
        let stack_key: Vec<SyntaxId> = call_stack.iter().map(|f| f.call_syntax).collect();
        if !visited.insert((ir.id, stack_key)) {
            return;
        }

        let reachable = compute_explicit_reachable_vertices(ir);
        for v in &ir.vertices {
            if !reachable
                .get(usize::try_from(v.id.0).unwrap_or(usize::MAX))
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            let effect = self.compute_vertex_effect(ir, v, call_stack);
            if self.conflict.is_some() {
                return;
            }
            for (_, mut site) in effect.direct_disposals {
                let effective_via =
                    via_call.or_else(|| top_site.filter(|ts| *ts != site.site_syntax));
                site.via_call = effective_via;
                site.via_call_location = effective_via.and_then(|c| self.syntax.syntax_location(c));
                for ev in chain_evidence {
                    push_unique_evidence(&mut site.evidence, ev.clone());
                }
                disposals.push(site);
            }
            for (container_key, container_stack, mut site) in effect.container_disposals {
                let effective_via =
                    via_call.or_else(|| top_site.filter(|ts| *ts != site.site_syntax));
                site.via_call = effective_via;
                site.via_call_location = effective_via.and_then(|c| self.syntax.syntax_location(c));
                for ev in chain_evidence {
                    push_unique_evidence(&mut site.evidence, ev.clone());
                }
                let children = self.expand_container_children_static(
                    &container_key,
                    &container_stack,
                    &site,
                    site.site_syntax,
                );
                disposals.extend(children);
            }
            for (_, _, mut site) in effect.registrations {
                let effective_via =
                    via_call.or_else(|| top_site.filter(|ts| *ts != site.site_syntax));
                site.via_call = effective_via;
                site.via_call_location = effective_via.and_then(|c| self.syntax.syntax_location(c));
                registrations.push(site);
            }
            for (_, _, mut site) in effect.detachments {
                let effective_via =
                    via_call.or_else(|| top_site.filter(|ts| *ts != site.site_syntax));
                site.via_call = effective_via;
                site.via_call_location = effective_via.and_then(|c| self.syntax.syntax_location(c));
                detachments.push(site);
            }
            if let Some(gap) = effect.dynamic_gap {
                push_unique_gap(gaps, gap);
            }
            for callee in effect
                .explicit_callees
                .into_iter()
                .chain(effect.summary_sync_callbacks)
            {
                let mut next_ev = chain_evidence.to_vec();
                let fn_label = self
                    .semantic
                    .resolve_declaration(callee.callee_ir.root_syntax)
                    .and_then(|d| d.name)
                    .unwrap_or_else(|| "<helper>".to_string());
                push_unique_evidence(
                    &mut next_ev,
                    EvidenceItem::simple(
                        "source",
                        format!("Called '{fn_label}'"),
                        callee.call_location.clone(),
                    ),
                );
                self.collect_scope_actions(
                    &callee.callee_ir,
                    &callee.next_stack,
                    Some(top_site.unwrap_or(callee.call_syntax)),
                    Some(via_call.unwrap_or(callee.call_syntax)),
                    &next_ev,
                    depth + 1,
                    visited,
                    disposals,
                    registrations,
                    detachments,
                    gaps,
                );
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Computes summary resourceOps, callee targets, and target-touching external gaps for one CFG vertex."
    )]
    fn compute_vertex_effect(
        &mut self,
        ir: &Arc<FunctionIr>,
        vertex: &CfgVertex,
        call_stack: &[IrCallFrame],
    ) -> VertexCleanupEffect {
        let cache_key = (
            ir.id,
            vertex.id,
            call_stack.iter().map(|f| f.call_syntax).collect(),
        );
        if let Some(cached) = self.vertex_effect_cache.get(&cache_key) {
            return cached.clone();
        }

        let mut effect = VertexCleanupEffect::default();
        let Some(syn) = vertex.syntax else {
            self.vertex_effect_cache.insert(cache_key, effect.clone());
            return effect;
        };

        match &vertex.op {
            IrOp::Call {
                call_kind,
                callee,
                receiver,
                args,
            } => {
                // Record any instances passed as receiver or args.
                if let Some(rv) = receiver {
                    for h in self.trace_val_hosts(ir, *rv, Some(vertex.id), call_stack) {
                        effect.touched_instances.insert(h.origin.instance_key);
                    }
                }
                for &arg in args {
                    for h in
                        self.trace_val_hosts(ir, arg_value_id(arg), Some(vertex.id), call_stack)
                    {
                        effect.touched_instances.insert(h.origin.instance_key);
                    }
                }

                // 1. Check `calls` summary (e.g. Array.prototype.forEach synchronous callbacks).
                let mut handled_by_calls_summary = false;
                if let Some((_, call_summary)) = self.lookup_calls_summary_with_id(syn)
                    && !call_summary.aspect.invokes_callbacks.is_empty()
                {
                    handled_by_calls_summary = true;
                    for cb in &call_summary.aspect.invokes_callbacks {
                        if cb.timing == CallbackTiming::Synchronous
                            && let Some(arg) = args.get(cb.callback_arg_index).copied()
                        {
                            let (entries, mut cb_gaps) = self.resolve_callable_value(
                                ir,
                                arg_value_id(arg),
                                Some(vertex.id),
                                call_stack,
                            );
                            if let Some(g) = cb_gaps.pop() {
                                effect.dynamic_gap = Some(g);
                            }
                            for e in entries {
                                effect.summary_sync_callbacks.push(CalleeInvocation {
                                    callee_ir: e.ir,
                                    call_syntax: syn,
                                    call_location: vertex.location.clone(),
                                    next_stack: e.captured_stack,
                                });
                            }
                        }
                    }
                }
                if self.conflict.is_some() {
                    return effect;
                }

                // 2. Check `effects` summary for DisposesTarget / RegistersIntoContainer / DetachesFromContainer / ReplacesPreviousValue.
                let mut matched_effects_summary = self.lookup_effects_summary(syn, *call_kind);
                if self.conflict.is_some() {
                    return effect;
                }
                if matched_effects_summary.is_none()
                    && matches!(call_kind, IrCallKind::ImplicitDispose { .. })
                {
                    // Fallback for structural `[Symbol.dispose]` / `[Symbol.asyncDispose]` in `using`/`await using`.
                    let res = resolve_call_targets_with_summaries(
                        syn,
                        self.semantic,
                        self.syntax,
                        self.ir_provider,
                        self.summary_index,
                    );
                    if let Some(conflict) = res.conflict {
                        self.conflict = Some(conflict);
                        return effect;
                    }
                    let has_source_body = res.targets.iter().any(|t| {
                        !t.is_external && !t.is_default_library && t.fn_scope_syntax.is_some()
                    });
                    if !has_source_body {
                        matched_effects_summary = Some(MatchedSummaryAspect {
                            source: SummarySource::Builtin,
                            pack_id: "builtin:stdlib".to_string(),
                            entry_id: None,
                            origin_file: None,
                            matcher: tssae_common::SummaryTargetMatcher {
                                package: None,
                                version: None,
                                export_path: None,
                                module: None,
                                member_path: vec![
                                    "Disposable".to_string(),
                                    "prototype".to_string(),
                                    "[Symbol.dispose]".to_string(),
                                ],
                            },
                            aspect: EffectAspectSummary {
                                completeness: SummaryCompleteness::Complete,
                                resource_ops: vec![ResourceOpSpec::DisposesTarget {
                                    target: SummarySlotRef::Receiver(
                                        tssae_common::ForwardCallReceiver::Receiver,
                                    ),
                                }],
                                mutates_receiver: Some(true),
                            },
                        });
                    }
                }

                let mut summary_is_complete = false;
                if let Some(matched) = &matched_effects_summary {
                    self.semantic.record_summary_lookup(Some(matched.source));
                    summary_is_complete =
                        matched.aspect.completeness == SummaryCompleteness::Complete;
                    let loc = vertex
                        .location
                        .clone()
                        .or_else(|| self.syntax.syntax_location(syn));
                    if let Some(loc) = loc {
                        for op in &matched.aspect.resource_ops {
                            self.apply_call_resource_op(
                                ir,
                                vertex,
                                syn,
                                &loc,
                                *receiver,
                                args,
                                call_stack,
                                matched,
                                op,
                                &mut effect,
                            );
                        }
                    }
                }

                // 3. Always resolve concrete source callees (helpers, wrappers, parameter callbacks)
                //    so `{ dispose: () => store.dispose() }` expands even when IDisposable.dispose matched.
                if !handled_by_calls_summary {
                    let (callees, dynamic_gap, is_external_decl_only) = self
                        .resolve_cleanup_call_callees(
                            ir, vertex, syn, *callee, *receiver, args, call_stack,
                        );
                    if self.conflict.is_some() {
                        return effect;
                    }
                    effect.explicit_callees = callees;
                    if !summary_is_complete {
                        if is_external_decl_only
                            && effect.explicit_callees.is_empty()
                            && dynamic_gap.as_ref().is_some_and(|g| {
                                g.kind == UnknownReason::MissingBehaviorSummary.as_gap_kind()
                            })
                        {
                            // External call lacking a `calls` summary: only degrade live targets
                            // passed via receiver/arguments unless a closure argument was supplied.
                            let passes_closure = args.iter().any(|arg| {
                                !self
                                    .resolve_callable_value(
                                        ir,
                                        arg_value_id(*arg),
                                        Some(vertex.id),
                                        call_stack,
                                    )
                                    .0
                                    .is_empty()
                            });
                            if passes_closure {
                                effect.dynamic_gap = dynamic_gap;
                            } else {
                                effect.unmodeled_external_gap = dynamic_gap;
                            }
                        } else if let Some(g) = dynamic_gap {
                            effect.dynamic_gap = Some(g);
                        } else if is_external_decl_only && effect.explicit_callees.is_empty() {
                            effect.unmodeled_external_gap = Some(GapItem::from_reason(
                                UnknownReason::MissingFunctionBody,
                                format!(
                                    "External call '{}' has no behavior summary declaring resource effects.",
                                    self.syntax
                                        .syntax_text(syn)
                                        .unwrap_or_else(|| "<call>".to_string())
                                ),
                                vertex.location.clone(),
                            ));
                        }
                    }
                }
            }
            IrOp::Construct { callee, args, .. } => {
                for &arg in args {
                    for h in
                        self.trace_val_hosts(ir, arg_value_id(arg), Some(vertex.id), call_stack)
                    {
                        effect.touched_instances.insert(h.origin.instance_key);
                    }
                }
                let _ = callee;
                let res = resolve_call_targets_with_summaries(
                    syn,
                    self.semantic,
                    self.syntax,
                    self.ir_provider,
                    self.summary_index,
                );
                if let Some(conflict) = res.conflict {
                    self.conflict = Some(conflict);
                    return effect;
                }
                for t in res.targets {
                    if !t.is_external
                        && !t.is_default_library
                        && let Some(fn_syn) = t.fn_scope_syntax
                        && let Ok(callee_ir) = self.ir_provider.function_ir(fn_syn)
                    {
                        let mut next_stack = call_stack.to_vec();
                        next_stack.push(IrCallFrame {
                            call_syntax: syn,
                            call_location: vertex.location.clone(),
                            caller_ir: Arc::clone(ir),
                            callee_ir: Arc::clone(&callee_ir),
                            receiver: None,
                            args: args.clone(),
                            is_construct: true,
                            closure_env: Vec::new(),
                        });
                        effect.explicit_callees.push(CalleeInvocation {
                            callee_ir,
                            call_syntax: syn,
                            call_location: vertex.location.clone(),
                            next_stack,
                        });
                    }
                }
            }
            IrOp::PropWrite { object, value, .. } => {
                if let Some(matched) = self.lookup_prop_write_effects_summary(syn) {
                    if self.conflict.is_some() {
                        return effect;
                    }
                    if matched
                        .aspect
                        .resource_ops
                        .iter()
                        .any(|op| matches!(op, ResourceOpSpec::ReplacesPreviousValue { .. }))
                    {
                        self.semantic.record_summary_lookup(Some(matched.source));
                        let slot_hosts =
                            self.trace_val_hosts(ir, *object, Some(vertex.id), call_stack);
                        let rhs_keys: BTreeSet<String> = self
                            .trace_val_hosts(ir, *value, Some(vertex.id), call_stack)
                            .into_iter()
                            .map(|h| h.origin.instance_key)
                            .collect();
                        let write_start = vertex.location.as_ref().map_or(0, |l| l.start);
                        let write_loc = vertex
                            .location
                            .clone()
                            .or_else(|| self.syntax.syntax_location(syn));
                        let slot_writes = self.ensure_cleanup_index().slot_writes.clone();

                        for slot_host in &slot_hosts {
                            effect
                                .touched_instances
                                .insert(slot_host.origin.instance_key.clone());
                            for sw in &slot_writes {
                                if sw.write_syntax == syn {
                                    continue;
                                }
                                if sw.file == syn.file_id() && sw.start > write_start {
                                    continue;
                                }
                                let sw_slots = self.trace_val_hosts(
                                    &sw.scope_ir,
                                    sw.object_val,
                                    Some(sw.vertex_id),
                                    &[],
                                );
                                if !sw_slots
                                    .iter()
                                    .any(|o| o.origin.instance_key == slot_host.origin.instance_key)
                                {
                                    continue;
                                }
                                let prev_items = self.trace_val_hosts(
                                    &sw.scope_ir,
                                    sw.rhs_val,
                                    Some(sw.vertex_id),
                                    &[],
                                );
                                for prev in prev_items {
                                    if rhs_keys.contains(&prev.origin.instance_key) {
                                        continue;
                                    }
                                    if let Some(ref loc) = write_loc {
                                        let mut ev = vec![EvidenceItem::simple(
                                            "source",
                                            "Previous value assigned to replacement slot here."
                                                .to_string(),
                                            sw.location.clone(),
                                        )];
                                        ev.push(EvidenceItem::summary(
                                            format!(
                                                "Summary '{}' declares property assignment replaces and disposes previous value.",
                                                matched.pack_id
                                            ),
                                            Some(loc.clone()),
                                            matched.source.as_str().to_string(),
                                            matched.pack_id.clone(),
                                            Some(matched.pack_id.clone()),
                                        ));
                                        let key = prev.origin.instance_key.clone();
                                        effect.is_direct_cleanup_call = true;
                                        effect.direct_disposals.push((
                                            key,
                                            CleanupSiteData {
                                                origin: prev.origin,
                                                operation_kind: "replaces-previous-value"
                                                    .to_string(),
                                                site_syntax: syn,
                                                location: loc.clone(),
                                                via_call: None,
                                                via_call_location: None,
                                                summary_id: Some(matched.pack_id.clone()),
                                                summary_source: Some(matched.source),
                                                evidence: ev,
                                            },
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            IrOp::Unknown {
                reason,
                message,
                location,
                ..
            } => {
                effect.dynamic_gap = Some(GapItem::from_reason(
                    *reason,
                    message.clone(),
                    Some(location.clone()),
                ));
            }
            _ => {}
        }

        self.vertex_effect_cache.insert(cache_key, effect.clone());
        effect
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Applies DisposesTarget, ReplacesPreviousValue, RegistersIntoContainer, or DetachesFromContainer from summary."
    )]
    fn apply_call_resource_op(
        &mut self,
        ir: &Arc<FunctionIr>,
        vertex: &CfgVertex,
        syn: SyntaxId,
        loc: &DiagnosticLocation,
        receiver: Option<ValueId>,
        args: &[IrCallArg],
        call_stack: &[IrCallFrame],
        matched: &MatchedSummaryAspect<EffectAspectSummary>,
        op: &ResourceOpSpec,
        effect: &mut VertexCleanupEffect,
    ) {
        let resolve_slot = |slot: &SummarySlotRef| -> Option<ValueId> {
            match slot {
                SummarySlotRef::Receiver(_) => receiver,
                SummarySlotRef::Arg { arg_index } => {
                    args.get(*arg_index).copied().map(arg_value_id)
                }
            }
        };
        let summary_ev = EvidenceItem::summary(
            format!(
                "Summary '{}' declares call performs '{}'.",
                matched.pack_id,
                resource_op_kind_str(op)
            ),
            Some(loc.clone()),
            matched.source.as_str().to_string(),
            matched.pack_id.clone(),
            Some(matched.pack_id.clone()),
        );

        match op {
            ResourceOpSpec::DisposesTarget { target } => {
                if let Some(tv) = resolve_slot(target) {
                    let hosts = self.trace_val_hosts(ir, tv, Some(vertex.id), call_stack);
                    for host in hosts {
                        let key = host.origin.instance_key.clone();
                        effect.is_direct_cleanup_call = true;
                        let site = CleanupSiteData {
                            origin: host.origin,
                            operation_kind: "disposes-target".to_string(),
                            site_syntax: syn,
                            location: loc.clone(),
                            via_call: None,
                            via_call_location: None,
                            summary_id: Some(matched.pack_id.clone()),
                            summary_source: Some(matched.source),
                            evidence: vec![summary_ev.clone()],
                        };
                        effect.direct_disposals.push((key.clone(), site.clone()));
                        effect
                            .container_disposals
                            .push((key, host.captured_stack, site));
                    }
                }
            }
            ResourceOpSpec::ReplacesPreviousValue { target } => {
                if let Some(tv) = resolve_slot(target) {
                    let hosts = self.trace_val_hosts(ir, tv, Some(vertex.id), call_stack);
                    for host in hosts {
                        let key = host.origin.instance_key.clone();
                        effect.is_direct_cleanup_call = true;
                        let site = CleanupSiteData {
                            origin: host.origin,
                            operation_kind: "replaces-previous-value".to_string(),
                            site_syntax: syn,
                            location: loc.clone(),
                            via_call: None,
                            via_call_location: None,
                            summary_id: Some(matched.pack_id.clone()),
                            summary_source: Some(matched.source),
                            evidence: vec![summary_ev.clone()],
                        };
                        effect.direct_disposals.push((key, site));
                    }
                }
            }
            ResourceOpSpec::RegistersIntoContainer { item, container } => {
                let container_keys: Vec<String> = resolve_slot(container).map_or_default(|cv| {
                    self.trace_val_hosts(ir, cv, Some(vertex.id), call_stack)
                        .into_iter()
                        .map(|h| h.origin.instance_key)
                        .collect()
                });
                if let Some(iv) = resolve_slot(item) {
                    let item_hosts = self.trace_val_hosts(ir, iv, Some(vertex.id), call_stack);
                    for item_host in item_hosts {
                        let item_key = item_host.origin.instance_key.clone();
                        let site = CleanupSiteData {
                            origin: item_host.origin,
                            operation_kind: "registers-into-container".to_string(),
                            site_syntax: syn,
                            location: loc.clone(),
                            via_call: None,
                            via_call_location: None,
                            summary_id: Some(matched.pack_id.clone()),
                            summary_source: Some(matched.source),
                            evidence: vec![summary_ev.clone()],
                        };
                        for ckey in &container_keys {
                            effect.registrations.push((
                                ckey.clone(),
                                item_key.clone(),
                                site.clone(),
                            ));
                        }
                    }
                }
            }
            ResourceOpSpec::DetachesFromContainer { item, container } => {
                let container_keys: Vec<String> = resolve_slot(container).map_or_default(|cv| {
                    self.trace_val_hosts(ir, cv, Some(vertex.id), call_stack)
                        .into_iter()
                        .map(|h| h.origin.instance_key)
                        .collect()
                });
                if let Some(iv) = resolve_slot(item) {
                    let item_hosts = self.trace_val_hosts(ir, iv, Some(vertex.id), call_stack);
                    for item_host in item_hosts {
                        let item_key = item_host.origin.instance_key.clone();
                        let site = CleanupSiteData {
                            origin: item_host.origin,
                            operation_kind: "detaches-from-container".to_string(),
                            site_syntax: syn,
                            location: loc.clone(),
                            via_call: None,
                            via_call_location: None,
                            summary_id: Some(matched.pack_id.clone()),
                            summary_source: Some(matched.source),
                            evidence: vec![summary_ev.clone()],
                        };
                        for ckey in &container_keys {
                            effect
                                .detachments
                                .push((ckey.clone(), item_key.clone(), site.clone()));
                        }
                    }
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Resolves parameter callbacks, closure wrappers, and static project targets with external fallback detection."
    )]
    fn resolve_cleanup_call_callees(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_syntax: SyntaxId,
        callee_val: ValueId,
        receiver: Option<ValueId>,
        args: &[IrCallArg],
        call_stack: &[IrCallFrame],
    ) -> (Vec<CalleeInvocation>, Option<GapItem>, bool) {
        // 1. Parameter callback inside helper frame.
        if let Some(producer) =
            find_value_producer_vertex(caller_ir, callee_val, Some(call_vertex.id))
            && let IrOp::ReadLocal { binding } = producer.op
            && let Some(param_idx) = caller_ir
                .parameters
                .iter()
                .position(|p| p.binding == Some(binding))
            && let Some(frame_pos) = call_stack
                .iter()
                .rposition(|f| f.callee_ir.id == caller_ir.id)
        {
            let frame = &call_stack[frame_pos];
            let parent_stack = &call_stack[..frame_pos];
            if let Some(arg) = frame.args.get(param_idx).copied() {
                let (entries, mut gaps) = self.resolve_callable_value(
                    &frame.caller_ir,
                    arg_value_id(arg),
                    None,
                    parent_stack,
                );
                if entries.is_empty() && gaps.is_empty() {
                    gaps.push(GapItem::from_reason(
                        UnknownReason::DynamicCallee,
                        "Callback argument is invoked synchronously but its body cannot be statically resolved.".to_string(),
                        call_vertex.location.clone(),
                    ));
                }
                let invs = entries
                    .into_iter()
                    .map(|e| CalleeInvocation {
                        callee_ir: e.ir,
                        call_syntax,
                        call_location: call_vertex.location.clone(),
                        next_stack: e.captured_stack,
                    })
                    .collect();
                return (invs, gaps.into_iter().next(), false);
            }
        }

        // 2. Closure or object-literal method (`{ dispose: () => store.dispose() }`).
        let (closure_entries, _) =
            self.resolve_callable_value(caller_ir, callee_val, Some(call_vertex.id), call_stack);
        if self.conflict.is_some() {
            return (Vec::new(), None, false);
        }
        if !closure_entries.is_empty() {
            let invs = closure_entries
                .into_iter()
                .map(|e| {
                    let mut next_stack = call_stack.to_vec();
                    for cf in &e.captured_stack {
                        if !next_stack.iter().any(|x| x.call_syntax == cf.call_syntax) {
                            next_stack.push(cf.clone());
                        }
                    }
                    next_stack.push(IrCallFrame {
                        call_syntax,
                        call_location: call_vertex.location.clone(),
                        caller_ir: Arc::clone(caller_ir),
                        callee_ir: Arc::clone(&e.ir),
                        receiver: receiver.map(ReceiverBinding::Value),
                        args: args.to_vec(),
                        is_construct: false,
                        closure_env: e.captured_stack,
                    });
                    CalleeInvocation {
                        callee_ir: e.ir,
                        call_syntax,
                        call_location: call_vertex.location.clone(),
                        next_stack,
                    }
                })
                .collect();
            return (invs, None, false);
        }

        // 3. Standard static IR call target resolution.
        let res = resolve_call_targets_with_summaries(
            call_syntax,
            self.semantic,
            self.syntax,
            self.ir_provider,
            self.summary_index,
        );
        if let Some(conflict) = res.conflict {
            self.conflict = Some(conflict);
            return (Vec::new(), None, false);
        }

        let mut invs = Vec::new();
        let mut has_external_decl = false;
        for t in res.targets {
            if t.is_external || t.is_default_library || t.fn_scope_syntax.is_none() {
                has_external_decl = true;
                continue;
            }
            if let Some(fn_syn) = t.fn_scope_syntax
                && let Ok(callee_ir) = self.ir_provider.function_ir(fn_syn)
            {
                let mut next_stack = call_stack.to_vec();
                next_stack.push(IrCallFrame {
                    call_syntax,
                    call_location: call_vertex.location.clone(),
                    caller_ir: Arc::clone(caller_ir),
                    callee_ir: Arc::clone(&callee_ir),
                    receiver: receiver.map(ReceiverBinding::Value),
                    args: args.to_vec(),
                    is_construct: false,
                    closure_env: Vec::new(),
                });
                invs.push(CalleeInvocation {
                    callee_ir,
                    call_syntax,
                    call_location: call_vertex.location.clone(),
                    next_stack,
                });
            }
        }

        (invs, res.gaps.into_iter().next(), has_external_decl)
    }

    fn match_container_stack<'b>(
        &mut self,
        ir: &Arc<FunctionIr>,
        object_val: ValueId,
        vertex_id: VertexId,
        container_key: &str,
        container_stack: &'b [IrCallFrame],
    ) -> Option<&'b [IrCallFrame]> {
        if !container_stack.is_empty()
            && self
                .trace_val_hosts(ir, object_val, Some(vertex_id), container_stack)
                .iter()
                .any(|h| h.origin.instance_key == container_key)
        {
            Some(container_stack)
        } else if self
            .trace_val_hosts(ir, object_val, Some(vertex_id), &[])
            .iter()
            .any(|h| h.origin.instance_key == container_key)
        {
            Some(&[])
        } else {
            None
        }
    }

    fn expand_container_children_static(
        &mut self,
        container_key: &str,
        container_stack: &[IrCallFrame],
        dispose_site: &CleanupSiteData,
        dispose_syntax: SyntaxId,
    ) -> Vec<CleanupSiteData> {
        let mut out = Vec::new();
        let (registrations, slot_writes) = {
            let idx = self.ensure_cleanup_index();
            (idx.registrations.clone(), idx.slot_writes.clone())
        };

        for reg in registrations {
            let Some(c_val) = slot_val_in_call(&reg.scope_ir, reg.vertex_id, &reg.container_slot)
            else {
                continue;
            };
            let Some(i_val) = slot_val_in_call(&reg.scope_ir, reg.vertex_id, &reg.item_slot) else {
                continue;
            };
            let Some(used_stack) = self.match_container_stack(
                &reg.scope_ir,
                c_val,
                reg.vertex_id,
                container_key,
                container_stack,
            ) else {
                continue;
            };
            let i_hosts =
                self.trace_val_hosts(&reg.scope_ir, i_val, Some(reg.vertex_id), used_stack);
            for ih in i_hosts {
                let mut ev = dispose_site.evidence.clone();
                ev.push(EvidenceItem::summary(
                    reg.label.clone(),
                    reg.location.clone(),
                    reg.summary_source.as_str().to_string(),
                    reg.summary_id.clone(),
                    Some("container-registration".to_string()),
                ));
                out.push(CleanupSiteData {
                    origin: ih.origin,
                    operation_kind: "disposes-target".to_string(),
                    site_syntax: dispose_site.site_syntax,
                    location: dispose_site.location.clone(),
                    via_call: dispose_site.via_call,
                    via_call_location: dispose_site.via_call_location.clone(),
                    summary_id: Some(reg.summary_id.clone()),
                    summary_source: Some(reg.summary_source),
                    evidence: ev,
                });
            }
        }

        let dispose_start = dispose_site.location.start;
        for sw in slot_writes {
            if sw.write_syntax == dispose_syntax {
                continue;
            }
            if sw.file == dispose_syntax.file_id() && sw.start > dispose_start {
                continue;
            }
            let Some(used_stack) = self.match_container_stack(
                &sw.scope_ir,
                sw.object_val,
                sw.vertex_id,
                container_key,
                container_stack,
            ) else {
                continue;
            };
            let sw_items =
                self.trace_val_hosts(&sw.scope_ir, sw.rhs_val, Some(sw.vertex_id), used_stack);
            for ih in sw_items {
                let mut ev = dispose_site.evidence.clone();
                ev.push(EvidenceItem::summary(
                    sw.label.clone(),
                    sw.location.clone(),
                    sw.summary_source.as_str().to_string(),
                    sw.summary_id.clone(),
                    Some("slot-held-value".to_string()),
                ));
                out.push(CleanupSiteData {
                    origin: ih.origin,
                    operation_kind: "disposes-target".to_string(),
                    site_syntax: dispose_site.site_syntax,
                    location: dispose_site.location.clone(),
                    via_call: dispose_site.via_call,
                    via_call_location: dispose_site.via_call_location.clone(),
                    summary_id: Some(sw.summary_id.clone()),
                    summary_source: Some(sw.summary_source),
                    evidence: ev,
                });
            }
        }

        out
    }

    fn collect_initial_target_containers(&mut self, target_key: &str) -> Vec<String> {
        let mut containers = Vec::new();
        let (registrations, slot_writes) = {
            let idx = self.ensure_cleanup_index();
            (idx.registrations.clone(), idx.slot_writes.clone())
        };

        for reg in registrations {
            let Some(c_val) = slot_val_in_call(&reg.scope_ir, reg.vertex_id, &reg.container_slot)
            else {
                continue;
            };
            let Some(i_val) = slot_val_in_call(&reg.scope_ir, reg.vertex_id, &reg.item_slot) else {
                continue;
            };
            let i_hosts = self.trace_val_hosts(&reg.scope_ir, i_val, Some(reg.vertex_id), &[]);
            if i_hosts.iter().any(|h| h.origin.instance_key == target_key) {
                for ch in self.trace_val_hosts(&reg.scope_ir, c_val, Some(reg.vertex_id), &[]) {
                    if !containers.contains(&ch.origin.instance_key) {
                        containers.push(ch.origin.instance_key);
                    }
                }
            }
        }

        // Also check slot-held values written before scope (e.g. MutableDisposable.value = second before slot.clear()).
        // Wait: if a wrapper `{ dispose: () => store.dispose() }` stored `item` inside `makeStoreWrapper(item)`,
        // tracing `item` inside `makeStoreWrapper` WITHOUT stack resolves to parameter `item`,
        // so we also match container disposal sites expanded with the container's `captured_stack`!
        let _ = slot_writes;
        containers
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Aggregates explicit and implicit-exception cleanup outcomes across resolved scope entries."
    )]
    fn evaluate_target_across_entries(
        &mut self,
        target: &CleanupTargetSpec,
        entries: &[ResolvedEntryFn],
        scope_evidence: &[EvidenceItem],
        scope_gaps: &[GapItem],
    ) -> TargetInstanceCleanupResultData {
        if entries.is_empty() {
            let mut gaps = scope_gaps.to_vec();
            if gaps.is_empty() {
                gaps.push(GapItem::from_reason(
                    UnknownReason::MissingFunctionBody,
                    "No analyzable cleanup scope function body was resolved.".to_string(),
                    target.allocation_location.clone(),
                ));
            }
            let evidence = gaps_to_evidence(&gaps);
            return TargetInstanceCleanupResultData {
                instance_key: target.instance_key.clone(),
                allocation_location: target.allocation_location.clone(),
                status: if scope_gaps.is_empty() {
                    TargetCleanupStatusData::Disproved {
                        cleaned_on_any_path: false,
                        cleanup_sites: Vec::new(),
                        uncleaned_path: Vec::new(),
                        evidence: evidence.clone(),
                    }
                } else {
                    TargetCleanupStatusData::Inconclusive {
                        cleanup_sites: Vec::new(),
                        gaps: gaps.clone(),
                        evidence: evidence.clone(),
                    }
                },
                implicit_exceptions: ImplicitExceptionCleanupOutcome::Inconclusive {
                    may_throw_sites: Vec::new(),
                    gaps,
                    evidence,
                },
            };
        }

        let initial_containers = self.collect_initial_target_containers(&target.instance_key);
        let mut container_registry = initial_containers.clone();
        let initial_mask: u32 = if initial_containers.is_empty() {
            0
        } else {
            (1u32 << initial_containers.len().min(MAX_CONTAINER_BITS)) - 1
        };

        let mut all_explicit_proved = true;
        let mut any_explicit_cleaned = false;
        let mut explicit_sites = Vec::new();
        let mut explicit_gaps = scope_gaps.to_vec();
        let mut uncleaned_path = Vec::new();
        let mut explicit_ev = scope_evidence.to_vec();

        let mut all_implicit_proved = true;
        let mut implicit_sites = Vec::new();
        let mut implicit_may_throw = Vec::new();
        let mut implicit_counterexample = Vec::new();
        let mut implicit_gaps = scope_gaps.to_vec();
        let mut implicit_ev = scope_evidence.to_vec();

        for entry in entries {
            let live_at_entry = target
                .origin_syntax
                .is_none_or(|alloc_syn| !ir_contains_syntax(&entry.ir, alloc_syn));
            let mut visited_fns = FxHashSet::default();
            let summary = self.evaluate_fn_for_target(
                &entry.ir,
                &entry.captured_stack,
                target,
                live_at_entry,
                initial_mask,
                &mut container_registry,
                0,
                &mut visited_fns,
            );
            if self.conflict.is_some() {
                break;
            }

            all_explicit_proved &= summary.explicit_always_cleans;
            any_explicit_cleaned |= summary.explicit_any_cleans;
            for site in &summary.cleanup_sites {
                for ev in &site.evidence {
                    push_unique_evidence(&mut explicit_ev, ev.clone());
                }
            }
            explicit_sites.extend(summary.cleanup_sites.clone());
            for g in summary.explicit_gaps {
                push_unique_gap(&mut explicit_gaps, g);
            }
            if uncleaned_path.is_empty() && !summary.uncleaned_path.is_empty() {
                uncleaned_path = summary.uncleaned_path;
            }

            all_implicit_proved &= summary.implicit_always_cleans;
            implicit_sites.extend(summary.cleanup_sites);
            for loc in summary.implicit_may_throw_sites {
                if !implicit_may_throw.contains(&loc) {
                    implicit_may_throw.push(loc);
                }
            }
            if implicit_counterexample.is_empty() && !summary.implicit_counterexample.is_empty() {
                implicit_counterexample = summary.implicit_counterexample;
            }
            for g in summary.implicit_gaps {
                push_unique_gap(&mut implicit_gaps, g);
            }
        }

        let explicit_sites = dedup_sites(explicit_sites);
        let implicit_sites = dedup_sites(implicit_sites);

        let status = if all_explicit_proved && !explicit_sites.is_empty() {
            TargetCleanupStatusData::Proved {
                cleanup_sites: explicit_sites,
                evidence: explicit_ev,
            }
        } else if !explicit_gaps.is_empty() && (uncleaned_path.is_empty() || !any_explicit_cleaned)
        {
            for g in &explicit_gaps {
                push_unique_evidence(
                    &mut explicit_ev,
                    EvidenceItem::simple("note", g.message.clone(), g.at.clone()),
                );
            }
            TargetCleanupStatusData::Inconclusive {
                cleanup_sites: explicit_sites,
                gaps: explicit_gaps,
                evidence: explicit_ev,
            }
        } else {
            for step in &uncleaned_path {
                push_unique_evidence(
                    &mut explicit_ev,
                    EvidenceItem::simple(
                        "source",
                        step.description.clone(),
                        Some(step.location.clone()),
                    ),
                );
            }
            TargetCleanupStatusData::Disproved {
                cleaned_on_any_path: any_explicit_cleaned,
                cleanup_sites: explicit_sites,
                uncleaned_path,
                evidence: explicit_ev,
            }
        };

        let implicit_exceptions = if all_implicit_proved {
            for site in &implicit_sites {
                for ev in &site.evidence {
                    push_unique_evidence(&mut implicit_ev, ev.clone());
                }
            }
            if implicit_ev.is_empty() {
                implicit_ev.push(EvidenceItem::simple(
                    "source",
                    "No unprotected may-throw evaluation path bypasses cleanup for this instance."
                        .to_string(),
                    target.allocation_location.clone(),
                ));
            }
            ImplicitExceptionCleanupOutcome::Proved {
                cleanup_sites: implicit_sites,
                evidence: implicit_ev,
            }
        } else if !implicit_counterexample.is_empty() {
            for loc in &implicit_may_throw {
                push_unique_evidence(
                    &mut implicit_ev,
                    EvidenceItem::simple(
                        "source",
                        "Implicit exception may be thrown here before cleanup completes."
                            .to_string(),
                        Some(loc.clone()),
                    ),
                );
            }
            ImplicitExceptionCleanupOutcome::Disproved {
                may_throw_sites: implicit_may_throw,
                counterexample_path: implicit_counterexample,
                evidence: implicit_ev,
            }
        } else {
            for g in &implicit_gaps {
                push_unique_evidence(
                    &mut implicit_ev,
                    EvidenceItem::simple("note", g.message.clone(), g.at.clone()),
                );
            }
            ImplicitExceptionCleanupOutcome::Inconclusive {
                may_throw_sites: implicit_may_throw,
                gaps: implicit_gaps,
                evidence: implicit_ev,
            }
        };

        TargetInstanceCleanupResultData {
            instance_key: target.instance_key.clone(),
            allocation_location: target.allocation_location.clone(),
            status,
            implicit_exceptions,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Dual-edge CFG exploration for explicit + implicit exception cleanup of one target inside one FunctionIr."
    )]
    fn evaluate_fn_for_target(
        &mut self,
        ir: &Arc<FunctionIr>,
        call_stack: &[IrCallFrame],
        target: &CleanupTargetSpec,
        live_at_entry: bool,
        initial_mask: u32,
        container_registry: &mut Vec<String>,
        depth: usize,
        visited_fns: &mut FxHashSet<(FunctionIrId, Vec<SyntaxId>)>,
    ) -> FunctionTargetSummary {
        let stack_key: Vec<SyntaxId> = call_stack.iter().map(|f| f.call_syntax).collect();
        if depth >= MAX_CLEANUP_CALL_DEPTH || !visited_fns.insert((ir.id, stack_key)) {
            return FunctionTargetSummary {
                explicit_always_cleans: false,
                explicit_any_cleans: false,
                implicit_always_cleans: false,
                cleanup_sites: Vec::new(),
                explicit_gaps: Vec::new(),
                implicit_may_throw_sites: Vec::new(),
                implicit_gaps: Vec::new(),
                implicit_counterexample: Vec::new(),
                uncleaned_path: Vec::new(),
            };
        }

        // Precompute per-vertex summary effects on `target` in `ir`.
        let mut vertex_direct_clean: Vec<bool> = vec![false; ir.vertices.len()];
        let mut vertex_cleanup_sites: Vec<Vec<CleanupSiteData>> =
            vec![Vec::new(); ir.vertices.len()];
        let mut vertex_helper_summary: Vec<Option<FunctionTargetSummary>> =
            vec![None; ir.vertices.len()];
        let mut vertex_relevant_gap: Vec<Option<GapItem>> = vec![None; ir.vertices.len()];
        let mut vertex_reg_mask: Vec<u32> = vec![0; ir.vertices.len()];
        let mut vertex_det_mask: Vec<u32> = vec![0; ir.vertices.len()];
        let mut vertex_container_clean_mask: Vec<u32> = vec![0; ir.vertices.len()];
        let mut all_collected_sites = Vec::new();

        for idx in 0..ir.vertices.len() {
            let v = &ir.vertices[idx];
            let effect = self.compute_vertex_effect(ir, v, call_stack);
            if self.conflict.is_some() {
                break;
            }

            for (key, site) in &effect.direct_disposals {
                if key == &target.instance_key {
                    vertex_direct_clean[idx] = true;
                    vertex_cleanup_sites[idx].push(site.clone());
                    all_collected_sites.push(site.clone());
                }
            }

            // Check container disposal expansion (including wrapper captured_stack!)
            for (ckey, container_stack, site) in &effect.container_disposals {
                let mut bit_idx = container_bit(container_registry, ckey);
                let mut matched_via_expansion = false;
                if let Some(syn) = v.syntax {
                    let expanded =
                        self.expand_container_children_static(ckey, container_stack, site, syn);
                    for child_site in expanded {
                        if child_site.origin.instance_key == target.instance_key {
                            matched_via_expansion = true;
                            vertex_cleanup_sites[idx].push(child_site.clone());
                            all_collected_sites.push(child_site);
                        }
                    }
                }
                if matched_via_expansion {
                    // Check if detached inside `ir` dynamically using container bit; if registered outside
                    // AND never registered/detached explicitly with `target`, still make sure bit is set in initial_mask?
                    // Note: `collect_initial_target_containers` already checked registrations matching `target`.
                    // If it matched only via wrapper `captured_stack` (e.g. `makeStoreWrapper`), `direct_clean` applies
                    // unless detached on a path inside `ir`.
                    if (initial_mask & (1u32 << bit_idx)) == 0 {
                        // Registered via wrapper/captured context -> treat container disposal as direct clean
                        // unless detached by `DetachesFromContainer` on the same container.
                        vertex_direct_clean[idx] = true;
                    } else {
                        vertex_container_clean_mask[idx] |= 1u32 << bit_idx;
                    }
                } else {
                    vertex_container_clean_mask[idx] |= 1u32 << bit_idx;
                }
                let _ = &mut bit_idx;
            }

            for (ckey, ikey, _) in &effect.registrations {
                if ikey == &target.instance_key {
                    let b = container_bit(container_registry, ckey);
                    vertex_reg_mask[idx] |= 1u32 << b;
                }
            }
            for (ckey, ikey, _) in &effect.detachments {
                if ikey == &target.instance_key {
                    let b = container_bit(container_registry, ckey);
                    vertex_det_mask[idx] |= 1u32 << b;
                }
            }

            // Recursively summarize helper callees for `target`.
            let callees: Vec<CalleeInvocation> = effect
                .explicit_callees
                .iter()
                .chain(effect.summary_sync_callbacks.iter())
                .cloned()
                .collect();
            if !callees.is_empty() {
                let mut combined_always_exp = true;
                let mut combined_any_exp = false;
                let mut combined_always_imp = true;
                let mut helper_sites = Vec::new();
                let mut helper_exp_gaps = Vec::new();
                let mut helper_imp_throws = Vec::new();
                let mut helper_imp_gaps = Vec::new();
                let mut helper_imp_ce = Vec::new();
                let mut helper_uncleaned = Vec::new();

                for callee in callees {
                    let sub = self.evaluate_fn_for_target(
                        &callee.callee_ir,
                        &callee.next_stack,
                        target,
                        true,
                        initial_mask,
                        container_registry,
                        depth + 1,
                        visited_fns,
                    );
                    combined_always_exp &= sub.explicit_always_cleans;
                    combined_any_exp |= sub.explicit_any_cleans;
                    combined_always_imp &= sub.implicit_always_cleans;
                    let fn_label = self
                        .semantic
                        .resolve_declaration(callee.callee_ir.root_syntax)
                        .and_then(|d| d.name)
                        .unwrap_or_else(|| "<helper>".to_string());
                    let call_ev = EvidenceItem::simple(
                        "source",
                        format!("Called '{fn_label}'"),
                        callee.call_location.clone(),
                    );
                    for mut s in sub.cleanup_sites {
                        if s.via_call.is_none() {
                            s.via_call = Some(callee.call_syntax);
                            s.via_call_location.clone_from(&callee.call_location);
                        }
                        push_unique_evidence(&mut s.evidence, call_ev.clone());
                        all_collected_sites.push(s.clone());
                        helper_sites.push(s);
                    }
                    helper_exp_gaps.extend(sub.explicit_gaps);
                    helper_imp_throws.extend(sub.implicit_may_throw_sites);
                    helper_imp_gaps.extend(sub.implicit_gaps);
                    if helper_imp_ce.is_empty() {
                        helper_imp_ce = sub.implicit_counterexample;
                    }
                    if helper_uncleaned.is_empty() {
                        helper_uncleaned = sub.uncleaned_path;
                    }
                }
                vertex_cleanup_sites[idx].extend(helper_sites.clone());
                vertex_helper_summary[idx] = Some(FunctionTargetSummary {
                    explicit_always_cleans: combined_always_exp,
                    explicit_any_cleans: combined_any_exp,
                    implicit_always_cleans: combined_always_imp,
                    cleanup_sites: helper_sites,
                    explicit_gaps: helper_exp_gaps,
                    implicit_may_throw_sites: helper_imp_throws,
                    implicit_gaps: helper_imp_gaps,
                    implicit_counterexample: helper_imp_ce,
                    uncleaned_path: helper_uncleaned,
                });
            }

            if let Some(g) = &effect.dynamic_gap {
                vertex_relevant_gap[idx] = Some(g.clone());
            } else if let Some(g) = &effect.unmodeled_external_gap
                && effect.touched_instances.contains(&target.instance_key)
            {
                vertex_relevant_gap[idx] = Some(g.clone());
            }
        }

        // Identify PropRead vertices that are solely direct `callee` lookups for cleanup-guaranteed calls.
        let mut skip_prop_read_may_throw: FxHashSet<VertexId> = FxHashSet::default();
        for (idx, v) in ir.vertices.iter().enumerate() {
            let guaranteed_cleanup_call = vertex_direct_clean[idx]
                || vertex_container_clean_mask[idx] != 0
                || vertex_helper_summary[idx]
                    .as_ref()
                    .is_some_and(|h| h.implicit_always_cleans);
            if guaranteed_cleanup_call
                && let IrOp::Call {
                    call_kind: IrCallKind::Explicit | IrCallKind::TaggedTemplate,
                    callee,
                    ..
                } = v.op
                && let Some(prod) = find_value_producer_vertex(ir, callee, Some(v.id))
                && matches!(
                    prod.op,
                    IrOp::PropRead {
                        implicit_call_kind: None,
                        ..
                    }
                )
            {
                skip_prop_read_may_throw.insert(prod.id);
            }
        }

        // Run BFS over dual CFG `(VertexId, StackId, live, cleaned, attached_mask, used_may_throw, has_unknown)`.
        let mut stacks = StackArena::new();
        let mut nodes: Vec<CfgExploreNode> = Vec::new();
        let mut key_to_idx: FxHashMap<CfgExploreKey, usize> = FxHashMap::default();
        let mut queue = VecDeque::new();

        let start_key = CfgExploreKey {
            vid: ir.entry,
            stack: StackId(0),
            life: if live_at_entry {
                TargetLifeState::LiveUncleaned
            } else {
                TargetLifeState::PreAlloc
            },
            attached_mask: initial_mask,
            used_may_throw: false,
            has_unknown: false,
        };
        key_to_idx.insert(start_key, 0);
        nodes.push(CfgExploreNode {
            key: start_key,
            parent: None,
        });
        queue.push_back(0);

        let mut exit_indices: Vec<usize> = Vec::new();

        while let Some(u_idx) = queue.pop_front() {
            let u_key = nodes[u_idx].key;
            if u_key.vid == ir.exit {
                exit_indices.push(u_idx);
                continue;
            }
            let u_v_idx = usize::try_from(u_key.vid.0).unwrap_or(usize::MAX);
            let Some(vertex) = ir.vertices.get(u_v_idx) else {
                continue;
            };

            let was_live = u_key.life != TargetLifeState::PreAlloc;
            let was_cleaned = u_key.life == TargetLifeState::Cleaned;
            let is_alloc_vertex = !was_live
                && target
                    .origin_syntax
                    .is_some_and(|s| vertex.syntax == Some(s));

            let mut next_attached = u_key.attached_mask;
            next_attached |= vertex_reg_mask[u_v_idx];
            next_attached &= !vertex_det_mask[u_v_idx];

            let cleaned_by_container =
                (u_key.attached_mask & vertex_container_clean_mask[u_v_idx]) != 0;
            let direct_or_container_cleans = vertex_direct_clean[u_v_idx] || cleaned_by_container;

            let helper_sum = vertex_helper_summary[u_v_idx].as_ref();
            let helper_always_cleans = helper_sum.is_some_and(|h| h.explicit_always_cleans);
            let helper_any_cleans = helper_sum.is_some_and(|h| h.explicit_any_cleans);
            let helper_imp_cleans = helper_sum.is_some_and(|h| h.implicit_always_cleans);
            let helper_has_exp_gaps = helper_sum.is_some_and(|h| !h.explicit_gaps.is_empty());

            let next_live_explicit = was_live || is_alloc_vertex;
            let vertex_unknown = u_key.life == TargetLifeState::LiveUncleaned
                && vertex_relevant_gap[u_v_idx].is_some();

            // 1. Explicit transitions.
            let mut explicit_outcomes: Vec<(bool, bool, CfgStepKind)> = Vec::new();
            if direct_or_container_cleans || helper_always_cleans {
                explicit_outcomes.push((true, u_key.has_unknown, CfgStepKind::Cleanup));
            } else if helper_any_cleans {
                // Helper cleans on some paths, misses on others.
                explicit_outcomes.push((true, u_key.has_unknown, CfgStepKind::Cleanup));
                explicit_outcomes.push((
                    was_cleaned,
                    u_key.has_unknown
                        || (u_key.life == TargetLifeState::LiveUncleaned && helper_has_exp_gaps),
                    CfgStepKind::HelperUncleaned,
                ));
            } else {
                let next_unk = u_key.has_unknown
                    || vertex_unknown
                    || (u_key.life == TargetLifeState::LiveUncleaned && helper_has_exp_gaps);
                explicit_outcomes.push((
                    was_cleaned,
                    next_unk,
                    if is_alloc_vertex {
                        CfgStepKind::Alloc
                    } else {
                        CfgStepKind::Explicit(ExplicitEdgeKind::Fallthrough)
                    },
                ));
            }

            for edge in &vertex.explicit_edges {
                let Some(next_stack) = stacks.transition_explicit(ir, u_key.stack, edge) else {
                    continue;
                };
                for (cleaned_flag, unk_flag, base_step) in &explicit_outcomes {
                    let step_kind = match base_step {
                        CfgStepKind::Explicit(_) => CfgStepKind::Explicit(edge.kind.clone()),
                        other => other.clone(),
                    };
                    let next_life = if !next_live_explicit {
                        TargetLifeState::PreAlloc
                    } else if was_cleaned || *cleaned_flag {
                        TargetLifeState::Cleaned
                    } else {
                        TargetLifeState::LiveUncleaned
                    };
                    let next_key = CfgExploreKey {
                        vid: edge.target,
                        stack: next_stack,
                        life: next_life,
                        attached_mask: next_attached,
                        used_may_throw: u_key.used_may_throw,
                        has_unknown: *unk_flag,
                    };
                    if let std::collections::hash_map::Entry::Vacant(e) = key_to_idx.entry(next_key)
                    {
                        let next_idx = nodes.len();
                        e.insert(next_idx);
                        nodes.push(CfgExploreNode {
                            key: next_key,
                            parent: Some((u_idx, u_key.vid, step_kind)),
                        });
                        queue.push_back(next_idx);
                    }
                }
            }

            // 2. Implicit `MayThrowEdge` transition (only when ALREADY live BEFORE `vertex`, NOT cleaned,
            //    and NOT a direct cleanup or implicit-safe helper call or its callee property lookup!).
            let guarantees_implicit_cleanup = direct_or_container_cleans || helper_imp_cleans;
            if let Some(may_throw) = vertex.may_throw_edge
                && u_key.life == TargetLifeState::LiveUncleaned
                && !guarantees_implicit_cleanup
                && !skip_prop_read_may_throw.contains(&vertex.id)
            {
                let next_stack = stacks.transition_may_throw(ir, u_key.stack, may_throw.target);
                let next_key = CfgExploreKey {
                    vid: may_throw.target,
                    stack: next_stack,
                    life: TargetLifeState::LiveUncleaned,
                    attached_mask: u_key.attached_mask,
                    used_may_throw: true,
                    has_unknown: u_key.has_unknown,
                };
                if let std::collections::hash_map::Entry::Vacant(e) = key_to_idx.entry(next_key) {
                    let next_idx = nodes.len();
                    e.insert(next_idx);
                    nodes.push(CfgExploreNode {
                        key: next_key,
                        parent: Some((u_idx, u_key.vid, CfgStepKind::MayThrow(may_throw.reason))),
                    });
                    queue.push_back(next_idx);
                }
            }
        }

        // Summarize explicit and implicit exit states where `live == true`.
        let mut explicit_any_cleans = false;
        let mut explicit_known_uncleaned: Option<usize> = None;
        let mut explicit_unknown_uncleaned: Vec<usize> = Vec::new();
        let mut explicit_live_seen = false;

        let mut implicit_known_uncleaned: Option<usize> = None;
        let mut implicit_unknown_uncleaned: Vec<usize> = Vec::new();

        for &exit_idx in &exit_indices {
            let k = nodes[exit_idx].key;
            if k.life == TargetLifeState::PreAlloc {
                continue;
            }
            let is_cleaned = k.life == TargetLifeState::Cleaned;
            if !k.used_may_throw {
                explicit_live_seen = true;
                if is_cleaned {
                    explicit_any_cleans = true;
                } else if k.has_unknown {
                    explicit_unknown_uncleaned.push(exit_idx);
                } else if explicit_known_uncleaned.is_none() {
                    explicit_known_uncleaned = Some(exit_idx);
                }
            } else if !is_cleaned {
                if k.has_unknown {
                    implicit_unknown_uncleaned.push(exit_idx);
                } else if implicit_known_uncleaned.is_none() {
                    implicit_known_uncleaned = Some(exit_idx);
                }
            }
        }

        let explicit_always_cleans = explicit_live_seen
            && explicit_known_uncleaned.is_none()
            && explicit_unknown_uncleaned.is_empty();
        let implicit_always_cleans =
            implicit_known_uncleaned.is_none() && implicit_unknown_uncleaned.is_empty();

        let uncleaned_path = explicit_known_uncleaned
            .map_or_default(|idx| reconstruct_path_steps(ir, &nodes, idx, &ir.scope_location));

        let mut explicit_gaps = Vec::new();
        if !explicit_always_cleans && explicit_known_uncleaned.is_none() {
            for &idx in &explicit_unknown_uncleaned {
                collect_gaps_on_chain(
                    &nodes,
                    idx,
                    &vertex_relevant_gap,
                    &vertex_helper_summary,
                    &mut explicit_gaps,
                );
            }
        }

        let mut implicit_may_throw_sites = Vec::new();
        let implicit_counterexample = if let Some(idx) = implicit_known_uncleaned {
            collect_may_throw_sites_on_chain(
                ir,
                &nodes,
                idx,
                &vertex_helper_summary,
                &mut implicit_may_throw_sites,
            );
            reconstruct_path_steps(ir, &nodes, idx, &ir.scope_location)
        } else {
            for &idx in &implicit_unknown_uncleaned {
                collect_may_throw_sites_on_chain(
                    ir,
                    &nodes,
                    idx,
                    &vertex_helper_summary,
                    &mut implicit_may_throw_sites,
                );
            }
            Vec::new()
        };

        let mut implicit_gaps = Vec::new();
        if !implicit_always_cleans && implicit_known_uncleaned.is_none() {
            for &idx in &implicit_unknown_uncleaned {
                collect_gaps_on_chain(
                    &nodes,
                    idx,
                    &vertex_relevant_gap,
                    &vertex_helper_summary,
                    &mut implicit_gaps,
                );
            }
        }

        FunctionTargetSummary {
            explicit_always_cleans,
            explicit_any_cleans,
            implicit_always_cleans,
            cleanup_sites: dedup_sites(all_collected_sites),
            explicit_gaps,
            implicit_may_throw_sites,
            implicit_gaps,
            implicit_counterexample,
            uncleaned_path,
        }
    }

    fn trace_val_hosts(
        &mut self,
        ir: &Arc<FunctionIr>,
        val: ValueId,
        hint: Option<VertexId>,
        call_stack: &[IrCallFrame],
    ) -> Vec<HostOrigin> {
        let mut extra_files = self.extra_field_files.clone();
        extra_files.insert(ir.id.file);
        let mut resolver = IrInstanceResolver::new(
            self.project_files,
            self.allowed_files,
            extra_files,
            self.semantic,
            self.syntax,
            self.ir_provider,
            self.summary_index,
            self.cached_call_graph,
            self.owned_graph,
            self.cached_field_index,
            &mut self.owned_field_index,
            true,
        );
        let mut active = Vec::new();
        let res = resolver.trace_value(ir, val, hint, call_stack, 0, &mut active);
        if let Some(conflict) = resolver.conflict.take() {
            self.conflict = Some(conflict);
        }
        res.hosts
    }

    fn resolve_callable_value(
        &mut self,
        ir: &Arc<FunctionIr>,
        val: ValueId,
        hint: Option<VertexId>,
        call_stack: &[IrCallFrame],
    ) -> (Vec<ResolvedEntryFn>, Vec<GapItem>) {
        let mut extra_files = self.extra_field_files.clone();
        extra_files.insert(ir.id.file);
        let mut resolver = IrInstanceResolver::new(
            self.project_files,
            self.allowed_files,
            extra_files,
            self.semantic,
            self.syntax,
            self.ir_provider,
            self.summary_index,
            self.cached_call_graph,
            self.owned_graph,
            self.cached_field_index,
            &mut self.owned_field_index,
            true,
        );
        let mut active = Vec::new();
        let res = resolver.trace_value(ir, val, hint, call_stack, 0, &mut active);
        if let Some(conflict) = resolver.conflict.take() {
            self.conflict = Some(conflict);
            return (Vec::new(), Vec::new());
        }
        let mut entries = Vec::new();
        let mut seen = FxHashSet::default();
        for host in res.hosts {
            if let Some(fn_syn) = host.closure_fn
                && let Ok(callee_ir) = self.ir_provider.function_ir(fn_syn)
                && seen.insert((callee_ir.id, host.captured_stack.len()))
            {
                entries.push(ResolvedEntryFn {
                    ir: callee_ir,
                    captured_stack: host.captured_stack,
                });
            }
        }
        (entries, res.gaps)
    }

    fn lookup_calls_summary_with_id(
        &mut self,
        call_syntax: SyntaxId,
    ) -> Option<(
        DeclarationIdentity,
        MatchedSummaryAspect<tssae_common::CallAspectSummary>,
    )> {
        let mut syns = vec![call_syntax];
        if let Some(decl_syn) = self
            .semantic
            .resolve_declaration(call_syntax)
            .and_then(|d| d.primary_syntax)
            && !syns.contains(&decl_syn)
        {
            syns.push(decl_syn);
        }
        for syn in syns {
            let Some(identity) = self.semantic.declaration_identity(syn) else {
                continue;
            };
            match self.summary_index.resolve_calls_summary(&identity, false) {
                Ok(Some(matched)) => return Some((identity, matched)),
                Ok(None) => {}
                Err(conflict) => {
                    self.conflict = Some(conflict);
                    return None;
                }
            }
        }
        None
    }

    fn lookup_effects_summary(
        &mut self,
        call_syntax: SyntaxId,
        call_kind: IrCallKind,
    ) -> Option<MatchedSummaryAspect<EffectAspectSummary>> {
        let mut syns = Vec::new();
        if !matches!(call_kind, IrCallKind::ImplicitDispose { .. }) {
            syns.push(call_syntax);
        }
        if let Some(decl_syn) = self
            .semantic
            .resolve_declaration(call_syntax)
            .and_then(|d| d.primary_syntax)
            && !syns.contains(&decl_syn)
        {
            syns.push(decl_syn);
        }
        if matches!(
            call_kind,
            IrCallKind::ImplicitDispose { .. } | IrCallKind::ImplicitSetter
        ) {
            let res = resolve_call_targets_with_summaries(
                call_syntax,
                self.semantic,
                self.syntax,
                self.ir_provider,
                self.summary_index,
            );
            if let Some(conflict) = res.conflict {
                self.conflict = Some(conflict);
                return None;
            }
            for t in res.targets {
                if !syns.contains(&t.declaration_syntax) {
                    syns.push(t.declaration_syntax);
                }
            }
        }
        for syn in syns {
            let Some(identity) = self.semantic.declaration_identity(syn) else {
                continue;
            };
            match self.summary_index.resolve_effects_summary(&identity, false) {
                Ok(Some(matched)) => return Some(matched),
                Ok(None) => {}
                Err(conflict) => {
                    self.conflict = Some(conflict);
                    return None;
                }
            }
        }
        None
    }

    fn lookup_prop_write_effects_summary(
        &mut self,
        write_syntax: SyntaxId,
    ) -> Option<MatchedSummaryAspect<EffectAspectSummary>> {
        let mut syns = vec![write_syntax];
        if let Some(parts) = self.semantic.write_syntax_info(write_syntax)
            && !syns.contains(&parts.target_syntax)
        {
            syns.push(parts.target_syntax);
        }
        for syn in syns {
            let Some(identity) = self.semantic.declaration_identity(syn) else {
                continue;
            };
            match self.summary_index.resolve_effects_summary(&identity, false) {
                Ok(Some(matched)) => return Some(matched),
                Ok(None) => {}
                Err(conflict) => {
                    self.conflict = Some(conflict);
                    return None;
                }
            }
        }
        None
    }
}

fn container_bit(registry: &mut Vec<String>, ckey: &str) -> usize {
    if let Some(pos) = registry.iter().position(|k| k == ckey) {
        return pos.min(MAX_CONTAINER_BITS - 1);
    }
    let idx = registry.len();
    registry.push(ckey.to_string());
    idx.min(MAX_CONTAINER_BITS - 1)
}

fn ir_contains_syntax(ir: &FunctionIr, syn: SyntaxId) -> bool {
    ir.vertices.iter().any(|v| v.syntax == Some(syn))
}

fn compute_explicit_reachable_vertices(ir: &FunctionIr) -> Vec<bool> {
    let mut visited = vec![false; ir.vertices.len()];
    let mut seen_states = FxHashSet::default();
    let mut stacks = StackArena::new();
    let mut queue = VecDeque::new();
    let entry_idx = usize::try_from(ir.entry.0).unwrap_or(usize::MAX);
    if entry_idx < visited.len() {
        visited[entry_idx] = true;
        seen_states.insert((ir.entry, StackId(0)));
        queue.push_back((ir.entry, StackId(0)));
    }
    while let Some((u, stack)) = queue.pop_front() {
        let Some(vert) = ir.vertices.get(usize::try_from(u.0).unwrap_or(usize::MAX)) else {
            continue;
        };
        for edge in &vert.explicit_edges {
            let Some(next_stack) = stacks.transition_explicit(ir, stack, edge) else {
                continue;
            };
            let n_idx = usize::try_from(edge.target.0).unwrap_or(usize::MAX);
            if n_idx < visited.len() {
                visited[n_idx] = true;
                if seen_states.insert((edge.target, next_stack)) {
                    queue.push_back((edge.target, next_stack));
                }
            }
        }
    }
    visited
}

fn slot_val_in_call(ir: &FunctionIr, vid: VertexId, slot: &SummarySlotRef) -> Option<ValueId> {
    let v = ir
        .vertices
        .get(usize::try_from(vid.0).unwrap_or(usize::MAX))?;
    match &v.op {
        IrOp::Call { receiver, args, .. } => match slot {
            SummarySlotRef::Receiver(_) => *receiver,
            SummarySlotRef::Arg { arg_index } => args.get(*arg_index).copied().map(arg_value_id),
        },
        _ => None,
    }
}

const fn arg_value_id(arg: IrCallArg) -> ValueId {
    match arg {
        IrCallArg::Positional(v) | IrCallArg::Spread(v) => v,
    }
}

const fn resource_op_kind_str(op: &ResourceOpSpec) -> &'static str {
    match op {
        ResourceOpSpec::DisposesTarget { .. } => "disposes-target",
        ResourceOpSpec::ReplacesPreviousValue { .. } => "replaces-previous-value",
        ResourceOpSpec::RegistersIntoContainer { .. } => "registers-into-container",
        ResourceOpSpec::DetachesFromContainer { .. } => "detaches-from-container",
    }
}

fn describe_identity(identity: &DeclarationIdentity) -> String {
    identity
        .member_paths
        .first()
        .map_or_else(|| "<symbol>".to_string(), |p| p.join("."))
}

fn reconstruct_path_steps(
    ir: &FunctionIr,
    nodes: &[CfgExploreNode],
    mut idx: usize,
    fallback_loc: &DiagnosticLocation,
) -> Vec<ControlFlowPathStep> {
    let mut raw = Vec::new();
    while let Some((parent_idx, from_vid, step_kind)) = &nodes[idx].parent {
        raw.push((*from_vid, step_kind.clone()));
        idx = *parent_idx;
    }
    raw.reverse();

    let mut steps = Vec::new();
    for (from_vid, step_kind) in raw {
        let loc = ir
            .vertices
            .get(usize::try_from(from_vid.0).unwrap_or(usize::MAX))
            .and_then(|v| v.location.clone());
        match step_kind {
            CfgStepKind::Alloc => {
                if let Some(l) = loc {
                    steps.push(ControlFlowPathStep {
                        location: l,
                        description: "Acquired target resource instance".to_string(),
                    });
                }
            }
            CfgStepKind::MayThrow(reason) => {
                if let Some(l) = loc {
                    steps.push(ControlFlowPathStep {
                        location: l,
                        description: may_throw_step_desc(reason).to_string(),
                    });
                }
            }
            CfgStepKind::Explicit(ExplicitEdgeKind::Branch(branch_id)) => {
                if let Some(branch) = ir
                    .branches
                    .get(usize::try_from(branch_id.0).unwrap_or(usize::MAX))
                {
                    let dir = branch.direction.as_str();
                    steps.push(ControlFlowPathStep {
                        location: branch.location.clone(),
                        description: format!("Take {dir} branch of \"{}\"", branch.condition_text),
                    });
                }
            }
            CfgStepKind::Explicit(ExplicitEdgeKind::ExplicitThrow) => {
                if let Some(l) = loc {
                    steps.push(ControlFlowPathStep {
                        location: l,
                        description: "Explicit throw bypasses remaining cleanup".to_string(),
                    });
                }
            }
            CfgStepKind::Explicit(ExplicitEdgeKind::Return) => {
                if let Some(l) = loc {
                    steps.push(ControlFlowPathStep {
                        location: l,
                        description: "Return exits scope without cleaning instance".to_string(),
                    });
                }
            }
            CfgStepKind::Explicit(ExplicitEdgeKind::Break { .. }) => {
                if let Some(l) = loc {
                    steps.push(ControlFlowPathStep {
                        location: l,
                        description: "Break transfers control out of block".to_string(),
                    });
                }
            }
            CfgStepKind::Explicit(ExplicitEdgeKind::SwitchNoMatch { switch_location }) => {
                steps.push(ControlFlowPathStep {
                    location: switch_location,
                    description: "Switch has no default clause; unmatched value bypasses cleanup"
                        .to_string(),
                });
            }
            CfgStepKind::Cleanup | CfgStepKind::HelperUncleaned | CfgStepKind::Explicit(_) => {}
        }
    }

    if steps.is_empty() {
        steps.push(ControlFlowPathStep {
            location: fallback_loc.clone(),
            description: "Control flow exits scope without disposing target instance".to_string(),
        });
    } else {
        steps.push(ControlFlowPathStep {
            location: fallback_loc.clone(),
            description: "Scope exits with target instance still uncleaned".to_string(),
        });
    }
    steps
}

const fn may_throw_step_desc(reason: MayThrowReason) -> &'static str {
    match reason {
        MayThrowReason::Call => "Call expression may throw an implicit exception before cleanup",
        MayThrowReason::Construct => {
            "Constructor call may throw an implicit exception before cleanup"
        }
        MayThrowReason::PropertyAccess => {
            "Property access may throw an implicit exception before cleanup"
        }
        MayThrowReason::Await => "Await expression may reject before cleanup",
        MayThrowReason::Yield => "Yield expression may throw before cleanup",
        MayThrowReason::DynamicOperator => "Dynamic operator may throw before cleanup",
        MayThrowReason::IteratorStep => "Iteration step may throw before cleanup",
    }
}

fn collect_gaps_on_chain(
    nodes: &[CfgExploreNode],
    mut idx: usize,
    vertex_gaps: &[Option<GapItem>],
    helper_summaries: &[Option<FunctionTargetSummary>],
    out: &mut Vec<GapItem>,
) {
    while let Some((parent_idx, from_vid, _)) = &nodes[idx].parent {
        let v_idx = usize::try_from(from_vid.0).unwrap_or(usize::MAX);
        if let Some(g) = vertex_gaps.get(v_idx).and_then(|o| o.as_ref()) {
            push_unique_gap(out, g.clone());
        }
        if let Some(hs) = helper_summaries.get(v_idx).and_then(|o| o.as_ref()) {
            for g in &hs.explicit_gaps {
                push_unique_gap(out, g.clone());
            }
        }
        idx = *parent_idx;
    }
}

fn collect_may_throw_sites_on_chain(
    ir: &FunctionIr,
    nodes: &[CfgExploreNode],
    mut idx: usize,
    helper_summaries: &[Option<FunctionTargetSummary>],
    out: &mut Vec<DiagnosticLocation>,
) {
    while let Some((parent_idx, from_vid, step_kind)) = &nodes[idx].parent {
        if matches!(step_kind, CfgStepKind::MayThrow(_)) {
            let v_idx = usize::try_from(from_vid.0).unwrap_or(usize::MAX);
            if let Some(hs) = helper_summaries.get(v_idx).and_then(|o| o.as_ref())
                && !hs.implicit_may_throw_sites.is_empty()
            {
                for loc in &hs.implicit_may_throw_sites {
                    if !out.contains(loc) {
                        out.push(loc.clone());
                    }
                }
            } else if let Some(loc) = ir.vertices.get(v_idx).and_then(|v| v.location.clone())
                && !out.contains(&loc)
            {
                out.push(loc);
            }
        }
        idx = *parent_idx;
    }
}

fn dedup_sites(sites: Vec<CleanupSiteData>) -> Vec<CleanupSiteData> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for site in sites {
        let key = (
            site.origin.instance_key.clone(),
            site.operation_kind.clone(),
            site.location.file.clone(),
            site.location.start,
            site.location.end,
        );
        if seen.insert(key) {
            out.push(site);
        }
    }
    out
}

fn gaps_to_evidence(gaps: &[GapItem]) -> Vec<EvidenceItem> {
    gaps.iter()
        .map(|g| EvidenceItem::simple("note", g.message.clone(), g.at.clone()))
        .collect()
}

fn push_unique_gap(gaps: &mut Vec<GapItem>, gap: GapItem) {
    let dup = gaps.iter().any(|g| {
        g.kind == gap.kind
            && g.message == gap.message
            && g.at.as_ref().map(|a| (&a.file, a.start, a.end))
                == gap.at.as_ref().map(|a| (&a.file, a.start, a.end))
    });
    if !dup {
        gaps.push(gap);
    }
}

fn push_unique_evidence(evidence: &mut Vec<EvidenceItem>, item: EvidenceItem) {
    if !evidence.contains(&item) {
        evidence.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashMap;
    use tssae_common::{
        BehaviorSummaryEntry, BehaviorSummaryPack, DeclarationSummaryData, FinallyId,
        ImplementationFact, LocalBindingId, MemberTargetHierarchy, SignatureId,
        SummaryTargetMatcher, SymbolId, TaggedSummaryPack, TypeId, TypeSummaryData,
    };

    use crate::ir::{
        AssignOp, BindingMutability, DeclarationSemantics, ExplicitEdge, IrLocalBinding,
        MayThrowEdge,
    };

    struct CleanupTestEnv {
        irs: FxHashMap<SyntaxId, Arc<FunctionIr>>,
        identities: FxHashMap<SyntaxId, DeclarationIdentity>,
        locs: FxHashMap<SyntaxId, DiagnosticLocation>,
        texts: FxHashMap<SyntaxId, String>,
        decls: FxHashMap<SyntaxId, DeclarationSummaryData>,
    }

    impl CleanupTestEnv {
        fn new() -> Self {
            Self {
                irs: FxHashMap::default(),
                identities: FxHashMap::default(),
                locs: FxHashMap::default(),
                texts: FxHashMap::default(),
                decls: FxHashMap::default(),
            }
        }
    }

    impl SyntaxCatalog for CleanupTestEnv {
        fn syntax_location(&self, syntax: SyntaxId) -> Option<DiagnosticLocation> {
            self.locs.get(&syntax).cloned()
        }
        fn syntax_text(&self, syntax: SyntaxId) -> Option<String> {
            self.texts.get(&syntax).cloned()
        }
        fn syntax_kind(&self, _syntax: SyntaxId) -> Option<String> {
            None
        }
        fn file_path(&self, _file: FileId) -> Option<&str> {
            Some("src/app.ts")
        }
    }

    impl SemanticModel for CleanupTestEnv {
        fn symbol_at_syntax(&self, _syntax: SyntaxId) -> Option<SymbolId> {
            None
        }
        fn type_at_syntax(&self, _syntax: SyntaxId) -> Option<TypeSummaryData> {
            None
        }
        fn signature_at_syntax(&self, _syntax: SyntaxId) -> Option<SignatureId> {
            None
        }
        fn resolve_declaration(&self, syntax: SyntaxId) -> Option<DeclarationSummaryData> {
            self.decls.get(&syntax).cloned()
        }
        fn resolve_declaration_by_symbol(
            &self,
            _symbol: SymbolId,
        ) -> Option<DeclarationSummaryData> {
            None
        }
        fn is_assignable_to(&self, _source: TypeId, _target: TypeId) -> bool {
            false
        }
        fn base_types(&self, _ty: TypeId) -> Vec<TypeId> {
            Vec::new()
        }
        fn is_external_syntax(&self, _syntax: SyntaxId) -> bool {
            false
        }
        fn member_target_hierarchy(
            &self,
            _receiver_syntax: SyntaxId,
            _member_name: Option<&str>,
            _static_receiver: bool,
        ) -> Option<MemberTargetHierarchy> {
            None
        }
        fn class_hierarchy_for_symbol(
            &self,
            _class_symbol: SymbolId,
        ) -> Option<MemberTargetHierarchy> {
            None
        }
        fn resolve_class_constructor(
            &self,
            _class_syntax: SyntaxId,
        ) -> Option<DeclarationSummaryData> {
            None
        }
        fn resolve_super_target(&self, _call_syntax: SyntaxId) -> Option<DeclarationSummaryData> {
            None
        }
        fn resolve_implementation_for_syntax(
            &self,
            _decl_or_node_syntax: SyntaxId,
            _call_site_syntax: Option<SyntaxId>,
        ) -> ImplementationFact {
            ImplementationFact::External {
                package_name: None,
                declaration_file: None,
                evidence: Vec::new(),
            }
        }
        fn check_call_missing_package_gap(&self, _call_syntax: SyntaxId) -> Option<GapItem> {
            None
        }
        fn declaration_identity(&self, syntax: SyntaxId) -> Option<DeclarationIdentity> {
            self.identities.get(&syntax).cloned()
        }
        fn record_summary_lookup(&self, _source: Option<SummarySource>) {}
    }

    impl IrProvider for CleanupTestEnv {
        fn function_ir(&self, scope_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
            self.irs
                .get(&scope_syntax)
                .cloned()
                .ok_or_else(|| GapItem::new("missing-function-body", "missing".to_string(), None))
        }
        fn enclosing_scope_ir(&self, _node_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
            Err(GapItem::new(
                "missing-function-body",
                "missing".to_string(),
                None,
            ))
        }
        fn file_scope_ir(&self, _file: FileId) -> Option<Arc<FunctionIr>> {
            None
        }
        fn scopes_in_file(&self, _file: FileId) -> Vec<crate::ir::ScopeCatalogEntry> {
            Vec::new()
        }
    }

    fn syn(id: u32) -> SyntaxId {
        SyntaxId::from_parts(FileId(0), tssae_common::LocalSyntaxId(id))
    }

    fn loc(line: u32, start: u32, end: u32) -> DiagnosticLocation {
        DiagnosticLocation {
            file: "src/app.ts".to_string(),
            line,
            column: 1,
            end_line: line,
            end_column: 10,
            start,
            end,
        }
    }

    fn summary_index_for_tests() -> BehaviorSummaryIndex {
        let pack = BehaviorSummaryPack {
            id: "test-cleanup-pack".to_string(),
            version: None,
            description: None,
            entries: vec![
                BehaviorSummaryEntry {
                    id: Some("acquire".to_string()),
                    matcher: SummaryTargetMatcher {
                        package: None,
                        version: None,
                        export_path: None,
                        module: Some("src/app.ts".to_string()),
                        member_path: vec!["acquire".to_string()],
                    },
                    description: None,
                    calls: None,
                    values: None,
                    instances: Some(tssae_common::InstanceAspectSummary {
                        completeness: SummaryCompleteness::Complete,
                        outcome: tssae_common::InstanceOutcome::AllocatesFreshInstance,
                    }),
                    effects: None,
                },
                BehaviorSummaryEntry {
                    id: Some("release".to_string()),
                    matcher: SummaryTargetMatcher {
                        package: None,
                        version: None,
                        export_path: None,
                        module: Some("src/app.ts".to_string()),
                        member_path: vec!["release".to_string()],
                    },
                    description: None,
                    calls: None,
                    values: None,
                    instances: None,
                    effects: Some(EffectAspectSummary {
                        completeness: SummaryCompleteness::Complete,
                        resource_ops: vec![ResourceOpSpec::DisposesTarget {
                            target: SummarySlotRef::Arg { arg_index: 0 },
                        }],
                        mutates_receiver: None,
                    }),
                },
            ],
        };
        BehaviorSummaryIndex::from_config(
            &tssae_common::SummaryConfigPayload {
                packs: vec![TaggedSummaryPack {
                    source: SummarySource::Local,
                    origin_file: None,
                    pack,
                }],
                enable_builtin: true,
            },
            &FxHashMap::default(),
        )
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "Builds acquire->write->release and try/finally CFG fixtures to verify dual tri-state verdicts."
    )]
    fn acquire_write_release_and_try_finally_produce_expected_dual_tristate() {
        let mut env = CleanupTestEnv::new();
        let fn_unguarded = syn(1);
        let fn_finally = syn(2);
        let acq_syn = syn(10);
        let write_syn = syn(11);
        let rel_syn = syn(12);
        let r_syn = syn(13);

        env.locs.insert(acq_syn, loc(2, 10, 19));
        env.locs.insert(write_syn, loc(3, 20, 28));
        env.locs.insert(rel_syn, loc(4, 30, 40));

        env.identities.insert(
            acq_syn,
            DeclarationIdentity {
                package_name: None,
                package_version: None,
                export_path: None,
                module_path: Some("src/app.ts".to_string()),
                is_default_library: false,
                member_paths: vec![vec!["acquire".to_string()]],
                is_instance_member: false,
            },
        );
        env.identities.insert(
            rel_syn,
            DeclarationIdentity {
                package_name: None,
                package_version: None,
                export_path: None,
                module_path: Some("src/app.ts".to_string()),
                is_default_library: false,
                member_paths: vec![vec!["release".to_string()]],
                is_instance_member: false,
            },
        );
        env.identities.insert(
            write_syn,
            DeclarationIdentity {
                package_name: None,
                package_version: None,
                export_path: None,
                module_path: Some("src/app.ts".to_string()),
                is_default_library: false,
                member_paths: vec![vec!["write".to_string()]],
                is_instance_member: false,
            },
        );

        let build_ir = |root: SyntaxId, with_finally: bool| -> Arc<FunctionIr> {
            // 0: entry -> 1: acquire() -> 2: assign r -> 3: write(r) -> 4: release(r) -> 5: normal_exit -> 6: exit
            // 7: exceptional_exit -> 6: exit
            // If with_finally: 3.may_throw -> 8 (FinallyEnter(0, 7)) -> 4 (in finally 0) -> 9 (FinallyResume(0, 7)) -> 7
            let mut vertices = vec![
                CfgVertex {
                    id: VertexId(0),
                    syntax: Some(root),
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: Some(loc(1, 0, 50)),
                    value: None,
                    op: IrOp::Entry,
                    explicit_edges: vec![ExplicitEdge {
                        target: VertexId(1),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                    may_throw_edge: None,
                    enclosing_finallies: Vec::new(),
                },
                CfgVertex {
                    id: VertexId(1),
                    syntax: Some(acq_syn),
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: Some(loc(2, 10, 19)),
                    value: Some(ValueId(0)),
                    op: IrOp::Call {
                        call_kind: IrCallKind::Explicit,
                        callee: ValueId(10),
                        receiver: None,
                        args: Vec::new(),
                    },
                    explicit_edges: vec![ExplicitEdge {
                        target: VertexId(2),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                    may_throw_edge: Some(MayThrowEdge {
                        target: VertexId(7),
                        site_syntax: acq_syn,
                        reason: MayThrowReason::Call,
                    }),
                    enclosing_finallies: Vec::new(),
                },
                CfgVertex {
                    id: VertexId(2),
                    syntax: Some(r_syn),
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: Some(loc(2, 6, 7)),
                    value: Some(ValueId(1)),
                    op: IrOp::AssignLocal {
                        target: LocalBindingId(0),
                        op: AssignOp::Assign,
                        value: ValueId(0),
                        is_init: true,
                    },
                    explicit_edges: vec![ExplicitEdge {
                        target: if with_finally {
                            VertexId(8)
                        } else {
                            VertexId(3)
                        },
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                    may_throw_edge: None,
                    enclosing_finallies: Vec::new(),
                },
                CfgVertex {
                    id: VertexId(3),
                    syntax: Some(write_syn),
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: Some(loc(3, 20, 28)),
                    value: Some(ValueId(2)),
                    op: IrOp::Call {
                        call_kind: IrCallKind::Explicit,
                        callee: ValueId(11),
                        receiver: None,
                        args: vec![IrCallArg::Positional(ValueId(0))],
                    },
                    explicit_edges: vec![ExplicitEdge {
                        target: if with_finally {
                            VertexId(9)
                        } else {
                            VertexId(4)
                        },
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                    may_throw_edge: Some(MayThrowEdge {
                        target: if with_finally {
                            VertexId(10)
                        } else {
                            VertexId(7)
                        },
                        site_syntax: write_syn,
                        reason: MayThrowReason::Call,
                    }),
                    enclosing_finallies: if with_finally {
                        vec![FinallyId(0)]
                    } else {
                        Vec::new()
                    },
                },
                CfgVertex {
                    id: VertexId(4),
                    syntax: Some(rel_syn),
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: Some(loc(4, 30, 40)),
                    value: Some(ValueId(3)),
                    op: IrOp::Call {
                        call_kind: IrCallKind::Explicit,
                        callee: ValueId(12),
                        receiver: None,
                        args: vec![IrCallArg::Positional(ValueId(0))],
                    },
                    explicit_edges: vec![ExplicitEdge {
                        target: if with_finally {
                            VertexId(11)
                        } else {
                            VertexId(5)
                        },
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                    may_throw_edge: Some(MayThrowEdge {
                        target: VertexId(7),
                        site_syntax: rel_syn,
                        reason: MayThrowReason::Call,
                    }),
                    enclosing_finallies: Vec::new(),
                },
                CfgVertex {
                    id: VertexId(5),
                    syntax: None,
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: None,
                    value: None,
                    op: IrOp::Exit,
                    explicit_edges: vec![ExplicitEdge {
                        target: VertexId(6),
                        kind: ExplicitEdgeKind::ExitMerge,
                    }],
                    may_throw_edge: None,
                    enclosing_finallies: Vec::new(),
                },
                CfgVertex {
                    id: VertexId(6),
                    syntax: None,
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: None,
                    value: None,
                    op: IrOp::Exit,
                    explicit_edges: Vec::new(),
                    may_throw_edge: None,
                    enclosing_finallies: Vec::new(),
                },
                CfgVertex {
                    id: VertexId(7),
                    syntax: None,
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: None,
                    value: None,
                    op: IrOp::Exit,
                    explicit_edges: vec![ExplicitEdge {
                        target: VertexId(6),
                        kind: ExplicitEdgeKind::ExitMerge,
                    }],
                    may_throw_edge: None,
                    enclosing_finallies: Vec::new(),
                },
            ];
            if with_finally {
                // 8: try entry -> 3
                vertices.push(CfgVertex {
                    id: VertexId(8),
                    syntax: None,
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: None,
                    value: None,
                    op: IrOp::NopMerge,
                    explicit_edges: vec![ExplicitEdge {
                        target: VertexId(3),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                    may_throw_edge: None,
                    enclosing_finallies: vec![FinallyId(0)],
                });
                // 9: normal finally trampoline -> 4 (FinallyEnter -> 5)
                vertices.push(CfgVertex {
                    id: VertexId(9),
                    syntax: None,
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: None,
                    value: None,
                    op: IrOp::NopMerge,
                    explicit_edges: vec![ExplicitEdge {
                        target: VertexId(4),
                        kind: ExplicitEdgeKind::FinallyEnter {
                            finally_id: FinallyId(0),
                            resume: VertexId(5),
                        },
                    }],
                    may_throw_edge: None,
                    enclosing_finallies: Vec::new(),
                });
                // 10: exc finally trampoline -> 4 (FinallyEnter -> 7)
                vertices.push(CfgVertex {
                    id: VertexId(10),
                    syntax: None,
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: None,
                    value: None,
                    op: IrOp::NopMerge,
                    explicit_edges: vec![ExplicitEdge {
                        target: VertexId(4),
                        kind: ExplicitEdgeKind::FinallyEnter {
                            finally_id: FinallyId(0),
                            resume: VertexId(7),
                        },
                    }],
                    may_throw_edge: None,
                    enclosing_finallies: Vec::new(),
                });
                // 11: finally exit -> FinallyResume(0, 5) | FinallyResume(0, 7)
                vertices.push(CfgVertex {
                    id: VertexId(11),
                    syntax: None,
                    local_syntax: None,
                    symbol: None,
                    ty: None,
                    signature: None,
                    location: None,
                    value: None,
                    op: IrOp::NopMerge,
                    explicit_edges: vec![
                        ExplicitEdge {
                            target: VertexId(5),
                            kind: ExplicitEdgeKind::FinallyResume {
                                finally_id: FinallyId(0),
                                resume: VertexId(5),
                            },
                        },
                        ExplicitEdge {
                            target: VertexId(7),
                            kind: ExplicitEdgeKind::FinallyResume {
                                finally_id: FinallyId(0),
                                resume: VertexId(7),
                            },
                        },
                    ],
                    may_throw_edge: None,
                    enclosing_finallies: Vec::new(),
                });
            }
            Arc::new(FunctionIr {
                id: FunctionIrId {
                    file: FileId(0),
                    local_scope: root.local_id().0,
                },
                root_syntax: root,
                scope_kind: IrScopeKind::Function,
                scope_location: loc(1, 0, 50),
                decl_semantics: DeclarationSemantics::default(),
                type_guard: None,
                parameters: Vec::new(),
                bindings: vec![IrLocalBinding {
                    id: LocalBindingId(0),
                    name: "r".to_string(),
                    symbol: None,
                    syntax: r_syn,
                    mutability: BindingMutability::Const,
                }],
                captured_bindings: Vec::new(),
                vertices,
                entry: VertexId(0),
                normal_exit: VertexId(5),
                exceptional_exit: VertexId(7),
                exit: VertexId(6),
                syntax_to_vertex: FxHashMap::default(),
                expr_to_vertex: FxHashMap::default(),
                expr_to_value: FxHashMap::default(),
                value_sources: FxHashMap::default(),
                branches: Vec::new(),
                value_count: 13,
            })
        };

        env.irs.insert(fn_unguarded, build_ir(fn_unguarded, false));
        env.irs.insert(fn_finally, build_ir(fn_finally, true));
        let summaries = summary_index_for_tests();

        let target = CleanupTargetSpec {
            instance_key: "summary:test-cleanup-pack:src/app.ts:10..19".to_string(),
            origin_kind: InstanceOriginKind::SummaryAllocation,
            allocation_location: Some(loc(2, 10, 19)),
            origin_syntax: Some(acq_syn),
            summary_id: Some("test-cleanup-pack".to_string()),
            summary_source: Some(SummarySource::Local),
            evidence: Vec::new(),
        };

        let (batch, _, _) =
            evaluate_instance_cleanup_on_ir::<std::collections::hash_map::RandomState>(
                &[
                    CleanupScopeSpec {
                        syntax: fn_unguarded,
                    },
                    CleanupScopeSpec { syntax: fn_finally },
                ],
                &[target],
                &[FileId(0)],
                None,
                &env,
                &env,
                &env,
                &summaries,
                None,
                None,
            );
        assert!(batch.conflict.is_none());

        // 1. acquire -> write -> release: primary Proved, implicit Disproved with write_syn location.
        let res_unguarded = &batch.results[0].target_results[0];
        assert!(matches!(
            res_unguarded.status,
            TargetCleanupStatusData::Proved { .. }
        ));
        match &res_unguarded.implicit_exceptions {
            ImplicitExceptionCleanupOutcome::Disproved {
                may_throw_sites,
                counterexample_path,
                ..
            } => {
                assert_eq!(may_throw_sites, &[loc(3, 20, 28)]);
                assert_ne!(counterexample_path.len(), 0);
            }
            other => panic!("expected Disproved implicitExceptions, got {other:?}"),
        }

        // 2. try { write(r); } finally { release(r); }: both Proved.
        let res_finally = &batch.results[1].target_results[0];
        assert!(matches!(
            res_finally.status,
            TargetCleanupStatusData::Proved { .. }
        ));
        assert!(matches!(
            res_finally.implicit_exceptions,
            ImplicitExceptionCleanupOutcome::Proved { .. }
        ));
    }
}
