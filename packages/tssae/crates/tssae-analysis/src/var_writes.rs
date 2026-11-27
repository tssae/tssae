use std::{collections::VecDeque, sync::Arc};

use rustc_hash::{FxHashMap, FxHashSet};
use tssae_common::{
    DiagnosticLocation, FunctionIrId, LocalBindingId, SemanticModel, SymbolId, SyntaxCatalog,
    SyntaxId, ValueId, VertexId,
};

use crate::ir::{
    AssignOp, BindingMutability, ExplicitEdgeKind, FunctionIr, IrBranchDirection, IrBranchKind,
    IrOp, IrProvider, IrScopeKind, LiteralKind,
};

/// Value source written at a variable write site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarWriteSource {
    /// Explicit expression value evaluated in the write's owning [`FunctionIr`].
    Value(ValueId),
    /// Uninitialized declaration (`let x;`) without an initializer expression.
    Uninitialized,
    /// Hoisted function declaration bound to `decl_syntax`.
    FunctionDecl(SyntaxId),
    /// Value bound from a `catch (e)` parameter or otherwise opaque pattern.
    Opaque,
}

/// Single initialization or reassignment site of a lexical variable.
#[derive(Clone, Debug)]
pub struct VarWriteSite {
    pub scope_ir: Arc<FunctionIr>,
    pub vertex: VertexId,
    pub syntax: Option<SyntaxId>,
    pub location: Option<DiagnosticLocation>,
    pub op: AssignOp,
    pub source: VarWriteSource,
    pub is_init: bool,
    pub in_closure: bool,
}

/// Exhaustive or reaching write summary for a variable across scopes and assignment forms.
#[derive(Clone, Debug)]
pub struct VarWritesResult {
    pub decl_syntax: SyntaxId,
    pub symbol: Option<SymbolId>,
    pub mutability: BindingMutability,
    pub writes: Vec<VarWriteSite>,
    pub has_closure_writes: bool,
    pub entry_param_reaches: bool,
    pub is_exhaustive: bool,
}

/// Collects all write sites to the local binding `binding` inside `decl_ir` and any nested scopes.
#[must_use]
pub fn collect_all_writes_for_local(
    decl_ir: &Arc<FunctionIr>,
    binding: LocalBindingId,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
) -> VarWritesResult {
    let Some(binding_info) = decl_ir.bindings.get(binding.0 as usize) else {
        return VarWritesResult {
            decl_syntax: decl_ir.root_syntax,
            symbol: None,
            mutability: BindingMutability::Let,
            writes: Vec::new(),
            has_closure_writes: false,
            entry_param_reaches: false,
            is_exhaustive: false,
        };
    };

    let decl_syntax = binding_info.syntax;
    let symbol = binding_info
        .symbol
        .or_else(|| semantic.symbol_at_syntax(decl_syntax));
    let mutability = binding_info.mutability;
    let decl_name = binding_info.name.clone();

    let mut writes =
        collect_writes_in_scope(decl_ir, Some(binding), decl_syntax, symbol, semantic, false);

    let mut has_closure_writes = false;
    let mut is_exhaustive = true;

    if is_mutable_binding(mutability) {
        let nested = collect_closure_writes(
            decl_ir,
            Some(binding),
            decl_syntax,
            symbol,
            &decl_name,
            semantic,
            syntax,
            ir_provider,
            &mut is_exhaustive,
        );
        if !nested.is_empty() {
            has_closure_writes = true;
            writes.extend(nested);
        }
    }

    VarWritesResult {
        decl_syntax,
        symbol,
        mutability,
        writes,
        has_closure_writes,
        entry_param_reaches: mutability == BindingMutability::Parameter,
        is_exhaustive,
    }
}

