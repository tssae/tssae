use std::{
    collections::{BTreeSet, HashSet},
    hash::BuildHasher,
    sync::Arc,
};

use rustc_hash::{FxHashMap, FxHashSet};
use tssae_common::{
    CallAspectSummary, DeclarationIdentity, DeclarationSummaryData, DiagnosticLocation,
    EvidenceItem, FileId, ForwardCallMode, ForwardCallTarget, FunctionIrId, GapItem,
    ImplementationFact, ResolvedImplementationFact, SemanticModel, SummaryCompleteness,
    SummaryConflictError, SummaryNamedValueRef, SummarySource, SummaryTaggedValueRef,
    SummaryValueRef, SymbolId, SyntaxCatalog, SyntaxId, UnknownReason, ValueAspectSummary, ValueId,
};

use crate::{
    class_overrides::{self, MemberAccessKind},
    ir::{
        BindingMutability, CfgVertex, FunctionIr, ImplicitAccessorKind, IrArrayElem, IrCallArg,
        IrCallKind, IrObjectProp, IrOp, IrPropKey, IrProvider, IrScopeKind, LiteralKind,
    },
    summaries::{BehaviorSummaryIndex, MatchedSummaryAspect},
    var_writes::{self, VarWriteSource},
};

const MAX_TRACE_DEPTH: usize = 32;
const MAX_CALL_STACK_DEPTH: usize = 12;

/// Mapping from a call-site argument expression to a callee parameter declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallParameterBindingData {
    pub argument_index: usize,
    pub argument_syntax: SyntaxId,
    pub parameter_name: Option<String>,
    pub parameter_decl_syntax: Option<SyntaxId>,
    pub is_rest: bool,
}

/// One resolved callee target descriptor returned by [`resolve_call_targets_on_ir`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCallTargetData {
    pub declaration_syntax: SyntaxId,
    pub symbol: Option<SymbolId>,
    pub name: Option<String>,
    pub location: Option<DiagnosticLocation>,
    pub is_external: bool,
    pub is_default_library: bool,
    pub fn_scope_syntax: Option<SyntaxId>,
    pub implementation: Option<ResolvedImplementationFact>,
    pub parameter_bindings: Vec<CallParameterBindingData>,
    pub return_expressions: Vec<SyntaxId>,
}

/// Completeness-aware result of resolving call targets at a single call vertex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallTargetsOutcome {
    pub targets: Vec<ResolvedCallTargetData>,
    pub gaps: Vec<GapItem>,
    pub is_complete: bool,
    pub evidence: Vec<EvidenceItem>,
    pub conflict: Option<SummaryConflictError>,
}

/// One caller site matched by [`find_callers_on_ir`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerSiteData {
    pub call_syntax: SyntaxId,
    pub location: DiagnosticLocation,
    pub caller_decl_syntax: Option<SyntaxId>,
    pub caller_symbol: Option<SymbolId>,
    pub parameter_bindings: Vec<CallParameterBindingData>,
}

/// Completeness-aware result of querying callers of a declaration via [`find_callers_on_ir`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallersOutcome {
    pub call_sites: Vec<CallerSiteData>,
    pub gaps: Vec<GapItem>,
    pub is_complete: bool,
    pub evidence: Vec<EvidenceItem>,
}

/// Indexed call site stored in [`IrCallGraph`] for fast project-wide reverse caller queries.
#[derive(Clone, Debug)]
pub struct IndexedCallEntry {
    pub call_syntax: SyntaxId,
    pub file: FileId,
    pub location: DiagnosticLocation,
    pub caller_decl_syntax: Option<SyntaxId>,
    pub caller_symbol: Option<SymbolId>,
    pub resolution: CallTargetsOutcome,
    /// When `Some`, this call site is an unresolved/open member access of `member_name`
    /// that could dispatch to matching methods/getters/setters.
    pub open_member_name: Option<String>,
    /// When `true`, this call site has an unresolved non-member callee (e.g. callback parameter,
    /// computed property, or dynamic expression) that could potentially invoke any callable.
    pub is_untyped_dynamic_incomplete: bool,
}

/// Snapshot-scoped IR call graph over all project source files.
#[derive(Default, Debug)]
pub struct IrCallGraph {
    pub sites: Vec<IndexedCallEntry>,
    pub by_decl: FxHashMap<SyntaxId, Vec<usize>>,
    pub by_symbol: FxHashMap<SymbolId, Vec<usize>>,
    pub incomplete_sites: Vec<usize>,
    pub escaped_decls: FxHashSet<SyntaxId>,
    pub escaped_symbols: FxHashSet<SymbolId>,
    pub conflict: Option<SummaryConflictError>,
}

/// Static frame mapping callee parameters back to caller arguments during inter-procedural tracing.
#[derive(Clone)]
struct CallFrame {
    caller_ir: Arc<FunctionIr>,
    args: Vec<IrCallArg>,
}

/// Active call context threaded while resolving a direct call or constructor expression.
#[derive(Clone, Copy)]
struct ActiveCallContext<'a> {
    caller_ir: &'a Arc<FunctionIr>,
    receiver: Option<ValueId>,
    args: &'a [IrCallArg],
    call_stack: &'a [CallFrame],
    is_construct: bool,
}

/// Internal intermediate callable descriptor prior to parameter-binding & return enrichment.
#[derive(Clone, Debug)]
struct RawTargetDescriptor {
    decl_syntax: SyntaxId,
    fn_scope_syntax: Option<SyntaxId>,
    symbol: Option<SymbolId>,
    name: Option<String>,
    location: Option<DiagnosticLocation>,
    is_external: bool,
    is_default_library: bool,
    implementation: Option<ResolvedImplementationFact>,
    args_offset: usize,
}

/// Mutable state threaded through IR callee value tracing.
struct TraceCollector<'a> {
    call_syntax: SyntaxId,
    call_location: Option<DiagnosticLocation>,
    semantic: &'a dyn SemanticModel,
    syntax: &'a dyn SyntaxCatalog,
    ir_provider: &'a dyn IrProvider,
    summary_index: &'a BehaviorSummaryIndex,
    targets: Vec<RawTargetDescriptor>,
    seen_targets: FxHashSet<SyntaxId>,
    gaps: Vec<GapItem>,
    seen_gaps: FxHashSet<(&'static str, String)>,
    evidence: Vec<EvidenceItem>,
    is_complete: bool,
    is_untyped_dynamic_incomplete: bool,
    open_member_name: Option<String>,
    visited: FxHashSet<(FunctionIrId, ValueId, usize)>,
    conflict: Option<SummaryConflictError>,
}

impl<'a> TraceCollector<'a> {
    fn new(
        call_syntax: SyntaxId,
        call_location: Option<DiagnosticLocation>,
        semantic: &'a dyn SemanticModel,
        syntax: &'a dyn SyntaxCatalog,
        ir_provider: &'a dyn IrProvider,
        summary_index: &'a BehaviorSummaryIndex,
    ) -> Self {
        Self {
            call_syntax,
            call_location,
            semantic,
            syntax,
            ir_provider,
            summary_index,
            targets: Vec::new(),
            seen_targets: FxHashSet::default(),
            gaps: Vec::new(),
            seen_gaps: FxHashSet::default(),
            evidence: Vec::new(),
            is_complete: true,
            is_untyped_dynamic_incomplete: false,
            open_member_name: None,
            visited: FxHashSet::default(),
            conflict: None,
        }
    }

    fn push_gap(&mut self, gap: GapItem) {
        self.is_complete = false;
        let key = (gap.kind, gap.message.clone());
        if self.seen_gaps.insert(key) {
            self.gaps.push(gap);
        }
    }

    fn push_dynamic_gap(&mut self, message: impl Into<String>) {
        self.is_untyped_dynamic_incomplete = true;
        self.push_gap(GapItem::from_reason(
            UnknownReason::DynamicCallee,
            message.into(),
            self.call_location.clone(),
        ));
    }

    fn push_target(&mut self, target: RawTargetDescriptor) {
        if let Some(loc) = &target.location {
            let decl_name = target.name.clone().unwrap_or_else(|| "target".to_string());
            self.evidence.push(EvidenceItem::simple(
                "source",
                format!("Resolved call target '{decl_name}'."),
                Some(loc.clone()),
            ));
            if let Some(last) = self.evidence.last_mut() {
                last.declaration.clone_from(&target.name);
            }
        }
        if self.seen_targets.insert(target.decl_syntax) {
            self.targets.push(target);
        }
    }

    fn push_summary_evidence(
        &mut self,
        source: SummarySource,
        pack_id: &str,
        target_label: &str,
        at: Option<DiagnosticLocation>,
    ) {
        self.evidence.push(EvidenceItem::summary(
            format!(
                "Applied {} behavior summary '{}' for '{}'.",
                source.as_str(),
                pack_id,
                target_label
            ),
            at,
            source.as_str().to_string(),
            pack_id.to_string(),
            Some(target_label.to_string()),
        ));
    }
}

/// Resolves all reachable callee declarations and completeness metadata for `call_syntax`.
#[must_use]
pub fn resolve_call_targets_on_ir(
    call_syntax: SyntaxId,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
) -> CallTargetsOutcome {
    let default_index = BehaviorSummaryIndex::default();
    resolve_call_targets_with_summaries(call_syntax, semantic, syntax, ir_provider, &default_index)
}

/// Resolves all reachable callee declarations using `summary_index` for external and summarized calls.
#[must_use]
pub fn resolve_call_targets_with_summaries(
    call_syntax: SyntaxId,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    summary_index: &BehaviorSummaryIndex,
) -> CallTargetsOutcome {
    let analyzed = analyze_call_vertex(call_syntax, semantic, syntax, ir_provider, summary_index);
    analyzed.outcome
}

/// Resolves all reachable callee declarations for an already-lowered `(caller_ir, call_vertex)` pair.
#[must_use]
pub fn resolve_call_targets_in_scope(
    call_syntax: SyntaxId,
    caller_ir: &Arc<FunctionIr>,
    call_vertex: &CfgVertex,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    summary_index: &BehaviorSummaryIndex,
) -> CallTargetsOutcome {
    analyze_call_vertex_with_scope(
        call_syntax,
        Some((caller_ir, call_vertex)),
        semantic,
        syntax,
        ir_provider,
        summary_index,
    )
    .outcome
}

struct AnalyzedCallSite {
    outcome: CallTargetsOutcome,
    caller_decl_syntax: Option<SyntaxId>,
    caller_symbol: Option<SymbolId>,
    open_member_name: Option<String>,
    is_untyped_dynamic_incomplete: bool,
}

fn analyze_call_vertex(
    call_syntax: SyntaxId,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    summary_index: &BehaviorSummaryIndex,
) -> AnalyzedCallSite {
    analyze_call_vertex_with_scope(
        call_syntax,
        None,
        semantic,
        syntax,
        ir_provider,
        summary_index,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "Dispatches across Call/Construct IR variants and constructs CallTargetsOutcome."
)]
fn analyze_call_vertex_with_scope(
    call_syntax: SyntaxId,
    known_scope: Option<(&Arc<FunctionIr>, &CfgVertex)>,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    summary_index: &BehaviorSummaryIndex,
) -> AnalyzedCallSite {
    let call_loc = known_scope
        .and_then(|(_, v)| v.location.clone())
        .or_else(|| syntax.syntax_location(call_syntax));
    let mut collector = TraceCollector::new(
        call_syntax,
        call_loc.clone(),
        semantic,
        syntax,
        ir_provider,
        summary_index,
    );

    if let Some(missing_pkg_gap) = semantic.check_call_missing_package_gap(call_syntax) {
        collector.push_gap(missing_pkg_gap);
    }

    let owned_ir;
    let ir: &Arc<FunctionIr> = if let Some((scope_ir, _)) = known_scope {
        scope_ir
    } else {
        let Ok(resolved_ir) = ir_provider.enclosing_scope_ir(call_syntax) else {
            collector.push_gap(GapItem::from_reason(
                UnknownReason::UnresolvedDeclaration,
                "Enclosing function or module body could not be analyzed for this call site."
                    .to_string(),
                call_loc,
            ));
            return AnalyzedCallSite {
                outcome: CallTargetsOutcome {
                    targets: Vec::new(),
                    gaps: collector.gaps,
                    is_complete: false,
                    evidence: collector.evidence,
                    conflict: None,
                },
                caller_decl_syntax: None,
                caller_symbol: None,
                open_member_name: None,
                is_untyped_dynamic_incomplete: true,
            };
        };
        owned_ir = resolved_ir;
        &owned_ir
    };

    let caller_decl_syntax =
        (ir.scope_kind != crate::ir::IrScopeKind::ModuleTopLevel).then_some(ir.root_syntax);
    let caller_symbol = caller_decl_syntax.and_then(|syn| semantic.symbol_at_syntax(syn));

    let call_vertex = if let Some((_, v)) = known_scope {
        v
    } else if let Some(v) = find_call_vertex(ir, call_syntax) {
        v
    } else {
        collector
            .push_dynamic_gap("Call expression could not be resolved as an analyzable call site.");
        return AnalyzedCallSite {
            outcome: CallTargetsOutcome {
                targets: Vec::new(),
                gaps: collector.gaps,
                is_complete: false,
                evidence: collector.evidence,
                conflict: None,
            },
            caller_decl_syntax,
            caller_symbol,
            open_member_name: None,
            is_untyped_dynamic_incomplete: true,
        };
    };

    let mut arg_syntaxes: Vec<(SyntaxId, bool)> = Vec::new();
    match &call_vertex.op {
        IrOp::Call {
            call_kind,
            callee,
            receiver,
            args,
        } => {
            arg_syntaxes = collect_arg_syntaxes(ir, args, syntax);
            let call_ctx = ActiveCallContext {
                caller_ir: ir,
                receiver: *receiver,
                args,
                call_stack: &[],
                is_construct: false,
            };
            match call_kind {
                IrCallKind::Explicit | IrCallKind::TaggedTemplate | IrCallKind::DecoratorApply => {
                    trace_callable_value(ir, *callee, &[], 0, Some(call_ctx), 0, &mut collector);
                }
                IrCallKind::SuperCall => {
                    resolve_super_constructor_call(call_syntax, &mut collector);
                }
                IrCallKind::ImplicitGetter => {
                    resolve_implicit_getter_call(ir, *callee, *receiver, &mut collector);
                }
                IrCallKind::ImplicitSetter => {
                    resolve_implicit_setter_call(ir, call_vertex, *receiver, &mut collector);
                }
                IrCallKind::ImplicitDispose { is_async } => {
                    let member_name = if *is_async {
                        "[Symbol.asyncDispose]"
                    } else {
                        "[Symbol.dispose]"
                    };
                    let recv_val = receiver.unwrap_or(*callee);
                    resolve_member_call(
                        ir,
                        recv_val,
                        member_name,
                        false,
                        MemberAccessKind::MethodOrProp,
                        &[],
                        Some(call_ctx),
                        0,
                        &mut collector,
                    );
                }
                IrCallKind::ImplicitIteratorMethod => {
                    collector.push_dynamic_gap(
                        "Implicit iterator method calls do not have statically resolved targets.",
                    );
                }
            }
        }
        IrOp::Construct { callee, args, .. } => {
            arg_syntaxes = collect_arg_syntaxes(ir, args, syntax);
            let call_ctx = ActiveCallContext {
                caller_ir: ir,
                receiver: None,
                args,
                call_stack: &[],
                is_construct: true,
            };
            trace_construct_value(ir, *callee, &[], 0, Some(call_ctx), &mut collector);
        }
        _ => {}
    }

    if let Some(loc) = &call_loc {
        collector.evidence.push(EvidenceItem::simple(
            "source",
            "Analyzed call site.".to_string(),
            Some(loc.clone()),
        ));
    }

    if collector.targets.is_empty() && collector.is_complete && collector.conflict.is_none() {
        collector.push_dynamic_gap("Call target could not be statically determined.");
    }

    let resolved_targets = collector
        .targets
        .iter()
        .map(|raw| enrich_target_descriptor(raw, &arg_syntaxes, semantic, ir_provider))
        .collect();

    AnalyzedCallSite {
        outcome: CallTargetsOutcome {
            targets: resolved_targets,
            gaps: collector.gaps,
            is_complete: collector.is_complete,
            evidence: collector.evidence,
            conflict: collector.conflict,
        },
        caller_decl_syntax,
        caller_symbol,
        open_member_name: collector.open_member_name,
        is_untyped_dynamic_incomplete: collector.is_untyped_dynamic_incomplete,
    }
}

fn find_call_vertex(ir: &FunctionIr, call_syntax: SyntaxId) -> Option<&CfgVertex> {
    if let Some(direct) = ir.vertices.iter().find(|v| {
        v.syntax == Some(call_syntax) && matches!(v.op, IrOp::Call { .. } | IrOp::Construct { .. })
    }) {
        return Some(direct);
    }

    // Fallback: if caller selected the LHS PropertyAccessExpression of an implicit setter write,
    // follow its outgoing edge to the associated `ImplicitSetter` vertex.
    if let Some(vid) = ir.syntax_to_vertex.get(&call_syntax).copied()
        && let Some(vert) = ir.vertices.get(vid.0 as usize)
    {
        if matches!(vert.op, IrOp::Call { .. } | IrOp::Construct { .. }) {
            return Some(vert);
        }
        if matches!(
            vert.op,
            IrOp::PropWrite {
                implicit_call_kind: Some(ImplicitAccessorKind::Setter),
                ..
            }
        ) {
            for edge in &vert.explicit_edges {
                if let Some(next_v) = ir.vertices.get(edge.target.0 as usize)
                    && matches!(
                        next_v.op,
                        IrOp::Call {
                            call_kind: IrCallKind::ImplicitSetter,
                            ..
                        }
                    )
                {
                    return Some(next_v);
                }
            }
        }
    }
    None
}

