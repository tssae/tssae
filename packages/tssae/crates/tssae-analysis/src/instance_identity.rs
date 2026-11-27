use std::{
    collections::{HashSet, VecDeque},
    hash::BuildHasher,
    sync::Arc,
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use tssae_common::{
    DeclarationIdentity, DiagnosticLocation, EvidenceItem, FileId, FunctionIrId, GapItem,
    ImplementationFact, InstanceAspectSummary, InstanceOutcome, LocalBindingId, SemanticModel,
    SummaryCompleteness, SummaryConflictError, SummarySlotRef, SummarySource, SymbolId,
    SyntaxCatalog, SyntaxId, UnknownReason, ValueId, VertexId,
};

use crate::{
    call_resolution::{
        self, IrCallGraph, build_ir_call_graph_with_summaries, prop_key_static_name,
        resolve_call_targets_with_summaries,
    },
    class_overrides::{self, MemberAccessKind},
    ir::{
        BinaryOp, BindingMutability, CfgVertex, ExplicitEdgeKind, FunctionIr, IrArrayElem,
        IrBranchKind, IrCallArg, IrCallKind, IrObjectProp, IrOp, IrPropKey, IrProvider,
        IrScopeKind, LiteralKind,
    },
    summaries::{BehaviorSummaryIndex, MatchedSummaryAspect},
    var_writes::{self, VarWriteSite, VarWriteSource},
};

const MAX_TRACE_DEPTH: usize = 24;
const MAX_CALL_STACK_DEPTH: usize = 12;

/// Target syntax reference and optional canonical symbol passed into [`resolve_instances_on_ir`].
#[derive(Clone, Copy, Debug)]
pub struct InstanceTargetSpec {
    pub syntax: SyntaxId,
    pub symbol: Option<SymbolId>,
}

/// Origin classification for a resolved object instance.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstanceOriginKind {
    Allocation,
    SummaryAllocation,
    Parameter,
}

impl InstanceOriginKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allocation => "allocation",
            Self::SummaryAllocation => "summary-allocation",
            Self::Parameter => "parameter",
        }
    }
}

/// Resolved canonical instance origin metadata returned by [`resolve_instances_on_ir`].
#[derive(Clone, Debug)]
pub struct ResolvedInstanceOrigin {
    pub instance_key: String,
    pub origin_kind: InstanceOriginKind,
    pub allocation_location: Option<DiagnosticLocation>,
    pub origin_syntax: Option<SyntaxId>,
    pub summary_id: Option<String>,
    pub summary_source: Option<SummarySource>,
    pub evidence: Vec<EvidenceItem>,
}

/// Tri-state resolution outcome for a single instance target.
#[derive(Clone, Debug)]
pub enum InstanceResolutionOutcome {
    Proved {
        origins: Vec<ResolvedInstanceOrigin>,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        origins: Vec<ResolvedInstanceOrigin>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

/// Batch outcome for [`resolve_instances_on_ir`] including any fatal same-tier summary conflict.
#[derive(Clone, Debug)]
pub struct ResolveInstancesBatchOutcome {
    pub results: Vec<InstanceResolutionOutcome>,
    pub conflict: Option<SummaryConflictError>,
}

#[derive(Clone)]
pub(crate) struct IrCallFrame {
    pub(crate) call_syntax: SyntaxId,
    pub(crate) call_location: Option<DiagnosticLocation>,
    pub(crate) caller_ir: Arc<FunctionIr>,
    pub(crate) callee_ir: Arc<FunctionIr>,
    pub(crate) receiver: Option<ReceiverBinding>,
    pub(crate) args: Vec<IrCallArg>,
    pub(crate) is_construct: bool,
    pub(crate) closure_env: Vec<Self>,
}

#[derive(Clone)]
pub(crate) enum ReceiverBinding {
    Value(ValueId),
    PreResolved(Vec<HostOrigin>),
}

#[derive(Clone)]
pub(crate) struct HostOrigin {
    pub(crate) origin: ResolvedInstanceOrigin,
    pub(crate) captured_stack: Vec<IrCallFrame>,
    pub(crate) class_syntax: Option<SyntaxId>,
    pub(crate) closure_fn: Option<SyntaxId>,
    pub(crate) host_scope_ir: Option<Arc<FunctionIr>>,
    pub(crate) object_alloc_props: Option<Vec<IrObjectProp>>,
    pub(crate) array_alloc_elems: Option<Vec<IrArrayElem>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NonInstanceKind {
    Nullish,
    FalsyNonNullish,
    TruthyOrUnknownPrimitive,
    SummaryNone,
}

#[derive(Clone, Default)]
pub(crate) struct TraceResult {
    pub(crate) hosts: Vec<HostOrigin>,
    pub(crate) non_instances: Vec<(NonInstanceKind, EvidenceItem)>,
    pub(crate) gaps: Vec<GapItem>,
    pub(crate) evidence: Vec<EvidenceItem>,
    pub(crate) hit_cycle: bool,
}

impl TraceResult {
    fn push_gap(&mut self, gap: GapItem) {
        let dup = self.gaps.iter().any(|g| {
            g.kind == gap.kind
                && g.message == gap.message
                && g.at.as_ref().map(|a| (&a.file, a.start, a.end))
                    == gap.at.as_ref().map(|a| (&a.file, a.start, a.end))
        });
        if !dup {
            self.gaps.push(gap);
        }
    }

    fn push_evidence(&mut self, ev: EvidenceItem) {
        if !self.evidence.iter().any(|e| e == &ev) {
            self.evidence.push(ev);
        }
    }

    fn merge(&mut self, other: Self) {
        for host in other.hosts {
            if !self
                .hosts
                .iter()
                .any(|existing| existing.origin.instance_key == host.origin.instance_key)
            {
                self.hosts.push(host);
            }
        }
        for (kind, ev) in other.non_instances {
            self.push_evidence(ev.clone());
            if !self
                .non_instances
                .iter()
                .any(|(k, e)| k == &kind && e == &ev)
            {
                self.non_instances.push((kind, ev));
            }
        }
        for gap in other.gaps {
            self.push_gap(gap);
        }
        for ev in other.evidence {
            self.push_evidence(ev);
        }
        self.hit_cycle |= other.hit_cycle;
    }

    fn prepend_step(&mut self, step: &EvidenceItem) {
        for host in &mut self.hosts {
            if !host.origin.evidence.contains(step) {
                let mut next = Vec::with_capacity(1 + host.origin.evidence.len());
                next.push(step.clone());
                next.extend(host.origin.evidence.iter().cloned());
                host.origin.evidence = next;
            }
        }
        self.push_evidence(step.clone());
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum VisitKey {
    Value {
        ir_id: FunctionIrId,
        value: ValueId,
        stack_keys: Vec<SyntaxId>,
    },
    PropRead {
        host_key: String,
        prop_name: String,
        is_private: bool,
        read_ir_id: FunctionIrId,
        read_vertex: VertexId,
    },
    Binding {
        ir_id: FunctionIrId,
        binding: LocalBindingId,
        stack_keys: Vec<SyntaxId>,
    },
}

#[derive(Clone)]
struct IndexedPropWrite {
    scope_ir: Arc<FunctionIr>,
    vertex: VertexId,
    object: ValueId,
    is_private: bool,
    rhs: ValueId,
}

#[derive(Default)]
pub struct IrFieldWriteIndex {
    writes_by_prop: FxHashMap<String, Vec<IndexedPropWrite>>,
}

impl IrFieldWriteIndex {
    #[must_use]
    pub fn build(project_files: &[FileId], ir_provider: &dyn IrProvider) -> Self {
        let mut index = Self::default();
        for &file in project_files {
            for scope in ir_provider.scopes_in_file(file) {
                if let Ok(ir) = ir_provider.function_ir(scope.root_syntax) {
                    index.collect_from_ir(&ir);
                }
            }
        }
        index
    }

    fn collect_from_ir(&mut self, ir: &Arc<FunctionIr>) {
        for v in &ir.vertices {
            if let IrOp::PropWrite {
                object, key, value, ..
            } = &v.op
                && let Some((prop_name, is_private)) = static_prop_key(key)
            {
                self.writes_by_prop
                    .entry(prop_name.to_string())
                    .or_default()
                    .push(IndexedPropWrite {
                        scope_ir: Arc::clone(ir),
                        vertex: v.id,
                        object: *object,
                        is_private,
                        rhs: *value,
                    });
            }
        }
    }
}

#[derive(Clone)]
struct TimelineAnchor {
    ir: Arc<FunctionIr>,
    vertex: VertexId,
}

#[derive(Clone)]
struct MatchedPropWrite {
    scope_ir: Arc<FunctionIr>,
    write_vertex: VertexId,
    rhs: ValueId,
    rhs_stack: Vec<IrCallFrame>,
    anchor_in_timeline: Option<(usize, VertexId, bool)>,
}

/// Resolves tri-state instance identities on IR for all `targets`.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Matches trace_value_propagation signature with project files, summary index, and optional cached call graph/field write index."
)]
pub fn resolve_instances_on_ir<S: BuildHasher>(
    targets: &[InstanceTargetSpec],
    project_files: &[FileId],
    allowed_files: Option<&HashSet<FileId, S>>,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    summary_index: &BehaviorSummaryIndex,
    cached_call_graph: Option<&IrCallGraph>,
    cached_field_index: Option<&IrFieldWriteIndex>,
) -> (
    ResolveInstancesBatchOutcome,
    Option<IrCallGraph>,
    Option<IrFieldWriteIndex>,
) {
    let owned_allowed: Option<FxHashSet<FileId>> =
        allowed_files.map(|set| set.iter().copied().collect());
    let extra_field_files: FxHashSet<FileId> = targets.iter().map(|t| t.syntax.file_id()).collect();
    let mut owned_graph: Option<IrCallGraph> = None;
    let mut owned_field_index: Option<IrFieldWriteIndex> = None;
    let mut resolver = IrInstanceResolver {
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
        owned_field_index: &mut owned_field_index,
        allow_unanchored_caller_lookup: true,
        call_targets_cache: CallTargetsCache::Owned(FxHashMap::default()),
        conflict: None,
    };

    let mut results = Vec::with_capacity(targets.len());
    for target in targets {
        if resolver.conflict.is_some() {
            break;
        }
        results.push(resolver.resolve_target(*target));
    }

    let conflict = resolver.conflict.take();
    drop(resolver);

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
        ResolveInstancesBatchOutcome { results, conflict },
        ret_graph,
        ret_field_index,
    )
}

enum CallTargetsCache<'a> {
    Owned(FxHashMap<SyntaxId, call_resolution::CallTargetsOutcome>),
    Shared(&'a mut FxHashMap<SyntaxId, call_resolution::CallTargetsOutcome>),
}

impl CallTargetsCache<'_> {
    fn get(&self, key: SyntaxId) -> Option<&call_resolution::CallTargetsOutcome> {
        match self {
            Self::Owned(map) => map.get(&key),
            Self::Shared(map) => map.get(&key),
        }
    }

    fn insert(&mut self, key: SyntaxId, val: call_resolution::CallTargetsOutcome) {
        match self {
            Self::Owned(map) => {
                map.insert(key, val);
            }
            Self::Shared(map) => {
                map.insert(key, val);
            }
        }
    }
}

pub(crate) struct IrInstanceResolver<'a> {
    pub(crate) project_files: &'a [FileId],
    pub(crate) allowed_files: Option<&'a FxHashSet<FileId>>,
    extra_field_files: FxHashSet<FileId>,
    pub(crate) semantic: &'a dyn SemanticModel,
    pub(crate) syntax: &'a dyn SyntaxCatalog,
    pub(crate) ir_provider: &'a dyn IrProvider,
    pub(crate) summary_index: &'a BehaviorSummaryIndex,
    pub(crate) cached_call_graph: Option<&'a IrCallGraph>,
    pub(crate) owned_graph: &'a mut Option<IrCallGraph>,
    cached_field_index: Option<&'a IrFieldWriteIndex>,
    owned_field_index: &'a mut Option<IrFieldWriteIndex>,
    allow_unanchored_caller_lookup: bool,
    call_targets_cache: CallTargetsCache<'a>,
    pub(crate) conflict: Option<SummaryConflictError>,
}

