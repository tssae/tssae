use std::{collections::VecDeque, sync::Arc};

use rustc_hash::FxHashSet;
use tssae_common::{
    BranchId, CallbackTiming, DiagnosticLocation, EvidenceItem, FileId, ForwardCallTarget,
    FunctionIrId, GapItem, SemanticModel, SummaryConflictError, SummaryThisArg, SymbolId,
    SyntaxCatalog, SyntaxId, UnknownReason, ValueId, VertexId,
};

use crate::{
    branch_coverage::{StackArena, StackId},
    call_resolution::{IrCallGraph, resolve_call_targets_with_summaries},
    instance_identity::{
        IrCallFrame, IrInstanceResolver, ReceiverBinding, VisitKey, find_call_vertex_by_syntax,
        find_value_producer_vertex, static_prop_key,
    },
    ir::{
        AssignOp, CfgVertex, ExplicitEdgeKind, FunctionIr, IrBranchDirection, IrBranchKind,
        IrCallArg, IrCallKind, IrOp, IrProvider, IrScopeKind,
    },
    summaries::BehaviorSummaryIndex,
};

const DEFAULT_MAX_EFFECT_CALL_DEPTH: usize = 12;

/// Target syntax reference passed into [`analyze_call_effects_on_ir`].
#[derive(Clone, Copy, Debug)]
pub struct CallEffectsTargetSpec {
    pub syntax: SyntaxId,
    pub symbol: Option<SymbolId>,
}

/// Traversal options controlling callback expansion, read/write filtering, and call depth.
#[derive(Clone, Copy, Debug)]
pub struct CallEffectsOptions {
    pub follow_callbacks: bool,
    pub include_reads: bool,
    pub include_writes: bool,
    pub max_depth: usize,
}

impl Default for CallEffectsOptions {
    fn default() -> Self {
        Self {
            follow_callbacks: true,
            include_reads: true,
            include_writes: true,
            max_depth: DEFAULT_MAX_EFFECT_CALL_DEPTH,
        }
    }
}

/// Canonical `(SyntaxId, Option<SymbolId>)` pair used for declaration references.
pub type DeclTargetRef = (SyntaxId, Option<SymbolId>);

/// Structural/CFG branch guard condition dominating an effect vertex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardConditionData {
    pub branch_syntax: SyntaxId,
    pub condition_syntax: SyntaxId,
    pub condition_text: String,
    pub direction: &'static str,
    pub location: DiagnosticLocation,
}

/// Collected property read or write effect in IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldEffectData {
    pub kind: &'static str,
    pub property_name: String,
    pub operator: Option<&'static str>,
    pub syntax: SyntaxId,
    pub target_syntax: SyntaxId,
    pub receiver_syntax: SyntaxId,
    pub receiver_declaration: Option<DeclTargetRef>,
    pub value_syntax: Option<SyntaxId>,
    pub value_text: Option<String>,
    pub location: DiagnosticLocation,
    pub via_call: Option<SyntaxId>,
    pub enclosing_function: Option<SyntaxId>,
    pub enclosing_block: Option<SyntaxId>,
    pub cfg_scope: Option<SyntaxId>,
    pub guards: Vec<GuardConditionData>,
    pub evidence: Vec<EvidenceItem>,
}

/// Collected direct or indirect callback invocation site in IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallbackInvocationData {
    pub kind: &'static str,
    pub call_syntax: SyntaxId,
    pub outer_call_syntax: SyntaxId,
    pub syntax: SyntaxId,
    pub callback_argument_syntax: SyntaxId,
    pub parameter_index: Option<usize>,
    pub target_declaration: Option<DeclTargetRef>,
    pub helper_declaration: Option<DeclTargetRef>,
    pub resolved_targets: Vec<DeclTargetRef>,
    pub enclosing_function: Option<SyntaxId>,
    pub enclosing_block: Option<SyntaxId>,
    pub cfg_scope: Option<SyntaxId>,
    pub location: DiagnosticLocation,
    pub outer_call_location: DiagnosticLocation,
    pub evidence: Vec<EvidenceItem>,
}

/// Completeness-aware `callEffects` result for one target.
#[derive(Clone, Debug)]
pub enum CallEffectsOutcome {
    Complete {
        field_reads: Vec<FieldEffectData>,
        field_writes: Vec<FieldEffectData>,
        effects: Vec<FieldEffectData>,
        callback_invocations: Vec<CallbackInvocationData>,
        calls: Vec<SyntaxId>,
        evidence: Vec<EvidenceItem>,
    },
    Incomplete {
        field_reads: Vec<FieldEffectData>,
        field_writes: Vec<FieldEffectData>,
        effects: Vec<FieldEffectData>,
        callback_invocations: Vec<CallbackInvocationData>,
        calls: Vec<SyntaxId>,
        evidence: Vec<EvidenceItem>,
        gaps: Vec<GapItem>,
    },
}

/// Batch output returned by [`analyze_call_effects_on_ir`].
#[derive(Clone, Debug)]
pub struct CallEffectsBatchOutcome {
    pub results: Vec<CallEffectsOutcome>,
    pub conflict: Option<SummaryConflictError>,
}

#[derive(Clone)]
struct EffectFrame {
    call_syntax: SyntaxId,
    call_location: Option<DiagnosticLocation>,
    caller_ir: Arc<FunctionIr>,
    callee_ir: Arc<FunctionIr>,
    args: Vec<IrCallArg>,
    ir_frame: IrCallFrame,
}

#[derive(Clone)]
struct ResolvedCalleeTarget {
    callee_ir: Arc<FunctionIr>,
    captured_stack: Vec<EffectFrame>,
    decl_override: Option<DeclTargetRef>,
}

type VisitedEffectScopes = FxHashSet<(FunctionIrId, Vec<SyntaxId>, Option<(u32, u32)>)>;

#[derive(Default)]
struct CollectedEffects {
    reads: Vec<FieldEffectData>,
    writes: Vec<FieldEffectData>,
    callback_invocations: Vec<CallbackInvocationData>,
    calls: Vec<SyntaxId>,
    gaps: Vec<GapItem>,
    evidence: Vec<EvidenceItem>,
}

/// Evaluates `callEffects` on IR for all `targets`.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Accepts IR environment, project files, summary index, and optional cached call graph."
)]
pub fn analyze_call_effects_on_ir(
    targets: &[CallEffectsTargetSpec],
    options: CallEffectsOptions,
    project_files: &[FileId],
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    summary_index: &BehaviorSummaryIndex,
    cached_call_graph: Option<&IrCallGraph>,
) -> CallEffectsBatchOutcome {
    let mut owned_graph = None;
    let mut analyzer = CallEffectsAnalyzer {
        options,
        project_files,
        semantic,
        syntax,
        ir_provider,
        summary_index,
        cached_call_graph,
        owned_graph: &mut owned_graph,
        conflict: None,
    };

    let mut results = Vec::with_capacity(targets.len());
    for target in targets {
        if analyzer.conflict.is_some() {
            break;
        }
        results.push(analyzer.analyze_target(*target));
    }

    CallEffectsBatchOutcome {
        results,
        conflict: analyzer.conflict.take(),
    }
}