fn collect_arg_syntaxes(
    ir: &FunctionIr,
    args: &[IrCallArg],
    syntax: &dyn SyntaxCatalog,
) -> Vec<(SyntaxId, bool)> {
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        let (val, is_spread) = match arg {
            IrCallArg::Positional(v) => (*v, false),
            IrCallArg::Spread(v) => (*v, true),
        };
        if let Some(syn) = value_syntax_in_ir(ir, val, syntax) {
            out.push((syn, is_spread));
        }
    }
    out
}

pub(crate) fn value_syntax_in_ir(
    ir: &FunctionIr,
    val: ValueId,
    syntax: &dyn SyntaxCatalog,
) -> Option<SyntaxId> {
    if let Some(v) = ir.vertices.iter().rfind(|v| {
        v.value == Some(val)
            && v.syntax.is_some()
            && !matches!(
                v.op,
                IrOp::AssignLocal { is_init: true, .. } | IrOp::AssignSymbol { is_init: true, .. }
            )
    }) {
        let _ = syntax;
        return v.syntax;
    }
    if let Some((&lsid, _)) = ir.expr_to_value.iter().find(|&(_, &v)| v == val) {
        return Some(SyntaxId::from_parts(ir.id.file, lsid));
    }
    if let Some(sources) = ir.value_sources.get(&val)
        && let Some(&first) = sources.first()
    {
        return value_syntax_in_ir(ir, first, syntax);
    }
    None
}

fn resolve_super_constructor_call(call_syntax: SyntaxId, collector: &mut TraceCollector<'_>) {
    let Some(decl) = collector.semantic.resolve_super_target(call_syntax) else {
        collector.push_dynamic_gap("Could not resolve `super()` target constructor.");
        return;
    };
    emit_resolved_declaration(&decl, Some(call_syntax), false, None, 0, collector);
}

fn resolve_implicit_getter_call(
    ir: &Arc<FunctionIr>,
    callee_val: ValueId,
    receiver_val: Option<ValueId>,
    collector: &mut TraceCollector<'_>,
) {
    let Some(read_v) = ir
        .vertices
        .iter()
        .find(|v| v.value == Some(callee_val) && matches!(v.op, IrOp::PropRead { .. }))
    else {
        collector.push_dynamic_gap("Could not locate property read for implicit getter call.");
        return;
    };
    if let IrOp::PropRead {
        object,
        key,
        is_super,
        ..
    } = &read_v.op
    {
        let recv = receiver_val.unwrap_or(*object);
        let Some(member_name) = prop_key_static_name(key) else {
            collector.push_dynamic_gap(
                "Dynamic computed property getter cannot be statically resolved.",
            );
            return;
        };
        if *is_super
            && let Some(decl) = collector
                .semantic
                .resolve_declaration(collector.call_syntax)
        {
            emit_resolved_declaration(
                &decl,
                Some(collector.call_syntax),
                false,
                None,
                0,
                collector,
            );
            return;
        }
        let obj_ctx = ObjectLookupContext::from_collector(collector);
        if let Some(local_prop) =
            resolve_local_object_property(ir, recv, member_name, &[], 0, Some(&obj_ctx))
            && !local_prop.values.is_empty()
        {
            for (prop_ir, prop_val, _) in local_prop.values {
                trace_callable_value(&prop_ir, prop_val, &[], 1, None, 0, collector);
            }
            if local_prop.is_exhaustive && !collector.targets.is_empty() {
                return;
            }
        }
        resolve_member_call(
            ir,
            recv,
            member_name,
            *is_super,
            MemberAccessKind::Getter,
            &[],
            None,
            0,
            collector,
        );
        if collector.targets.is_empty()
            && let Some(decl) = collector
                .semantic
                .resolve_declaration(collector.call_syntax)
        {
            let before_complete = collector.is_complete;
            collector.gaps.clear();
            collector.seen_gaps.clear();
            collector.is_complete = true;
            emit_resolved_declaration(
                &decl,
                Some(collector.call_syntax),
                false,
                None,
                0,
                collector,
            );
            if collector.targets.is_empty() {
                collector.is_complete = before_complete;
            }
        }
    }
}

fn resolve_implicit_setter_call(
    ir: &Arc<FunctionIr>,
    call_vertex: &CfgVertex,
    receiver_val: Option<ValueId>,
    collector: &mut TraceCollector<'_>,
) {
    let write_v = ir.vertices.iter().find(|v| {
        matches!(
            v.op,
            IrOp::PropWrite {
                implicit_call_kind: Some(ImplicitAccessorKind::Setter),
                ..
            }
        ) && (v.syntax == call_vertex.syntax
            || v.explicit_edges.iter().any(|e| e.target == call_vertex.id))
    });
    if let Some(wv) = write_v
        && let IrOp::PropWrite {
            object,
            key,
            is_super,
            ..
        } = &wv.op
    {
        let recv = receiver_val.unwrap_or(*object);
        let Some(member_name) = prop_key_static_name(key) else {
            collector.push_dynamic_gap(
                "Dynamic computed property setter cannot be statically resolved.",
            );
            return;
        };
        resolve_member_call(
            ir,
            recv,
            member_name,
            *is_super,
            MemberAccessKind::Setter,
            &[],
            None,
            0,
            collector,
        );
        return;
    }

    if let Some(decl) = collector
        .semantic
        .resolve_declaration(collector.call_syntax)
    {
        emit_resolved_declaration(
            &decl,
            Some(collector.call_syntax),
            false,
            None,
            0,
            collector,
        );
    } else {
        collector.push_dynamic_gap("Could not locate setter target declaration.");
    }
}

fn value_producers(ir: &FunctionIr, val: ValueId) -> Vec<&CfgVertex> {
    ir.vertices
        .iter()
        .filter(|v| {
            v.value == Some(val)
                && !matches!(
                    v.op,
                    IrOp::AssignLocal { is_init: true, .. }
                        | IrOp::AssignSymbol { is_init: true, .. }
                )
        })
        .collect()
}

fn emit_function_literal_target(
    fn_syn: SyntaxId,
    vertex_syntax: Option<SyntaxId>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    let sym = collector.semantic.symbol_at_syntax(fn_syn);
    let decl_info = collector.semantic.resolve_declaration(fn_syn);
    let name = decl_info
        .as_ref()
        .and_then(|d| d.name.clone())
        .or_else(|| vertex_syntax.and_then(|s| extract_fn_name_heuristic(collector.syntax, s)));
    let location = decl_info
        .as_ref()
        .and_then(|d| d.location.clone())
        .or_else(|| collector.syntax.syntax_location(fn_syn));
    let is_external = collector.semantic.is_external_syntax(fn_syn);
    collector.push_target(RawTargetDescriptor {
        decl_syntax: fn_syn,
        fn_scope_syntax: Some(fn_syn),
        symbol: sym,
        name,
        location,
        is_external,
        is_default_library: decl_info.as_ref().is_some_and(|d| d.is_default_library),
        implementation: None,
        args_offset,
    });
}

fn trace_callable_value(
    ir: &Arc<FunctionIr>,
    val: ValueId,
    call_stack: &[CallFrame],
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    if collector.conflict.is_some() {
        return;
    }
    if depth >= MAX_TRACE_DEPTH {
        collector.push_dynamic_gap("Call target tracing exceeded maximum recursion depth.");
        return;
    }
    if !collector
        .visited
        .insert((ir.id, val, stack_fingerprint(call_stack)))
    {
        return;
    }

    if let Some(sources) = ir.value_sources.get(&val)
        && !sources.is_empty()
    {
        for &src in sources {
            trace_callable_value(
                ir,
                src,
                call_stack,
                depth + 1,
                call_ctx,
                args_offset,
                collector,
            );
        }
        return;
    }

    let producers = value_producers(ir, val);
    if producers.is_empty() {
        collector.push_dynamic_gap("Callable expression has no local value definition.");
        return;
    }

    for v in producers {
        if collector.conflict.is_some() {
            return;
        }
        trace_callable_vertex_op(ir, v, call_stack, depth, call_ctx, args_offset, collector);
    }
}

fn trace_callable_vertex_op(
    ir: &Arc<FunctionIr>,
    v: &CfgVertex,
    call_stack: &[CallFrame],
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    match &v.op {
        IrOp::FunctionLiteral { syntax: fn_syn, .. } => {
            emit_function_literal_target(*fn_syn, v.syntax, args_offset, collector);
        }
        IrOp::ValueForward { source, .. }
        | IrOp::AssignLocal { value: source, .. }
        | IrOp::AssignSymbol { value: source, .. } => {
            trace_callable_value(
                ir,
                *source,
                call_stack,
                depth + 1,
                call_ctx,
                args_offset,
                collector,
            );
        }
        IrOp::Literal {
            kind: LiteralKind::Null | LiteralKind::Undefined,
            ..
        } => {}
        IrOp::ReadLocal { binding } => {
            trace_read_local_callee(
                ir,
                v,
                *binding,
                call_stack,
                depth,
                call_ctx,
                args_offset,
                collector,
            );
        }
        IrOp::ReadSymbol { ident_syntax } => {
            resolve_symbol_callee(*ident_syntax, depth, call_ctx, args_offset, collector);
        }
        IrOp::PropRead {
            object,
            key,
            is_super,
            ..
        } => {
            resolve_prop_read_callable(
                ir,
                v,
                *object,
                key,
                *is_super,
                call_stack,
                depth,
                call_ctx,
                args_offset,
                collector,
            );
        }
        IrOp::IteratorNext { iterator, .. } => {
            trace_iterator_element_callable(ir, v, *iterator, call_stack, depth, collector);
        }
        IrOp::Call {
            callee: inner_callee,
            receiver: inner_receiver,
            args: inner_args,
            ..
        } => {
            trace_call_return_values(
                ir,
                v.syntax,
                *inner_callee,
                *inner_receiver,
                inner_args,
                call_stack,
                depth,
                collector,
                false,
            );
        }
        IrOp::Await { promise } => {
            trace_callable_value(
                ir,
                *promise,
                call_stack,
                depth + 1,
                call_ctx,
                args_offset,
                collector,
            );
        }
        other => collector.push_dynamic_gap(non_callable_op_gap(other)),
    }
}

const fn non_callable_op_gap(op: &IrOp) -> &'static str {
    match op {
        IrOp::Literal { .. } => "Cannot statically call a primitive literal value.",
        IrOp::ClassLiteral { .. } => {
            "Class constructors require `new` and cannot be invoked directly."
        }
        IrOp::IteratorRest { .. } | IrOp::ObjectRest { .. } => {
            "Rest destructuring target cannot yield a callable function."
        }
        IrOp::ThisRef | IrOp::SuperRef | IrOp::MetaProp => {
            "Dynamic `this`/`super`/meta property callee cannot be resolved statically."
        }
        _ => "Callee expression could not be resolved to static declarations.",
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Traces local variable writes, CallFrame parameters, and callback parameter summaries."
)]
fn trace_read_local_callee(
    ir: &Arc<FunctionIr>,
    read_vertex: &CfgVertex,
    binding: tssae_common::LocalBindingId,
    call_stack: &[CallFrame],
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    let mut handled_by_frame = false;
    if let Some((frame, parent_stack)) = call_stack.split_last()
        && let Some(param_idx) = ir
            .parameters
            .iter()
            .position(|p| p.binding == Some(binding))
        && let Some(arg) = frame.args.get(param_idx)
    {
        handled_by_frame = true;
        match arg {
            IrCallArg::Positional(arg_val) => {
                trace_callable_value(
                    &frame.caller_ir,
                    *arg_val,
                    parent_stack,
                    depth + 1,
                    call_ctx,
                    args_offset,
                    collector,
                );
            }
            IrCallArg::Spread(_) => {
                collector.push_dynamic_gap(
                    "Spread argument passed to callee parameter cannot be statically resolved.",
                );
            }
        }
    }

    let writes_res = var_writes::resolve_reaching_writes_for_local(
        ir,
        read_vertex.id,
        binding,
        collector.semantic,
        collector.syntax,
        collector.ir_provider,
    );
    if !writes_res.is_exhaustive {
        collector.push_dynamic_gap(
            "Variable writes across nested scopes could not be completely analyzed.",
        );
    }

    for write in &writes_res.writes {
        match write.source {
            VarWriteSource::FunctionDecl(fn_syn) => {
                let decl = collector.semantic.resolve_declaration(fn_syn);
                collector.push_target(RawTargetDescriptor {
                    decl_syntax: fn_syn,
                    fn_scope_syntax: Some(fn_syn),
                    symbol: decl.as_ref().and_then(|d| d.symbol),
                    name: decl.as_ref().and_then(|d| d.name.clone()),
                    location: decl
                        .as_ref()
                        .and_then(|d| d.location.clone())
                        .or_else(|| collector.syntax.syntax_location(fn_syn)),
                    is_external: collector.semantic.is_external_syntax(fn_syn),
                    is_default_library: decl.as_ref().is_some_and(|d| d.is_default_library),
                    implementation: None,
                    args_offset,
                });
            }
            VarWriteSource::Value(w_val) => {
                trace_callable_value(
                    &write.scope_ir,
                    w_val,
                    call_stack,
                    depth + 1,
                    call_ctx,
                    args_offset,
                    collector,
                );
            }
            VarWriteSource::Uninitialized => {
                if !handled_by_frame {
                    collector.push_dynamic_gap(
                        "Variable may remain uninitialized along a control-flow path to this call.",
                    );
                }
            }
            VarWriteSource::Opaque => {
                collector.push_dynamic_gap(
                    "Variable receives an opaque runtime value without a static declaration.",
                );
            }
        }
    }

    if writes_res.entry_param_reaches && !handled_by_frame {
        let resolved_completely =
            resolve_callback_param_from_callers(ir, binding, depth, collector);
        if !resolved_completely && collector.conflict.is_none() {
            let param_name = ir
                .bindings
                .get(binding.0 as usize)
                .map_or("<param>", |b| b.name.as_str());
            collector.push_dynamic_gap(format!(
                "Callee parameter '{param_name}' holds an unknown runtime callback value."
            ));
        }
    }
}