impl<'a> IrInstanceResolver<'a> {
    #[expect(
        clippy::too_many_arguments,
        reason = "Constructs internal IR instance resolver from snapshot IR environment and optional call graph/field write caches."
    )]
    pub(crate) fn new(
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
        owned_field_index: &'a mut Option<IrFieldWriteIndex>,
        allow_unanchored_caller_lookup: bool,
    ) -> Self {
        Self {
            project_files,
            allowed_files,
            extra_field_files,
            semantic,
            syntax,
            ir_provider,
            summary_index,
            cached_call_graph,
            owned_graph,
            cached_field_index,
            owned_field_index,
            allow_unanchored_caller_lookup,
            call_targets_cache: CallTargetsCache::Owned(FxHashMap::default()),
            conflict: None,
        }
    }

    pub(crate) fn with_call_targets_cache(
        mut self,
        cache: &'a mut FxHashMap<SyntaxId, call_resolution::CallTargetsOutcome>,
    ) -> Self {
        self.call_targets_cache = CallTargetsCache::Shared(cache);
        self
    }

    fn resolve_call_targets_cached(
        &mut self,
        call_syntax: SyntaxId,
    ) -> call_resolution::CallTargetsOutcome {
        if let Some(cached) = self.call_targets_cache.get(call_syntax) {
            return cached.clone();
        }
        let outcome = resolve_call_targets_with_summaries(
            call_syntax,
            self.semantic,
            self.syntax,
            self.ir_provider,
            self.summary_index,
        );
        self.call_targets_cache.insert(call_syntax, outcome.clone());
        outcome
    }
    fn call_graph(&mut self) -> Option<&IrCallGraph> {
        if let Some(g) = self.cached_call_graph {
            return Some(g);
        }
        if self.owned_graph.is_none() {
            let scoped_files: Vec<FileId>;
            let files: &[FileId] = if let Some(allowed) = self.allowed_files {
                scoped_files = self
                    .project_files
                    .iter()
                    .copied()
                    .filter(|f| allowed.contains(f))
                    .collect();
                &scoped_files
            } else {
                self.project_files
            };
            let graph = build_ir_call_graph_with_summaries(
                files,
                self.semantic,
                self.syntax,
                self.ir_provider,
                self.summary_index,
            );
            if let Some(conflict) = &graph.conflict {
                self.conflict = Some(conflict.clone());
            }
            *self.owned_graph = Some(graph);
        }
        self.owned_graph.as_ref()
    }

    fn is_file_allowed_for_fields(&self, file: FileId) -> bool {
        self.allowed_files
            .is_none_or(|allowed| allowed.contains(&file) || self.extra_field_files.contains(&file))
    }

    fn ensure_field_index(&mut self) -> &IrFieldWriteIndex {
        if let Some(idx) = self.cached_field_index {
            return idx;
        }
        if self.owned_field_index.is_none() {
            let scoped_files: Vec<FileId>;
            let files: &[FileId] = if let Some(allowed) = self.allowed_files {
                scoped_files = self
                    .project_files
                    .iter()
                    .copied()
                    .filter(|f| allowed.contains(f) || self.extra_field_files.contains(f))
                    .collect();
                &scoped_files
            } else {
                self.project_files
            };
            *self.owned_field_index = Some(IrFieldWriteIndex::build(files, self.ir_provider));
        }
        self.owned_field_index
            .as_ref()
            .expect("field index initialized")
    }

    fn resolve_target(&mut self, target: InstanceTargetSpec) -> InstanceResolutionOutcome {
        let target_loc = self.syntax.syntax_location(target.syntax);

        let Ok(ir) = self.ir_provider.enclosing_scope_ir(target.syntax) else {
            return self.resolve_external_or_unscoped_target(target, target_loc);
        };

        let mut active = Vec::new();
        let mut res = self.trace_target_in_scope(&ir, target, &mut active);

        if res.hosts.is_empty() && res.gaps.is_empty() && res.non_instances.is_empty() {
            if res.hit_cycle {
                res.push_gap(GapItem::from_reason(
                    UnknownReason::UnmodeledConstruct,
                    "Instance identity trace terminated at a recursive self-reference cycle."
                        .to_string(),
                    target_loc,
                ));
            } else {
                res.push_gap(GapItem::from_reason(
                    UnknownReason::UnresolvedDeclaration,
                    "Expression could not be resolved to an instance allocation or non-instance value."
                        .to_string(),
                    target_loc,
                ));
            }
        }

        let mut all_evidence = res.evidence;
        for host in &res.hosts {
            for ev in &host.origin.evidence {
                if !all_evidence.contains(ev) {
                    all_evidence.push(ev.clone());
                }
            }
        }

        let origins: Vec<ResolvedInstanceOrigin> =
            res.hosts.into_iter().map(|h| h.origin).collect();

        if !res.gaps.is_empty() {
            return InstanceResolutionOutcome::Inconclusive {
                origins,
                gaps: res.gaps,
                evidence: all_evidence,
            };
        }

        if origins.is_empty() {
            return InstanceResolutionOutcome::Disproved {
                evidence: all_evidence,
            };
        }

        InstanceResolutionOutcome::Proved {
            origins,
            evidence: all_evidence,
        }
    }

    fn resolve_external_or_unscoped_target(
        &mut self,
        target: InstanceTargetSpec,
        target_loc: Option<DiagnosticLocation>,
    ) -> InstanceResolutionOutcome {
        if let Some(decl) = self.semantic.resolve_declaration(target.syntax) {
            if let Some(init_syn) = decl.initializer_syntax
                && init_syn != target.syntax
            {
                return self.resolve_target(InstanceTargetSpec {
                    syntax: init_syn,
                    symbol: decl.symbol,
                });
            }
            if decl.is_external || decl.is_default_library {
                let label = decl.name.as_deref().unwrap_or("external declaration");
                return InstanceResolutionOutcome::Inconclusive {
                    origins: Vec::new(),
                    gaps: vec![GapItem::from_reason(
                        UnknownReason::MissingFunctionBody,
                        format!(
                            "Target '{label}' resides in an external declaration without an analyzable source implementation."
                        ),
                        target_loc.or(decl.location),
                    )],
                    evidence: Vec::new(),
                };
            }
        }

        InstanceResolutionOutcome::Inconclusive {
            origins: Vec::new(),
            gaps: vec![GapItem::from_reason(
                UnknownReason::NoEnclosingFunction,
                "Target syntax has no enclosing function or module scope.".to_string(),
                target_loc,
            )],
            evidence: Vec::new(),
        }
    }

    fn trace_target_in_scope(
        &mut self,
        ir: &Arc<FunctionIr>,
        target: InstanceTargetSpec,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        if target.syntax.file_id() == ir.root_syntax.file_id()
            && let Some(&val) = ir.expr_to_value.get(&target.syntax.local_id())
        {
            let read_v = ir.expr_to_vertex.get(&target.syntax.local_id()).copied();
            return self.trace_value(ir, val, read_v, &[], 0, active);
        }

        if let Some(param_idx) = ir.parameters.iter().position(|p| p.syntax == target.syntax) {
            return self.resolve_parameter(ir, param_idx, &[], 0, active);
        }

        if let Some(binding) = ir.bindings.iter().find(|b| {
            b.syntax == target.syntax
                || self
                    .semantic
                    .resolve_declaration(b.syntax)
                    .and_then(|d| d.primary_syntax)
                    == Some(target.syntax)
        }) {
            let anchor_v = ir
                .syntax_to_vertex
                .get(&target.syntax)
                .or_else(|| ir.syntax_to_vertex.get(&binding.syntax))
                .copied()
                .unwrap_or(ir.normal_exit);
            return self.trace_local_binding(ir, binding.id, anchor_v, &[], 0, active);
        }

        if ir.root_syntax == target.syntax
            && !matches!(
                ir.scope_kind,
                IrScopeKind::ModuleTopLevel | IrScopeKind::LocalBlock
            )
        {
            let mut out = TraceResult::default();
            out.hosts.push(self.make_closure_host(ir.root_syntax, &[]));
            return out;
        }

        if let Some(decl) = self.semantic.resolve_declaration(target.syntax) {
            if let Some(init_syn) = decl.initializer_syntax
                && init_syn != target.syntax
            {
                return self.resolve_syntax_in_any_scope(init_syn, &[], 0, active);
            }
            if let Some(callable_syn) = decl.callable_syntax {
                let mut out = TraceResult::default();
                out.hosts.push(self.make_closure_host(callable_syn, &[]));
                return out;
            }
        }

        let mut out = TraceResult::default();
        out.push_gap(GapItem::from_reason(
            UnknownReason::UnmodeledConstruct,
            "Target syntax is not an analyzable value expression.".to_string(),
            self.syntax.syntax_location(target.syntax),
        ));
        out
    }

    pub(crate) fn resolve_syntax_in_any_scope(
        &mut self,
        syn: SyntaxId,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let Ok(ir) = self.ir_provider.enclosing_scope_ir(syn) else {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::MissingSourceFile,
                "Referenced expression has no enclosing function or module scope.".to_string(),
                self.syntax.syntax_location(syn),
            ));
            return out;
        };
        if syn.file_id() == ir.root_syntax.file_id()
            && let Some(&val) = ir.expr_to_value.get(&syn.local_id())
        {
            let read_v = ir.expr_to_vertex.get(&syn.local_id()).copied();
            return self.trace_value(&ir, val, read_v, call_stack, depth, active);
        }
        let mut out = TraceResult::default();
        out.push_gap(GapItem::from_reason(
            UnknownReason::UnmodeledConstruct,
            "Expression could not be mapped to an analyzable value.".to_string(),
            self.syntax.syntax_location(syn),
        ));
        out
    }

    pub(crate) fn trace_value(
        &mut self,
        ir: &Arc<FunctionIr>,
        value: ValueId,
        read_vertex_hint: Option<VertexId>,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        if self.conflict.is_some() {
            return TraceResult::default();
        }
        if depth > MAX_TRACE_DEPTH || call_stack.len() > MAX_CALL_STACK_DEPTH {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::UnmodeledConstruct,
                "Instance identity trace exceeded maximum recursion depth.".to_string(),
                read_vertex_hint.and_then(|vid| ir.vertices.get(vid.0 as usize)?.location.clone()),
            ));
            return out;
        }

        let visit_key = VisitKey::Value {
            ir_id: ir.id,
            value,
            stack_keys: stack_keys(call_stack),
        };
        if active.contains(&visit_key) {
            return TraceResult {
                hit_cycle: true,
                ..TraceResult::default()
            };
        }
        active.push(visit_key);
        let result = self.trace_value_inner(ir, value, read_vertex_hint, call_stack, depth, active);
        active.pop();
        result
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Handles all IrOp value-producing variants on FunctionIr."
    )]
    fn trace_value_inner(
        &mut self,
        ir: &Arc<FunctionIr>,
        value: ValueId,
        read_vertex_hint: Option<VertexId>,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        if let Some(sources) = ir.value_sources.get(&value) {
            return self.trace_value_sources(
                ir,
                value,
                sources,
                read_vertex_hint,
                call_stack,
                depth,
                active,
            );
        }

        let producer = find_value_producer_vertex(ir, value, read_vertex_hint);
        let Some(vertex) = producer else {
            if let Some(param_idx) = ir
                .parameters
                .iter()
                .position(|p| p.raw_value == Some(value))
            {
                return self.resolve_parameter(ir, param_idx, call_stack, depth + 1, active);
            }
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::UnmodeledConstruct,
                "Value has no traceable source expression.".to_string(),
                read_vertex_hint.and_then(|vid| ir.vertices.get(vid.0 as usize)?.location.clone()),
            ));
            return out;
        };

        let loc = vertex.location.clone();
        match &vertex.op {
            IrOp::ValueForward { source, .. } => {
                self.trace_value(ir, *source, Some(vertex.id), call_stack, depth + 1, active)
            }
            IrOp::Await { promise } => {
                self.trace_value(ir, *promise, Some(vertex.id), call_stack, depth + 1, active)
            }
            IrOp::Literal { kind, .. } => {
                let non_inst_kind = classify_literal_non_instance(kind);
                let ev = EvidenceItem::simple(
                    "source",
                    "Literal value does not produce a trackable object instance.".to_string(),
                    loc,
                );
                TraceResult {
                    non_instances: vec![(non_inst_kind, ev.clone())],
                    evidence: vec![ev],
                    ..TraceResult::default()
                }
            }
            IrOp::TemplateString { .. } | IrOp::Unary { .. } => {
                let ev = EvidenceItem::simple(
                    "source",
                    "Primitive expression does not produce a trackable object instance."
                        .to_string(),
                    loc,
                );
                TraceResult {
                    non_instances: vec![(NonInstanceKind::TruthyOrUnknownPrimitive, ev.clone())],
                    evidence: vec![ev],
                    ..TraceResult::default()
                }
            }
            IrOp::Binary { op, right, .. } => {
                if *op == BinaryOp::Comma {
                    return self.trace_value(
                        ir,
                        *right,
                        Some(vertex.id),
                        call_stack,
                        depth + 1,
                        active,
                    );
                }
                let ev = EvidenceItem::simple(
                    "source",
                    "Binary operation yields a primitive value rather than an object instance."
                        .to_string(),
                    loc,
                );
                TraceResult {
                    non_instances: vec![(NonInstanceKind::TruthyOrUnknownPrimitive, ev.clone())],
                    evidence: vec![ev],
                    ..TraceResult::default()
                }
            }
            IrOp::ObjectAlloc { props, .. } => {
                let mut out = TraceResult::default();
                let mut host = self.make_alloc_host(
                    vertex.syntax,
                    loc,
                    call_stack,
                    "Allocated object literal instance.",
                    None,
                );
                host.host_scope_ir = Some(Arc::clone(ir));
                host.object_alloc_props = Some(props.clone());
                out.hosts.push(host);
                out
            }
            IrOp::ArrayAlloc { elems, .. } => {
                let mut out = TraceResult::default();
                let mut host = self.make_alloc_host(
                    vertex.syntax,
                    loc,
                    call_stack,
                    "Allocated array literal instance.",
                    None,
                );
                host.host_scope_ir = Some(Arc::clone(ir));
                host.array_alloc_elems = Some(elems.clone());
                out.hosts.push(host);
                out
            }
            IrOp::FunctionLiteral { syntax, .. } => {
                let mut out = TraceResult::default();
                out.hosts.push(self.make_closure_host(*syntax, call_stack));
                out
            }
            IrOp::ClassLiteral { syntax } => {
                let mut out = TraceResult::default();
                let mut host = self.make_alloc_host(
                    Some(*syntax),
                    loc,
                    call_stack,
                    "Evaluated class constructor value.",
                    Some(*syntax),
                );
                host.class_syntax = Some(*syntax);
                out.hosts.push(host);
                out
            }
            IrOp::JsxElement { .. } => {
                let mut out = TraceResult::default();
                out.hosts.push(self.make_alloc_host(
                    vertex.syntax,
                    loc,
                    call_stack,
                    "Allocated JSX element instance.",
                    None,
                ));
                out
            }
            IrOp::ReadLocal { binding } => {
                self.trace_local_binding(ir, *binding, vertex.id, call_stack, depth + 1, active)
            }
            IrOp::ReadSymbol { ident_syntax } => self.trace_read_symbol(
                ir,
                vertex.id,
                *ident_syntax,
                loc,
                call_stack,
                depth + 1,
                active,
            ),
            IrOp::AssignLocal { value: rhs, .. }
            | IrOp::AssignSymbol { value: rhs, .. }
            | IrOp::PropWrite { value: rhs, .. } => {
                self.trace_value(ir, *rhs, Some(vertex.id), call_stack, depth + 1, active)
            }
            IrOp::ThisRef => self.trace_this_ref(ir, vertex.id, loc, call_stack, depth + 1, active),
            IrOp::SuperRef | IrOp::MetaProp => {
                let mut out = TraceResult::default();
                out.push_gap(GapItem::from_reason(
                    UnknownReason::UnmodeledConstruct,
                    "Meta-property or super reference does not hold a trackable instance."
                        .to_string(),
                    loc,
                ));
                out
            }
            IrOp::PropRead {
                object,
                key,
                is_super,
                ..
            } => self.trace_prop_read(
                ir,
                vertex,
                *object,
                key,
                *is_super,
                call_stack,
                depth + 1,
                active,
            ),
            IrOp::Call {
                call_kind,
                callee,
                receiver,
                args,
            } => self.trace_call(
                ir,
                vertex,
                *call_kind,
                *callee,
                *receiver,
                args,
                call_stack,
                depth + 1,
                active,
            ),
            IrOp::Construct { callee, args, .. } => {
                self.trace_construct(ir, vertex, *callee, args, call_stack, depth + 1, active)
            }
            IrOp::IteratorNext { iterator, .. } => {
                self.trace_iterator_next(ir, vertex, *iterator, call_stack, depth + 1, active)
            }
            IrOp::IteratorRest { .. } => {
                let mut out = TraceResult::default();
                out.hosts.push(self.make_alloc_host(
                    vertex.syntax,
                    loc,
                    call_stack,
                    "Allocated array from rest destructuring.",
                    None,
                ));
                out
            }
            IrOp::ObjectRest { .. } => {
                let mut out = TraceResult::default();
                out.hosts.push(self.make_alloc_host(
                    vertex.syntax,
                    loc,
                    call_stack,
                    "Allocated object from object rest destructuring.",
                    None,
                ));
                out
            }
            IrOp::Unknown {
                reason,
                message,
                location,
                ..
            } => {
                let mut out = TraceResult::default();
                out.push_gap(GapItem::from_reason(
                    *reason,
                    message.clone(),
                    Some(location.clone()),
                ));
                out
            }
            IrOp::Entry
            | IrOp::Exit
            | IrOp::NopMerge
            | IrOp::Branch { .. }
            | IrOp::SwitchDispatch { .. }
            | IrOp::Return { .. }
            | IrOp::Throw { .. }
            | IrOp::CatchParamBind { .. }
            | IrOp::Yield { .. }
            | IrOp::IteratorInit { .. }
            | IrOp::IteratorClose { .. } => {
                let mut out = TraceResult::default();
                out.push_gap(GapItem::from_reason(
                    UnknownReason::UnmodeledConstruct,
                    "Control-flow vertex does not produce a trackable object instance.".to_string(),
                    loc,
                ));
                out
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Handles conditional, nullish-coalescing, logical-or, and logical-and value merges."
    )]
    fn trace_value_sources(
        &mut self,
        ir: &Arc<FunctionIr>,
        merged_val: ValueId,
        sources: &[ValueId],
        read_vertex_hint: Option<VertexId>,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        if sources.is_empty() {
            return TraceResult::default();
        }
        let merge_kind = detect_merge_branch_kind(ir, merged_val, read_vertex_hint);
        if let Some(kind) = merge_kind
            && sources.len() == 2
        {
            match kind {
                IrBranchKind::NullishCoalescing | IrBranchKind::LogicalOr => {
                    let mut lhs =
                        self.trace_value(ir, sources[0], None, call_stack, depth + 1, active);
                    if self.conflict.is_some() {
                        return lhs;
                    }
                    let definitely_non_nullish = !lhs.hosts.is_empty()
                        && lhs.non_instances.is_empty()
                        && lhs.gaps.is_empty()
                        && lhs
                            .hosts
                            .iter()
                            .all(|h| h.origin.origin_kind != InstanceOriginKind::Parameter);
                    if definitely_non_nullish {
                        return lhs;
                    }
                    let rhs = self.trace_value(ir, sources[1], None, call_stack, depth + 1, active);
                    let only_nullish_or_falsy = lhs.hosts.is_empty()
                        && lhs.gaps.is_empty()
                        && !lhs.non_instances.is_empty()
                        && lhs.non_instances.iter().all(|(k, _)| {
                            *k == NonInstanceKind::Nullish
                                || (kind == IrBranchKind::LogicalOr
                                    && *k == NonInstanceKind::FalsyNonNullish)
                        });
                    if only_nullish_or_falsy {
                        return rhs;
                    }
                    lhs.merge(rhs);
                    return lhs;
                }
                IrBranchKind::LogicalAnd => {
                    let mut lhs =
                        self.trace_value(ir, sources[0], None, call_stack, depth + 1, active);
                    if self.conflict.is_some() {
                        return lhs;
                    }
                    let definitely_truthy = !lhs.hosts.is_empty()
                        && lhs.non_instances.is_empty()
                        && lhs.gaps.is_empty()
                        && lhs
                            .hosts
                            .iter()
                            .all(|h| h.origin.origin_kind != InstanceOriginKind::Parameter);
                    if definitely_truthy {
                        return self.trace_value(
                            ir,
                            sources[1],
                            None,
                            call_stack,
                            depth + 1,
                            active,
                        );
                    }
                    let rhs = self.trace_value(ir, sources[1], None, call_stack, depth + 1, active);
                    lhs.merge(rhs);
                    return lhs;
                }
                _ => {}
            }
        }

        let mut combined = TraceResult::default();
        for &src in sources {
            let sub = self.trace_value(ir, src, None, call_stack, depth + 1, active);
            combined.merge(sub);
            if self.conflict.is_some() {
                break;
            }
        }
        combined
    }

    fn trace_local_binding(
        &mut self,
        ir: &Arc<FunctionIr>,
        binding: LocalBindingId,
        read_vertex: VertexId,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let visit_key = VisitKey::Binding {
            ir_id: ir.id,
            binding,
            stack_keys: stack_keys(call_stack),
        };
        if active.contains(&visit_key) {
            return TraceResult {
                hit_cycle: true,
                ..TraceResult::default()
            };
        }
        active.push(visit_key);
        let result =
            self.trace_local_binding_inner(ir, binding, read_vertex, call_stack, depth, active);
        active.pop();
        result
    }

    fn trace_local_binding_inner(
        &mut self,
        ir: &Arc<FunctionIr>,
        binding: LocalBindingId,
        read_vertex: VertexId,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let writes_res = var_writes::resolve_reaching_writes_for_local(
            ir,
            read_vertex,
            binding,
            self.semantic,
            self.syntax,
            self.ir_provider,
        );

        let binding_info = ir.bindings.get(binding.0 as usize);
        let binding_name = binding_info.map_or("binding", |b| b.name.as_str());
        let decl_loc = binding_info.and_then(|b| self.syntax.syntax_location(b.syntax));
        let read_loc = ir
            .vertices
            .get(read_vertex.0 as usize)
            .and_then(|v| v.location.clone());

        let mut combined = TraceResult::default();
        if !writes_res.is_exhaustive {
            combined.push_gap(GapItem::from_reason(
                UnknownReason::UnmodeledConstruct,
                format!("Writes to variable '{binding_name}' could not be exhaustively analyzed."),
                read_loc.clone().or_else(|| decl_loc.clone()),
            ));
        }

        if writes_res.entry_param_reaches
            && let Some(param_idx) = ir
                .parameters
                .iter()
                .position(|p| p.binding == Some(binding))
        {
            let param_res = self.resolve_parameter(ir, param_idx, call_stack, depth + 1, active);
            combined.merge(param_res);
        }

        for w in &writes_res.writes {
            let sub = self.trace_var_write(ir, w, call_stack, depth, active);
            combined.merge(sub);
            if self.conflict.is_some() {
                return combined;
            }
        }

        if let Some(b) = binding_info
            && b.syntax != ir.root_syntax
            && let Some(loc) = decl_loc
            && ir
                .vertices
                .get(read_vertex.0 as usize)
                .is_some_and(|v| v.syntax != Some(b.syntax))
        {
            combined.prepend_step(&EvidenceItem::simple(
                "source",
                format!("Bound to '{binding_name}'"),
                Some(loc),
            ));
        }
        if let Some(rloc) = read_loc
            && binding_info.is_some_and(|b| {
                ir.vertices
                    .get(read_vertex.0 as usize)
                    .is_some_and(|v| v.syntax != Some(b.syntax))
            })
        {
            combined.prepend_step(&EvidenceItem::simple(
                "source",
                format!("Referenced '{binding_name}'"),
                Some(rloc),
            ));
        }

        combined
    }

    fn trace_var_write(
        &mut self,
        read_ir: &Arc<FunctionIr>,
        write: &VarWriteSite,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let effective_stack: &[IrCallFrame] =
            if write.scope_ir.scope_kind == IrScopeKind::ModuleTopLevel {
                &[]
            } else if write.scope_ir.id == read_ir.id && !write.in_closure {
                call_stack
            } else {
                find_stack_for_scope(call_stack, write.scope_ir.id).unwrap_or(&[])
            };

        match write.source {
            VarWriteSource::Value(val) => self.trace_value(
                &write.scope_ir,
                val,
                Some(write.vertex),
                effective_stack,
                depth + 1,
                active,
            ),
            VarWriteSource::FunctionDecl(fn_syn) => {
                let mut out = TraceResult::default();
                out.hosts
                    .push(self.make_closure_host(fn_syn, effective_stack));
                out
            }
            VarWriteSource::Uninitialized => {
                let ev = EvidenceItem::simple(
                    "source",
                    "Uninitialized binding yields undefined rather than an object instance."
                        .to_string(),
                    write.location.clone(),
                );
                TraceResult {
                    non_instances: vec![(NonInstanceKind::Nullish, ev.clone())],
                    evidence: vec![ev],
                    ..TraceResult::default()
                }
            }
            VarWriteSource::Opaque => {
                let mut out = TraceResult::default();
                out.push_gap(GapItem::from_reason(
                    UnknownReason::UnmodeledConstruct,
                    "Variable receives an opaque or unmodeled binding value.".to_string(),
                    write.location.clone(),
                ));
                out
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Resolves captured outer bindings, module-level singletons, and first-party imports."
    )]
    fn trace_read_symbol(
        &mut self,
        read_ir: &Arc<FunctionIr>,
        read_vertex: VertexId,
        ident_syntax: SyntaxId,
        read_loc: Option<DiagnosticLocation>,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let Some(mut decl) = self.semantic.resolve_declaration(ident_syntax) else {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::UnresolvedDeclaration,
                "Symbol reference could not be resolved to a declaration.".to_string(),
                read_loc,
            ));
            return out;
        };

        if (decl.is_external || (decl.callable_syntax.is_some() && !decl.has_body))
            && let Some(decl_syn) = decl.primary_syntax
        {
            match self
                .semantic
                .resolve_implementation_for_syntax(decl_syn, Some(ident_syntax))
            {
                ImplementationFact::Resolved(imp) => {
                    if let Some(fn_syn) = imp.fn_syntax {
                        let mut out = TraceResult::default();
                        out.hosts.push(self.make_closure_host(fn_syn, &[]));
                        return out;
                    }
                    if let Some(resolved_decl) =
                        self.semantic.resolve_declaration(imp.declaration_syntax)
                    {
                        decl = resolved_decl;
                    } else {
                        decl.primary_syntax = Some(imp.declaration_syntax);
                        decl.callable_syntax = imp.fn_syntax;
                        decl.is_external = false;
                    }
                }
                ImplementationFact::Inconclusive { gaps, evidence, .. } => {
                    let mut out = TraceResult::default();
                    for g in gaps {
                        out.push_gap(g);
                    }
                    for ev in evidence {
                        out.push_evidence(ev);
                    }
                    return out;
                }
                ImplementationFact::External { evidence, .. } => {
                    let mut out = TraceResult::default();
                    for ev in evidence {
                        out.push_evidence(ev);
                    }
                    let label = decl.name.as_deref().unwrap_or("external symbol");
                    out.push_gap(GapItem::from_reason(
                        UnknownReason::MissingBehaviorSummary,
                        format!("External symbol '{label}' has no source implementation."),
                        read_loc,
                    ));
                    return out;
                }
            }
        }

        let Some(decl_syn) = decl.primary_syntax else {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::UnresolvedDeclaration,
                "Resolved symbol has no primary declaration syntax.".to_string(),
                read_loc,
            ));
            return out;
        };

        if let Some(callable_syn) = decl.callable_syntax {
            let mut out = TraceResult::default();
            out.hosts.push(self.make_closure_host(callable_syn, &[]));
            return out;
        }

        let Ok(decl_ir) = self.ir_provider.enclosing_scope_ir(decl_syn) else {
            if let Some(init_syn) = decl.initializer_syntax {
                return self.resolve_syntax_in_any_scope(init_syn, &[], depth + 1, active);
            }
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::MissingSourceFile,
                "Declaration scope could not be analyzed.".to_string(),
                read_loc,
            ));
            return out;
        };

        let effective_stack: &[IrCallFrame] = if decl_ir.scope_kind == IrScopeKind::ModuleTopLevel {
            &[]
        } else if decl_ir.id == read_ir.id {
            call_stack
        } else {
            find_stack_for_scope(call_stack, decl_ir.id).unwrap_or(&[])
        };

        if let Some(param_idx) = decl_ir.parameters.iter().position(|p| p.syntax == decl_syn) {
            let mut res =
                self.resolve_parameter(&decl_ir, param_idx, effective_stack, depth + 1, active);
            if let Some(loc) = read_loc {
                let name = decl.name.as_deref().unwrap_or("parameter");
                res.prepend_step(&EvidenceItem::simple(
                    "source",
                    format!("Captured parameter '{name}'"),
                    Some(loc),
                ));
            }
            return res;
        }

        if let Some(binding) = decl_ir
            .bindings
            .iter()
            .find(|b| b.syntax == decl_syn || (decl.symbol.is_some() && b.symbol == decl.symbol))
        {
            if decl_ir.id == read_ir.id {
                return self.trace_local_binding(
                    &decl_ir,
                    binding.id,
                    read_vertex,
                    effective_stack,
                    depth + 1,
                    active,
                );
            }
            let writes = var_writes::collect_all_writes_for_local(
                &decl_ir,
                binding.id,
                self.semantic,
                self.syntax,
                self.ir_provider,
            );
            let mut combined = TraceResult::default();
            if !writes.is_exhaustive {
                combined.push_gap(GapItem::from_reason(
                    UnknownReason::UnmodeledConstruct,
                    format!(
                        "Writes to captured variable '{}' could not be exhaustively analyzed.",
                        binding.name
                    ),
                    read_loc.clone(),
                ));
            }
            for w in &writes.writes {
                let write_stack: &[IrCallFrame] =
                    if w.scope_ir.scope_kind == IrScopeKind::ModuleTopLevel {
                        &[]
                    } else if w.scope_ir.id == read_ir.id {
                        call_stack
                    } else if w.scope_ir.id == decl_ir.id && !w.in_closure {
                        effective_stack
                    } else {
                        find_stack_for_scope(call_stack, w.scope_ir.id).unwrap_or(&[])
                    };
                let sub = match w.source {
                    VarWriteSource::Value(val) => self.trace_value(
                        &w.scope_ir,
                        val,
                        Some(w.vertex),
                        write_stack,
                        depth + 1,
                        active,
                    ),
                    VarWriteSource::FunctionDecl(fn_syn) => {
                        let mut out = TraceResult::default();
                        out.hosts.push(self.make_closure_host(fn_syn, write_stack));
                        out
                    }
                    VarWriteSource::Uninitialized => {
                        let ev = EvidenceItem::simple(
                            "source",
                            "Uninitialized binding yields undefined rather than an object instance."
                                .to_string(),
                            w.location.clone(),
                        );
                        TraceResult {
                            non_instances: vec![(NonInstanceKind::Nullish, ev.clone())],
                            evidence: vec![ev],
                            ..TraceResult::default()
                        }
                    }
                    VarWriteSource::Opaque => {
                        let mut out = TraceResult::default();
                        out.push_gap(GapItem::from_reason(
                            UnknownReason::UnmodeledConstruct,
                            "Variable receives an opaque or unmodeled binding value.".to_string(),
                            w.location.clone(),
                        ));
                        out
                    }
                };
                combined.merge(sub);
                if self.conflict.is_some() {
                    return combined;
                }
            }
            if let Some(loc) = decl.location {
                let msg = if decl_ir.scope_kind == IrScopeKind::ModuleTopLevel {
                    format!("Bound to module binding '{}'", binding.name)
                } else {
                    format!("Captured '{}' in closure", binding.name)
                };
                combined.prepend_step(&EvidenceItem::simple("source", msg, Some(loc)));
            }
            if let Some(rloc) = read_loc {
                combined.prepend_step(&EvidenceItem::simple(
                    "source",
                    format!("Referenced '{}'", binding.name),
                    Some(rloc),
                ));
            }
            return combined;
        }

        if let Some(init_syn) = decl.initializer_syntax {
            return self.resolve_syntax_in_any_scope(init_syn, effective_stack, depth + 1, active);
        }

        let mut out = TraceResult::default();
        out.push_gap(GapItem::from_reason(
            UnknownReason::UnresolvedDeclaration,
            format!(
                "Declaration '{}' has no traceable binding or initializer.",
                decl.name.as_deref().unwrap_or("<symbol>")
            ),
            read_loc,
        ));
        out
    }

    fn resolve_parameter(
        &mut self,
        fn_ir: &Arc<FunctionIr>,
        param_idx: usize,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let Some(param) = fn_ir.parameters.get(param_idx) else {
            return TraceResult::default();
        };
        let param_name = param.name.clone();
        let param_syn = param.syntax;
        let param_loc = self.syntax.syntax_location(param_syn);

        if let Some(frame_pos) = call_stack.iter().rposition(|f| f.callee_ir.id == fn_ir.id) {
            let frame = call_stack[frame_pos].clone();
            let rest_stack = &call_stack[..frame_pos];
            return self.resolve_param_from_frame(
                &frame,
                rest_stack,
                param_idx,
                &param_name,
                param_syn,
                depth,
                active,
            );
        }

        if !self.allow_unanchored_caller_lookup {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::DynamicCallee,
                format!("Parameter '{param_name}' is unbound in the analyzed call scope."),
                param_loc,
            ));
            return out;
        }

        if call_stack.is_empty()
            && let Some(combined) = self.resolve_unanchored_parameter_callers(
                fn_ir,
                param_idx,
                &param_name,
                depth,
                active,
            )
        {
            return combined;
        }

        let mut out = TraceResult::default();
        out.hosts
            .push(self.make_parameter_host(param_syn, &param_name, param_loc));
        out
    }

    fn resolve_unanchored_parameter_callers(
        &mut self,
        fn_ir: &Arc<FunctionIr>,
        param_idx: usize,
        param_name: &str,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> Option<TraceResult> {
        let fn_syn = fn_ir.root_syntax;
        let fn_symbol = self.semantic.symbol_at_syntax(fn_syn);
        let mut site_syntaxes: Vec<(SyntaxId, FileId)> = Vec::new();
        let mut seen_sites: FxHashSet<SyntaxId> = FxHashSet::default();
        if let Some(graph) = self.call_graph() {
            if let Some(sym) = fn_symbol
                && let Some(indices) = graph.by_symbol.get(&sym)
            {
                for &idx in indices {
                    let site = &graph.sites[idx];
                    if seen_sites.insert(site.call_syntax) {
                        site_syntaxes.push((site.call_syntax, site.file));
                    }
                }
            }
            if let Some(indices) = graph.by_decl.get(&fn_syn) {
                for &idx in indices {
                    let site = &graph.sites[idx];
                    if seen_sites.insert(site.call_syntax) {
                        site_syntaxes.push((site.call_syntax, site.file));
                    }
                }
            }
        }
        if self.conflict.is_some() {
            return Some(TraceResult::default());
        }

        if let Some(allowed) = self.allowed_files {
            site_syntaxes.retain(|(_, file)| allowed.contains(file));
        }
        site_syntaxes.sort_by_key(|(s, _)| (s.file_id().0, s.local_id().0));

        if site_syntaxes.is_empty() {
            return None;
        }
        let mut combined = TraceResult::default();
        for (call_syn, _) in site_syntaxes {
            if let Ok(caller_ir) = self.ir_provider.enclosing_scope_ir(call_syn)
                && let Some(call_v) = find_call_vertex_by_syntax(&caller_ir, call_syn)
            {
                let args = call_vertex_args(call_v);
                if let Some(arg) = args.get(param_idx).copied() {
                    let mut sub =
                        self.trace_call_arg(&caller_ir, call_v.id, arg, &[], depth + 1, active);
                    if let Some(cloc) = call_v.location.clone() {
                        sub.prepend_step(&EvidenceItem::simple(
                            "source",
                            format!("Passed as argument '{param_name}'"),
                            Some(cloc),
                        ));
                    }
                    combined.merge(sub);
                    if self.conflict.is_some() {
                        return Some(combined);
                    }
                }
            }
        }
        (!combined.hosts.is_empty()
            || !combined.gaps.is_empty()
            || !combined.non_instances.is_empty())
        .then_some(combined)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Maps caller argument or default initializer for an active IrCallFrame."
    )]
    fn resolve_param_from_frame(
        &mut self,
        frame: &IrCallFrame,
        rest_stack: &[IrCallFrame],
        param_idx: usize,
        param_name: &str,
        param_syn: SyntaxId,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        if let Some(&arg) = frame.args.get(param_idx) {
            let mut res = self.trace_call_arg(
                &frame.caller_ir,
                frame.callee_ir.entry,
                arg,
                rest_stack,
                depth + 1,
                active,
            );
            if let Some(cloc) = frame.call_location.clone() {
                res.prepend_step(&EvidenceItem::simple(
                    "source",
                    format!("Passed as argument '{param_name}'"),
                    Some(cloc),
                ));
            }
            return res;
        }

        if frame.args.iter().any(|a| matches!(a, IrCallArg::Spread(_))) {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::ParameterOutOfBounds,
                format!(
                    "Parameter '{param_name}' may be populated via spread argument and cannot be statically aligned."
                ),
                frame.call_location.clone(),
            ));
            return out;
        }

        if let Some(param_decl) = self.semantic.resolve_declaration(param_syn)
            && let Some(def_init) = param_decl.initializer_syntax
        {
            return self.resolve_syntax_in_any_scope(def_init, rest_stack, depth + 1, active);
        }

        let ev = EvidenceItem::simple(
            "source",
            format!("Omitted optional parameter '{param_name}' defaults to undefined."),
            frame.call_location.clone(),
        );
        TraceResult {
            non_instances: vec![(NonInstanceKind::Nullish, ev.clone())],
            evidence: vec![ev],
            ..TraceResult::default()
        }
    }

    fn trace_call_arg(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        _hint_vertex: VertexId,
        arg: IrCallArg,
        stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        match arg {
            IrCallArg::Positional(val) => {
                self.trace_value(caller_ir, val, None, stack, depth + 1, active)
            }
            IrCallArg::Spread(val) => {
                let loc = find_value_producer_vertex(caller_ir, val, None)
                    .and_then(|v| v.location.clone());
                let mut out = TraceResult::default();
                out.push_gap(GapItem::from_reason(
                    UnknownReason::UnmodeledConstruct,
                    "Spread argument cannot be statically mapped to positional parameter."
                        .to_string(),
                    loc,
                ));
                out
            }
        }
    }

    fn trace_this_ref(
        &mut self,
        ir: &Arc<FunctionIr>,
        _vertex_id: VertexId,
        loc: Option<DiagnosticLocation>,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        if let Some((top, rest)) = call_stack.split_last()
            && top.callee_ir.id == ir.id
        {
            if top.is_construct {
                let mut out = TraceResult::default();
                let class_syn = self.resolve_constructed_class_syntax(top.call_syntax);
                out.hosts.push(self.make_alloc_host(
                    Some(top.call_syntax),
                    top.call_location.clone(),
                    rest,
                    "Constructed class instance receiver 'this'.",
                    class_syn,
                ));
                return out;
            }
            if let Some(recv) = &top.receiver {
                match recv {
                    ReceiverBinding::Value(recv_val) => {
                        return self.trace_value(
                            &top.caller_ir,
                            *recv_val,
                            None,
                            rest,
                            depth + 1,
                            active,
                        );
                    }
                    ReceiverBinding::PreResolved(hosts) => {
                        return TraceResult {
                            hosts: hosts.clone(),
                            ..TraceResult::default()
                        };
                    }
                }
            }
        }

        if let Some(class_syn) = self.find_enclosing_class_syntax(ir.root_syntax) {
            let class_loc = self.syntax.syntax_location(class_syn);
            let span_key = self.format_syntax_span_key(class_syn, class_loc.as_ref());
            let class_name = self
                .semantic
                .resolve_declaration(class_syn)
                .and_then(|d| d.name)
                .unwrap_or_else(|| "<class>".to_string());
            let mut out = TraceResult::default();
            let ev = EvidenceItem::simple(
                "source",
                format!("Class receiver 'this' of '{class_name}'."),
                class_loc.clone(),
            );
            out.hosts.push(HostOrigin {
                origin: ResolvedInstanceOrigin {
                    instance_key: format!("this:{span_key}"),
                    origin_kind: InstanceOriginKind::Allocation,
                    allocation_location: class_loc,
                    origin_syntax: Some(class_syn),
                    summary_id: None,
                    summary_source: None,
                    evidence: vec![ev],
                },
                captured_stack: Vec::new(),
                class_syntax: Some(class_syn),
                closure_fn: None,
                host_scope_ir: None,
                object_alloc_props: None,
                array_alloc_elems: None,
            });
            return out;
        }

        let mut out = TraceResult::default();
        out.push_gap(GapItem::from_reason(
            UnknownReason::UnresolvedDeclaration,
            "'this' reference has no statically known enclosing class receiver.".to_string(),
            loc,
        ));
        out
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Resolves object/array literal reads, constructor field initializers, parameter properties, #private fields, and flow-sensitive field writes."
    )]
    fn trace_prop_read(
        &mut self,
        read_ir: &Arc<FunctionIr>,
        read_vertex: &CfgVertex,
        object_val: ValueId,
        key: &IrPropKey,
        _is_super: bool,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let Some((prop_name, is_private)) = static_prop_key(key) else {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::UnmodeledConstruct,
                "Computed property access cannot be statically resolved to a fixed field."
                    .to_string(),
                read_vertex.location.clone(),
            ));
            return out;
        };

        let host_res = self.trace_value(
            read_ir,
            object_val,
            Some(read_vertex.id),
            call_stack,
            depth + 1,
            active,
        );
        if self.conflict.is_some() {
            return host_res;
        }

        let mut combined = TraceResult {
            gaps: host_res.gaps,
            evidence: host_res.evidence,
            hit_cycle: host_res.hit_cycle,
            ..TraceResult::default()
        };

        if host_res.hosts.is_empty() {
            if combined.gaps.is_empty() {
                combined.push_gap(GapItem::from_reason(
                    UnknownReason::UnresolvedDeclaration,
                    format!(
                        "Receiver of property '.{prop_name}' could not be resolved to an object instance."
                    ),
                    read_vertex.location.clone(),
                ));
            }
            return combined;
        }

        for host in &host_res.hosts {
            let visit_key = VisitKey::PropRead {
                host_key: host.origin.instance_key.clone(),
                prop_name: prop_name.to_string(),
                is_private,
                read_ir_id: read_ir.id,
                read_vertex: read_vertex.id,
            };
            if active.contains(&visit_key) {
                combined.hit_cycle = true;
                continue;
            }
            active.push(visit_key);
            let mut per_host = self.resolve_prop_on_host(
                host,
                prop_name,
                is_private,
                read_ir,
                read_vertex,
                call_stack,
                depth,
                active,
            );
            active.pop();

            if per_host.hosts.is_empty()
                && per_host.gaps.is_empty()
                && per_host.non_instances.is_empty()
                && !per_host.hit_cycle
            {
                per_host.push_gap(GapItem::from_reason(
                    UnknownReason::UnresolvedDeclaration,
                    format!(
                        "Field '.{prop_name}' has no known initialization or assignment in analyzed sources."
                    ),
                    read_vertex.location.clone(),
                ));
            }
            if let Some(rloc) = read_vertex.location.clone() {
                per_host.prepend_step(&EvidenceItem::simple(
                    "source",
                    format!("Read field '.{prop_name}'"),
                    Some(rloc),
                ));
            }
            combined.merge(per_host);
            if self.conflict.is_some() {
                return combined;
            }
        }

        combined
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Combines array element access, object literal properties, constructor field writes, and CFG reaching imperative writes."
    )]
    fn resolve_prop_on_host(
        &mut self,
        host: &HostOrigin,
        prop_name: &str,
        is_private: bool,
        read_ir: &Arc<FunctionIr>,
        read_vertex: &CfgVertex,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let mut out = TraceResult::default();

        // Array element access by static integer index (e.g. `arr[0]`).
        if !is_private
            && let Some(elems) = &host.array_alloc_elems
            && let Some(scope_ir) = &host.host_scope_ir
            && let Ok(idx) = prop_name.parse::<usize>()
            && let Some(elem) = elems.get(idx)
        {
            match elem {
                IrArrayElem::Value(val) => {
                    return self.trace_value(
                        scope_ir,
                        *val,
                        None,
                        &host.captured_stack,
                        depth + 1,
                        active,
                    );
                }
                IrArrayElem::Elision => {
                    let ev = EvidenceItem::simple(
                        "source",
                        "Elided array element yields undefined.".to_string(),
                        read_vertex.location.clone(),
                    );
                    out.non_instances
                        .push((NonInstanceKind::Nullish, ev.clone()));
                    out.push_evidence(ev);
                    return out;
                }
                IrArrayElem::Spread(_) => {
                    out.push_gap(GapItem::from_reason(
                        UnknownReason::UnmodeledConstruct,
                        "Array spread element prevents static index instance resolution."
                            .to_string(),
                        read_vertex.location.clone(),
                    ));
                    return out;
                }
            }
        }

        // Check flow-sensitive imperative PropWrites + constructor writes.
        let timeline = build_timeline_anchors(read_ir, read_vertex.id, call_stack);
        let matched_writes =
            self.find_matching_prop_writes(host, prop_name, is_private, &timeline, depth, active);
        if self.conflict.is_some() {
            return out;
        }

        let mut killed_initial_literal = false;
        if !matched_writes.is_empty() {
            let surviving = filter_surviving_writes(matched_writes, &timeline);
            for w in surviving {
                let sub = self.trace_value(
                    &w.scope_ir,
                    w.rhs,
                    Some(w.write_vertex),
                    &w.rhs_stack,
                    depth + 1,
                    active,
                );
                out.merge(sub);
                if self.conflict.is_some() {
                    return out;
                }
                if let Some((tl_idx, w_vid, is_uncond)) = w.anchor_in_timeline
                    && is_uncond
                    && tl_idx == 0
                {
                    let _ = w_vid;
                    killed_initial_literal = true;
                }
            }
        }

        // Object literal initial properties (unless unambiguously overwritten before this read).
        if !is_private
            && !killed_initial_literal
            && let Some(props) = &host.object_alloc_props
            && let Some(scope_ir) = &host.host_scope_ir
        {
            let mut matched_prop_val: Option<ValueId> = None;
            let mut has_spread_after = false;
            for prop in props.iter().rev() {
                match prop {
                    IrObjectProp::KeyValue { key, value } => {
                        if prop_key_static_name(key) == Some(prop_name) {
                            matched_prop_val = Some(*value);
                            break;
                        }
                    }
                    IrObjectProp::Spread(spread_val) => {
                        let spread_res = self.trace_value(
                            scope_ir,
                            *spread_val,
                            None,
                            &host.captured_stack,
                            depth + 1,
                            active,
                        );
                        for sh in &spread_res.hosts {
                            let sub = self.resolve_prop_on_host(
                                sh,
                                prop_name,
                                false,
                                read_ir,
                                read_vertex,
                                call_stack,
                                depth + 1,
                                active,
                            );
                            out.merge(sub);
                        }
                        if spread_res.hosts.is_empty() {
                            has_spread_after = true;
                        }
                    }
                }
            }
            if let Some(val) = matched_prop_val {
                let sub =
                    self.trace_value(scope_ir, val, None, &host.captured_stack, depth + 1, active);
                out.merge(sub);
            } else if has_spread_after && out.hosts.is_empty() {
                out.push_gap(GapItem::from_reason(
                    UnknownReason::UnmodeledConstruct,
                    format!("Object spread prevents statically resolving property '.{prop_name}'."),
                    read_vertex.location.clone(),
                ));
            }
        }

        out
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Matches constructor, method `this`, helper parameter, and direct receiver PropWrites against a host instance."
    )]
    fn find_matching_prop_writes(
        &mut self,
        host: &HostOrigin,
        prop_name: &str,
        is_private: bool,
        timeline: &[TimelineAnchor],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> Vec<MatchedPropWrite> {
        let mut matched: Vec<MatchedPropWrite> = Vec::new();
        let host_key = &host.origin.instance_key;
        let host_class_syn = host.class_syntax;

        // 1. Constructor & base-class constructor writes when `host` is a `new Class(...)` allocation.
        if let Some(new_syn) = host.origin.origin_syntax
            && let Some(class_syn) = host_class_syn
            && !host_key.starts_with("this:")
        {
            self.collect_constructor_writes_for_host(
                new_syn,
                class_syn,
                &host.captured_stack,
                prop_name,
                is_private,
                timeline,
                &mut matched,
            );
        }

        // 2. Imperative PropWrites across all project (or lookupFiles-scoped) scopes.
        if let Some(syn) = host.origin.origin_syntax {
            self.extra_field_files.insert(syn.file_id());
        }
        if let Some(class_syn) = host_class_syn {
            self.extra_field_files.insert(class_syn.file_id());
        }
        if let Some(host_ir) = &host.host_scope_ir {
            self.extra_field_files.insert(host_ir.id.file);
        }
        for anchor in timeline {
            self.extra_field_files.insert(anchor.ir.id.file);
        }
        let candidates: Vec<IndexedPropWrite> = self
            .ensure_field_index()
            .writes_by_prop
            .get(prop_name)
            .cloned()
            .unwrap_or_default();

        for site in candidates {
            if !self.is_file_allowed_for_fields(site.scope_ir.id.file) {
                continue;
            }
            if site.is_private != is_private {
                continue;
            }
            if is_private
                && let Some(hc) = host_class_syn
                && self.find_enclosing_class_syntax(site.scope_ir.root_syntax) != Some(hc)
            {
                continue;
            }

            let obj_producer =
                find_value_producer_vertex(&site.scope_ir, site.object, Some(site.vertex));
            let Some(obj_v) = obj_producer else {
                continue;
            };

            match &obj_v.op {
                IrOp::ThisRef => {
                    if site.scope_ir.scope_kind == IrScopeKind::Constructor {
                        // Already handled for concrete `new` hosts in step 1; handle symbolic `this:` host here.
                        if host_key.starts_with("this:")
                            && let Some(hc) = host_class_syn
                            && self.find_enclosing_class_syntax(site.scope_ir.root_syntax)
                                == Some(hc)
                        {
                            matched.push(MatchedPropWrite {
                                scope_ir: Arc::clone(&site.scope_ir),
                                write_vertex: site.vertex,
                                rhs: site.rhs,
                                rhs_stack: Vec::new(),
                                anchor_in_timeline: find_timeline_pos(
                                    &site.scope_ir,
                                    site.vertex,
                                    true,
                                    timeline,
                                ),
                            });
                        }
                        continue;
                    }
                    self.match_this_method_writes(
                        host,
                        &site,
                        timeline,
                        depth,
                        active,
                        &mut matched,
                    );
                }
                IrOp::ReadLocal { binding }
                    if site
                        .scope_ir
                        .bindings
                        .get(binding.0 as usize)
                        .is_some_and(|b| b.mutability == BindingMutability::Parameter) =>
                {
                    if let Some(param_idx) = site
                        .scope_ir
                        .parameters
                        .iter()
                        .position(|p| p.binding == Some(*binding))
                    {
                        self.match_param_prop_writes(
                            host,
                            &site,
                            param_idx,
                            timeline,
                            depth,
                            active,
                            &mut matched,
                        );
                    }
                }
                _ => {
                    let recv_res = self.trace_value(
                        &site.scope_ir,
                        site.object,
                        Some(obj_v.id),
                        &[],
                        depth + 1,
                        active,
                    );
                    if self.conflict.is_some() {
                        return matched;
                    }
                    if recv_res
                        .hosts
                        .iter()
                        .any(|h| h.origin.instance_key == *host_key)
                    {
                        let is_unambiguous = recv_res.hosts.len() == 1
                            && recv_res.gaps.is_empty()
                            && is_unconditional_in_scope(&site.scope_ir, site.vertex);
                        let anchor = find_timeline_pos(
                            &site.scope_ir,
                            site.vertex,
                            is_unambiguous,
                            timeline,
                        );
                        if !timeline_excludes_write(&site.scope_ir, anchor, timeline) {
                            matched.push(MatchedPropWrite {
                                scope_ir: Arc::clone(&site.scope_ir),
                                write_vertex: site.vertex,
                                rhs: site.rhs,
                                rhs_stack: Vec::new(),
                                anchor_in_timeline: anchor,
                            });
                        }
                    }
                }
            }
        }

        matched
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Collects constructor field initializers, parameter properties, and super-constructor writes for `new Class(...)`."
    )]
    fn collect_constructor_writes_for_host(
        &mut self,
        new_syn: SyntaxId,
        class_syn: SyntaxId,
        host_captured_stack: &[IrCallFrame],
        prop_name: &str,
        is_private: bool,
        timeline: &[TimelineAnchor],
        matched: &mut Vec<MatchedPropWrite>,
    ) {
        let Ok(caller_ir) = self.ir_provider.enclosing_scope_ir(new_syn) else {
            return;
        };
        let Some(new_v) = find_call_vertex_by_syntax(&caller_ir, new_syn) else {
            return;
        };
        let args = call_vertex_args(new_v);

        let anchor = find_timeline_pos(
            &caller_ir,
            new_v.id,
            is_unconditional_in_scope(&caller_ir, new_v.id),
            timeline,
        );
        if timeline_excludes_write(&caller_ir, anchor, timeline) {
            return;
        }

        let Some(ctor_decl) = self.semantic.resolve_class_constructor(class_syn) else {
            return;
        };
        let Some(ctor_syn) = ctor_decl.primary_syntax else {
            return;
        };
        let Ok(ctor_ir) = self.ir_provider.function_ir(ctor_syn) else {
            return;
        };

        let mut ctor_stack = host_captured_stack.to_vec();
        ctor_stack.push(IrCallFrame {
            call_syntax: new_syn,
            call_location: new_v.location.clone(),
            caller_ir: Arc::clone(&caller_ir),
            callee_ir: Arc::clone(&ctor_ir),
            receiver: None,
            args: args.to_vec(),
            is_construct: true,
            closure_env: Vec::new(),
        });

        self.collect_writes_inside_ctor(
            &ctor_ir,
            &ctor_stack,
            prop_name,
            is_private,
            anchor,
            matched,
            0,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Recursively traverses `super(...)` chain for inherited constructor field initializers."
    )]
    fn collect_writes_inside_ctor(
        &mut self,
        ctor_ir: &Arc<FunctionIr>,
        ctor_stack: &[IrCallFrame],
        prop_name: &str,
        is_private: bool,
        anchor: Option<(usize, VertexId, bool)>,
        matched: &mut Vec<MatchedPropWrite>,
        super_depth: usize,
    ) {
        if super_depth > MAX_CALL_STACK_DEPTH {
            return;
        }
        for v in &ctor_ir.vertices {
            if let IrOp::PropWrite {
                object, key, value, ..
            } = &v.op
                && let Some((w_name, w_priv)) = static_prop_key(key)
                && w_name == prop_name
                && w_priv == is_private
                && find_value_producer_vertex(ctor_ir, *object, Some(v.id))
                    .is_some_and(|ov| matches!(ov.op, IrOp::ThisRef))
            {
                matched.push(MatchedPropWrite {
                    scope_ir: Arc::clone(ctor_ir),
                    write_vertex: v.id,
                    rhs: *value,
                    rhs_stack: ctor_stack.to_vec(),
                    anchor_in_timeline: anchor,
                });
            } else if !is_private
                && let IrOp::Call {
                    call_kind: IrCallKind::SuperCall,
                    args,
                    ..
                } = &v.op
                && let Some(super_syn) = v.syntax
                && let Some(base_ctor) = self.semantic.resolve_super_target(super_syn)
                && let Some(base_syn) = base_ctor.primary_syntax
                && let Ok(base_ir) = self.ir_provider.function_ir(base_syn)
            {
                let mut next_stack = ctor_stack.to_vec();
                next_stack.push(IrCallFrame {
                    call_syntax: super_syn,
                    call_location: v.location.clone(),
                    caller_ir: Arc::clone(ctor_ir),
                    callee_ir: Arc::clone(&base_ir),
                    receiver: None,
                    args: args.clone(),
                    is_construct: true,
                    closure_env: Vec::new(),
                });
                self.collect_writes_inside_ctor(
                    &base_ir,
                    &next_stack,
                    prop_name,
                    false,
                    anchor,
                    matched,
                    super_depth + 1,
                );
            }
        }
    }

    fn match_this_method_writes(
        &mut self,
        host: &HostOrigin,
        site: &IndexedPropWrite,
        timeline: &[TimelineAnchor],
        depth: usize,
        active: &mut Vec<VisitKey>,
        matched: &mut Vec<MatchedPropWrite>,
    ) {
        let host_key = &host.origin.instance_key;
        if host_key.starts_with("this:") {
            if let Some(hc) = host.class_syntax
                && self.find_enclosing_class_syntax(site.scope_ir.root_syntax) == Some(hc)
            {
                matched.push(MatchedPropWrite {
                    scope_ir: Arc::clone(&site.scope_ir),
                    write_vertex: site.vertex,
                    rhs: site.rhs,
                    rhs_stack: Vec::new(),
                    anchor_in_timeline: None,
                });
            }
            return;
        }

        let call_sites = self.collect_caller_syntaxes(site.scope_ir.root_syntax);
        for call_syn in call_sites {
            let Ok(caller_ir) = self.ir_provider.enclosing_scope_ir(call_syn) else {
                continue;
            };
            let Some(call_v) = find_call_vertex_by_syntax(&caller_ir, call_syn) else {
                continue;
            };
            let recv_val = match &call_v.op {
                IrOp::Call {
                    receiver: Some(r), ..
                } => *r,
                _ => continue,
            };
            let recv_res = self.trace_value(
                &caller_ir,
                recv_val,
                Some(call_v.id),
                &[],
                depth + 1,
                active,
            );
            if self.conflict.is_some() {
                return;
            }
            if !recv_res
                .hosts
                .iter()
                .any(|h| h.origin.instance_key == *host_key)
            {
                continue;
            }
            let is_unambiguous = recv_res.hosts.len() == 1
                && recv_res.gaps.is_empty()
                && is_unconditional_in_scope(&caller_ir, call_v.id)
                && is_unconditional_in_scope(&site.scope_ir, site.vertex);
            let anchor = find_timeline_pos(&caller_ir, call_v.id, is_unambiguous, timeline);
            if timeline_excludes_write(&caller_ir, anchor, timeline) {
                continue;
            }
            let frame = IrCallFrame {
                call_syntax: call_syn,
                call_location: call_v.location.clone(),
                caller_ir: Arc::clone(&caller_ir),
                callee_ir: Arc::clone(&site.scope_ir),
                receiver: Some(ReceiverBinding::PreResolved(recv_res.hosts)),
                args: call_vertex_args(call_v).to_vec(),
                is_construct: false,
                closure_env: Vec::new(),
            };
            matched.push(MatchedPropWrite {
                scope_ir: Arc::clone(&site.scope_ir),
                write_vertex: site.vertex,
                rhs: site.rhs,
                rhs_stack: vec![frame],
                anchor_in_timeline: anchor,
            });
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Matches `param.field = rhs` writes inside helper functions across callers."
    )]
    fn match_param_prop_writes(
        &mut self,
        host: &HostOrigin,
        site: &IndexedPropWrite,
        param_idx: usize,
        timeline: &[TimelineAnchor],
        depth: usize,
        active: &mut Vec<VisitKey>,
        matched: &mut Vec<MatchedPropWrite>,
    ) {
        let host_key = &host.origin.instance_key;
        let call_sites = self.collect_caller_syntaxes(site.scope_ir.root_syntax);
        for call_syn in call_sites {
            let Ok(caller_ir) = self.ir_provider.enclosing_scope_ir(call_syn) else {
                continue;
            };
            let Some(call_v) = find_call_vertex_by_syntax(&caller_ir, call_syn) else {
                continue;
            };
            let args = call_vertex_args(call_v);
            let Some(&arg) = args.get(param_idx) else {
                continue;
            };
            let arg_res = self.trace_call_arg(&caller_ir, call_v.id, arg, &[], depth + 1, active);
            if self.conflict.is_some() {
                return;
            }
            if !arg_res
                .hosts
                .iter()
                .any(|h| h.origin.instance_key == *host_key)
            {
                continue;
            }
            let is_unambiguous = arg_res.hosts.len() == 1
                && arg_res.gaps.is_empty()
                && is_unconditional_in_scope(&caller_ir, call_v.id)
                && is_unconditional_in_scope(&site.scope_ir, site.vertex);
            let anchor = find_timeline_pos(&caller_ir, call_v.id, is_unambiguous, timeline);
            if timeline_excludes_write(&caller_ir, anchor, timeline) {
                continue;
            }
            let frame = IrCallFrame {
                call_syntax: call_syn,
                call_location: call_v.location.clone(),
                caller_ir: Arc::clone(&caller_ir),
                callee_ir: Arc::clone(&site.scope_ir),
                receiver: None,
                args: args.to_vec(),
                is_construct: false,
                closure_env: Vec::new(),
            };
            matched.push(MatchedPropWrite {
                scope_ir: Arc::clone(&site.scope_ir),
                write_vertex: site.vertex,
                rhs: site.rhs,
                rhs_stack: vec![frame],
                anchor_in_timeline: anchor,
            });
        }
    }

    fn collect_caller_syntaxes(&mut self, fn_syn: SyntaxId) -> Vec<SyntaxId> {
        let target_symbol = self.semantic.symbol_at_syntax(fn_syn);
        let allowed_files = self.allowed_files;
        let Some(graph) = self.call_graph() else {
            return Vec::new();
        };
        let mut syntaxes = Vec::new();
        let mut seen = FxHashSet::default();
        if let Some(sym) = target_symbol
            && let Some(indices) = graph.by_symbol.get(&sym)
        {
            for &idx in indices {
                if let Some(allowed) = allowed_files
                    && !allowed.contains(&graph.sites[idx].file)
                {
                    continue;
                }
                let s = graph.sites[idx].call_syntax;
                if seen.insert(s) {
                    syntaxes.push(s);
                }
            }
        }
        if let Some(indices) = graph.by_decl.get(&fn_syn) {
            for &idx in indices {
                if let Some(allowed) = allowed_files
                    && !allowed.contains(&graph.sites[idx].file)
                {
                    continue;
                }
                let s = graph.sites[idx].call_syntax;
                if seen.insert(s) {
                    syntaxes.push(s);
                }
            }
        }
        syntaxes.sort_by_key(|s| (s.file_id().0, s.local_id().0));
        syntaxes
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Dispatches calls across behavior summaries, exact-receiver methods, source functions, and closure values."
    )]
    fn trace_call(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_kind: IrCallKind,
        callee: ValueId,
        receiver: Option<ValueId>,
        args: &[IrCallArg],
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let Some(call_syntax) = call_vertex.syntax else {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::NotACall,
                "Call vertex has no source syntax reference.".to_string(),
                call_vertex.location.clone(),
            ));
            return out;
        };

        if let Some(missing_pkg) = self.semantic.check_call_missing_package_gap(call_syntax) {
            let mut out = TraceResult::default();
            out.push_gap(missing_pkg);
            return out;
        }

        // 1. Check direct behavior summary for the call site / resolved declaration first
        //    (summaries override source bodies and resolve external/interface calls cleanly).
        if let Some((identity, matched)) =
            self.resolve_call_instances_summary(caller_ir, call_vertex, call_syntax, callee, false)
        {
            return self.apply_instances_summary(
                caller_ir,
                call_vertex,
                call_syntax,
                receiver,
                args,
                call_stack,
                &identity,
                &matched,
                depth,
                active,
            );
        }
        if self.conflict.is_some() {
            return TraceResult::default();
        }

        // 2. If `callee` is a local closure/function value (e.g. `g1()` from `makeGetter()`,
        //    or callback parameter `cb()`), invoke its closure body directly preserving `closure_env`.
        if matches!(call_kind, IrCallKind::Explicit | IrCallKind::TaggedTemplate) {
            let callee_producer =
                find_value_producer_vertex(caller_ir, callee, Some(call_vertex.id));
            let is_prop_callee =
                callee_producer.is_some_and(|v| matches!(v.op, IrOp::PropRead { .. }));
            if !is_prop_callee {
                let callee_res = self.trace_value(
                    caller_ir,
                    callee,
                    Some(call_vertex.id),
                    call_stack,
                    depth + 1,
                    active,
                );
                if self.conflict.is_some() {
                    return callee_res;
                }
                let closures: Vec<&HostOrigin> = callee_res
                    .hosts
                    .iter()
                    .filter(|h| h.closure_fn.is_some())
                    .collect();
                if !closures.is_empty() && callee_res.gaps.is_empty() {
                    let mut combined = TraceResult::default();
                    for ch in closures {
                        let fn_syn = ch.closure_fn.expect("filtered");
                        let sub = self.invoke_source_callable(
                            caller_ir,
                            call_vertex,
                            call_syntax,
                            fn_syn,
                            receiver.map(ReceiverBinding::Value),
                            args,
                            call_stack,
                            ch.captured_stack.clone(),
                            depth,
                            active,
                        );
                        combined.merge(sub);
                        if self.conflict.is_some() {
                            return combined;
                        }
                    }
                    return combined;
                }
            } else if let Some(prop_v) = callee_producer
                && let IrOp::PropRead {
                    object: recv_val,
                    key,
                    is_super: false,
                    ..
                } = &prop_v.op
                && let Some(member_name) = prop_key_static_name(key)
            {
                // Exact receiver check for `new Class().method(...)` or `{ method() {} }.method(...)`.
                if let Some(exact) = self.try_invoke_exact_receiver_method(
                    caller_ir,
                    call_vertex,
                    call_syntax,
                    *recv_val,
                    member_name,
                    args,
                    call_stack,
                    depth,
                    active,
                ) {
                    return exact;
                }
                if self.conflict.is_some() {
                    return TraceResult::default();
                }
            }
        }

        // 3. Fall back to Ticket 08's `call_resolution` (handles polymorphic overrides, getters,
        //    first-party workspace redirection, `Function.prototype.call/apply`, etc.).
        let call_targets = self.resolve_call_targets_cached(call_syntax);
        if let Some(conflict) = call_targets.conflict {
            self.conflict = Some(conflict);
            return TraceResult::default();
        }

        self.apply_call_targets_outcome(
            caller_ir,
            call_vertex,
            call_syntax,
            receiver,
            args,
            call_stack,
            &call_targets,
            depth,
            active,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Resolves methods/getters on concrete `new Class()` or object literal receivers without spurious open-hierarchy gaps."
    )]
    fn try_invoke_exact_receiver_method(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_syntax: SyntaxId,
        recv_val: ValueId,
        member_name: &str,
        args: &[IrCallArg],
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> Option<TraceResult> {
        let recv_res = self.trace_value(
            caller_ir,
            recv_val,
            Some(call_vertex.id),
            call_stack,
            depth + 1,
            active,
        );
        if self.conflict.is_some() {
            return Some(TraceResult::default());
        }
        if recv_res.hosts.is_empty() || !recv_res.gaps.is_empty() {
            return None;
        }
        // Only handle exact known `new Class()` or `ObjectAlloc` receivers here.
        let all_concrete = recv_res
            .hosts
            .iter()
            .all(|h| h.class_syntax.is_some() || h.object_alloc_props.is_some());
        if !all_concrete {
            return None;
        }

        let mut combined = TraceResult::default();
        for host in &recv_res.hosts {
            if let Some(class_syn) = host.class_syntax {
                let class_sym = self.semantic.symbol_at_syntax(class_syn)?;
                let hierarchy = self.semantic.class_hierarchy_for_symbol(class_sym)?;
                let overrides = class_overrides::resolve_member_overrides(
                    &hierarchy,
                    member_name,
                    MemberAccessKind::MethodOrProp,
                    call_vertex.location.as_ref(),
                    true,
                );
                if overrides.targets.is_empty() {
                    return None;
                }
                for ov in &overrides.targets {
                    if let Some(identity) = self.semantic.declaration_identity(ov.target_syntax) {
                        match self
                            .summary_index
                            .resolve_instances_summary(&identity, false)
                        {
                            Ok(Some(matched)) => {
                                let sub = self.apply_instances_summary(
                                    caller_ir,
                                    call_vertex,
                                    call_syntax,
                                    Some(recv_val),
                                    args,
                                    call_stack,
                                    &identity,
                                    &matched,
                                    depth,
                                    active,
                                );
                                combined.merge(sub);
                                continue;
                            }
                            Ok(None) => {}
                            Err(conflict) => {
                                self.conflict = Some(conflict);
                                return Some(combined);
                            }
                        }
                    }
                    if ov.is_external || !ov.has_body {
                        return None;
                    }
                    let sub = self.invoke_source_callable(
                        caller_ir,
                        call_vertex,
                        call_syntax,
                        ov.target_syntax,
                        Some(ReceiverBinding::PreResolved(vec![host.clone()])),
                        args,
                        call_stack,
                        Vec::new(),
                        depth,
                        active,
                    );
                    combined.merge(sub);
                }
            } else if let Some(props) = &host.object_alloc_props
                && let Some(scope_ir) = &host.host_scope_ir
            {
                let mut found_val = None;
                for p in props.iter().rev() {
                    if let IrObjectProp::KeyValue { key, value } = p
                        && prop_key_static_name(key) == Some(member_name)
                    {
                        found_val = Some(*value);
                        break;
                    }
                }
                let val = found_val?;
                let fn_res =
                    self.trace_value(scope_ir, val, None, &host.captured_stack, depth + 1, active);
                let closures: Vec<&HostOrigin> = fn_res
                    .hosts
                    .iter()
                    .filter(|h| h.closure_fn.is_some())
                    .collect();
                if closures.is_empty() {
                    return None;
                }
                for ch in closures {
                    let sub = self.invoke_source_callable(
                        caller_ir,
                        call_vertex,
                        call_syntax,
                        ch.closure_fn.expect("filtered"),
                        Some(ReceiverBinding::PreResolved(vec![host.clone()])),
                        args,
                        call_stack,
                        ch.captured_stack.clone(),
                        depth,
                        active,
                    );
                    combined.merge(sub);
                }
            }
        }
        Some(combined)
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Applies CallTargetsOutcome targets, instances summaries, and missing-summary / hierarchy gaps."
    )]
    fn apply_call_targets_outcome(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_syntax: SyntaxId,
        receiver: Option<ValueId>,
        args: &[IrCallArg],
        call_stack: &[IrCallFrame],
        call_targets: &call_resolution::CallTargetsOutcome,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let mut combined = TraceResult::default();
        for ev in &call_targets.evidence {
            combined.push_evidence(ev.clone());
        }

        // Preserve non-`MissingBehaviorSummary` gaps (e.g., `DynamicCallee`, `MissingSourceFile`,
        // `OpenExternalHierarchy`, `MissingPackage`). We perform our own `instances`-aspect summary
        // check below so a call with an `instances` summary is not downgraded by a missing `calls` summary.
        for g in &call_targets.gaps {
            if g.kind != UnknownReason::MissingBehaviorSummary.as_gap_kind() {
                combined.push_gap(g.clone());
            }
        }

        if call_targets.targets.is_empty() {
            if let Some(decl) = self.semantic.resolve_declaration(call_syntax)
                && (decl.is_external || decl.is_default_library)
            {
                let label = decl.name.as_deref().unwrap_or("external declaration");
                self.semantic.record_summary_lookup(None);
                combined.gaps.retain(|g| {
                    g.kind != UnknownReason::OpenExternalHierarchy.as_gap_kind()
                        && g.kind != UnknownReason::DynamicCallee.as_gap_kind()
                });
                combined.push_gap(GapItem::from_reason(
                    UnknownReason::MissingBehaviorSummary,
                    format!("External call '{label}' has no 'instances' behavior summary."),
                    call_vertex.location.clone(),
                ));
                combined.push_evidence(EvidenceItem::simple(
                    "note",
                    format!(
                        "Looked up 'instances' behavior summary for '{label}', but none matched."
                    ),
                    call_vertex.location.clone(),
                ));
            } else if combined.gaps.is_empty() {
                combined.push_gap(GapItem::from_reason(
                    UnknownReason::DynamicCallee,
                    "Call target could not be statically resolved.".to_string(),
                    call_vertex.location.clone(),
                ));
            }
            return combined;
        }

        for target in &call_targets.targets {
            if let Some(identity) = self
                .semantic
                .declaration_identity(target.declaration_syntax)
            {
                match self
                    .summary_index
                    .resolve_instances_summary(&identity, false)
                {
                    Ok(Some(matched)) => {
                        let sub = self.apply_instances_summary(
                            caller_ir,
                            call_vertex,
                            call_syntax,
                            receiver,
                            args,
                            call_stack,
                            &identity,
                            &matched,
                            depth,
                            active,
                        );
                        // Summary overrides open-hierarchy gap for that target.
                        combined.gaps.retain(|g| {
                            g.kind != UnknownReason::OpenExternalHierarchy.as_gap_kind()
                        });
                        combined.merge(sub);
                        if self.conflict.is_some() {
                            return combined;
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(conflict) => {
                        self.conflict = Some(conflict);
                        return combined;
                    }
                }
            }

            if (target.is_external && target.implementation.is_none()) || target.is_default_library
            {
                self.semantic.record_summary_lookup(None);
                let label = target.name.as_deref().unwrap_or("external declaration");
                combined.push_gap(GapItem::from_reason(
                    UnknownReason::MissingBehaviorSummary,
                    format!("External call '{label}' has no 'instances' behavior summary."),
                    call_vertex.location.clone(),
                ));
                combined.push_evidence(EvidenceItem::simple(
                    "note",
                    format!(
                        "Looked up 'instances' behavior summary for '{label}', but none matched."
                    ),
                    call_vertex.location.clone(),
                ));
                continue;
            }

            let Some(fn_syn) = target.fn_scope_syntax else {
                combined.push_gap(GapItem::from_reason(
                    UnknownReason::MissingFunctionBody,
                    format!(
                        "Target '{}' has no analyzable function body.",
                        target.name.as_deref().unwrap_or("<callable>")
                    ),
                    call_vertex.location.clone(),
                ));
                continue;
            };

            let recv_binding = receiver.map(ReceiverBinding::Value);
            let sub = self.invoke_source_callable(
                caller_ir,
                call_vertex,
                call_syntax,
                fn_syn,
                recv_binding,
                args,
                call_stack,
                Vec::new(),
                depth,
                active,
            );
            combined.merge(sub);
            if self.conflict.is_some() {
                return combined;
            }
        }

        combined
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Executes a source function/method/getter IR body and collects its reachable return outcomes."
    )]
    fn invoke_source_callable(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_syntax: SyntaxId,
        fn_scope_syntax: SyntaxId,
        receiver: Option<ReceiverBinding>,
        args: &[IrCallArg],
        call_stack: &[IrCallFrame],
        closure_env: Vec<IrCallFrame>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let target_ir = match self.ir_provider.function_ir(fn_scope_syntax) {
            Ok(ir) => ir,
            Err(gap) => {
                let mut out = TraceResult::default();
                out.push_gap(gap);
                return out;
            }
        };

        let mut next_stack = call_stack.to_vec();
        next_stack.push(IrCallFrame {
            call_syntax,
            call_location: call_vertex.location.clone(),
            caller_ir: Arc::clone(caller_ir),
            callee_ir: Arc::clone(&target_ir),
            receiver,
            args: args.to_vec(),
            is_construct: false,
            closure_env,
        });

        let (reachable_returns, implicit_void) = collect_reachable_returns(&target_ir);
        let mut combined = TraceResult::default();
        for (ret_v_id, ret_val_opt, ret_loc) in reachable_returns {
            if let Some(ret_val) = ret_val_opt {
                let sub = self.trace_value(
                    &target_ir,
                    ret_val,
                    Some(ret_v_id),
                    &next_stack,
                    depth + 1,
                    active,
                );
                combined.merge(sub);
            } else {
                let ev = EvidenceItem::simple(
                    "source",
                    "Explicit bare return yields undefined rather than an object instance."
                        .to_string(),
                    ret_loc,
                );
                combined
                    .non_instances
                    .push((NonInstanceKind::Nullish, ev.clone()));
                combined.push_evidence(ev);
            }
            if self.conflict.is_some() {
                return combined;
            }
        }
        if implicit_void {
            let ev = EvidenceItem::simple(
                "source",
                "Function path exits without returning an object instance.".to_string(),
                Some(target_ir.scope_location.clone()),
            );
            combined
                .non_instances
                .push((NonInstanceKind::Nullish, ev.clone()));
            combined.push_evidence(ev);
        }

        if let Some(cloc) = call_vertex.location.clone() {
            combined.prepend_step(&EvidenceItem::simple(
                "source",
                "Returned from call".to_string(),
                Some(cloc),
            ));
        }

        combined
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Traces a `new Class(...)` construct expression including summaries and dynamic callees."
    )]
    fn trace_construct(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        new_vertex: &CfgVertex,
        callee: ValueId,
        args: &[IrCallArg],
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let Some(new_syntax) = new_vertex.syntax else {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::NotACall,
                "Construct vertex has no syntax reference.".to_string(),
                new_vertex.location.clone(),
            ));
            return out;
        };

        if let Some((identity, matched)) =
            self.resolve_call_instances_summary(caller_ir, new_vertex, new_syntax, callee, true)
        {
            return self.apply_instances_summary(
                caller_ir, new_vertex, new_syntax, None, args, call_stack, &identity, &matched,
                depth, active,
            );
        }
        if self.conflict.is_some() {
            return TraceResult::default();
        }

        let call_targets = self.resolve_call_targets_cached(new_syntax);
        if let Some(conflict) = call_targets.conflict {
            self.conflict = Some(conflict);
            return TraceResult::default();
        }

        if call_targets
            .gaps
            .iter()
            .any(|g| g.kind == UnknownReason::DynamicCallee.as_gap_kind())
            && call_targets.targets.is_empty()
        {
            let mut out = TraceResult::default();
            for g in call_targets.gaps {
                out.push_gap(g);
            }
            return out;
        }

        let class_syn = self.resolve_constructed_class_syntax(new_syntax);
        let class_name = class_syn
            .and_then(|s| self.semantic.resolve_declaration(s))
            .and_then(|d| d.name)
            .or_else(|| call_targets.targets.first().and_then(|t| t.name.clone()))
            .unwrap_or_else(|| "instance".to_string());

        let mut out = TraceResult::default();
        out.hosts.push(self.make_alloc_host(
            Some(new_syntax),
            new_vertex.location.clone(),
            call_stack,
            &format!("Allocated new {class_name} instance."),
            class_syn,
        ));
        out
    }

    fn resolve_call_instances_summary(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_syntax: SyntaxId,
        callee: ValueId,
        is_construct: bool,
    ) -> Option<(
        DeclarationIdentity,
        MatchedSummaryAspect<InstanceAspectSummary>,
    )> {
        let mut lookup_syns = vec![call_syntax];
        if let Some(callee_v) = find_value_producer_vertex(caller_ir, callee, Some(call_vertex.id))
            && let Some(callee_syn) = callee_v.syntax
            && !lookup_syns.contains(&callee_syn)
        {
            lookup_syns.push(callee_syn);
        }
        if let Some(decl_syn) = self
            .semantic
            .resolve_declaration(call_syntax)
            .and_then(|d| d.primary_syntax)
            && !lookup_syns.contains(&decl_syn)
        {
            lookup_syns.push(decl_syn);
        }

        for syn in lookup_syns {
            let Some(identity) = self.semantic.declaration_identity(syn) else {
                continue;
            };
            match self
                .summary_index
                .resolve_instances_summary(&identity, is_construct)
            {
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

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Applies AllocatesFreshInstance, ForwardsInstance, or None from a matched InstanceAspectSummary."
    )]
    fn apply_instances_summary(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_syntax: SyntaxId,
        receiver: Option<ValueId>,
        args: &[IrCallArg],
        call_stack: &[IrCallFrame],
        identity: &DeclarationIdentity,
        matched: &MatchedSummaryAspect<InstanceAspectSummary>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        self.semantic.record_summary_lookup(Some(matched.source));
        let target_label = describe_identity_short(identity);
        let summary_ev = EvidenceItem::summary(
            format!(
                "Applied {} behavior summary '{}' for '{}'.",
                matched.source.as_str(),
                matched.pack_id,
                target_label
            ),
            call_vertex.location.clone(),
            matched.source.as_str().to_string(),
            matched.pack_id.clone(),
            Some(target_label.clone()),
        );

        let mut out = TraceResult::default();
        if matched.aspect.completeness == SummaryCompleteness::Partial {
            out.push_gap(GapItem::from_reason(
                UnknownReason::MissingBehaviorSummary,
                format!(
                    "Behavior summary '{}' for '{target_label}' is partial on the 'instances' aspect.",
                    matched.pack_id
                ),
                call_vertex.location.clone(),
            ));
        }

        match &matched.aspect.outcome {
            InstanceOutcome::AllocatesFreshInstance => {
                let loc = call_vertex.location.clone();
                let span_key = self.format_syntax_span_key(call_syntax, loc.as_ref());
                let ctx = self.format_context_suffix(call_stack);
                let key = format!("summary:{}:{span_key}{ctx}", matched.pack_id);
                let src_ev = EvidenceItem::simple(
                    "source",
                    format!("Summary-allocated instance via '{target_label}'."),
                    loc.clone(),
                );
                out.hosts.push(HostOrigin {
                    origin: ResolvedInstanceOrigin {
                        instance_key: key,
                        origin_kind: InstanceOriginKind::SummaryAllocation,
                        allocation_location: loc,
                        origin_syntax: Some(call_syntax),
                        summary_id: Some(matched.pack_id.clone()),
                        summary_source: Some(matched.source),
                        evidence: vec![summary_ev.clone(), src_ev],
                    },
                    captured_stack: call_stack.to_vec(),
                    class_syntax: None,
                    closure_fn: None,
                    host_scope_ir: None,
                    object_alloc_props: None,
                    array_alloc_elems: None,
                });
                out.push_evidence(summary_ev);
            }
            InstanceOutcome::ForwardsInstance { from } => {
                let mut forwarded = match from {
                    SummarySlotRef::Receiver(_) => receiver.map_or_else(
                        || {
                            let mut err = TraceResult::default();
                            err.push_gap(GapItem::from_reason(
                                UnknownReason::UnresolvedDeclaration,
                                format!(
                                    "Summary for '{target_label}' forwards receiver, but call has no receiver."
                                ),
                                call_vertex.location.clone(),
                            ));
                            err
                        },
                        |recv_val| {
                            self.trace_value(
                                caller_ir,
                                recv_val,
                                Some(call_vertex.id),
                                call_stack,
                                depth + 1,
                                active,
                            )
                        },
                    ),
                    SummarySlotRef::Arg { arg_index } => {
                        if let Some(&arg) = args.get(*arg_index) {
                            self.trace_call_arg(
                                caller_ir,
                                call_vertex.id,
                                arg,
                                call_stack,
                                depth + 1,
                                active,
                            )
                        } else {
                            let mut err = TraceResult::default();
                            err.push_gap(GapItem::from_reason(
                                UnknownReason::ParameterOutOfBounds,
                                format!(
                                    "Summary for '{target_label}' forwards argument {arg_index}, which was not provided."
                                ),
                                call_vertex.location.clone(),
                            ));
                            err
                        }
                    }
                };
                forwarded.prepend_step(&summary_ev);
                out.merge(forwarded);
            }
            InstanceOutcome::None => {
                out.non_instances
                    .push((NonInstanceKind::SummaryNone, summary_ev.clone()));
                out.push_evidence(summary_ev);
            }
        }
        out
    }

    fn trace_iterator_next(
        &mut self,
        ir: &Arc<FunctionIr>,
        next_vertex: &CfgVertex,
        iterator_val: ValueId,
        call_stack: &[IrCallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceResult {
        let Some(init_v) = find_value_producer_vertex(ir, iterator_val, Some(next_vertex.id))
        else {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::UnmodeledConstruct,
                "Iterator step has no matching IteratorInit producer.".to_string(),
                next_vertex.location.clone(),
            ));
            return out;
        };
        let IrOp::IteratorInit { iterable, .. } = init_v.op else {
            let mut out = TraceResult::default();
            out.push_gap(GapItem::from_reason(
                UnknownReason::UnmodeledConstruct,
                "Iterator source is not an IteratorInit vertex.".to_string(),
                next_vertex.location.clone(),
            ));
            return out;
        };

        let iter_res =
            self.trace_value(ir, iterable, Some(init_v.id), call_stack, depth + 1, active);
        if iter_res.hosts.is_empty() {
            return iter_res;
        }
        let mut combined = TraceResult {
            gaps: iter_res.gaps,
            evidence: iter_res.evidence,
            ..TraceResult::default()
        };
        for host in &iter_res.hosts {
            if let Some(elems) = &host.array_alloc_elems
                && let Some(scope_ir) = &host.host_scope_ir
            {
                for elem in elems {
                    if let IrArrayElem::Value(v) = elem {
                        let sub = self.trace_value(
                            scope_ir,
                            *v,
                            None,
                            &host.captured_stack,
                            depth + 1,
                            active,
                        );
                        combined.merge(sub);
                    }
                }
            } else {
                combined.push_gap(GapItem::from_reason(
                    UnknownReason::UnmodeledConstruct,
                    "Iterable container elements could not be statically unpacked.".to_string(),
                    next_vertex.location.clone(),
                ));
            }
        }
        combined
    }

    fn make_alloc_host(
        &self,
        origin_syntax: Option<SyntaxId>,
        loc: Option<DiagnosticLocation>,
        call_stack: &[IrCallFrame],
        message: &str,
        class_syntax: Option<SyntaxId>,
    ) -> HostOrigin {
        let span_key = origin_syntax.map_or_else(
            || "<unknown>".to_string(),
            |s| self.format_syntax_span_key(s, loc.as_ref()),
        );
        let ctx = self.format_context_suffix(call_stack);
        let key = format!("alloc:{span_key}{ctx}");
        let ev = EvidenceItem::simple("source", message.to_string(), loc.clone());
        HostOrigin {
            origin: ResolvedInstanceOrigin {
                instance_key: key,
                origin_kind: InstanceOriginKind::Allocation,
                allocation_location: loc,
                origin_syntax,
                summary_id: None,
                summary_source: None,
                evidence: vec![ev],
            },
            captured_stack: call_stack.to_vec(),
            class_syntax,
            closure_fn: None,
            host_scope_ir: None,
            object_alloc_props: None,
            array_alloc_elems: None,
        }
    }

    fn make_closure_host(&self, fn_syntax: SyntaxId, call_stack: &[IrCallFrame]) -> HostOrigin {
        let loc = self.syntax.syntax_location(fn_syntax);
        let span_key = self.format_syntax_span_key(fn_syntax, loc.as_ref());
        let ctx = self.format_context_suffix(call_stack);
        let key = format!("closure:{span_key}{ctx}");
        let ev = EvidenceItem::simple(
            "source",
            "Created closure or function object instance.".to_string(),
            loc.clone(),
        );
        HostOrigin {
            origin: ResolvedInstanceOrigin {
                instance_key: key,
                origin_kind: InstanceOriginKind::Allocation,
                allocation_location: loc,
                origin_syntax: Some(fn_syntax),
                summary_id: None,
                summary_source: None,
                evidence: vec![ev],
            },
            captured_stack: call_stack.to_vec(),
            class_syntax: None,
            closure_fn: Some(fn_syntax),
            host_scope_ir: None,
            object_alloc_props: None,
            array_alloc_elems: None,
        }
    }

    fn make_parameter_host(
        &self,
        param_syntax: SyntaxId,
        param_name: &str,
        loc: Option<DiagnosticLocation>,
    ) -> HostOrigin {
        let span_key = self.format_syntax_span_key(param_syntax, loc.as_ref());
        let key = format!("param:{span_key}");
        let ev = EvidenceItem::simple(
            "source",
            format!("Unbound function parameter '{param_name}'."),
            loc.clone(),
        );
        HostOrigin {
            origin: ResolvedInstanceOrigin {
                instance_key: key,
                origin_kind: InstanceOriginKind::Parameter,
                allocation_location: loc,
                origin_syntax: Some(param_syntax),
                summary_id: None,
                summary_source: None,
                evidence: vec![ev],
            },
            captured_stack: Vec::new(),
            class_syntax: None,
            closure_fn: None,
            host_scope_ir: None,
            object_alloc_props: None,
            array_alloc_elems: None,
        }
    }

    fn format_syntax_span_key(
        &self,
        syn: SyntaxId,
        loc_opt: Option<&DiagnosticLocation>,
    ) -> String {
        if let Some(loc) = loc_opt {
            return format!("{}:{}..{}", loc.file, loc.start, loc.end);
        }
        if let Some(loc) = self.syntax.syntax_location(syn) {
            return format!("{}:{}..{}", loc.file, loc.start, loc.end);
        }
        format!("syn:{}:{}", syn.file_id().0, syn.local_id().0)
    }

    fn format_context_suffix(&self, call_stack: &[IrCallFrame]) -> String {
        if call_stack.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = call_stack
            .iter()
            .map(|frame| {
                self.format_syntax_span_key(frame.call_syntax, frame.call_location.as_ref())
            })
            .collect();
        format!("@ctx[{}]", parts.join("->"))
    }

    fn resolve_constructed_class_syntax(&self, new_syntax: SyntaxId) -> Option<SyntaxId> {
        let ir = self.ir_provider.enclosing_scope_ir(new_syntax).ok()?;
        let new_v = find_call_vertex_by_syntax(&ir, new_syntax)?;
        let IrOp::Construct { callee, .. } = new_v.op else {
            return None;
        };
        let callee_v = find_value_producer_vertex(&ir, callee, Some(new_v.id))?;
        let callee_syn = callee_v.syntax?;
        let decl = self.semantic.resolve_declaration(callee_syn)?;
        if matches!(
            decl.kind.as_deref(),
            Some("ClassDeclaration" | "ClassExpression")
        ) {
            return decl.primary_syntax;
        }
        if let Some(ctor_syn) = decl.primary_syntax {
            return self.find_enclosing_class_syntax(ctor_syn);
        }
        None
    }

    fn find_enclosing_class_syntax(&self, node_syntax: SyntaxId) -> Option<SyntaxId> {
        let loc = self.syntax.syntax_location(node_syntax)?;
        let file = node_syntax.file_id();
        let mut best: Option<(u32, SyntaxId)> = None;
        for scope in self.ir_provider.scopes_in_file(file) {
            if (scope.span_start > loc.start || loc.end > scope.span_end)
                && scope.kind != IrScopeKind::ModuleTopLevel
            {
                continue;
            }
            if let Ok(ir) = self.ir_provider.function_ir(scope.root_syntax) {
                for v in &ir.vertices {
                    if let IrOp::ClassLiteral { syntax: cls_syn } = v.op
                        && let Some(cls_loc) = self.syntax.syntax_location(cls_syn)
                        && cls_loc.start <= loc.start
                        && loc.end <= cls_loc.end
                    {
                        let span = cls_loc.end.saturating_sub(cls_loc.start);
                        if best.is_none_or(|(best_span, _)| span < best_span) {
                            best = Some((span, cls_syn));
                        }
                    }
                }
            }
        }
        if let Some((_, cls_syn)) = best {
            return Some(cls_syn);
        }
        let decl = self.semantic.resolve_declaration(node_syntax)?;
        if matches!(
            decl.kind.as_deref(),
            Some("ClassDeclaration" | "ClassExpression")
        ) {
            return decl.primary_syntax;
        }
        None
    }
}

