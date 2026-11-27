use std::{
    collections::{BTreeSet, HashSet, VecDeque},
    hash::BuildHasher,
    sync::Arc,
};

use rustc_hash::{FxHashMap, FxHashSet};
use tssae_common::{
    DeclarationIdentity, DiagnosticLocation, EvidenceItem, FileId, FunctionIrId, GapItem,
    LocalBindingId, SemanticModel, SummaryCompleteness, SummaryConflictError, SummaryNamedValueRef,
    SummarySource, SummaryTaggedValueRef, SummaryValueRef, SymbolId, SyntaxCatalog, SyntaxId,
    UnknownReason, ValueAspectSummary, ValueId, VertexId,
};

use crate::{
    call_resolution::{
        IrCallGraph, build_ir_call_graph_with_summaries, prop_key_static_name,
        resolve_call_targets_with_summaries, value_syntax_in_ir,
    },
    ir::{
        AssignOp, BindingMutability, CfgVertex, ExplicitEdgeKind, FunctionIr, ImplicitAccessorKind,
        IrArrayElem, IrBranchKind, IrCallArg, IrCallKind, IrObjectProp, IrOp, IrProvider,
        IrScopeKind,
    },
    summaries::{BehaviorSummaryIndex, MatchedSummaryAspect},
    var_writes,
};

const MAX_PROP_STATES: usize = 8192;
const MAX_CALL_STACK_DEPTH: usize = 16;
const MAX_PATHS_PER_PAIR: usize = 8;
const MAX_FIELD_PATH_DEPTH: usize = 8;

#[derive(Default)]
struct DualQueue {
    untainted: VecDeque<TraceItem>,
    tainted: VecDeque<TraceItem>,
    hit_state_budget: Option<DiagnosticLocation>,
}

impl DualQueue {
    fn pop_next(&mut self) -> Option<TraceItem> {
        self.untainted
            .pop_front()
            .or_else(|| self.tainted.pop_front())
    }

    fn record_budget_hit(&mut self, loc: Option<DiagnosticLocation>) {
        if self.hit_state_budget.is_none() {
            self.hit_state_budget = loc;
        }
    }
}

fn push_field_segment(
    queue: &mut DualQueue,
    fields: &mut ValueFieldState,
    seg: FieldSegment,
    loc: Option<DiagnosticLocation>,
) -> bool {
    if fields.field_path.len() >= MAX_FIELD_PATH_DEPTH {
        queue.record_budget_hit(loc);
        return false;
    }
    fields.field_path.push(seg);
    true
}

/// Endpoint handle and optional canonical symbol passed into [`trace_value_propagation`].
#[derive(Clone, Copy, Debug)]
pub struct ValueEndpointSpec {
    pub syntax: SyntaxId,
    pub symbol: Option<SymbolId>,
}

/// One recorded transition step along a propagated value path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValuePropagationStepData {
    pub kind: &'static str,
    pub description: String,
    pub from_location: DiagnosticLocation,
    pub to_location: DiagnosticLocation,
    pub property: Option<String>,
    pub omitted_properties: Option<Vec<String>>,
}

/// One concrete witness path from `from` to `to`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValuePropagationPathData {
    pub steps: Vec<ValuePropagationStepData>,
    pub evidence: Vec<EvidenceItem>,
    pub source_path: Vec<String>,
    pub omitted_properties: Vec<String>,
}

/// Tri-state result for a single `(from_index, to_index)` endpoint pair.
#[derive(Clone, Debug)]
pub enum ValuePropagationTargetOutcome {
    Proved {
        paths: Vec<ValuePropagationPathData>,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        partial_paths: Vec<ValuePropagationPathData>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

/// Full IR value-propagation outcome across all `(from, to)` endpoint pairs.
#[derive(Clone, Debug)]
pub struct ValuePropagationOutcome {
    /// Pair results ordered row-major by `from_index * to_endpoints.len() + to_index`.
    pub results: Vec<ValuePropagationTargetOutcome>,
    /// Fatal same-tier behavior summary conflict, if encountered during lookup.
    pub conflict: Option<SummaryConflictError>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum FieldSegment {
    Prop(String),
    Element(Option<usize>),
}

#[derive(Clone, PartialEq, Eq, Hash, Default, Debug)]
struct ValueFieldState {
    field_path: Vec<FieldSegment>,
    source_path: Vec<String>,
    omitted_properties: BTreeSet<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum TraceNode {
    Value(ValueId),
    Binding(LocalBindingId),
    Vertex(VertexId),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum CallFrameKey {
    Direct {
        caller_ir_id: FunctionIrId,
        call_vertex: VertexId,
        is_construct: bool,
    },
    SummaryCallback {
        caller_ir_id: FunctionIrId,
        call_vertex: VertexId,
        callback_arg_index: usize,
    },
}

#[derive(Clone)]
enum CallFrameData {
    Direct {
        caller_ir: Arc<FunctionIr>,
        call_vertex: VertexId,
        call_location: Option<DiagnosticLocation>,
        receiver_binding: Option<LocalBindingId>,
        is_construct: bool,
    },
    SummaryCallback {
        caller_ir: Arc<FunctionIr>,
        call_vertex: VertexId,
        call_location: Option<DiagnosticLocation>,
        callback_arg_index: usize,
        summary_label: String,
        summary_source: SummarySource,
        summary_pack_id: String,
        values_aspect: ValueAspectSummary,
    },
}

impl CallFrameData {
    fn key(&self) -> CallFrameKey {
        match self {
            Self::Direct {
                caller_ir,
                call_vertex,
                is_construct,
                ..
            } => CallFrameKey::Direct {
                caller_ir_id: caller_ir.id,
                call_vertex: *call_vertex,
                is_construct: *is_construct,
            },
            Self::SummaryCallback {
                caller_ir,
                call_vertex,
                callback_arg_index,
                ..
            } => CallFrameKey::SummaryCallback {
                caller_ir_id: caller_ir.id,
                call_vertex: *call_vertex,
                callback_arg_index: *callback_arg_index,
            },
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct TraceKey {
    ir_id: FunctionIrId,
    node: TraceNode,
    fields: ValueFieldState,
    stack: Vec<CallFrameKey>,
    is_gap_tainted: bool,
}

#[derive(Clone)]
struct TraceItem {
    ir: Arc<FunctionIr>,
    node: TraceNode,
    location: Option<DiagnosticLocation>,
    fields: ValueFieldState,
    stack: Vec<CallFrameData>,
    steps: Vec<ValuePropagationStepData>,
    extra_evidence: Vec<EvidenceItem>,
    gap_indices: BTreeSet<usize>,
}

#[derive(Clone)]
struct ResolvedTargetEndpoint {
    syntax: SyntaxId,
    location: Option<DiagnosticLocation>,
    scope_id: Option<FunctionIrId>,
    expr_value: Option<ValueId>,
    binding_syntax: Option<SyntaxId>,
    symbol: Option<SymbolId>,
    is_decl_target: bool,
}

#[derive(Clone, Copy, Debug)]
enum UseRole {
    ValueForward {
        non_null_assert: bool,
    },
    Await,
    AssignLocal {
        target: LocalBindingId,
        op: AssignOp,
    },
    AssignSymbol {
        target_syntax: SyntaxId,
        op: AssignOp,
    },
    PropWriteValue {
        object: ValueId,
        op: AssignOp,
        implicit_call_kind: Option<ImplicitAccessorKind>,
    },
    PropReadObject {
        implicit_call_kind: Option<ImplicitAccessorKind>,
    },
    ObjectRestSource,
    ObjectAllocProp {
        prop_index: usize,
    },
    ObjectAllocSpread {
        prop_index: usize,
    },
    ArrayAllocElem {
        elem_index: usize,
    },
    ArrayAllocSpread {
        elem_index: usize,
    },
    IteratorInitIterable,
    IteratorNextIter,
    IteratorRestIter,
    CallCallee,
    CallReceiver,
    CallArg {
        arg_index: usize,
        is_spread: bool,
    },
    ConstructCallee,
    ConstructArg {
        arg_index: usize,
        is_spread: bool,
    },
    ReturnValue,
    YieldValue,
    UnknownChild,
}

#[derive(Clone, Copy, Debug)]
enum ValueConsumer {
    MergedInto {
        target_value: ValueId,
        vertex: Option<VertexId>,
    },
    VertexUse {
        vertex: VertexId,
        role: UseRole,
    },
}

#[derive(Clone, Copy, Debug)]
struct IteratorStepInfo {
    is_loop: bool,
    next_index: Option<usize>,
    rest_offset: usize,
}

struct IrValueIndex {
    consumers: FxHashMap<ValueId, Vec<ValueConsumer>>,
    reads_of_binding: FxHashMap<LocalBindingId, Vec<VertexId>>,
    assigns_of_binding: FxHashMap<LocalBindingId, Vec<VertexId>>,
    iterator_steps: FxHashMap<VertexId, IteratorStepInfo>,
    value_locations: FxHashMap<ValueId, DiagnosticLocation>,
}

impl IrValueIndex {
    #[expect(
        clippy::too_many_lines,
        reason = "Builds a single-pass per-FunctionIr index over all IrOp value consumers and iterator steps."
    )]
    fn build(ir: &FunctionIr) -> Self {
        let mut consumers: FxHashMap<ValueId, Vec<ValueConsumer>> = FxHashMap::default();
        let mut reads_of_binding: FxHashMap<LocalBindingId, Vec<VertexId>> = FxHashMap::default();
        let mut assigns_of_binding: FxHashMap<LocalBindingId, Vec<VertexId>> = FxHashMap::default();
        let mut value_locations: FxHashMap<ValueId, DiagnosticLocation> = FxHashMap::default();
        let mut value_producer_vertex: FxHashMap<ValueId, VertexId> = FxHashMap::default();
        let mut iterator_inits: Vec<(VertexId, ValueId)> = Vec::new();

        for vertex in &ir.vertices {
            if let Some(val) = vertex.value {
                if let Some(loc) = &vertex.location {
                    value_locations.entry(val).or_insert_with(|| loc.clone());
                }
                if !matches!(
                    vertex.op,
                    IrOp::AssignLocal { is_init: true, .. }
                        | IrOp::AssignSymbol { is_init: true, .. }
                ) {
                    value_producer_vertex.entry(val).or_insert(vertex.id);
                }
            }

            match &vertex.op {
                IrOp::Entry
                | IrOp::Exit
                | IrOp::NopMerge
                | IrOp::Literal { .. }
                | IrOp::ReadSymbol { .. }
                | IrOp::ThisRef
                | IrOp::SuperRef
                | IrOp::MetaProp
                | IrOp::CatchParamBind { .. }
                | IrOp::FunctionLiteral { .. }
                | IrOp::ClassLiteral { .. }
                | IrOp::TemplateString { .. }
                | IrOp::Unary { .. }
                | IrOp::Binary { .. }
                | IrOp::Branch { .. }
                | IrOp::SwitchDispatch { .. }
                | IrOp::Throw { .. }
                | IrOp::Yield { value: None, .. }
                | IrOp::IteratorClose { .. }
                | IrOp::JsxElement { .. }
                | IrOp::Return { value: None } => {}
                IrOp::Yield {
                    value: Some(val), ..
                } => {
                    consumers
                        .entry(*val)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::YieldValue,
                        });
                }
                IrOp::ReadLocal { binding } => {
                    reads_of_binding
                        .entry(*binding)
                        .or_default()
                        .push(vertex.id);
                }
                IrOp::ValueForward {
                    source,
                    non_null_assert,
                    ..
                } => {
                    consumers
                        .entry(*source)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::ValueForward {
                                non_null_assert: *non_null_assert,
                            },
                        });
                }
                IrOp::Await { promise } => {
                    consumers
                        .entry(*promise)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::Await,
                        });
                }
                IrOp::AssignLocal {
                    target, op, value, ..
                } => {
                    assigns_of_binding
                        .entry(*target)
                        .or_default()
                        .push(vertex.id);
                    consumers
                        .entry(*value)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::AssignLocal {
                                target: *target,
                                op: *op,
                            },
                        });
                }
                IrOp::AssignSymbol {
                    target_syntax,
                    op,
                    value,
                    ..
                } => {
                    consumers
                        .entry(*value)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::AssignSymbol {
                                target_syntax: *target_syntax,
                                op: *op,
                            },
                        });
                }
                IrOp::PropWrite {
                    object,
                    op,
                    value,
                    implicit_call_kind,
                    ..
                } => {
                    consumers
                        .entry(*value)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::PropWriteValue {
                                object: *object,
                                op: *op,
                                implicit_call_kind: *implicit_call_kind,
                            },
                        });
                }
                IrOp::PropRead {
                    object,
                    implicit_call_kind,
                    ..
                } => {
                    consumers
                        .entry(*object)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::PropReadObject {
                                implicit_call_kind: *implicit_call_kind,
                            },
                        });
                }
                IrOp::ObjectRest { source, .. } => {
                    consumers
                        .entry(*source)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::ObjectRestSource,
                        });
                }
                IrOp::ObjectAlloc { props, .. } => {
                    for (prop_index, prop) in props.iter().enumerate() {
                        match prop {
                            IrObjectProp::KeyValue { value, .. } => {
                                consumers.entry(*value).or_default().push(
                                    ValueConsumer::VertexUse {
                                        vertex: vertex.id,
                                        role: UseRole::ObjectAllocProp { prop_index },
                                    },
                                );
                            }
                            IrObjectProp::Spread(val) => {
                                consumers
                                    .entry(*val)
                                    .or_default()
                                    .push(ValueConsumer::VertexUse {
                                        vertex: vertex.id,
                                        role: UseRole::ObjectAllocSpread { prop_index },
                                    });
                            }
                        }
                    }
                }
                IrOp::ArrayAlloc { elems, .. } => {
                    for (elem_index, elem) in elems.iter().enumerate() {
                        match elem {
                            IrArrayElem::Value(val) => {
                                consumers
                                    .entry(*val)
                                    .or_default()
                                    .push(ValueConsumer::VertexUse {
                                        vertex: vertex.id,
                                        role: UseRole::ArrayAllocElem { elem_index },
                                    });
                            }
                            IrArrayElem::Spread(val) => {
                                consumers
                                    .entry(*val)
                                    .or_default()
                                    .push(ValueConsumer::VertexUse {
                                        vertex: vertex.id,
                                        role: UseRole::ArrayAllocSpread { elem_index },
                                    });
                            }
                            IrArrayElem::Elision => {}
                        }
                    }
                }
                IrOp::IteratorInit { iterable, .. } => {
                    if let Some(iter_val) = vertex.value {
                        iterator_inits.push((vertex.id, iter_val));
                    }
                    consumers
                        .entry(*iterable)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::IteratorInitIterable,
                        });
                }
                IrOp::IteratorNext { iterator, .. } => {
                    consumers
                        .entry(*iterator)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::IteratorNextIter,
                        });
                }
                IrOp::IteratorRest { iterator, .. } => {
                    consumers
                        .entry(*iterator)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::IteratorRestIter,
                        });
                }
                IrOp::Call {
                    callee,
                    receiver,
                    args,
                    ..
                } => {
                    consumers
                        .entry(*callee)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::CallCallee,
                        });
                    if let Some(recv) = receiver {
                        consumers
                            .entry(*recv)
                            .or_default()
                            .push(ValueConsumer::VertexUse {
                                vertex: vertex.id,
                                role: UseRole::CallReceiver,
                            });
                    }
                    for (arg_index, arg) in args.iter().enumerate() {
                        let (val, is_spread) = match arg {
                            IrCallArg::Positional(v) => (*v, false),
                            IrCallArg::Spread(v) => (*v, true),
                        };
                        consumers
                            .entry(val)
                            .or_default()
                            .push(ValueConsumer::VertexUse {
                                vertex: vertex.id,
                                role: UseRole::CallArg {
                                    arg_index,
                                    is_spread,
                                },
                            });
                    }
                }
                IrOp::Construct { callee, args, .. } => {
                    consumers
                        .entry(*callee)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::ConstructCallee,
                        });
                    for (arg_index, arg) in args.iter().enumerate() {
                        let (val, is_spread) = match arg {
                            IrCallArg::Positional(v) => (*v, false),
                            IrCallArg::Spread(v) => (*v, true),
                        };
                        consumers
                            .entry(val)
                            .or_default()
                            .push(ValueConsumer::VertexUse {
                                vertex: vertex.id,
                                role: UseRole::ConstructArg {
                                    arg_index,
                                    is_spread,
                                },
                            });
                    }
                }
                IrOp::Return { value: Some(val) } => {
                    consumers
                        .entry(*val)
                        .or_default()
                        .push(ValueConsumer::VertexUse {
                            vertex: vertex.id,
                            role: UseRole::ReturnValue,
                        });
                }
                IrOp::Unknown {
                    evaluated_children, ..
                } => {
                    for &child_val in evaluated_children {
                        consumers
                            .entry(child_val)
                            .or_default()
                            .push(ValueConsumer::VertexUse {
                                vertex: vertex.id,
                                role: UseRole::UnknownChild,
                            });
                    }
                }
            }
        }

        for (&merged, sources) in &ir.value_sources {
            let vert = value_producer_vertex.get(&merged).copied();
            for &src in sources {
                consumers
                    .entry(src)
                    .or_default()
                    .push(ValueConsumer::MergedInto {
                        target_value: merged,
                        vertex: vert,
                    });
            }
        }

        let iterator_steps = compute_iterator_steps(ir, &iterator_inits);

        Self {
            consumers,
            reads_of_binding,
            assigns_of_binding,
            iterator_steps,
            value_locations,
        }
    }
}