fn trace_construct_value(
    ir: &Arc<FunctionIr>,
    val: ValueId,
    call_stack: &[CallFrame],
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    collector: &mut TraceCollector<'_>,
) {
    if collector.conflict.is_some() {
        return;
    }
    if depth >= MAX_TRACE_DEPTH {
        collector.push_dynamic_gap("Constructor target tracing exceeded maximum depth.");
        return;
    }

    if let Some(sources) = ir.value_sources.get(&val)
        && !sources.is_empty()
    {
        for &src in sources {
            trace_construct_value(ir, src, call_stack, depth + 1, call_ctx, collector);
        }
        return;
    }

    let producers = value_producers(ir, val);
    if producers.is_empty() {
        collector.push_dynamic_gap("Constructor expression has no static definition.");
        return;
    }

    for v in producers {
        if collector.conflict.is_some() {
            return;
        }
        trace_construct_vertex_op(ir, v, call_stack, depth, call_ctx, collector);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Dispatches across all constructor target IR operations."
)]
fn trace_construct_vertex_op(
    ir: &Arc<FunctionIr>,
    v: &CfgVertex,
    call_stack: &[CallFrame],
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    collector: &mut TraceCollector<'_>,
) {
    match &v.op {
        IrOp::ClassLiteral { syntax: class_syn } => {
            emit_class_constructor_from_syntax(*class_syn, collector);
        }
        IrOp::ValueForward { source, .. }
        | IrOp::AssignLocal { value: source, .. }
        | IrOp::AssignSymbol { value: source, .. } => {
            trace_construct_value(ir, *source, call_stack, depth + 1, call_ctx, collector);
        }
        IrOp::ReadSymbol { ident_syntax } => {
            if let Some(decl) = collector.semantic.resolve_declaration(*ident_syntax) {
                let mut lookup_syns = vec![*ident_syntax];
                if depth == 0 {
                    lookup_syns.insert(0, collector.call_syntax);
                }
                if try_apply_calls_summary(
                    &lookup_syns,
                    Some(&decl),
                    call_ctx.as_ref(),
                    depth,
                    0,
                    collector,
                ) {
                    return;
                }
                emit_constructor_from_decl(&decl, depth, collector);
            } else {
                collector.push_dynamic_gap("Constructor target symbol could not be resolved.");
            }
        }
        IrOp::ReadLocal { binding } => {
            let mut handled_by_frame = false;
            if let Some((frame, parent_stack)) = call_stack.split_last()
                && let Some(param_idx) = ir
                    .parameters
                    .iter()
                    .position(|p| p.binding == Some(*binding))
                && let Some(IrCallArg::Positional(arg_val)) = frame.args.get(param_idx)
            {
                handled_by_frame = true;
                trace_construct_value(
                    &frame.caller_ir,
                    *arg_val,
                    parent_stack,
                    depth + 1,
                    call_ctx,
                    collector,
                );
            }
            let writes = var_writes::resolve_reaching_writes_for_local(
                ir,
                v.id,
                *binding,
                collector.semantic,
                collector.syntax,
                collector.ir_provider,
            );
            for write in &writes.writes {
                if let VarWriteSource::Value(w_val) = write.source {
                    trace_construct_value(
                        &write.scope_ir,
                        w_val,
                        call_stack,
                        depth + 1,
                        call_ctx,
                        collector,
                    );
                }
            }
            if writes.entry_param_reaches && !handled_by_frame {
                collector.push_dynamic_gap(
                    "Constructor parameter holds an unknown runtime class/constructor.",
                );
            }
        }
        IrOp::Call {
            callee: inner_callee,
            receiver: inner_receiver,
            args: inner_args,
            ..
        } => {
            trace_call_return_values(
                ir,
                v.syntax,
                *inner_callee,
                *inner_receiver,
                inner_args,
                call_stack,
                depth,
                collector,
                true,
            );
        }
        IrOp::PropRead { object, key, .. } => {
            trace_prop_read_construct(ir, v, *object, key, call_stack, depth, call_ctx, collector);
        }
        _ => {
            collector
                .push_dynamic_gap("Dynamic constructor target could not be statically resolved.");
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Resolves namespace, module, and object-literal constructor targets for PropRead."
)]
fn trace_prop_read_construct(
    ir: &Arc<FunctionIr>,
    v: &CfgVertex,
    object: ValueId,
    key: &IrPropKey,
    call_stack: &[CallFrame],
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    collector: &mut TraceCollector<'_>,
) {
    if let Some(prop_syn) = v.syntax {
        let decl = collector.semantic.resolve_declaration(prop_syn);
        let mut lookup_syns = vec![prop_syn];
        if depth == 0 {
            lookup_syns.insert(0, collector.call_syntax);
        }
        if try_apply_calls_summary(
            &lookup_syns,
            decl.as_ref(),
            call_ctx.as_ref(),
            depth,
            0,
            collector,
        ) {
            return;
        }
    }
    let obj_ctx = ObjectLookupContext::from_collector(collector);
    if let Some(member_name) = prop_key_static_name(key)
        && let Some(prop_vals) = resolve_local_object_property(
            ir,
            object,
            member_name,
            call_stack,
            depth + 1,
            Some(&obj_ctx),
        )
    {
        if !prop_vals.is_exhaustive {
            collector.push_dynamic_gap(format!(
                "Object literal holding constructor property '{member_name}' may have overwritten or spread properties."
            ));
        }
        for (prop_ir, prop_val, prop_stack) in prop_vals.values {
            trace_construct_value(
                &prop_ir,
                prop_val,
                &prop_stack,
                depth + 1,
                call_ctx,
                collector,
            );
        }
        return;
    }
    if let Some(prop_syn) = v.syntax
        && let Some(decl) = collector.semantic.resolve_declaration(prop_syn)
    {
        if decl.is_signature_only {
            collector.push_dynamic_gap(
                "Constructed property resolves only to a type signature without a runtime class or constructor.",
            );
        } else {
            emit_constructor_from_decl(&decl, depth, collector);
        }
        return;
    }
    collector.push_dynamic_gap("Dynamic constructor property could not be statically resolved.");
}

fn emit_class_constructor_from_syntax(class_syn: SyntaxId, collector: &mut TraceCollector<'_>) {
    if let Some(ctor_decl) = collector.semantic.resolve_class_constructor(class_syn) {
        emit_resolved_declaration(
            &ctor_decl,
            Some(collector.call_syntax),
            false,
            None,
            0,
            collector,
        );
    } else if let Some(class_decl) = collector.semantic.resolve_declaration(class_syn) {
        emit_resolved_declaration(
            &class_decl,
            Some(collector.call_syntax),
            false,
            None,
            0,
            collector,
        );
    } else {
        collector.push_dynamic_gap("Class constructor declaration could not be resolved.");
    }
}

fn emit_constructor_from_decl(
    decl: &DeclarationSummaryData,
    depth: usize,
    collector: &mut TraceCollector<'_>,
) {
    let Some(decl_syn) = decl.primary_syntax else {
        collector.push_dynamic_gap("Constructed declaration has no primary syntax.");
        return;
    };
    if matches!(
        decl.kind.as_deref(),
        Some("ClassDeclaration" | "ClassExpression" | "Constructor")
    ) {
        emit_class_constructor_from_syntax(decl_syn, collector);
        return;
    }
    if matches!(
        decl.kind.as_deref(),
        Some("VariableDeclaration" | "BindingElement" | "ExportAssignment")
    ) && !decl.is_external
    {
        let effective_decl_syn = resolve_writable_decl_syntax(decl_syn, collector);
        let writes = var_writes::collect_all_writes_for_decl(
            effective_decl_syn,
            decl.symbol,
            collector.semantic,
            collector.syntax,
            collector.ir_provider,
        );
        if !writes.writes.is_empty() {
            for w in writes.writes {
                if let VarWriteSource::Value(w_val) = w.source {
                    trace_construct_value(&w.scope_ir, w_val, &[], depth + 1, None, collector);
                }
            }
            return;
        }
    }
    emit_resolved_declaration(decl, Some(collector.call_syntax), false, None, 0, collector);
}

fn resolve_writable_decl_syntax(decl_syn: SyntaxId, collector: &TraceCollector<'_>) -> SyntaxId {
    if collector.ir_provider.enclosing_scope_ir(decl_syn).is_ok() {
        return decl_syn;
    }
    if let ImplementationFact::Resolved(imp) = collector
        .semantic
        .resolve_implementation_for_syntax(decl_syn, None)
    {
        return imp.declaration_syntax;
    }
    decl_syn
}

fn emit_decl_or_variable_writes(
    decl: &DeclarationSummaryData,
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    let Some(decl_syn) = decl.primary_syntax else {
        collector.push_dynamic_gap("Resolved callee symbol has no primary declaration node.");
        return;
    };

    if matches!(
        decl.kind.as_deref(),
        Some("VariableDeclaration" | "BindingElement" | "ExportAssignment")
    ) && !decl.is_external
    {
        let effective_decl_syn = resolve_writable_decl_syntax(decl_syn, collector);
        let writes = var_writes::collect_all_writes_for_decl(
            effective_decl_syn,
            decl.symbol,
            collector.semantic,
            collector.syntax,
            collector.ir_provider,
        );
        if !writes.writes.is_empty() {
            if !writes.is_exhaustive {
                collector.push_dynamic_gap(
                    "Module variable writes across scopes could not be exhaustively resolved.",
                );
            }
            for w in writes.writes {
                match w.source {
                    VarWriteSource::FunctionDecl(fn_syn) => {
                        if let Some(fn_decl) = collector.semantic.resolve_declaration(fn_syn) {
                            emit_resolved_declaration(
                                &fn_decl,
                                Some(collector.call_syntax),
                                false,
                                call_ctx.as_ref(),
                                args_offset,
                                collector,
                            );
                        }
                    }
                    VarWriteSource::Value(w_val) => {
                        trace_callable_value(
                            &w.scope_ir,
                            w_val,
                            &[],
                            depth + 1,
                            call_ctx,
                            args_offset,
                            collector,
                        );
                    }
                    VarWriteSource::Uninitialized => {
                        collector.push_dynamic_gap("Variable may be uninitialized at call site.");
                    }
                    VarWriteSource::Opaque => {
                        collector.push_dynamic_gap("Variable holds an opaque value.");
                    }
                }
            }
            return;
        }
    }

    emit_resolved_declaration(
        decl,
        Some(collector.call_syntax),
        false,
        call_ctx.as_ref(),
        args_offset,
        collector,
    );
}

fn resolve_symbol_callee(
    ident_syntax: SyntaxId,
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    let Some(decl) = collector.semantic.resolve_declaration(ident_syntax) else {
        collector
            .push_dynamic_gap("Unresolved callee identifier cannot be mapped to a declaration.");
        return;
    };

    if decl.is_default_library
        && decl.name.as_deref() == Some("undefined")
        && decl.kind.as_deref() == Some("VariableDeclaration")
    {
        return;
    }

    let mut lookup_syns = vec![ident_syntax];
    if depth == 0 && call_ctx.is_some() {
        lookup_syns.insert(0, collector.call_syntax);
    }
    if try_apply_calls_summary(
        &lookup_syns,
        Some(&decl),
        call_ctx.as_ref(),
        depth,
        args_offset,
        collector,
    ) {
        return;
    }

    if decl.is_signature_only {
        collector.push_dynamic_gap(
            "Callee resolves only to a TypeScript type or parameter signature without a runtime function.",
        );
        return;
    }

    emit_decl_or_variable_writes(&decl, depth, call_ctx, args_offset, collector);
}

#[expect(
    clippy::too_many_arguments,
    reason = "Resolves local object literals, exact constructor instances, static accessors, behavior summaries, and polymorphic class overrides."
)]
fn resolve_prop_read_callable(
    ir: &Arc<FunctionIr>,
    prop_vertex: &CfgVertex,
    object_val: ValueId,
    key: &IrPropKey,
    is_super: bool,
    call_stack: &[CallFrame],
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    let Some(member_name) = prop_key_static_name(key) else {
        collector.push_dynamic_gap(
            "Computed property call without a static string key cannot be statically resolved.",
        );
        return;
    };

    // 1. Direct `super.method(...)` or ECMAScript `#private` method calls are statically single-target.
    if (is_super || matches!(key, IrPropKey::PrivateIdent(_)))
        && let Some(prop_syn) = prop_vertex.syntax
        && let Some(decl) = collector.semantic.resolve_declaration(prop_syn)
        && !decl.is_signature_only
    {
        emit_resolved_declaration(
            &decl,
            Some(collector.call_syntax),
            false,
            call_ctx.as_ref(),
            args_offset,
            collector,
        );
        return;
    }

    // 2. Local or imported object-literal property resolution (`const obj = { run: fn }; obj.run()`,
    //    `export const posix: IPath = { normalize() {} }`, or destructured IIFE exports).
    let obj_ctx = ObjectLookupContext::from_collector(collector);
    if let Some(prop_vals) = resolve_local_object_property(
        ir,
        object_val,
        member_name,
        call_stack,
        depth + 1,
        Some(&obj_ctx),
    ) {
        if !prop_vals.is_exhaustive {
            collector.push_dynamic_gap(format!(
                "Object literal holding property '{member_name}' may have overwritten or spread properties."
            ));
        }
        for (prop_ir, prop_val, prop_stack) in prop_vals.values {
            trace_callable_value(
                &prop_ir,
                prop_val,
                &prop_stack,
                depth + 1,
                call_ctx,
                args_offset,
                collector,
            );
        }
        return;
    }

    // 2.5 Behavior summary lookup on resolved property/member access (takes precedence over body & external gaps).
    if let Some(prop_syn) = prop_vertex.syntax {
        let decl = collector.semantic.resolve_declaration(prop_syn);
        let mut lookup_syns = vec![prop_syn];
        if depth == 0 && call_ctx.is_some() {
            lookup_syns.push(collector.call_syntax);
        }
        if try_apply_calls_summary(
            &lookup_syns,
            decl.as_ref(),
            call_ctx.as_ref(),
            depth,
            args_offset,
            collector,
        ) {
            return;
        }
    }

    // 3. Static class methods / module namespace property calls (`Namespace.fn()`, `Foo.bar()`).
    if let Some(prop_syn) = prop_vertex.syntax
        && resolve_static_member_call(
            prop_syn,
            member_name,
            depth,
            call_ctx,
            args_offset,
            collector,
        )
    {
        return;
    }

    // 4. Instance member call -> `class_overrides` (using exact local instance identity when known).
    resolve_member_call(
        ir,
        object_val,
        member_name,
        is_super,
        MemberAccessKind::MethodOrProp,
        call_stack,
        call_ctx,
        args_offset,
        collector,
    );
}

fn resolve_static_member_call(
    prop_syn: SyntaxId,
    member_name: &str,
    depth: usize,
    call_ctx: Option<ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) -> bool {
    if let Some(decl) = collector.semantic.resolve_declaration(prop_syn)
        && matches!(
            decl.kind.as_deref(),
            Some(
                "FunctionDeclaration"
                    | "VariableDeclaration"
                    | "BindingElement"
                    | "ExportAssignment"
            )
        )
        && !decl.is_signature_only
    {
        emit_decl_or_variable_writes(&decl, depth, call_ctx, args_offset, collector);
        return true;
    }

    let Some(static_hierarchy) =
        collector
            .semantic
            .member_target_hierarchy(prop_syn, Some(member_name), true)
    else {
        return false;
    };

    let mut found_static = false;
    for node in &static_hierarchy.candidate_classes {
        if static_hierarchy.receiver_symbols.contains(&node.symbol)
            && let Some(m) = node
                .members
                .iter()
                .find(|m| m.is_static && m.name == member_name)
        {
            found_static = true;
            let target_syn = m.callable_syntax.unwrap_or(m.member_syntax);
            if let Some(decl) = collector.semantic.resolve_declaration(target_syn) {
                emit_resolved_declaration(
                    &decl,
                    Some(collector.call_syntax),
                    node.is_external,
                    call_ctx.as_ref(),
                    args_offset,
                    collector,
                );
            }
        }
    }
    found_static
}

#[expect(
    clippy::too_many_arguments,
    reason = "Resolves exact-instance or polymorphic member call targets with optional call summary context."
)]
fn resolve_member_call(
    ir: &Arc<FunctionIr>,
    object_val: ValueId,
    member_name: &str,
    _is_super: bool,
    access_kind: MemberAccessKind,
    call_stack: &[CallFrame],
    call_ctx: Option<ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    // Check if receiver originates purely from local `new Sub()` allocations.
    let exact_classes = resolve_local_constructed_classes(
        ir,
        object_val,
        call_stack,
        collector.semantic,
        collector.syntax,
        collector.ir_provider,
    );
    if !exact_classes.is_empty() && exact_classes.iter().all(Option::is_some) {
        let mut any_resolved = false;
        for sym in exact_classes.into_iter().flatten() {
            if let Some(hierarchy) = collector.semantic.class_hierarchy_for_symbol(sym) {
                any_resolved = true;
                let res = class_overrides::resolve_member_overrides(
                    &hierarchy,
                    member_name,
                    access_kind,
                    collector.call_location.as_ref(),
                    true,
                );
                apply_override_resolution(member_name, res, call_ctx, args_offset, collector);
            }
        }
        if any_resolved {
            return;
        }
    }

    let recv_syntax =
        value_syntax_in_ir(ir, object_val, collector.syntax).unwrap_or(collector.call_syntax);
    let Some(hierarchy) =
        collector
            .semantic
            .member_target_hierarchy(recv_syntax, Some(member_name), false)
    else {
        collector.open_member_name = Some(member_name.to_string());
        collector.push_gap(GapItem::from_reason(
            UnknownReason::UnresolvedType,
            format!("Receiver type for member '{member_name}' could not be determined."),
            collector.call_location.clone(),
        ));
        return;
    };

    let res = class_overrides::resolve_member_overrides(
        &hierarchy,
        member_name,
        access_kind,
        collector.call_location.as_ref(),
        false,
    );
    apply_override_resolution(member_name, res, call_ctx, args_offset, collector);
}

fn apply_override_resolution(
    member_name: &str,
    res: class_overrides::ClassOverrideResolution,
    call_ctx: Option<ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    if !res.is_complete {
        collector.open_member_name = Some(member_name.to_string());
    }
    for gap in res.gaps {
        collector.push_gap(gap);
    }
    for ov in res.targets {
        if let Some(decl) = collector.semantic.resolve_declaration(ov.target_syntax) {
            emit_resolved_declaration(
                &decl,
                Some(collector.call_syntax),
                ov.is_external,
                call_ctx.as_ref(),
                args_offset,
                collector,
            );
        } else {
            collector.push_target(RawTargetDescriptor {
                decl_syntax: ov.target_syntax,
                fn_scope_syntax: ov.has_body.then_some(ov.target_syntax),
                symbol: ov.member_symbol,
                name: Some(member_name.to_string()),
                location: ov.location,
                is_external: ov.is_external,
                is_default_library: false,
                implementation: None,
                args_offset,
            });
        }
    }
}