fn stack_keys(call_stack: &[IrCallFrame]) -> Vec<SyntaxId> {
    call_stack.iter().map(|f| f.call_syntax).collect()
}

fn find_stack_for_scope(
    call_stack: &[IrCallFrame],
    target_ir: FunctionIrId,
) -> Option<&[IrCallFrame]> {
    if let Some(pos) = call_stack.iter().rposition(|f| f.callee_ir.id == target_ir) {
        return Some(&call_stack[..=pos]);
    }
    for frame in call_stack.iter().rev() {
        if let Some(found) = find_stack_for_scope(&frame.closure_env, target_ir) {
            return Some(found);
        }
    }
    None
}

pub(crate) const fn static_prop_key(key: &IrPropKey) -> Option<(&str, bool)> {
    match key {
        IrPropKey::Named(name)
        | IrPropKey::Computed {
            static_name: Some(name),
            ..
        } => Some((name.as_str(), false)),
        IrPropKey::PrivateIdent(name) => Some((name.as_str(), true)),
        IrPropKey::Computed {
            static_name: None, ..
        } => None,
    }
}

fn classify_literal_non_instance(kind: &LiteralKind) -> NonInstanceKind {
    match kind {
        LiteralKind::Null | LiteralKind::Undefined => NonInstanceKind::Nullish,
        LiteralKind::Boolean(false) => NonInstanceKind::FalsyNonNullish,
        LiteralKind::Number(s) if matches!(s.as_str(), "0" | "0.0" | "NaN") => {
            NonInstanceKind::FalsyNonNullish
        }
        LiteralKind::BigInt(s) if s == "0n" || s == "0" => NonInstanceKind::FalsyNonNullish,
        LiteralKind::String(s) if s.is_empty() => NonInstanceKind::FalsyNonNullish,
        LiteralKind::Number(_)
        | LiteralKind::BigInt(_)
        | LiteralKind::String(_)
        | LiteralKind::Boolean(true)
        | LiteralKind::RegExp(_) => NonInstanceKind::TruthyOrUnknownPrimitive,
    }
}