fn compute_iterator_steps(
    ir: &FunctionIr,
    iterator_inits: &[(VertexId, ValueId)],
) -> FxHashMap<VertexId, IteratorStepInfo> {
    let mut out = FxHashMap::default();
    for &(init_vid, iter_val) in iterator_inits {
        let mut queue = VecDeque::new();
        let mut seen = FxHashSet::default();
        let mut next_count = 0usize;
        let mut is_loop_iter = false;
        queue.push_back(init_vid);
        seen.insert(init_vid);

        while let Some(vid) = queue.pop_front() {
            let Some(v) = ir.vertices.get(vid.0 as usize) else {
                continue;
            };
            match &v.op {
                IrOp::IteratorNext { iterator, .. } if *iterator == iter_val => {
                    let has_loop_branch = v.explicit_edges.iter().any(|e| {
                        let Some(next_v) = ir.vertices.get(e.target.0 as usize) else {
                            return false;
                        };
                        next_v.explicit_edges.iter().any(|ne| {
                            matches!(ne.kind, ExplicitEdgeKind::Branch(bid)
                                if ir.branches.get(bid.0 as usize).is_some_and(|b| b.kind == IrBranchKind::Loop))
                        })
                    });
                    if has_loop_branch {
                        is_loop_iter = true;
                        out.insert(
                            vid,
                            IteratorStepInfo {
                                is_loop: true,
                                next_index: None,
                                rest_offset: 0,
                            },
                        );
                    } else {
                        out.insert(
                            vid,
                            IteratorStepInfo {
                                is_loop: false,
                                next_index: Some(next_count),
                                rest_offset: next_count,
                            },
                        );
                        next_count += 1;
                    }
                }
                IrOp::IteratorRest { iterator, .. } if *iterator == iter_val => {
                    out.insert(
                        vid,
                        IteratorStepInfo {
                            is_loop: is_loop_iter,
                            next_index: None,
                            rest_offset: next_count,
                        },
                    );
                }
                IrOp::IteratorClose { iterator, .. }
                    if *iterator == iter_val && vid != init_vid =>
                {
                    continue;
                }
                _ => {}
            }
            for edge in &v.explicit_edges {
                if matches!(edge.kind, ExplicitEdgeKind::LoopBack) {
                    continue;
                }
                if seen.insert(edge.target) {
                    queue.push_back(edge.target);
                }
            }
        }
    }
    out
}

/// Evaluates IR-backed tri-state value propagation across all `(from, to)` endpoint pairs.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Coordinates endpoints, member-access mode, lookup scope, boundary facades, and optional call graph."
)]
pub fn trace_value_propagation<S: BuildHasher>(
    from_endpoints: &[ValueEndpointSpec],
    to_endpoints: &[ValueEndpointSpec],
    include_member_access: bool,
    project_files: &[FileId],
    allowed_reverse_files: Option<&HashSet<FileId, S>>,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    summary_index: &BehaviorSummaryIndex,
    cached_call_graph: Option<&IrCallGraph>,
) -> (ValuePropagationOutcome, Option<IrCallGraph>) {
    let mut runner = PropagationEngine {
        include_member_access,
        project_files,
        allowed_reverse_files: allowed_reverse_files.map(|s| s.iter().copied().collect()),
        semantic,
        syntax,
        ir_provider,
        summary_index,
        cached_call_graph,
        built_call_graph: None,
        ir_indices: FxHashMap::default(),
        targets: resolve_target_endpoints(to_endpoints, semantic, syntax, ir_provider),
        all_gaps: Vec::new(),
        proved_paths: vec![Vec::new(); to_endpoints.len()],
        partial_paths: vec![Vec::new(); to_endpoints.len()],
        target_gaps: vec![BTreeSet::new(); to_endpoints.len()],
        target_evidence: vec![Vec::new(); to_endpoints.len()],
        conflict: None,
    };

    let mut results = Vec::with_capacity(from_endpoints.len() * to_endpoints.len());
    for &from_spec in from_endpoints {
        results.extend(runner.trace_from(from_spec));
        if runner.conflict.is_some() {
            break;
        }
    }

    let built_graph = if runner.allowed_reverse_files.is_none() {
        runner.built_call_graph
    } else {
        None
    };
    let outcome = ValuePropagationOutcome {
        results,
        conflict: runner.conflict,
    };
    (outcome, built_graph)
}

fn resolve_target_endpoints(
    to_endpoints: &[ValueEndpointSpec],
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
) -> Vec<ResolvedTargetEndpoint> {
    to_endpoints
        .iter()
        .map(|spec| {
            let enclosing_ir = ir_provider.enclosing_scope_ir(spec.syntax).ok();
            let scope_id = enclosing_ir.as_ref().map(|ir| ir.id);
            let expr_value = enclosing_ir
                .as_ref()
                .and_then(|ir| ir.expr_to_value.get(&spec.syntax.local_id()).copied());
            let decl_summary = semantic.resolve_declaration(spec.syntax);
            let binding_syntax = decl_summary
                .as_ref()
                .and_then(|d| d.primary_syntax)
                .or(Some(spec.syntax));
            let symbol = spec
                .symbol
                .or_else(|| semantic.symbol_at_syntax(spec.syntax))
                .or_else(|| decl_summary.as_ref().and_then(|d| d.symbol));
            let kind = syntax.syntax_kind(spec.syntax);
            let is_decl_target = expr_value.is_none()
                || matches!(
                    kind.as_deref(),
                    Some("VariableDeclaration" | "Parameter" | "BindingElement")
                );
            ResolvedTargetEndpoint {
                syntax: spec.syntax,
                location: syntax.syntax_location(spec.syntax),
                scope_id,
                expr_value,
                binding_syntax,
                symbol,
                is_decl_target,
            }
        })
        .collect()
}

struct PropagationEngine<'a> {
    include_member_access: bool,
    project_files: &'a [FileId],
    allowed_reverse_files: Option<FxHashSet<FileId>>,
    semantic: &'a dyn SemanticModel,
    syntax: &'a dyn SyntaxCatalog,
    ir_provider: &'a dyn IrProvider,
    summary_index: &'a BehaviorSummaryIndex,
    cached_call_graph: Option<&'a IrCallGraph>,
    built_call_graph: Option<IrCallGraph>,
    ir_indices: FxHashMap<FunctionIrId, Arc<IrValueIndex>>,
    targets: Vec<ResolvedTargetEndpoint>,
    all_gaps: Vec<GapItem>,
    proved_paths: Vec<Vec<ValuePropagationPathData>>,
    partial_paths: Vec<Vec<ValuePropagationPathData>>,
    target_gaps: Vec<BTreeSet<usize>>,
    target_evidence: Vec<Vec<EvidenceItem>>,
    conflict: Option<SummaryConflictError>,
}