fn emit_resolved_declaration(
    decl: &DeclarationSummaryData,
    call_site_syntax: Option<SyntaxId>,
    force_external: bool,
    call_ctx: Option<&ActiveCallContext<'_>>,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    if let Some(decl_syn) = decl.primary_syntax {
        let mut lookup_syns = vec![decl_syn];
        if call_ctx.is_some()
            && let Some(cs) = call_site_syntax
            && cs != decl_syn
        {
            lookup_syns.insert(0, cs);
        }
        if try_apply_calls_summary(
            &lookup_syns,
            Some(decl),
            call_ctx,
            0,
            args_offset,
            collector,
        ) {
            return;
        }
    }

    if decl.is_signature_only {
        collector.push_dynamic_gap(
            "Target resolves only to a type signature rather than a concrete implementation.",
        );
        return;
    }

    let Some(decl_syn) = decl.primary_syntax else {
        collector.push_dynamic_gap("Resolved declaration has no syntax node.");
        return;
    };

    let impl_fact = collector
        .semantic
        .resolve_implementation_for_syntax(decl_syn, call_site_syntax);
    match impl_fact {
        ImplementationFact::Resolved(imp) => {
            let is_external = force_external || decl.is_external;
            let fn_scope_syntax = imp
                .fn_syntax
                .or(decl.callable_syntax)
                .or_else(|| decl.has_body.then_some(decl_syn));
            collector.push_target(RawTargetDescriptor {
                decl_syntax: imp.declaration_syntax,
                fn_scope_syntax,
                symbol: imp.declaration_symbol.or(decl.symbol),
                name: decl.name.clone().or_else(|| imp.export_name.clone()),
                location: Some(imp.location.clone()),
                is_external,
                is_default_library: decl.is_default_library,
                implementation: (imp.resolution_source
                    != tssae_common::ImplementationSourceKind::CurrentProject)
                    .then_some(imp),
                args_offset,
            });
        }
        ImplementationFact::Inconclusive { gaps, evidence, .. } => {
            collector.evidence.extend(evidence);
            for g in gaps {
                collector.push_gap(g);
            }
            if !collector
                .gaps
                .iter()
                .any(|g| g.kind == "missing-source-file")
            {
                collector.is_complete = false;
            }
            push_external_fallback_target(decl, decl_syn, args_offset, collector);
        }
        ImplementationFact::External {
            package_name,
            evidence,
            ..
        } => {
            collector.evidence.extend(evidence);
            let target_label = decl
                .name
                .as_deref()
                .or(package_name.as_deref())
                .unwrap_or("external declaration");
            collector.push_gap(GapItem::from_reason(
                UnknownReason::MissingBehaviorSummary,
                format!(
                    "External declaration '{target_label}' has no behavior summary describing callbacks or callees."
                ),
                collector.call_location.clone(),
            ));
            push_external_fallback_target(decl, decl_syn, args_offset, collector);
        }
    }
}

fn push_external_fallback_target(
    decl: &DeclarationSummaryData,
    decl_syn: SyntaxId,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) {
    collector.push_target(RawTargetDescriptor {
        decl_syntax: decl_syn,
        fn_scope_syntax: None,
        symbol: decl.symbol,
        name: decl.name.clone(),
        location: decl.location.clone(),
        is_external: true,
        is_default_library: decl.is_default_library,
        implementation: None,
        args_offset,
    });
}

struct ObjectPropLookupResult {
    values: Vec<(Arc<FunctionIr>, ValueId, Vec<CallFrame>)>,
    is_exhaustive: bool,
}

#[derive(Clone, Copy)]
struct ObjectLookupContext<'a> {
    call_syntax: SyntaxId,
    call_location: Option<&'a DiagnosticLocation>,
    semantic: &'a dyn SemanticModel,
    syntax: &'a dyn SyntaxCatalog,
    ir_provider: &'a dyn IrProvider,
    summary_index: &'a BehaviorSummaryIndex,
}

impl<'a> ObjectLookupContext<'a> {
    fn from_collector(collector: &'a TraceCollector<'a>) -> Self {
        Self {
            call_syntax: collector.call_syntax,
            call_location: collector.call_location.as_ref(),
            semantic: collector.semantic,
            syntax: collector.syntax,
            ir_provider: collector.ir_provider,
            summary_index: collector.summary_index,
        }
    }
}

struct ObjectPropSearch<'a> {
    ctx: Option<&'a ObjectLookupContext<'a>>,
    visited: &'a mut FxHashSet<(FunctionIrId, ValueId, String, usize)>,
    out: ObjectPropLookupResult,
}

fn resolve_local_object_property(
    ir: &Arc<FunctionIr>,
    object_val: ValueId,
    prop_name: &str,
    call_stack: &[CallFrame],
    initial_depth: usize,
    ctx: Option<&ObjectLookupContext<'_>>,
) -> Option<ObjectPropLookupResult> {
    let mut visited = FxHashSet::default();
    resolve_local_object_property_with_visited(
        ir,
        object_val,
        prop_name,
        call_stack,
        initial_depth,
        ctx,
        &mut visited,
    )
}

fn resolve_local_object_property_with_visited(
    ir: &Arc<FunctionIr>,
    object_val: ValueId,
    prop_name: &str,
    call_stack: &[CallFrame],
    initial_depth: usize,
    ctx: Option<&ObjectLookupContext<'_>>,
    visited: &mut FxHashSet<(FunctionIrId, ValueId, String, usize)>,
) -> Option<ObjectPropLookupResult> {
    if initial_depth >= MAX_TRACE_DEPTH {
        return None;
    }
    let mut search = ObjectPropSearch {
        ctx,
        visited,
        out: ObjectPropLookupResult {
            values: Vec::new(),
            is_exhaustive: true,
        },
    };
    if !collect_object_alloc_props(
        ir,
        object_val,
        prop_name,
        call_stack,
        initial_depth,
        &mut search,
    ) || search.out.values.is_empty()
    {
        return None;
    }
    Some(search.out)
}