pub(crate) fn find_value_producer_vertex(
    ir: &FunctionIr,
    value: ValueId,
    hint: Option<VertexId>,
) -> Option<&CfgVertex> {
    if let Some(vid) = hint
        && let Some(v) = ir.vertices.get(vid.0 as usize)
        && v.value == Some(value)
        && !matches!(
            v.op,
            IrOp::AssignLocal { is_init: true, .. } | IrOp::AssignSymbol { is_init: true, .. }
        )
    {
        return Some(v);
    }
    ir.vertices.iter().find(|v| {
        v.value == Some(value)
            && !matches!(
                v.op,
                IrOp::AssignLocal { is_init: true, .. } | IrOp::AssignSymbol { is_init: true, .. }
            )
    })
}

pub(crate) fn find_call_vertex_by_syntax(
    ir: &FunctionIr,
    call_syntax: SyntaxId,
) -> Option<&CfgVertex> {
    ir.vertices.iter().find(|v| {
        v.syntax == Some(call_syntax) && matches!(v.op, IrOp::Call { .. } | IrOp::Construct { .. })
    })
}

const fn call_vertex_args(vertex: &CfgVertex) -> &[IrCallArg] {
    match &vertex.op {
        IrOp::Call { args, .. } | IrOp::Construct { args, .. } => args.as_slice(),
        _ => &[],
    }
}