/// Collects all write sites for a declaration identified by `decl_syntax` across its declaring scope and nested scopes.
#[must_use]
pub fn collect_all_writes_for_decl(
    decl_syntax: SyntaxId,
    symbol: Option<SymbolId>,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
) -> VarWritesResult {
    if let Ok(decl_ir) = ir_provider.enclosing_scope_ir(decl_syntax) {
        if let Some(binding_info) = decl_ir.bindings.iter().find(|b| {
            b.syntax == decl_syntax
                || (symbol.is_some()
                    && (b.symbol == symbol || semantic.symbol_at_syntax(b.syntax) == symbol))
                || semantic
                    .resolve_declaration(b.syntax)
                    .and_then(|d| d.primary_syntax)
                    == Some(decl_syntax)
        }) {
            return collect_all_writes_for_local(
                &decl_ir,
                binding_info.id,
                semantic,
                syntax,
                ir_provider,
            );
        }

        let resolved = semantic.resolve_declaration(decl_syntax);
        let decl_name = resolved
            .as_ref()
            .and_then(|d| d.name.clone())
            .or_else(|| syntax.syntax_text(decl_syntax))
            .unwrap_or_default();
        let mut writes =
            collect_writes_in_scope(&decl_ir, None, decl_syntax, symbol, semantic, false);
        let mut is_exhaustive = true;
        let nested = collect_closure_writes(
            &decl_ir,
            None,
            decl_syntax,
            symbol,
            &decl_name,
            semantic,
            syntax,
            ir_provider,
            &mut is_exhaustive,
        );
        let has_closure_writes = !nested.is_empty();
        writes.extend(nested);
        return VarWritesResult {
            decl_syntax,
            symbol,
            mutability: BindingMutability::Let,
            writes,
            has_closure_writes,
            entry_param_reaches: false,
            is_exhaustive,
        };
    }

    VarWritesResult {
        decl_syntax,
        symbol,
        mutability: BindingMutability::Let,
        writes: Vec::new(),
        has_closure_writes: false,
        entry_param_reaches: false,
        is_exhaustive: false,
    }
}

/// Resolves the variable writes that can reach `read_vertex` in `read_ir` for local binding `binding`.
#[must_use]
pub fn resolve_reaching_writes_for_local(
    read_ir: &Arc<FunctionIr>,
    read_vertex: VertexId,
    binding: LocalBindingId,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
) -> VarWritesResult {
    let all = collect_all_writes_for_local(read_ir, binding, semantic, syntax, ir_provider);

    if !is_mutable_binding(all.mutability)
        || all
            .writes
            .iter()
            .any(|w| matches!(w.source, VarWriteSource::FunctionDecl(_)))
    {
        return VarWritesResult {
            entry_param_reaches: false,
            ..all
        };
    }

    let local_writes: Vec<VarWriteSite> = all
        .writes
        .iter()
        .filter(|w| w.scope_ir.id == read_ir.id && !w.in_closure)
        .cloned()
        .collect();
    let closure_writes: Vec<VarWriteSite> = all
        .writes
        .iter()
        .filter(|w| w.in_closure || w.scope_ir.id != read_ir.id)
        .cloned()
        .collect();

    let (mut reaching_local, entry_param_reaches) =
        compute_reaching_local_writes(read_ir, read_vertex, binding, &local_writes);

    if all.has_closure_writes {
        // A closure write may execute before or between calls, so keep both closure writes and reaching local writes.
        let mut seen: FxHashSet<(FunctionIrId, VertexId)> = reaching_local
            .iter()
            .map(|w| (w.scope_ir.id, w.vertex))
            .collect();
        for cw in closure_writes {
            if seen.insert((cw.scope_ir.id, cw.vertex)) {
                reaching_local.push(cw);
            }
        }
    }

    VarWritesResult {
        decl_syntax: all.decl_syntax,
        symbol: all.symbol,
        mutability: all.mutability,
        writes: reaching_local,
        has_closure_writes: all.has_closure_writes,
        entry_param_reaches,
        is_exhaustive: all.is_exhaustive,
    }
}

/// Resolves writes for a symbol-referenced variable `ident_syntax` read inside `read_ir`.
#[must_use]
pub fn resolve_reaching_writes_for_symbol(
    decl_syntax: SyntaxId,
    symbol: Option<SymbolId>,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
) -> VarWritesResult {
    collect_all_writes_for_decl(decl_syntax, symbol, semantic, syntax, ir_provider)
}