struct CallEffectsAnalyzer<'a> {
    options: CallEffectsOptions,
    project_files: &'a [FileId],
    semantic: &'a dyn SemanticModel,
    syntax: &'a dyn SyntaxCatalog,
    ir_provider: &'a dyn IrProvider,
    summary_index: &'a BehaviorSummaryIndex,
    cached_call_graph: Option<&'a IrCallGraph>,
    owned_graph: &'a mut Option<IrCallGraph>,
    conflict: Option<SummaryConflictError>,
}

impl CallEffectsAnalyzer<'_> {
    fn analyze_target(&mut self, target: CallEffectsTargetSpec) -> CallEffectsOutcome {
        let mut out = CollectedEffects::default();
        let mut visited = VisitedEffectScopes::default();

        if self.try_collect_direct_or_expr(target.syntax, &mut visited, &mut out) {
            return finalize_outcome(out);
        }

        if let Some(decl) = self.semantic.resolve_declaration(target.syntax) {
            if let Some(syn) = decl.callable_syntax.or(decl.initializer_syntax)
                && self.try_collect_direct_or_expr(syn, &mut visited, &mut out)
            {
                return finalize_outcome(out);
            }
            if decl.is_external || decl.is_default_library {
                push_unique_gap(
                    &mut out.gaps,
                    GapItem::from_reason(
                        UnknownReason::MissingFunctionBody,
                        format!(
                            "Target '{}' is an external declaration without source IR.",
                            decl.name.as_deref().unwrap_or("<external>")
                        ),
                        self.syntax.syntax_location(target.syntax).or(decl.location),
                    ),
                );
                return finalize_outcome(out);
            }
        }

        push_unique_gap(
            &mut out.gaps,
            GapItem::from_reason(
                UnknownReason::MissingFunctionBody,
                "Target cannot be resolved to analyzable IR.".to_string(),
                self.syntax.syntax_location(target.syntax),
            ),
        );
        finalize_outcome(out)
    }

    fn try_collect_direct_or_expr(
        &mut self,
        syn: SyntaxId,
        visited: &mut VisitedEffectScopes,
        out: &mut CollectedEffects,
    ) -> bool {
        if let Ok(ir) = self.ir_provider.function_ir(syn)
            && !matches!(
                ir.scope_kind,
                IrScopeKind::ModuleTopLevel | IrScopeKind::LocalBlock
            )
        {
            self.collect_in_ir(&ir, None, &[], 0, None, &[], visited, out);
            return true;
        }

        let Ok(enclosing_ir) = self.ir_provider.enclosing_scope_ir(syn) else {
            return false;
        };
        if let Some(call_v) = find_call_vertex_by_syntax(&enclosing_ir, syn) {
            let call_loc = call_v
                .location
                .clone()
                .or_else(|| self.syntax.syntax_location(syn));
            let span_filter = call_loc.as_ref().map(|l| (l.file.clone(), l.start, l.end));
            self.collect_in_ir(
                &enclosing_ir,
                span_filter.as_ref(),
                &[],
                0,
                None,
                &[],
                visited,
                out,
            );
            return true;
        }

        if syn.file_id() == enclosing_ir.root_syntax.file_id()
            && let Some(&val) = enclosing_ir.expr_to_value.get(&syn.local_id())
        {
            let hint = enclosing_ir.expr_to_vertex.get(&syn.local_id()).copied();
            let mut gaps = Vec::new();
            let targets =
                self.resolve_value_callables(&enclosing_ir, val, hint, &[], Some(syn), &mut gaps);
            for gap in gaps {
                push_unique_gap(&mut out.gaps, gap);
            }
            for resolved in targets {
                self.collect_in_ir(
                    &resolved.callee_ir,
                    None,
                    &resolved.captured_stack,
                    0,
                    None,
                    &[],
                    visited,
                    out,
                );
            }
            return true;
        }

        false
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Traverses CFG-reachable vertices of one FunctionIr in source order while preserving call frames."
    )]
    fn collect_in_ir(
        &mut self,
        ir: &Arc<FunctionIr>,
        span_filter: Option<&(String, u32, u32)>,
        call_stack: &[EffectFrame],
        depth: usize,
        via_call: Option<SyntaxId>,
        chain_evidence: &[EvidenceItem],
        visited: &mut VisitedEffectScopes,
        out: &mut CollectedEffects,
    ) {
        if self.conflict.is_some() {
            return;
        }
        if depth >= self.options.max_depth {
            push_unique_gap(
                &mut out.gaps,
                GapItem::from_reason(
                    UnknownReason::UnmodeledConstruct,
                    "Call effect traversal reached maximum recursion depth.".to_string(),
                    Some(ir.scope_location.clone()),
                ),
            );
            return;
        }

        let stack_key: Vec<SyntaxId> = call_stack.iter().map(|f| f.call_syntax).collect();
        let span_key = span_filter.map(|(_, s, e)| (*s, *e));
        if !visited.insert((ir.id, stack_key, span_key)) {
            return;
        }

        let reachable = compute_explicit_reachable(ir);
        let callee_reads = collect_direct_call_callee_prop_reads(ir);

        let mut ordered_vertices: Vec<&CfgVertex> = ir
            .vertices
            .iter()
            .filter(|v| {
                reachable
                    .get(usize::try_from(v.id.0).unwrap_or(usize::MAX))
                    .copied()
                    .unwrap_or(false)
            })
            .filter(|v| {
                span_filter.is_none_or(|(file, start, end)| {
                    v.location.as_ref().is_some_and(|loc| {
                        loc.file == *file && loc.start >= *start && loc.end <= *end
                    })
                })
            })
            .collect();

        ordered_vertices.sort_by_key(|v| {
            let (s, e) = v
                .location
                .as_ref()
                .map_or((u32::MAX, u32::MAX), |l| (l.start, l.end));
            (s, e, v.id.0)
        });

        for vertex in ordered_vertices {
            if self.conflict.is_some() {
                return;
            }
            match &vertex.op {
                IrOp::PropRead {
                    object,
                    key,
                    implicit_call_kind,
                    ..
                } if self.options.include_reads
                    && (implicit_call_kind.is_some() || !callee_reads.contains(&vertex.id)) =>
                {
                    if let Some((prop_name, _)) = static_prop_key(key)
                        && let Some(syn) = vertex.syntax
                        && let Some(loc) = vertex
                            .location
                            .clone()
                            .or_else(|| self.syntax.syntax_location(syn))
                    {
                        let receiver_syntax =
                            resolve_value_syntax(ir, *object, Some(vertex.id), self.syntax)
                                .unwrap_or(syn);
                        let mut ev = chain_evidence.to_vec();
                        push_unique_evidence(
                            &mut ev,
                            EvidenceItem::simple(
                                "source",
                                format!("Read field '.{prop_name}'"),
                                Some(loc.clone()),
                            ),
                        );
                        out.reads.push(self.build_field_effect(
                            ir,
                            vertex.id,
                            "read",
                            prop_name.to_string(),
                            None,
                            syn,
                            syn,
                            receiver_syntax,
                            None,
                            None,
                            loc,
                            via_call,
                            ev,
                        ));
                    }
                }
                IrOp::PropWrite {
                    object,
                    key,
                    op,
                    value,
                    ..
                } if self.options.include_writes => {
                    if let Some((prop_name, _)) = static_prop_key(key)
                        && let Some(syn) = vertex.syntax
                        && let Some(loc) = vertex
                            .location
                            .clone()
                            .or_else(|| self.syntax.syntax_location(syn))
                    {
                        let write_parts = self.semantic.write_syntax_info(syn);
                        let target_syntax = write_parts.map_or(syn, |p| p.target_syntax);
                        let op_str = write_parts
                            .and_then(|p| p.logical_assign_op)
                            .unwrap_or_else(|| assign_op_str(*op));
                        let receiver_syntax =
                            resolve_value_syntax(ir, *object, Some(vertex.id), self.syntax)
                                .unwrap_or(syn);
                        let value_syntax =
                            if matches!(op, AssignOp::Increment | AssignOp::Decrement) {
                                None
                            } else {
                                resolve_value_syntax(ir, *value, Some(vertex.id), self.syntax)
                            };
                        let value_text = value_syntax.and_then(|vs| self.syntax.syntax_text(vs));
                        let verb = if matches!(op, AssignOp::Increment | AssignOp::Decrement) {
                            "Updated"
                        } else {
                            "Wrote"
                        };
                        let mut ev = chain_evidence.to_vec();
                        push_unique_evidence(
                            &mut ev,
                            EvidenceItem::simple(
                                "source",
                                format!("{verb} field '.{prop_name}' using '{op_str}'"),
                                Some(loc.clone()),
                            ),
                        );
                        out.writes.push(self.build_field_effect(
                            ir,
                            vertex.id,
                            "write",
                            prop_name.to_string(),
                            Some(op_str),
                            syn,
                            target_syntax,
                            receiver_syntax,
                            value_syntax,
                            value_text,
                            loc,
                            via_call,
                            ev,
                        ));
                    }
                }
                IrOp::Call {
                    call_kind,
                    callee,
                    receiver,
                    args,
                } => {
                    self.visit_call_vertex(
                        ir,
                        vertex,
                        *call_kind,
                        *callee,
                        *receiver,
                        args,
                        call_stack,
                        depth,
                        via_call,
                        chain_evidence,
                        visited,
                        out,
                    );
                }
                IrOp::Construct { callee, args, .. } => {
                    self.visit_construct_vertex(
                        ir,
                        vertex,
                        *callee,
                        args,
                        call_stack,
                        depth,
                        via_call,
                        chain_evidence,
                        visited,
                        out,
                    );
                }
                IrOp::Unknown {
                    reason,
                    message,
                    location,
                    ..
                } => {
                    push_unique_gap(
                        &mut out.gaps,
                        GapItem::from_reason(*reason, message.clone(), Some(location.clone())),
                    );
                }
                _ => {}
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Constructs normalized FieldEffectData with CFG guard dominators and AST scope metadata."
    )]
    fn build_field_effect(
        &self,
        ir: &FunctionIr,
        vertex_id: VertexId,
        kind: &'static str,
        property_name: String,
        operator: Option<&'static str>,
        syntax: SyntaxId,
        target_syntax: SyntaxId,
        receiver_syntax: SyntaxId,
        value_syntax: Option<SyntaxId>,
        value_text: Option<String>,
        location: DiagnosticLocation,
        via_call: Option<SyntaxId>,
        evidence: Vec<EvidenceItem>,
    ) -> FieldEffectData {
        let receiver_declaration = self.resolve_decl_for_expr(receiver_syntax);
        let scope_info = self.semantic.syntax_scope_info(syntax);
        let enclosing_function = scope_info
            .and_then(|s| s.enclosing_function)
            .or(Some(ir.root_syntax));
        let enclosing_block = scope_info.and_then(|s| s.enclosing_block);
        let cfg_scope = scope_info.and_then(|s| s.cfg_scope).or(enclosing_function);
        let guards = compute_cfg_guards(ir, vertex_id, self.semantic);

        FieldEffectData {
            kind,
            property_name,
            operator,
            syntax,
            target_syntax,
            receiver_syntax,
            receiver_declaration,
            value_syntax,
            value_text,
            location,
            via_call,
            enclosing_function,
            enclosing_block,
            cfg_scope,
            guards,
            evidence,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Handles explicit/implicit calls, callback parameters, summary callback specs, and transitive call traversal."
    )]
    fn visit_call_vertex(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_kind: IrCallKind,
        callee_val: ValueId,
        receiver: Option<ValueId>,
        args: &[IrCallArg],
        call_stack: &[EffectFrame],
        depth: usize,
        via_call: Option<SyntaxId>,
        chain_evidence: &[EvidenceItem],
        visited: &mut VisitedEffectScopes,
        out: &mut CollectedEffects,
    ) {
        let Some(call_syntax) = call_vertex.syntax else {
            return;
        };
        if matches!(
            call_kind,
            IrCallKind::Explicit | IrCallKind::TaggedTemplate | IrCallKind::SuperCall
        ) {
            out.calls.push(call_syntax);
        }

        let call_loc = call_vertex
            .location
            .clone()
            .or_else(|| self.syntax.syntax_location(call_syntax));

        // 1. Check if callee is a parameter of an active helper frame (indirect callback invocation).
        if let Some((param_idx, arg_targets, param_gaps)) =
            self.try_resolve_param_callback(caller_ir, call_vertex, callee_val, call_stack)
        {
            for g in param_gaps {
                push_unique_gap(&mut out.gaps, g);
            }
            if let Some(loc) = call_loc.as_ref() {
                self.record_indirect_callback_invocation(
                    caller_ir,
                    call_syntax,
                    call_stack,
                    &arg_targets,
                    param_idx,
                    loc,
                    chain_evidence,
                    out,
                );
            }
            if self.options.follow_callbacks {
                self.enter_resolved_targets(
                    caller_ir,
                    call_syntax,
                    call_loc.as_ref(),
                    receiver,
                    args,
                    &arg_targets,
                    call_stack,
                    depth,
                    via_call,
                    chain_evidence,
                    visited,
                    out,
                );
            }
            return;
        }

        // 2. Check summary `calls` aspect (callbacks like Array.prototype.forEach / Promise.then,
        //    or forwarding like Function.prototype.call/apply/bind).
        let summary_match =
            self.lookup_call_summary(caller_ir, call_vertex, call_syntax, call_kind);
        if self.conflict.is_some() {
            return;
        }
        if let Some(matched) = summary_match {
            self.semantic.record_summary_lookup(Some(matched.source));
            if let Some(fwd) = &matched.aspect.forwards_call {
                let target_val = match fwd.target {
                    ForwardCallTarget::Receiver(_) => receiver,
                    ForwardCallTarget::Arg { arg_index } => {
                        args.get(arg_index).copied().map(arg_value_id)
                    }
                };
                if let Some(tv) = target_val {
                    let mut fwd_gaps = Vec::new();
                    let fwd_targets = self.resolve_value_callables(
                        caller_ir,
                        tv,
                        Some(call_vertex.id),
                        call_stack,
                        Some(call_syntax),
                        &mut fwd_gaps,
                    );
                    for g in fwd_gaps {
                        push_unique_gap(&mut out.gaps, g);
                    }
                    let forwarded_args: Vec<IrCallArg> =
                        fwd.args_from_index.map_or_else(Vec::new, |from_idx| {
                            args.get(from_idx..).unwrap_or(&[]).to_vec()
                        });
                    let forwarded_recv = fwd.this_arg.as_ref().and_then(|ta| match ta {
                        SummaryThisArg::FromArg { from_arg_index } => {
                            args.get(*from_arg_index).copied().map(arg_value_id)
                        }
                        SummaryThisArg::Undefined(_) => None,
                    });
                    self.enter_resolved_targets(
                        caller_ir,
                        call_syntax,
                        call_loc.as_ref(),
                        forwarded_recv,
                        &forwarded_args,
                        &fwd_targets,
                        call_stack,
                        depth,
                        via_call,
                        chain_evidence,
                        visited,
                        out,
                    );
                }
                return;
            }
            if !matched.aspect.invokes_callbacks.is_empty() {
                if self.options.follow_callbacks {
                    for cb_spec in &matched.aspect.invokes_callbacks {
                        if matches!(cb_spec.timing, CallbackTiming::Deferred) {
                            // Keep summary callback traversal aligned with synchronous/explicitly followed callbacks.
                        }
                        if let Some(arg) = args.get(cb_spec.callback_arg_index) {
                            let arg_val = arg_value_id(*arg);
                            let arg_syn = resolve_value_syntax(
                                caller_ir,
                                arg_val,
                                Some(call_vertex.id),
                                self.syntax,
                            );
                            let mut cb_gaps = Vec::new();
                            let cb_targets = self.resolve_value_callables(
                                caller_ir,
                                arg_val,
                                Some(call_vertex.id),
                                call_stack,
                                arg_syn,
                                &mut cb_gaps,
                            );
                            for g in cb_gaps {
                                push_unique_gap(&mut out.gaps, g);
                            }
                            self.enter_resolved_targets(
                                caller_ir,
                                call_syntax,
                                call_loc.as_ref(),
                                None,
                                &[],
                                &cb_targets,
                                call_stack,
                                depth,
                                via_call,
                                chain_evidence,
                                visited,
                                out,
                            );
                        }
                    }
                }
                return;
            }
        }

        // 3. Resolve static targets via `call_resolution` + local closure/object-method resolution.
        let (targets, call_gaps) = self.resolve_direct_call_targets(
            caller_ir,
            call_vertex,
            call_syntax,
            callee_val,
            call_stack,
        );
        if self.conflict.is_some() {
            return;
        }
        for g in call_gaps {
            push_unique_gap(&mut out.gaps, g);
        }

        if targets.is_empty()
            && let Some(loc) = call_loc.as_ref()
        {
            self.record_direct_callback_invocation(
                caller_ir,
                call_syntax,
                call_vertex,
                callee_val,
                loc,
                chain_evidence,
                out,
            );
        }

        self.enter_resolved_targets(
            caller_ir,
            call_syntax,
            call_loc.as_ref(),
            receiver,
            args,
            &targets,
            call_stack,
            depth,
            via_call,
            chain_evidence,
            visited,
            out,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Enters constructor bodies when `new Class()` targets a local project constructor."
    )]
    fn visit_construct_vertex(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        _callee_val: ValueId,
        args: &[IrCallArg],
        call_stack: &[EffectFrame],
        depth: usize,
        via_call: Option<SyntaxId>,
        chain_evidence: &[EvidenceItem],
        visited: &mut VisitedEffectScopes,
        out: &mut CollectedEffects,
    ) {
        let Some(call_syntax) = call_vertex.syntax else {
            return;
        };
        let resolution = resolve_call_targets_with_summaries(
            call_syntax,
            self.semantic,
            self.syntax,
            self.ir_provider,
            self.summary_index,
        );
        if let Some(conflict) = resolution.conflict {
            self.conflict = Some(conflict);
            return;
        }
        for g in resolution.gaps {
            push_unique_gap(&mut out.gaps, g);
        }
        let mut targets = Vec::new();
        for t in resolution.targets {
            if !t.is_external
                && !t.is_default_library
                && let Some(fn_syn) = t.fn_scope_syntax
                && let Ok(callee_ir) = self.ir_provider.function_ir(fn_syn)
            {
                targets.push(ResolvedCalleeTarget {
                    callee_ir,
                    captured_stack: call_stack.to_vec(),
                    decl_override: Some((t.declaration_syntax, t.symbol)),
                });
            }
        }
        let call_loc = call_vertex
            .location
            .clone()
            .or_else(|| self.syntax.syntax_location(call_syntax));
        self.enter_resolved_targets(
            caller_ir,
            call_syntax,
            call_loc.as_ref(),
            None,
            args,
            &targets,
            call_stack,
            depth,
            via_call,
            chain_evidence,
            visited,
            out,
        );
    }

    fn try_resolve_param_callback(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        callee_val: ValueId,
        call_stack: &[EffectFrame],
    ) -> Option<(usize, Vec<ResolvedCalleeTarget>, Vec<GapItem>)> {
        let producer = find_value_producer_vertex(caller_ir, callee_val, Some(call_vertex.id))?;
        let param_idx = match producer.op {
            IrOp::ReadLocal { binding } => caller_ir
                .parameters
                .iter()
                .position(|p| p.binding == Some(binding))?,
            _ => return None,
        };
        let frame_pos = call_stack
            .iter()
            .rposition(|f| f.callee_ir.id == caller_ir.id)?;
        let frame = &call_stack[frame_pos];
        let parent_stack = &call_stack[..frame_pos];
        let arg = frame.args.get(param_idx).copied()?;
        let arg_val = arg_value_id(arg);
        let arg_syn = resolve_value_syntax(&frame.caller_ir, arg_val, None, self.syntax);

        let mut gaps = Vec::new();
        let targets = self.resolve_value_callables(
            &frame.caller_ir,
            arg_val,
            None,
            parent_stack,
            arg_syn,
            &mut gaps,
        );
        if targets.is_empty() {
            let arg_text = arg_syn
                .and_then(|s| self.syntax.syntax_text(s))
                .unwrap_or_else(|| "callback".to_string());
            push_unique_gap(
                &mut gaps,
                GapItem::from_reason(
                    UnknownReason::DynamicCallee,
                    format!(
                        "Callback argument '{arg_text}' is invoked synchronously but its concrete body cannot be statically resolved."
                    ),
                    call_vertex.location.clone(),
                ),
            );
        }
        Some((param_idx, targets, gaps))
    }

    fn lookup_call_summary(
        &mut self,
        _caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_syntax: SyntaxId,
        call_kind: IrCallKind,
    ) -> Option<crate::summaries::MatchedSummaryAspect<tssae_common::CallAspectSummary>> {
        let mut syns = Vec::new();
        if matches!(
            call_kind,
            IrCallKind::Explicit | IrCallKind::TaggedTemplate | IrCallKind::DecoratorApply
        ) {
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
        for syn in syns {
            let Some(identity) = self.semantic.declaration_identity(syn) else {
                continue;
            };
            match self.summary_index.resolve_calls_summary(&identity, false) {
                Ok(Some(matched)) => return Some(matched),
                Ok(None) => {}
                Err(conflict) => {
                    self.conflict = Some(conflict);
                    return None;
                }
            }
        }
        let _ = call_vertex;
        None
    }

    fn resolve_direct_call_targets(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_vertex: &CfgVertex,
        call_syntax: SyntaxId,
        callee_val: ValueId,
        call_stack: &[EffectFrame],
    ) -> (Vec<ResolvedCalleeTarget>, Vec<GapItem>) {
        // 1. Try local/captured value tracing (handles closures, local function vars, object literals).
        let mut trace_gaps = Vec::new();
        let value_targets = self.resolve_value_callables(
            caller_ir,
            callee_val,
            Some(call_vertex.id),
            call_stack,
            None,
            &mut trace_gaps,
        );
        if !value_targets.is_empty() {
            return (value_targets, trace_gaps);
        }

        // 2. Fall back to Ticket 08's `call_resolution`.
        let resolution = resolve_call_targets_with_summaries(
            call_syntax,
            self.semantic,
            self.syntax,
            self.ir_provider,
            self.summary_index,
        );
        if let Some(conflict) = resolution.conflict {
            self.conflict = Some(conflict);
            return (Vec::new(), Vec::new());
        }

        let mut targets = Vec::new();
        for t in resolution.targets {
            if !t.is_external
                && !t.is_default_library
                && let Some(fn_syn) = t.fn_scope_syntax
                && let Ok(callee_ir) = self.ir_provider.function_ir(fn_syn)
            {
                targets.push(ResolvedCalleeTarget {
                    callee_ir,
                    captured_stack: call_stack.to_vec(),
                    decl_override: Some((t.declaration_syntax, t.symbol)),
                });
            }
        }
        (targets, resolution.gaps)
    }

    fn resolve_value_callables(
        &mut self,
        scope_ir: &Arc<FunctionIr>,
        val: ValueId,
        vertex_hint: Option<VertexId>,
        call_stack: &[EffectFrame],
        expr_hint: Option<SyntaxId>,
        gaps: &mut Vec<GapItem>,
    ) -> Vec<ResolvedCalleeTarget> {
        let ir_stack: Vec<IrCallFrame> = call_stack.iter().map(|f| f.ir_frame.clone()).collect();
        let mut resolver = IrInstanceResolver::new(
            self.project_files,
            None,
            self.semantic,
            self.syntax,
            self.ir_provider,
            self.summary_index,
            self.cached_call_graph,
            self.owned_graph,
        );
        let mut active: Vec<VisitKey> = Vec::new();
        let trace = resolver.trace_value(scope_ir, val, vertex_hint, &ir_stack, 0, &mut active);
        if let Some(conflict) = resolver.conflict.take() {
            self.conflict = Some(conflict);
            return Vec::new();
        }
        for g in trace.gaps {
            push_unique_gap(gaps, g);
        }

        let decl_from_expr = expr_hint.and_then(|s| self.resolve_decl_for_expr(s));
        let mut out = Vec::new();
        let mut seen_irs = FxHashSet::default();
        for host in trace.hosts {
            let Some(fn_syn) = host.closure_fn else {
                continue;
            };
            let Ok(callee_ir) = self.ir_provider.function_ir(fn_syn) else {
                continue;
            };
            if !seen_irs.insert((callee_ir.id, host.captured_stack.len())) {
                continue;
            }
            let captured: Vec<EffectFrame> = host
                .captured_stack
                .iter()
                .map(|irf| EffectFrame {
                    call_syntax: irf.call_syntax,
                    call_location: irf.call_location.clone(),
                    caller_ir: Arc::clone(&irf.caller_ir),
                    callee_ir: Arc::clone(&irf.callee_ir),
                    args: irf.args.clone(),
                    ir_frame: irf.clone(),
                })
                .collect();
            let decl_override =
                Some(decl_from_expr.unwrap_or_else(|| self.resolve_decl_for_fn(&callee_ir)));
            out.push(ResolvedCalleeTarget {
                callee_ir,
                captured_stack: captured,
                decl_override,
            });
        }
        out
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Pushes EffectFrame onto stack and invokes collect_in_ir for each resolved callee."
    )]
    fn enter_resolved_targets(
        &mut self,
        caller_ir: &Arc<FunctionIr>,
        call_syntax: SyntaxId,
        call_location: Option<&DiagnosticLocation>,
        receiver: Option<ValueId>,
        args: &[IrCallArg],
        targets: &[ResolvedCalleeTarget],
        _call_stack: &[EffectFrame],
        depth: usize,
        via_call: Option<SyntaxId>,
        chain_evidence: &[EvidenceItem],
        visited: &mut VisitedEffectScopes,
        out: &mut CollectedEffects,
    ) {
        for target in targets {
            let fn_label = self
                .semantic
                .resolve_declaration(target.callee_ir.root_syntax)
                .and_then(|d| d.name)
                .or_else(|| self.syntax.syntax_text(call_syntax))
                .unwrap_or_else(|| "<callback>".to_string());
            let mut next_stack = target.captured_stack.clone();
            let ir_closure_env: Vec<IrCallFrame> = target
                .captured_stack
                .iter()
                .map(|f| f.ir_frame.clone())
                .collect();
            let frame = EffectFrame {
                call_syntax,
                call_location: call_location.cloned(),
                caller_ir: Arc::clone(caller_ir),
                callee_ir: Arc::clone(&target.callee_ir),
                args: args.to_vec(),
                ir_frame: IrCallFrame {
                    call_syntax,
                    call_location: call_location.cloned(),
                    caller_ir: Arc::clone(caller_ir),
                    callee_ir: Arc::clone(&target.callee_ir),
                    receiver: receiver.map(ReceiverBinding::Value),
                    args: args.to_vec(),
                    is_construct: false,
                    closure_env: ir_closure_env,
                },
            };
            next_stack.push(frame);
            let mut next_ev = chain_evidence.to_vec();
            push_unique_evidence(
                &mut next_ev,
                EvidenceItem::simple(
                    "source",
                    format!("Entered call to '{fn_label}'"),
                    call_location.cloned(),
                ),
            );
            self.collect_in_ir(
                &target.callee_ir,
                None,
                &next_stack,
                depth + 1,
                Some(via_call.unwrap_or(call_syntax)),
                &next_ev,
                visited,
                out,
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Records indirect callback invocation with outer call, helper declaration, and resolved target provenance."
    )]
    fn record_indirect_callback_invocation(
        &self,
        caller_ir: &Arc<FunctionIr>,
        call_syntax: SyntaxId,
        call_stack: &[EffectFrame],
        targets: &[ResolvedCalleeTarget],
        param_idx: usize,
        loc: &DiagnosticLocation,
        chain_evidence: &[EvidenceItem],
        out: &mut CollectedEffects,
    ) {
        let Some(frame) = call_stack.iter().rfind(|f| f.callee_ir.id == caller_ir.id) else {
            return;
        };
        let Some(&cb_arg) = frame.args.get(param_idx) else {
            return;
        };
        let Some(cb_arg_syntax) =
            resolve_value_syntax(&frame.caller_ir, arg_value_id(cb_arg), None, self.syntax)
        else {
            return;
        };
        let Some(outer_loc) = frame
            .call_location
            .clone()
            .or_else(|| self.syntax.syntax_location(frame.call_syntax))
        else {
            return;
        };

        let helper_decl = Some(self.resolve_decl_for_fn(caller_ir));
        let fn_name = helper_decl
            .and_then(|(s, _)| self.semantic.resolve_declaration(s))
            .and_then(|d| d.name)
            .unwrap_or_else(|| "<helper>".to_string());
        let arg_txt = self
            .syntax
            .syntax_text(cb_arg_syntax)
            .unwrap_or_else(|| "callback".to_string());
        let helper_decl_loc = helper_decl
            .and_then(|(s, _)| self.syntax.syntax_location(s))
            .unwrap_or_else(|| caller_ir.scope_location.clone());
        let target_declaration = self.resolve_decl_for_expr(cb_arg_syntax);
        let resolved_targets: Vec<DeclTargetRef> = targets
            .iter()
            .map(|t| {
                t.decl_override
                    .unwrap_or_else(|| self.resolve_decl_for_fn(&t.callee_ir))
            })
            .collect();
        let outer_scopes = self.semantic.syntax_scope_info(frame.call_syntax);

        let mut cb_ev: Vec<EvidenceItem> = chain_evidence
            .iter()
            .filter(|item| {
                item.at.as_ref().is_none_or(|a| {
                    a.file != outer_loc.file || a.start != outer_loc.start || a.end != outer_loc.end
                })
            })
            .cloned()
            .collect();
        push_unique_evidence(
            &mut cb_ev,
            EvidenceItem::simple(
                "source",
                format!("Passed callback argument '{arg_txt}' to helper '{fn_name}'"),
                Some(outer_loc.clone()),
            ),
        );
        push_unique_evidence(
            &mut cb_ev,
            EvidenceItem::simple(
                "source",
                format!("Helper '{fn_name}' accepts callback parameter #{param_idx}"),
                Some(helper_decl_loc),
            ),
        );
        push_unique_evidence(
            &mut cb_ev,
            EvidenceItem::simple(
                "source",
                format!("Helper '{fn_name}' invokes callback parameter #{param_idx} ('{arg_txt}')"),
                Some(loc.clone()),
            ),
        );

        out.callback_invocations.push(CallbackInvocationData {
            kind: "indirect",
            call_syntax,
            outer_call_syntax: frame.call_syntax,
            syntax: call_syntax,
            callback_argument_syntax: cb_arg_syntax,
            parameter_index: Some(param_idx),
            target_declaration,
            helper_declaration: helper_decl,
            resolved_targets,
            enclosing_function: outer_scopes
                .and_then(|s| s.enclosing_function)
                .or(Some(frame.caller_ir.root_syntax)),
            enclosing_block: outer_scopes.and_then(|s| s.enclosing_block),
            cfg_scope: outer_scopes
                .and_then(|s| s.cfg_scope)
                .or(Some(frame.caller_ir.root_syntax)),
            location: loc.clone(),
            outer_call_location: outer_loc,
            evidence: cb_ev,
        });
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Records direct invocation of a local variable, parameter, or field callable."
    )]
    fn record_direct_callback_invocation(
        &self,
        caller_ir: &Arc<FunctionIr>,
        call_syntax: SyntaxId,
        call_vertex: &CfgVertex,
        callee_val: ValueId,
        loc: &DiagnosticLocation,
        chain_evidence: &[EvidenceItem],
        out: &mut CollectedEffects,
    ) {
        let Some(callee_expr) =
            resolve_value_syntax(caller_ir, callee_val, Some(call_vertex.id), self.syntax)
        else {
            return;
        };
        let Some((decl_syn, sym_opt)) = self.resolve_decl_for_expr(callee_expr) else {
            return;
        };
        let Some(decl_data) = self.semantic.resolve_declaration(callee_expr) else {
            return;
        };
        if decl_data.is_external
            || decl_data.is_default_library
            || !matches!(
                decl_data.kind.as_deref(),
                Some(
                    "VariableDeclaration"
                        | "Parameter"
                        | "BindingElement"
                        | "PropertyDeclaration"
                        | "PropertySignature"
                )
            )
        {
            return;
        }

        let arg_txt = self
            .syntax
            .syntax_text(callee_expr)
            .unwrap_or_else(|| "callback".to_string());
        let mut cb_ev = chain_evidence.to_vec();
        push_unique_evidence(
            &mut cb_ev,
            EvidenceItem::simple(
                "source",
                format!("Direct invocation of callable '{arg_txt}'"),
                Some(loc.clone()),
            ),
        );
        let scope_info = self.semantic.syntax_scope_info(call_syntax);

        out.callback_invocations.push(CallbackInvocationData {
            kind: "direct",
            call_syntax,
            outer_call_syntax: call_syntax,
            syntax: call_syntax,
            callback_argument_syntax: callee_expr,
            parameter_index: None,
            target_declaration: Some((decl_syn, sym_opt)),
            helper_declaration: None,
            resolved_targets: Vec::new(),
            enclosing_function: scope_info
                .and_then(|s| s.enclosing_function)
                .or(Some(caller_ir.root_syntax)),
            enclosing_block: scope_info.and_then(|s| s.enclosing_block),
            cfg_scope: scope_info
                .and_then(|s| s.cfg_scope)
                .or(Some(caller_ir.root_syntax)),
            location: loc.clone(),
            outer_call_location: loc.clone(),
            evidence: cb_ev,
        });
    }

    fn resolve_decl_for_expr(&self, expr_syn: SyntaxId) -> Option<DeclTargetRef> {
        let decl = self.semantic.resolve_declaration(expr_syn)?;
        let primary = decl.primary_syntax.or(Some(expr_syn))?;
        Some((primary, decl.symbol))
    }

    fn resolve_decl_for_fn(&self, fn_ir: &FunctionIr) -> DeclTargetRef {
        if let Some(decl) = self.semantic.resolve_declaration(fn_ir.root_syntax) {
            let primary = decl.primary_syntax.unwrap_or(fn_ir.root_syntax);
            return (primary, decl.symbol);
        }
        (
            fn_ir.root_syntax,
            self.semantic.symbol_at_syntax(fn_ir.root_syntax),
        )
    }
}