fn detect_merge_branch_kind(
    ir: &FunctionIr,
    merged_val: ValueId,
    hint: Option<VertexId>,
) -> Option<IrBranchKind> {
    let join_vid = find_value_producer_vertex(ir, merged_val, hint)?.id;
    for v in &ir.vertices {
        for edge in &v.explicit_edges {
            if edge.target == join_vid
                && let ExplicitEdgeKind::Branch(branch_id) = edge.kind
                && let Some(rec) = ir.branches.get(branch_id.0 as usize)
            {
                return Some(rec.kind);
            }
        }
    }
    None
}

type ReturnVertexEntry = (VertexId, Option<ValueId>, Option<DiagnosticLocation>);

fn collect_reachable_returns(ir: &FunctionIr) -> (Vec<ReturnVertexEntry>, bool) {
    let mut visited = FxHashSet::default();
    let mut queue = VecDeque::new();
    let mut returns = Vec::new();
    let mut implicit_void = false;

    if visited.insert(ir.entry) {
        queue.push_back(ir.entry);
    }
    while let Some(vid) = queue.pop_front() {
        let Some(v) = ir.vertices.get(vid.0 as usize) else {
            continue;
        };
        if let IrOp::Return { value } = v.op {
            returns.push((v.id, value, v.location.clone()));
            continue;
        }
        for edge in &v.explicit_edges {
            if edge.target == ir.normal_exit {
                implicit_void = true;
                continue;
            }
            if visited.insert(edge.target) {
                queue.push_back(edge.target);
            }
        }
    }
    (returns, implicit_void)
}