const fn is_mutable_binding(mutability: BindingMutability) -> bool {
    matches!(
        mutability,
        BindingMutability::Let
            | BindingMutability::Var
            | BindingMutability::Parameter
            | BindingMutability::CatchBinding
    )
}

fn collect_writes_in_scope(
    scope_ir: &Arc<FunctionIr>,
    binding: Option<LocalBindingId>,
    decl_syntax: SyntaxId,
    decl_symbol: Option<SymbolId>,
    semantic: &dyn SemanticModel,
    in_closure: bool,
) -> Vec<VarWriteSite> {
    let mut producers: FxHashSet<ValueId> = FxHashSet::default();
    for v in &scope_ir.vertices {
        if let Some(val) = v.value
            && !matches!(
                v.op,
                IrOp::AssignLocal { is_init: true, .. } | IrOp::AssignSymbol { is_init: true, .. }
            )
        {
            producers.insert(val);
        }
    }
    for &val in scope_ir.value_sources.keys() {
        producers.insert(val);
    }

    let mut writes = Vec::new();
    for vertex in &scope_ir.vertices {
        match &vertex.op {
            IrOp::AssignLocal {
                target,
                op,
                value,
                is_init,
            } if binding == Some(*target) => {
                let is_uninit =
                    *is_init && vertex.value == Some(*value) && !producers.contains(value);
                let source = if is_uninit {
                    VarWriteSource::Uninitialized
                } else {
                    VarWriteSource::Value(*value)
                };
                writes.push(VarWriteSite {
                    scope_ir: Arc::clone(scope_ir),
                    vertex: vertex.id,
                    syntax: vertex.syntax,
                    location: vertex.location.clone(),
                    op: *op,
                    source,
                    is_init: *is_init,
                    in_closure,
                });
            }
            IrOp::AssignSymbol {
                target_syntax,
                op,
                value,
                is_init,
            } if matches_decl(*target_syntax, decl_syntax, decl_symbol, semantic) => {
                let is_uninit =
                    *is_init && vertex.value == Some(*value) && !producers.contains(value);
                let source = if is_uninit {
                    VarWriteSource::Uninitialized
                } else {
                    VarWriteSource::Value(*value)
                };
                writes.push(VarWriteSite {
                    scope_ir: Arc::clone(scope_ir),
                    vertex: vertex.id,
                    syntax: vertex.syntax,
                    location: vertex.location.clone(),
                    op: *op,
                    source,
                    is_init: *is_init,
                    in_closure,
                });
            }
            IrOp::CatchParamBind { binding: Some(b) } if binding == Some(*b) => {
                writes.push(VarWriteSite {
                    scope_ir: Arc::clone(scope_ir),
                    vertex: vertex.id,
                    syntax: vertex.syntax,
                    location: vertex.location.clone(),
                    op: AssignOp::Assign,
                    source: VarWriteSource::Opaque,
                    is_init: true,
                    in_closure,
                });
            }
            IrOp::FunctionLiteral { syntax: fn_syn, .. } if *fn_syn == decl_syntax => {
                writes.push(VarWriteSite {
                    scope_ir: Arc::clone(scope_ir),
                    vertex: vertex.id,
                    syntax: vertex.syntax,
                    location: vertex.location.clone(),
                    op: AssignOp::Assign,
                    source: VarWriteSource::FunctionDecl(*fn_syn),
                    is_init: true,
                    in_closure,
                });
            }
            _ => {}
        }
    }
    writes
}

fn matches_decl(
    target_syntax: SyntaxId,
    decl_syntax: SyntaxId,
    decl_symbol: Option<SymbolId>,
    semantic: &dyn SemanticModel,
) -> bool {
    if target_syntax == decl_syntax {
        return true;
    }
    let Some(resolved) = semantic.resolve_declaration(target_syntax) else {
        return false;
    };
    if resolved.primary_syntax == Some(decl_syntax) {
        return true;
    }
    if let (Some(expected_sym), Some(actual_sym)) = (decl_symbol, resolved.symbol) {
        return expected_sym == actual_sym;
    }
    false
}