fn finalize_outcome(mut out: CollectedEffects) -> CallEffectsOutcome {
    let mut effects = Vec::with_capacity(out.reads.len() + out.writes.len());
    for r in &out.reads {
        for ev in &r.evidence {
            push_unique_evidence(&mut out.evidence, ev.clone());
        }
        effects.push(r.clone());
    }
    for w in &out.writes {
        for ev in &w.evidence {
            push_unique_evidence(&mut out.evidence, ev.clone());
        }
        effects.push(w.clone());
    }
    for cb in &out.callback_invocations {
        for ev in &cb.evidence {
            push_unique_evidence(&mut out.evidence, ev.clone());
        }
    }

    if out.gaps.is_empty() {
        CallEffectsOutcome::Complete {
            field_reads: out.reads,
            field_writes: out.writes,
            effects,
            callback_invocations: out.callback_invocations,
            calls: out.calls,
            evidence: out.evidence,
        }
    } else {
        CallEffectsOutcome::Incomplete {
            field_reads: out.reads,
            field_writes: out.writes,
            effects,
            callback_invocations: out.callback_invocations,
            calls: out.calls,
            evidence: out.evidence,
            gaps: out.gaps,
        }
    }
}

const fn arg_value_id(arg: IrCallArg) -> ValueId {
    match arg {
        IrCallArg::Positional(v) | IrCallArg::Spread(v) => v,
    }
}