fn build_timeline_anchors(
    read_ir: &Arc<FunctionIr>,
    read_vertex: VertexId,
    call_stack: &[IrCallFrame],
) -> Vec<TimelineAnchor> {
    let mut anchors = Vec::with_capacity(1 + call_stack.len());
    anchors.push(TimelineAnchor {
        ir: Arc::clone(read_ir),
        vertex: read_vertex,
    });
    for frame in call_stack.iter().rev() {
        let call_vid = find_call_vertex_by_syntax(&frame.caller_ir, frame.call_syntax)
            .map_or(frame.caller_ir.normal_exit, |v| v.id);
        anchors.push(TimelineAnchor {
            ir: Arc::clone(&frame.caller_ir),
            vertex: call_vid,
        });
    }
    anchors
}

fn timeline_excludes_write(
    write_scope_ir: &Arc<FunctionIr>,
    anchor: Option<(usize, VertexId, bool)>,
    timeline: &[TimelineAnchor],
) -> bool {
    anchor.is_none() && timeline.iter().any(|a| a.ir.id == write_scope_ir.id)
}

fn find_timeline_pos(
    scope_ir: &Arc<FunctionIr>,
    write_vertex: VertexId,
    is_unconditional: bool,
    timeline: &[TimelineAnchor],
) -> Option<(usize, VertexId, bool)> {
    for (idx, anchor) in timeline.iter().enumerate() {
        if anchor.ir.id == scope_ir.id {
            if cfg_can_reach(
                &anchor.ir,
                write_vertex,
                anchor.vertex,
                &FxHashSet::default(),
            ) {
                return Some((idx, write_vertex, is_unconditional));
            }
            return None;
        }
    }
    None
}