#[expect(
    clippy::too_many_arguments,
    reason = "Coordinates nested scope filtering, text pre-checks, and cross-scope IR traversal."
)]
fn collect_closure_writes(
    decl_ir: &FunctionIr,
    binding: Option<LocalBindingId>,
    decl_syntax: SyntaxId,
    decl_symbol: Option<SymbolId>,
    decl_name: &str,
    semantic: &dyn SemanticModel,
    syntax: &dyn SyntaxCatalog,
    ir_provider: &dyn IrProvider,
    is_exhaustive: &mut bool,
) -> Vec<VarWriteSite> {
    let mut capture_write_flags: FxHashMap<SyntaxId, bool> = FxHashMap::default();
    let mut has_class_literal = false;
    for vertex in &decl_ir.vertices {
        match &vertex.op {
            IrOp::FunctionLiteral {
                syntax: fn_syn,
                captures,
            } => {
                let writes_binding = captures.iter().any(|c| {
                    ((binding.is_some() && c.outer_binding == binding)
                        || c.decl_syntax == decl_syntax)
                        && c.is_written_in_closure
                });
                capture_write_flags.insert(*fn_syn, writes_binding);
            }
            IrOp::ClassLiteral { .. } => {
                has_class_literal = true;
            }
            _ => {}
        }
    }

    let scopes = ir_provider.scopes_in_file(decl_ir.id.file);
    let is_module_scope = decl_ir.scope_kind == IrScopeKind::ModuleTopLevel;
    let outer_start = decl_ir.scope_location.start;
    let outer_end = decl_ir.scope_location.end;

    let mut out = Vec::new();
    for entry in scopes {
        if entry.root_syntax == decl_ir.root_syntax {
            continue;
        }
        if !is_module_scope && (entry.span_start < outer_start || entry.span_end > outer_end) {
            continue;
        }
        if let Some(&written) = capture_write_flags.get(&entry.root_syntax)
            && !written
            && !has_class_literal
        {
            continue;
        }
        if !decl_name.is_empty()
            && let Some(scope_text) = syntax.syntax_text(entry.root_syntax)
            && !scope_text.contains(decl_name)
        {
            continue;
        }

        let Ok(nested_ir) = ir_provider.function_ir(entry.root_syntax) else {
            *is_exhaustive = false;
            continue;
        };
        out.extend(collect_writes_in_scope(
            &nested_ir,
            None,
            decl_syntax,
            decl_symbol,
            semantic,
            true,
        ));
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum DefTag {
    EntryParam,
    LocalWrite(usize),
}

fn compute_reaching_local_writes(
    ir: &FunctionIr,
    read_vertex: VertexId,
    binding: LocalBindingId,
    local_writes: &[VarWriteSite],
) -> (Vec<VarWriteSite>, bool) {
    let mut write_index_by_vid: FxHashMap<VertexId, usize> = FxHashMap::default();
    for (idx, w) in local_writes.iter().enumerate() {
        write_index_by_vid.insert(w.vertex, idx);
    }

    let is_param = ir
        .bindings
        .get(binding.0 as usize)
        .is_some_and(|b| b.mutability == BindingMutability::Parameter);

    let n = ir.vertices.len();
    let mut in_states: Vec<FxHashSet<DefTag>> = vec![FxHashSet::default(); n];
    let mut visited: Vec<bool> = vec![false; n];
    let mut queue = VecDeque::new();

    if is_param {
        in_states[ir.entry.0 as usize].insert(DefTag::EntryParam);
    }
    visited[ir.entry.0 as usize] = true;
    queue.push_back(ir.entry);

    while let Some(vid) = queue.pop_front() {
        let idx = vid.0 as usize;
        let mut out_set = in_states[idx].clone();

        if let Some(&write_idx) = write_index_by_vid.get(&vid) {
            let site = &local_writes[write_idx];
            if site.op == AssignOp::Assign {
                out_set.clear();
            }
            // Compound assignment keeps prior possibilities and also records the write site.
            out_set.insert(DefTag::LocalWrite(write_idx));
        }

        let Some(vertex) = ir.vertices.get(idx) else {
            continue;
        };

        for edge in &vertex.explicit_edges {
            let mut filtered = out_set.clone();
            if let ExplicitEdgeKind::Branch(branch_id) = edge.kind
                && let Some(branch) = ir.branches.get(branch_id.0 as usize)
                && branch.direction == IrBranchDirection::ShortCircuit
            {
                filter_short_circuit_defs(ir, branch.kind, local_writes, &mut filtered);
            }
            propagate_to_target(
                edge.target,
                &filtered,
                &mut in_states,
                &mut visited,
                &mut queue,
            );
        }

        if let Some(may_throw) = vertex.may_throw_edge {
            propagate_to_target(
                may_throw.target,
                &out_set,
                &mut in_states,
                &mut visited,
                &mut queue,
            );
        }
    }

    let read_state = in_states
        .get(read_vertex.0 as usize)
        .cloned()
        .unwrap_or_default();
    let entry_param_reaches = read_state.contains(&DefTag::EntryParam);
    let mut out_writes = Vec::new();
    for (idx, site) in local_writes.iter().enumerate() {
        if read_state.contains(&DefTag::LocalWrite(idx)) {
            out_writes.push(site.clone());
        }
    }
    (out_writes, entry_param_reaches)
}

fn propagate_to_target(
    target: VertexId,
    incoming: &FxHashSet<DefTag>,
    in_states: &mut [FxHashSet<DefTag>],
    visited: &mut [bool],
    queue: &mut VecDeque<VertexId>,
) {
    let tidx = target.0 as usize;
    let Some(slot) = in_states.get_mut(tidx) else {
        return;
    };
    let before = slot.len();
    slot.extend(incoming.iter().copied());
    if !visited[tidx] || slot.len() > before {
        visited[tidx] = true;
        queue.push_back(target);
    }
}

fn filter_short_circuit_defs(
    ir: &FunctionIr,
    branch_kind: IrBranchKind,
    local_writes: &[VarWriteSite],
    defs: &mut FxHashSet<DefTag>,
) {
    defs.retain(|tag| {
        let DefTag::LocalWrite(idx) = tag else {
            return true;
        };
        let site = &local_writes[*idx];
        match branch_kind {
            IrBranchKind::LogicalOr | IrBranchKind::NullishCoalescing => {
                !is_definitely_nullish_or_uninit(ir, site.source)
            }
            IrBranchKind::LogicalAnd => !is_definitely_truthy_callable_or_object(ir, site.source),
            _ => true,
        }
    });
}

fn is_definitely_nullish_or_uninit(ir: &FunctionIr, source: VarWriteSource) -> bool {
    match source {
        VarWriteSource::Uninitialized => true,
        VarWriteSource::Value(val) => {
            if let Some(sources) = ir.value_sources.get(&val)
                && !sources.is_empty()
            {
                return sources
                    .iter()
                    .all(|&s| is_definitely_nullish_or_uninit(ir, VarWriteSource::Value(s)));
            }
            ir.vertices.iter().any(|v| {
                v.value == Some(val)
                    && match &v.op {
                        IrOp::Literal {
                            kind: LiteralKind::Null | LiteralKind::Undefined,
                            ..
                        } => true,
                        IrOp::ValueForward { source, .. } => {
                            is_definitely_nullish_or_uninit(ir, VarWriteSource::Value(*source))
                        }
                        _ => false,
                    }
            })
        }
        VarWriteSource::FunctionDecl(_) | VarWriteSource::Opaque => false,
    }
}

fn is_definitely_truthy_callable_or_object(ir: &FunctionIr, source: VarWriteSource) -> bool {
    match source {
        VarWriteSource::FunctionDecl(_) => true,
        VarWriteSource::Value(val) => ir.vertices.iter().any(|v| {
            v.value == Some(val)
                && matches!(
                    v.op,
                    IrOp::FunctionLiteral { .. }
                        | IrOp::ClassLiteral { .. }
                        | IrOp::ObjectAlloc { .. }
                        | IrOp::ArrayAlloc { .. }
                )
        }),
        VarWriteSource::Uninitialized | VarWriteSource::Opaque => false,
    }
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;
    use tssae_common::{
        BranchId, DeclarationSummaryData, DiagnosticLocation, FileId, FunctionIrId, GapItem,
        ImplementationFact, LocalBindingId, LocalSyntaxId, MemberTargetHierarchy, SemanticModel,
        SignatureId, SymbolId, SyntaxCatalog, SyntaxId, TypeId, TypeSummaryData, ValueId, VertexId,
    };

    use super::*;
    use crate::ir::{
        CfgVertex, DeclarationSemantics, ExplicitEdge, IrBranchRecord, IrLocalBinding,
        ScopeCatalogEntry,
    };

    struct DummyEnv;

    impl SyntaxCatalog for DummyEnv {
        fn syntax_location(&self, _syntax: SyntaxId) -> Option<DiagnosticLocation> {
            None
        }
        fn syntax_text(&self, _syntax: SyntaxId) -> Option<String> {
            None
        }
        fn syntax_kind(&self, _syntax: SyntaxId) -> Option<String> {
            None
        }
        fn file_path(&self, _file: FileId) -> Option<&str> {
            None
        }
    }

    impl SemanticModel for DummyEnv {
        fn symbol_at_syntax(&self, _syntax: SyntaxId) -> Option<SymbolId> {
            None
        }
        fn type_at_syntax(&self, _syntax: SyntaxId) -> Option<TypeSummaryData> {
            None
        }
        fn signature_at_syntax(&self, _syntax: SyntaxId) -> Option<SignatureId> {
            None
        }
        fn resolve_declaration(&self, _syntax: SyntaxId) -> Option<DeclarationSummaryData> {
            None
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
    }

    impl IrProvider for DummyEnv {
        fn function_ir(&self, _scope_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
            Err(GapItem::new("missing-function-body", String::new(), None))
        }
        fn enclosing_scope_ir(&self, _node_syntax: SyntaxId) -> Result<Arc<FunctionIr>, GapItem> {
            Err(GapItem::new("missing-function-body", String::new(), None))
        }
        fn file_scope_ir(&self, _file: FileId) -> Option<Arc<FunctionIr>> {
            None
        }
        fn scopes_in_file(&self, _file: FileId) -> Vec<ScopeCatalogEntry> {
            Vec::new()
        }
    }

    fn mk_vertex(id: u32, op: IrOp, value: Option<ValueId>, edges: Vec<ExplicitEdge>) -> CfgVertex {
        CfgVertex {
            id: VertexId(id),
            syntax: None,
            local_syntax: None,
            symbol: None,
            ty: None,
            signature: None,
            location: None,
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
        reason = "Constructs an explicit synthetic FunctionIr CFG to verify ||= reaching definitions."
    )]
    fn logical_or_assignment_preserves_both_init_and_fallback_writes() {
        let loc = DiagnosticLocation {
            file: "test.ts".to_string(),
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
            start: 0,
            end: 50,
        };
        let b0 = LocalBindingId(0);
        // 0: Entry -> 4
        // 1: normal_exit -> 3
        // 2: exceptional_exit -> 3
        // 3: Exit
        // 4: lit_a (val 0) -> 5
        // 5: init b0 = val 0 -> 6
        // 6: branch gate -> Branch(0) ShortCircuit -> 9, Branch(1) Continue -> 7
        // 7: lit_b (val 1) -> 8
        // 8: assign b0 = val 1 -> 9
        // 9: read b0 -> 1
        let vertices = vec![
            mk_vertex(
                0,
                IrOp::Entry,
                None,
                vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                1,
                IrOp::NopMerge,
                None,
                vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(
                2,
                IrOp::NopMerge,
                None,
                vec![ExplicitEdge {
                    target: VertexId(3),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(3, IrOp::Exit, None, Vec::new()),
            mk_vertex(
                4,
                IrOp::FunctionLiteral {
                    syntax: SyntaxId::from_parts(FileId(0), LocalSyntaxId(10)),
                    captures: Vec::new(),
                },
                Some(ValueId(0)),
                vec![ExplicitEdge {
                    target: VertexId(5),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                5,
                IrOp::AssignLocal {
                    target: b0,
                    op: AssignOp::Assign,
                    value: ValueId(0),
                    is_init: true,
                },
                Some(ValueId(0)),
                vec![ExplicitEdge {
                    target: VertexId(6),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                6,
                IrOp::NopMerge,
                None,
                vec![
                    ExplicitEdge {
                        target: VertexId(9),
                        kind: ExplicitEdgeKind::Branch(BranchId(0)),
                    },
                    ExplicitEdge {
                        target: VertexId(7),
                        kind: ExplicitEdgeKind::Branch(BranchId(1)),
                    },
                ],
            ),
            mk_vertex(
                7,
                IrOp::FunctionLiteral {
                    syntax: SyntaxId::from_parts(FileId(0), LocalSyntaxId(11)),
                    captures: Vec::new(),
                },
                Some(ValueId(1)),
                vec![ExplicitEdge {
                    target: VertexId(8),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                8,
                IrOp::AssignLocal {
                    target: b0,
                    op: AssignOp::Assign,
                    value: ValueId(1),
                    is_init: false,
                },
                Some(ValueId(1)),
                vec![ExplicitEdge {
                    target: VertexId(9),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                9,
                IrOp::ReadLocal { binding: b0 },
                Some(ValueId(2)),
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
        ];
        let ir = Arc::new(FunctionIr {
            id: FunctionIrId {
                file: FileId(0),
                local_scope: 0,
            },
            root_syntax: SyntaxId(1),
            scope_kind: IrScopeKind::Function,
            scope_location: loc.clone(),
            decl_semantics: DeclarationSemantics::default(),
            type_guard: None,
            parameters: Vec::new(),
            bindings: vec![IrLocalBinding {
                id: b0,
                name: "fn".to_string(),
                mutability: BindingMutability::Let,
                syntax: SyntaxId(2),
                symbol: None,
            }],
            captured_bindings: Vec::new(),
            vertices,
            entry: VertexId(0),
            normal_exit: VertexId(1),
            exceptional_exit: VertexId(2),
            exit: VertexId(3),
            syntax_to_vertex: FxHashMap::default(),
            expr_to_vertex: FxHashMap::default(),
            expr_to_value: FxHashMap::default(),
            value_sources: FxHashMap::default(),
            branches: vec![
                IrBranchRecord {
                    id: BranchId(0),
                    kind: IrBranchKind::LogicalOr,
                    direction: IrBranchDirection::ShortCircuit,
                    condition_syntax: SyntaxId(3),
                    condition_text: "fn".to_string(),
                    location: loc.clone(),
                    statement_location: loc.clone(),
                },
                IrBranchRecord {
                    id: BranchId(1),
                    kind: IrBranchKind::LogicalOr,
                    direction: IrBranchDirection::Continue,
                    condition_syntax: SyntaxId(3),
                    condition_text: "fn".to_string(),
                    location: loc.clone(),
                    statement_location: loc,
                },
            ],
            value_count: 3,
        });

        let env = DummyEnv;
        let result = resolve_reaching_writes_for_local(&ir, VertexId(9), b0, &env, &env, &env);
        assert_eq!(result.writes.len(), 2);
        assert_eq!(result.writes[0].source, VarWriteSource::Value(ValueId(0)));
        assert_eq!(result.writes[1].source, VarWriteSource::Value(ValueId(1)));
    }
}