const fn assign_op_str(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "=",
        AssignOp::AddAssign => "+=",
        AssignOp::SubAssign => "-=",
        AssignOp::MulAssign => "*=",
        AssignOp::DivAssign => "/=",
        AssignOp::ModAssign => "%=",
        AssignOp::ExpAssign => "**=",
        AssignOp::BitAndAssign => "&=",
        AssignOp::BitOrAssign => "|=",
        AssignOp::BitXorAssign => "^=",
        AssignOp::ShlAssign => "<<=",
        AssignOp::ShrAssign => ">>=",
        AssignOp::UShrAssign => ">>>=",
        AssignOp::Increment => "++",
        AssignOp::Decrement => "--",
    }
}

fn resolve_value_syntax(
    ir: &FunctionIr,
    mut val: ValueId,
    hint: Option<VertexId>,
    syntax: &dyn SyntaxCatalog,
) -> Option<SyntaxId> {
    let mut cur_hint = hint;
    for _ in 0..16 {
        let v = find_value_producer_vertex(ir, val, cur_hint)?;
        if let IrOp::ValueForward { source, .. } = v.op {
            val = source;
            cur_hint = Some(v.id);
            continue;
        }
        if let Some(syn) = v.syntax {
            let _ = syntax;
            return Some(syn);
        }
        return None;
    }
    None
}