fn collect_object_props_from_decl_syntax(
    decl_syn: SyntaxId,
    prop_name: &str,
    depth: usize,
    search: &mut ObjectPropSearch<'_>,
) -> bool {
    let Some(ctx) = search.ctx else {
        return false;
    };
    let Some(decl) = ctx.semantic.resolve_declaration(decl_syn) else {
        return false;
    };
    if decl.is_external || decl.is_signature_only {
        return false;
    }
    if !matches!(
        decl.kind.as_deref(),
        Some("VariableDeclaration" | "BindingElement" | "ExportAssignment")
    ) {
        return false;
    }
    let Some(raw_decl_syn) = decl.primary_syntax else {
        return false;
    };
    let effective_decl_syn = if ctx.ir_provider.enclosing_scope_ir(raw_decl_syn).is_ok() {
        raw_decl_syn
    } else if let ImplementationFact::Resolved(imp) = ctx
        .semantic
        .resolve_implementation_for_syntax(raw_decl_syn, None)
    {
        imp.declaration_syntax
    } else {
        return false;
    };
    let writes = var_writes::collect_all_writes_for_decl(
        effective_decl_syn,
        decl.symbol,
        ctx.semantic,
        ctx.syntax,
        ctx.ir_provider,
    );
    if writes.mutability != BindingMutability::Const || writes.writes.is_empty() {
        return false;
    }
    if !writes.is_exhaustive {
        search.out.is_exhaustive = false;
    }
    for w in writes.writes {
        match w.source {
            VarWriteSource::Value(w_val) => {
                if !collect_object_alloc_props(
                    &w.scope_ir,
                    w_val,
                    prop_name,
                    &[],
                    depth + 1,
                    search,
                ) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

#[expect(
    clippy::too_many_lines,
    reason = "Traces local and imported object literals, chained properties, and IIFE/factory returns uniformly."
)]
fn collect_object_alloc_props(
    ir: &Arc<FunctionIr>,
    val: ValueId,
    prop_name: &str,
    call_stack: &[CallFrame],
    depth: usize,
    search: &mut ObjectPropSearch<'_>,
) -> bool {
    if depth >= MAX_TRACE_DEPTH
        || !search.visited.insert((
            ir.id,
            val,
            prop_name.to_owned(),
            stack_fingerprint(call_stack),
        ))
    {
        return false;
    }

    if let Some(sources) = ir.value_sources.get(&val)
        && !sources.is_empty()
    {
        return sources
            .iter()
            .all(|&s| collect_object_alloc_props(ir, s, prop_name, call_stack, depth + 1, search));
    }

    let producers = value_producers(ir, val);
    if producers.is_empty() {
        return false;
    }

    for v in producers {
        match &v.op {
            IrOp::ObjectAlloc { props, .. } => {
                let mut matched_here = false;
                for prop in props.iter().rev() {
                    match prop {
                        IrObjectProp::KeyValue { key, value } => {
                            if prop_key_static_name(key) == Some(prop_name) {
                                search.out.values.push((
                                    Arc::clone(ir),
                                    *value,
                                    call_stack.to_vec(),
                                ));
                                matched_here = true;
                                break;
                            }
                        }
                        IrObjectProp::Spread(_) => {
                            search.out.is_exhaustive = false;
                        }
                    }
                }
                if !matched_here {
                    return false;
                }
            }
            IrOp::ValueForward { source, .. }
            | IrOp::AssignLocal { value: source, .. }
            | IrOp::AssignSymbol { value: source, .. } => {
                if !collect_object_alloc_props(
                    ir,
                    *source,
                    prop_name,
                    call_stack,
                    depth + 1,
                    search,
                ) {
                    return false;
                }
            }
            IrOp::ReadLocal { binding } => {
                if let Some((frame, parent_stack)) = call_stack.split_last()
                    && let Some(param_idx) = ir
                        .parameters
                        .iter()
                        .position(|p| p.binding == Some(*binding))
                    && let Some(IrCallArg::Positional(arg_val)) = frame.args.get(param_idx)
                {
                    if !collect_object_alloc_props(
                        &frame.caller_ir,
                        *arg_val,
                        prop_name,
                        parent_stack,
                        depth + 1,
                        search,
                    ) {
                        return false;
                    }
                    continue;
                }
                let b_info = ir.bindings.get(binding.0 as usize);
                if b_info.is_some_and(|b| b.mutability != BindingMutability::Const) {
                    return false;
                }
                let writes: Vec<ValueId> = ir
                    .vertices
                    .iter()
                    .filter_map(|wv| match wv.op {
                        IrOp::AssignLocal { target, value, .. } if target == *binding => {
                            Some(value)
                        }
                        _ => None,
                    })
                    .collect();
                if writes.len() != 1 {
                    return false;
                }
                if !collect_object_alloc_props(
                    ir,
                    writes[0],
                    prop_name,
                    call_stack,
                    depth + 1,
                    search,
                ) {
                    return false;
                }
            }
            IrOp::ReadSymbol { ident_syntax } => {
                if !collect_object_props_from_decl_syntax(*ident_syntax, prop_name, depth, search) {
                    return false;
                }
            }
            IrOp::PropRead {
                object: inner_obj,
                key: inner_key,
                ..
            } => {
                let resolved_inner = if let Some(inner_name) = prop_key_static_name(inner_key)
                    && let Some(inner_props) = resolve_local_object_property_with_visited(
                        ir,
                        *inner_obj,
                        inner_name,
                        call_stack,
                        depth + 1,
                        search.ctx,
                        search.visited,
                    ) {
                    if !inner_props.is_exhaustive {
                        search.out.is_exhaustive = false;
                    }
                    for (p_ir, p_val, p_stack) in inner_props.values {
                        if !collect_object_alloc_props(
                            &p_ir,
                            p_val,
                            prop_name,
                            &p_stack,
                            depth + 1,
                            search,
                        ) {
                            return false;
                        }
                    }
                    true
                } else {
                    false
                };
                if !resolved_inner
                    && let Some(prop_syn) = v.syntax
                    && collect_object_props_from_decl_syntax(prop_syn, prop_name, depth, search)
                {
                    continue;
                }
                if !resolved_inner {
                    return false;
                }
            }
            IrOp::Call {
                callee: inner_callee,
                args: inner_args,
                ..
            } => {
                let Some(ctx) = search.ctx else {
                    return false;
                };
                if call_stack.len() >= MAX_CALL_STACK_DEPTH {
                    return false;
                }
                let inner_syntax = v.syntax.unwrap_or(ctx.call_syntax);
                let mut sub_collector = TraceCollector::new(
                    inner_syntax,
                    ctx.call_location.cloned(),
                    ctx.semantic,
                    ctx.syntax,
                    ctx.ir_provider,
                    ctx.summary_index,
                );
                trace_callable_value(
                    ir,
                    *inner_callee,
                    call_stack,
                    depth + 1,
                    None,
                    0,
                    &mut sub_collector,
                );
                if sub_collector.conflict.is_some()
                    || !sub_collector.is_complete
                    || sub_collector.targets.is_empty()
                {
                    return false;
                }
                let mut next_stack = call_stack.to_vec();
                next_stack.push(CallFrame {
                    caller_ir: Arc::clone(ir),
                    args: inner_args.clone(),
                });
                let mut has_return = false;
                for target in sub_collector.targets {
                    let Some(scope_syn) = target.fn_scope_syntax else {
                        return false;
                    };
                    let Ok(target_ir) = ctx.ir_provider.function_ir(scope_syn) else {
                        return false;
                    };
                    let mut target_has_return = false;
                    for cv in &target_ir.vertices {
                        if let IrOp::Return {
                            value: Some(ret_val),
                        } = cv.op
                        {
                            target_has_return = true;
                            has_return = true;
                            if !collect_object_alloc_props(
                                &target_ir,
                                ret_val,
                                prop_name,
                                &next_stack,
                                depth + 1,
                                search,
                            ) {
                                return false;
                            }
                        }
                    }
                    if !target_has_return {
                        return false;
                    }
                }
                if !has_return {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

struct LocalTraceContext<'a> {
    semantic: &'a dyn SemanticModel,
    syntax: &'a dyn SyntaxCatalog,
    ir_provider: &'a dyn IrProvider,
}

fn resolve_local_constructed_classes(
    ir: &Arc<FunctionIr>,
    object_val: ValueId,
    call_stack: &[CallFrame],
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
) -> Vec<Option<SymbolId>> {
    let mut visited = FxHashSet::default();
    let mut classes = Vec::new();
    let ctx = LocalTraceContext {
        semantic,
        syntax,
        ir_provider,
    };
    collect_constructed_classes(
        ir,
        object_val,
        call_stack,
        0,
        &mut visited,
        &ctx,
        &mut classes,
    );
    classes
}

fn collect_constructed_classes(
    ir: &Arc<FunctionIr>,
    val: ValueId,
    call_stack: &[CallFrame],
    depth: usize,
    visited: &mut FxHashSet<(FunctionIrId, ValueId)>,
    ctx: &LocalTraceContext<'_>,
    out: &mut Vec<Option<SymbolId>>,
) {
    if depth >= MAX_TRACE_DEPTH || !visited.insert((ir.id, val)) {
        out.push(None);
        return;
    }

    if let Some(sources) = ir.value_sources.get(&val)
        && !sources.is_empty()
    {
        for &s in sources {
            collect_constructed_classes(ir, s, call_stack, depth + 1, visited, ctx, out);
        }
        return;
    }

    let producers = value_producers(ir, val);
    if producers.is_empty() {
        out.push(None);
        return;
    }

    for v in producers {
        match &v.op {
            IrOp::Construct { callee, .. } => {
                collect_class_symbol_from_val(ir, *callee, depth + 1, visited, ctx.semantic, out);
            }
            IrOp::ValueForward { source, .. }
            | IrOp::AssignLocal { value: source, .. }
            | IrOp::AssignSymbol { value: source, .. } => {
                collect_constructed_classes(ir, *source, call_stack, depth + 1, visited, ctx, out);
            }
            IrOp::ReadLocal { binding } => {
                collect_read_local_constructed_classes(
                    ir, v.id, *binding, call_stack, depth, visited, ctx, out,
                );
            }
            _ => out.push(None),
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Traces parameter forwarding and reaching writes for exact constructed class resolution."
)]
fn collect_read_local_constructed_classes(
    ir: &Arc<FunctionIr>,
    read_vertex: tssae_common::VertexId,
    binding: tssae_common::LocalBindingId,
    call_stack: &[CallFrame],
    depth: usize,
    visited: &mut FxHashSet<(FunctionIrId, ValueId)>,
    ctx: &LocalTraceContext<'_>,
    out: &mut Vec<Option<SymbolId>>,
) {
    if let Some((frame, parent_stack)) = call_stack.split_last()
        && let Some(param_idx) = ir
            .parameters
            .iter()
            .position(|p| p.binding == Some(binding))
        && let Some(IrCallArg::Positional(arg_val)) = frame.args.get(param_idx)
    {
        collect_constructed_classes(
            &frame.caller_ir,
            *arg_val,
            parent_stack,
            depth + 1,
            visited,
            ctx,
            out,
        );
        return;
    }
    let writes = var_writes::resolve_reaching_writes_for_local(
        ir,
        read_vertex,
        binding,
        ctx.semantic,
        ctx.syntax,
        ctx.ir_provider,
    );
    if !writes.is_exhaustive || writes.entry_param_reaches || writes.writes.is_empty() {
        out.push(None);
        return;
    }
    for w in writes.writes {
        if let VarWriteSource::Value(w_val) = w.source {
            collect_constructed_classes(
                &w.scope_ir,
                w_val,
                call_stack,
                depth + 1,
                visited,
                ctx,
                out,
            );
        } else {
            out.push(None);
        }
    }
}

fn collect_class_symbol_from_val(
    ir: &Arc<FunctionIr>,
    val: ValueId,
    depth: usize,
    visited: &mut FxHashSet<(FunctionIrId, ValueId)>,
    semantic: &dyn SemanticModel,
    out: &mut Vec<Option<SymbolId>>,
) {
    if depth >= MAX_TRACE_DEPTH || !visited.insert((ir.id, val)) {
        out.push(None);
        return;
    }
    let producers: Vec<&CfgVertex> = ir
        .vertices
        .iter()
        .filter(|v| {
            v.value == Some(val)
                && !matches!(
                    v.op,
                    IrOp::AssignLocal { is_init: true, .. }
                        | IrOp::AssignSymbol { is_init: true, .. }
                )
        })
        .collect();
    if producers.is_empty() {
        out.push(None);
        return;
    }
    for v in producers {
        match &v.op {
            IrOp::ClassLiteral { syntax: syn } => {
                out.push(v.symbol.or_else(|| semantic.symbol_at_syntax(*syn)));
            }
            IrOp::ReadSymbol { ident_syntax } => {
                out.push(
                    v.symbol
                        .or_else(|| semantic.symbol_at_syntax(*ident_syntax)),
                );
            }
            IrOp::ValueForward { source, .. } => {
                collect_class_symbol_from_val(ir, *source, depth + 1, visited, semantic, out);
            }
            _ => out.push(None),
        }
    }
}

fn trace_iterator_element_callable(
    ir: &Arc<FunctionIr>,
    next_vertex: &CfgVertex,
    iterator_val: ValueId,
    call_stack: &[CallFrame],
    depth: usize,
    collector: &mut TraceCollector<'_>,
) {
    let Some(init_v) = ir
        .vertices
        .iter()
        .find(|v| v.value == Some(iterator_val) && matches!(v.op, IrOp::IteratorInit { .. }))
    else {
        collector.push_dynamic_gap("Iterator step has no backing iterable initializer.");
        return;
    };
    let IrOp::IteratorInit { iterable, .. } = init_v.op else {
        return;
    };

    // Determine whether this step belongs to a positional destructuring or a `for-of` loop.
    let step_index = ir
        .vertices
        .iter()
        .filter(|v| {
            matches!(
                &v.op,
                IrOp::IteratorNext { iterator, .. } if *iterator == iterator_val
            )
        })
        .position(|v| v.id == next_vertex.id);

    let is_for_of = next_vertex
        .syntax
        .and_then(|s| collector.syntax.syntax_kind(s))
        .is_some_and(|k| k == "ForOfStatement");

    let mut elem_vals: Vec<(Arc<FunctionIr>, ValueId, Vec<CallFrame>)> = Vec::new();
    let mut array_visited = FxHashSet::default();
    let complete_array = collect_array_elements(
        ir,
        iterable,
        call_stack,
        0,
        &mut array_visited,
        if is_for_of { None } else { step_index },
        &mut elem_vals,
    );
    if !complete_array || elem_vals.is_empty() {
        collector.push_dynamic_gap(
            "Iterable source in destructuring or `for-of` is not a statically closed array literal.",
        );
    }
    for (el_ir, el_val, el_stack) in elem_vals {
        trace_callable_value(&el_ir, el_val, &el_stack, depth + 1, None, 0, collector);
    }
}

fn collect_array_alloc_elems(
    ir: &Arc<FunctionIr>,
    elems: &[IrArrayElem],
    call_stack: &[CallFrame],
    positional_index: Option<usize>,
    out: &mut Vec<(Arc<FunctionIr>, ValueId, Vec<CallFrame>)>,
) -> bool {
    let has_spread = elems.iter().any(|e| matches!(e, IrArrayElem::Spread(_)));
    if let Some(target_idx) = positional_index
        && !has_spread
    {
        // Note: `ir_lower` emits `IteratorNext` vertices from back to front (due to `.iter().rev()`),
        // so vertex order `0..n` corresponds to reversed element index `n - 1 - target_idx`.
        let next_count = ir
            .vertices
            .iter()
            .filter(|nv| matches!(nv.op, IrOp::IteratorNext { .. }))
            .count();
        let rev_idx = elems
            .len()
            .checked_sub(1 + target_idx)
            .filter(|_| next_count <= elems.len());
        if let Some(el) = rev_idx.and_then(|idx| elems.get(idx)) {
            return match el {
                IrArrayElem::Value(ev) => {
                    out.push((Arc::clone(ir), *ev, call_stack.to_vec()));
                    true
                }
                IrArrayElem::Elision | IrArrayElem::Spread(_) => false,
            };
        }
    }
    let mut all_closed = !has_spread;
    for el in elems {
        if let IrArrayElem::Value(ev) = el {
            out.push((Arc::clone(ir), *ev, call_stack.to_vec()));
        } else {
            all_closed = false;
        }
    }
    all_closed
}

fn collect_array_elements(
    ir: &Arc<FunctionIr>,
    val: ValueId,
    call_stack: &[CallFrame],
    depth: usize,
    visited: &mut FxHashSet<(FunctionIrId, ValueId)>,
    positional_index: Option<usize>,
    out: &mut Vec<(Arc<FunctionIr>, ValueId, Vec<CallFrame>)>,
) -> bool {
    if depth >= MAX_TRACE_DEPTH || !visited.insert((ir.id, val)) {
        return false;
    }

    if let Some(sources) = ir.value_sources.get(&val)
        && !sources.is_empty()
    {
        return sources.iter().all(|&s| {
            collect_array_elements(ir, s, call_stack, depth + 1, visited, positional_index, out)
        });
    }

    let producers = value_producers(ir, val);
    if producers.is_empty() {
        return false;
    }

    let mut all_closed = true;
    for v in producers {
        match &v.op {
            IrOp::ArrayAlloc { elems, .. } => {
                if !collect_array_alloc_elems(ir, elems, call_stack, positional_index, out) {
                    all_closed = false;
                }
            }
            IrOp::ValueForward { source, .. }
            | IrOp::AssignLocal { value: source, .. }
            | IrOp::AssignSymbol { value: source, .. } => {
                if !collect_array_elements(
                    ir,
                    *source,
                    call_stack,
                    depth + 1,
                    visited,
                    positional_index,
                    out,
                ) {
                    all_closed = false;
                }
            }
            IrOp::ReadLocal { binding } => {
                let mut traced = false;
                for wv in &ir.vertices {
                    if let IrOp::AssignLocal { target, value, .. } = wv.op
                        && target == *binding
                    {
                        traced = true;
                        if !collect_array_elements(
                            ir,
                            value,
                            call_stack,
                            depth + 1,
                            visited,
                            positional_index,
                            out,
                        ) {
                            all_closed = false;
                        }
                    }
                }
                if !traced {
                    all_closed = false;
                }
            }
            _ => all_closed = false,
        }
    }
    all_closed
}

#[expect(
    clippy::too_many_arguments,
    reason = "Inter-procedural return value resolution propagating CallFrame parameter bindings and summary return flows."
)]
fn trace_call_return_values(
    caller_ir: &Arc<FunctionIr>,
    inner_call_syntax: Option<SyntaxId>,
    inner_callee: ValueId,
    inner_receiver: Option<ValueId>,
    inner_args: &[IrCallArg],
    call_stack: &[CallFrame],
    depth: usize,
    collector: &mut TraceCollector<'_>,
    expect_constructor: bool,
) {
    if call_stack.len() >= MAX_CALL_STACK_DEPTH {
        collector.push_dynamic_gap("Nested factory call depth exceeded static resolution limit.");
        return;
    }

    let inner_syntax = inner_call_syntax.unwrap_or(collector.call_syntax);

    // Check behavior summaries first: summaries take precedence over source body inspection.
    if try_resolve_call_return_by_summary(
        caller_ir,
        inner_call_syntax,
        inner_callee,
        inner_receiver,
        inner_args,
        call_stack,
        depth,
        collector,
        expect_constructor,
    ) || collector.conflict.is_some()
    {
        return;
    }

    let mut sub_collector = TraceCollector::new(
        inner_syntax,
        collector.call_location.clone(),
        collector.semantic,
        collector.syntax,
        collector.ir_provider,
        collector.summary_index,
    );
    trace_callable_value(
        caller_ir,
        inner_callee,
        call_stack,
        depth + 1,
        None,
        0,
        &mut sub_collector,
    );

    if let Some(conflict) = sub_collector.conflict {
        collector.conflict = Some(conflict);
        return;
    }

    if !sub_collector.is_complete {
        for gap in sub_collector.gaps {
            collector.push_gap(gap);
        }
    }
    collector.evidence.extend(sub_collector.evidence);

    if sub_collector.targets.is_empty() {
        collector.push_dynamic_gap(
            "Intermediate call whose return value is called could not be resolved.",
        );
        return;
    }

    let mut next_stack = call_stack.to_vec();
    next_stack.push(CallFrame {
        caller_ir: Arc::clone(caller_ir),
        args: inner_args.to_vec(),
    });

    for target in sub_collector.targets {
        trace_target_return_values(&target, &next_stack, depth, expect_constructor, collector);
    }
}

fn trace_target_return_values(
    target: &RawTargetDescriptor,
    next_stack: &[CallFrame],
    depth: usize,
    expect_constructor: bool,
    collector: &mut TraceCollector<'_>,
) {
    let Some(scope_syn) = target.fn_scope_syntax else {
        collector.push_gap(GapItem::from_reason(
            UnknownReason::MissingFunctionBody,
            "Called factory declaration has no analyzable body for return resolution.".to_string(),
            collector.call_location.clone(),
        ));
        return;
    };
    let Ok(target_ir) = collector.ir_provider.function_ir(scope_syn) else {
        collector.push_gap(GapItem::from_reason(
            UnknownReason::MissingFunctionBody,
            "Called factory implementation could not be analyzed.".to_string(),
            collector.call_location.clone(),
        ));
        return;
    };

    let mut return_vals: Vec<ValueId> = Vec::new();
    for cv in &target_ir.vertices {
        if let IrOp::Return {
            value: Some(ret_val),
        } = cv.op
        {
            return_vals.push(ret_val);
        }
    }
    if return_vals.is_empty() {
        collector.push_dynamic_gap("Callee factory does not return a value.");
        return;
    }
    for ret_val in return_vals {
        if expect_constructor {
            trace_construct_value(&target_ir, ret_val, next_stack, depth + 1, None, collector);
        } else {
            trace_callable_value(
                &target_ir,
                ret_val,
                next_stack,
                depth + 1,
                None,
                0,
                collector,
            );
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Applies summary-backed bound calls or return value flows for intermediate calls."
)]
fn try_resolve_call_return_by_summary(
    caller_ir: &Arc<FunctionIr>,
    inner_call_syntax: Option<SyntaxId>,
    inner_callee: ValueId,
    inner_receiver: Option<ValueId>,
    inner_args: &[IrCallArg],
    call_stack: &[CallFrame],
    depth: usize,
    collector: &mut TraceCollector<'_>,
    expect_constructor: bool,
) -> bool {
    let Some(id_syn) =
        inner_call_syntax.or_else(|| value_syntax_in_ir(caller_ir, inner_callee, collector.syntax))
    else {
        return false;
    };
    let Some(identity) = collector.semantic.declaration_identity(id_syn) else {
        return false;
    };

    match collector
        .summary_index
        .resolve_calls_summary(&identity, expect_constructor)
    {
        Err(conflict) => {
            collector.conflict = Some(conflict);
            return true;
        }
        Ok(Some(calls_matched))
            if apply_bind_calls_summary(
                &identity,
                &calls_matched,
                caller_ir,
                inner_receiver,
                inner_args,
                call_stack,
                depth,
                expect_constructor,
                collector,
            ) =>
        {
            return true;
        }
        Ok(_) => {}
    }

    match collector
        .summary_index
        .resolve_values_summary(&identity, expect_constructor)
    {
        Err(conflict) => {
            collector.conflict = Some(conflict);
            true
        }
        Ok(Some(values_matched)) => {
            apply_return_values_summary(
                &identity,
                &values_matched,
                caller_ir,
                inner_receiver,
                inner_args,
                call_stack,
                depth,
                expect_constructor,
                collector,
            );
            true
        }
        Ok(None) => false,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Forwards bound callable targets specified by a `calls.forwardsCall` bind summary."
)]
fn apply_bind_calls_summary(
    identity: &DeclarationIdentity,
    calls_matched: &MatchedSummaryAspect<CallAspectSummary>,
    caller_ir: &Arc<FunctionIr>,
    inner_receiver: Option<ValueId>,
    inner_args: &[IrCallArg],
    call_stack: &[CallFrame],
    depth: usize,
    expect_constructor: bool,
    collector: &mut TraceCollector<'_>,
) -> bool {
    let Some(fwd) = &calls_matched.aspect.forwards_call else {
        return false;
    };
    if fwd.mode != ForwardCallMode::Bind {
        return false;
    }
    collector
        .semantic
        .record_summary_lookup(Some(calls_matched.source));
    let target_label = describe_identity_short(identity);
    collector.push_summary_evidence(
        calls_matched.source,
        &calls_matched.pack_id,
        &target_label,
        collector.call_location.clone(),
    );
    let target_val = match fwd.target {
        ForwardCallTarget::Receiver(_) => inner_receiver,
        ForwardCallTarget::Arg { arg_index } => match inner_args.get(arg_index) {
            Some(IrCallArg::Positional(v)) => Some(*v),
            _ => None,
        },
    };
    if let Some(tv) = target_val {
        if expect_constructor {
            trace_construct_value(caller_ir, tv, call_stack, depth + 1, None, collector);
        } else {
            trace_callable_value(
                caller_ir,
                tv,
                call_stack,
                depth + 1,
                None,
                fwd.args_from_index.unwrap_or(0),
                collector,
            );
        }
    } else {
        collector
            .push_dynamic_gap("Forwarded call target in behavior summary could not be resolved.");
    }
    if calls_matched.aspect.completeness == SummaryCompleteness::Partial {
        collector.push_gap(GapItem::from_reason(
            UnknownReason::MissingBehaviorSummary,
            format!(
                "Behavior summary '{}' declares partial 'calls' completeness.",
                calls_matched.pack_id
            ),
            collector.call_location.clone(),
        ));
    }
    true
}

#[expect(
    clippy::too_many_arguments,
    reason = "Applies `values` return flows specified by a summary for an intermediate call."
)]
fn apply_return_values_summary(
    identity: &DeclarationIdentity,
    values_matched: &MatchedSummaryAspect<ValueAspectSummary>,
    caller_ir: &Arc<FunctionIr>,
    inner_receiver: Option<ValueId>,
    inner_args: &[IrCallArg],
    call_stack: &[CallFrame],
    depth: usize,
    expect_constructor: bool,
    collector: &mut TraceCollector<'_>,
) {
    collector
        .semantic
        .record_summary_lookup(Some(values_matched.source));
    let target_label = describe_identity_short(identity);
    collector.push_summary_evidence(
        values_matched.source,
        &values_matched.pack_id,
        &target_label,
        collector.call_location.clone(),
    );
    let mut any_return_flow = false;
    for flow in &values_matched.aspect.flows {
        if flow.to == SummaryValueRef::Named(SummaryNamedValueRef::Return) {
            any_return_flow = true;
            let _ = trace_summary_value_ref(
                caller_ir,
                inner_receiver,
                inner_args,
                &flow.from,
                call_stack,
                depth + 1,
                expect_constructor,
                collector,
            );
        }
    }
    if !any_return_flow || values_matched.aspect.completeness == SummaryCompleteness::Partial {
        collector.push_gap(GapItem::from_reason(
            UnknownReason::MissingBehaviorSummary,
            format!(
                "Behavior summary '{}' for '{}' does not completely specify returned callables.",
                values_matched.pack_id, target_label
            ),
            collector.call_location.clone(),
        ));
    }
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

#[expect(
    clippy::too_many_lines,
    reason = "Matches declaration identity against BehaviorSummaryIndex, records lookup metrics, and applies callback/forwarding rules."
)]
fn try_apply_calls_summary(
    lookup_syntaxes: &[SyntaxId],
    decl_opt: Option<&DeclarationSummaryData>,
    call_ctx: Option<&ActiveCallContext<'_>>,
    depth: usize,
    args_offset: usize,
    collector: &mut TraceCollector<'_>,
) -> bool {
    let is_construct = call_ctx.is_some_and(|c| c.is_construct);
    let mut matched: Option<(DeclarationIdentity, MatchedSummaryAspect<CallAspectSummary>)> = None;
    let mut first_external_or_default_id: Option<DeclarationIdentity> = None;

    for &syn in lookup_syntaxes {
        let Some(identity) = collector.semantic.declaration_identity(syn) else {
            continue;
        };
        if (identity.package_name.is_some() || identity.is_default_library)
            && first_external_or_default_id.is_none()
        {
            first_external_or_default_id = Some(identity.clone());
        }
        match collector
            .summary_index
            .resolve_calls_summary(&identity, is_construct)
        {
            Ok(Some(m)) => {
                matched = Some((identity, m));
                break;
            }
            Ok(None) => {}
            Err(conflict) => {
                collector.conflict = Some(conflict);
                return true;
            }
        }
    }

    let Some((identity, matched_calls)) = matched else {
        if first_external_or_default_id.is_some()
            || decl_opt.is_some_and(|d| d.is_external || d.is_default_library)
        {
            collector.semantic.record_summary_lookup(None);
        }
        return false;
    };

    collector
        .semantic
        .record_summary_lookup(Some(matched_calls.source));
    let target_label = describe_identity_short(&identity);
    let decl_name = decl_opt.and_then(|d| d.name.clone()).or_else(|| {
        identity
            .member_paths
            .first()
            .and_then(|p| p.last().cloned())
    });
    collector.push_summary_evidence(
        matched_calls.source,
        &matched_calls.pack_id,
        &target_label,
        collector.call_location.clone(),
    );

    if let Some(fwd) = &matched_calls.aspect.forwards_call {
        match fwd.mode {
            ForwardCallMode::Bind => {
                if let Some(decl_syn) = decl_opt
                    .and_then(|d| d.primary_syntax)
                    .or_else(|| lookup_syntaxes.first().copied())
                {
                    collector.push_target(RawTargetDescriptor {
                        decl_syntax: decl_syn,
                        fn_scope_syntax: None,
                        symbol: decl_opt.and_then(|d| d.symbol),
                        name: decl_name,
                        location: decl_opt.and_then(|d| d.location.clone()),
                        is_external: decl_opt.is_none_or(|d| d.is_external || d.is_default_library),
                        is_default_library: decl_opt.is_some_and(|d| d.is_default_library),
                        implementation: None,
                        args_offset,
                    });
                }
            }
            ForwardCallMode::Call | ForwardCallMode::Apply => {
                if let Some(ctx) = call_ctx {
                    let target_val = match fwd.target {
                        ForwardCallTarget::Receiver(_) => ctx.receiver,
                        ForwardCallTarget::Arg { arg_index } => match ctx.args.get(arg_index) {
                            Some(IrCallArg::Positional(v)) => Some(*v),
                            _ => None,
                        },
                    };
                    let shifted_offset = if fwd.mode == ForwardCallMode::Call {
                        args_offset + fwd.args_from_index.unwrap_or(0)
                    } else {
                        args_offset
                    };
                    if let Some(tv) = target_val {
                        trace_callable_value(
                            ctx.caller_ir,
                            tv,
                            ctx.call_stack,
                            depth + 1,
                            None,
                            shifted_offset,
                            collector,
                        );
                    } else {
                        collector.push_dynamic_gap(
                            "Forwarded call target in behavior summary has no receiver or argument.",
                        );
                    }
                }
            }
        }
        if matched_calls.aspect.completeness == SummaryCompleteness::Partial {
            collector.push_gap(GapItem::from_reason(
                UnknownReason::MissingBehaviorSummary,
                format!(
                    "Behavior summary '{}' for '{}' declares partial 'calls' completeness.",
                    matched_calls.pack_id, target_label
                ),
                collector.call_location.clone(),
            ));
        }
        return true;
    }

    if let Some(decl_syn) = decl_opt
        .and_then(|d| d.primary_syntax)
        .or_else(|| lookup_syntaxes.first().copied())
    {
        collector.push_target(RawTargetDescriptor {
            decl_syntax: decl_syn,
            fn_scope_syntax: None,
            symbol: decl_opt.and_then(|d| d.symbol),
            name: decl_name,
            location: decl_opt.and_then(|d| d.location.clone()),
            is_external: decl_opt.is_none_or(|d| d.is_external || d.is_default_library),
            is_default_library: decl_opt.is_some_and(|d| d.is_default_library),
            implementation: None,
            args_offset,
        });
    }

    if let Some(ctx) = call_ctx {
        for cb in &matched_calls.aspect.invokes_callbacks {
            if let Some(arg) = ctx.args.get(cb.callback_arg_index) {
                match arg {
                    IrCallArg::Positional(arg_val) => {
                        trace_callable_value(
                            ctx.caller_ir,
                            *arg_val,
                            ctx.call_stack,
                            depth + 1,
                            None,
                            0,
                            collector,
                        );
                    }
                    IrCallArg::Spread(_) => {
                        collector.push_dynamic_gap(
                            "Spread argument passed to summarized callback parameter cannot be statically resolved.",
                        );
                    }
                }
            }
        }
    }

    if matched_calls.aspect.completeness == SummaryCompleteness::Partial {
        collector.push_gap(GapItem::from_reason(
            UnknownReason::MissingBehaviorSummary,
            format!(
                "Behavior summary '{}' for '{}' declares partial 'calls' completeness; additional unknown call targets may exist.",
                matched_calls.pack_id, target_label
            ),
            collector.call_location.clone(),
        ));
    }
    true
}

#[expect(
    clippy::too_many_lines,
    reason = "Resolves callback parameter call targets by tracing enclosing call sites through behavior summaries."
)]
fn resolve_callback_param_from_callers(
    cb_ir: &Arc<FunctionIr>,
    binding: tssae_common::LocalBindingId,
    depth: usize,
    collector: &mut TraceCollector<'_>,
) -> bool {
    let Some(param_idx) = cb_ir
        .parameters
        .iter()
        .position(|p| p.binding == Some(binding))
    else {
        return false;
    };
    let cb_syn = cb_ir.root_syntax;
    let Ok(outer_ir) = collector.ir_provider.enclosing_scope_ir(cb_syn) else {
        return false;
    };
    if outer_ir.id == cb_ir.id {
        return false;
    }

    let is_inline_literal = matches!(
        collector.syntax.syntax_kind(cb_syn).as_deref(),
        Some("ArrowFunction" | "FunctionExpression")
    );
    let mut escapes = !is_inline_literal || cb_ir.decl_semantics.is_exported;

    let mut cb_vals: FxHashSet<ValueId> = FxHashSet::default();
    let mut cb_bindings: FxHashSet<tssae_common::LocalBindingId> = FxHashSet::default();
    for v in &outer_ir.vertices {
        match &v.op {
            IrOp::FunctionLiteral { syntax, .. } if *syntax == cb_syn => {
                if let Some(val) = v.value {
                    cb_vals.insert(val);
                }
            }
            IrOp::ReadSymbol { ident_syntax } => {
                if collector
                    .semantic
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
        return false;
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
                IrOp::ReadLocal { binding: rb } if cb_bindings.contains(rb) => {
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

    for v in &outer_ir.vertices {
        match &v.op {
            IrOp::Return { value: Some(val) } | IrOp::Throw { exception: val }
                if cb_vals.contains(val) =>
            {
                escapes = true;
            }
            IrOp::AssignSymbol { value, .. } | IrOp::PropWrite { value, .. }
                if cb_vals.contains(value) =>
            {
                escapes = true;
            }
            IrOp::ArrayAlloc { elems, .. } => {
                if elems.iter().any(|e| match e {
                    IrArrayElem::Value(val) | IrArrayElem::Spread(val) => cb_vals.contains(val),
                    IrArrayElem::Elision => false,
                }) {
                    escapes = true;
                }
            }
            IrOp::ObjectAlloc { props, .. } => {
                if props.iter().any(|p| match p {
                    IrObjectProp::KeyValue { value, .. } | IrObjectProp::Spread(value) => {
                        cb_vals.contains(value)
                    }
                }) {
                    escapes = true;
                }
            }
            IrOp::Call { args, .. } | IrOp::Construct { args, .. }
                if args
                    .iter()
                    .any(|a| matches!(a, IrCallArg::Spread(val) if cb_vals.contains(val))) =>
            {
                escapes = true;
            }
            _ => {}
        }
    }

    let mut matched_any_call = false;
    let mut all_matched_complete = !escapes;

    for v in &outer_ir.vertices {
        let (outer_callee, outer_receiver, outer_args, is_construct) = match &v.op {
            IrOp::Call {
                callee,
                receiver,
                args,
                ..
            } => (*callee, *receiver, args.as_slice(), false),
            IrOp::Construct { callee, args, .. } => (*callee, None, args.as_slice(), true),
            _ => continue,
        };

        let matching_arg_indices: Vec<usize> = outer_args
            .iter()
            .enumerate()
            .filter_map(|(idx, arg)| match arg {
                IrCallArg::Positional(val) if cb_vals.contains(val) => Some(idx),
                _ => None,
            })
            .collect();
        if matching_arg_indices.is_empty() {
            continue;
        }

        let mut lookup_syns = Vec::new();
        if let Some(cs) = v.syntax {
            lookup_syns.push(cs);
        }
        if let Some(callee_syn) = value_syntax_in_ir(&outer_ir, outer_callee, collector.syntax)
            && !lookup_syns.contains(&callee_syn)
        {
            lookup_syns.push(callee_syn);
        }

        let mut resolved_calls: Option<(
            DeclarationIdentity,
            MatchedSummaryAspect<CallAspectSummary>,
        )> = None;
        let mut resolved_values: Option<(
            DeclarationIdentity,
            MatchedSummaryAspect<ValueAspectSummary>,
        )> = None;
        for &syn in &lookup_syns {
            let Some(id) = collector.semantic.declaration_identity(syn) else {
                continue;
            };
            if resolved_calls.is_none() {
                match collector
                    .summary_index
                    .resolve_calls_summary(&id, is_construct)
                {
                    Ok(Some(m)) => resolved_calls = Some((id.clone(), m)),
                    Ok(None) => {}
                    Err(conflict) => {
                        collector.conflict = Some(conflict);
                        return false;
                    }
                }
            }
            if resolved_values.is_none() {
                match collector
                    .summary_index
                    .resolve_values_summary(&id, is_construct)
                {
                    Ok(Some(m)) => resolved_values = Some((id, m)),
                    Ok(None) => {}
                    Err(conflict) => {
                        collector.conflict = Some(conflict);
                        return false;
                    }
                }
            }
        }

        let (Some((id_calls, calls_matched)), Some((_, values_matched))) =
            (resolved_calls, resolved_values)
        else {
            all_matched_complete = false;
            continue;
        };

        collector
            .semantic
            .record_summary_lookup(Some(calls_matched.source));
        let target_label = describe_identity_short(&id_calls);
        let site_loc = v
            .syntax
            .and_then(|s| collector.syntax.syntax_location(s))
            .or_else(|| collector.call_location.clone());
        collector.push_summary_evidence(
            calls_matched.source,
            &calls_matched.pack_id,
            &target_label,
            site_loc,
        );
        if values_matched.pack_id != calls_matched.pack_id
            || values_matched.source != calls_matched.source
        {
            collector.push_summary_evidence(
                values_matched.source,
                &values_matched.pack_id,
                &target_label,
                collector.call_location.clone(),
            );
        }

        if calls_matched.aspect.completeness != SummaryCompleteness::Complete
            || values_matched.aspect.completeness != SummaryCompleteness::Complete
        {
            all_matched_complete = false;
        }

        for cb_arg_idx in matching_arg_indices {
            if !calls_matched
                .aspect
                .invokes_callbacks
                .iter()
                .any(|cb| cb.callback_arg_index == cb_arg_idx)
            {
                all_matched_complete = false;
                continue;
            }
            let target_ref = SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackParam {
                callback_arg_index: cb_arg_idx,
                param_index: param_idx,
            });
            let mut matched_flow = false;
            for flow in &values_matched.aspect.flows {
                if flow.to == target_ref {
                    matched_flow = true;
                    matched_any_call = true;
                    let flow_complete = trace_summary_value_ref(
                        &outer_ir,
                        outer_receiver,
                        outer_args,
                        &flow.from,
                        &[],
                        depth + 1,
                        false,
                        collector,
                    );
                    if !flow_complete {
                        all_matched_complete = false;
                    }
                }
            }
            if !matched_flow {
                all_matched_complete = false;
            }
        }
    }

    matched_any_call && all_matched_complete
}

#[expect(
    clippy::too_many_arguments,
    reason = "Traces a SummaryValueRef endpoint (receiver, arg, or container elements) to callable targets."
)]
fn trace_summary_value_ref(
    ir: &Arc<FunctionIr>,
    receiver: Option<ValueId>,
    args: &[IrCallArg],
    vref: &SummaryValueRef,
    call_stack: &[CallFrame],
    depth: usize,
    expect_constructor: bool,
    collector: &mut TraceCollector<'_>,
) -> bool {
    match vref {
        SummaryValueRef::Named(SummaryNamedValueRef::Receiver) => {
            let Some(recv_val) = receiver else {
                return false;
            };
            if expect_constructor {
                trace_construct_value(ir, recv_val, call_stack, depth + 1, None, collector);
            } else {
                trace_callable_value(ir, recv_val, call_stack, depth + 1, None, 0, collector);
            }
            true
        }
        SummaryValueRef::Named(SummaryNamedValueRef::ReceiverElement) => {
            let Some(recv_val) = receiver else {
                return false;
            };
            let mut visited = FxHashSet::default();
            trace_container_elements_callable(
                ir,
                recv_val,
                call_stack,
                depth + 1,
                &mut visited,
                collector,
            )
        }
        SummaryValueRef::Tagged(SummaryTaggedValueRef::Arg { index }) => match args.get(*index) {
            Some(IrCallArg::Positional(arg_val)) => {
                if expect_constructor {
                    trace_construct_value(ir, *arg_val, call_stack, depth + 1, None, collector);
                } else {
                    trace_callable_value(ir, *arg_val, call_stack, depth + 1, None, 0, collector);
                }
                true
            }
            _ => false,
        },
        SummaryValueRef::Tagged(SummaryTaggedValueRef::ArgElement { index }) => {
            match args.get(*index) {
                Some(IrCallArg::Positional(arg_val)) => {
                    let mut visited = FxHashSet::default();
                    trace_container_elements_callable(
                        ir,
                        *arg_val,
                        call_stack,
                        depth + 1,
                        &mut visited,
                        collector,
                    )
                }
                _ => false,
            }
        }
        SummaryValueRef::Named(
            SummaryNamedValueRef::Return | SummaryNamedValueRef::ReturnElement,
        )
        | SummaryValueRef::Tagged(
            SummaryTaggedValueRef::CallbackParam { .. }
            | SummaryTaggedValueRef::CallbackReturn { .. }
            | SummaryTaggedValueRef::CallbackParamArg { .. },
        ) => false,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Handles ArrayAlloc, local aliases, and summarized container-producing calls such as Promise.resolve and Promise.all."
)]
fn trace_container_elements_callable(
    ir: &Arc<FunctionIr>,
    container_val: ValueId,
    call_stack: &[CallFrame],
    depth: usize,
    visited: &mut FxHashSet<(FunctionIrId, ValueId)>,
    collector: &mut TraceCollector<'_>,
) -> bool {
    if depth >= MAX_TRACE_DEPTH || !visited.insert((ir.id, container_val)) {
        return false;
    }

    if let Some(sources) = ir.value_sources.get(&container_val)
        && !sources.is_empty()
    {
        let mut all_ok = true;
        for &src in sources {
            if !trace_container_elements_callable(
                ir,
                src,
                call_stack,
                depth + 1,
                visited,
                collector,
            ) {
                all_ok = false;
            }
        }
        return all_ok;
    }

    let producers = value_producers(ir, container_val);
    if producers.is_empty() {
        return false;
    }

    let mut all_complete = true;
    for v in producers {
        match &v.op {
            IrOp::ArrayAlloc { elems, .. } => {
                for elem in elems {
                    match elem {
                        IrArrayElem::Value(ev) => {
                            trace_callable_value(
                                ir,
                                *ev,
                                call_stack,
                                depth + 1,
                                None,
                                0,
                                collector,
                            );
                        }
                        IrArrayElem::Spread(_) | IrArrayElem::Elision => {
                            all_complete = false;
                        }
                    }
                }
            }
            IrOp::ValueForward { source, .. }
            | IrOp::AssignLocal { value: source, .. }
            | IrOp::AssignSymbol { value: source, .. } => {
                if !trace_container_elements_callable(
                    ir,
                    *source,
                    call_stack,
                    depth + 1,
                    visited,
                    collector,
                ) {
                    all_complete = false;
                }
            }
            IrOp::ReadLocal { binding } => {
                let writes = var_writes::resolve_reaching_writes_for_local(
                    ir,
                    v.id,
                    *binding,
                    collector.semantic,
                    collector.syntax,
                    collector.ir_provider,
                );
                if !writes.is_exhaustive || writes.entry_param_reaches || writes.writes.is_empty() {
                    all_complete = false;
                }
                for write in writes.writes {
                    if let VarWriteSource::Value(w_val) = write.source {
                        if !trace_container_elements_callable(
                            &write.scope_ir,
                            w_val,
                            call_stack,
                            depth + 1,
                            visited,
                            collector,
                        ) {
                            all_complete = false;
                        }
                    } else {
                        all_complete = false;
                    }
                }
            }
            IrOp::Call {
                callee: inner_callee,
                receiver: inner_recv,
                args: inner_args,
                ..
            } => {
                let Some(id_syn) = v
                    .syntax
                    .or_else(|| value_syntax_in_ir(ir, *inner_callee, collector.syntax))
                else {
                    all_complete = false;
                    continue;
                };
                let Some(identity) = collector.semantic.declaration_identity(id_syn) else {
                    all_complete = false;
                    continue;
                };
                let Ok(Some(values_matched)) = collector
                    .summary_index
                    .resolve_values_summary(&identity, false)
                else {
                    all_complete = false;
                    continue;
                };
                if values_matched.aspect.completeness != SummaryCompleteness::Complete {
                    all_complete = false;
                }
                let mut matched_ret_elem = false;
                for flow in &values_matched.aspect.flows {
                    if flow.to == SummaryValueRef::Named(SummaryNamedValueRef::ReturnElement) {
                        matched_ret_elem = true;
                        match &flow.from {
                            SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackReturn {
                                callback_arg_index,
                            }) => {
                                if let Some(IrCallArg::Positional(cb_val)) =
                                    inner_args.get(*callback_arg_index)
                                {
                                    if !trace_callback_return_callable(
                                        ir,
                                        *cb_val,
                                        *inner_recv,
                                        call_stack,
                                        depth + 1,
                                        visited,
                                        collector,
                                    ) {
                                        all_complete = false;
                                    }
                                } else {
                                    all_complete = false;
                                }
                            }
                            other => {
                                if !trace_summary_value_ref(
                                    ir,
                                    *inner_recv,
                                    inner_args,
                                    other,
                                    call_stack,
                                    depth + 1,
                                    false,
                                    collector,
                                ) {
                                    all_complete = false;
                                }
                            }
                        }
                    }
                }
                if !matched_ret_elem {
                    all_complete = false;
                }
            }
            IrOp::Construct {
                callee: inner_callee,
                args: inner_args,
                ..
            } => {
                let Some(id_syn) = v
                    .syntax
                    .or_else(|| value_syntax_in_ir(ir, *inner_callee, collector.syntax))
                else {
                    all_complete = false;
                    continue;
                };
                let Some(identity) = collector.semantic.declaration_identity(id_syn) else {
                    all_complete = false;
                    continue;
                };
                let Ok(Some(values_matched)) = collector
                    .summary_index
                    .resolve_values_summary(&identity, true)
                else {
                    all_complete = false;
                    continue;
                };
                if values_matched.aspect.completeness != SummaryCompleteness::Complete {
                    all_complete = false;
                }
                let mut matched_ret_elem = false;
                for flow in &values_matched.aspect.flows {
                    if flow.to == SummaryValueRef::Named(SummaryNamedValueRef::ReturnElement)
                        && let SummaryValueRef::Tagged(SummaryTaggedValueRef::CallbackParamArg {
                            callback_arg_index,
                            callback_param_index,
                            arg_index,
                        }) = &flow.from
                    {
                        matched_ret_elem = true;
                        if let Some(IrCallArg::Positional(exec_val)) =
                            inner_args.get(*callback_arg_index)
                        {
                            if !trace_executor_param_call_args(
                                ir,
                                *exec_val,
                                *callback_param_index,
                                *arg_index,
                                depth + 1,
                                collector,
                            ) {
                                all_complete = false;
                            }
                        } else {
                            all_complete = false;
                        }
                    }
                }
                if !matched_ret_elem {
                    all_complete = false;
                }
            }
            _ => all_complete = false,
        }
    }
    all_complete
}

fn trace_callback_return_callable(
    outer_ir: &Arc<FunctionIr>,
    cb_val: ValueId,
    outer_recv: Option<ValueId>,
    call_stack: &[CallFrame],
    depth: usize,
    visited: &mut FxHashSet<(FunctionIrId, ValueId)>,
    collector: &mut TraceCollector<'_>,
) -> bool {
    let Some(fn_syn) = value_producers(outer_ir, cb_val)
        .into_iter()
        .find_map(|v| match v.op {
            IrOp::FunctionLiteral { syntax, .. } => Some(syntax),
            _ => None,
        })
    else {
        return false;
    };
    let Ok(cb_ir) = collector.ir_provider.function_ir(fn_syn) else {
        return false;
    };
    let mut all_ok = true;
    let mut has_return = false;
    for rv in &cb_ir.vertices {
        if let IrOp::Return {
            value: Some(ret_val),
        } = rv.op
        {
            has_return = true;
            // If the callback returns its first parameter (`x => x`), propagate outer receiver elements.
            let returns_param0 = cb_ir
                .parameters
                .first()
                .and_then(|p| p.binding)
                .is_some_and(|b| {
                    value_producers(&cb_ir, ret_val)
                        .iter()
                        .any(|pv| matches!(pv.op, IrOp::ReadLocal { binding } if binding == b))
                });
            if returns_param0 {
                if let Some(recv_val) = outer_recv {
                    if !trace_container_elements_callable(
                        outer_ir,
                        recv_val,
                        call_stack,
                        depth + 1,
                        visited,
                        collector,
                    ) {
                        all_ok = false;
                    }
                } else {
                    all_ok = false;
                }
            } else {
                trace_callable_value(&cb_ir, ret_val, &[], depth + 1, None, 0, collector);
            }
        }
    }
    has_return && all_ok
}

fn trace_executor_param_call_args(
    outer_ir: &Arc<FunctionIr>,
    exec_val: ValueId,
    callback_param_index: usize,
    arg_index: usize,
    depth: usize,
    collector: &mut TraceCollector<'_>,
) -> bool {
    let Some(fn_syn) = value_producers(outer_ir, exec_val)
        .into_iter()
        .find_map(|v| match v.op {
            IrOp::FunctionLiteral { syntax, .. } => Some(syntax),
            _ => None,
        })
    else {
        return false;
    };
    let Ok(exec_ir) = collector.ir_provider.function_ir(fn_syn) else {
        return false;
    };
    let Some(param_binding) = exec_ir
        .parameters
        .get(callback_param_index)
        .and_then(|p| p.binding)
    else {
        return false;
    };
    let mut param_vals: FxHashSet<ValueId> = FxHashSet::default();
    for v in &exec_ir.vertices {
        if let IrOp::ReadLocal { binding } = v.op
            && binding == param_binding
            && let Some(val) = v.value
        {
            param_vals.insert(val);
        }
    }
    let mut called = false;
    let mut all_ok = true;
    for v in &exec_ir.vertices {
        if let IrOp::Call { callee, args, .. } = &v.op
            && param_vals.contains(callee)
        {
            called = true;
            if let Some(IrCallArg::Positional(arg_val)) = args.get(arg_index) {
                trace_callable_value(&exec_ir, *arg_val, &[], depth + 1, None, 0, collector);
            } else {
                all_ok = false;
            }
        }
    }
    called && all_ok
}

fn enrich_target_descriptor(
    raw: &RawTargetDescriptor,
    arg_syntaxes: &[(SyntaxId, bool)],
    semantic: &dyn SemanticModel,
    ir_provider: &dyn IrProvider,
) -> ResolvedCallTargetData {
    let mut parameter_bindings = Vec::new();
    let mut return_expressions = Vec::new();

    if let Some(scope_syn) = raw.fn_scope_syntax
        && let Ok(target_ir) = ir_provider.function_ir(scope_syn)
    {
        let sliced_args = &arg_syntaxes[raw.args_offset.min(arg_syntaxes.len())..];
        for (idx, &(arg_syn, is_spread)) in sliced_args.iter().enumerate() {
            if target_ir.parameters.is_empty() {
                break;
            }
            let last_is_rest = target_ir.parameters.last().is_some_and(|p| p.is_rest);
            let param_opt = if idx < target_ir.parameters.len() {
                Some(&target_ir.parameters[idx])
            } else if last_is_rest {
                target_ir.parameters.last()
            } else {
                None
            };
            if let Some(param) = param_opt {
                parameter_bindings.push(CallParameterBindingData {
                    argument_index: idx + raw.args_offset,
                    argument_syntax: arg_syn,
                    parameter_name: Some(param.name.clone()),
                    parameter_decl_syntax: Some(param.syntax),
                    is_rest: param.is_rest || is_spread,
                });
            }
        }

        let mut seen_ret = FxHashSet::default();
        for v in &target_ir.vertices {
            if let IrOp::Return {
                value: Some(ret_val),
            } = v.op
            {
                let ret_syn = return_expr_syntax_for_vertex(&target_ir, v, ret_val);
                if let Some(syn) = ret_syn
                    && seen_ret.insert(syn)
                {
                    return_expressions.push(syn);
                }
            }
        }
    }

    let _ = semantic;
    ResolvedCallTargetData {
        declaration_syntax: raw.decl_syntax,
        symbol: raw.symbol,
        name: raw.name.clone(),
        location: raw.location.clone(),
        is_external: raw.is_external,
        is_default_library: raw.is_default_library,
        fn_scope_syntax: raw.fn_scope_syntax,
        implementation: raw.implementation.clone(),
        parameter_bindings,
        return_expressions,
    }
}

fn return_expr_syntax_for_vertex(
    ir: &FunctionIr,
    ret_vertex: &CfgVertex,
    ret_val: ValueId,
) -> Option<SyntaxId> {
    if let Some(v) = ir.vertices.iter().rfind(|v| {
        v.value == Some(ret_val)
            && v.syntax.is_some()
            && !matches!(
                v.op,
                IrOp::AssignLocal { is_init: true, .. } | IrOp::AssignSymbol { is_init: true, .. }
            )
    }) {
        return v.syntax;
    }
    if let Some(sources) = ir.value_sources.get(&ret_val)
        && let Some(&first) = sources.first()
    {
        return return_expr_syntax_for_vertex(ir, ret_vertex, first);
    }
    ret_vertex.syntax
}

/// Indexes all call and construct sites across `project_files` into an [`IrCallGraph`].
#[must_use]
pub fn build_ir_call_graph(
    project_files: &[FileId],
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
) -> IrCallGraph {
    let default_index = BehaviorSummaryIndex::default();
    build_ir_call_graph_with_summaries(project_files, semantic, syntax, ir_provider, &default_index)
}

/// Indexes all call and construct sites across `project_files` using `summary_index`.
#[must_use]
pub fn build_ir_call_graph_with_summaries(
    project_files: &[FileId],
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    summary_index: &BehaviorSummaryIndex,
) -> IrCallGraph {
    let mut graph = IrCallGraph::default();
    let mut seen_call_syntaxes: FxHashSet<SyntaxId> = FxHashSet::default();

    for &file_id in project_files {
        let scopes = ir_provider.scopes_in_file(file_id);
        let mut loaded_scopes = Vec::with_capacity(scopes.len());
        for scope_entry in &scopes {
            if let Ok(scope_ir) = ir_provider.function_ir(scope_entry.root_syntax) {
                loaded_scopes.push(scope_ir);
            }
        }
        collect_escaped_callables_in_file(&loaded_scopes, semantic, syntax, &mut graph);

        for scope_ir in &loaded_scopes {
            for vertex in &scope_ir.vertices {
                if !matches!(vertex.op, IrOp::Call { .. } | IrOp::Construct { .. }) {
                    continue;
                }
                let Some(call_syn) = vertex.syntax else {
                    continue;
                };
                if !seen_call_syntaxes.insert(call_syn) {
                    continue;
                }
                let Some(loc) = syntax
                    .syntax_location(call_syn)
                    .or_else(|| vertex.location.clone())
                else {
                    continue;
                };

                let analyzed = analyze_call_vertex_with_scope(
                    call_syn,
                    Some((scope_ir, vertex)),
                    semantic,
                    syntax,
                    ir_provider,
                    summary_index,
                );
                if let Some(conflict) = analyzed.outcome.conflict {
                    graph.conflict = Some(conflict);
                    return graph;
                }
                let site_idx = graph.sites.len();
                let mut seen_decls = FxHashSet::default();
                let mut seen_syms = FxHashSet::default();
                for t in &analyzed.outcome.targets {
                    if seen_decls.insert(t.declaration_syntax) {
                        graph
                            .by_decl
                            .entry(t.declaration_syntax)
                            .or_default()
                            .push(site_idx);
                    }
                    if let Some(sym) = t.symbol
                        && seen_syms.insert(sym)
                    {
                        graph.by_symbol.entry(sym).or_default().push(site_idx);
                    }
                }
                if analyzed.is_untyped_dynamic_incomplete || analyzed.open_member_name.is_some() {
                    graph.incomplete_sites.push(site_idx);
                }
                graph.sites.push(IndexedCallEntry {
                    call_syntax: call_syn,
                    file: file_id,
                    location: loc,
                    caller_decl_syntax: analyzed.caller_decl_syntax,
                    caller_symbol: analyzed.caller_symbol,
                    resolution: analyzed.outcome,
                    open_member_name: analyzed.open_member_name,
                    is_untyped_dynamic_incomplete: analyzed.is_untyped_dynamic_incomplete,
                });
            }
        }
    }

    graph
}

fn mark_escaped_syntax(syn: SyntaxId, semantic: &dyn SemanticModel, graph: &mut IrCallGraph) {
    graph.escaped_decls.insert(syn);
    if let Some(sym) = semantic.symbol_at_syntax(syn) {
        graph.escaped_symbols.insert(sym);
    }
    if let Some(decl) = semantic.resolve_declaration(syn) {
        if let Some(primary) = decl.primary_syntax {
            graph.escaped_decls.insert(primary);
        }
        if let Some(callable) = decl.callable_syntax {
            graph.escaped_decls.insert(callable);
        }
        if let Some(sym) = decl.symbol {
            graph.escaped_symbols.insert(sym);
        }
    }
}

fn extract_export_decl_local_names(text: &str) -> Vec<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with("export type") || trimmed.contains(" from ") {
        return Vec::new();
    }
    let Some(open) = trimmed.find('{') else {
        return Vec::new();
    };
    let Some(close) = trimmed[open + 1..].find('}') else {
        return Vec::new();
    };
    let body = &trimmed[open + 1..open + 1 + close];
    let mut names = Vec::new();
    for part in body.split(',') {
        let spec = part.trim();
        if spec.is_empty() || spec.starts_with("type ") || spec.starts_with("type\t") {
            continue;
        }
        let local = spec.split(" as ").next().unwrap_or(spec).trim();
        if !local.is_empty()
            && local
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        {
            names.push(local);
        }
    }
    names
}