fn filter_surviving_writes(
    writes: Vec<MatchedPropWrite>,
    timeline: &[TimelineAnchor],
) -> Vec<MatchedPropWrite> {
    let mut kills_by_anchor: FxHashMap<usize, FxHashSet<VertexId>> = FxHashMap::default();
    for w in &writes {
        if let Some((tl_idx, w_vid, true)) = w.anchor_in_timeline {
            kills_by_anchor.entry(tl_idx).or_default().insert(w_vid);
        }
    }

    writes
        .into_iter()
        .filter(|w| {
            let Some((tl_idx, w_vid, _)) = w.anchor_in_timeline else {
                return true;
            };
            let Some(kills) = kills_by_anchor.get(&tl_idx) else {
                return true;
            };
            if kills.is_empty() {
                return true;
            }
            let mut other_kills = kills.clone();
            other_kills.remove(&w_vid);
            if other_kills.is_empty() {
                return true;
            }
            cfg_can_reach_after_first_step(
                &timeline[tl_idx].ir,
                w_vid,
                timeline[tl_idx].vertex,
                &other_kills,
            )
        })
        .collect()
}

fn cfg_can_reach(
    ir: &FunctionIr,
    from: VertexId,
    target: VertexId,
    barriers: &FxHashSet<VertexId>,
) -> bool {
    if from == target {
        return true;
    }
    let mut visited = FxHashSet::default();
    let mut queue = VecDeque::new();
    visited.insert(from);
    queue.push_back(from);
    while let Some(vid) = queue.pop_front() {
        let Some(v) = ir.vertices.get(vid.0 as usize) else {
            continue;
        };
        for edge in &v.explicit_edges {
            if edge.target == target {
                return true;
            }
            if barriers.contains(&edge.target) {
                continue;
            }
            if visited.insert(edge.target) {
                queue.push_back(edge.target);
            }
        }
    }
    false
}