fn collect_direct_call_callee_prop_reads(ir: &FunctionIr) -> FxHashSet<VertexId> {
    let mut callee_vals: Vec<ValueId> = Vec::new();
    for v in &ir.vertices {
        match v.op {
            IrOp::Call {
                call_kind:
                    IrCallKind::Explicit | IrCallKind::TaggedTemplate | IrCallKind::DecoratorApply,
                callee,
                ..
            }
            | IrOp::Construct { callee, .. } => {
                callee_vals.push(callee);
            }
            _ => {}
        }
    }

    let mut excluded = FxHashSet::default();
    let mut visited_vals = FxHashSet::default();
    while let Some(val) = callee_vals.pop() {
        if !visited_vals.insert(val) {
            continue;
        }
        if let Some(v) = find_value_producer_vertex(ir, val, None) {
            match v.op {
                IrOp::ValueForward { source, .. } => callee_vals.push(source),
                IrOp::PropRead {
                    implicit_call_kind: None,
                    ..
                } => {
                    excluded.insert(v.id);
                }
                _ => {}
            }
        }
    }
    excluded
}

fn compute_explicit_reachable(ir: &FunctionIr) -> Vec<bool> {
    reachable_explicit_excluding_branch(ir, None)
}

fn reachable_explicit_excluding_branch(
    ir: &FunctionIr,
    blocked_branch: Option<BranchId>,
) -> Vec<bool> {
    let mut visited_vertices = vec![false; ir.vertices.len()];
    let mut visited_states: FxHashSet<(VertexId, StackId)> = FxHashSet::default();
    let mut stacks = StackArena::new();
    let mut queue = VecDeque::new();

    let empty_stack = StackId(0);
    let entry_idx = usize::try_from(ir.entry.0).unwrap_or(usize::MAX);
    if entry_idx < visited_vertices.len() {
        visited_vertices[entry_idx] = true;
        visited_states.insert((ir.entry, empty_stack));
        queue.push_back((ir.entry, empty_stack));
    }

    while let Some((u, stack_id)) = queue.pop_front() {
        let Some(vertex) = ir.vertices.get(usize::try_from(u.0).unwrap_or(usize::MAX)) else {
            continue;
        };
        for edge in &vertex.explicit_edges {
            if let Some(cut) = blocked_branch
                && edge.kind == ExplicitEdgeKind::Branch(cut)
            {
                continue;
            }
            let Some(next_stack) = stacks.transition_explicit(ir, stack_id, edge) else {
                continue;
            };
            let next_idx = usize::try_from(edge.target.0).unwrap_or(usize::MAX);
            if next_idx < visited_vertices.len() {
                visited_vertices[next_idx] = true;
                if visited_states.insert((edge.target, next_stack)) {
                    queue.push_back((edge.target, next_stack));
                }
            }
        }
    }
    visited_vertices
}