type StepRecord = (&'static str, String, Option<String>, Option<Vec<String>>);

impl PropagationEngine<'_> {
    fn index_for(&mut self, ir: &FunctionIr) -> Arc<IrValueIndex> {
        if let Some(existing) = self.ir_indices.get(&ir.id) {
            return Arc::clone(existing);
        }
        let built = Arc::new(IrValueIndex::build(ir));
        self.ir_indices.insert(ir.id, Arc::clone(&built));
        built
    }

    fn call_graph(&mut self) -> &IrCallGraph {
        if let Some(g) = self.cached_call_graph {
            return g;
        }
        if self.built_call_graph.is_none() {
            let scoped_files: Vec<FileId>;
            let files: &[FileId] = if let Some(allowed) = &self.allowed_reverse_files {
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
            let g = build_ir_call_graph_with_summaries(
                files,
                self.semantic,
                self.syntax,
                self.ir_provider,
                self.summary_index,
            );
            if let Some(conflict) = &g.conflict {
                self.conflict = Some(conflict.clone());
            }
            self.built_call_graph = Some(g);
        }
        self.built_call_graph.as_ref().expect("just initialized")
    }

    fn record_gap(&mut self, gap: GapItem) -> usize {
        if let Some(pos) = self.all_gaps.iter().position(|g| {
            g.kind == gap.kind
                && g.message == gap.message
                && g.at.as_ref().map(|a| (&a.file, a.start, a.end))
                    == gap.at.as_ref().map(|a| (&a.file, a.start, a.end))
        }) {
            return pos;
        }
        let idx = self.all_gaps.len();
        self.all_gaps.push(gap);
        idx
    }

    fn trace_from(&mut self, from_spec: ValueEndpointSpec) -> Vec<ValuePropagationTargetOutcome> {
        self.proved_paths.iter_mut().for_each(Vec::clear);
        self.partial_paths.iter_mut().for_each(Vec::clear);
        self.target_gaps.iter_mut().for_each(BTreeSet::clear);
        self.target_evidence.iter_mut().for_each(Vec::clear);

        let from_loc = self.syntax.syntax_location(from_spec.syntax);
        let mut queue = DualQueue::default();
        let mut visited = FxHashSet::default();
        let mut untainted_visited = FxHashSet::default();

        self.seed_from_endpoint(from_spec, &mut queue, &mut visited, &mut untainted_visited);

        while let Some(item) = queue.pop_next() {
            if self.conflict.is_some() {
                break;
            }
            self.check_matches(&item);
            self.expand_item(&item, &mut queue, &mut visited, &mut untainted_visited);
        }

        let mut outcomes = Vec::with_capacity(self.targets.len());
        for idx in 0..self.targets.len() {
            let paths = std::mem::take(&mut self.proved_paths[idx]);
            let partial = std::mem::take(&mut self.partial_paths[idx]);
            let gap_set = std::mem::take(&mut self.target_gaps[idx]);
            let mut extra_ev = std::mem::take(&mut self.target_evidence[idx]);

            if !paths.is_empty() {
                let mut evidence = paths.first().map_or_else(Vec::new, |p| p.evidence.clone());
                for p in paths.iter().skip(1) {
                    for ev in &p.evidence {
                        if ev.kind == "summary" && !evidence.contains(ev) {
                            evidence.push(ev.clone());
                        }
                    }
                }
                outcomes.push(ValuePropagationTargetOutcome::Proved { paths, evidence });
            } else if !gap_set.is_empty() || queue.hit_state_budget.is_some() {
                let mut gaps: Vec<GapItem> = gap_set
                    .into_iter()
                    .filter_map(|gi| self.all_gaps.get(gi).cloned())
                    .collect();
                if let Some(budget_loc) = &queue.hit_state_budget {
                    let budget_gap = GapItem::from_reason(
                        UnknownReason::UnmodeledConstruct,
                        "Value propagation exceeded state exploration budget before exhausting all paths."
                            .to_string(),
                        Some(budget_loc.clone()),
                    );
                    if !gaps.contains(&budget_gap) {
                        gaps.push(budget_gap);
                    }
                }
                let mut evidence = Vec::new();
                if let Some(loc) = &from_loc {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        "Origin value starts here.".to_string(),
                        Some(loc.clone()),
                    ));
                }
                evidence.append(&mut extra_ev);
                for g in &gaps {
                    if let Some(at) = &g.at {
                        evidence.push(EvidenceItem::simple(
                            "note",
                            g.message.clone(),
                            Some(at.clone()),
                        ));
                    }
                }
                outcomes.push(ValuePropagationTargetOutcome::Inconclusive {
                    partial_paths: partial,
                    gaps,
                    evidence,
                });
            } else {
                let mut evidence = Vec::new();
                if let Some(loc) = &from_loc {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        "Origin value starts here.".to_string(),
                        Some(loc.clone()),
                    ));
                }
                if let Some(to_loc) = &self.targets[idx].location {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        "Target endpoint is unreachable from origin on modeled value-flow paths."
                            .to_string(),
                        Some(to_loc.clone()),
                    ));
                }
                outcomes.push(ValuePropagationTargetOutcome::Disproved { evidence });
            }
        }
        outcomes
    }

    fn enqueue(
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
        item: TraceItem,
    ) {
        let is_tainted = !item.gap_indices.is_empty();
        let stack_keys: Vec<CallFrameKey> = item.stack.iter().map(CallFrameData::key).collect();
        if is_tainted
            && untainted_visited.contains(&(
                item.ir.id,
                item.node,
                item.fields.clone(),
                stack_keys.clone(),
            ))
        {
            return;
        }
        let key = TraceKey {
            ir_id: item.ir.id,
            node: item.node,
            fields: item.fields.clone(),
            stack: stack_keys.clone(),
            is_gap_tainted: is_tainted,
        };
        if visited.contains(&key) {
            return;
        }
        if visited.len() >= MAX_PROP_STATES {
            queue.record_budget_hit(
                item.location
                    .clone()
                    .or_else(|| Some(item.ir.scope_location.clone())),
            );
            return;
        }
        visited.insert(key);
        if is_tainted {
            queue.tainted.push_back(item);
        } else {
            untainted_visited.insert((item.ir.id, item.node, item.fields.clone(), stack_keys));
            queue.untainted.push_back(item);
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Queues one propagation step with optional step record, summary evidence, and gap indices."
    )]
    fn enqueue_transition(
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
        base: &TraceItem,
        next_ir: Arc<FunctionIr>,
        next_node: TraceNode,
        next_loc: Option<DiagnosticLocation>,
        next_fields: ValueFieldState,
        next_stack: Vec<CallFrameData>,
        step_info: Option<StepRecord>,
        extra_ev: Option<EvidenceItem>,
        next_gaps: BTreeSet<usize>,
    ) {
        let resolved_next_loc = next_loc.or_else(|| base.location.clone());
        let mut steps = base.steps.clone();
        if let Some((kind, description, property, omitted_properties)) = step_info
            && let (Some(from_location), Some(to_location)) =
                (base.location.clone(), resolved_next_loc.clone())
        {
            steps.push(ValuePropagationStepData {
                kind,
                description,
                from_location,
                to_location,
                property,
                omitted_properties,
            });
        }
        let mut extra_evidence = base.extra_evidence.clone();
        if let Some(ev) = extra_ev
            && !extra_evidence.contains(&ev)
        {
            extra_evidence.push(ev);
        }
        Self::enqueue(
            queue,
            visited,
            untainted_visited,
            TraceItem {
                ir: next_ir,
                node: next_node,
                location: resolved_next_loc,
                fields: next_fields,
                stack: next_stack,
                steps,
                extra_evidence,
                gap_indices: next_gaps,
            },
        );
    }

    fn seed_from_endpoint(
        &mut self,
        from_spec: ValueEndpointSpec,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let Ok(from_ir) = self.ir_provider.enclosing_scope_ir(from_spec.syntax) else {
            if let Some(loc) = self.syntax.syntax_location(from_spec.syntax) {
                let _ = self.record_gap(GapItem::from_reason(
                    UnknownReason::NoEnclosingFunction,
                    "Origin syntax has no enclosing function or module scope.".to_string(),
                    Some(loc),
                ));
            }
            return;
        };

        let from_loc = self.syntax.syntax_location(from_spec.syntax);
        let from_kind = self.syntax.syntax_kind(from_spec.syntax);
        let decl_summary = self.semantic.resolve_declaration(from_spec.syntax);
        let primary_syn = decl_summary.as_ref().and_then(|d| d.primary_syntax);
        let is_decl_syntax = matches!(
            from_kind.as_deref(),
            Some(
                "VariableDeclaration"
                    | "Parameter"
                    | "BindingElement"
                    | "FunctionDeclaration"
                    | "ClassDeclaration"
            )
        ) || !from_ir
            .expr_to_value
            .contains_key(&from_spec.syntax.local_id());

        if is_decl_syntax
            && self.try_seed_decl_endpoint(
                from_spec,
                &from_ir,
                from_loc.clone(),
                primary_syn,
                queue,
                visited,
                untainted_visited,
            )
        {
            return;
        }

        if let Some(&val) = from_ir.expr_to_value.get(&from_spec.syntax.local_id()) {
            let loc = from_loc.or_else(|| {
                from_ir
                    .expr_to_vertex
                    .get(&from_spec.syntax.local_id())
                    .and_then(|vid| from_ir.vertices.get(vid.0 as usize))
                    .and_then(|v| v.location.clone())
            });
            Self::enqueue(
                queue,
                visited,
                untainted_visited,
                TraceItem {
                    ir: from_ir,
                    node: TraceNode::Value(val),
                    location: loc,
                    fields: ValueFieldState::default(),
                    stack: Vec::new(),
                    steps: Vec::new(),
                    extra_evidence: Vec::new(),
                    gap_indices: BTreeSet::new(),
                },
            );
            return;
        }

        if let Some(&vid) = from_ir.syntax_to_vertex.get(&from_spec.syntax)
            && let Some(vert) = from_ir.vertices.get(vid.0 as usize)
        {
            let loc = from_loc.or_else(|| vert.location.clone());
            let node = vert.value.map_or(TraceNode::Vertex(vid), TraceNode::Value);
            Self::enqueue(
                queue,
                visited,
                untainted_visited,
                TraceItem {
                    ir: from_ir,
                    node,
                    location: loc,
                    fields: ValueFieldState::default(),
                    stack: Vec::new(),
                    steps: Vec::new(),
                    extra_evidence: Vec::new(),
                    gap_indices: BTreeSet::new(),
                },
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Seeds declaration-based parameter or local variable origin endpoints into the propagation queue."
    )]
    fn try_seed_decl_endpoint(
        &mut self,
        from_spec: ValueEndpointSpec,
        from_ir: &Arc<FunctionIr>,
        from_loc: Option<DiagnosticLocation>,
        primary_syn: Option<SyntaxId>,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) -> bool {
        if let Some(param) = from_ir
            .parameters
            .iter()
            .find(|p| p.syntax == from_spec.syntax || primary_syn == Some(p.syntax))
        {
            if let Some(rv) = param.raw_value {
                Self::enqueue(
                    queue,
                    visited,
                    untainted_visited,
                    TraceItem {
                        ir: Arc::clone(from_ir),
                        node: TraceNode::Value(rv),
                        location: from_loc.clone(),
                        fields: ValueFieldState::default(),
                        stack: Vec::new(),
                        steps: Vec::new(),
                        extra_evidence: Vec::new(),
                        gap_indices: BTreeSet::new(),
                    },
                );
            }
            if let Some(b) = param.binding {
                Self::enqueue(
                    queue,
                    visited,
                    untainted_visited,
                    TraceItem {
                        ir: Arc::clone(from_ir),
                        node: TraceNode::Binding(b),
                        location: from_loc.clone(),
                        fields: ValueFieldState::default(),
                        stack: Vec::new(),
                        steps: Vec::new(),
                        extra_evidence: Vec::new(),
                        gap_indices: BTreeSet::new(),
                    },
                );
                self.seed_param_reads(from_ir, b, from_loc, queue, visited, untainted_visited);
            }
            return true;
        }

        let Some(binding_info) = from_ir
            .bindings
            .iter()
            .find(|b| b.syntax == from_spec.syntax || primary_syn == Some(b.syntax))
        else {
            return false;
        };
        let b_id = binding_info.id;
        let idx = self.index_for(from_ir);
        let mut seeded_any = false;
        if let Some(assigns) = idx.assigns_of_binding.get(&b_id) {
            for &avid in assigns {
                let vert = &from_ir.vertices[avid.0 as usize];
                if let IrOp::AssignLocal { is_init: true, .. } = vert.op {
                    let item_at_init = TraceItem {
                        ir: Arc::clone(from_ir),
                        node: TraceNode::Vertex(avid),
                        location: vert.location.clone().or_else(|| from_loc.clone()),
                        fields: ValueFieldState::default(),
                        stack: Vec::new(),
                        steps: Vec::new(),
                        extra_evidence: Vec::new(),
                        gap_indices: BTreeSet::new(),
                    };
                    Self::enqueue(queue, visited, untainted_visited, item_at_init.clone());
                    seeded_any = true;
                    self.propagate_local_write(
                        &item_at_init,
                        avid,
                        b_id,
                        &ValueFieldState::default(),
                        &BTreeSet::new(),
                        queue,
                        visited,
                        untainted_visited,
                    );
                }
            }
        }
        if !seeded_any {
            let decl_sym = from_spec
                .symbol
                .or_else(|| self.semantic.symbol_at_syntax(binding_info.syntax));
            self.propagate_decl_reads_in_nested_scopes(
                &TraceItem {
                    ir: Arc::clone(from_ir),
                    node: TraceNode::Binding(b_id),
                    location: from_loc,
                    fields: ValueFieldState::default(),
                    stack: Vec::new(),
                    steps: Vec::new(),
                    extra_evidence: Vec::new(),
                    gap_indices: BTreeSet::new(),
                },
                from_ir,
                binding_info.syntax,
                decl_sym,
                &binding_info.name,
                None,
                &ValueFieldState::default(),
                &BTreeSet::new(),
                queue,
                visited,
                untainted_visited,
            );
        }
        true
    }

    fn seed_param_reads(
        &mut self,
        ir: &Arc<FunctionIr>,
        binding: LocalBindingId,
        from_loc: Option<DiagnosticLocation>,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let base = TraceItem {
            ir: Arc::clone(ir),
            node: TraceNode::Binding(binding),
            location: from_loc,
            fields: ValueFieldState::default(),
            stack: Vec::new(),
            steps: Vec::new(),
            extra_evidence: Vec::new(),
            gap_indices: BTreeSet::new(),
        };
        let idx = self.index_for(ir);
        let reads = idx
            .reads_of_binding
            .get(&binding)
            .cloned()
            .unwrap_or_default();
        for read_vid in reads {
            let reaching = var_writes::resolve_reaching_writes_for_local(
                ir,
                read_vid,
                binding,
                self.semantic,
                self.syntax,
                self.ir_provider,
            );
            if reaching.entry_param_reaches
                && let Some(read_v) = ir.vertices.get(read_vid.0 as usize)
                && let Some(read_val) = read_v.value
            {
                let binding_name = ir
                    .bindings
                    .get(binding.0 as usize)
                    .map_or("param", |b| b.name.as_str());
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    &base,
                    Arc::clone(ir),
                    TraceNode::Value(read_val),
                    read_v.location.clone(),
                    ValueFieldState::default(),
                    Vec::new(),
                    Some((
                        "alias",
                        format!("Read parameter '{binding_name}'"),
                        None,
                        None,
                    )),
                    None,
                    BTreeSet::new(),
                );
            }
        }
        if let Some(b_info) = ir.bindings.get(binding.0 as usize) {
            let decl_sym = self.semantic.symbol_at_syntax(b_info.syntax);
            self.propagate_decl_reads_in_nested_scopes(
                &base,
                ir,
                b_info.syntax,
                decl_sym,
                &b_info.name,
                None,
                &ValueFieldState::default(),
                &BTreeSet::new(),
                queue,
                visited,
                untainted_visited,
            );
        }
    }

    fn check_matches(&mut self, item: &TraceItem) {
        if !item.fields.field_path.is_empty() {
            return;
        }
        if !self.include_member_access && !item.fields.source_path.is_empty() {
            return;
        }

        for (t_idx, target) in self.targets.iter().enumerate() {
            let matched = match item.node {
                TraceNode::Value(val) => {
                    target.scope_id == Some(item.ir.id) && target.expr_value == Some(val)
                }
                TraceNode::Vertex(vid) => {
                    let Some(v) = item.ir.vertices.get(vid.0 as usize) else {
                        continue;
                    };
                    if v.syntax == Some(target.syntax) {
                        true
                    } else if target.is_decl_target
                        && let IrOp::AssignLocal {
                            target: lb,
                            op: AssignOp::Assign,
                            ..
                        } = v.op
                        && let Some(b_info) = item.ir.bindings.get(lb.0 as usize)
                    {
                        b_info.syntax == target.syntax
                            || target.binding_syntax == Some(b_info.syntax)
                            || (target.symbol.is_some()
                                && self.semantic.symbol_at_syntax(b_info.syntax) == target.symbol)
                    } else if target.is_decl_target
                        && let IrOp::AssignSymbol {
                            target_syntax,
                            op: AssignOp::Assign,
                            ..
                        } = v.op
                    {
                        let decl = self.semantic.resolve_declaration(target_syntax);
                        let decl_syn = decl.as_ref().and_then(|d| d.primary_syntax);
                        let sym = self
                            .semantic
                            .symbol_at_syntax(target_syntax)
                            .or_else(|| decl.as_ref().and_then(|d| d.symbol));
                        target_syntax == target.syntax
                            || target.binding_syntax == Some(target_syntax)
                            || decl_syn.is_some_and(|ds| {
                                ds == target.syntax || target.binding_syntax == Some(ds)
                            })
                            || (target.symbol.is_some() && sym == target.symbol)
                    } else {
                        false
                    }
                }
                TraceNode::Binding(lb) => {
                    if let Some(b_info) = item.ir.bindings.get(lb.0 as usize) {
                        b_info.syntax == target.syntax
                            || target.binding_syntax == Some(b_info.syntax)
                            || (target.is_decl_target
                                && target.symbol.is_some()
                                && self.semantic.symbol_at_syntax(b_info.syntax) == target.symbol)
                    } else {
                        false
                    }
                }
            };

            if !matched {
                continue;
            }

            if item.gap_indices.is_empty() {
                if self.proved_paths[t_idx].len() < MAX_PATHS_PER_PAIR {
                    self.proved_paths[t_idx].push(build_path_data(item));
                }
            } else {
                for &gi in &item.gap_indices {
                    self.target_gaps[t_idx].insert(gi);
                }
                for ev in &item.extra_evidence {
                    if !self.target_evidence[t_idx].contains(ev) {
                        self.target_evidence[t_idx].push(ev.clone());
                    }
                }
                if self.partial_paths[t_idx].len() < MAX_PATHS_PER_PAIR {
                    self.partial_paths[t_idx].push(build_path_data(item));
                }
            }
        }
    }

    fn expand_item(
        &mut self,
        item: &TraceItem,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let val = match item.node {
            TraceNode::Value(v) => v,
            TraceNode::Vertex(_) | TraceNode::Binding(_) => return,
        };

        let idx = self.index_for(&item.ir);
        let Some(consumers) = idx.consumers.get(&val).cloned() else {
            return;
        };

        for consumer in consumers {
            if self.conflict.is_some() {
                return;
            }
            match consumer {
                ValueConsumer::MergedInto {
                    target_value,
                    vertex,
                } => {
                    let loc = vertex
                        .and_then(|vid| item.ir.vertices.get(vid.0 as usize))
                        .and_then(|v| v.location.clone())
                        .or_else(|| idx.value_locations.get(&target_value).cloned())
                        .or_else(|| item.location.clone());
                    let step_kind = vertex
                        .and_then(|vid| item.ir.vertices.get(vid.0 as usize))
                        .and_then(|v| v.syntax)
                        .and_then(|s| self.syntax.syntax_kind(s))
                        .map_or("conditionalBranch", |k| {
                            if k == "ConditionalExpression" || k == "BinaryExpression" {
                                "conditionalBranch"
                            } else {
                                "alias"
                            }
                        });
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        item,
                        Arc::clone(&item.ir),
                        TraceNode::Value(target_value),
                        loc,
                        item.fields.clone(),
                        item.stack.clone(),
                        Some((
                            step_kind,
                            "Value flows through branch merge".to_string(),
                            None,
                            None,
                        )),
                        None,
                        item.gap_indices.clone(),
                    );
                }
                ValueConsumer::VertexUse { vertex, role } => {
                    self.expand_vertex_use(
                        item,
                        &idx,
                        vertex,
                        role,
                        queue,
                        visited,
                        untainted_visited,
                    );
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Dispatches value transitions across all IR consumer roles uniformly."
    )]
    fn expand_vertex_use(
        &mut self,
        item: &TraceItem,
        idx: &IrValueIndex,
        vid: VertexId,
        role: UseRole,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let ir = &item.ir;
        let Some(v) = ir.vertices.get(vid.0 as usize) else {
            return;
        };
        let v_loc = v.location.clone();

        match role {
            UseRole::ValueForward { non_null_assert } => {
                let Some(out_val) = v.value else {
                    return;
                };
                let kind_str = v
                    .syntax
                    .and_then(|s| self.syntax.syntax_kind(s))
                    .unwrap_or_default();
                let (step_kind, desc) = if kind_str == "ParenthesizedExpression" {
                    (
                        "parenthesized",
                        "Value passes through parenthesized expression",
                    )
                } else if non_null_assert
                    || matches!(
                        kind_str.as_str(),
                        "AsExpression"
                            | "TypeAssertionExpression"
                            | "SatisfiesExpression"
                            | "NonNullExpression"
                    )
                {
                    ("typeAssertion", "Value passes through type assertion")
                } else {
                    ("alias", "Value forwarded")
                };
                let step_info = v_loc
                    .is_some()
                    .then(|| (step_kind, desc.to_string(), None, None));
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(out_val),
                    v_loc,
                    item.fields.clone(),
                    item.stack.clone(),
                    step_info,
                    None,
                    item.gap_indices.clone(),
                );
            }
            UseRole::Await => {
                let Some(out_val) = v.value else {
                    return;
                };
                let mut next_fields = item.fields.clone();
                if matches!(
                    next_fields.field_path.last(),
                    Some(FieldSegment::Element(_))
                ) {
                    next_fields.field_path.pop();
                } else if !next_fields.field_path.is_empty() {
                    return;
                }
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(out_val),
                    v_loc,
                    next_fields,
                    item.stack.clone(),
                    Some((
                        "awaitUnwrap",
                        "Value unwrapped via await".to_string(),
                        None,
                        None,
                    )),
                    None,
                    item.gap_indices.clone(),
                );
            }
            UseRole::AssignLocal { target, op } => {
                if op != AssignOp::Assign {
                    return;
                }
                let b_name = ir
                    .bindings
                    .get(target.0 as usize)
                    .map_or("variable", |b| b.name.as_str());
                let is_destructure_assign = matches!(
                    v.syntax.and_then(|s| self.syntax.syntax_kind(s)).as_deref(),
                    Some("Identifier")
                ) && matches!(
                    item.steps.last().map(|s| s.kind),
                    Some("destructure" | "restDestructure")
                );
                let step_kind = if is_destructure_assign {
                    "destructure"
                } else if matches!(v.op, IrOp::AssignLocal { is_init: true, .. }) {
                    "variableInit"
                } else {
                    "assignment"
                };
                let desc = format!("Assigned value to '{b_name}'");
                let step_meta = (step_kind, desc, None, None);

                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Vertex(vid),
                    v_loc.clone(),
                    item.fields.clone(),
                    item.stack.clone(),
                    Some(step_meta.clone()),
                    None,
                    item.gap_indices.clone(),
                );
                if let Some(out_val) = v.value
                    && !matches!(v.op, IrOp::AssignLocal { is_init: true, .. })
                {
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        item,
                        Arc::clone(ir),
                        TraceNode::Value(out_val),
                        v_loc.clone(),
                        item.fields.clone(),
                        item.stack.clone(),
                        Some(step_meta),
                        None,
                        item.gap_indices.clone(),
                    );
                }

                let item_at_write = TraceItem {
                    ir: Arc::clone(ir),
                    node: TraceNode::Vertex(vid),
                    location: v_loc,
                    fields: item.fields.clone(),
                    stack: item.stack.clone(),
                    steps: {
                        let mut s = item.steps.clone();
                        if let (Some(from_loc), Some(to_loc)) =
                            (item.location.clone(), v.location.clone())
                        {
                            s.push(ValuePropagationStepData {
                                kind: step_kind,
                                description: format!("Assigned value to '{b_name}'"),
                                from_location: from_loc,
                                to_location: to_loc,
                                property: None,
                                omitted_properties: None,
                            });
                        }
                        s
                    },
                    extra_evidence: item.extra_evidence.clone(),
                    gap_indices: item.gap_indices.clone(),
                };
                self.propagate_local_write(
                    &item_at_write,
                    vid,
                    target,
                    &item.fields,
                    &item.gap_indices,
                    queue,
                    visited,
                    untainted_visited,
                );
            }
            UseRole::AssignSymbol { target_syntax, op } => {
                if op != AssignOp::Assign {
                    return;
                }
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Vertex(vid),
                    v_loc.clone(),
                    item.fields.clone(),
                    item.stack.clone(),
                    Some((
                        "assignment",
                        "Assigned value to captured or module variable".to_string(),
                        None,
                        None,
                    )),
                    None,
                    item.gap_indices.clone(),
                );
                if let Some(out_val) = v.value
                    && !matches!(v.op, IrOp::AssignSymbol { is_init: true, .. })
                {
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        item,
                        Arc::clone(ir),
                        TraceNode::Value(out_val),
                        v_loc,
                        item.fields.clone(),
                        item.stack.clone(),
                        None,
                        None,
                        item.gap_indices.clone(),
                    );
                }
                self.propagate_symbol_write(
                    item,
                    vid,
                    target_syntax,
                    queue,
                    visited,
                    untainted_visited,
                );
            }
            UseRole::PropWriteValue {
                object,
                op,
                implicit_call_kind,
            } => {
                if op != AssignOp::Assign
                    || implicit_call_kind == Some(ImplicitAccessorKind::Setter)
                {
                    return;
                }
                let IrOp::PropWrite { key, .. } = &v.op else {
                    return;
                };
                let Some(prop_name) = prop_key_static_name(key).map(str::to_string) else {
                    let gap_idx = self.record_gap(GapItem::new(
                        "dynamic-property-access",
                        "Computed property write prevents static property tracking.".to_string(),
                        v_loc,
                    ));
                    let mut next_gaps = item.gap_indices.clone();
                    next_gaps.insert(gap_idx);
                    self.propagate_prop_write_mutation(
                        item,
                        vid,
                        object,
                        None,
                        &next_gaps,
                        queue,
                        visited,
                        untainted_visited,
                    );
                    return;
                };
                self.propagate_prop_write_mutation(
                    item,
                    vid,
                    object,
                    Some(&prop_name),
                    &item.gap_indices,
                    queue,
                    visited,
                    untainted_visited,
                );
            }
            UseRole::PropReadObject { implicit_call_kind } => {
                if implicit_call_kind == Some(ImplicitAccessorKind::Getter) {
                    return;
                }
                let Some(out_val) = v.value else {
                    return;
                };
                let IrOp::PropRead { key, .. } = &v.op else {
                    return;
                };
                let Some(prop_name) = prop_key_static_name(key).map(str::to_string) else {
                    if !item.fields.field_path.is_empty() || self.include_member_access {
                        let mut next_fields = item.fields.clone();
                        if !next_fields.field_path.is_empty() {
                            next_fields.field_path.pop();
                        }
                        let gap_idx = self.record_gap(GapItem::new(
                            "dynamic-property-access",
                            "Computed element access prevents static property tracking."
                                .to_string(),
                            v_loc.clone(),
                        ));
                        let mut next_gaps = item.gap_indices.clone();
                        next_gaps.insert(gap_idx);
                        Self::enqueue_transition(
                            queue,
                            visited,
                            untainted_visited,
                            item,
                            Arc::clone(ir),
                            TraceNode::Value(out_val),
                            v_loc,
                            next_fields,
                            item.stack.clone(),
                            Some((
                                "propertyRead",
                                "Dynamic property access".to_string(),
                                None,
                                None,
                            )),
                            None,
                            next_gaps,
                        );
                    }
                    return;
                };

                if !item.fields.field_path.is_empty() {
                    let matched = match item.fields.field_path.last() {
                        Some(FieldSegment::Prop(k)) => k == &prop_name,
                        Some(FieldSegment::Element(idx_opt)) => prop_name
                            .parse::<usize>()
                            .is_ok_and(|ri| idx_opt.is_none_or(|i| i == ri)),
                        None => false,
                    };
                    if matched {
                        let mut next_fields = item.fields.clone();
                        next_fields.field_path.pop();
                        let step_kind =
                            if v.syntax.and_then(|s| self.syntax.syntax_kind(s)).as_deref()
                                == Some("BindingElement")
                            {
                                "destructure"
                            } else {
                                "propertyRead"
                            };
                        let desc = if step_kind == "destructure" {
                            format!("Destructured property '{prop_name}'")
                        } else {
                            format!("Read property '.{prop_name}'")
                        };
                        Self::enqueue_transition(
                            queue,
                            visited,
                            untainted_visited,
                            item,
                            Arc::clone(ir),
                            TraceNode::Value(out_val),
                            v_loc,
                            next_fields,
                            item.stack.clone(),
                            Some((step_kind, desc, Some(prop_name), None)),
                            None,
                            item.gap_indices.clone(),
                        );
                    }
                } else if self.include_member_access
                    && !item.fields.omitted_properties.contains(&prop_name)
                {
                    let mut next_fields = item.fields.clone();
                    let mut next_gaps = item.gap_indices.clone();
                    let sp_len = next_fields.source_path.len();
                    let repeats_tail = sp_len >= 2
                        && next_fields.source_path[sp_len - 1] == prop_name
                        && next_fields.source_path[sp_len - 2] == prop_name;
                    if repeats_tail {
                        // Collapse consecutive self-recursive property cycles (`w = w.parent`).
                    } else if sp_len >= MAX_FIELD_PATH_DEPTH {
                        let gap_idx = self.record_gap(GapItem::from_reason(
                            UnknownReason::UnmodeledConstruct,
                            format!(
                                "Member-access path depth exceeded limit ({MAX_FIELD_PATH_DEPTH}) at '.{prop_name}'."
                            ),
                            v_loc.clone(),
                        ));
                        next_gaps.insert(gap_idx);
                    } else {
                        next_fields.source_path.push(prop_name.clone());
                    }
                    next_fields.omitted_properties.clear();
                    let step_kind = if v.syntax.and_then(|s| self.syntax.syntax_kind(s)).as_deref()
                        == Some("BindingElement")
                    {
                        "destructure"
                    } else {
                        "propertyRead"
                    };
                    let desc = if step_kind == "destructure" {
                        format!("Destructured property '{prop_name}'")
                    } else {
                        format!("Read property '.{prop_name}'")
                    };
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        item,
                        Arc::clone(ir),
                        TraceNode::Value(out_val),
                        v_loc,
                        next_fields,
                        item.stack.clone(),
                        Some((step_kind, desc, Some(prop_name), None)),
                        None,
                        next_gaps,
                    );
                }
            }
            UseRole::ObjectRestSource => {
                let Some(out_val) = v.value else {
                    return;
                };
                let IrOp::ObjectRest {
                    omitted_static_keys,
                    has_dynamic_omitted,
                    ..
                } = &v.op
                else {
                    return;
                };
                if let Some(FieldSegment::Prop(k)) = item.fields.field_path.last()
                    && omitted_static_keys.iter().any(|om| om == k)
                {
                    return;
                }
                let mut next_fields = item.fields.clone();
                if next_fields.field_path.is_empty() {
                    next_fields
                        .omitted_properties
                        .extend(omitted_static_keys.iter().cloned());
                }
                let mut sorted_omitted = omitted_static_keys.clone();
                sorted_omitted.sort();
                let mut next_gaps = item.gap_indices.clone();
                if *has_dynamic_omitted {
                    let gap_idx = self.record_gap(GapItem::new(
                        "dynamic-property-access",
                        "Dynamic omitted key in object rest destructuring prevents exact exclusion."
                            .to_string(),
                        v_loc.clone(),
                    ));
                    next_gaps.insert(gap_idx);
                }
                let desc = if sorted_omitted.is_empty() {
                    "Rest destructuring preserves remaining object properties".to_string()
                } else {
                    format!(
                        "Rest destructuring preserves remaining properties (omitting {})",
                        sorted_omitted.join(", ")
                    )
                };
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(out_val),
                    v_loc,
                    next_fields,
                    item.stack.clone(),
                    Some(("restDestructure", desc, None, Some(sorted_omitted))),
                    None,
                    next_gaps,
                );
            }
            UseRole::ObjectAllocProp { prop_index } => {
                let Some(out_val) = v.value else {
                    return;
                };
                let IrOp::ObjectAlloc { props, .. } = &v.op else {
                    return;
                };
                let Some(IrObjectProp::KeyValue { key, .. }) = props.get(prop_index) else {
                    return;
                };
                let Some(prop_name) = prop_key_static_name(key).map(str::to_string) else {
                    let gap_idx = self.record_gap(GapItem::new(
                        "dynamic-property-access",
                        "Computed property key in object literal prevents static field tracking."
                            .to_string(),
                        v_loc.clone(),
                    ));
                    let mut next_gaps = item.gap_indices.clone();
                    next_gaps.insert(gap_idx);
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        item,
                        Arc::clone(ir),
                        TraceNode::Value(out_val),
                        v_loc,
                        item.fields.clone(),
                        item.stack.clone(),
                        Some((
                            "propertyWrite",
                            "Stored value in dynamic object property".to_string(),
                            None,
                            None,
                        )),
                        None,
                        next_gaps,
                    );
                    return;
                };
                let overwritten = props[prop_index + 1..].iter().any(|later| match later {
                    IrObjectProp::KeyValue { key: lk, .. } => {
                        prop_key_static_name(lk) == Some(prop_name.as_str())
                    }
                    IrObjectProp::Spread(_) => false,
                });
                if overwritten {
                    return;
                }
                let mut next_fields = item.fields.clone();
                if !push_field_segment(
                    queue,
                    &mut next_fields,
                    FieldSegment::Prop(prop_name.clone()),
                    v_loc.clone().or_else(|| Some(ir.scope_location.clone())),
                ) {
                    return;
                }
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(out_val),
                    v_loc,
                    next_fields,
                    item.stack.clone(),
                    Some((
                        "propertyWrite",
                        format!("Stored value in object property '{prop_name}'"),
                        Some(prop_name),
                        None,
                    )),
                    None,
                    item.gap_indices.clone(),
                );
            }
            UseRole::ObjectAllocSpread { prop_index } => {
                let Some(out_val) = v.value else {
                    return;
                };
                let IrOp::ObjectAlloc { props, .. } = &v.op else {
                    return;
                };
                let mut overwritten_keys = BTreeSet::new();
                for later in &props[prop_index + 1..] {
                    if let IrObjectProp::KeyValue { key: lk, .. } = later
                        && let Some(name) = prop_key_static_name(lk)
                    {
                        overwritten_keys.insert(name.to_string());
                    }
                }
                if let Some(FieldSegment::Prop(k)) = item.fields.field_path.last()
                    && overwritten_keys.contains(k)
                {
                    return;
                }
                let mut next_fields = item.fields.clone();
                if next_fields.field_path.is_empty() {
                    next_fields.omitted_properties.extend(overwritten_keys);
                }
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(out_val),
                    v_loc,
                    next_fields,
                    item.stack.clone(),
                    Some((
                        "spread",
                        "Spread object properties into object literal".to_string(),
                        None,
                        None,
                    )),
                    None,
                    item.gap_indices.clone(),
                );
            }
            UseRole::ArrayAllocElem { elem_index } => {
                let Some(out_val) = v.value else {
                    return;
                };
                let IrOp::ArrayAlloc { elems, .. } = &v.op else {
                    return;
                };
                let seen_spread_before = elems[..elem_index]
                    .iter()
                    .any(|e| matches!(e, IrArrayElem::Spread(_)));
                let slot_idx = (!seen_spread_before).then_some(elem_index);
                let mut next_fields = item.fields.clone();
                if !push_field_segment(
                    queue,
                    &mut next_fields,
                    FieldSegment::Element(slot_idx),
                    v_loc.clone().or_else(|| Some(ir.scope_location.clone())),
                ) {
                    return;
                }
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(out_val),
                    v_loc,
                    next_fields,
                    item.stack.clone(),
                    Some((
                        "arrayElement",
                        "Stored value in array literal".to_string(),
                        None,
                        None,
                    )),
                    None,
                    item.gap_indices.clone(),
                );
            }
            UseRole::ArrayAllocSpread { elem_index } => {
                let Some(out_val) = v.value else {
                    return;
                };
                let IrOp::ArrayAlloc { elems, .. } = &v.op else {
                    return;
                };
                let seen_spread_before = elems[..elem_index]
                    .iter()
                    .any(|e| matches!(e, IrArrayElem::Spread(_)));
                let mut next_fields = item.fields.clone();
                if let Some(FieldSegment::Element(inner_idx)) = next_fields.field_path.last_mut() {
                    *inner_idx = if seen_spread_before {
                        None
                    } else {
                        inner_idx.map(|i| elem_index + i)
                    };
                }
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(out_val),
                    v_loc,
                    next_fields,
                    item.stack.clone(),
                    Some((
                        "spread",
                        "Spread elements into array literal".to_string(),
                        None,
                        None,
                    )),
                    None,
                    item.gap_indices.clone(),
                );
            }
            UseRole::IteratorInitIterable => {
                let Some(iter_val) = v.value else {
                    return;
                };
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(iter_val),
                    v_loc,
                    item.fields.clone(),
                    item.stack.clone(),
                    None,
                    None,
                    item.gap_indices.clone(),
                );
            }
            UseRole::IteratorNextIter => {
                let Some(out_val) = v.value else {
                    return;
                };
                let step_info = idx
                    .iterator_steps
                    .get(&vid)
                    .copied()
                    .unwrap_or(IteratorStepInfo {
                        is_loop: false,
                        next_index: None,
                        rest_offset: 0,
                    });
                let is_async_iter = matches!(v.op, IrOp::IteratorNext { is_async: true, .. });
                let mut next_fields = item.fields.clone();
                match next_fields.field_path.last() {
                    Some(FieldSegment::Element(elem_idx_opt)) => {
                        let matches_slot = step_info.is_loop
                            || elem_idx_opt.is_none()
                            || step_info.next_index.is_none()
                            || *elem_idx_opt == step_info.next_index;
                        if !matches_slot {
                            return;
                        }
                        next_fields.field_path.pop();
                    }
                    Some(FieldSegment::Prop(_)) => return,
                    None => {}
                }
                if is_async_iter
                    && matches!(
                        next_fields.field_path.last(),
                        Some(FieldSegment::Element(None))
                    )
                {
                    next_fields.field_path.pop();
                }
                let (step_kind, desc) = if step_info.is_loop {
                    ("forOfElement", "Iterated element in for-of loop")
                } else {
                    ("destructure", "Destructured array element")
                };
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(out_val),
                    v_loc,
                    next_fields,
                    item.stack.clone(),
                    Some((step_kind, desc.to_string(), None, None)),
                    None,
                    item.gap_indices.clone(),
                );
            }
            UseRole::IteratorRestIter => {
                let Some(out_val) = v.value else {
                    return;
                };
                let step_info = idx
                    .iterator_steps
                    .get(&vid)
                    .copied()
                    .unwrap_or(IteratorStepInfo {
                        is_loop: false,
                        next_index: None,
                        rest_offset: 0,
                    });
                let mut next_fields = item.fields.clone();
                match next_fields.field_path.last_mut() {
                    Some(FieldSegment::Element(elem_idx_opt)) => {
                        if let Some(i) = *elem_idx_opt {
                            if i < step_info.rest_offset {
                                return;
                            }
                            *elem_idx_opt = Some(i - step_info.rest_offset);
                        }
                    }
                    Some(FieldSegment::Prop(_)) => return,
                    None => {}
                }
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(ir),
                    TraceNode::Value(out_val),
                    v_loc,
                    next_fields,
                    item.stack.clone(),
                    Some((
                        "restDestructure",
                        "Rest iteration preserves remaining elements".to_string(),
                        None,
                        None,
                    )),
                    None,
                    item.gap_indices.clone(),
                );
            }
            UseRole::CallCallee | UseRole::ConstructCallee => {}
            UseRole::CallReceiver => {
                self.handle_call_arrival(
                    item,
                    v,
                    CallArrivalRole::Receiver,
                    queue,
                    visited,
                    untainted_visited,
                );
            }
            UseRole::CallArg {
                arg_index,
                is_spread,
            }
            | UseRole::ConstructArg {
                arg_index,
                is_spread,
            } => {
                self.handle_call_arrival(
                    item,
                    v,
                    CallArrivalRole::Arg {
                        arg_index,
                        is_spread,
                    },
                    queue,
                    visited,
                    untainted_visited,
                );
            }
            UseRole::ReturnValue => {
                self.handle_return(item, v, queue, visited, untainted_visited);
            }
            UseRole::YieldValue => {
                let mut next_fields = item.fields.clone();
                if !push_field_segment(
                    queue,
                    &mut next_fields,
                    FieldSegment::Element(None),
                    v_loc.or_else(|| Some(ir.scope_location.clone())),
                ) {
                    return;
                }
                let mut yielded_item = item.clone();
                yielded_item.fields = next_fields;
                self.handle_return(&yielded_item, v, queue, visited, untainted_visited);
            }
            UseRole::UnknownChild => {
                let Some(out_val) = v.value else {
                    return;
                };
                if let IrOp::Unknown {
                    reason,
                    message,
                    location,
                    ..
                } = &v.op
                {
                    let gap_idx = self.record_gap(GapItem::from_reason(
                        *reason,
                        message.clone(),
                        Some(location.clone()),
                    ));
                    let mut next_gaps = item.gap_indices.clone();
                    next_gaps.insert(gap_idx);
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        item,
                        Arc::clone(ir),
                        TraceNode::Value(out_val),
                        v_loc,
                        item.fields.clone(),
                        item.stack.clone(),
                        Some((
                            "alias",
                            "Value flows through unmodeled construct".to_string(),
                            None,
                            None,
                        )),
                        None,
                        next_gaps,
                    );
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Coordinates reaching-definition checks and nested closure reads for local variable writes."
    )]
    fn propagate_local_write(
        &mut self,
        base: &TraceItem,
        write_vid: VertexId,
        binding: LocalBindingId,
        fields: &ValueFieldState,
        gaps: &BTreeSet<usize>,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let ir = &base.ir;
        let idx = self.index_for(ir);
        let b_info = ir.bindings.get(binding.0 as usize).cloned();
        let b_name = b_info.as_ref().map_or("var", |b| b.name.as_str());

        let reads = idx
            .reads_of_binding
            .get(&binding)
            .cloned()
            .unwrap_or_default();
        for read_vid in reads {
            let reaches = if b_info.as_ref().is_some_and(|b| !is_mutable(b.mutability)) {
                true
            } else {
                let rw = var_writes::resolve_reaching_writes_for_local(
                    ir,
                    read_vid,
                    binding,
                    self.semantic,
                    self.syntax,
                    self.ir_provider,
                );
                rw.writes
                    .iter()
                    .any(|w| w.scope_ir.id == ir.id && w.vertex == write_vid)
            };
            if reaches
                && let Some(read_v) = ir.vertices.get(read_vid.0 as usize)
                && let Some(read_val) = read_v.value
            {
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    base,
                    Arc::clone(ir),
                    TraceNode::Value(read_val),
                    read_v.location.clone(),
                    fields.clone(),
                    base.stack.clone(),
                    Some(("alias", format!("Read value from '{b_name}'"), None, None)),
                    None,
                    gaps.clone(),
                );
            }
        }

        if let Some(info) = b_info {
            let decl_sym = self.semantic.symbol_at_syntax(info.syntax);
            self.propagate_decl_reads_in_nested_scopes(
                base,
                ir,
                info.syntax,
                decl_sym,
                &info.name,
                Some((binding, write_vid)),
                fields,
                gaps,
                queue,
                visited,
                untainted_visited,
            );
        }
    }

    fn propagate_symbol_write(
        &mut self,
        base: &TraceItem,
        write_vid: VertexId,
        target_syntax: SyntaxId,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let Some(decl) = self.semantic.resolve_declaration(target_syntax) else {
            return;
        };
        let Some(decl_syn) = decl.primary_syntax else {
            return;
        };
        let Ok(decl_ir) = self.ir_provider.enclosing_scope_ir(decl_syn) else {
            return;
        };
        let resolved_sym = decl
            .symbol
            .or_else(|| self.semantic.symbol_at_syntax(decl_syn));
        let decl_name = decl.name.unwrap_or_default();

        if let Some(binding_info) = decl_ir.bindings.iter().find(|b| {
            b.syntax == decl_syn
                || (resolved_sym.is_some()
                    && self.semantic.symbol_at_syntax(b.syntax) == resolved_sym)
        }) {
            let b_id = binding_info.id;
            let decl_idx = self.index_for(&decl_ir);
            let reads = decl_idx
                .reads_of_binding
                .get(&b_id)
                .cloned()
                .unwrap_or_default();
            for read_vid in reads {
                let rw = var_writes::resolve_reaching_writes_for_local(
                    &decl_ir,
                    read_vid,
                    b_id,
                    self.semantic,
                    self.syntax,
                    self.ir_provider,
                );
                let reaches = rw
                    .writes
                    .iter()
                    .any(|w| w.scope_ir.id == base.ir.id && w.vertex == write_vid);
                if reaches
                    && let Some(read_v) = decl_ir.vertices.get(read_vid.0 as usize)
                    && let Some(read_val) = read_v.value
                {
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        base,
                        Arc::clone(&decl_ir),
                        TraceNode::Value(read_val),
                        read_v.location.clone(),
                        base.fields.clone(),
                        Vec::new(),
                        Some((
                            "closureCapture",
                            format!("Read closure-mutated variable '{}'", binding_info.name),
                            None,
                            None,
                        )),
                        None,
                        base.gap_indices.clone(),
                    );
                }
            }
        }

        self.propagate_decl_reads_in_nested_scopes(
            base,
            &decl_ir,
            decl_syn,
            resolved_sym,
            &decl_name,
            None,
            &base.fields,
            &base.gap_indices,
            queue,
            visited,
            untainted_visited,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Finds closure and module-level ReadSymbol reads of an outer declaration while respecting kill writes."
    )]
    fn propagate_decl_reads_in_nested_scopes(
        &self,
        base: &TraceItem,
        decl_ir: &Arc<FunctionIr>,
        decl_syn: SyntaxId,
        target_sym: Option<SymbolId>,
        decl_name: &str,
        local_write_check: Option<(LocalBindingId, VertexId)>,
        fields: &ValueFieldState,
        gaps: &BTreeSet<usize>,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let is_module = decl_ir.scope_kind == IrScopeKind::ModuleTopLevel;
        let files_to_scan: Vec<FileId> = if is_module {
            self.allowed_reverse_files.as_ref().map_or_else(
                || self.project_files.to_vec(),
                |allowed| {
                    self.project_files
                        .iter()
                        .copied()
                        .filter(|f| {
                            allowed.contains(f)
                                || *f == decl_ir.id.file
                                || self.targets.iter().any(|t| {
                                    t.syntax.file_id() == *f
                                        || t.scope_id.is_some_and(|sid| sid.file == *f)
                                })
                        })
                        .collect()
                },
            )
        } else {
            vec![decl_ir.id.file]
        };

        let outer_start = decl_ir.scope_location.start;
        let outer_end = decl_ir.scope_location.end;

        for file_id in files_to_scan {
            let scopes = self.ir_provider.scopes_in_file(file_id);
            for entry in scopes {
                if entry.root_syntax == decl_ir.root_syntax {
                    continue;
                }
                if !is_module
                    && file_id == decl_ir.id.file
                    && (entry.span_start < outer_start || entry.span_end > outer_end)
                {
                    continue;
                }
                if !decl_name.is_empty()
                    && let Some(scope_text) = self.syntax.syntax_text(entry.root_syntax)
                    && !scope_text.contains(decl_name)
                {
                    continue;
                }

                if let Some((binding, write_vid)) = local_write_check
                    && !is_module
                    && !self.write_can_reach_nested_scope(
                        decl_ir,
                        binding,
                        write_vid,
                        entry.span_start,
                        entry.span_end,
                    )
                {
                    continue;
                }

                let Ok(nested_ir) = self.ir_provider.function_ir(entry.root_syntax) else {
                    continue;
                };
                for rv in &nested_ir.vertices {
                    if let IrOp::ReadSymbol { ident_syntax } = rv.op
                        && let Some(read_val) = rv.value
                        && self.ident_matches_decl(ident_syntax, decl_syn, target_sym)
                    {
                        Self::enqueue_transition(
                            queue,
                            visited,
                            untainted_visited,
                            base,
                            Arc::clone(&nested_ir),
                            TraceNode::Value(read_val),
                            rv.location.clone(),
                            fields.clone(),
                            Vec::new(),
                            Some((
                                "closureCapture",
                                format!("Captured variable '{decl_name}'"),
                                None,
                                None,
                            )),
                            None,
                            gaps.clone(),
                        );
                    }
                }
            }
        }
    }

    fn write_can_reach_nested_scope(
        &self,
        decl_ir: &Arc<FunctionIr>,
        binding: LocalBindingId,
        write_vid: VertexId,
        nested_start: u32,
        nested_end: u32,
    ) -> bool {
        let mut anchor_vid: Option<VertexId> = None;
        for v in &decl_ir.vertices {
            if let IrOp::FunctionLiteral { syntax, .. } = v.op
                && let Some(loc) = self.syntax.syntax_location(syntax)
                && loc.start <= nested_start
                && loc.end >= nested_end
            {
                anchor_vid = Some(v.id);
                break;
            }
        }
        let Some(anchor) = anchor_vid else {
            return true;
        };
        let rw = var_writes::resolve_reaching_writes_for_local(
            decl_ir,
            anchor,
            binding,
            self.semantic,
            self.syntax,
            self.ir_provider,
        );
        if rw
            .writes
            .iter()
            .any(|w| w.scope_ir.id == decl_ir.id && w.vertex == write_vid)
        {
            return true;
        }
        // If the write happens after the closure literal is created (e.g. `const fn = () => x; x = raw; fn();`),
        // `anchor` can reach `write_vid` in CFG.
        can_reach_vertex(decl_ir, anchor, write_vid)
    }

    fn ident_matches_decl(
        &self,
        ident_syntax: SyntaxId,
        decl_syn: SyntaxId,
        target_sym: Option<SymbolId>,
    ) -> bool {
        if ident_syntax == decl_syn {
            return true;
        }
        let Some(resolved) = self.semantic.resolve_declaration(ident_syntax) else {
            return false;
        };
        if resolved.primary_syntax == Some(decl_syn) {
            return true;
        }
        if let (Some(expected), Some(actual)) = (target_sym, resolved.symbol) {
            return expected == actual;
        }
        false
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Propagates property mutations on `this` in constructors/setters or local object bindings."
    )]
    fn propagate_prop_write_mutation(
        &mut self,
        base: &TraceItem,
        write_vid: VertexId,
        object_val: ValueId,
        prop_name: Option<&str>,
        gaps: &BTreeSet<usize>,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let ir = &base.ir;
        let mut next_fields = base.fields.clone();
        if let Some(p) = prop_name {
            next_fields
                .field_path
                .push(FieldSegment::Prop(p.to_string()));
        }

        // Check if `object_val` traces back to `ThisRef` or a local binding in `ir`.
        let mut is_this = false;
        let mut local_bindings = Vec::new();
        let mut work = vec![object_val];
        let mut seen = FxHashSet::default();
        while let Some(v_id) = work.pop() {
            if !seen.insert(v_id) {
                continue;
            }
            for vert in &ir.vertices {
                if vert.value == Some(v_id) {
                    match vert.op {
                        IrOp::ThisRef => is_this = true,
                        IrOp::ReadLocal { binding } => local_bindings.push(binding),
                        IrOp::ValueForward { source, .. } => work.push(source),
                        _ => {}
                    }
                }
            }
            if let Some(sources) = ir.value_sources.get(&v_id) {
                work.extend(sources.iter().copied());
            }
        }

        if is_this
            && let Some((top_frame, rest_stack)) = base.stack.split_last()
            && let CallFrameData::Direct {
                caller_ir,
                call_vertex,
                call_location,
                receiver_binding,
                is_construct,
                ..
            } = top_frame
        {
            if *is_construct
                && let Some(cv) = caller_ir.vertices.get(call_vertex.0 as usize)
                && let Some(out_val) = cv.value
            {
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    base,
                    Arc::clone(caller_ir),
                    TraceNode::Value(out_val),
                    call_location.clone(),
                    next_fields.clone(),
                    rest_stack.to_vec(),
                    prop_name.map(|p| {
                        (
                            "propertyWrite",
                            format!("Initialized constructed property '{p}'"),
                            Some(p.to_string()),
                            None,
                        )
                    }),
                    None,
                    gaps.clone(),
                );
            } else if let Some(recv_b) = *receiver_binding {
                self.propagate_mutated_binding(
                    base,
                    caller_ir,
                    *call_vertex,
                    recv_b,
                    &next_fields,
                    rest_stack,
                    prop_name,
                    gaps,
                    queue,
                    visited,
                    untainted_visited,
                );
            }
        }

        for lb in local_bindings {
            self.propagate_mutated_binding(
                base,
                ir,
                write_vid,
                lb,
                &next_fields,
                &base.stack,
                prop_name,
                gaps,
                queue,
                visited,
                untainted_visited,
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Propagates mutated object field state to subsequent reachable reads of the receiver local binding."
    )]
    fn propagate_mutated_binding(
        &mut self,
        base: &TraceItem,
        scope_ir: &Arc<FunctionIr>,
        mutation_vid: VertexId,
        binding: LocalBindingId,
        fields: &ValueFieldState,
        stack: &[CallFrameData],
        prop_name: Option<&str>,
        gaps: &BTreeSet<usize>,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let idx = self.index_for(scope_ir);
        let reads = idx
            .reads_of_binding
            .get(&binding)
            .cloned()
            .unwrap_or_default();
        for read_vid in reads {
            if !can_reach_vertex(scope_ir, mutation_vid, read_vid) {
                continue;
            }
            if let Some(read_v) = scope_ir.vertices.get(read_vid.0 as usize)
                && let Some(read_val) = read_v.value
            {
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    base,
                    Arc::clone(scope_ir),
                    TraceNode::Value(read_val),
                    read_v.location.clone(),
                    fields.clone(),
                    stack.to_vec(),
                    prop_name.map(|p| {
                        (
                            "propertyWrite",
                            format!("Mutated property '{p}' on receiver"),
                            Some(p.to_string()),
                            None,
                        )
                    }),
                    None,
                    gaps.clone(),
                );
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Handles behavior summary values flows, executor callback params, first-party call resolution, and missing summary gaps."
    )]
    fn handle_call_arrival(
        &mut self,
        item: &TraceItem,
        call_v: &CfgVertex,
        arrival: CallArrivalRole,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let (call_callee, call_receiver, call_args, is_construct, call_kind) = match &call_v.op {
            IrOp::Call {
                call_kind,
                callee,
                receiver,
                args,
            } => (*callee, *receiver, args.as_slice(), false, Some(*call_kind)),
            IrOp::Construct { callee, args, .. } => (*callee, None, args.as_slice(), true, None),
            _ => return,
        };

        // 0. Check if this call invokes a callback parameter of an active SummaryCallback frame
        //    (supporting `CallbackParamArg` such as `new Promise((resolve) => resolve(src))`).
        if let CallArrivalRole::Arg {
            arg_index,
            is_spread: false,
        } = arrival
            && !is_construct
            && let Some((top_frame, rest_stack)) = item.stack.split_last()
            && let CallFrameData::SummaryCallback {
                caller_ir,
                call_vertex,
                call_location,
                callback_arg_index,
                summary_label,
                summary_source,
                summary_pack_id,
                values_aspect,
                ..
            } = top_frame
            && let Some(cb_param_idx) = callee_param_index(&item.ir, call_callee)
        {
            let matching_flows: Vec<_> = values_aspect
                .flows
                .iter()
                .filter(|flow| {
                    matches!(
                        flow.from,
                        SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackParamArg {
                            callback_arg_index: cb_arg_idx,
                            callback_param_index: cb_param,
                            arg_index: a_idx,
                        }) if cb_arg_idx == *callback_arg_index && cb_param == cb_param_idx && a_idx == arg_index
                    )
                })
                .cloned()
                .collect();
            if !matching_flows.is_empty() {
                let outer_call_v = &caller_ir.vertices[call_vertex.0 as usize];
                let sum_ev = EvidenceItem::summary(
                    format!(
                        "Applied {} behavior summary '{}' for '{}'.",
                        summary_source.as_str(),
                        summary_pack_id,
                        summary_label
                    ),
                    call_location.clone(),
                    summary_source.as_str().to_string(),
                    summary_pack_id.clone(),
                    Some(summary_label.clone()),
                );
                for flow in &matching_flows {
                    self.apply_summary_flow_target(
                        item,
                        caller_ir,
                        outer_call_v,
                        &flow.to,
                        flow.preserve_object_path,
                        rest_stack,
                        Some(&sum_ev),
                        &item.gap_indices,
                        queue,
                        visited,
                        untainted_visited,
                    );
                }
                return;
            }
        }

        // Also check unstacked executor `resolve(src)` inside an inline callback passed to `new Promise(...)`.
        if let CallArrivalRole::Arg {
            arg_index,
            is_spread: false,
        } = arrival
            && !is_construct
            && item.stack.is_empty()
            && let Some(cb_param_idx) = callee_param_index(&item.ir, call_callee)
        {
            self.try_apply_enclosing_callback_param_arg(
                item,
                cb_param_idx,
                arg_index,
                queue,
                visited,
                untainted_visited,
            );
        }

        // 1. Check `values` behavior summary FIRST (summaries take priority over source bodies).
        if let Some((identity, matched_values)) =
            self.lookup_values_summary_for_call(&item.ir, call_v, call_callee, is_construct)
        {
            if self.conflict.is_some() {
                return;
            }
            self.semantic
                .record_summary_lookup(Some(matched_values.source));
            let label = describe_identity_short(&identity);
            let sum_ev = EvidenceItem::summary(
                format!(
                    "Applied {} behavior summary '{}' for '{}'.",
                    matched_values.source.as_str(),
                    matched_values.pack_id,
                    label
                ),
                call_v.location.clone(),
                matched_values.source.as_str().to_string(),
                matched_values.pack_id.clone(),
                Some(label.clone()),
            );

            let mut applied_any = false;
            for flow in &matched_values.aspect.flows {
                if let Some(transformed) =
                    match_summary_flow_source(&flow.from, arrival, &item.fields)
                {
                    applied_any = true;
                    let flow_item = TraceItem {
                        ir: Arc::clone(&item.ir),
                        node: item.node,
                        location: item.location.clone(),
                        fields: transformed,
                        stack: item.stack.clone(),
                        steps: item.steps.clone(),
                        extra_evidence: item.extra_evidence.clone(),
                        gap_indices: item.gap_indices.clone(),
                    };
                    self.apply_summary_flow_target(
                        &flow_item,
                        &item.ir,
                        call_v,
                        &flow.to,
                        flow.preserve_object_path,
                        &item.stack,
                        Some(&sum_ev),
                        &item.gap_indices,
                        queue,
                        visited,
                        untainted_visited,
                    );
                }
            }

            // If the summary aspect is partial, retain a gap-tainted fallback to the call's return
            // (and unmodeled callbacks) in case the actual flow uses an unsummarized path.
            if matched_values.aspect.completeness == SummaryCompleteness::Partial
                && !applied_any
                && let Some(out_val) = call_v.value
            {
                let gap_idx = self.record_gap(GapItem::from_reason(
                    UnknownReason::MissingBehaviorSummary,
                    format!(
                        "Behavior summary '{}' for '{}' declares partial 'values' completeness.",
                        matched_values.pack_id, label
                    ),
                    call_v.location.clone(),
                ));
                let mut next_gaps = item.gap_indices.clone();
                next_gaps.insert(gap_idx);
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(&item.ir),
                    TraceNode::Value(out_val),
                    call_v.location.clone(),
                    item.fields.clone(),
                    item.stack.clone(),
                    Some((
                        "summaryFlow",
                        format!("Partial behavior summary '{label}'"),
                        None,
                        None,
                    )),
                    Some(sum_ev),
                    next_gaps,
                );
            }
            return;
        }
        if self.conflict.is_some() {
            return;
        }

        // 2. Resolve call targets on IR (using `calls` summary forwarding / class overrides / first-party sources).
        let Some(call_syn) = call_v.syntax else {
            return;
        };
        let outcome = resolve_call_targets_with_summaries(
            call_syn,
            self.semantic,
            self.syntax,
            self.ir_provider,
            self.summary_index,
        );
        if let Some(conflict) = outcome.conflict {
            self.conflict = Some(conflict);
            return;
        }

        let mut base_gaps = item.gap_indices.clone();
        let mut had_gap = false;
        for g in &outcome.gaps {
            let gi = self.record_gap(g.clone());
            base_gaps.insert(gi);
            had_gap = true;
        }

        let mut forwarded_into_body = false;
        for target in &outcome.targets {
            if let Some(scope_syn) = target.fn_scope_syntax
                && let Ok(target_ir) = self.ir_provider.function_ir(scope_syn)
            {
                forwarded_into_body = true;
                self.enter_first_party_target(
                    item,
                    call_v,
                    call_receiver,
                    call_args,
                    is_construct,
                    call_kind,
                    target.name.as_deref(),
                    &target_ir,
                    arrival,
                    queue,
                    visited,
                    untainted_visited,
                );
            } else if target.is_external || target.is_default_library {
                self.semantic.record_summary_lookup(None);
                let target_label = target.name.as_deref().unwrap_or("external declaration");
                let gi = self.record_gap(GapItem::from_reason(
                    UnknownReason::MissingBehaviorSummary,
                    format!(
                        "External declaration '{target_label}' has no 'values' behavior summary."
                    ),
                    call_v.location.clone(),
                ));
                base_gaps.insert(gi);
                had_gap = true;
            } else if !had_gap {
                let target_label = target.name.as_deref().unwrap_or("<callee>");
                let gi = self.record_gap(GapItem::from_reason(
                    UnknownReason::MissingFunctionBody,
                    format!(
                        "Callee '{target_label}' has no implementation body; value propagation across this call cannot be verified."
                    ),
                    call_v.location.clone(),
                ));
                base_gaps.insert(gi);
                had_gap = true;
            }
        }

        if outcome.targets.is_empty() && !had_gap {
            let gi = self.record_gap(GapItem::from_reason(
                UnknownReason::DynamicCallee,
                "Call target could not be statically determined.".to_string(),
                call_v.location.clone(),
            ));
            base_gaps.insert(gi);
            had_gap = true;
        }

        // If the call has unknown targets or missing summary/body, propagate a gap-tainted transition
        // to its return value (and to any inline callback parameters) so critical unknown paths produce `inconclusive`.
        if had_gap || !outcome.is_complete {
            if base_gaps == item.gap_indices && !outcome.is_complete {
                let gi = self.record_gap(GapItem::from_reason(
                    UnknownReason::DynamicCallee,
                    "Call target resolution is incomplete.".to_string(),
                    call_v.location.clone(),
                ));
                base_gaps.insert(gi);
            }
            if let Some(out_val) = call_v.value {
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    Arc::clone(&item.ir),
                    TraceNode::Value(out_val),
                    call_v.location.clone(),
                    item.fields.clone(),
                    item.stack.clone(),
                    Some((
                        "returnValue",
                        "Potential return value across unresolved or unsummarized call".to_string(),
                        None,
                        None,
                    )),
                    None,
                    base_gaps.clone(),
                );
            }
            if !forwarded_into_body {
                self.taint_inline_callbacks_at_call(
                    item,
                    call_args,
                    &base_gaps,
                    queue,
                    visited,
                    untainted_visited,
                );
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Binds caller argument or receiver state into a resolved first-party function, method, constructor, or getter."
    )]
    fn enter_first_party_target(
        &mut self,
        item: &TraceItem,
        call_v: &CfgVertex,
        call_receiver: Option<ValueId>,
        call_args: &[IrCallArg],
        is_construct: bool,
        call_kind: Option<IrCallKind>,
        target_name: Option<&str>,
        target_ir: &Arc<FunctionIr>,
        arrival: CallArrivalRole,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        if item.stack.len() >= MAX_CALL_STACK_DEPTH {
            return;
        }
        let receiver_binding =
            call_receiver.and_then(|rv| resolve_local_binding_of_value(&item.ir, rv));
        let mut next_stack = item.stack.clone();
        next_stack.push(CallFrameData::Direct {
            caller_ir: Arc::clone(&item.ir),
            call_vertex: call_v.id,
            call_location: call_v.location.clone(),
            receiver_binding,
            is_construct,
        });

        match arrival {
            CallArrivalRole::Receiver => {
                // Bind `ThisRef` vertices inside the method / getter / setter body.
                let fn_label = target_name.unwrap_or("<method>");
                for vert in &target_ir.vertices {
                    if matches!(vert.op, IrOp::ThisRef)
                        && let Some(this_val) = vert.value
                    {
                        Self::enqueue_transition(
                            queue,
                            visited,
                            untainted_visited,
                            item,
                            Arc::clone(target_ir),
                            TraceNode::Value(this_val),
                            vert.location.clone().or_else(|| call_v.location.clone()),
                            item.fields.clone(),
                            next_stack.clone(),
                            Some((
                                "callArgument",
                                format!("Passed receiver 'this' into '{fn_label}'"),
                                None,
                                None,
                            )),
                            None,
                            item.gap_indices.clone(),
                        );
                    }
                }
            }
            CallArrivalRole::Arg {
                arg_index,
                is_spread,
            } => {
                if target_ir.parameters.is_empty() {
                    return;
                }
                let _ = call_kind;
                let fn_label = target_name.unwrap_or("<callee>");
                let args_offset = resolve_call_targets_with_summaries(
                    call_v.syntax.unwrap_or(target_ir.root_syntax),
                    self.semantic,
                    self.syntax,
                    self.ir_provider,
                    self.summary_index,
                )
                .targets
                .into_iter()
                .find(|t| t.fn_scope_syntax == Some(target_ir.root_syntax))
                .and_then(|t| t.parameter_bindings.first().map(|pb| pb.argument_index))
                .unwrap_or(0);

                // If `args_offset` shifted (e.g. `fn.call(thisArg, arg1)`), compute effective index.
                let shifted_idx = if arg_index >= args_offset {
                    arg_index - args_offset
                } else if args_offset > 0 && call_args.len() > args_offset {
                    return;
                } else {
                    arg_index
                };

                if is_spread {
                    let spread_prefix_unknown = call_args[..arg_index]
                        .iter()
                        .any(|a| matches!(a, IrCallArg::Spread(_)));
                    for (p_idx, param) in target_ir.parameters.iter().enumerate() {
                        if param.is_rest {
                            let mut rest_fields = item.fields.clone();
                            if let Some(FieldSegment::Element(idx_opt)) =
                                rest_fields.field_path.last_mut()
                            {
                                let rel = p_idx.saturating_sub(shifted_idx);
                                if spread_prefix_unknown {
                                    *idx_opt = None;
                                } else if let Some(i) = *idx_opt {
                                    if i < rel {
                                        continue;
                                    }
                                    *idx_opt = Some(i - rel);
                                }
                            }
                            self.bind_into_parameter(
                                item,
                                target_ir,
                                param,
                                fn_label,
                                rest_fields,
                                &next_stack,
                                &item.gap_indices,
                                queue,
                                visited,
                                untainted_visited,
                            );
                        } else if p_idx >= shifted_idx {
                            let mut elem_fields = item.fields.clone();
                            match elem_fields.field_path.last() {
                                Some(FieldSegment::Element(idx_opt)) => {
                                    let rel = p_idx - shifted_idx;
                                    if !spread_prefix_unknown && idx_opt.is_some_and(|i| i != rel) {
                                        continue;
                                    }
                                    elem_fields.field_path.pop();
                                }
                                Some(FieldSegment::Prop(_)) => continue,
                                None => {}
                            }
                            self.bind_into_parameter(
                                item,
                                target_ir,
                                param,
                                fn_label,
                                elem_fields,
                                &next_stack,
                                &item.gap_indices,
                                queue,
                                visited,
                                untainted_visited,
                            );
                        }
                    }
                    return;
                }

                let has_spread_before = call_args[..arg_index]
                    .iter()
                    .any(|a| matches!(a, IrCallArg::Spread(_)));
                let last_is_rest = target_ir.parameters.last().is_some_and(|p| p.is_rest);
                if has_spread_before {
                    for param in target_ir.parameters.iter().skip(shifted_idx) {
                        let mut next_fields = item.fields.clone();
                        if param.is_rest
                            && !push_field_segment(
                                queue,
                                &mut next_fields,
                                FieldSegment::Element(None),
                                call_v
                                    .location
                                    .clone()
                                    .or_else(|| Some(item.ir.scope_location.clone())),
                            )
                        {
                            continue;
                        }
                        self.bind_into_parameter(
                            item,
                            target_ir,
                            param,
                            fn_label,
                            next_fields,
                            &next_stack,
                            &item.gap_indices,
                            queue,
                            visited,
                            untainted_visited,
                        );
                    }
                } else if let Some(param) = target_ir.parameters.get(shifted_idx) {
                    let mut next_fields = item.fields.clone();
                    if param.is_rest
                        && !push_field_segment(
                            queue,
                            &mut next_fields,
                            FieldSegment::Element(Some(0)),
                            call_v
                                .location
                                .clone()
                                .or_else(|| Some(item.ir.scope_location.clone())),
                        )
                    {
                        return;
                    }
                    self.bind_into_parameter(
                        item,
                        target_ir,
                        param,
                        fn_label,
                        next_fields,
                        &next_stack,
                        &item.gap_indices,
                        queue,
                        visited,
                        untainted_visited,
                    );
                } else if last_is_rest && let Some(rest_param) = target_ir.parameters.last() {
                    let rest_idx = target_ir.parameters.len() - 1;
                    let mut next_fields = item.fields.clone();
                    if !push_field_segment(
                        queue,
                        &mut next_fields,
                        FieldSegment::Element(Some(shifted_idx - rest_idx)),
                        call_v
                            .location
                            .clone()
                            .or_else(|| Some(item.ir.scope_location.clone())),
                    ) {
                        return;
                    }
                    self.bind_into_parameter(
                        item,
                        target_ir,
                        rest_param,
                        fn_label,
                        next_fields,
                        &next_stack,
                        &item.gap_indices,
                        queue,
                        visited,
                        untainted_visited,
                    );
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Binds an incoming value state to a FunctionIr parameter's raw value or local binding."
    )]
    fn bind_into_parameter(
        &mut self,
        base: &TraceItem,
        target_ir: &Arc<FunctionIr>,
        param: &crate::ir::IrParameter,
        fn_label: &str,
        next_fields: ValueFieldState,
        next_stack: &[CallFrameData],
        gaps: &BTreeSet<usize>,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let param_loc = self.syntax.syntax_location(param.syntax);
        let desc = format!(
            "Passed argument to parameter '{}' of '{fn_label}'",
            param.name
        );

        Self::enqueue_transition(
            queue,
            visited,
            untainted_visited,
            base,
            Arc::clone(target_ir),
            TraceNode::Binding(param.binding.unwrap_or(LocalBindingId(u32::MAX))),
            param_loc.clone(),
            next_fields.clone(),
            next_stack.to_vec(),
            Some(("callArgument", desc.clone(), None, None)),
            None,
            gaps.clone(),
        );

        if let Some(rv) = param.raw_value {
            Self::enqueue_transition(
                queue,
                visited,
                untainted_visited,
                base,
                Arc::clone(target_ir),
                TraceNode::Value(rv),
                param_loc,
                next_fields,
                next_stack.to_vec(),
                Some(("callArgument", desc, None, None)),
                None,
                gaps.clone(),
            );
            return;
        }

        if let Some(binding) = param.binding {
            let target_idx = self.index_for(target_ir);
            let reads = target_idx
                .reads_of_binding
                .get(&binding)
                .cloned()
                .unwrap_or_default();
            for read_vid in reads {
                let rw = var_writes::resolve_reaching_writes_for_local(
                    target_ir,
                    read_vid,
                    binding,
                    self.semantic,
                    self.syntax,
                    self.ir_provider,
                );
                if rw.entry_param_reaches
                    && let Some(read_v) = target_ir.vertices.get(read_vid.0 as usize)
                    && let Some(read_val) = read_v.value
                {
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        base,
                        Arc::clone(target_ir),
                        TraceNode::Value(read_val),
                        read_v.location.clone(),
                        next_fields.clone(),
                        next_stack.to_vec(),
                        Some(("callArgument", desc.clone(), None, None)),
                        None,
                        gaps.clone(),
                    );
                }
            }
            if let Some(b_info) = target_ir.bindings.get(binding.0 as usize) {
                let decl_sym = self.semantic.symbol_at_syntax(b_info.syntax);
                let captured_base = TraceItem {
                    ir: Arc::clone(target_ir),
                    node: TraceNode::Binding(binding),
                    location: param_loc,
                    fields: next_fields.clone(),
                    stack: next_stack.to_vec(),
                    steps: {
                        let mut s = base.steps.clone();
                        if let (Some(fl), Some(tl)) = (
                            base.location.clone(),
                            self.syntax.syntax_location(param.syntax),
                        ) {
                            s.push(ValuePropagationStepData {
                                kind: "callArgument",
                                description: desc,
                                from_location: fl,
                                to_location: tl,
                                property: None,
                                omitted_properties: None,
                            });
                        }
                        s
                    },
                    extra_evidence: base.extra_evidence.clone(),
                    gap_indices: gaps.clone(),
                };
                self.propagate_decl_reads_in_nested_scopes(
                    &captured_base,
                    target_ir,
                    b_info.syntax,
                    decl_sym,
                    &b_info.name,
                    None,
                    &next_fields,
                    gaps,
                    queue,
                    visited,
                    untainted_visited,
                );
            }
        }
    }

    fn taint_inline_callbacks_at_call(
        &mut self,
        item: &TraceItem,
        call_args: &[IrCallArg],
        gaps: &BTreeSet<usize>,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        for arg in call_args {
            let IrCallArg::Positional(arg_val) = arg else {
                continue;
            };
            let cb_syns = find_function_literals_for_value(&item.ir, *arg_val);
            for cb_syn in cb_syns {
                if let Ok(cb_ir) = self.ir_provider.function_ir(cb_syn) {
                    for param in &cb_ir.parameters {
                        self.bind_into_parameter(
                            item,
                            &cb_ir,
                            param,
                            "<callback>",
                            item.fields.clone(),
                            &[],
                            gaps,
                            queue,
                            visited,
                            untainted_visited,
                        );
                    }
                }
            }
        }
    }

    fn lookup_values_summary_for_call(
        &mut self,
        ir: &FunctionIr,
        call_v: &CfgVertex,
        call_callee: ValueId,
        is_construct: bool,
    ) -> Option<(
        DeclarationIdentity,
        MatchedSummaryAspect<ValueAspectSummary>,
    )> {
        let mut lookup_syns = Vec::new();
        if let Some(cs) = call_v.syntax {
            lookup_syns.push(cs);
        }
        if let Some(callee_syn) = value_syntax_in_ir(ir, call_callee, self.syntax)
            && !lookup_syns.contains(&callee_syn)
        {
            lookup_syns.push(callee_syn);
        }
        if let Some(decl_syn) = call_v
            .syntax
            .and_then(|s| self.semantic.resolve_declaration(s))
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
                .resolve_values_summary(&identity, is_construct)
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
        reason = "Applies a ValueFlowSpec target (return, returnElement, callbackParam, receiver) cleanly."
    )]
    fn apply_summary_flow_target(
        &mut self,
        item: &TraceItem,
        call_ir: &Arc<FunctionIr>,
        call_v: &CfgVertex,
        target: &SummaryValueRef,
        preserve_object_path: Option<bool>,
        stack: &[CallFrameData],
        sum_ev: Option<&EvidenceItem>,
        gaps: &BTreeSet<usize>,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let mut next_fields = item.fields.clone();
        if preserve_object_path == Some(false) {
            next_fields.source_path.clear();
            next_fields.omitted_properties.clear();
        }
        let call_args = match &call_v.op {
            IrOp::Call { args, .. } | IrOp::Construct { args, .. } => args.as_slice(),
            _ => &[],
        };
        let call_recv = match &call_v.op {
            IrOp::Call { receiver, .. } => *receiver,
            _ => None,
        };

        match target {
            SummaryValueRef::Named(SummaryNamedValueRef::Return) => {
                if let Some(out_val) = call_v.value {
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        item,
                        Arc::clone(call_ir),
                        TraceNode::Value(out_val),
                        call_v.location.clone(),
                        next_fields,
                        stack.to_vec(),
                        Some((
                            "summaryFlow",
                            "Value propagates to summarized return value".to_string(),
                            None,
                            None,
                        )),
                        sum_ev.cloned(),
                        gaps.clone(),
                    );
                }
            }
            SummaryValueRef::Named(SummaryNamedValueRef::ReturnElement) => {
                if let Some(out_val) = call_v.value {
                    if !push_field_segment(
                        queue,
                        &mut next_fields,
                        FieldSegment::Element(None),
                        call_v
                            .location
                            .clone()
                            .or_else(|| Some(call_ir.scope_location.clone())),
                    ) {
                        return;
                    }
                    Self::enqueue_transition(
                        queue,
                        visited,
                        untainted_visited,
                        item,
                        Arc::clone(call_ir),
                        TraceNode::Value(out_val),
                        call_v.location.clone(),
                        next_fields,
                        stack.to_vec(),
                        Some((
                            "summaryFlow",
                            "Value propagates into summarized return container".to_string(),
                            None,
                            None,
                        )),
                        sum_ev.cloned(),
                        gaps.clone(),
                    );
                }
            }
            SummaryValueRef::Named(
                SummaryNamedValueRef::Receiver | SummaryNamedValueRef::ReceiverElement,
            ) => {
                if matches!(
                    target,
                    SummaryValueRef::Named(SummaryNamedValueRef::ReceiverElement)
                ) && !push_field_segment(
                    queue,
                    &mut next_fields,
                    FieldSegment::Element(None),
                    call_v
                        .location
                        .clone()
                        .or_else(|| Some(call_ir.scope_location.clone())),
                ) {
                    return;
                }
                if let Some(recv_val) = call_recv
                    && let Some(recv_b) = resolve_local_binding_of_value(call_ir, recv_val)
                {
                    let mut next_gaps = gaps.clone();
                    let _ = &mut next_gaps;
                    self.propagate_mutated_binding(
                        item,
                        call_ir,
                        call_v.id,
                        recv_b,
                        &next_fields,
                        stack,
                        None,
                        gaps,
                        queue,
                        visited,
                        untainted_visited,
                    );
                }
            }
            SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackParam {
                callback_arg_index,
                param_index,
            }) => {
                let Some(arg) = call_args.get(*callback_arg_index) else {
                    return;
                };
                let IrCallArg::Positional(cb_val) = arg else {
                    return;
                };
                let cb_syns = find_function_literals_for_value(call_ir, *cb_val);
                if cb_syns.is_empty() {
                    if let Some(out_val) = call_v.value {
                        let gi = self.record_gap(GapItem::from_reason(
                            UnknownReason::DynamicCallee,
                            "Callback argument in summarized call could not be statically resolved."
                                .to_string(),
                            call_v.location.clone(),
                        ));
                        let mut next_gaps = gaps.clone();
                        next_gaps.insert(gi);
                        Self::enqueue_transition(
                            queue,
                            visited,
                            untainted_visited,
                            item,
                            Arc::clone(call_ir),
                            TraceNode::Value(out_val),
                            call_v.location.clone(),
                            next_fields,
                            stack.to_vec(),
                            Some((
                                "summaryFlow",
                                "Dynamic callback in summarized call".to_string(),
                                None,
                                None,
                            )),
                            sum_ev.cloned(),
                            next_gaps,
                        );
                    }
                    return;
                }
                if stack.len() >= MAX_CALL_STACK_DEPTH {
                    return;
                }
                let call_callee = match &call_v.op {
                    IrOp::Call { callee, .. } | IrOp::Construct { callee, .. } => *callee,
                    _ => return,
                };
                let Some((id, matched_vals)) = self.lookup_values_summary_for_call(
                    call_ir,
                    call_v,
                    call_callee,
                    matches!(call_v.op, IrOp::Construct { .. }),
                ) else {
                    return;
                };
                let mut cb_stack = stack.to_vec();
                cb_stack.push(CallFrameData::SummaryCallback {
                    caller_ir: Arc::clone(call_ir),
                    call_vertex: call_v.id,
                    call_location: call_v.location.clone(),
                    callback_arg_index: *callback_arg_index,
                    summary_label: describe_identity_short(&id),
                    summary_source: matched_vals.source,
                    summary_pack_id: matched_vals.pack_id,
                    values_aspect: matched_vals.aspect,
                });

                for cb_syn in cb_syns {
                    let Ok(cb_ir) = self.ir_provider.function_ir(cb_syn) else {
                        continue;
                    };
                    if let Some(param) = cb_ir.parameters.get(*param_index) {
                        let mut with_ev = item.clone();
                        if let Some(ev) = sum_ev
                            && !with_ev.extra_evidence.contains(ev)
                        {
                            with_ev.extra_evidence.push(ev.clone());
                        }
                        self.bind_into_parameter(
                            &with_ev,
                            &cb_ir,
                            param,
                            "<callback>",
                            next_fields.clone(),
                            &cb_stack,
                            gaps,
                            queue,
                            visited,
                            untainted_visited,
                        );
                    }
                }
            }
            SummaryValueRef::Tagged(
                SummaryTaggedValueRef::Arg { .. }
                | SummaryTaggedValueRef::ArgElement { .. }
                | SummaryTaggedValueRef::CallbackReturn { .. }
                | SummaryTaggedValueRef::CallbackParamArg { .. },
            ) => {}
        }
    }

    fn try_apply_enclosing_callback_param_arg(
        &mut self,
        item: &TraceItem,
        cb_param_idx: usize,
        arg_index: usize,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        let cb_syn = item.ir.root_syntax;
        let Ok(outer_ir) = self.ir_provider.enclosing_scope_ir(cb_syn) else {
            return;
        };
        if outer_ir.id == item.ir.id {
            return;
        }
        let cb_vals = collect_callback_values_in_scope(&outer_ir, cb_syn, self.semantic);
        if cb_vals.is_empty() {
            return;
        }
        for v in &outer_ir.vertices {
            let (outer_callee, outer_args, is_construct) = match &v.op {
                IrOp::Call { callee, args, .. } => (*callee, args.as_slice(), false),
                IrOp::Construct { callee, args, .. } => (*callee, args.as_slice(), true),
                _ => continue,
            };
            for (cb_arg_idx, arg) in outer_args.iter().enumerate() {
                let IrCallArg::Positional(av) = arg else {
                    continue;
                };
                if !cb_vals.contains(av) {
                    continue;
                }
                if let Some((identity, matched)) =
                    self.lookup_values_summary_for_call(&outer_ir, v, outer_callee, is_construct)
                {
                    self.semantic.record_summary_lookup(Some(matched.source));
                    let label = describe_identity_short(&identity);
                    let sum_ev = EvidenceItem::summary(
                        format!(
                            "Applied {} behavior summary '{}' for '{}'.",
                            matched.source.as_str(),
                            matched.pack_id,
                            label
                        ),
                        v.location.clone(),
                        matched.source.as_str().to_string(),
                        matched.pack_id.clone(),
                        Some(label),
                    );
                    for flow in &matched.aspect.flows {
                        if matches!(
                            flow.from,
                            SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackParamArg {
                                callback_arg_index,
                                callback_param_index,
                                arg_index: expected_arg,
                            }) if callback_arg_index == cb_arg_idx
                                && callback_param_index == cb_param_idx
                                && expected_arg == arg_index
                        ) {
                            self.apply_summary_flow_target(
                                item,
                                &outer_ir,
                                v,
                                &flow.to,
                                flow.preserve_object_path,
                                &[],
                                Some(&sum_ev),
                                &item.gap_indices,
                                queue,
                                visited,
                                untainted_visited,
                            );
                        }
                    }
                }
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Handles direct call returns, summarized callback returns, and unanchored caller graph returns."
    )]
    fn handle_return(
        &mut self,
        item: &TraceItem,
        ret_v: &CfgVertex,
        queue: &mut DualQueue,
        visited: &mut FxHashSet<TraceKey>,
        untainted_visited: &mut FxHashSet<(
            FunctionIrId,
            TraceNode,
            ValueFieldState,
            Vec<CallFrameKey>,
        )>,
    ) {
        if let Some((top_frame, rest_stack)) = item.stack.split_last() {
            match top_frame {
                CallFrameData::Direct {
                    caller_ir,
                    call_vertex,
                    call_location,
                    ..
                } => {
                    if let Some(cv) = caller_ir.vertices.get(call_vertex.0 as usize)
                        && let Some(out_val) = cv.value
                    {
                        Self::enqueue_transition(
                            queue,
                            visited,
                            untainted_visited,
                            item,
                            Arc::clone(caller_ir),
                            TraceNode::Value(out_val),
                            call_location.clone().or_else(|| cv.location.clone()),
                            item.fields.clone(),
                            rest_stack.to_vec(),
                            Some((
                                "returnValue",
                                "Return value from callee to call site".to_string(),
                                None,
                                None,
                            )),
                            None,
                            item.gap_indices.clone(),
                        );
                    }
                }
                CallFrameData::SummaryCallback {
                    caller_ir,
                    call_vertex,
                    call_location,
                    callback_arg_index,
                    summary_label,
                    summary_source,
                    summary_pack_id,
                    values_aspect,
                    ..
                } => {
                    let Some(cv) = caller_ir.vertices.get(call_vertex.0 as usize) else {
                        return;
                    };
                    let sum_ev = EvidenceItem::summary(
                        format!(
                            "Applied {} behavior summary '{}' for '{}'.",
                            summary_source.as_str(),
                            summary_pack_id,
                            summary_label
                        ),
                        call_location.clone(),
                        summary_source.as_str().to_string(),
                        summary_pack_id.clone(),
                        Some(summary_label.clone()),
                    );
                    for flow in &values_aspect.flows {
                        if matches!(
                            flow.from,
                            SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackReturn {
                                callback_arg_index: idx,
                            }) if idx == *callback_arg_index
                        ) {
                            self.apply_summary_flow_target(
                                item,
                                caller_ir,
                                cv,
                                &flow.to,
                                flow.preserve_object_path,
                                rest_stack,
                                Some(&sum_ev),
                                &item.gap_indices,
                                queue,
                                visited,
                                untainted_visited,
                            );
                        }
                    }
                }
            }
            return;
        }

        // Empty call stack: first check if this function is an inline callback passed to an enclosing
        // summarized call (e.g. `from` starts inside `() => src` in `[1].map(() => src)`).
        let fn_syn = item.ir.root_syntax;
        if let Ok(outer_ir) = self.ir_provider.enclosing_scope_ir(fn_syn)
            && outer_ir.id != item.ir.id
        {
            let cb_vals = collect_callback_values_in_scope(&outer_ir, fn_syn, self.semantic);
            if !cb_vals.is_empty() {
                for v in &outer_ir.vertices {
                    let (outer_callee, outer_args, is_construct) = match &v.op {
                        IrOp::Call { callee, args, .. } => (*callee, args.as_slice(), false),
                        IrOp::Construct { callee, args, .. } => (*callee, args.as_slice(), true),
                        _ => continue,
                    };
                    for (cb_idx, arg) in outer_args.iter().enumerate() {
                        let IrCallArg::Positional(av) = arg else {
                            continue;
                        };
                        if !cb_vals.contains(av) {
                            continue;
                        }
                        if let Some((identity, matched)) = self.lookup_values_summary_for_call(
                            &outer_ir,
                            v,
                            outer_callee,
                            is_construct,
                        ) {
                            self.semantic.record_summary_lookup(Some(matched.source));
                            let label = describe_identity_short(&identity);
                            let sum_ev = EvidenceItem::summary(
                                format!(
                                    "Applied {} behavior summary '{}' for '{}'.",
                                    matched.source.as_str(),
                                    matched.pack_id,
                                    label
                                ),
                                v.location.clone(),
                                matched.source.as_str().to_string(),
                                matched.pack_id.clone(),
                                Some(label),
                            );
                            for flow in &matched.aspect.flows {
                                if matches!(
                                    flow.from,
                                    SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackReturn {
                                        callback_arg_index,
                                    }) if callback_arg_index == cb_idx
                                ) {
                                    self.apply_summary_flow_target(
                                        item,
                                        &outer_ir,
                                        v,
                                        &flow.to,
                                        flow.preserve_object_path,
                                        &[],
                                        Some(&sum_ev),
                                        &item.gap_indices,
                                        queue,
                                        visited,
                                        untainted_visited,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Unanchored reverse caller lookup via `IrCallGraph`, filtered by `allowed_reverse_files` (`lookupFiles`).
        if let Some(allowed) = &self.allowed_reverse_files
            && allowed.is_empty()
        {
            return;
        }
        let callee_sym = self.semantic.symbol_at_syntax(fn_syn);
        let allowed_clone = self.allowed_reverse_files.clone();
        let caller_sites: Vec<SyntaxId> = {
            let graph = self.call_graph();
            let mut indices = BTreeSet::new();
            if let Some(list) = graph.by_decl.get(&fn_syn) {
                indices.extend(list.iter().copied());
            }
            if let Some(sym) = callee_sym
                && let Some(list) = graph.by_symbol.get(&sym)
            {
                indices.extend(list.iter().copied());
            }
            indices
                .into_iter()
                .filter_map(|idx| {
                    let site = &graph.sites[idx];
                    if allowed_clone
                        .as_ref()
                        .is_some_and(|allowed| !allowed.contains(&site.file))
                    {
                        return None;
                    }
                    Some(site.call_syntax)
                })
                .collect()
        };
        if self.conflict.is_some() {
            return;
        }

        let _ = ret_v;
        for call_syn in caller_sites {
            let Ok(caller_ir) = self.ir_provider.enclosing_scope_ir(call_syn) else {
                continue;
            };
            if let Some(call_val) = caller_ir.expr_to_value.get(&call_syn.local_id()).copied() {
                let call_loc = self.syntax.syntax_location(call_syn);
                Self::enqueue_transition(
                    queue,
                    visited,
                    untainted_visited,
                    item,
                    caller_ir,
                    TraceNode::Value(call_val),
                    call_loc,
                    item.fields.clone(),
                    Vec::new(),
                    Some((
                        "returnValue",
                        "Return value from function to caller".to_string(),
                        None,
                        None,
                    )),
                    None,
                    item.gap_indices.clone(),
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
enum CallArrivalRole {
    Receiver,
    Arg { arg_index: usize, is_spread: bool },
}

fn build_path_data(item: &TraceItem) -> ValuePropagationPathData {
    let mut evidence = Vec::with_capacity(item.steps.len() + item.extra_evidence.len() + 1);
    if let Some(first) = item.steps.first() {
        evidence.push(EvidenceItem::simple(
            "source",
            "Origin value starts here".to_string(),
            Some(first.from_location.clone()),
        ));
    } else if let Some(loc) = &item.location {
        evidence.push(EvidenceItem::simple(
            "source",
            "Origin value matches target directly".to_string(),
            Some(loc.clone()),
        ));
    }
    for ev in &item.extra_evidence {
        if !evidence.contains(ev) {
            evidence.push(ev.clone());
        }
    }
    for step in &item.steps {
        evidence.push(EvidenceItem::simple(
            "source",
            step.description.clone(),
            Some(step.to_location.clone()),
        ));
    }
    ValuePropagationPathData {
        steps: item.steps.clone(),
        evidence,
        source_path: item.fields.source_path.clone(),
        omitted_properties: item.fields.omitted_properties.iter().cloned().collect(),
    }
}

const fn is_mutable(mutability: BindingMutability) -> bool {
    matches!(
        mutability,
        BindingMutability::Let
            | BindingMutability::Var
            | BindingMutability::Parameter
            | BindingMutability::CatchBinding
    )
}

fn match_summary_flow_source(
    from_ref: &SummaryValueRef,
    arrival: CallArrivalRole,
    fields: &ValueFieldState,
) -> Option<ValueFieldState> {
    match (from_ref, arrival) {
        (SummaryValueRef::Named(SummaryNamedValueRef::Receiver), CallArrivalRole::Receiver) => {
            Some(fields.clone())
        }
        (
            SummaryValueRef::Named(SummaryNamedValueRef::ReceiverElement),
            CallArrivalRole::Receiver,
        ) => unwrap_element_field(fields),
        (
            SummaryValueRef::Tagged(SummaryTaggedValueRef::Arg { index }),
            CallArrivalRole::Arg {
                arg_index,
                is_spread: false,
            },
        ) if *index == arg_index => Some(fields.clone()),
        (
            SummaryValueRef::Tagged(SummaryTaggedValueRef::ArgElement { index }),
            CallArrivalRole::Arg {
                arg_index,
                is_spread: false,
            },
        ) if *index == arg_index => unwrap_element_field(fields),
        _ => None,
    }
}

fn unwrap_element_field(fields: &ValueFieldState) -> Option<ValueFieldState> {
    match fields.field_path.last() {
        Some(FieldSegment::Element(_)) => {
            let mut next = fields.clone();
            next.field_path.pop();
            Some(next)
        }
        Some(FieldSegment::Prop(_)) => None,
        None => Some(fields.clone()),
    }
}

fn resolve_local_binding_of_value(ir: &FunctionIr, val: ValueId) -> Option<LocalBindingId> {
    let mut work = vec![val];
    let mut seen = FxHashSet::default();
    while let Some(cur) = work.pop() {
        if !seen.insert(cur) {
            continue;
        }
        for v in &ir.vertices {
            if v.value == Some(cur) {
                match v.op {
                    IrOp::ReadLocal { binding } => return Some(binding),
                    IrOp::ValueForward { source, .. } => work.push(source),
                    _ => {}
                }
            }
        }
    }
    None
}

fn find_function_literals_for_value(ir: &FunctionIr, val: ValueId) -> Vec<SyntaxId> {
    let mut out = Vec::new();
    let mut work = vec![val];
    let mut seen = FxHashSet::default();
    let mut local_bindings = FxHashSet::default();

    while let Some(cur) = work.pop() {
        if !seen.insert(cur) {
            continue;
        }
        if let Some(sources) = ir.value_sources.get(&cur) {
            work.extend(sources.iter().copied());
        }
        for v in &ir.vertices {
            if v.value == Some(cur) {
                match v.op {
                    IrOp::FunctionLiteral { syntax, .. } => {
                        if !out.contains(&syntax) {
                            out.push(syntax);
                        }
                    }
                    IrOp::ValueForward { source, .. } => work.push(source),
                    IrOp::ReadLocal { binding } if local_bindings.insert(binding) => {
                        for av in &ir.vertices {
                            if let IrOp::AssignLocal { target, value, .. } = av.op
                                && target == binding
                            {
                                work.push(value);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

fn collect_callback_values_in_scope(
    outer_ir: &FunctionIr,
    cb_syn: SyntaxId,
    semantic: &dyn SemanticModel,
) -> FxHashSet<ValueId> {
    let mut cb_vals = FxHashSet::default();
    let mut cb_bindings = FxHashSet::default();
    for v in &outer_ir.vertices {
        match &v.op {
            IrOp::FunctionLiteral { syntax, .. } if *syntax == cb_syn => {
                if let Some(val) = v.value {
                    cb_vals.insert(val);
                }
            }
            IrOp::ReadSymbol { ident_syntax } => {
                if semantic
                    .resolve_declaration(*ident_syntax)
                    .and_then(|d| d.primary_syntax)
                    == Some(cb_syn)
                    && let Some(val) = v.value
                {
                    cb_vals.insert(val);
                }
            }
            _ => {}
        }
    }
    if cb_vals.is_empty() {
        return cb_vals;
    }
    let mut changed = true;
    while changed {
        changed = false;
        for v in &outer_ir.vertices {
            match &v.op {
                IrOp::ValueForward { source, .. } if cb_vals.contains(source) => {
                    if let Some(val) = v.value
                        && cb_vals.insert(val)
                    {
                        changed = true;
                    }
                }
                IrOp::AssignLocal { target, value, .. } if cb_vals.contains(value) => {
                    if cb_bindings.insert(*target) {
                        changed = true;
                    }
                    if let Some(val) = v.value
                        && cb_vals.insert(val)
                    {
                        changed = true;
                    }
                }
                IrOp::ReadLocal { binding } if cb_bindings.contains(binding) => {
                    if let Some(val) = v.value
                        && cb_vals.insert(val)
                    {
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        for (&derived, sources) in &outer_ir.value_sources {
            if sources.iter().any(|s| cb_vals.contains(s)) && cb_vals.insert(derived) {
                changed = true;
            }
        }
    }
    cb_vals
}

fn callee_param_index(ir: &FunctionIr, callee_val: ValueId) -> Option<usize> {
    let mut work = vec![callee_val];
    let mut seen = FxHashSet::default();
    while let Some(cur) = work.pop() {
        if !seen.insert(cur) {
            continue;
        }
        for v in &ir.vertices {
            if v.value == Some(cur) {
                match v.op {
                    IrOp::ReadLocal { binding } => {
                        if let Some(idx) = ir
                            .parameters
                            .iter()
                            .position(|p| p.binding == Some(binding))
                        {
                            return Some(idx);
                        }
                    }
                    IrOp::ValueForward { source, .. } => work.push(source),
                    _ => {}
                }
            }
        }
    }
    None
}

fn can_reach_vertex(ir: &FunctionIr, from: VertexId, target: VertexId) -> bool {
    if from == target {
        return true;
    }
    let mut queue = VecDeque::new();
    let mut seen = FxHashSet::default();
    queue.push_back(from);
    seen.insert(from);
    while let Some(vid) = queue.pop_front() {
        let Some(v) = ir.vertices.get(vid.0 as usize) else {
            continue;
        };
        for edge in &v.explicit_edges {
            if edge.target == target {
                return true;
            }
            if seen.insert(edge.target) {
                queue.push_back(edge.target);
            }
        }
    }
    false
}

fn describe_identity_short(identity: &DeclarationIdentity) -> String {
    let member = identity
        .member_paths
        .first()
        .map_or_else(|| "<symbol>".to_string(), |p| p.join("."));
    match &identity.package_name {
        Some(pkg) => format!("{pkg}:{member}"),
        None => member,
    }
}

#[cfg(test)]
mod tests {
    use tssae_common::{
        DeclarationSummaryData, ImplementationFact, LocalSyntaxId, MemberTargetHierarchy,
        ResolvedImplementationFact, SignatureId, TypeId, TypeSummaryData,
    };

    use super::*;
    use crate::ir::{
        BindingMutability, CfgVertex, DeclarationSemantics, ExplicitEdge, IrLocalBinding,
        ScopeCatalogEntry,
    };

    struct SyntheticEnv {
        irs: FxHashMap<SyntaxId, Arc<FunctionIr>>,
        enclosing: FxHashMap<SyntaxId, SyntaxId>,
        decls: FxHashMap<SyntaxId, DeclarationSummaryData>,
        identities: FxHashMap<SyntaxId, DeclarationIdentity>,
        locations: FxHashMap<SyntaxId, DiagnosticLocation>,
        kinds: FxHashMap<SyntaxId, String>,
    }

    impl SyntheticEnv {
        fn new() -> Self {
            Self {
                irs: FxHashMap::default(),
                enclosing: FxHashMap::default(),
                decls: FxHashMap::default(),
                identities: FxHashMap::default(),
                locations: FxHashMap::default(),
                kinds: FxHashMap::default(),
            }
        }
    }

    impl SyntaxCatalog for SyntheticEnv {
        fn syntax_location(&self, syntax: SyntaxId) -> Option<DiagnosticLocation> {
            self.locations.get(&syntax).cloned()
        }
        fn syntax_text(&self, _syntax: SyntaxId) -> Option<String> {
            None
        }
        fn syntax_kind(&self, syntax: SyntaxId) -> Option<String> {
            self.kinds.get(&syntax).cloned()
        }
        fn file_path(&self, _file: FileId) -> Option<&str> {
            Some("test.ts")
        }
    }

    impl SemanticModel for SyntheticEnv {
        fn symbol_at_syntax(&self, syntax: SyntaxId) -> Option<SymbolId> {
            self.decls.get(&syntax).and_then(|d| d.symbol)
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
            symbol: SymbolId,
        ) -> Option<DeclarationSummaryData> {
            self.decls
                .values()
                .find(|d| d.symbol == Some(symbol))
                .cloned()
        }
        fn is_assignable_to(&self, _source: TypeId, _target: TypeId) -> bool {
            false
        }
        fn base_types(&self, _ty: TypeId) -> Vec<TypeId> {
            Vec::new()
        }
        fn is_external_syntax(&self, syntax: SyntaxId) -> bool {
            self.decls.get(&syntax).is_some_and(|d| d.is_external)
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
            decl_or_node_syntax: SyntaxId,
            _call_site_syntax: Option<SyntaxId>,
        ) -> ImplementationFact {
            let decl = self.decls.get(&decl_or_node_syntax);
            if decl.is_some_and(|d| d.is_external) {
                return ImplementationFact::External {
                    package_name: None,
                    declaration_file: None,
                    evidence: Vec::new(),
                };
            }
            ImplementationFact::Resolved(ResolvedImplementationFact {
                file: "test.ts".to_string(),
                location: self
                    .locations
                    .get(&decl_or_node_syntax)
                    .cloned()
                    .unwrap_or_else(|| loc(1)),
                declaration_syntax: decl_or_node_syntax,
                declaration_symbol: decl.and_then(|d| d.symbol),
                fn_syntax: Some(decl_or_node_syntax),
                export_name: decl.and_then(|d| d.name.clone()),
                package_name: None,
                resolution_source: tssae_common::ImplementationSourceKind::CurrentProject,
                declaration_file: None,
            })
        }
        fn check_call_missing_package_gap(&self, _call_syntax: SyntaxId) -> Option<GapItem> {
            None
        }
        fn declaration_identity(&self, syntax: SyntaxId) -> Option<DeclarationIdentity> {
            self.identities.get(&syntax).cloned()
        }
    }

    impl IrProvider for SyntheticEnv {
        fn function_ir(&self, scope_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
            self.irs
                .get(&scope_syntax)
                .cloned()
                .ok_or_else(|| GapItem::new("missing-function-body", String::new(), None))
        }
        fn enclosing_scope_ir(&self, node_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
            let scope = self
                .enclosing
                .get(&node_syntax)
                .copied()
                .unwrap_or(node_syntax);
            self.function_ir(scope)
        }
        fn file_scope_ir(&self, _file: FileId) -> Option<Arc<FunctionIr>> {
            None
        }
        fn scopes_in_file(&self, _file: FileId) -> Vec<ScopeCatalogEntry> {
            self.irs
                .keys()
                .map(|&root| ScopeCatalogEntry {
                    id: FunctionIrId {
                        file: FileId(0),
                        local_scope: root.local_id().0,
                    },
                    local_syntax: root.local_id(),
                    root_syntax: root,
                    kind: IrScopeKind::Function,
                    span_start: 0,
                    span_end: 200,
                })
                .collect()
        }
    }

    fn syn(n: u32) -> SyntaxId {
        SyntaxId::from_parts(FileId(0), LocalSyntaxId(n))
    }

    fn loc(line: u32) -> DiagnosticLocation {
        DiagnosticLocation {
            file: "test.ts".to_string(),
            line,
            column: 1,
            end_line: line,
            end_column: 5,
            start: line * 10,
            end: line * 10 + 4,
        }
    }

    fn mk_vertex(
        id: u32,
        syntax: Option<SyntaxId>,
        op: IrOp,
        value: Option<ValueId>,
        edges: Vec<ExplicitEdge>,
    ) -> CfgVertex {
        CfgVertex {
            id: VertexId(id),
            syntax,
            local_syntax: syntax.map(SyntaxId::local_id),
            symbol: None,
            ty: None,
            signature: None,
            location: syntax.map(|s| loc(s.local_id().0)),
            value,
            op,
            explicit_edges: edges,
            may_throw_edge: None,
            enclosing_finallies: Vec::new(),
        }
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "Verifies proved, disproved, and inconclusive tri-state isolation on one synthetic CFG."
    )]
    fn isolates_unrelated_unknown_calls_from_proved_and_disproved_pairs() {
        let mut env = SyntheticEnv::new();
        let fn_syn = syn(1);
        let src_syn = syn(10);
        let ext_call_syn = syn(11);
        let ext_callee_syn = syn(12);
        let good_decl_syn = syn(13);
        let ext_decl_syn = syn(14);
        let unrelated_syn = syn(15);

        for s in [
            fn_syn,
            src_syn,
            ext_call_syn,
            ext_callee_syn,
            good_decl_syn,
            ext_decl_syn,
            unrelated_syn,
        ] {
            env.enclosing.insert(s, fn_syn);
            env.locations.insert(s, loc(s.local_id().0));
        }
        env.decls.insert(
            ext_callee_syn,
            DeclarationSummaryData {
                symbol: Some(SymbolId(100)),
                primary_syntax: Some(ext_callee_syn),
                name: Some("unknownExt".to_string()),
                container_name: None,
                kind: Some("FunctionDeclaration".to_string()),
                location: Some(loc(12)),
                is_external: true,
                is_default_library: false,
                has_body: false,
                is_signature_only: false,
                initializer_syntax: None,
                callable_syntax: None,
            },
        );
        env.kinds
            .insert(good_decl_syn, "VariableDeclaration".to_string());
        env.kinds
            .insert(ext_decl_syn, "VariableDeclaration".to_string());
        env.kinds
            .insert(unrelated_syn, "VariableDeclaration".to_string());

        let b_good = LocalBindingId(0);
        let b_ext = LocalBindingId(1);
        let b_unrel = LocalBindingId(2);
        let v_src = ValueId(0);
        let v_ext_fn = ValueId(1);
        let v_ext_out = ValueId(2);
        let v_good_decl = ValueId(3);
        let v_ext_decl = ValueId(4);
        let v_unrel_lit = ValueId(5);
        let v_unrel_decl = ValueId(6);

        let mut expr_to_value = FxHashMap::default();
        expr_to_value.insert(src_syn.local_id(), v_src);
        expr_to_value.insert(ext_call_syn.local_id(), v_ext_out);

        let ir = Arc::new(FunctionIr {
            id: FunctionIrId {
                file: FileId(0),
                local_scope: 0,
            },
            root_syntax: fn_syn,
            scope_kind: IrScopeKind::Function,
            scope_location: loc(1),
            decl_semantics: DeclarationSemantics::default(),
            type_guard: None,
            parameters: Vec::new(),
            bindings: vec![
                IrLocalBinding {
                    id: b_good,
                    name: "good".to_string(),
                    mutability: BindingMutability::Const,
                    syntax: good_decl_syn,
                    symbol: None,
                },
                IrLocalBinding {
                    id: b_ext,
                    name: "extOut".to_string(),
                    mutability: BindingMutability::Const,
                    syntax: ext_decl_syn,
                    symbol: None,
                },
                IrLocalBinding {
                    id: b_unrel,
                    name: "unrelated".to_string(),
                    mutability: BindingMutability::Const,
                    syntax: unrelated_syn,
                    symbol: None,
                },
            ],
            captured_bindings: Vec::new(),
            vertices: vec![
                mk_vertex(
                    0,
                    None,
                    IrOp::Entry,
                    None,
                    vec![ExplicitEdge {
                        target: VertexId(4),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    1,
                    None,
                    IrOp::NopMerge,
                    None,
                    vec![ExplicitEdge {
                        target: VertexId(3),
                        kind: ExplicitEdgeKind::ExitMerge,
                    }],
                ),
                mk_vertex(
                    2,
                    None,
                    IrOp::NopMerge,
                    None,
                    vec![ExplicitEdge {
                        target: VertexId(3),
                        kind: ExplicitEdgeKind::ExitMerge,
                    }],
                ),
                mk_vertex(3, None, IrOp::Exit, None, Vec::new()),
                mk_vertex(
                    4,
                    Some(src_syn),
                    IrOp::Literal {
                        kind: crate::ir::LiteralKind::String("src".to_string()),
                        as_const: false,
                    },
                    Some(v_src),
                    vec![ExplicitEdge {
                        target: VertexId(5),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    5,
                    Some(good_decl_syn),
                    IrOp::AssignLocal {
                        target: b_good,
                        op: AssignOp::Assign,
                        value: v_src,
                        is_init: true,
                    },
                    Some(v_good_decl),
                    vec![ExplicitEdge {
                        target: VertexId(6),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    6,
                    Some(ext_callee_syn),
                    IrOp::ReadSymbol {
                        ident_syntax: ext_callee_syn,
                    },
                    Some(v_ext_fn),
                    vec![ExplicitEdge {
                        target: VertexId(7),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    7,
                    Some(ext_call_syn),
                    IrOp::Call {
                        call_kind: IrCallKind::Explicit,
                        callee: v_ext_fn,
                        receiver: None,
                        args: vec![IrCallArg::Positional(v_src)],
                    },
                    Some(v_ext_out),
                    vec![ExplicitEdge {
                        target: VertexId(8),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    8,
                    Some(ext_decl_syn),
                    IrOp::AssignLocal {
                        target: b_ext,
                        op: AssignOp::Assign,
                        value: v_ext_out,
                        is_init: true,
                    },
                    Some(v_ext_decl),
                    vec![ExplicitEdge {
                        target: VertexId(9),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    9,
                    None,
                    IrOp::Literal {
                        kind: crate::ir::LiteralKind::String("clean".to_string()),
                        as_const: false,
                    },
                    Some(v_unrel_lit),
                    vec![ExplicitEdge {
                        target: VertexId(10),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    10,
                    Some(unrelated_syn),
                    IrOp::AssignLocal {
                        target: b_unrel,
                        op: AssignOp::Assign,
                        value: v_unrel_lit,
                        is_init: true,
                    },
                    Some(v_unrel_decl),
                    vec![ExplicitEdge {
                        target: VertexId(1),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
            ],
            entry: VertexId(0),
            normal_exit: VertexId(1),
            exceptional_exit: VertexId(2),
            exit: VertexId(3),
            syntax_to_vertex: FxHashMap::default(),
            expr_to_vertex: FxHashMap::default(),
            expr_to_value,
            value_sources: FxHashMap::default(),
            branches: Vec::new(),
            value_count: 7,
        });
        env.irs.insert(fn_syn, ir);

        let summaries = BehaviorSummaryIndex::default();
        let (outcome, _) = trace_value_propagation(
            &[ValueEndpointSpec {
                syntax: src_syn,
                symbol: None,
            }],
            &[
                ValueEndpointSpec {
                    syntax: good_decl_syn,
                    symbol: None,
                },
                ValueEndpointSpec {
                    syntax: ext_decl_syn,
                    symbol: None,
                },
                ValueEndpointSpec {
                    syntax: unrelated_syn,
                    symbol: None,
                },
            ],
            false,
            &[FileId(0)],
            None::<&HashSet<FileId>>,
            &env,
            &env,
            &env,
            &summaries,
            None,
        );

        assert!(matches!(
            outcome.results[0],
            ValuePropagationTargetOutcome::Proved { .. }
        ));
        assert!(matches!(
            &outcome.results[1],
            ValuePropagationTargetOutcome::Inconclusive { gaps, .. }
                if gaps.iter().any(|g| g.kind == "missing-behavior-summary")
        ));
        assert!(matches!(
            outcome.results[2],
            ValuePropagationTargetOutcome::Disproved { .. }
        ));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "Constructs synthetic CFG vertices inline for array destructuring with rest tracking."
    )]
    fn array_destructure_with_rest_tracks_exact_indices() {
        let mut env = SyntheticEnv::new();
        let fn_syn = syn(1);
        let src_syn = syn(10);
        let safe_syn = syn(11);
        let arr_syn = syn(12);
        let decl_a = syn(13);
        let decl_b = syn(14);
        let decl_tail = syn(15);

        for s in [
            fn_syn, src_syn, safe_syn, arr_syn, decl_a, decl_b, decl_tail,
        ] {
            env.enclosing.insert(s, fn_syn);
            env.locations.insert(s, loc(s.local_id().0));
        }
        env.kinds.insert(decl_a, "Identifier".to_string());
        env.kinds.insert(decl_b, "Identifier".to_string());
        env.kinds.insert(decl_tail, "Identifier".to_string());

        let b_a = LocalBindingId(0);
        let b_b = LocalBindingId(1);
        let b_tail = LocalBindingId(2);
        let v_src = ValueId(0);
        let v_safe = ValueId(1);
        let v_arr = ValueId(2);
        let v_iter = ValueId(3);
        let v_e0 = ValueId(4);
        let v_e1 = ValueId(5);
        let v_rest = ValueId(6);

        let mut expr_to_value = FxHashMap::default();
        expr_to_value.insert(src_syn.local_id(), v_src);

        let ir = Arc::new(FunctionIr {
            id: FunctionIrId {
                file: FileId(0),
                local_scope: 0,
            },
            root_syntax: fn_syn,
            scope_kind: IrScopeKind::Function,
            scope_location: loc(1),
            decl_semantics: DeclarationSemantics::default(),
            type_guard: None,
            parameters: Vec::new(),
            bindings: vec![
                IrLocalBinding {
                    id: b_a,
                    name: "a".to_string(),
                    mutability: BindingMutability::Const,
                    syntax: decl_a,
                    symbol: None,
                },
                IrLocalBinding {
                    id: b_b,
                    name: "b".to_string(),
                    mutability: BindingMutability::Const,
                    syntax: decl_b,
                    symbol: None,
                },
                IrLocalBinding {
                    id: b_tail,
                    name: "tail".to_string(),
                    mutability: BindingMutability::Const,
                    syntax: decl_tail,
                    symbol: None,
                },
            ],
            captured_bindings: Vec::new(),
            vertices: vec![
                mk_vertex(
                    0,
                    None,
                    IrOp::Entry,
                    None,
                    vec![ExplicitEdge {
                        target: VertexId(4),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    1,
                    None,
                    IrOp::NopMerge,
                    None,
                    vec![ExplicitEdge {
                        target: VertexId(3),
                        kind: ExplicitEdgeKind::ExitMerge,
                    }],
                ),
                mk_vertex(
                    2,
                    None,
                    IrOp::NopMerge,
                    None,
                    vec![ExplicitEdge {
                        target: VertexId(3),
                        kind: ExplicitEdgeKind::ExitMerge,
                    }],
                ),
                mk_vertex(3, None, IrOp::Exit, None, Vec::new()),
                mk_vertex(
                    4,
                    Some(safe_syn),
                    IrOp::Literal {
                        kind: crate::ir::LiteralKind::String("safe".to_string()),
                        as_const: false,
                    },
                    Some(v_safe),
                    vec![ExplicitEdge {
                        target: VertexId(5),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    5,
                    Some(src_syn),
                    IrOp::Literal {
                        kind: crate::ir::LiteralKind::String("src".to_string()),
                        as_const: false,
                    },
                    Some(v_src),
                    vec![ExplicitEdge {
                        target: VertexId(6),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                // [safe, src]
                mk_vertex(
                    6,
                    Some(arr_syn),
                    IrOp::ArrayAlloc {
                        elems: vec![IrArrayElem::Value(v_safe), IrArrayElem::Value(v_src)],
                        as_const: false,
                    },
                    Some(v_arr),
                    vec![ExplicitEdge {
                        target: VertexId(7),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                // const [a, b, ...tail] = [safe, src]
                mk_vertex(
                    7,
                    None,
                    IrOp::IteratorInit {
                        iterable: v_arr,
                        is_async: false,
                    },
                    Some(v_iter),
                    vec![ExplicitEdge {
                        target: VertexId(8),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    8,
                    None,
                    IrOp::IteratorNext {
                        iterator: v_iter,
                        is_async: false,
                    },
                    Some(v_e0),
                    vec![ExplicitEdge {
                        target: VertexId(9),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    9,
                    Some(decl_a),
                    IrOp::AssignLocal {
                        target: b_a,
                        op: AssignOp::Assign,
                        value: v_e0,
                        is_init: true,
                    },
                    Some(v_e0),
                    vec![ExplicitEdge {
                        target: VertexId(10),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    10,
                    None,
                    IrOp::IteratorNext {
                        iterator: v_iter,
                        is_async: false,
                    },
                    Some(v_e1),
                    vec![ExplicitEdge {
                        target: VertexId(11),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    11,
                    Some(decl_b),
                    IrOp::AssignLocal {
                        target: b_b,
                        op: AssignOp::Assign,
                        value: v_e1,
                        is_init: true,
                    },
                    Some(v_e1),
                    vec![ExplicitEdge {
                        target: VertexId(12),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    12,
                    None,
                    IrOp::IteratorRest {
                        iterator: v_iter,
                        is_async: false,
                    },
                    Some(v_rest),
                    vec![ExplicitEdge {
                        target: VertexId(13),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    13,
                    Some(decl_tail),
                    IrOp::AssignLocal {
                        target: b_tail,
                        op: AssignOp::Assign,
                        value: v_rest,
                        is_init: true,
                    },
                    Some(v_rest),
                    vec![ExplicitEdge {
                        target: VertexId(1),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
            ],
            entry: VertexId(0),
            normal_exit: VertexId(1),
            exceptional_exit: VertexId(2),
            exit: VertexId(3),
            syntax_to_vertex: FxHashMap::default(),
            expr_to_vertex: FxHashMap::default(),
            expr_to_value,
            value_sources: FxHashMap::default(),
            branches: Vec::new(),
            value_count: 7,
        });
        env.irs.insert(fn_syn, ir);

        let summaries = BehaviorSummaryIndex::default();
        let (outcome, _) = trace_value_propagation(
            &[ValueEndpointSpec {
                syntax: src_syn,
                symbol: None,
            }],
            &[
                ValueEndpointSpec {
                    syntax: decl_a,
                    symbol: None,
                },
                ValueEndpointSpec {
                    syntax: decl_b,
                    symbol: None,
                },
                ValueEndpointSpec {
                    syntax: decl_tail,
                    symbol: None,
                },
            ],
            false,
            &[FileId(0)],
            None::<&HashSet<FileId>>,
            &env,
            &env,
            &env,
            &summaries,
            None,
        );

        assert!(matches!(
            outcome.results[0],
            ValuePropagationTargetOutcome::Disproved { .. }
        ));
        assert!(matches!(
            outcome.results[1],
            ValuePropagationTargetOutcome::Proved { .. }
        ));
        assert!(matches!(
            outcome.results[2],
            ValuePropagationTargetOutcome::Disproved { .. }
        ));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "Constructs > MAX_PROP_STATES synthetic vertices to test state budget exhaustion."
    )]
    fn state_budget_exhaustion_reports_inconclusive_never_silent_disproved() {
        use crate::ir::LiteralKind;
        let mut env = SyntheticEnv::new();
        let fn_syn = syn(1);
        let src_syn = syn(10);
        let final_syn = syn(11);
        for s in [fn_syn, src_syn, final_syn] {
            env.locations.insert(s, loc(s.local_id().0));
            env.enclosing.insert(s, fn_syn);
        }

        // Build > MAX_PROP_STATES shallow local copies from `v0` so exploration hits the state budget
        // without quadratic step-vector cloning.
        let count = MAX_PROP_STATES + 100;
        let mut bindings = Vec::with_capacity(count + 1);
        let mut vertices = vec![
            mk_vertex(
                0,
                None,
                IrOp::Entry,
                None,
                vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                1,
                None,
                IrOp::NopMerge,
                None,
                vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(2, None, IrOp::NopMerge, None, vec![]),
            mk_vertex(3, None, IrOp::Exit, None, vec![]),
        ];

        bindings.push(IrLocalBinding {
            id: LocalBindingId(0),
            syntax: src_syn,
            name: "v0".to_string(),
            mutability: BindingMutability::Const,
            symbol: None,
        });
        vertices.push(mk_vertex(
            4,
            Some(src_syn),
            IrOp::Literal {
                kind: LiteralKind::String("seed".to_string()),
                as_const: false,
            },
            Some(ValueId(0)),
            vec![ExplicitEdge {
                target: VertexId(5),
                kind: ExplicitEdgeKind::Fallthrough,
            }],
        ));

        let mut next_vid = 5u32;
        let mut next_val = 1u32;
        for i in 1..=count {
            let decl_s = syn(100 + u32::try_from(i).unwrap());
            env.locations.insert(decl_s, loc(decl_s.local_id().0));
            env.enclosing.insert(decl_s, fn_syn);
            let b_id = LocalBindingId(u32::try_from(i).unwrap());
            bindings.push(IrLocalBinding {
                id: b_id,
                syntax: decl_s,
                name: format!("v{i}"),
                mutability: BindingMutability::Const,
                symbol: None,
            });
            let assign_vid = next_vid;
            let read_vid = next_vid + 1;
            let out_val = ValueId(next_val);
            next_vid += 2;
            next_val += 1;

            vertices.push(mk_vertex(
                assign_vid,
                Some(decl_s),
                IrOp::AssignLocal {
                    target: b_id,
                    op: AssignOp::Assign,
                    value: ValueId(0),
                    is_init: true,
                },
                Some(ValueId(0)),
                vec![ExplicitEdge {
                    target: VertexId(read_vid),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ));
            let next_target = if i == count {
                VertexId(1)
            } else {
                VertexId(next_vid)
            };
            vertices.push(mk_vertex(
                read_vid,
                if i == count { Some(final_syn) } else { None },
                IrOp::ReadLocal { binding: b_id },
                Some(out_val),
                vec![ExplicitEdge {
                    target: next_target,
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ));
        }

        let mut expr_to_value = FxHashMap::default();
        expr_to_value.insert(src_syn.local_id(), ValueId(0));
        expr_to_value.insert(final_syn.local_id(), ValueId(next_val - 1));

        let ir = Arc::new(FunctionIr {
            id: FunctionIrId {
                file: FileId(0),
                local_scope: 1,
            },
            root_syntax: fn_syn,
            scope_kind: IrScopeKind::Function,
            scope_location: loc(1),
            decl_semantics: DeclarationSemantics::default(),
            type_guard: None,
            parameters: Vec::new(),
            bindings,
            captured_bindings: Vec::new(),
            vertices,
            entry: VertexId(0),
            normal_exit: VertexId(1),
            exceptional_exit: VertexId(2),
            exit: VertexId(3),
            syntax_to_vertex: FxHashMap::default(),
            expr_to_vertex: FxHashMap::default(),
            expr_to_value,
            value_sources: FxHashMap::default(),
            branches: Vec::new(),
            value_count: next_val,
        });
        env.irs.insert(fn_syn, ir);

        let summaries = BehaviorSummaryIndex::default();
        let (outcome, _) = trace_value_propagation(
            &[ValueEndpointSpec {
                syntax: src_syn,
                symbol: None,
            }],
            &[ValueEndpointSpec {
                syntax: final_syn,
                symbol: None,
            }],
            false,
            &[FileId(0)],
            None::<&HashSet<FileId>>,
            &env,
            &env,
            &env,
            &summaries,
            None,
        );

        match &outcome.results[0] {
            ValuePropagationTargetOutcome::Inconclusive { gaps, .. } => {
                assert!(
                    gaps.iter().any(|g| g.kind == "unmodeled-construct"
                        && g.message.contains("state exploration budget")),
                    "expected budget gap, got {gaps:?}"
                );
            }
            other => panic!("expected Inconclusive on budget exhaustion, got {other:?}"),
        }
    }
}