fn cfg_can_reach_after_first_step(
    ir: &FunctionIr,
    from: VertexId,
    target: VertexId,
    barriers: &FxHashSet<VertexId>,
) -> bool {
    if from == target {
        return true;
    }
    let mut visited = FxHashSet::default();
    let mut queue = VecDeque::new();
    if let Some(v) = ir.vertices.get(from.0 as usize) {
        for edge in &v.explicit_edges {
            if edge.target == target {
                return true;
            }
            if !barriers.contains(&edge.target) && visited.insert(edge.target) {
                queue.push_back(edge.target);
            }
        }
    }
    while let Some(vid) = queue.pop_front() {
        let Some(v) = ir.vertices.get(vid.0 as usize) else {
            continue;
        };
        for edge in &v.explicit_edges {
            if edge.target == target {
                return true;
            }
            if barriers.contains(&edge.target) {
                continue;
            }
            if visited.insert(edge.target) {
                queue.push_back(edge.target);
            }
        }
    }
    false
}

fn is_unconditional_in_scope(ir: &FunctionIr, target: VertexId) -> bool {
    !cfg_can_reach_avoiding_single(ir, ir.entry, ir.normal_exit, target)
}

fn cfg_can_reach_avoiding_single(
    ir: &FunctionIr,
    from: VertexId,
    target: VertexId,
    avoid: VertexId,
) -> bool {
    if from == avoid {
        return false;
    }
    if from == target {
        return true;
    }
    let mut visited = FxHashSet::default();
    let mut queue = VecDeque::new();
    visited.insert(from);
    queue.push_back(from);
    while let Some(vid) = queue.pop_front() {
        let Some(v) = ir.vertices.get(vid.0 as usize) else {
            continue;
        };
        for edge in &v.explicit_edges {
            if edge.target == avoid {
                continue;
            }
            if edge.target == target {
                return true;
            }
            if visited.insert(edge.target) {
                queue.push_back(edge.target);
            }
        }
    }
    false
}

fn describe_identity_short(identity: &DeclarationIdentity) -> String {
    let path = identity
        .member_paths
        .first()
        .map_or_else(|| "<symbol>".to_string(), |p| p.join("."));
    match &identity.package_name {
        Some(pkg) => format!("{pkg}:{path}"),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_classification_distinguishes_nullish_falsy_and_truthy() {
        assert_eq!(
            classify_literal_non_instance(&LiteralKind::Null),
            NonInstanceKind::Nullish
        );
        assert_eq!(
            classify_literal_non_instance(&LiteralKind::Undefined),
            NonInstanceKind::Nullish
        );
        assert_eq!(
            classify_literal_non_instance(&LiteralKind::Boolean(false)),
            NonInstanceKind::FalsyNonNullish
        );
        assert_eq!(
            classify_literal_non_instance(&LiteralKind::Number("42".to_string())),
            NonInstanceKind::TruthyOrUnknownPrimitive
        );
    }

    #[test]
    fn instance_origin_kind_serializes_as_kebab_case() {
        assert_eq!(
            serde_json::to_string(&InstanceOriginKind::SummaryAllocation).unwrap(),
            "\"summary-allocation\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceOriginKind::Allocation).unwrap(),
            "\"allocation\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceOriginKind::Parameter).unwrap(),
            "\"parameter\""
        );
    }
}