fn compute_cfg_guards(
    ir: &FunctionIr,
    target_vid: VertexId,
    semantic: &dyn SemanticModel,
) -> Vec<GuardConditionData> {
    let target_idx = usize::try_from(target_vid.0).unwrap_or(usize::MAX);
    let mut guards = Vec::new();

    for branch in &ir.branches {
        if branch.kind != IrBranchKind::If
            || !matches!(
                branch.direction,
                IrBranchDirection::True | IrBranchDirection::False
            )
        {
            continue;
        }
        let reachable_without = reachable_explicit_excluding_branch(ir, Some(branch.id));
        if !reachable_without.get(target_idx).copied().unwrap_or(false) {
            let branch_syntax = semantic
                .branch_statement_syntax(branch.condition_syntax)
                .unwrap_or(branch.condition_syntax);
            guards.push(GuardConditionData {
                branch_syntax,
                condition_syntax: branch.condition_syntax,
                condition_text: branch.condition_text.clone(),
                direction: branch.direction.as_str(),
                location: branch.location.clone(),
            });
        }
    }

    guards.sort_by_key(|g| (g.location.start, g.location.end));
    guards
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
        DeclarationSummaryData, FileId, ImplementationFact, LocalBindingId, LocalSyntaxId,
        MemberTargetHierarchy, SignatureId, SummarySource, TypeId, TypeSummaryData,
    };

    use crate::ir::{
        BindingMutability, DeclarationSemantics, ExplicitEdge, IrBranchRecord, IrLocalBinding,
        IrPropKey, LiteralKind,
    };

    struct MockEnv {
        irs: FxHashMap<SyntaxId, Arc<FunctionIr>>,
        texts: FxHashMap<SyntaxId, String>,
        locs: FxHashMap<SyntaxId, DiagnosticLocation>,
        decls: FxHashMap<SyntaxId, DeclarationSummaryData>,
    }

    impl MockEnv {
        fn new() -> Self {
            Self {
                irs: FxHashMap::default(),
                texts: FxHashMap::default(),
                locs: FxHashMap::default(),
                decls: FxHashMap::default(),
            }
        }
    }

    impl SyntaxCatalog for MockEnv {
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

    impl SemanticModel for MockEnv {
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
        fn record_summary_lookup(&self, _source: Option<SummarySource>) {}
    }

    impl IrProvider for MockEnv {
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

    fn syn(id: u32) -> SyntaxId {
        SyntaxId::from_parts(FileId(0), LocalSyntaxId(id))
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "Constructs synthetic CFG vertices inline for guarded PropWrite + Call callee PropRead regression test."
    )]
    fn collects_guarded_prop_write_and_excludes_direct_call_callee_read() {
        let mut env = MockEnv::new();
        let fn_syn = syn(1);
        let recv_syn = syn(2);
        let cond_syn = syn(3);
        let rhs_syn = syn(4);
        let write_syn = syn(5);
        let method_read_syn = syn(6);
        let call_syn = syn(7);

        env.texts.insert(rhs_syn, "1".to_string());
        env.locs.insert(recv_syn, loc(2, 10, 13));
        env.locs.insert(cond_syn, loc(2, 5, 9));
        env.locs.insert(write_syn, loc(3, 20, 35));
        env.locs.insert(method_read_syn, loc(4, 40, 48));
        env.locs.insert(call_syn, loc(4, 40, 50));

        let vertices = vec![
            CfgVertex {
                id: VertexId(0),
                syntax: Some(fn_syn),
                local_syntax: None,
                symbol: None,
                ty: None,
                signature: None,
                location: Some(loc(1, 0, 60)),
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
                syntax: Some(cond_syn),
                local_syntax: None,
                symbol: None,
                ty: None,
                signature: None,
                location: Some(loc(2, 5, 9)),
                value: Some(ValueId(0)),
                op: IrOp::Branch {
                    condition: ValueId(0),
                },
                explicit_edges: vec![
                    ExplicitEdge {
                        target: VertexId(2),
                        kind: ExplicitEdgeKind::Branch(BranchId(0)),
                    },
                    ExplicitEdge {
                        target: VertexId(8),
                        kind: ExplicitEdgeKind::Branch(BranchId(1)),
                    },
                ],
                may_throw_edge: None,
                enclosing_finallies: Vec::new(),
            },
            CfgVertex {
                id: VertexId(2),
                syntax: Some(recv_syn),
                local_syntax: None,
                symbol: None,
                ty: None,
                signature: None,
                location: Some(loc(2, 10, 13)),
                value: Some(ValueId(1)),
                op: IrOp::ReadLocal {
                    binding: LocalBindingId(0),
                },
                explicit_edges: vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
                may_throw_edge: None,
                enclosing_finallies: Vec::new(),
            },
            CfgVertex {
                id: VertexId(3),
                syntax: Some(rhs_syn),
                local_syntax: None,
                symbol: None,
                ty: None,
                signature: None,
                location: Some(loc(3, 30, 31)),
                value: Some(ValueId(2)),
                op: IrOp::Literal {
                    kind: LiteralKind::Number("1".to_string()),
                    as_const: false,
                },
                explicit_edges: vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
                may_throw_edge: None,
                enclosing_finallies: Vec::new(),
            },
            CfgVertex {
                id: VertexId(4),
                syntax: Some(write_syn),
                local_syntax: None,
                symbol: None,
                ty: None,
                signature: None,
                location: Some(loc(3, 20, 35)),
                value: Some(ValueId(3)),
                op: IrOp::PropWrite {
                    object: ValueId(1),
                    key: IrPropKey::Named("flags".to_string()),
                    op: AssignOp::BitOrAssign,
                    value: ValueId(2),
                    is_super: false,
                    implicit_call_kind: None,
                },
                explicit_edges: vec![ExplicitEdge {
                    target: VertexId(5),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
                may_throw_edge: None,
                enclosing_finallies: Vec::new(),
            },
            CfgVertex {
                id: VertexId(5),
                syntax: Some(method_read_syn),
                local_syntax: None,
                symbol: None,
                ty: None,
                signature: None,
                location: Some(loc(4, 40, 48)),
                value: Some(ValueId(4)),
                op: IrOp::PropRead {
                    object: ValueId(1),
                    key: IrPropKey::Named("run".to_string()),
                    is_super: false,
                    implicit_call_kind: None,
                },
                explicit_edges: vec![ExplicitEdge {
                    target: VertexId(6),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
                may_throw_edge: None,
                enclosing_finallies: Vec::new(),
            },
            CfgVertex {
                id: VertexId(6),
                syntax: Some(call_syn),
                local_syntax: None,
                symbol: None,
                ty: None,
                signature: None,
                location: Some(loc(4, 40, 50)),
                value: Some(ValueId(5)),
                op: IrOp::Call {
                    call_kind: IrCallKind::Explicit,
                    callee: ValueId(4),
                    receiver: Some(ValueId(1)),
                    args: Vec::new(),
                },
                explicit_edges: vec![ExplicitEdge {
                    target: VertexId(8),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
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
                explicit_edges: Vec::new(),
                may_throw_edge: None,
                enclosing_finallies: Vec::new(),
            },
            CfgVertex {
                id: VertexId(8),
                syntax: None,
                local_syntax: None,
                symbol: None,
                ty: None,
                signature: None,
                location: None,
                value: None,
                op: IrOp::Exit,
                explicit_edges: vec![ExplicitEdge {
                    target: VertexId(7),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
                may_throw_edge: None,
                enclosing_finallies: Vec::new(),
            },
        ];

        let ir = Arc::new(FunctionIr {
            id: FunctionIrId {
                file: FileId(0),
                local_scope: 1,
            },
            root_syntax: fn_syn,
            scope_kind: IrScopeKind::Function,
            scope_location: loc(1, 0, 60),
            decl_semantics: DeclarationSemantics::default(),
            type_guard: None,
            parameters: Vec::new(),
            bindings: vec![IrLocalBinding {
                id: LocalBindingId(0),
                name: "job".to_string(),
                symbol: None,
                syntax: recv_syn,
                mutability: BindingMutability::Let,
            }],
            captured_bindings: Vec::new(),
            vertices,
            entry: VertexId(0),
            normal_exit: VertexId(8),
            exceptional_exit: VertexId(8),
            exit: VertexId(7),
            syntax_to_vertex: FxHashMap::default(),
            expr_to_vertex: FxHashMap::default(),
            expr_to_value: FxHashMap::default(),
            value_sources: FxHashMap::default(),
            branches: vec![
                IrBranchRecord {
                    id: BranchId(0),
                    kind: IrBranchKind::If,
                    direction: IrBranchDirection::True,
                    condition_syntax: cond_syn,
                    condition_text: "ok".to_string(),
                    location: loc(2, 5, 9),
                    statement_location: loc(2, 1, 36),
                },
                IrBranchRecord {
                    id: BranchId(1),
                    kind: IrBranchKind::If,
                    direction: IrBranchDirection::False,
                    condition_syntax: cond_syn,
                    condition_text: "ok".to_string(),
                    location: loc(2, 5, 9),
                    statement_location: loc(2, 1, 36),
                },
            ],
            value_count: 6,
        });
        env.irs.insert(fn_syn, ir);

        let batch = analyze_call_effects_on_ir(
            &[CallEffectsTargetSpec {
                syntax: fn_syn,
                symbol: None,
            }],
            CallEffectsOptions::default(),
            &[FileId(0)],
            &env,
            &env,
            &env,
            &BehaviorSummaryIndex::default(),
            None,
        );
        assert!(batch.conflict.is_none());
        let outcome = &batch.results[0];
        match outcome {
            CallEffectsOutcome::Incomplete {
                field_reads,
                field_writes,
                gaps,
                ..
            } => {
                // `job.run` direct callee PropRead must be excluded from fieldReads.
                assert_eq!(field_reads.len(), 0);
                assert_eq!(field_writes.len(), 1);
                assert_eq!(field_writes[0].property_name, "flags");
                assert_eq!(field_writes[0].operator, Some("|="));
                assert_eq!(field_writes[0].guards.len(), 1);
                assert_eq!(field_writes[0].guards[0].condition_text, "ok");
                assert_eq!(field_writes[0].guards[0].direction, "true");
                // Dynamic unresolved `job.run()` produces a gap.
                assert_ne!(gaps.len(), 0);
            }
            CallEffectsOutcome::Complete { .. } => {
                panic!("expected Incomplete for dynamic job.run()")
            }
        }
    }
}