fn mark_prop_key_used(key: &IrPropKey, used: &mut FxHashSet<ValueId>) {
    if let IrPropKey::Computed { key: k, .. } = key {
        used.insert(*k);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Exhaustively collects all non-callee ValueId operands across every IrOp variant."
)]
fn collect_non_callee_used_values(scope_ir: &FunctionIr) -> FxHashSet<ValueId> {
    let mut used = FxHashSet::default();
    for v in &scope_ir.vertices {
        match &v.op {
            IrOp::Entry
            | IrOp::Exit
            | IrOp::NopMerge
            | IrOp::Literal { .. }
            | IrOp::ReadLocal { .. }
            | IrOp::ReadSymbol { .. }
            | IrOp::ThisRef
            | IrOp::SuperRef
            | IrOp::MetaProp
            | IrOp::CatchParamBind { .. }
            | IrOp::FunctionLiteral { .. }
            | IrOp::ClassLiteral { .. }
            | IrOp::ValueForward { .. } => {}
            IrOp::TemplateString { spans } => used.extend(spans.iter().copied()),
            IrOp::Unary { operand, .. } => {
                used.insert(*operand);
            }
            IrOp::Binary { left, right, .. } => {
                used.insert(*left);
                used.insert(*right);
            }
            IrOp::AssignLocal { value, .. } | IrOp::AssignSymbol { value, .. } => {
                used.insert(*value);
            }
            IrOp::PropWrite {
                object, key, value, ..
            } => {
                used.insert(*object);
                mark_prop_key_used(key, &mut used);
                used.insert(*value);
            }
            IrOp::PropRead { object, key, .. } => {
                used.insert(*object);
                mark_prop_key_used(key, &mut used);
            }
            IrOp::ObjectRest { source, .. }
            | IrOp::Branch { condition: source }
            | IrOp::SwitchDispatch {
                discriminant: source,
            }
            | IrOp::Throw { exception: source }
            | IrOp::Await { promise: source }
            | IrOp::IteratorInit {
                iterable: source, ..
            }
            | IrOp::IteratorNext {
                iterator: source, ..
            }
            | IrOp::IteratorRest {
                iterator: source, ..
            }
            | IrOp::IteratorClose {
                iterator: source, ..
            } => {
                used.insert(*source);
            }
            IrOp::Call { receiver, args, .. } => {
                if let Some(recv) = receiver {
                    used.insert(*recv);
                }
                for arg in args {
                    match arg {
                        IrCallArg::Positional(a) | IrCallArg::Spread(a) => {
                            used.insert(*a);
                        }
                    }
                }
            }
            IrOp::Construct { args, .. } => {
                for arg in args {
                    match arg {
                        IrCallArg::Positional(a) | IrCallArg::Spread(a) => {
                            used.insert(*a);
                        }
                    }
                }
            }
            IrOp::ObjectAlloc { props, .. } => {
                for prop in props {
                    match prop {
                        IrObjectProp::KeyValue { key, value } => {
                            mark_prop_key_used(key, &mut used);
                            used.insert(*value);
                        }
                        IrObjectProp::Spread(s) => {
                            used.insert(*s);
                        }
                    }
                }
            }
            IrOp::ArrayAlloc { elems, .. } => {
                for elem in elems {
                    if let IrArrayElem::Value(val) | IrArrayElem::Spread(val) = elem {
                        used.insert(*val);
                    }
                }
            }
            IrOp::JsxElement {
                tag_expr,
                props,
                children,
                ..
            } => {
                if let Some(tag) = tag_expr {
                    used.insert(*tag);
                }
                for prop in props {
                    match prop {
                        IrObjectProp::KeyValue { key, value } => {
                            mark_prop_key_used(key, &mut used);
                            used.insert(*value);
                        }
                        IrObjectProp::Spread(s) => {
                            used.insert(*s);
                        }
                    }
                }
                used.extend(children.iter().copied());
            }
            IrOp::Return { value } | IrOp::Yield { value, .. } => {
                if let Some(val) = value {
                    used.insert(*val);
                }
            }
            IrOp::Unknown {
                evaluated_children, ..
            } => used.extend(evaluated_children.iter().copied()),
        }
    }

    // Propagate non-callee usage backwards through `value_sources` and `ValueForward` wrappers.
    loop {
        let mut changed = false;
        for (&merged, sources) in &scope_ir.value_sources {
            if used.contains(&merged) {
                for &src in sources {
                    if used.insert(src) {
                        changed = true;
                    }
                }
            }
        }
        for v in &scope_ir.vertices {
            if let IrOp::ValueForward { source, .. } = v.op
                && v.value.is_some_and(|val| used.contains(&val))
                && used.insert(source)
            {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    used
}

fn collect_escaped_callables_in_file(
    scopes: &[Arc<FunctionIr>],
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    graph: &mut IrCallGraph,
) {
    let mut exported_names: FxHashSet<String> = FxHashSet::default();

    for scope_ir in scopes {
        if scope_ir.decl_semantics.is_exported || scope_ir.decl_semantics.is_default_export {
            mark_escaped_syntax(scope_ir.root_syntax, semantic, graph);
        }

        if scope_ir.scope_kind == IrScopeKind::ModuleTopLevel {
            for v in &scope_ir.vertices {
                if let Some(syn) = v.syntax
                    && syntax.syntax_kind(syn).as_deref() == Some("ExportDeclaration")
                    && let Some(text) = syntax.syntax_text(syn)
                {
                    for name in extract_export_decl_local_names(&text) {
                        exported_names.insert(name.to_string());
                    }
                }
            }
        }

        let non_callee_used = collect_non_callee_used_values(scope_ir);
        for v in &scope_ir.vertices {
            let Some(val) = v.value else {
                continue;
            };
            if !non_callee_used.contains(&val) {
                continue;
            }
            match &v.op {
                IrOp::FunctionLiteral { syntax: fn_syn, .. }
                | IrOp::ClassLiteral { syntax: fn_syn } => {
                    mark_escaped_syntax(*fn_syn, semantic, graph);
                }
                IrOp::ReadLocal { binding } => {
                    if let Some(b) = scope_ir.bindings.get(binding.0 as usize) {
                        graph.escaped_decls.insert(b.syntax);
                        if let Some(sym) = b.symbol {
                            graph.escaped_symbols.insert(sym);
                        }
                        mark_escaped_syntax(b.syntax, semantic, graph);
                    }
                }
                IrOp::ReadSymbol { ident_syntax } => {
                    mark_escaped_syntax(*ident_syntax, semantic, graph);
                }
                IrOp::PropRead { .. } => {
                    if let Some(prop_syn) = v.syntax {
                        mark_escaped_syntax(prop_syn, semantic, graph);
                    }
                }
                _ => {}
            }
        }
    }

    if !exported_names.is_empty() {
        for scope_ir in scopes {
            if scope_ir.scope_kind == IrScopeKind::ModuleTopLevel {
                for b in &scope_ir.bindings {
                    if exported_names.contains(&b.name) {
                        graph.escaped_decls.insert(b.syntax);
                        if let Some(sym) = b.symbol {
                            graph.escaped_symbols.insert(sym);
                        }
                        mark_escaped_syntax(b.syntax, semantic, graph);
                    }
                }
            } else if let Some(decl) = semantic.resolve_declaration(scope_ir.root_syntax)
                && decl
                    .name
                    .as_deref()
                    .is_some_and(|n| exported_names.contains(n))
            {
                mark_escaped_syntax(scope_ir.root_syntax, semantic, graph);
            }
        }
    }
}

fn target_may_escape_as_value(
    target_decl_syntax: SyntaxId,
    target_symbol: Option<SymbolId>,
    target_summary: Option<&DeclarationSummaryData>,
    graph: &IrCallGraph,
) -> bool {
    if graph.escaped_decls.contains(&target_decl_syntax) {
        return true;
    }
    if let Some(sym) = target_symbol
        && graph.escaped_symbols.contains(&sym)
    {
        return true;
    }
    if let Some(summary) = target_summary {
        if summary.is_external {
            return true;
        }
        if matches!(
            summary.kind.as_deref(),
            Some("ExportAssignment" | "ExportSpecifier")
        ) {
            return true;
        }
        if summary
            .primary_syntax
            .is_some_and(|s| graph.escaped_decls.contains(&s))
        {
            return true;
        }
        if summary
            .callable_syntax
            .is_some_and(|s| graph.escaped_decls.contains(&s))
        {
            return true;
        }
        if summary
            .symbol
            .is_some_and(|s| graph.escaped_symbols.contains(&s))
        {
            return true;
        }
    }
    false
}

/// Resolves all call sites inside `allowed_files` that target `target_decl_syntax` (or `target_symbol`),
/// reporting `completeness: false` with structural gaps if unexcluded incomplete sites remain.
#[must_use]
pub fn find_callers_on_ir<S: BuildHasher>(
    target_decl_syntax: SyntaxId,
    target_symbol: Option<SymbolId>,
    graph: &IrCallGraph,
    semantic: &dyn SemanticModel,
    allowed_files: Option<&HashSet<FileId, S>>,
) -> CallersOutcome {
    let target_summary = semantic.resolve_declaration(target_decl_syntax);
    if target_summary.as_ref().is_some_and(|d| d.is_signature_only) {
        return CallersOutcome {
            call_sites: Vec::new(),
            gaps: vec![GapItem::from_reason(
                UnknownReason::DynamicCallee,
                "Target declaration is a type signature without a runtime function body."
                    .to_string(),
                target_summary.as_ref().and_then(|d| d.location.clone()),
            )],
            is_complete: false,
            evidence: Vec::new(),
        };
    }

    let mut matched_indices: BTreeSet<usize> = BTreeSet::new();
    if let Some(indices) = graph.by_decl.get(&target_decl_syntax) {
        matched_indices.extend(indices.iter().copied());
    }
    if let Some(sym) = target_symbol
        && let Some(indices) = graph.by_symbol.get(&sym)
    {
        matched_indices.extend(indices.iter().copied());
    }

    let mut call_sites = Vec::new();
    let mut evidence = Vec::new();
    for &idx in &matched_indices {
        let site = &graph.sites[idx];
        if let Some(allowed) = allowed_files
            && !allowed.contains(&site.file)
        {
            continue;
        }
        let matching_target = site.resolution.targets.iter().find(|t| {
            t.declaration_syntax == target_decl_syntax
                || (target_symbol.is_some() && t.symbol == target_symbol)
        });
        let parameter_bindings = matching_target.map_or_default(|t| t.parameter_bindings.clone());
        evidence.push(EvidenceItem::simple(
            "source",
            "Matched call site invoking target declaration.".to_string(),
            Some(site.location.clone()),
        ));
        if let Some(last) = evidence.last_mut() {
            last.declaration = target_summary.as_ref().and_then(|d| d.name.clone());
        }
        call_sites.push(CallerSiteData {
            call_syntax: site.call_syntax,
            location: site.location.clone(),
            caller_decl_syntax: site.caller_decl_syntax,
            caller_symbol: site.caller_symbol,
            parameter_bindings,
        });
    }

    let mut gaps = Vec::new();
    let mut seen_gap_keys: FxHashSet<(&'static str, String)> = FxHashSet::default();
    let target_name = target_summary.as_ref().and_then(|d| d.name.as_deref());
    let target_is_method = target_summary.as_ref().is_some_and(|d| {
        matches!(
            d.kind.as_deref(),
            Some("MethodDeclaration" | "GetAccessor" | "SetAccessor" | "PropertyDeclaration")
        )
    });

    for &idx in &graph.incomplete_sites {
        let site = &graph.sites[idx];
        if let Some(allowed) = allowed_files
            && !allowed.contains(&site.file)
        {
            continue;
        }
        // Check if this incomplete site could invoke `target_decl_syntax`.
        let could_invoke_target = if matched_indices.contains(&idx) {
            true
        } else if site.is_untyped_dynamic_incomplete {
            // An unresolved callback/dynamic call within scope can only call targets that escape as values.
            target_may_escape_as_value(
                target_decl_syntax,
                target_symbol,
                target_summary.as_ref(),
                graph,
            )
        } else if let Some(open_member) = &site.open_member_name {
            // Open polymorphic member call can target a method/accessor of the same member name.
            target_is_method && target_name == Some(open_member.as_str())
        } else {
            false
        };

        if !could_invoke_target {
            continue;
        }

        for gap in &site.resolution.gaps {
            let key = (gap.kind, gap.message.clone());
            if seen_gap_keys.insert(key) {
                gaps.push(gap.clone());
            }
        }
    }

    let is_complete = gaps.is_empty();
    CallersOutcome {
        call_sites,
        gaps,
        is_complete,
        evidence,
    }
}

pub(crate) fn prop_key_static_name(key: &IrPropKey) -> Option<&str> {
    match key {
        IrPropKey::Named(name) | IrPropKey::PrivateIdent(name) => Some(name.as_str()),
        IrPropKey::Computed { static_name, .. } => static_name.as_deref(),
    }
}

fn stack_fingerprint(stack: &[CallFrame]) -> usize {
    stack.iter().fold(0usize, |acc, frame| {
        acc.wrapping_mul(31) ^ (frame.caller_ir.id.local_scope as usize) ^ frame.args.len()
    })
}

fn extract_fn_name_heuristic(syntax: &dyn SyntaxCatalog, syn: SyntaxId) -> Option<String> {
    let text = syntax.syntax_text(syn)?;
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("function") {
        let rest = rest.trim_start();
        let ident_len = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
            .unwrap_or(0);
        if ident_len > 0 {
            return Some(rest[..ident_len].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use tssae_common::{
        ClassOrInterfaceKind, ImplementationFact, LocalBindingId, LocalSyntaxId,
        MemberTargetHierarchy, SignatureId, TypeHierarchyNode, TypeId, TypeSummaryData, VertexId,
    };

    use super::*;
    use crate::ir::{
        BindingMutability, CfgVertex, DeclarationSemantics, ExplicitEdge, ExplicitEdgeKind,
        IrLocalBinding, IrParameter, IrScopeKind, ScopeCatalogEntry,
    };

    struct TestEnv {
        irs: FxHashMap<SyntaxId, Arc<FunctionIr>>,
        enclosing: FxHashMap<SyntaxId, SyntaxId>,
        decls: FxHashMap<SyntaxId, DeclarationSummaryData>,
        hierarchies: FxHashMap<SyntaxId, MemberTargetHierarchy>,
        locations: FxHashMap<SyntaxId, DiagnosticLocation>,
        kinds: FxHashMap<SyntaxId, String>,
    }

    impl TestEnv {
        fn new() -> Self {
            Self {
                irs: FxHashMap::default(),
                enclosing: FxHashMap::default(),
                decls: FxHashMap::default(),
                hierarchies: FxHashMap::default(),
                locations: FxHashMap::default(),
                kinds: FxHashMap::default(),
            }
        }
    }

    impl SyntaxCatalog for TestEnv {
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

    impl SemanticModel for TestEnv {
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
            receiver_syntax: SyntaxId,
            _member_name: Option<&str>,
            _static_receiver: bool,
        ) -> Option<MemberTargetHierarchy> {
            self.hierarchies.get(&receiver_syntax).cloned()
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
                    .unwrap_or_else(|| DiagnosticLocation {
                        file: "test.ts".to_string(),
                        line: 1,
                        column: 1,
                        end_line: 1,
                        end_column: 1,
                        start: 0,
                        end: 1,
                    }),
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
    }

    impl IrProvider for TestEnv {
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
                    span_end: 100,
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
        reason = "Constructs an explicit synthetic FunctionIr CFG with a parameter call vertex."
    )]
    fn param_callback_never_falls_back_to_signature() {
        let mut env = TestEnv::new();
        let fn_syn = syn(10);
        let call_syn = syn(20);
        let param_syn = syn(11);
        let b0 = LocalBindingId(0);

        env.decls.insert(
            param_syn,
            DeclarationSummaryData {
                symbol: Some(SymbolId(11)),
                primary_syntax: Some(param_syn),
                name: Some("f".to_string()),
                container_name: None,
                kind: Some("Parameter".to_string()),
                location: Some(loc(1)),
                is_external: false,
                is_default_library: false,
                has_body: false,
                is_signature_only: true,
                initializer_syntax: None,
                callable_syntax: None,
            },
        );

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
            parameters: vec![IrParameter {
                name: "f".to_string(),
                syntax: param_syn,
                binding: Some(b0),
                raw_value: None,
                is_rest: false,
                has_default: false,
            }],
            bindings: vec![IrLocalBinding {
                id: b0,
                name: "f".to_string(),
                mutability: BindingMutability::Parameter,
                syntax: param_syn,
                symbol: Some(SymbolId(11)),
            }],
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
                    None,
                    IrOp::ReadLocal { binding: b0 },
                    Some(ValueId(0)),
                    vec![ExplicitEdge {
                        target: VertexId(5),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    5,
                    Some(call_syn),
                    IrOp::Call {
                        call_kind: IrCallKind::Explicit,
                        callee: ValueId(0),
                        receiver: None,
                        args: Vec::new(),
                    },
                    Some(ValueId(1)),
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
            expr_to_value: FxHashMap::default(),
            value_sources: FxHashMap::default(),
            branches: Vec::new(),
            value_count: 2,
        });
        env.irs.insert(fn_syn, ir);
        env.enclosing.insert(call_syn, fn_syn);
        env.locations.insert(call_syn, loc(2));

        let outcome = resolve_call_targets_on_ir(call_syn, &env, &env, &env);
        assert_eq!(outcome.targets.len(), 0);
        assert!(!outcome.is_complete);
        assert!(outcome.gaps.iter().any(|g| g.kind == "dynamic-callee"));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "Constructs two synthetic hierarchy nodes and FunctionIr to verify polymorphic completeness."
    )]
    fn exported_base_class_override_keeps_targets_and_marks_incomplete() {
        let mut env = TestEnv::new();
        let fn_syn = syn(10);
        let call_syn = syn(20);
        let recv_syn = syn(21);
        let base_method = syn(101);
        let sub_method = syn(201);
        let base_sym = SymbolId(100);
        let sub_sym = SymbolId(200);

        for (msyn, mname) in [(base_method, "run"), (sub_method, "run")] {
            env.decls.insert(
                msyn,
                DeclarationSummaryData {
                    symbol: Some(SymbolId(u64::from(msyn.local_id().0))),
                    primary_syntax: Some(msyn),
                    name: Some(mname.to_string()),
                    container_name: None,
                    kind: Some("MethodDeclaration".to_string()),
                    location: Some(loc(msyn.local_id().0)),
                    is_external: false,
                    is_default_library: false,
                    has_body: true,
                    is_signature_only: false,
                    initializer_syntax: None,
                    callable_syntax: Some(msyn),
                },
            );
            env.locations.insert(msyn, loc(msyn.local_id().0));
        }

        let mk_node = |syntax, symbol, name: &str, bases, member_syn| TypeHierarchyNode {
            syntax,
            symbol,
            name: Some(name.to_string()),
            kind: ClassOrInterfaceKind::Class,
            is_exported: true,
            is_abstract: false,
            is_external: false,
            base_symbols: bases,
            members: vec![tssae_common::ClassMemberFact {
                name: "run".to_string(),
                member_syntax: member_syn,
                symbol: Some(SymbolId(u64::from(member_syn.local_id().0))),
                kind: "MethodDeclaration".to_string(),
                location: Some(loc(member_syn.local_id().0)),
                has_body: true,
                is_abstract: false,
                is_static: false,
                is_private: false,
                is_private_ecma: false,
                is_override: false,
                initializer_syntax: None,
                callable_syntax: Some(member_syn),
            }],
            constructor_syntax: None,
        };

        env.hierarchies.insert(
            recv_syn,
            MemberTargetHierarchy {
                receiver_kind: ClassOrInterfaceKind::Class,
                receiver_symbols: vec![base_sym],
                receiver_name: Some("Base".to_string()),
                receiver_is_exported: true,
                receiver_is_abstract: false,
                receiver_is_external: false,
                is_union_or_intersection: false,
                has_unresolved_type: false,
                candidate_classes: vec![
                    mk_node(syn(100), base_sym, "Base", Vec::new(), base_method),
                    mk_node(syn(200), sub_sym, "Sub", vec![base_sym], sub_method),
                ],
            },
        );

        let b0 = LocalBindingId(0);
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
            parameters: vec![IrParameter {
                name: "b".to_string(),
                syntax: recv_syn,
                binding: Some(b0),
                raw_value: None,
                is_rest: false,
                has_default: false,
            }],
            bindings: vec![IrLocalBinding {
                id: b0,
                name: "b".to_string(),
                mutability: BindingMutability::Parameter,
                syntax: recv_syn,
                symbol: Some(SymbolId(21)),
            }],
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
                    Some(recv_syn),
                    IrOp::ReadLocal { binding: b0 },
                    Some(ValueId(0)),
                    vec![ExplicitEdge {
                        target: VertexId(5),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    5,
                    None,
                    IrOp::PropRead {
                        object: ValueId(0),
                        key: IrPropKey::Named("run".to_string()),
                        is_super: false,
                        implicit_call_kind: None,
                    },
                    Some(ValueId(1)),
                    vec![ExplicitEdge {
                        target: VertexId(6),
                        kind: ExplicitEdgeKind::Fallthrough,
                    }],
                ),
                mk_vertex(
                    6,
                    Some(call_syn),
                    IrOp::Call {
                        call_kind: IrCallKind::Explicit,
                        callee: ValueId(1),
                        receiver: Some(ValueId(0)),
                        args: Vec::new(),
                    },
                    Some(ValueId(2)),
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
            expr_to_value: FxHashMap::default(),
            value_sources: FxHashMap::default(),
            branches: Vec::new(),
            value_count: 3,
        });
        env.irs.insert(fn_syn, ir);
        env.enclosing.insert(call_syn, fn_syn);
        env.locations.insert(call_syn, loc(2));

        let outcome = resolve_call_targets_on_ir(call_syn, &env, &env, &env);
        assert_eq!(outcome.targets.len(), 2);
        assert!(!outcome.is_complete);
        assert!(
            outcome
                .gaps
                .iter()
                .any(|g| g.kind == "open-external-hierarchy")
        );
    }
}
