use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use tsrs_ast::{
    Kind, ModifierFlags, Node, NodeFlags, SourceFile, Symbol, get_combined_modifier_flags,
    get_source_file_of_node, is_const_type_reference, is_function_like_declaration,
    is_optional_chain, is_type_node,
};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;
use tssae_analysis::ir::{
    AssignOp, BinaryOp, BindingMutability, CapturedBinding, CfgVertex, ClassFieldInit, ClassIr,
    DeclarationSemantics, ExplicitEdge, ExplicitEdgeKind, FunctionIr, ImplicitAccessorKind,
    IrArrayElem, IrBranchDirection, IrBranchKind, IrBranchRecord, IrCallArg, IrCallKind,
    IrLocalBinding, IrObjectProp, IrOp, IrParameter, IrPropKey, IrScopeKind, LiteralKind,
    MayThrowEdge, MayThrowReason, MemberVisibility, TypeGuardSemantics, UnaryOp,
};
use tssae_common::{
    BranchId, DiagnosticLocation, FileId, FinallyId, FunctionIrId, GapItem, LocalBindingId,
    LocalSyntaxId, SyntaxId, UnknownReason, ValueId, VertexId,
};

use crate::semantic_facade::SemanticModelBridge;
use crate::syntax_view::{
    FileSyntaxIndex, node_location, node_text, prop_key_text, relative_to_cwd, unwrap_expr_wrappers,
};

/// Boundary-side lowered function IR paired with bidirectional AST node <-> `SyntaxId` tables.
///
/// The inner [`FunctionIr`] is completely free of `tsrs` arena pointers (`P<Node>`) so it can
/// reside in `tssae-analysis`, while the boundary keeps this lookup map for RPC handle encoding.
#[derive(Debug)]
pub struct LoweredScope {
    pub ir: Arc<FunctionIr>,
    pub node_to_syntax: FxHashMap<P<Node>, SyntaxId>,
    pub syntax_to_node: FxHashMap<SyntaxId, P<Node>>,
}

/// Resolves the closest enclosing function-like declaration for `node`, stopping at class boundaries.
#[must_use]
pub fn resolve_enclosing_function(node: P<Node>) -> Option<P<Node>> {
    let mut current = node.parent();
    while let Some(candidate) = current {
        if is_function_like_declaration(Some(candidate)) {
            return Some(candidate);
        }
        if matches!(
            candidate.kind(),
            Kind::ClassDeclaration | Kind::ClassExpression
        ) {
            return None;
        }
        current = candidate.parent();
    }
    None
}

/// Normalizes a function-like node or its direct body `Block` to the owning function-like node.
#[must_use]
pub fn normalize_function_node(node: P<Node>) -> Option<P<Node>> {
    if is_function_like_declaration(Some(node)) {
        return Some(node);
    }
    if node.kind() == Kind::Block
        && let Some(parent) = node.parent()
        && is_function_like_declaration(Some(parent))
        && parent.body() == Some(node)
    {
        return Some(parent);
    }
    None
}

/// Normalizes a function-like node, block, class static block, or statement suitable as a CFG root scope.
#[must_use]
pub fn normalize_cfg_scope_node(node: P<Node>) -> Option<P<Node>> {
    if let Some(fn_node) = normalize_function_node(node) {
        return Some(fn_node);
    }
    if matches!(
        node.kind(),
        Kind::Block | Kind::ModuleBlock | Kind::ClassStaticBlockDeclaration | Kind::SourceFile
    ) || tsrs_ast::is_statement(node)
    {
        return Some(node);
    }
    None
}

/// Lowers a normalized function, block, class static block, or statement scope into a verified [`LoweredScope`].
///
/// # Errors
/// Returns a [`GapItem`] if the scope node has no source file, has no body (e.g., ambient
/// declaration), or is not a valid CFG scope.
pub fn lower_scope_to_ir(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    scope_node: P<Node>,
    local_scope: u32,
) -> Result<LoweredScope, GapItem> {
    lower_scope_to_ir_with_index(cwd, program, checker, scope_node, local_scope, None)
}

/// Lowers a normalized scope using an optional prebuilt [`FileSyntaxIndex`] for cross-snapshot stable IDs.
///
/// # Errors
/// Returns a [`GapItem`] if the scope node has no source file, has no body, or fails invariant checks.
pub fn lower_scope_to_ir_with_index(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    scope_node: P<Node>,
    local_scope: u32,
    file_index: Option<&FileSyntaxIndex>,
) -> Result<LoweredScope, GapItem> {
    let Some(sf) = get_source_file_of_node(scope_node) else {
        return Err(GapItem::from_reason(
            UnknownReason::MissingSourceFile,
            "Function node has no associated source file.".to_string(),
            None,
        ));
    };
    let rel_file = relative_to_cwd(cwd, sf.file_name());
    let scope_location = node_location(sf, &rel_file, scope_node);

    let normalized_fn = normalize_function_node(scope_node);
    let is_static_block = scope_node.kind() == Kind::ClassStaticBlockDeclaration;
    let is_source_file = scope_node.kind() == Kind::SourceFile;
    if normalized_fn.is_none()
        && !is_static_block
        && !is_source_file
        && !matches!(scope_node.kind(), Kind::Block | Kind::ModuleBlock)
        && !tsrs_ast::is_statement(scope_node)
    {
        return Err(GapItem::from_reason(
            UnknownReason::InvalidCfgScope,
            "CFG scope node must be a function, block, static block, or statement.".to_string(),
            Some(scope_location),
        ));
    }

    let body_opt = normalized_fn.map(|fn_node| fn_node.body());
    if body_opt == Some(None) {
        return Err(GapItem::from_reason(
            UnknownReason::MissingFunctionBody,
            "Function declaration has no body to analyze.".to_string(),
            Some(scope_location),
        ));
    }

    let scope_ir = {
        let bridge = match file_index {
            Some(idx) => SemanticModelBridge::with_file_index(cwd, program, checker, idx),
            None => SemanticModelBridge::new(cwd, program, checker),
        };
        lower_validated_scope_inner(
            sf,
            &rel_file,
            Some(&bridge),
            scope_node,
            normalized_fn,
            is_static_block,
            is_source_file,
            scope_location,
            local_scope,
            file_index,
        )?
    };
    if let Err(msg) = scope_ir.ir.validate_invariants() {
        return Err(GapItem::from_reason(
            UnknownReason::UnmodeledConstruct,
            format!("IR CFG invariant violation: {msg}"),
            Some(scope_ir.ir.scope_location.clone()),
        ));
    }
    Ok(scope_ir)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Internal helper shared between full semantic lowering and syntax-indexed lowering."
)]
fn lower_validated_scope_inner(
    sf: P<SourceFile>,
    rel_file: &str,
    bridge: Option<&SemanticModelBridge<'_>>,
    scope_node: P<Node>,
    normalized_fn: Option<P<Node>>,
    is_static_block: bool,
    is_source_file: bool,
    scope_location: DiagnosticLocation,
    local_scope: u32,
    file_index: Option<&FileSyntaxIndex>,
) -> Result<LoweredScope, GapItem> {
    let mut lowerer = ScopeLowerer::new(
        sf,
        rel_file,
        bridge,
        scope_node,
        scope_location,
        local_scope,
        file_index,
    );

    if let Some(fn_node) = normalized_fn {
        let body = fn_node.body().ok_or_else(|| {
            GapItem::from_reason(
                UnknownReason::MissingFunctionBody,
                "Function declaration has no body to analyze.".to_string(),
                Some(lowerer.scope_location.clone()),
            )
        })?;

        lowerer.type_guard = bridge.and_then(|b| b.type_guard_for_function(fn_node));
        lowerer.collect_parameters(fn_node);
        lowerer.preregister_body_locals(body);
        lowerer.captured_bindings = lowerer.collect_own_captured_bindings(fn_node);

        let enclosing_class = if fn_node.kind() == Kind::Constructor {
            fn_node
                .parent()
                .filter(|p| matches!(p.kind(), Kind::ClassDeclaration | Kind::ClassExpression))
        } else {
            None
        };
        let ctor_has_super =
            enclosing_class.is_some_and(class_has_extends) && subtree_contains_super_call(body);
        lowerer.active_ctor_class = if ctor_has_super {
            enclosing_class
        } else {
            None
        };

        let mut body_entry = if body.kind() == Kind::Block {
            lowerer.lower_stmt(body, lowerer.normal_exit)
        } else {
            let ret_val = lowerer.alloc_value();
            let ret_v = lowerer.new_vertex(
                None,
                IrOp::Return {
                    value: Some(ret_val),
                },
                None,
                false,
            );
            lowerer.add_edge(ret_v, lowerer.normal_exit, ExplicitEdgeKind::Return);
            let (expr_entry, expr_val) = lowerer.lower_expr_to(body, ret_v);
            if let Some(v) = lowerer.vertices.get_mut(ret_v.0 as usize)
                && let IrOp::Return { value } = &mut v.op
            {
                *value = Some(expr_val);
            }
            expr_entry
        };
        lowerer.active_ctor_class = None;

        if let Some(class_node) = enclosing_class
            && !ctor_has_super
        {
            body_entry = lowerer.lower_class_instance_init(class_node, Some(fn_node), body_entry);
        }

        let start = lowerer.lower_function_parameters(fn_node, body_entry);
        lowerer.add_edge(lowerer.entry, start, ExplicitEdgeKind::Fallthrough);
    } else if is_static_block {
        let block = scope_node.as_class_static_block_declaration().body();
        lowerer.preregister_body_locals(block);
        let entry_target = lowerer.lower_stmt(block, lowerer.normal_exit);
        lowerer.add_edge(lowerer.entry, entry_target, ExplicitEdgeKind::Fallthrough);
    } else if is_source_file {
        lowerer.preregister_body_locals(sf.as_node());
        let entry_target = lowerer.lower_sequence(sf.statements().nodes(), lowerer.normal_exit);
        lowerer.add_edge(lowerer.entry, entry_target, ExplicitEdgeKind::Fallthrough);
    } else {
        // Inside a local statement/block scope (such as a loop body), unlabeled break/continue
        // exit the local scope cleanly without requiring outer loop back-edges.
        lowerer.preregister_body_locals(scope_node);
        lowerer.cf_frames.push(CfFrame::LocalScopeFallback {
            target: lowerer.normal_exit,
        });
        let entry_target = lowerer.lower_stmt(scope_node, lowerer.normal_exit);
        lowerer.cf_frames.pop();
        lowerer.add_edge(lowerer.entry, entry_target, ExplicitEdgeKind::Fallthrough);
    }

    Ok(lowerer.finish())
}

/// Summarizes a `ClassDeclaration` or `ClassExpression` into [`ClassIr`], preserving field modifiers,
/// `#private` visibility, parameter properties, and whether a default constructor or static init is needed.
///
/// # Errors
/// Returns a [`GapItem`] if `class_node` has no source file or is not a class node.
pub fn lower_class_to_ir(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    class_node: P<Node>,
    file_index: Option<&FileSyntaxIndex>,
) -> Result<ClassIr, GapItem> {
    let Some(sf) = get_source_file_of_node(class_node) else {
        return Err(GapItem::from_reason(
            UnknownReason::MissingSourceFile,
            "Class node has no associated source file.".to_string(),
            None,
        ));
    };
    let rel_file = relative_to_cwd(cwd, sf.file_name());
    let loc = node_location(sf, &rel_file, class_node);
    if !matches!(
        class_node.kind(),
        Kind::ClassDeclaration | Kind::ClassExpression
    ) {
        return Err(GapItem::from_reason(
            UnknownReason::InvalidCfgScope,
            "Class IR lowering requires a ClassDeclaration or ClassExpression.".to_string(),
            Some(loc),
        ));
    }

    let bridge = match file_index {
        Some(idx) => SemanticModelBridge::with_file_index(cwd, program, checker, idx),
        None => SemanticModelBridge::new(cwd, program, checker),
    };
    let class_syntax = bridge.intern_syntax(class_node).unwrap_or(SyntaxId(0));
    let name = class_node.name().and_then(node_text);
    let decl_semantics = extract_decl_semantics(class_node);
    let has_extends = class_has_extends(class_node);

    let mut instance_fields = Vec::new();
    let mut static_fields = Vec::new();
    let mut has_explicit_constructor = false;
    let mut has_static_init = false;

    for &member in class_node.members() {
        let member_kind = member.kind();
        if member_kind == Kind::Constructor {
            has_explicit_constructor = true;
            for &param in member.parameters() {
                if let Some(field) = extract_param_property_init(&bridge, param) {
                    instance_fields.push(field);
                }
            }
        } else if member_kind == Kind::PropertyDeclaration {
            let prop = member.as_property_declaration();
            let sem = extract_decl_semantics(member);
            let field_name = prop_key_text(prop.name()).unwrap_or_default();
            let syntax = bridge.intern_syntax(member).unwrap_or(SyntaxId(0));
            let field = ClassFieldInit {
                name: field_name,
                syntax,
                is_static: sem.is_static,
                is_readonly: sem.is_readonly,
                is_override: sem.is_override,
                visibility: sem.visibility,
                has_initializer: prop.initializer().is_some(),
            };
            if sem.is_static {
                if field.has_initializer {
                    has_static_init = true;
                }
                static_fields.push(field);
            } else {
                instance_fields.push(field);
            }
        } else if member_kind == Kind::ClassStaticBlockDeclaration {
            has_static_init = true;
        }
    }
    drop(bridge);

    Ok(ClassIr {
        syntax: class_syntax,
        name,
        decl_semantics,
        has_extends,
        instance_fields,
        static_fields,
        has_explicit_constructor,
        has_static_init,
    })
}

/// Lowers all `static` field initializers and `static { ... }` blocks of a class in source order
/// into a single `IrScopeKind::ClassStaticInit` [`LoweredScope`].
///
/// # Errors
/// Returns a [`GapItem`] if `class_node` has no source file, is not a class node, or violates invariants.
pub fn lower_class_static_init_to_ir(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    class_node: P<Node>,
    local_scope: u32,
    file_index: Option<&FileSyntaxIndex>,
) -> Result<LoweredScope, GapItem> {
    let Some(sf) = get_source_file_of_node(class_node) else {
        return Err(GapItem::from_reason(
            UnknownReason::MissingSourceFile,
            "Class node has no associated source file.".to_string(),
            None,
        ));
    };
    let rel_file = relative_to_cwd(cwd, sf.file_name());
    let scope_location = node_location(sf, &rel_file, class_node);
    if !matches!(
        class_node.kind(),
        Kind::ClassDeclaration | Kind::ClassExpression
    ) {
        return Err(GapItem::from_reason(
            UnknownReason::InvalidCfgScope,
            "Static initializer lowering requires a ClassDeclaration or ClassExpression."
                .to_string(),
            Some(scope_location),
        ));
    }

    let scope_ir = {
        let bridge = match file_index {
            Some(idx) => SemanticModelBridge::with_file_index(cwd, program, checker, idx),
            None => SemanticModelBridge::new(cwd, program, checker),
        };
        let mut lowerer = ScopeLowerer::new(
            sf,
            &rel_file,
            Some(&bridge),
            class_node,
            scope_location,
            local_scope,
            file_index,
        );
        lowerer.scope_kind = IrScopeKind::ClassStaticInit;

        let mut cont = lowerer.normal_exit;
        for &member in class_node.members().iter().rev() {
            let member_kind = member.kind();
            if member_kind == Kind::ClassStaticBlockDeclaration {
                let block = member.as_class_static_block_declaration().body();
                cont = lowerer.lower_stmt(block, cont);
                lowerer.record_syntax_vertex(member, cont);
            } else if member_kind == Kind::PropertyDeclaration {
                let sem = extract_decl_semantics(member);
                if sem.is_static {
                    cont = lowerer.lower_class_field_write(member, Some(class_node), cont);
                }
            }
        }

        lowerer.add_edge(lowerer.entry, cont, ExplicitEdgeKind::Fallthrough);
        lowerer.finish()
    };
    if let Err(msg) = scope_ir.ir.validate_invariants() {
        return Err(GapItem::from_reason(
            UnknownReason::UnmodeledConstruct,
            format!("IR CFG invariant violation: {msg}"),
            Some(scope_ir.ir.scope_location.clone()),
        ));
    }
    Ok(scope_ir)
}

/// Synthesizes a default constructor [`LoweredScope`] for a class without an explicit `constructor`,
/// emitting implicit `super(...args)` (when `extends` is present) followed by instance field initializers.
///
/// # Errors
/// Returns a [`GapItem`] if `class_node` has no source file, is not a class node, or violates invariants.
pub fn lower_class_default_constructor_to_ir(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    class_node: P<Node>,
    local_scope: u32,
    file_index: Option<&FileSyntaxIndex>,
) -> Result<LoweredScope, GapItem> {
    let Some(sf) = get_source_file_of_node(class_node) else {
        return Err(GapItem::from_reason(
            UnknownReason::MissingSourceFile,
            "Class node has no associated source file.".to_string(),
            None,
        ));
    };
    let rel_file = relative_to_cwd(cwd, sf.file_name());
    let scope_location = node_location(sf, &rel_file, class_node);
    if !matches!(
        class_node.kind(),
        Kind::ClassDeclaration | Kind::ClassExpression
    ) {
        return Err(GapItem::from_reason(
            UnknownReason::InvalidCfgScope,
            "Default constructor synthesis requires a ClassDeclaration or ClassExpression."
                .to_string(),
            Some(scope_location),
        ));
    }

    let scope_ir = {
        let bridge = match file_index {
            Some(idx) => SemanticModelBridge::with_file_index(cwd, program, checker, idx),
            None => SemanticModelBridge::new(cwd, program, checker),
        };
        let mut lowerer = ScopeLowerer::new(
            sf,
            &rel_file,
            Some(&bridge),
            class_node,
            scope_location,
            local_scope,
            file_index,
        );
        lowerer.scope_kind = IrScopeKind::Constructor;

        let mut cont = lowerer.lower_class_instance_init(class_node, None, lowerer.normal_exit);
        if class_has_extends(class_node) {
            let super_val = lowerer.alloc_value();
            let super_v =
                lowerer.new_vertex(Some(class_node), IrOp::SuperRef, Some(super_val), false);
            let call_val = lowerer.alloc_value();
            let call_v = lowerer.new_vertex(
                Some(class_node),
                IrOp::Call {
                    call_kind: IrCallKind::SuperCall,
                    callee: super_val,
                    receiver: None,
                    args: Vec::new(),
                },
                Some(call_val),
                false,
            );
            lowerer.attach_may_throw(call_v, class_node, MayThrowReason::Call);
            lowerer.add_edge(super_v, call_v, ExplicitEdgeKind::Fallthrough);
            lowerer.add_edge(call_v, cont, ExplicitEdgeKind::Fallthrough);
            cont = super_v;
        }

        lowerer.add_edge(lowerer.entry, cont, ExplicitEdgeKind::Fallthrough);
        lowerer.finish()
    };
    if let Err(msg) = scope_ir.ir.validate_invariants() {
        return Err(GapItem::from_reason(
            UnknownReason::UnmodeledConstruct,
            format!("IR CFG invariant violation: {msg}"),
            Some(scope_ir.ir.scope_location.clone()),
        ));
    }
    Ok(scope_ir)
}

#[derive(Clone, Copy)]
enum AssignTargetPrefix {
    Prop {
        base: P<Node>,
        assign_v: VertexId,
        setter_call_v: Option<VertexId>,
    },
    Element {
        base: P<Node>,
        arg: P<Node>,
        assign_v: VertexId,
    },
    Other(P<Node>),
}

enum CfFrame {
    Loop {
        labels: Vec<String>,
        break_target: VertexId,
        continue_target: VertexId,
    },
    Switch {
        break_target: VertexId,
    },
    Label {
        name: String,
        break_target: VertexId,
    },
    Catch {
        entry: VertexId,
    },
    Finally {
        id: FinallyId,
        entry: VertexId,
        normal_exit: VertexId,
        trampolines: FxHashMap<VertexId, VertexId>,
    },
    LocalScopeFallback {
        target: VertexId,
    },
}

struct ScopeLowerer<'a, 'b> {
    sf: P<SourceFile>,
    rel_file: &'a str,
    bridge: Option<&'a SemanticModelBridge<'b>>,
    file_index: Option<&'a FileSyntaxIndex>,
    next_fallback_local_id: u32,
    root_node: P<Node>,
    scope_kind: IrScopeKind,
    scope_location: DiagnosticLocation,
    decl_semantics: DeclarationSemantics,
    type_guard: Option<TypeGuardSemantics>,
    captured_bindings: Vec<CapturedBinding>,
    active_ctor_class: Option<P<Node>>,
    local_scope: u32,
    vertices: Vec<CfgVertex>,
    entry: VertexId,
    normal_exit: VertexId,
    exceptional_exit: VertexId,
    exit: VertexId,
    parameters: Vec<IrParameter>,
    bindings: Vec<IrLocalBinding>,
    binding_by_decl_node: FxHashMap<P<Node>, LocalBindingId>,
    binding_by_raw_symbol: FxHashMap<P<Symbol>, LocalBindingId>,
    binding_by_name: FxHashMap<String, LocalBindingId>,
    node_to_syntax: FxHashMap<P<Node>, SyntaxId>,
    syntax_to_node: FxHashMap<SyntaxId, P<Node>>,
    syntax_to_vertex: FxHashMap<SyntaxId, VertexId>,
    expr_to_vertex: FxHashMap<LocalSyntaxId, VertexId>,
    expr_to_value: FxHashMap<LocalSyntaxId, ValueId>,
    value_sources: FxHashMap<ValueId, Vec<ValueId>>,
    branches: Vec<IrBranchRecord>,
    cf_frames: Vec<CfFrame>,
    active_finally_bodies: Vec<FinallyId>,
    next_finally_id: u32,
    next_value_id: u32,
}

impl<'a, 'b> ScopeLowerer<'a, 'b> {
    fn new(
        sf: P<SourceFile>,
        rel_file: &'a str,
        bridge: Option<&'a SemanticModelBridge<'b>>,
        root_node: P<Node>,
        scope_location: DiagnosticLocation,
        local_scope: u32,
        file_index: Option<&'a FileSyntaxIndex>,
    ) -> Self {
        let mut vertices = Vec::new();
        let entry = VertexId(0);
        let normal_exit = VertexId(1);
        let exceptional_exit = VertexId(2);
        let exit = VertexId(3);

        vertices.push(empty_vertex(entry, IrOp::Entry));
        vertices.push(empty_vertex(normal_exit, IrOp::NopMerge));
        vertices.push(empty_vertex(exceptional_exit, IrOp::NopMerge));
        vertices.push(empty_vertex(exit, IrOp::Exit));

        vertices[1].explicit_edges.push(ExplicitEdge {
            target: exit,
            kind: ExplicitEdgeKind::ExitMerge,
        });
        vertices[2].explicit_edges.push(ExplicitEdge {
            target: exit,
            kind: ExplicitEdgeKind::ExitMerge,
        });

        let scope_kind = classify_scope_kind(root_node);
        let decl_semantics = extract_decl_semantics(root_node);
        let next_fallback_local_id = file_index
            .and_then(|idx| u32::try_from(idx.local_to_node.len()).ok())
            .unwrap_or(0);

        Self {
            sf,
            rel_file,
            bridge,
            file_index,
            next_fallback_local_id,
            root_node,
            scope_kind,
            scope_location,
            decl_semantics,
            type_guard: None,
            captured_bindings: Vec::new(),
            active_ctor_class: None,
            local_scope,
            vertices,
            entry,
            normal_exit,
            exceptional_exit,
            exit,
            parameters: Vec::new(),
            bindings: Vec::new(),
            binding_by_decl_node: FxHashMap::default(),
            binding_by_raw_symbol: FxHashMap::default(),
            binding_by_name: FxHashMap::default(),
            node_to_syntax: FxHashMap::default(),
            syntax_to_node: FxHashMap::default(),
            syntax_to_vertex: FxHashMap::default(),
            expr_to_vertex: FxHashMap::default(),
            expr_to_value: FxHashMap::default(),
            value_sources: FxHashMap::default(),
            branches: Vec::new(),
            cf_frames: Vec::new(),
            active_finally_bodies: Vec::new(),
            next_finally_id: 0,
            next_value_id: 0,
        }
    }

    fn finish(mut self) -> LoweredScope {
        let mut written_bindings = FxHashSet::default();
        for vertex in &self.vertices {
            if let IrOp::AssignLocal {
                target,
                is_init: false,
                ..
            } = vertex.op
            {
                written_bindings.insert(target);
            }
        }
        for vertex in &mut self.vertices {
            if let IrOp::FunctionLiteral { captures, .. } = &mut vertex.op {
                for cap in captures {
                    if let Some(outer) = cap.outer_binding
                        && written_bindings.contains(&outer)
                    {
                        cap.is_written_in_outer_after_capture = true;
                    }
                }
            }
        }

        let root_syntax = self.intern_node(self.root_node);
        let file = root_syntax.file_id();
        let ir = FunctionIr {
            id: FunctionIrId {
                file,
                local_scope: self.local_scope,
            },
            root_syntax,
            scope_kind: self.scope_kind,
            scope_location: self.scope_location,
            decl_semantics: self.decl_semantics,
            type_guard: self.type_guard,
            parameters: self.parameters,
            bindings: self.bindings,
            captured_bindings: self.captured_bindings,
            vertices: self.vertices,
            entry: self.entry,
            normal_exit: self.normal_exit,
            exceptional_exit: self.exceptional_exit,
            exit: self.exit,
            syntax_to_vertex: self.syntax_to_vertex,
            expr_to_vertex: self.expr_to_vertex,
            expr_to_value: self.expr_to_value,
            value_sources: self.value_sources,
            branches: self.branches,
            value_count: self.next_value_id,
        };
        LoweredScope {
            ir: Arc::new(ir),
            node_to_syntax: self.node_to_syntax,
            syntax_to_node: self.syntax_to_node,
        }
    }

    fn record_value_sources(&mut self, merged: ValueId, sources: &[ValueId]) {
        let entry = self.value_sources.entry(merged).or_default();
        for &src in sources {
            if src != merged && !entry.contains(&src) {
                entry.push(src);
            }
        }
    }

    fn intern_node(&mut self, node: P<Node>) -> SyntaxId {
        if let Some(&sid) = self.node_to_syntax.get(&node) {
            return sid;
        }
        let sid = self
            .file_index
            .and_then(|idx| idx.syntax_id_for_node(node))
            .or_else(|| self.bridge.and_then(|b| b.intern_syntax(node)))
            .unwrap_or_else(|| {
                let file_id = self.file_index.map_or(FileId(0), |idx| idx.file_id);
                let local = LocalSyntaxId(self.next_fallback_local_id);
                self.next_fallback_local_id = self.next_fallback_local_id.saturating_add(1);
                SyntaxId::from_parts(file_id, local)
            });
        self.node_to_syntax.insert(node, sid);
        self.syntax_to_node.insert(sid, node);
        sid
    }

    const fn alloc_value(&mut self) -> ValueId {
        let id = ValueId(self.next_value_id);
        self.next_value_id = self.next_value_id.saturating_add(1);
        id
    }

    fn loc(&self, node: P<Node>) -> DiagnosticLocation {
        node_location(self.sf, self.rel_file, node)
    }

    fn new_vertex(
        &mut self,
        node: Option<P<Node>>,
        op: IrOp,
        value: Option<ValueId>,
        _with_semantics: bool,
    ) -> VertexId {
        let id = VertexId(u32::try_from(self.vertices.len()).unwrap_or(u32::MAX));
        let (syntax, local_syntax, location) = node.map_or((None, None, None), |n| {
            let sid = self.intern_node(n);
            let lsid = sid.local_id();
            let loc = Some(self.loc(n));
            (Some(sid), Some(lsid), loc)
        });

        // Cached FunctionIr deliberately omits transient TypeId/SignatureId/SymbolId values;
        // analysis passes resolve fresh snapshot handles via SyntaxId when needed.
        self.vertices.push(CfgVertex {
            id,
            syntax,
            local_syntax,
            symbol: None,
            ty: None,
            signature: None,
            location,
            value,
            op,
            explicit_edges: Vec::new(),
            may_throw_edge: None,
            enclosing_finallies: self.active_finally_bodies.clone(),
        });

        if let Some(sid) = syntax {
            self.syntax_to_vertex.insert(sid, id);
        }
        id
    }

    fn bind_expr_maps(&mut self, node: P<Node>, vid: VertexId, val: ValueId) {
        let sid = self.intern_node(node);
        let lsid = sid.local_id();
        self.syntax_to_vertex.insert(sid, vid);
        self.expr_to_vertex.insert(lsid, vid);
        self.expr_to_value.insert(lsid, val);
    }

    fn record_syntax_vertex(&mut self, node: P<Node>, vid: VertexId) {
        let sid = self.intern_node(node);
        self.syntax_to_vertex.entry(sid).or_insert(vid);
    }

    fn add_edge(&mut self, from: VertexId, to: VertexId, kind: ExplicitEdgeKind) {
        if let Some(v) = self.vertices.get_mut(from.0 as usize) {
            v.explicit_edges.push(ExplicitEdge { target: to, kind });
        }
    }

    fn push_branch(
        &mut self,
        kind: IrBranchKind,
        direction: IrBranchDirection,
        condition_node: P<Node>,
        location: DiagnosticLocation,
        statement_location: DiagnosticLocation,
        condition_text_override: Option<String>,
    ) -> BranchId {
        let id = BranchId(u32::try_from(self.branches.len()).unwrap_or(u32::MAX));
        let condition_syntax = self.intern_node(condition_node);
        let condition_text = condition_text_override
            .unwrap_or_else(|| node_text(condition_node).unwrap_or_default());
        self.branches.push(IrBranchRecord {
            id,
            kind,
            direction,
            condition_syntax,
            condition_text,
            location,
            statement_location,
        });
        id
    }

    fn collect_parameters(&mut self, fn_node: P<Node>) {
        self.intern_node(fn_node);
        for &param in fn_node.parameters() {
            let sid = self.intern_node(param);
            let param_data = param.as_parameter_declaration();
            let name = param.name().and_then(node_text).unwrap_or_default();
            let binding = param.name().and_then(|nn| {
                if nn.kind() == Kind::Identifier {
                    Some(self.register_binding(&name, BindingMutability::Parameter, param))
                } else {
                    self.preregister_pattern_bindings(nn, BindingMutability::Parameter);
                    None
                }
            });
            self.parameters.push(IrParameter {
                name,
                binding,
                syntax: sid,
                has_default: param_data.initializer().is_some(),
                is_rest: param_data.dot_dot_dot_token().is_some(),
            });
        }
    }

    fn preregister_body_locals(&mut self, node: P<Node>) {
        if node != self.root_node {
            let kind = node.kind();
            if kind == Kind::VariableDeclaration {
                let mutability = classify_decl_mutability(node);
                if let Some(nn) = node.name() {
                    if nn.kind() == Kind::Identifier {
                        let name = node_text(nn).unwrap_or_default();
                        self.register_binding(&name, mutability, node);
                    } else {
                        self.preregister_pattern_bindings(nn, mutability);
                    }
                }
            } else if kind == Kind::CatchClause {
                let cc = node.as_catch_clause();
                if let Some(var_decl) = cc.variable_declaration()
                    && let Some(nn) = var_decl.name()
                {
                    if nn.kind() == Kind::Identifier {
                        let name = node_text(nn).unwrap_or_default();
                        self.register_binding(&name, BindingMutability::CatchBinding, var_decl);
                    } else {
                        self.preregister_pattern_bindings(nn, BindingMutability::CatchBinding);
                    }
                }
            } else if matches!(kind, Kind::FunctionDeclaration | Kind::ClassDeclaration) {
                if let Some(nn) = node.name()
                    && nn.kind() == Kind::Identifier
                {
                    let name = node_text(nn).unwrap_or_default();
                    let mutability = if kind == Kind::ClassDeclaration {
                        BindingMutability::Const
                    } else {
                        BindingMutability::Var
                    };
                    self.register_binding(&name, mutability, node);
                }
                return;
            } else if tsrs_ast::is_function_like_declaration(Some(node))
                || kind == Kind::ClassExpression
            {
                return;
            }
        }
        node.for_each_child(&mut |child| {
            self.preregister_body_locals(child);
            false
        });
    }

    fn preregister_pattern_bindings(&mut self, pattern: P<Node>, mutability: BindingMutability) {
        let kind = pattern.kind();
        if kind == Kind::Identifier {
            let name = node_text(pattern).unwrap_or_default();
            self.register_binding(&name, mutability, pattern);
        } else if matches!(kind, Kind::ObjectBindingPattern | Kind::ArrayBindingPattern) {
            for &elem in pattern.elements() {
                self.preregister_pattern_bindings(elem, mutability);
            }
        } else if kind == Kind::BindingElement
            && let Some(name) = pattern.as_binding_element().name()
        {
            self.preregister_pattern_bindings(name, mutability);
        }
    }

    fn register_binding(
        &mut self,
        name: &str,
        mutability: BindingMutability,
        decl_node: P<Node>,
    ) -> LocalBindingId {
        if let Some(&existing) = self.binding_by_decl_node.get(&decl_node) {
            return existing;
        }
        let sid = self.intern_node(decl_node);
        let raw_symbol = self
            .bridge
            .and_then(|b| b.symbol_for_node_raw(decl_node))
            .or_else(|| decl_node.symbol());
        if let Some(sym) = raw_symbol
            && let Some(&existing) = self.binding_by_raw_symbol.get(&sym)
        {
            self.binding_by_decl_node.insert(decl_node, existing);
            return existing;
        }
        let id = LocalBindingId(u32::try_from(self.bindings.len()).unwrap_or(u32::MAX));
        self.bindings.push(IrLocalBinding {
            id,
            name: name.to_string(),
            mutability,
            syntax: sid,
            symbol: None,
        });
        self.binding_by_decl_node.insert(decl_node, id);
        if let Some(sym) = raw_symbol {
            self.binding_by_raw_symbol.insert(sym, id);
        }
        if !name.is_empty() {
            self.binding_by_name.insert(name.to_string(), id);
        }
        id
    }

    fn resolve_binding(&self, ident_node: P<Node>) -> Option<LocalBindingId> {
        if let Some(sym) = self.bridge.and_then(|b| b.symbol_for_node_raw(ident_node)) {
            return self.binding_by_raw_symbol.get(&sym).copied();
        }
        node_text(ident_node).and_then(|name| self.binding_by_name.get(&name).copied())
    }

    fn attach_may_throw(&mut self, vid: VertexId, node: P<Node>, reason: MayThrowReason) {
        let target = self.current_exceptional_target();
        let site_syntax = self.intern_node(node);
        if let Some(v) = self.vertices.get_mut(vid.0 as usize) {
            v.may_throw_edge = Some(MayThrowEdge {
                target,
                site_syntax,
                reason,
            });
        }
    }

    fn wrap_with_finallies_above(&mut self, start_idx: usize, mut target: VertexId) -> VertexId {
        for idx in start_idx..self.cf_frames.len() {
            let (id, entry, normal_exit, cached) = match &self.cf_frames[idx] {
                CfFrame::Finally {
                    id,
                    entry,
                    normal_exit,
                    trampolines,
                } => (*id, *entry, *normal_exit, trampolines.get(&target).copied()),
                CfFrame::Loop { .. }
                | CfFrame::Switch { .. }
                | CfFrame::Label { .. }
                | CfFrame::Catch { .. }
                | CfFrame::LocalScopeFallback { .. } => continue,
            };
            if let Some(tramp_v) = cached {
                target = tramp_v;
                continue;
            }
            let tramp_v = self.new_vertex(None, IrOp::NopMerge, None, false);
            self.add_edge(
                tramp_v,
                entry,
                ExplicitEdgeKind::FinallyEnter {
                    finally_id: id,
                    resume: target,
                },
            );
            self.add_edge(
                normal_exit,
                target,
                ExplicitEdgeKind::FinallyResume {
                    finally_id: id,
                    resume: target,
                },
            );
            if let CfFrame::Finally { trampolines, .. } = &mut self.cf_frames[idx] {
                trampolines.insert(target, tramp_v);
            }
            target = tramp_v;
        }
        target
    }

    fn current_exceptional_target(&mut self) -> VertexId {
        for idx in (0..self.cf_frames.len()).rev() {
            if let CfFrame::Catch { entry } = self.cf_frames[idx] {
                return self.wrap_with_finallies_above(idx + 1, entry);
            }
        }
        self.wrap_with_finallies_above(0, self.exceptional_exit)
    }

    fn resolve_return_target(&mut self) -> VertexId {
        self.wrap_with_finallies_above(0, self.normal_exit)
    }

    fn resolve_break_target(&mut self, label: Option<&str>) -> Option<VertexId> {
        for idx in (0..self.cf_frames.len()).rev() {
            let matched = match &self.cf_frames[idx] {
                CfFrame::Label { name, break_target } if label == Some(name.as_str()) => {
                    Some(*break_target)
                }
                CfFrame::Loop {
                    labels,
                    break_target,
                    ..
                } if label.is_none_or(|lbl| labels.iter().any(|l| l == lbl)) => Some(*break_target),
                CfFrame::Switch { break_target } if label.is_none() => Some(*break_target),
                CfFrame::LocalScopeFallback { target } => Some(*target),
                CfFrame::Label { .. }
                | CfFrame::Loop { .. }
                | CfFrame::Switch { .. }
                | CfFrame::Catch { .. }
                | CfFrame::Finally { .. } => None,
            };
            if let Some(target) = matched {
                return Some(self.wrap_with_finallies_above(idx + 1, target));
            }
        }
        None
    }

    fn resolve_continue_target(&mut self, label: Option<&str>) -> Option<VertexId> {
        for idx in (0..self.cf_frames.len()).rev() {
            let matched = match &self.cf_frames[idx] {
                CfFrame::Loop {
                    labels,
                    continue_target,
                    ..
                } if label.is_none_or(|lbl| labels.iter().any(|l| l == lbl)) => {
                    Some(*continue_target)
                }
                CfFrame::LocalScopeFallback { target } => Some(*target),
                CfFrame::Loop { .. }
                | CfFrame::Switch { .. }
                | CfFrame::Label { .. }
                | CfFrame::Catch { .. }
                | CfFrame::Finally { .. } => None,
            };
            if let Some(target) = matched {
                return Some(self.wrap_with_finallies_above(idx + 1, target));
            }
        }
        None
    }

    fn lower_sequence(&mut self, statements: &[P<Node>], mut next: VertexId) -> VertexId {
        if let Some(using_idx) = statements.iter().position(|&stmt| {
            stmt.kind() == Kind::VariableStatement
                && tsrs_ast::get_combined_node_flags(
                    stmt.as_variable_statement().declaration_list(),
                )
                .intersects(NodeFlags::Using)
        }) {
            let using_stmt = statements[using_idx];
            let decl_list = using_stmt.as_variable_statement().declaration_list();
            let flags = tsrs_ast::get_combined_node_flags(decl_list);
            let is_async = flags.contains(NodeFlags::AwaitUsing);
            let decls = decl_list
                .as_variable_declaration_list()
                .declarations()
                .nodes();
            let using_entry =
                self.lower_using_decls(decls, is_async, &statements[using_idx + 1..], next);
            self.record_syntax_vertex(using_stmt, using_entry);
            next = using_entry;
            for &stmt in statements[..using_idx].iter().rev() {
                next = self.lower_stmt(stmt, next);
            }
            return next;
        }

        for &stmt in statements.iter().rev() {
            next = self.lower_stmt(stmt, next);
        }
        next
    }

    fn lower_using_decls(
        &mut self,
        decls: &[P<Node>],
        is_async: bool,
        rest_stmts: &[P<Node>],
        next: VertexId,
    ) -> VertexId {
        let Some((&decl, tail_decls)) = decls.split_first() else {
            return self.lower_sequence(rest_stmts, next);
        };
        let mutability = if is_async {
            BindingMutability::AwaitUsing
        } else {
            BindingMutability::Using
        };
        let name_node = decl.name();
        let binding = if let Some(nn) = name_node
            && nn.kind() == Kind::Identifier
        {
            let name = node_text(nn).unwrap_or_default();
            Some(self.register_binding(&name, mutability, decl))
        } else {
            None
        };

        // `using` / `await using` wraps all subsequent declarations and statements in an implicit
        // `try ... finally` that emits an `ImplicitDispose` call on every exit path.
        let fid = FinallyId(self.next_finally_id);
        self.next_finally_id = self.next_finally_id.saturating_add(1);
        self.active_finally_bodies.push(fid);
        let finally_entry = self.new_vertex(None, IrOp::NopMerge, None, false);
        let finally_normal_exit = self.new_vertex(None, IrOp::NopMerge, None, false);

        let target_val = self.alloc_value();
        let read_op = binding.map_or_else(
            || IrOp::ReadSymbol {
                ident_syntax: self.intern_node(decl),
            },
            |b| IrOp::ReadLocal { binding: b },
        );
        let read_v = self.new_vertex(None, read_op, Some(target_val), false);
        let dispose_val = self.alloc_value();
        let dispose_v = self.new_vertex(
            Some(decl),
            IrOp::Call {
                call_kind: IrCallKind::ImplicitDispose { is_async },
                callee: target_val,
                receiver: Some(target_val),
                args: Vec::new(),
            },
            Some(dispose_val),
            true,
        );
        self.attach_may_throw(dispose_v, decl, MayThrowReason::Call);
        self.add_edge(finally_entry, read_v, ExplicitEdgeKind::Fallthrough);
        self.add_edge(read_v, dispose_v, ExplicitEdgeKind::Fallthrough);
        self.add_edge(
            dispose_v,
            finally_normal_exit,
            ExplicitEdgeKind::Fallthrough,
        );
        self.active_finally_bodies.pop();

        let frame_idx = self.cf_frames.len();
        self.cf_frames.push(CfFrame::Finally {
            id: fid,
            entry: finally_entry,
            normal_exit: finally_normal_exit,
            trampolines: FxHashMap::default(),
        });
        let try_cont = self.wrap_with_finallies_above(frame_idx, next);
        let body_entry = self.lower_using_decls(tail_decls, is_async, rest_stmts, try_cont);
        self.cf_frames.pop();

        self.lower_single_variable_decl(decl, mutability, None, body_entry)
    }

    fn lower_stmt(&mut self, node: P<Node>, next: VertexId) -> VertexId {
        self.lower_stmt_with_labels(node, next, Vec::new())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Exhaustive 353-variant tsrs_ast::Kind statement dispatcher."
    )]
    fn lower_stmt_with_labels(
        &mut self,
        node: P<Node>,
        next: VertexId,
        mut pending_labels: Vec<String>,
    ) -> VertexId {
        match node.kind() {
            Kind::Block | Kind::ModuleBlock => {
                let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                let first = self.lower_sequence(node.statements(), next);
                self.add_edge(here, first, ExplicitEdgeKind::Fallthrough);
                here
            }
            Kind::IfStatement => self.lower_if(node, next),
            Kind::SwitchStatement => self.lower_switch(node, next),
            Kind::ReturnStatement => {
                let expr_opt = node.as_return_statement().expression();
                let return_target = self.resolve_return_target();
                let ret_vid =
                    self.new_vertex(Some(node), IrOp::Return { value: None }, None, false);
                self.add_edge(ret_vid, return_target, ExplicitEdgeKind::Return);
                if let Some(expr) = expr_opt {
                    let (expr_entry, expr_val) = self.lower_expr_to(expr, ret_vid);
                    if let Some(v) = self.vertices.get_mut(ret_vid.0 as usize) {
                        v.op = IrOp::Return {
                            value: Some(expr_val),
                        };
                    }
                    expr_entry
                } else {
                    ret_vid
                }
            }
            Kind::ThrowStatement => {
                let expr = node.as_throw_statement().expression();
                let throw_target = self.current_exceptional_target();
                let dummy = ValueId(0);
                let throw_vid =
                    self.new_vertex(Some(node), IrOp::Throw { exception: dummy }, None, false);
                self.add_edge(throw_vid, throw_target, ExplicitEdgeKind::ExplicitThrow);
                let (expr_entry, expr_val) = self.lower_expr_to(expr, throw_vid);
                if let Some(v) = self.vertices.get_mut(throw_vid.0 as usize) {
                    v.op = IrOp::Throw {
                        exception: expr_val,
                    };
                }
                expr_entry
            }
            Kind::BreakStatement => {
                let label = node.as_break_statement().label().and_then(node_text);
                let target = self
                    .resolve_break_target(label.as_deref())
                    .unwrap_or(self.normal_exit);
                let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                self.add_edge(here, target, ExplicitEdgeKind::Break { label });
                here
            }
            Kind::ContinueStatement => {
                let label = node.as_continue_statement().label().and_then(node_text);
                let target = self
                    .resolve_continue_target(label.as_deref())
                    .unwrap_or(self.normal_exit);
                let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                self.add_edge(here, target, ExplicitEdgeKind::Continue { label });
                here
            }
            Kind::ExpressionStatement => {
                let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                let expr = node.as_expression_statement().expression();
                let (entry, _) = self.lower_expr_to(expr, here);
                entry
            }
            Kind::VariableStatement => self.lower_variable_stmt(node, next),
            Kind::VariableDeclarationList => {
                let decls = node.as_variable_declaration_list().declarations().nodes();
                self.lower_variable_decls(decls, next)
            }
            Kind::VariableDeclaration => self.lower_variable_decls(&[node], next),
            Kind::TryStatement => self.lower_try(node, next),
            Kind::WhileStatement => self.lower_while(node, next, pending_labels),
            Kind::DoStatement => self.lower_do_while(node, next, pending_labels),
            Kind::ForStatement => self.lower_for(node, next, pending_labels),
            Kind::ForInStatement | Kind::ForOfStatement => {
                self.lower_for_in_or_of(node, next, pending_labels)
            }
            Kind::LabeledStatement => {
                let lbl = node.as_labeled_statement();
                let label_name = node_text(lbl.label()).unwrap_or_default();
                let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                self.cf_frames.push(CfFrame::Label {
                    name: label_name.clone(),
                    break_target: next,
                });
                pending_labels.push(label_name);
                let inner_entry =
                    self.lower_stmt_with_labels(lbl.statement(), next, pending_labels);
                self.cf_frames.pop();
                self.add_edge(here, inner_entry, ExplicitEdgeKind::Fallthrough);
                here
            }
            Kind::WithStatement => {
                // `with (obj) { ... }` dynamically injects runtime properties into the lexical scope
                // chain, making variable binding and control-flow resolution statically undecidable.
                let unknown_v = self.new_vertex(
                    Some(node),
                    IrOp::Unknown {
                        reason: UnknownReason::WithStatement,
                        message: "`with` statement scope is not statically modeled.".to_string(),
                        location: self.loc(node),
                        evaluated_children: Vec::new(),
                    },
                    None,
                    false,
                );
                let with_stmt = node.as_with_statement();
                let body_entry = self.lower_stmt(with_stmt.statement(), next);
                let (expr_entry, expr_val) = self.lower_expr_to(with_stmt.expression(), body_entry);
                if let Some(v) = self.vertices.get_mut(unknown_v.0 as usize)
                    && let IrOp::Unknown {
                        evaluated_children, ..
                    } = &mut v.op
                {
                    evaluated_children.push(expr_val);
                }
                self.add_edge(unknown_v, expr_entry, ExplicitEdgeKind::Fallthrough);
                unknown_v
            }
            Kind::ExportAssignment => {
                let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                let expr = node.as_export_assignment().expression();
                let (entry, _) = self.lower_expr_to(expr, here);
                entry
            }
            Kind::FunctionDeclaration => {
                let sid = self.intern_node(node);
                let captures = self.collect_nested_captures(node);
                let here = self.new_vertex(
                    Some(node),
                    IrOp::FunctionLiteral {
                        syntax: sid,
                        captures,
                    },
                    None,
                    true,
                );
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                here
            }
            Kind::ClassDeclaration => self.lower_class_stmt(node, next),
            Kind::ClassStaticBlockDeclaration => {
                let block = node.as_class_static_block_declaration().body();
                self.lower_stmt(block, next)
            }
            Kind::Decorator => {
                let (entry, _) = self.lower_decorator_expr(node, next);
                entry
            }
            Kind::EmptyStatement
            | Kind::DebuggerStatement
            | Kind::InterfaceDeclaration
            | Kind::TypeAliasDeclaration
            | Kind::EnumDeclaration
            | Kind::ModuleDeclaration
            | Kind::NamespaceExportDeclaration
            | Kind::ImportEqualsDeclaration
            | Kind::ImportDeclaration
            | Kind::ExportDeclaration
            | Kind::MissingDeclaration
            | Kind::JSTypeAliasDeclaration
            | Kind::JSImportDeclaration
            | Kind::NotEmittedStatement => {
                let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                here
            }
            // Expressions appearing directly at statement position
            Kind::NumericLiteral
            | Kind::BigIntLiteral
            | Kind::StringLiteral
            | Kind::RegularExpressionLiteral
            | Kind::NoSubstitutionTemplateLiteral
            | Kind::Identifier
            | Kind::PrivateIdentifier
            | Kind::TrueKeyword
            | Kind::FalseKeyword
            | Kind::NullKeyword
            | Kind::ThisKeyword
            | Kind::SuperKeyword
            | Kind::ArrayLiteralExpression
            | Kind::ObjectLiteralExpression
            | Kind::PropertyAccessExpression
            | Kind::ElementAccessExpression
            | Kind::CallExpression
            | Kind::NewExpression
            | Kind::TaggedTemplateExpression
            | Kind::TypeAssertionExpression
            | Kind::ParenthesizedExpression
            | Kind::FunctionExpression
            | Kind::ArrowFunction
            | Kind::DeleteExpression
            | Kind::TypeOfExpression
            | Kind::VoidExpression
            | Kind::AwaitExpression
            | Kind::PrefixUnaryExpression
            | Kind::PostfixUnaryExpression
            | Kind::BinaryExpression
            | Kind::ConditionalExpression
            | Kind::TemplateExpression
            | Kind::YieldExpression
            | Kind::SpreadElement
            | Kind::ClassExpression
            | Kind::OmittedExpression
            | Kind::ExpressionWithTypeArguments
            | Kind::AsExpression
            | Kind::NonNullExpression
            | Kind::MetaProperty
            | Kind::SyntheticExpression
            | Kind::SatisfiesExpression
            | Kind::PartiallyEmittedExpression
            | Kind::SyntheticReferenceExpression
            | Kind::JsxElement
            | Kind::JsxSelfClosingElement
            | Kind::JsxFragment => {
                let (entry, _) = self.lower_expr_to(node, next);
                entry
            }
            // All remaining non-statement AST kinds (tokens, trivia, keywords, types, JSDoc, clauses, fragments)
            Kind::Unknown
            | Kind::EndOfFile
            | Kind::SingleLineCommentTrivia
            | Kind::MultiLineCommentTrivia
            | Kind::NewLineTrivia
            | Kind::WhitespaceTrivia
            | Kind::ConflictMarkerTrivia
            | Kind::NonTextFileMarkerTrivia
            | Kind::JsxText
            | Kind::JsxTextAllWhiteSpaces
            | Kind::TemplateHead
            | Kind::TemplateMiddle
            | Kind::TemplateTail
            | Kind::OpenBraceToken
            | Kind::CloseBraceToken
            | Kind::OpenParenToken
            | Kind::CloseParenToken
            | Kind::OpenBracketToken
            | Kind::CloseBracketToken
            | Kind::DotToken
            | Kind::DotDotDotToken
            | Kind::SemicolonToken
            | Kind::CommaToken
            | Kind::QuestionDotToken
            | Kind::LessThanToken
            | Kind::LessThanSlashToken
            | Kind::GreaterThanToken
            | Kind::LessThanEqualsToken
            | Kind::GreaterThanEqualsToken
            | Kind::EqualsEqualsToken
            | Kind::ExclamationEqualsToken
            | Kind::EqualsEqualsEqualsToken
            | Kind::ExclamationEqualsEqualsToken
            | Kind::EqualsGreaterThanToken
            | Kind::PlusToken
            | Kind::MinusToken
            | Kind::AsteriskToken
            | Kind::AsteriskAsteriskToken
            | Kind::SlashToken
            | Kind::PercentToken
            | Kind::PlusPlusToken
            | Kind::MinusMinusToken
            | Kind::LessThanLessThanToken
            | Kind::GreaterThanGreaterThanToken
            | Kind::GreaterThanGreaterThanGreaterThanToken
            | Kind::AmpersandToken
            | Kind::BarToken
            | Kind::CaretToken
            | Kind::ExclamationToken
            | Kind::TildeToken
            | Kind::AmpersandAmpersandToken
            | Kind::BarBarToken
            | Kind::QuestionToken
            | Kind::ColonToken
            | Kind::AtToken
            | Kind::QuestionQuestionToken
            | Kind::BacktickToken
            | Kind::HashToken
            | Kind::EqualsToken
            | Kind::PlusEqualsToken
            | Kind::MinusEqualsToken
            | Kind::AsteriskEqualsToken
            | Kind::AsteriskAsteriskEqualsToken
            | Kind::SlashEqualsToken
            | Kind::PercentEqualsToken
            | Kind::LessThanLessThanEqualsToken
            | Kind::GreaterThanGreaterThanEqualsToken
            | Kind::GreaterThanGreaterThanGreaterThanEqualsToken
            | Kind::AmpersandEqualsToken
            | Kind::BarEqualsToken
            | Kind::BarBarEqualsToken
            | Kind::AmpersandAmpersandEqualsToken
            | Kind::QuestionQuestionEqualsToken
            | Kind::CaretEqualsToken
            | Kind::JSDocCommentTextToken
            | Kind::BreakKeyword
            | Kind::CaseKeyword
            | Kind::CatchKeyword
            | Kind::ClassKeyword
            | Kind::ConstKeyword
            | Kind::ContinueKeyword
            | Kind::DebuggerKeyword
            | Kind::DefaultKeyword
            | Kind::DeleteKeyword
            | Kind::DoKeyword
            | Kind::ElseKeyword
            | Kind::EnumKeyword
            | Kind::ExportKeyword
            | Kind::ExtendsKeyword
            | Kind::FinallyKeyword
            | Kind::ForKeyword
            | Kind::FunctionKeyword
            | Kind::IfKeyword
            | Kind::ImportKeyword
            | Kind::InKeyword
            | Kind::InstanceOfKeyword
            | Kind::NewKeyword
            | Kind::ReturnKeyword
            | Kind::SwitchKeyword
            | Kind::ThrowKeyword
            | Kind::TryKeyword
            | Kind::TypeOfKeyword
            | Kind::VarKeyword
            | Kind::VoidKeyword
            | Kind::WhileKeyword
            | Kind::WithKeyword
            | Kind::ImplementsKeyword
            | Kind::InterfaceKeyword
            | Kind::LetKeyword
            | Kind::PackageKeyword
            | Kind::PrivateKeyword
            | Kind::ProtectedKeyword
            | Kind::PublicKeyword
            | Kind::StaticKeyword
            | Kind::YieldKeyword
            | Kind::AbstractKeyword
            | Kind::AccessorKeyword
            | Kind::AsKeyword
            | Kind::AssertsKeyword
            | Kind::AssertKeyword
            | Kind::AnyKeyword
            | Kind::AsyncKeyword
            | Kind::AwaitKeyword
            | Kind::BooleanKeyword
            | Kind::ConstructorKeyword
            | Kind::DeclareKeyword
            | Kind::GetKeyword
            | Kind::ImmediateKeyword
            | Kind::InferKeyword
            | Kind::IntrinsicKeyword
            | Kind::IsKeyword
            | Kind::KeyOfKeyword
            | Kind::ModuleKeyword
            | Kind::NamespaceKeyword
            | Kind::NeverKeyword
            | Kind::OutKeyword
            | Kind::ReadonlyKeyword
            | Kind::RequireKeyword
            | Kind::NumberKeyword
            | Kind::ObjectKeyword
            | Kind::SatisfiesKeyword
            | Kind::SetKeyword
            | Kind::StringKeyword
            | Kind::SymbolKeyword
            | Kind::TypeKeyword
            | Kind::UndefinedKeyword
            | Kind::UniqueKeyword
            | Kind::UnknownKeyword
            | Kind::UsingKeyword
            | Kind::FromKeyword
            | Kind::GlobalKeyword
            | Kind::BigIntKeyword
            | Kind::OverrideKeyword
            | Kind::OfKeyword
            | Kind::DeferKeyword
            | Kind::SourceKeyword
            | Kind::QualifiedName
            | Kind::ComputedPropertyName
            | Kind::TypeParameter
            | Kind::Parameter
            | Kind::PropertySignature
            | Kind::PropertyDeclaration
            | Kind::MethodSignature
            | Kind::MethodDeclaration
            | Kind::Constructor
            | Kind::GetAccessor
            | Kind::SetAccessor
            | Kind::CallSignature
            | Kind::ConstructSignature
            | Kind::IndexSignature
            | Kind::TypePredicate
            | Kind::TypeReference
            | Kind::FunctionType
            | Kind::ConstructorType
            | Kind::TypeQuery
            | Kind::TypeLiteral
            | Kind::ArrayType
            | Kind::TupleType
            | Kind::OptionalType
            | Kind::RestType
            | Kind::UnionType
            | Kind::IntersectionType
            | Kind::ConditionalType
            | Kind::InferType
            | Kind::ParenthesizedType
            | Kind::ThisType
            | Kind::TypeOperator
            | Kind::IndexedAccessType
            | Kind::MappedType
            | Kind::LiteralType
            | Kind::NamedTupleMember
            | Kind::TemplateLiteralType
            | Kind::TemplateLiteralTypeSpan
            | Kind::ImportType
            | Kind::ObjectBindingPattern
            | Kind::ArrayBindingPattern
            | Kind::BindingElement
            | Kind::TemplateSpan
            | Kind::SemicolonClassElement
            | Kind::CaseBlock
            | Kind::ImportClause
            | Kind::NamespaceImport
            | Kind::NamedImports
            | Kind::ImportSpecifier
            | Kind::NamedExports
            | Kind::NamespaceExport
            | Kind::ExportSpecifier
            | Kind::ExternalModuleReference
            | Kind::JsxOpeningElement
            | Kind::JsxClosingElement
            | Kind::JsxOpeningFragment
            | Kind::JsxClosingFragment
            | Kind::JsxAttribute
            | Kind::JsxAttributes
            | Kind::JsxSpreadAttribute
            | Kind::JsxExpression
            | Kind::JsxNamespacedName
            | Kind::CaseClause
            | Kind::DefaultClause
            | Kind::HeritageClause
            | Kind::CatchClause
            | Kind::ImportAttributes
            | Kind::ImportAttribute
            | Kind::PropertyAssignment
            | Kind::ShorthandPropertyAssignment
            | Kind::SpreadAssignment
            | Kind::EnumMember
            | Kind::SourceFile
            | Kind::JSDocTypeExpression
            | Kind::JSDocNameReference
            | Kind::JSDocAllType
            | Kind::JSDocNullableType
            | Kind::JSDocNonNullableType
            | Kind::JSDocOptionalType
            | Kind::JSDocVariadicType
            | Kind::JSDoc
            | Kind::JSDocText
            | Kind::JSDocTypeLiteral
            | Kind::JSDocSignature
            | Kind::JSDocLink
            | Kind::JSDocLinkCode
            | Kind::JSDocLinkPlain
            | Kind::JSDocUnknownTag
            | Kind::JSDocAugmentsTag
            | Kind::JSDocImplementsTag
            | Kind::JSDocDeprecatedTag
            | Kind::JSDocPublicTag
            | Kind::JSDocPrivateTag
            | Kind::JSDocProtectedTag
            | Kind::JSDocReadonlyTag
            | Kind::JSDocOverrideTag
            | Kind::JSDocCallbackTag
            | Kind::JSDocOverloadTag
            | Kind::JSDocParameterTag
            | Kind::JSDocReturnTag
            | Kind::JSDocThisTag
            | Kind::JSDocTypeTag
            | Kind::JSDocTemplateTag
            | Kind::JSDocTypedefTag
            | Kind::JSDocSeeTag
            | Kind::JSDocPropertyTag
            | Kind::JSDocThrowsTag
            | Kind::JSDocSatisfiesTag
            | Kind::JSDocImportTag
            | Kind::SyntaxList
            | Kind::NotEmittedTypeElement
            | Kind::Count => {
                // These AST kinds are parser error-recovery placeholders (`Kind::Unknown`) or
                // non-executable token/trivia/type/JSDoc/clause nodes that should never appear in
                // valid statement positions; emitting `Unknown` records the gap safely.
                let here = self.new_vertex(
                    Some(node),
                    IrOp::Unknown {
                        reason: UnknownReason::UnmodeledConstruct,
                        message: format!(
                            "Control flow for {:?} is not precisely modeled.",
                            node.kind()
                        ),
                        location: self.loc(node),
                        evaluated_children: Vec::new(),
                    },
                    None,
                    false,
                );
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                here
            }
        }
    }

    fn lower_if(&mut self, node: P<Node>, next: VertexId) -> VertexId {
        let if_stmt = node.as_if_statement();
        let cond = if_stmt.expression();
        let then_entry = self.lower_stmt(if_stmt.then_statement(), next);
        let else_entry = if_stmt
            .else_statement()
            .map_or(next, |else_node| self.lower_stmt(else_node, next));

        let cond_loc = self.loc(cond);
        let stmt_loc = self.loc(node);
        let true_branch = self.push_branch(
            IrBranchKind::If,
            IrBranchDirection::True,
            cond,
            cond_loc.clone(),
            stmt_loc.clone(),
            None,
        );
        let false_branch = self.push_branch(
            IrBranchKind::If,
            IrBranchDirection::False,
            cond,
            cond_loc,
            stmt_loc,
            None,
        );

        let true_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(true_gate, then_entry, ExplicitEdgeKind::Branch(true_branch));
        let false_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(
            false_gate,
            else_entry,
            ExplicitEdgeKind::Branch(false_branch),
        );

        let cond_entry = self.lower_bool_expr(cond, true_gate, false_gate);
        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
        self.add_edge(here, cond_entry, ExplicitEdgeKind::Fallthrough);
        here
    }

    fn lower_switch(&mut self, node: P<Node>, next: VertexId) -> VertexId {
        let sw = node.as_switch_statement();
        let disc = sw.expression();
        let clauses = sw.case_block().as_case_block().clauses().nodes();

        let dummy_val = ValueId(0);
        let dispatch = self.new_vertex(
            None,
            IrOp::SwitchDispatch {
                discriminant: dummy_val,
            },
            None,
            false,
        );

        self.cf_frames.push(CfFrame::Switch { break_target: next });
        let mut fallthrough = next;
        let mut has_default = false;
        let mut clause_entries = Vec::with_capacity(clauses.len());
        for &clause in clauses.iter().rev() {
            has_default |= clause.kind() == Kind::DefaultClause;
            let body_entry = self.lower_sequence(clause.statements(), fallthrough);
            self.record_syntax_vertex(clause, body_entry);
            clause_entries.push((clause, body_entry));
            fallthrough = body_entry;
        }
        self.cf_frames.pop();
        clause_entries.reverse();

        for (clause, body_entry) in clause_entries {
            let clause_loc = self.loc(clause);
            if clause.kind() == Kind::CaseClause {
                let case_expr = clause.expression().unwrap_or(clause);
                let case_loc = self.loc(case_expr);
                let branch_id = self.push_branch(
                    IrBranchKind::SwitchCase,
                    IrBranchDirection::Case,
                    case_expr,
                    case_loc,
                    clause_loc,
                    None,
                );
                let (case_entry, _) = self.lower_expr_to(case_expr, body_entry);
                self.add_edge(dispatch, case_entry, ExplicitEdgeKind::Branch(branch_id));
            } else {
                let branch_id = self.push_branch(
                    IrBranchKind::SwitchDefault,
                    IrBranchDirection::Default,
                    disc,
                    clause_loc.clone(),
                    clause_loc,
                    Some("default".to_string()),
                );
                self.add_edge(dispatch, body_entry, ExplicitEdgeKind::Branch(branch_id));
            }
        }

        if !has_default {
            self.add_edge(
                dispatch,
                next,
                ExplicitEdgeKind::SwitchNoMatch {
                    switch_location: self.loc(node),
                },
            );
        }

        let (disc_entry, disc_val) = self.lower_expr_to(disc, dispatch);
        if let Some(v) = self.vertices.get_mut(dispatch.0 as usize) {
            v.op = IrOp::SwitchDispatch {
                discriminant: disc_val,
            };
        }
        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
        self.add_edge(here, disc_entry, ExplicitEdgeKind::Fallthrough);
        here
    }

    fn lower_variable_stmt(&mut self, node: P<Node>, next: VertexId) -> VertexId {
        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
        self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
        let decl_list = node.as_variable_statement().declaration_list();
        let decls = decl_list
            .as_variable_declaration_list()
            .declarations()
            .nodes();
        self.lower_variable_decls(decls, here)
    }

    fn lower_variable_decls(&mut self, decls: &[P<Node>], mut cont: VertexId) -> VertexId {
        for &decl in decls.iter().rev() {
            let mutability = classify_decl_mutability(decl);
            cont = self.lower_single_variable_decl(decl, mutability, None, cont);
        }
        cont
    }

    fn lower_single_variable_decl(
        &mut self,
        decl: P<Node>,
        mutability: BindingMutability,
        external_value: Option<ValueId>,
        cont: VertexId,
    ) -> VertexId {
        let decl_data = decl.as_variable_declaration();
        let name_node = decl.name();
        let init_opt = decl_data.initializer();

        if let Some(nn) = name_node
            && matches!(
                nn.kind(),
                Kind::ObjectBindingPattern | Kind::ArrayBindingPattern
            )
        {
            if let Some(ext_val) = external_value {
                let pat_entry = self.lower_binding_pattern(nn, ext_val, mutability, true, cont);
                self.record_syntax_vertex(decl, pat_entry);
                return pat_entry;
            }
            if let Some(init) = init_opt {
                let dummy = self.alloc_value();
                let pat_entry = self.lower_binding_pattern(nn, dummy, mutability, true, cont);
                // `pat_entry` references `dummy`; forward `init_val` into `dummy` via `ValueForward`.
                let fwd = self.new_vertex(
                    None,
                    IrOp::ValueForward {
                        source: dummy,
                        as_const: false,
                        non_null_assert: false,
                    },
                    Some(dummy),
                    false,
                );
                self.add_edge(fwd, pat_entry, ExplicitEdgeKind::Fallthrough);
                let (init_entry, init_val) = self.lower_expr_to(init, fwd);
                if let Some(v) = self.vertices.get_mut(fwd.0 as usize)
                    && let IrOp::ValueForward { source, .. } = &mut v.op
                {
                    *source = init_val;
                }
                self.record_syntax_vertex(decl, init_entry);
                return init_entry;
            }
            let undef_val = self.alloc_value();
            let undef_v = self.new_vertex(
                Some(decl),
                IrOp::Literal {
                    kind: LiteralKind::Undefined,
                    as_const: false,
                },
                Some(undef_val),
                false,
            );
            let pat_entry = self.lower_binding_pattern(nn, undef_val, mutability, true, cont);
            self.add_edge(undef_v, pat_entry, ExplicitEdgeKind::Fallthrough);
            return undef_v;
        }

        let val_id = external_value.unwrap_or_else(|| self.alloc_value());
        let op = if let Some(nn) = name_node
            && nn.kind() == Kind::Identifier
        {
            let name = node_text(nn).unwrap_or_default();
            let binding = self.register_binding(&name, mutability, decl);
            IrOp::AssignLocal {
                target: binding,
                op: AssignOp::Assign,
                value: val_id,
                is_init: true,
            }
        } else {
            let sid = self.intern_node(decl);
            IrOp::AssignSymbol {
                target_syntax: sid,
                op: AssignOp::Assign,
                value: val_id,
                is_init: true,
            }
        };

        let decl_v = self.new_vertex(Some(decl), op, Some(val_id), true);
        self.add_edge(decl_v, cont, ExplicitEdgeKind::Fallthrough);
        if let Some(nn) = name_node {
            self.record_syntax_vertex(nn, decl_v);
        }

        if external_value.is_none()
            && let Some(init) = init_opt
        {
            let (init_entry, init_val) = self.lower_expr_to(init, decl_v);
            if let Some(v) = self.vertices.get_mut(decl_v.0 as usize) {
                if let IrOp::AssignLocal { value, .. } = &mut v.op {
                    *value = init_val;
                } else if let IrOp::AssignSymbol { value, .. } = &mut v.op {
                    *value = init_val;
                }
            }
            return init_entry;
        }
        decl_v
    }

    fn lower_default_value_branch(
        &mut self,
        branch_node: P<Node>,
        raw_val: ValueId,
        default_expr: P<Node>,
        cont: VertexId,
    ) -> (VertexId, ValueId) {
        let out_val = self.alloc_value();
        let join = self.new_vertex(None, IrOp::NopMerge, Some(out_val), false);
        self.add_edge(join, cont, ExplicitEdgeKind::Fallthrough);

        let (def_entry, def_val) = self.lower_expr_to(default_expr, join);
        self.record_value_sources(out_val, &[raw_val, def_val]);
        let use_raw = self.new_vertex(
            None,
            IrOp::ValueForward {
                source: raw_val,
                as_const: false,
                non_null_assert: false,
            },
            Some(out_val),
            false,
        );
        self.add_edge(use_raw, join, ExplicitEdgeKind::Fallthrough);

        let cond_loc = self.loc(default_expr);
        let stmt_loc = self.loc(branch_node);
        let true_branch = self.push_branch(
            IrBranchKind::Conditional,
            IrBranchDirection::True,
            default_expr,
            cond_loc.clone(),
            stmt_loc.clone(),
            None,
        );
        let false_branch = self.push_branch(
            IrBranchKind::Conditional,
            IrBranchDirection::False,
            default_expr,
            cond_loc,
            stmt_loc,
            None,
        );

        let undef_val = self.alloc_value();
        let undef_v = self.new_vertex(
            None,
            IrOp::Literal {
                kind: LiteralKind::Undefined,
                as_const: false,
            },
            Some(undef_val),
            false,
        );
        let eq_val = self.alloc_value();
        let eq_v = self.new_vertex(
            None,
            IrOp::Binary {
                op: BinaryOp::StrictEq,
                left: raw_val,
                right: undef_val,
            },
            Some(eq_val),
            false,
        );
        let branch_v = self.new_vertex(None, IrOp::Branch { condition: eq_val }, None, false);
        self.add_edge(undef_v, eq_v, ExplicitEdgeKind::Fallthrough);
        self.add_edge(eq_v, branch_v, ExplicitEdgeKind::Fallthrough);
        self.add_edge(branch_v, def_entry, ExplicitEdgeKind::Branch(true_branch));
        self.add_edge(branch_v, use_raw, ExplicitEdgeKind::Branch(false_branch));
        (undef_v, out_val)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Handles identifier, object binding pattern, and array binding pattern lowering together."
    )]
    fn lower_binding_pattern(
        &mut self,
        pattern: P<Node>,
        source_val: ValueId,
        mutability: BindingMutability,
        is_init: bool,
        mut cont: VertexId,
    ) -> VertexId {
        let pat_kind = pattern.kind();
        if pat_kind == Kind::Identifier {
            let name = node_text(pattern).unwrap_or_default();
            let binding = self.register_binding(&name, mutability, pattern);
            let assign_v = self.new_vertex(
                Some(pattern),
                IrOp::AssignLocal {
                    target: binding,
                    op: AssignOp::Assign,
                    value: source_val,
                    is_init,
                },
                Some(source_val),
                true,
            );
            self.add_edge(assign_v, cont, ExplicitEdgeKind::Fallthrough);
            assign_v
        } else if pat_kind == Kind::ObjectBindingPattern {
            let elems = pattern.elements();
            let mut omitted_static_keys = Vec::new();
            let mut has_dynamic_omitted = false;
            for &elem in elems {
                if elem.kind() != Kind::BindingElement {
                    continue;
                }
                let be = elem.as_binding_element();
                if be.dot_dot_dot_token().is_some() {
                    continue;
                }
                if let Some(prop_name) = be.property_name() {
                    if prop_name.kind() == Kind::ComputedPropertyName {
                        if let Some(static_name) = prop_key_text(prop_name) {
                            omitted_static_keys.push(static_name);
                        } else {
                            has_dynamic_omitted = true;
                        }
                    } else if let Some(static_name) = prop_key_text(prop_name) {
                        omitted_static_keys.push(static_name);
                    }
                } else if let Some(name_node) = be.name()
                    && let Some(static_name) = prop_key_text(name_node)
                {
                    omitted_static_keys.push(static_name);
                }
            }

            for &elem in elems.iter().rev() {
                if elem.kind() != Kind::BindingElement {
                    continue;
                }
                let be = elem.as_binding_element();
                let Some(target_node) = be.name() else {
                    continue;
                };
                if be.dot_dot_dot_token().is_some() {
                    let rest_val = self.alloc_value();
                    let next_entry = self.lower_binding_pattern(
                        target_node,
                        rest_val,
                        mutability,
                        is_init,
                        cont,
                    );
                    let rest_v = self.new_vertex(
                        Some(elem),
                        IrOp::ObjectRest {
                            source: source_val,
                            omitted_static_keys: omitted_static_keys.clone(),
                            has_dynamic_omitted,
                        },
                        Some(rest_val),
                        true,
                    );
                    self.attach_may_throw(rest_v, elem, MayThrowReason::PropertyAccess);
                    self.add_edge(rest_v, next_entry, ExplicitEdgeKind::Fallthrough);
                    cont = be.initializer().map_or(rest_v, |err_init| {
                        let (init_entry, _) = self.lower_expr_to(err_init, rest_v);
                        init_entry
                    });
                    continue;
                }

                let raw_prop_val = self.alloc_value();
                let next_after_read = if let Some(default_expr) = be.initializer() {
                    let eff_slot = self.alloc_value();
                    let assign_entry = self.lower_binding_pattern(
                        target_node,
                        eff_slot,
                        mutability,
                        is_init,
                        cont,
                    );
                    let eff_fwd = self.new_vertex(
                        None,
                        IrOp::ValueForward {
                            source: eff_slot,
                            as_const: false,
                            non_null_assert: false,
                        },
                        Some(eff_slot),
                        false,
                    );
                    self.add_edge(eff_fwd, assign_entry, ExplicitEdgeKind::Fallthrough);
                    let (def_branch_entry, def_out_val) =
                        self.lower_default_value_branch(elem, raw_prop_val, default_expr, eff_fwd);
                    if let Some(v) = self.vertices.get_mut(eff_fwd.0 as usize)
                        && let IrOp::ValueForward { source, .. } = &mut v.op
                    {
                        *source = def_out_val;
                    }
                    def_branch_entry
                } else {
                    self.lower_binding_pattern(target_node, raw_prop_val, mutability, is_init, cont)
                };
                if let Some(prop_name) = be.property_name()
                    && prop_name.kind() == Kind::ComputedPropertyName
                {
                    let static_name = prop_key_text(prop_name);
                    let key_val_slot = self.alloc_value();
                    let read_v = self.new_vertex(
                        Some(elem),
                        IrOp::PropRead {
                            object: source_val,
                            key: IrPropKey::Computed {
                                key: key_val_slot,
                                static_name,
                            },
                            is_super: false,
                            implicit_call_kind: None,
                        },
                        Some(raw_prop_val),
                        true,
                    );
                    self.attach_may_throw(read_v, elem, MayThrowReason::PropertyAccess);
                    self.add_edge(read_v, next_after_read, ExplicitEdgeKind::Fallthrough);
                    if let Some(key_expr) = prop_name.expression() {
                        let (key_entry, key_val) = self.lower_expr_to(key_expr, read_v);
                        if let Some(v) = self.vertices.get_mut(read_v.0 as usize)
                            && let IrOp::PropRead {
                                key: IrPropKey::Computed { key, .. },
                                ..
                            } = &mut v.op
                        {
                            *key = key_val;
                        }
                        cont = key_entry;
                    } else {
                        cont = read_v;
                    }
                } else {
                    let key_name = be
                        .property_name()
                        .or_else(|| be.name())
                        .and_then(prop_key_text)
                        .unwrap_or_default();
                    let read_v = self.new_vertex(
                        Some(elem),
                        IrOp::PropRead {
                            object: source_val,
                            key: IrPropKey::Named(key_name),
                            is_super: false,
                            implicit_call_kind: None,
                        },
                        Some(raw_prop_val),
                        true,
                    );
                    self.attach_may_throw(read_v, elem, MayThrowReason::PropertyAccess);
                    self.add_edge(read_v, next_after_read, ExplicitEdgeKind::Fallthrough);
                    cont = read_v;
                }
            }
            self.record_syntax_vertex(pattern, cont);
            cont
        } else if pat_kind == Kind::ArrayBindingPattern {
            let iter_val = self.alloc_value();
            let close_v = self.new_vertex(
                Some(pattern),
                IrOp::IteratorClose {
                    iterator: iter_val,
                    is_async: false,
                },
                None,
                false,
            );
            self.attach_may_throw(close_v, pattern, MayThrowReason::IteratorStep);
            self.add_edge(close_v, cont, ExplicitEdgeKind::Fallthrough);
            cont = close_v;

            for &elem in pattern.elements().iter().rev() {
                if elem.kind() == Kind::OmittedExpression {
                    let skip_val = self.alloc_value();
                    let next_v = self.new_vertex(
                        Some(elem),
                        IrOp::IteratorNext {
                            iterator: iter_val,
                            is_async: false,
                        },
                        Some(skip_val),
                        false,
                    );
                    self.attach_may_throw(next_v, elem, MayThrowReason::IteratorStep);
                    self.add_edge(next_v, cont, ExplicitEdgeKind::Fallthrough);
                    cont = next_v;
                    continue;
                }
                if elem.kind() != Kind::BindingElement {
                    continue;
                }
                let be = elem.as_binding_element();
                let Some(target_node) = be.name() else {
                    continue;
                };
                if be.dot_dot_dot_token().is_some() {
                    let rest_val = self.alloc_value();
                    let target_entry = self.lower_binding_pattern(
                        target_node,
                        rest_val,
                        mutability,
                        is_init,
                        cont,
                    );
                    let rest_v = self.new_vertex(
                        Some(elem),
                        IrOp::IteratorRest {
                            iterator: iter_val,
                            is_async: false,
                        },
                        Some(rest_val),
                        true,
                    );
                    self.attach_may_throw(rest_v, elem, MayThrowReason::IteratorStep);
                    self.add_edge(rest_v, target_entry, ExplicitEdgeKind::Fallthrough);
                    cont = be.initializer().map_or(rest_v, |err_init| {
                        let (init_entry, _) = self.lower_expr_to(err_init, rest_v);
                        init_entry
                    });
                } else {
                    let raw_elem_val = self.alloc_value();
                    let after_next = if let Some(default_expr) = be.initializer() {
                        let step_slot = self.alloc_value();
                        let target_entry = self.lower_binding_pattern(
                            target_node,
                            step_slot,
                            mutability,
                            is_init,
                            cont,
                        );
                        let step_fwd = self.new_vertex(
                            None,
                            IrOp::ValueForward {
                                source: step_slot,
                                as_const: false,
                                non_null_assert: false,
                            },
                            Some(step_slot),
                            false,
                        );
                        self.add_edge(step_fwd, target_entry, ExplicitEdgeKind::Fallthrough);
                        let (def_entry, def_out_val) = self.lower_default_value_branch(
                            elem,
                            raw_elem_val,
                            default_expr,
                            step_fwd,
                        );
                        if let Some(v) = self.vertices.get_mut(step_fwd.0 as usize)
                            && let IrOp::ValueForward { source, .. } = &mut v.op
                        {
                            *source = def_out_val;
                        }
                        def_entry
                    } else {
                        self.lower_binding_pattern(
                            target_node,
                            raw_elem_val,
                            mutability,
                            is_init,
                            cont,
                        )
                    };
                    let next_v = self.new_vertex(
                        Some(elem),
                        IrOp::IteratorNext {
                            iterator: iter_val,
                            is_async: false,
                        },
                        Some(raw_elem_val),
                        true,
                    );
                    self.attach_may_throw(next_v, elem, MayThrowReason::IteratorStep);
                    self.add_edge(next_v, after_next, ExplicitEdgeKind::Fallthrough);
                    cont = next_v;
                }
            }

            let init_v = self.new_vertex(
                Some(pattern),
                IrOp::IteratorInit {
                    iterable: source_val,
                    is_async: false,
                },
                Some(iter_val),
                true,
            );
            self.attach_may_throw(init_v, pattern, MayThrowReason::IteratorStep);
            self.add_edge(init_v, cont, ExplicitEdgeKind::Fallthrough);
            init_v
        } else {
            self.record_syntax_vertex(pattern, cont);
            cont
        }
    }

    fn lower_function_parameters(&mut self, fn_node: P<Node>, mut cont: VertexId) -> VertexId {
        for &param in fn_node.parameters().iter().rev() {
            let param_data = param.as_parameter_declaration();
            let name_node = param.name();
            let init_opt = param_data.initializer();

            let is_pattern = name_node.is_some_and(|n| {
                matches!(
                    n.kind(),
                    Kind::ObjectBindingPattern | Kind::ArrayBindingPattern
                )
            });

            if init_opt.is_none() && !is_pattern {
                continue;
            }

            let raw_param_val = self.alloc_value();
            if let Some(init) = init_opt {
                if is_pattern && let Some(nn) = name_node {
                    let placeholder = self.alloc_value();
                    let pat_entry = self.lower_binding_pattern(
                        nn,
                        placeholder,
                        BindingMutability::Parameter,
                        true,
                        cont,
                    );
                    let pat_fwd = self.new_vertex(
                        None,
                        IrOp::ValueForward {
                            source: placeholder,
                            as_const: false,
                            non_null_assert: false,
                        },
                        Some(placeholder),
                        false,
                    );
                    self.add_edge(pat_fwd, pat_entry, ExplicitEdgeKind::Fallthrough);
                    let (def_entry, def_out_val) =
                        self.lower_default_value_branch(param, raw_param_val, init, pat_fwd);
                    if let Some(v) = self.vertices.get_mut(pat_fwd.0 as usize)
                        && let IrOp::ValueForward { source, .. } = &mut v.op
                    {
                        *source = def_out_val;
                    }
                    self.record_syntax_vertex(param, def_entry);
                    cont = def_entry;
                } else if let Some(nn) = name_node
                    && nn.kind() == Kind::Identifier
                {
                    let name = node_text(nn).unwrap_or_default();
                    let binding = self.register_binding(&name, BindingMutability::Parameter, param);
                    let placeholder = self.alloc_value();
                    let assign_v = self.new_vertex(
                        Some(param),
                        IrOp::AssignLocal {
                            target: binding,
                            op: AssignOp::Assign,
                            value: placeholder,
                            is_init: true,
                        },
                        Some(placeholder),
                        true,
                    );
                    self.add_edge(assign_v, cont, ExplicitEdgeKind::Fallthrough);
                    let (def_entry, def_val) =
                        self.lower_default_value_branch(param, raw_param_val, init, assign_v);
                    if let Some(v) = self.vertices.get_mut(assign_v.0 as usize)
                        && let IrOp::AssignLocal { value, .. } = &mut v.op
                    {
                        *value = def_val;
                    }
                    cont = def_entry;
                } else {
                    let (init_entry, _) = self.lower_expr_to(init, cont);
                    cont = init_entry;
                }
            } else if is_pattern && let Some(nn) = name_node {
                let pat_entry = self.lower_binding_pattern(
                    nn,
                    raw_param_val,
                    BindingMutability::Parameter,
                    true,
                    cont,
                );
                self.record_syntax_vertex(param, pat_entry);
                cont = pat_entry;
            }
        }
        cont
    }

    fn lower_try(&mut self, node: P<Node>, next: VertexId) -> VertexId {
        let try_stmt = node.as_try_statement();
        let has_finally = try_stmt.finally_block().is_some();

        let try_normal_cont = if let Some(fb) = try_stmt.finally_block() {
            let fid = FinallyId(self.next_finally_id);
            self.next_finally_id = self.next_finally_id.saturating_add(1);
            self.active_finally_bodies.push(fid);
            let finally_entry = self.new_vertex(None, IrOp::NopMerge, None, false);
            let finally_normal_exit = self.new_vertex(None, IrOp::NopMerge, None, false);
            let f_body = self.lower_stmt(fb, finally_normal_exit);
            self.add_edge(finally_entry, f_body, ExplicitEdgeKind::Fallthrough);
            self.active_finally_bodies.pop();

            let frame_idx = self.cf_frames.len();
            self.cf_frames.push(CfFrame::Finally {
                id: fid,
                entry: finally_entry,
                normal_exit: finally_normal_exit,
                trampolines: FxHashMap::default(),
            });
            self.wrap_with_finallies_above(frame_idx, next)
        } else {
            next
        };

        let try_entry = if let Some(catch_node) = try_stmt.catch_clause() {
            let cc = catch_node.as_catch_clause();
            let block_entry = self.lower_stmt(cc.block(), try_normal_cont);
            let catch_entry = cc.variable_declaration().map_or(block_entry, |var_decl| {
                self.lower_catch_param(var_decl, block_entry)
            });
            self.record_syntax_vertex(catch_node, catch_entry);
            self.cf_frames.push(CfFrame::Catch { entry: catch_entry });
            let entry = self.lower_stmt(try_stmt.try_block(), try_normal_cont);
            self.cf_frames.pop();
            entry
        } else {
            self.lower_stmt(try_stmt.try_block(), try_normal_cont)
        };

        if has_finally {
            self.cf_frames.pop();
        }

        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
        self.add_edge(here, try_entry, ExplicitEdgeKind::Fallthrough);
        here
    }

    fn lower_catch_param(&mut self, var_decl: P<Node>, cont: VertexId) -> VertexId {
        let name_node = var_decl.name();
        let binding = if let Some(nn) = name_node
            && nn.kind() == Kind::Identifier
        {
            let name = node_text(nn).unwrap_or_default();
            Some(self.register_binding(&name, BindingMutability::CatchBinding, var_decl))
        } else {
            None
        };
        let catch_val = self.alloc_value();
        let bind_v = self.new_vertex(
            Some(var_decl),
            IrOp::CatchParamBind { binding },
            Some(catch_val),
            true,
        );
        let after_bind = if let Some(nn) = name_node
            && matches!(
                nn.kind(),
                Kind::ObjectBindingPattern | Kind::ArrayBindingPattern
            ) {
            self.lower_binding_pattern(nn, catch_val, BindingMutability::CatchBinding, true, cont)
        } else {
            if let Some(nn) = name_node {
                self.record_syntax_vertex(nn, bind_v);
            }
            cont
        };
        self.add_edge(bind_v, after_bind, ExplicitEdgeKind::Fallthrough);
        bind_v
    }

    fn lower_while(&mut self, node: P<Node>, next: VertexId, labels: Vec<String>) -> VertexId {
        let w = node.as_while_statement();
        let cond = w.expression();
        let cond_head = self.new_vertex(None, IrOp::NopMerge, None, false);
        let loop_back = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(loop_back, cond_head, ExplicitEdgeKind::LoopBack);

        self.cf_frames.push(CfFrame::Loop {
            labels,
            break_target: next,
            continue_target: cond_head,
        });
        let body_entry = self.lower_stmt(w.statement(), loop_back);
        self.cf_frames.pop();

        let (true_gate, false_gate) = self.emit_loop_branch_gates(node, cond, body_entry, next);
        let cond_entry = self.lower_bool_expr(cond, true_gate, false_gate);
        self.add_edge(cond_head, cond_entry, ExplicitEdgeKind::Fallthrough);

        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
        self.add_edge(here, cond_head, ExplicitEdgeKind::Fallthrough);
        here
    }

    fn lower_do_while(&mut self, node: P<Node>, next: VertexId, labels: Vec<String>) -> VertexId {
        let d = node.as_do_statement();
        let cond = d.expression();
        let body_head = self.new_vertex(None, IrOp::NopMerge, None, false);
        let cond_head = self.new_vertex(None, IrOp::NopMerge, None, false);
        let loop_back = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(loop_back, body_head, ExplicitEdgeKind::LoopBack);

        self.cf_frames.push(CfFrame::Loop {
            labels,
            break_target: next,
            continue_target: cond_head,
        });
        let body_entry = self.lower_stmt(d.statement(), cond_head);
        self.cf_frames.pop();
        self.add_edge(body_head, body_entry, ExplicitEdgeKind::Fallthrough);

        let (true_gate, false_gate) = self.emit_loop_branch_gates(node, cond, loop_back, next);
        let cond_entry = self.lower_bool_expr(cond, true_gate, false_gate);
        self.add_edge(cond_head, cond_entry, ExplicitEdgeKind::Fallthrough);

        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
        self.add_edge(here, body_head, ExplicitEdgeKind::Fallthrough);
        here
    }

    fn lower_for(&mut self, node: P<Node>, next: VertexId, labels: Vec<String>) -> VertexId {
        let f = node.as_for_statement();
        let cond_head = self.new_vertex(None, IrOp::NopMerge, None, false);
        let incr_head = self.new_vertex(None, IrOp::NopMerge, None, false);
        let loop_back = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(loop_back, cond_head, ExplicitEdgeKind::LoopBack);

        let incr_entry = f.incrementor().map_or(loop_back, |incr| {
            let (entry, _) = self.lower_expr_to(incr, loop_back);
            entry
        });
        self.add_edge(incr_head, incr_entry, ExplicitEdgeKind::Fallthrough);

        self.cf_frames.push(CfFrame::Loop {
            labels,
            break_target: next,
            continue_target: incr_head,
        });
        let body_entry = self.lower_stmt(f.statement(), incr_head);
        self.cf_frames.pop();

        if let Some(cond) = f.condition() {
            let (true_gate, false_gate) = self.emit_loop_branch_gates(node, cond, body_entry, next);
            let cond_entry = self.lower_bool_expr(cond, true_gate, false_gate);
            self.add_edge(cond_head, cond_entry, ExplicitEdgeKind::Fallthrough);
        } else {
            self.add_edge(cond_head, body_entry, ExplicitEdgeKind::Fallthrough);
        }

        let start = f
            .initializer()
            .map_or(cond_head, |init| self.lower_stmt(init, cond_head));

        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
        self.add_edge(here, start, ExplicitEdgeKind::Fallthrough);
        here
    }

    fn lower_for_in_or_of(
        &mut self,
        node: P<Node>,
        next: VertexId,
        labels: Vec<String>,
    ) -> VertexId {
        let f = node.as_for_in_or_of_statement();
        let is_for_of = node.kind() == Kind::ForOfStatement;
        let is_async = f.await_modifier().is_some();
        let iterable_expr = f.expression();
        let lhs = f.initializer();

        let iter_head = self.new_vertex(None, IrOp::NopMerge, None, false);
        let loop_back = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(loop_back, iter_head, ExplicitEdgeKind::LoopBack);

        self.cf_frames.push(CfFrame::Loop {
            labels,
            break_target: next,
            continue_target: iter_head,
        });
        let raw_body_entry = self.lower_stmt(f.statement(), loop_back);
        self.cf_frames.pop();

        let step_val = self.alloc_value();
        let lhs_entry = if lhs.kind() == Kind::VariableDeclarationList {
            let flags = tsrs_ast::get_combined_node_flags(lhs);
            let decls = lhs.as_variable_declaration_list().declarations().nodes();
            if flags.intersects(NodeFlags::Using)
                && let Some(&using_decl) = decls.first()
            {
                let using_async = flags.contains(NodeFlags::AwaitUsing);
                self.lower_using_decls(&[using_decl], using_async, &[], raw_body_entry)
            } else if let Some(&decl) = decls.first() {
                let mutability = classify_decl_mutability(decl);
                let entry = self.lower_single_variable_decl(
                    decl,
                    mutability,
                    Some(step_val),
                    raw_body_entry,
                );
                self.record_syntax_vertex(lhs, entry);
                entry
            } else {
                raw_body_entry
            }
        } else {
            self.lower_assign_destructure_or_target(lhs, lhs, step_val, raw_body_entry)
        };

        let iter_val = self.alloc_value();
        let false_target = if is_for_of {
            let close_v = self.new_vertex(
                Some(node),
                IrOp::IteratorClose {
                    iterator: iter_val,
                    is_async,
                },
                None,
                false,
            );
            self.attach_may_throw(close_v, node, MayThrowReason::IteratorStep);
            self.add_edge(close_v, next, ExplicitEdgeKind::Fallthrough);
            close_v
        } else {
            next
        };

        let (true_gate, false_gate) =
            self.emit_loop_branch_gates(node, iterable_expr, lhs_entry, false_target);

        let next_v = self.new_vertex(
            Some(node),
            IrOp::IteratorNext {
                iterator: iter_val,
                is_async,
            },
            Some(step_val),
            true,
        );
        self.attach_may_throw(next_v, node, MayThrowReason::IteratorStep);
        self.add_edge(next_v, true_gate, ExplicitEdgeKind::Fallthrough);
        self.add_edge(next_v, false_gate, ExplicitEdgeKind::Fallthrough);
        self.add_edge(iter_head, next_v, ExplicitEdgeKind::Fallthrough);

        let init_v = self.new_vertex(
            Some(node),
            IrOp::IteratorInit {
                iterable: iter_val,
                is_async,
            },
            Some(iter_val),
            true,
        );
        self.attach_may_throw(init_v, iterable_expr, MayThrowReason::IteratorStep);
        self.add_edge(init_v, iter_head, ExplicitEdgeKind::Fallthrough);

        let (expr_entry, expr_val) = self.lower_expr_to(iterable_expr, init_v);
        if let Some(v) = self.vertices.get_mut(init_v.0 as usize)
            && let IrOp::IteratorInit { iterable, .. } = &mut v.op
        {
            *iterable = expr_val;
        }

        let start_entry = self.lower_for_head_initializers(lhs, expr_entry);
        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
        self.add_edge(here, start_entry, ExplicitEdgeKind::Fallthrough);
        here
    }

    fn lower_for_head_initializers(&mut self, lhs: P<Node>, mut cont: VertexId) -> VertexId {
        if lhs.kind() != Kind::VariableDeclarationList {
            return cont;
        }
        let decls = lhs.as_variable_declaration_list().declarations().nodes();
        for &decl in decls.iter().rev() {
            let Some(head_init) = decl.as_variable_declaration().initializer() else {
                continue;
            };
            if let Some(nn) = decl.name()
                && nn.kind() == Kind::Identifier
            {
                let mutability = classify_decl_mutability(decl);
                let name = node_text(nn).unwrap_or_default();
                let binding = self.register_binding(&name, mutability, decl);
                let dummy = ValueId(0);
                let assign_v = self.new_vertex(
                    Some(decl),
                    IrOp::AssignLocal {
                        target: binding,
                        op: AssignOp::Assign,
                        value: dummy,
                        is_init: true,
                    },
                    Some(dummy),
                    true,
                );
                self.add_edge(assign_v, cont, ExplicitEdgeKind::Fallthrough);
                let (ie, iv) = self.lower_expr_to(head_init, assign_v);
                if let Some(v) = self.vertices.get_mut(assign_v.0 as usize)
                    && let IrOp::AssignLocal { value, .. } = &mut v.op
                {
                    *value = iv;
                    v.value = Some(iv);
                }
                cont = ie;
            } else {
                let (ie, _) = self.lower_expr_to(head_init, cont);
                cont = ie;
            }
        }
        cont
    }

    fn emit_loop_branch_gates(
        &mut self,
        stmt_node: P<Node>,
        cond_node: P<Node>,
        body_entry: VertexId,
        exit_next: VertexId,
    ) -> (VertexId, VertexId) {
        let cond_loc = self.loc(cond_node);
        let stmt_loc = self.loc(stmt_node);
        let true_branch = self.push_branch(
            IrBranchKind::Loop,
            IrBranchDirection::True,
            cond_node,
            cond_loc.clone(),
            stmt_loc.clone(),
            None,
        );
        let false_branch = self.push_branch(
            IrBranchKind::Loop,
            IrBranchDirection::False,
            cond_node,
            cond_loc,
            stmt_loc,
            None,
        );

        let true_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(true_gate, body_entry, ExplicitEdgeKind::Branch(true_branch));
        let false_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(
            false_gate,
            exit_next,
            ExplicitEdgeKind::Branch(false_branch),
        );
        (true_gate, false_gate)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Exhaustive 353-variant tsrs_ast::Kind boolean-expression dispatcher."
    )]
    fn lower_bool_expr(
        &mut self,
        node: P<Node>,
        on_true: VertexId,
        on_false: VertexId,
    ) -> VertexId {
        match node.kind() {
            Kind::ParenthesizedExpression => {
                let inner = node.as_parenthesized_expression().expression();
                let entry = self.lower_bool_expr(inner, on_true, on_false);
                self.alias_expr_vertex(node, inner, entry);
                entry
            }
            Kind::PrefixUnaryExpression
                if node.as_prefix_unary_expression().operator() == Kind::ExclamationToken =>
            {
                let operand = node.as_prefix_unary_expression().operand();
                let entry = self.lower_bool_expr(operand, on_false, on_true);
                self.alias_expr_vertex(node, operand, entry);
                entry
            }
            Kind::BinaryExpression => {
                let bin = node.as_binary_expression();
                let op_kind = bin.operator_token().kind();
                if op_kind == Kind::AmpersandAmpersandToken {
                    self.lower_logical_and_bool(node, bin.left(), bin.right(), on_true, on_false)
                } else if op_kind == Kind::BarBarToken {
                    self.lower_logical_or_bool(node, bin.left(), bin.right(), on_true, on_false)
                } else {
                    self.lower_bool_leaf(node, on_true, on_false)
                }
            }
            Kind::ConditionalExpression => {
                let cond_expr = node.as_conditional_expression();
                let true_entry = self.lower_bool_expr(cond_expr.when_true(), on_true, on_false);
                let false_entry = self.lower_bool_expr(cond_expr.when_false(), on_true, on_false);
                self.build_conditional_branches(
                    node,
                    cond_expr.condition(),
                    true_entry,
                    false_entry,
                )
            }
            // All remaining 349 Kind variants evaluate as a boolean leaf expression.
            Kind::Unknown
            | Kind::EndOfFile
            | Kind::SingleLineCommentTrivia
            | Kind::MultiLineCommentTrivia
            | Kind::NewLineTrivia
            | Kind::WhitespaceTrivia
            | Kind::ConflictMarkerTrivia
            | Kind::NonTextFileMarkerTrivia
            | Kind::NumericLiteral
            | Kind::BigIntLiteral
            | Kind::StringLiteral
            | Kind::JsxText
            | Kind::JsxTextAllWhiteSpaces
            | Kind::RegularExpressionLiteral
            | Kind::NoSubstitutionTemplateLiteral
            | Kind::TemplateHead
            | Kind::TemplateMiddle
            | Kind::TemplateTail
            | Kind::OpenBraceToken
            | Kind::CloseBraceToken
            | Kind::OpenParenToken
            | Kind::CloseParenToken
            | Kind::OpenBracketToken
            | Kind::CloseBracketToken
            | Kind::DotToken
            | Kind::DotDotDotToken
            | Kind::SemicolonToken
            | Kind::CommaToken
            | Kind::QuestionDotToken
            | Kind::LessThanToken
            | Kind::LessThanSlashToken
            | Kind::GreaterThanToken
            | Kind::LessThanEqualsToken
            | Kind::GreaterThanEqualsToken
            | Kind::EqualsEqualsToken
            | Kind::ExclamationEqualsToken
            | Kind::EqualsEqualsEqualsToken
            | Kind::ExclamationEqualsEqualsToken
            | Kind::EqualsGreaterThanToken
            | Kind::PlusToken
            | Kind::MinusToken
            | Kind::AsteriskToken
            | Kind::AsteriskAsteriskToken
            | Kind::SlashToken
            | Kind::PercentToken
            | Kind::PlusPlusToken
            | Kind::MinusMinusToken
            | Kind::LessThanLessThanToken
            | Kind::GreaterThanGreaterThanToken
            | Kind::GreaterThanGreaterThanGreaterThanToken
            | Kind::AmpersandToken
            | Kind::BarToken
            | Kind::CaretToken
            | Kind::ExclamationToken
            | Kind::TildeToken
            | Kind::AmpersandAmpersandToken
            | Kind::BarBarToken
            | Kind::QuestionToken
            | Kind::ColonToken
            | Kind::AtToken
            | Kind::QuestionQuestionToken
            | Kind::BacktickToken
            | Kind::HashToken
            | Kind::EqualsToken
            | Kind::PlusEqualsToken
            | Kind::MinusEqualsToken
            | Kind::AsteriskEqualsToken
            | Kind::AsteriskAsteriskEqualsToken
            | Kind::SlashEqualsToken
            | Kind::PercentEqualsToken
            | Kind::LessThanLessThanEqualsToken
            | Kind::GreaterThanGreaterThanEqualsToken
            | Kind::GreaterThanGreaterThanGreaterThanEqualsToken
            | Kind::AmpersandEqualsToken
            | Kind::BarEqualsToken
            | Kind::BarBarEqualsToken
            | Kind::AmpersandAmpersandEqualsToken
            | Kind::QuestionQuestionEqualsToken
            | Kind::CaretEqualsToken
            | Kind::Identifier
            | Kind::PrivateIdentifier
            | Kind::JSDocCommentTextToken
            | Kind::BreakKeyword
            | Kind::CaseKeyword
            | Kind::CatchKeyword
            | Kind::ClassKeyword
            | Kind::ConstKeyword
            | Kind::ContinueKeyword
            | Kind::DebuggerKeyword
            | Kind::DefaultKeyword
            | Kind::DeleteKeyword
            | Kind::DoKeyword
            | Kind::ElseKeyword
            | Kind::EnumKeyword
            | Kind::ExportKeyword
            | Kind::ExtendsKeyword
            | Kind::FalseKeyword
            | Kind::FinallyKeyword
            | Kind::ForKeyword
            | Kind::FunctionKeyword
            | Kind::IfKeyword
            | Kind::ImportKeyword
            | Kind::InKeyword
            | Kind::InstanceOfKeyword
            | Kind::NewKeyword
            | Kind::NullKeyword
            | Kind::ReturnKeyword
            | Kind::SuperKeyword
            | Kind::SwitchKeyword
            | Kind::ThisKeyword
            | Kind::ThrowKeyword
            | Kind::TrueKeyword
            | Kind::TryKeyword
            | Kind::TypeOfKeyword
            | Kind::VarKeyword
            | Kind::VoidKeyword
            | Kind::WhileKeyword
            | Kind::WithKeyword
            | Kind::ImplementsKeyword
            | Kind::InterfaceKeyword
            | Kind::LetKeyword
            | Kind::PackageKeyword
            | Kind::PrivateKeyword
            | Kind::ProtectedKeyword
            | Kind::PublicKeyword
            | Kind::StaticKeyword
            | Kind::YieldKeyword
            | Kind::AbstractKeyword
            | Kind::AccessorKeyword
            | Kind::AsKeyword
            | Kind::AssertsKeyword
            | Kind::AssertKeyword
            | Kind::AnyKeyword
            | Kind::AsyncKeyword
            | Kind::AwaitKeyword
            | Kind::BooleanKeyword
            | Kind::ConstructorKeyword
            | Kind::DeclareKeyword
            | Kind::GetKeyword
            | Kind::ImmediateKeyword
            | Kind::InferKeyword
            | Kind::IntrinsicKeyword
            | Kind::IsKeyword
            | Kind::KeyOfKeyword
            | Kind::ModuleKeyword
            | Kind::NamespaceKeyword
            | Kind::NeverKeyword
            | Kind::OutKeyword
            | Kind::ReadonlyKeyword
            | Kind::RequireKeyword
            | Kind::NumberKeyword
            | Kind::ObjectKeyword
            | Kind::SatisfiesKeyword
            | Kind::SetKeyword
            | Kind::StringKeyword
            | Kind::SymbolKeyword
            | Kind::TypeKeyword
            | Kind::UndefinedKeyword
            | Kind::UniqueKeyword
            | Kind::UnknownKeyword
            | Kind::UsingKeyword
            | Kind::FromKeyword
            | Kind::GlobalKeyword
            | Kind::BigIntKeyword
            | Kind::OverrideKeyword
            | Kind::OfKeyword
            | Kind::DeferKeyword
            | Kind::SourceKeyword
            | Kind::QualifiedName
            | Kind::TypeParameter
            | Kind::Parameter
            | Kind::PropertySignature
            | Kind::PropertyDeclaration
            | Kind::MethodSignature
            | Kind::MethodDeclaration
            | Kind::ClassStaticBlockDeclaration
            | Kind::Constructor
            | Kind::GetAccessor
            | Kind::SetAccessor
            | Kind::CallSignature
            | Kind::ConstructSignature
            | Kind::IndexSignature
            | Kind::TypePredicate
            | Kind::TypeReference
            | Kind::FunctionType
            | Kind::ConstructorType
            | Kind::TypeQuery
            | Kind::TypeLiteral
            | Kind::ArrayType
            | Kind::TupleType
            | Kind::OptionalType
            | Kind::RestType
            | Kind::UnionType
            | Kind::IntersectionType
            | Kind::ConditionalType
            | Kind::InferType
            | Kind::ParenthesizedType
            | Kind::ThisType
            | Kind::TypeOperator
            | Kind::IndexedAccessType
            | Kind::MappedType
            | Kind::LiteralType
            | Kind::NamedTupleMember
            | Kind::TemplateLiteralType
            | Kind::TemplateLiteralTypeSpan
            | Kind::ImportType
            | Kind::ObjectBindingPattern
            | Kind::ArrayBindingPattern
            | Kind::BindingElement
            | Kind::ArrayLiteralExpression
            | Kind::ObjectLiteralExpression
            | Kind::PropertyAccessExpression
            | Kind::ElementAccessExpression
            | Kind::CallExpression
            | Kind::NewExpression
            | Kind::TaggedTemplateExpression
            | Kind::TypeAssertionExpression
            | Kind::FunctionExpression
            | Kind::ArrowFunction
            | Kind::DeleteExpression
            | Kind::TypeOfExpression
            | Kind::VoidExpression
            | Kind::AwaitExpression
            | Kind::PrefixUnaryExpression
            | Kind::PostfixUnaryExpression
            | Kind::TemplateExpression
            | Kind::YieldExpression
            | Kind::SpreadElement
            | Kind::ClassExpression
            | Kind::OmittedExpression
            | Kind::ExpressionWithTypeArguments
            | Kind::AsExpression
            | Kind::NonNullExpression
            | Kind::MetaProperty
            | Kind::SyntheticExpression
            | Kind::SatisfiesExpression
            | Kind::TemplateSpan
            | Kind::SemicolonClassElement
            | Kind::Block
            | Kind::EmptyStatement
            | Kind::VariableStatement
            | Kind::ExpressionStatement
            | Kind::IfStatement
            | Kind::DoStatement
            | Kind::WhileStatement
            | Kind::ForStatement
            | Kind::ForInStatement
            | Kind::ForOfStatement
            | Kind::ContinueStatement
            | Kind::BreakStatement
            | Kind::ReturnStatement
            | Kind::WithStatement
            | Kind::SwitchStatement
            | Kind::LabeledStatement
            | Kind::ThrowStatement
            | Kind::TryStatement
            | Kind::DebuggerStatement
            | Kind::VariableDeclaration
            | Kind::VariableDeclarationList
            | Kind::FunctionDeclaration
            | Kind::ClassDeclaration
            | Kind::InterfaceDeclaration
            | Kind::TypeAliasDeclaration
            | Kind::EnumDeclaration
            | Kind::ModuleDeclaration
            | Kind::ModuleBlock
            | Kind::CaseBlock
            | Kind::NamespaceExportDeclaration
            | Kind::ImportEqualsDeclaration
            | Kind::ImportDeclaration
            | Kind::ImportClause
            | Kind::NamespaceImport
            | Kind::NamedImports
            | Kind::ImportSpecifier
            | Kind::ExportAssignment
            | Kind::ExportDeclaration
            | Kind::NamedExports
            | Kind::NamespaceExport
            | Kind::ExportSpecifier
            | Kind::MissingDeclaration
            | Kind::ExternalModuleReference
            | Kind::JsxElement
            | Kind::JsxSelfClosingElement
            | Kind::JsxOpeningElement
            | Kind::JsxClosingElement
            | Kind::JsxFragment
            | Kind::JsxOpeningFragment
            | Kind::JsxClosingFragment
            | Kind::JsxAttribute
            | Kind::JsxAttributes
            | Kind::JsxSpreadAttribute
            | Kind::JsxExpression
            | Kind::JsxNamespacedName
            | Kind::CaseClause
            | Kind::DefaultClause
            | Kind::HeritageClause
            | Kind::CatchClause
            | Kind::ImportAttributes
            | Kind::ImportAttribute
            | Kind::PropertyAssignment
            | Kind::ShorthandPropertyAssignment
            | Kind::SpreadAssignment
            | Kind::EnumMember
            | Kind::SourceFile
            | Kind::JSDocTypeExpression
            | Kind::JSDocNameReference
            | Kind::JSDocAllType
            | Kind::JSDocNullableType
            | Kind::JSDocNonNullableType
            | Kind::JSDocOptionalType
            | Kind::JSDocVariadicType
            | Kind::JSDoc
            | Kind::JSDocText
            | Kind::JSDocTypeLiteral
            | Kind::JSDocSignature
            | Kind::JSDocLink
            | Kind::JSDocLinkCode
            | Kind::JSDocLinkPlain
            | Kind::JSDocUnknownTag
            | Kind::JSDocAugmentsTag
            | Kind::JSDocImplementsTag
            | Kind::JSDocDeprecatedTag
            | Kind::JSDocPublicTag
            | Kind::JSDocPrivateTag
            | Kind::JSDocProtectedTag
            | Kind::JSDocReadonlyTag
            | Kind::JSDocOverrideTag
            | Kind::JSDocCallbackTag
            | Kind::JSDocOverloadTag
            | Kind::JSDocParameterTag
            | Kind::JSDocReturnTag
            | Kind::JSDocThisTag
            | Kind::JSDocTypeTag
            | Kind::JSDocTemplateTag
            | Kind::JSDocTypedefTag
            | Kind::JSDocSeeTag
            | Kind::JSDocPropertyTag
            | Kind::JSDocThrowsTag
            | Kind::JSDocSatisfiesTag
            | Kind::JSDocImportTag
            | Kind::SyntaxList
            | Kind::JSTypeAliasDeclaration
            | Kind::JSImportDeclaration
            | Kind::NotEmittedStatement
            | Kind::PartiallyEmittedExpression
            | Kind::SyntheticReferenceExpression
            | Kind::NotEmittedTypeElement
            | Kind::ComputedPropertyName
            | Kind::Decorator
            | Kind::Count => self.lower_bool_leaf(node, on_true, on_false),
        }
    }

    fn lower_bool_leaf(
        &mut self,
        node: P<Node>,
        on_true: VertexId,
        on_false: VertexId,
    ) -> VertexId {
        let split = self.new_vertex(None, IrOp::NopMerge, None, false);
        match static_bool_value(node) {
            Some(true) => self.add_edge(split, on_true, ExplicitEdgeKind::Fallthrough),
            Some(false) => self.add_edge(split, on_false, ExplicitEdgeKind::Fallthrough),
            None => {
                self.add_edge(split, on_true, ExplicitEdgeKind::Fallthrough);
                self.add_edge(split, on_false, ExplicitEdgeKind::Fallthrough);
            }
        }
        let (entry, cond_val) = self.lower_expr_to(node, split);
        if let Some(v) = self.vertices.get_mut(split.0 as usize) {
            v.op = IrOp::Branch {
                condition: cond_val,
            };
        }
        entry
    }

    fn lower_logical_and_bool(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        right: P<Node>,
        on_true: VertexId,
        on_false: VertexId,
    ) -> VertexId {
        let right_entry = self.lower_bool_expr(right, on_true, on_false);
        let expr_loc = self.loc(node);
        let left_loc = self.loc(left);
        let cont_branch = self.push_branch(
            IrBranchKind::LogicalAnd,
            IrBranchDirection::Continue,
            left,
            left_loc.clone(),
            expr_loc.clone(),
            None,
        );
        let short_branch = self.push_branch(
            IrBranchKind::LogicalAnd,
            IrBranchDirection::ShortCircuit,
            left,
            left_loc,
            expr_loc,
            None,
        );

        let cont_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(
            cont_gate,
            right_entry,
            ExplicitEdgeKind::Branch(cont_branch),
        );
        let short_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(short_gate, on_false, ExplicitEdgeKind::Branch(short_branch));

        let out_val = self.alloc_value();
        let here = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
        let left_entry = self.lower_bool_expr(left, cont_gate, short_gate);
        self.add_edge(here, left_entry, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, here, out_val);
        here
    }

    fn lower_logical_or_bool(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        right: P<Node>,
        on_true: VertexId,
        on_false: VertexId,
    ) -> VertexId {
        let right_entry = self.lower_bool_expr(right, on_true, on_false);
        let expr_loc = self.loc(node);
        let left_loc = self.loc(left);
        let short_branch = self.push_branch(
            IrBranchKind::LogicalOr,
            IrBranchDirection::ShortCircuit,
            left,
            left_loc.clone(),
            expr_loc.clone(),
            None,
        );
        let cont_branch = self.push_branch(
            IrBranchKind::LogicalOr,
            IrBranchDirection::Continue,
            left,
            left_loc,
            expr_loc,
            None,
        );

        let short_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(short_gate, on_true, ExplicitEdgeKind::Branch(short_branch));
        let cont_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(
            cont_gate,
            right_entry,
            ExplicitEdgeKind::Branch(cont_branch),
        );

        let out_val = self.alloc_value();
        let here = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
        let left_entry = self.lower_bool_expr(left, short_gate, cont_gate);
        self.add_edge(here, left_entry, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, here, out_val);
        here
    }

    fn build_conditional_branches(
        &mut self,
        node: P<Node>,
        cond: P<Node>,
        true_entry: VertexId,
        false_entry: VertexId,
    ) -> VertexId {
        let cond_loc = self.loc(cond);
        let stmt_loc = self.loc(node);
        let true_branch = self.push_branch(
            IrBranchKind::Conditional,
            IrBranchDirection::True,
            cond,
            cond_loc.clone(),
            stmt_loc.clone(),
            None,
        );
        let false_branch = self.push_branch(
            IrBranchKind::Conditional,
            IrBranchDirection::False,
            cond,
            cond_loc,
            stmt_loc,
            None,
        );

        let true_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(true_gate, true_entry, ExplicitEdgeKind::Branch(true_branch));
        let false_gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(
            false_gate,
            false_entry,
            ExplicitEdgeKind::Branch(false_branch),
        );

        let out_val = self.alloc_value();
        let here = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
        let cond_entry = self.lower_bool_expr(cond, true_gate, false_gate);
        self.add_edge(here, cond_entry, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, here, out_val);
        here
    }

    fn alias_expr_vertex(&mut self, outer: P<Node>, inner: P<Node>, fallback: VertexId) {
        let inner_sid = self.intern_node(inner).local_id();
        let vid = self
            .expr_to_vertex
            .get(&inner_sid)
            .copied()
            .unwrap_or(fallback);
        let val = self
            .expr_to_value
            .get(&inner_sid)
            .copied()
            .unwrap_or_else(|| self.alloc_value());
        self.bind_expr_maps(outer, vid, val);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Exhaustive 353-variant tsrs_ast::Kind expression dispatcher."
    )]
    fn lower_expr_to(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        if is_non_runtime_type_node(node) {
            let val = self.alloc_value();
            let v = self.new_vertex(
                Some(node),
                IrOp::Literal {
                    kind: LiteralKind::Undefined,
                    as_const: false,
                },
                Some(val),
                false,
            );
            self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
            self.bind_expr_maps(node, v, val);
            return (v, val);
        }

        match node.kind() {
            Kind::NumericLiteral => self.emit_leaf_expr(
                node,
                IrOp::Literal {
                    kind: LiteralKind::Number(node_text(node).unwrap_or_default()),
                    as_const: false,
                },
                next,
                true,
            ),
            Kind::BigIntLiteral => self.emit_leaf_expr(
                node,
                IrOp::Literal {
                    kind: LiteralKind::BigInt(node_text(node).unwrap_or_default()),
                    as_const: false,
                },
                next,
                true,
            ),
            Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral => self.emit_leaf_expr(
                node,
                IrOp::Literal {
                    kind: LiteralKind::String(node_text(node).unwrap_or_default()),
                    as_const: false,
                },
                next,
                true,
            ),
            Kind::RegularExpressionLiteral => self.emit_leaf_expr(
                node,
                IrOp::Literal {
                    kind: LiteralKind::RegExp(node_text(node).unwrap_or_default()),
                    as_const: false,
                },
                next,
                true,
            ),
            Kind::TrueKeyword => self.emit_leaf_expr(
                node,
                IrOp::Literal {
                    kind: LiteralKind::Boolean(true),
                    as_const: false,
                },
                next,
                true,
            ),
            Kind::FalseKeyword => self.emit_leaf_expr(
                node,
                IrOp::Literal {
                    kind: LiteralKind::Boolean(false),
                    as_const: false,
                },
                next,
                true,
            ),
            Kind::NullKeyword => self.emit_leaf_expr(
                node,
                IrOp::Literal {
                    kind: LiteralKind::Null,
                    as_const: false,
                },
                next,
                true,
            ),
            Kind::OmittedExpression => self.emit_leaf_expr(
                node,
                IrOp::Literal {
                    kind: LiteralKind::Undefined,
                    as_const: false,
                },
                next,
                false,
            ),
            Kind::ThisKeyword => self.emit_leaf_expr(node, IrOp::ThisRef, next, true),
            Kind::SuperKeyword => self.emit_leaf_expr(node, IrOp::SuperRef, next, true),
            Kind::MetaProperty => self.emit_leaf_expr(node, IrOp::MetaProp, next, true),
            Kind::Identifier => {
                if let Some(binding) = self.resolve_binding(node) {
                    self.emit_leaf_expr(node, IrOp::ReadLocal { binding }, next, true)
                } else if self.bridge.is_some_and(|b| b.is_global_undefined(node)) {
                    self.emit_leaf_expr(
                        node,
                        IrOp::Literal {
                            kind: LiteralKind::Undefined,
                            as_const: false,
                        },
                        next,
                        true,
                    )
                } else {
                    let sid = self.intern_node(node);
                    self.emit_leaf_expr(node, IrOp::ReadSymbol { ident_syntax: sid }, next, true)
                }
            }
            Kind::PrivateIdentifier => {
                let sid = self.intern_node(node);
                self.emit_leaf_expr(node, IrOp::ReadSymbol { ident_syntax: sid }, next, true)
            }
            Kind::FunctionDeclaration | Kind::FunctionExpression | Kind::ArrowFunction => {
                let sid = self.intern_node(node);
                let captures = self.collect_nested_captures(node);
                self.emit_leaf_expr(
                    node,
                    IrOp::FunctionLiteral {
                        syntax: sid,
                        captures,
                    },
                    next,
                    true,
                )
            }
            Kind::ClassDeclaration | Kind::ClassExpression => self.lower_class_expr(node, next),
            Kind::ParenthesizedExpression => {
                let inner = node.as_parenthesized_expression().expression();
                self.lower_wrapper_expr(node, inner, false, false, next)
            }
            Kind::AsExpression => {
                let as_expr = node.as_as_expression();
                let as_const = is_const_type_reference(as_expr.type_());
                self.lower_wrapper_expr(node, as_expr.expression(), as_const, false, next)
            }
            Kind::TypeAssertionExpression => {
                let ta = node.as_type_assertion();
                let as_const = is_const_type_reference(ta.type_());
                self.lower_wrapper_expr(node, ta.expression(), as_const, false, next)
            }
            Kind::SatisfiesExpression => {
                let sat = node.as_satisfies_expression();
                self.lower_wrapper_expr(node, sat.expression(), false, false, next)
            }
            Kind::NonNullExpression => {
                if is_optional_chain(node) {
                    self.lower_optional_chain_spine(node, next, None)
                } else {
                    let nn = node.as_non_null_expression();
                    self.lower_wrapper_expr(node, nn.expression(), false, true, next)
                }
            }
            Kind::ExpressionWithTypeArguments => {
                let e = node.as_expression_with_type_arguments().expression();
                self.lower_wrapper_expr(node, e, false, false, next)
            }
            Kind::SpreadElement => {
                let inner = node.as_spread_element().expression();
                let iter_val = self.alloc_value();
                let rest_val = self.alloc_value();
                let rest_v = self.new_vertex(
                    Some(node),
                    IrOp::IteratorRest {
                        iterator: iter_val,
                        is_async: false,
                    },
                    Some(rest_val),
                    true,
                );
                self.attach_may_throw(rest_v, node, MayThrowReason::IteratorStep);
                self.add_edge(rest_v, next, ExplicitEdgeKind::Fallthrough);
                self.bind_expr_maps(node, rest_v, rest_val);

                let init_v = self.new_vertex(
                    None,
                    IrOp::IteratorInit {
                        iterable: iter_val,
                        is_async: false,
                    },
                    Some(iter_val),
                    false,
                );
                self.attach_may_throw(init_v, inner, MayThrowReason::IteratorStep);
                self.add_edge(init_v, rest_v, ExplicitEdgeKind::Fallthrough);

                let (inner_entry, inner_val) = self.lower_expr_to(inner, init_v);
                if let Some(vert) = self.vertices.get_mut(init_v.0 as usize)
                    && let IrOp::IteratorInit { iterable, .. } = &mut vert.op
                {
                    *iterable = inner_val;
                }
                (inner_entry, rest_val)
            }
            Kind::SpreadAssignment => {
                let e = node.as_spread_assignment().expression();
                self.lower_wrapper_expr(node, e, false, false, next)
            }
            Kind::PrefixUnaryExpression => self.lower_prefix_unary(node, next),
            Kind::PostfixUnaryExpression => self.lower_postfix_unary(node, next),
            Kind::DeleteExpression => {
                let inner = node.as_delete_expression().expression();
                self.lower_unary_op_expr(node, UnaryOp::Delete, inner, next)
            }
            Kind::TypeOfExpression => {
                let inner = node.as_type_of_expression().expression();
                self.lower_unary_op_expr(node, UnaryOp::TypeOf, inner, next)
            }
            Kind::VoidExpression => {
                let inner = node.as_void_expression().expression();
                self.lower_unary_op_expr(node, UnaryOp::Void, inner, next)
            }
            Kind::ConditionalExpression => self.lower_conditional_expr(node, next),
            Kind::BinaryExpression => self.lower_binary_expr(node, next),
            Kind::PropertyAccessExpression => self.lower_property_access(node, next),
            Kind::ElementAccessExpression => self.lower_element_access(node, next),
            Kind::CallExpression => self.lower_call_expr(node, next),
            Kind::NewExpression => self.lower_new_expr(node, next),
            Kind::TaggedTemplateExpression => self.lower_tagged_template(node, next),
            Kind::TemplateExpression => self.lower_template_expr(node, next),
            Kind::TemplateSpan => {
                let expr = node.as_template_span().expression();
                self.lower_wrapper_expr(node, expr, false, false, next)
            }
            Kind::ComputedPropertyName => {
                if let Some(expr) = node.expression() {
                    self.lower_wrapper_expr(node, expr, false, false, next)
                } else {
                    self.emit_leaf_expr(
                        node,
                        IrOp::Literal {
                            kind: LiteralKind::Undefined,
                            as_const: false,
                        },
                        next,
                        false,
                    )
                }
            }
            Kind::Decorator => self.lower_decorator_expr(node, next),
            Kind::ObjectLiteralExpression => self.lower_object_literal(node, next),
            Kind::ArrayLiteralExpression => self.lower_array_literal(node, next),
            Kind::YieldExpression => {
                let y = node.as_yield_expression();
                let is_delegate = y.asterisk_token().is_some();
                let val = self.alloc_value();
                let here = self.new_vertex(
                    Some(node),
                    IrOp::Yield {
                        value: None,
                        is_delegate,
                    },
                    Some(val),
                    true,
                );
                if is_delegate {
                    self.attach_may_throw(here, node, MayThrowReason::Yield);
                }
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                self.bind_expr_maps(node, here, val);
                if let Some(inner) = y.expression() {
                    if is_delegate {
                        let is_async = self.decl_semantics.is_async;
                        let iter_val = self.alloc_value();
                        let next_val = self.alloc_value();
                        let next_v = self.new_vertex(
                            None,
                            IrOp::IteratorNext {
                                iterator: iter_val,
                                is_async,
                            },
                            Some(next_val),
                            false,
                        );
                        self.attach_may_throw(next_v, node, MayThrowReason::IteratorStep);
                        self.add_edge(next_v, here, ExplicitEdgeKind::Fallthrough);

                        let init_v = self.new_vertex(
                            None,
                            IrOp::IteratorInit {
                                iterable: iter_val,
                                is_async,
                            },
                            Some(iter_val),
                            false,
                        );
                        self.attach_may_throw(init_v, inner, MayThrowReason::IteratorStep);
                        self.add_edge(init_v, next_v, ExplicitEdgeKind::Fallthrough);

                        let (inner_entry, inner_val) = self.lower_expr_to(inner, init_v);
                        if let Some(v) = self.vertices.get_mut(init_v.0 as usize)
                            && let IrOp::IteratorInit { iterable, .. } = &mut v.op
                        {
                            *iterable = inner_val;
                        }
                        if let Some(v) = self.vertices.get_mut(here.0 as usize)
                            && let IrOp::Yield { value, .. } = &mut v.op
                        {
                            *value = Some(next_val);
                        }
                        (inner_entry, val)
                    } else {
                        let (inner_entry, inner_val) = self.lower_expr_to(inner, here);
                        if let Some(v) = self.vertices.get_mut(here.0 as usize)
                            && let IrOp::Yield { value, .. } = &mut v.op
                        {
                            *value = Some(inner_val);
                        }
                        (inner_entry, val)
                    }
                } else {
                    (here, val)
                }
            }
            Kind::AwaitExpression => {
                let val = self.alloc_value();
                let here =
                    self.new_vertex(Some(node), IrOp::Await { promise: val }, Some(val), true);
                self.attach_may_throw(here, node, MayThrowReason::Await);
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                self.bind_expr_maps(node, here, val);
                let inner = node.as_await_expression().expression();
                let (inner_entry, inner_val) = self.lower_expr_to(inner, here);
                if let Some(v) = self.vertices.get_mut(here.0 as usize)
                    && let IrOp::Await { promise } = &mut v.op
                {
                    *promise = inner_val;
                }
                (inner_entry, val)
            }
            Kind::JsxElement => self.lower_jsx_element(node, next),
            Kind::JsxSelfClosingElement => self.lower_jsx_self_closing(node, next),
            Kind::JsxFragment => self.lower_jsx_fragment(node, next),
            Kind::JsxExpression => {
                let jsx_expr = node.as_jsx_expression();
                if let Some(inner) = jsx_expr.expression() {
                    self.lower_wrapper_expr(node, inner, false, false, next)
                } else {
                    self.emit_leaf_expr(
                        node,
                        IrOp::Literal {
                            kind: LiteralKind::Undefined,
                            as_const: false,
                        },
                        next,
                        false,
                    )
                }
            }
            Kind::JsxAttribute => {
                let attr = node.as_jsx_attribute();
                if let Some(init) = attr.initializer() {
                    self.lower_wrapper_expr(node, init, false, false, next)
                } else {
                    self.emit_leaf_expr(
                        node,
                        IrOp::Literal {
                            kind: LiteralKind::Boolean(true),
                            as_const: false,
                        },
                        next,
                        false,
                    )
                }
            }
            Kind::JsxSpreadAttribute => {
                let expr = node.as_jsx_spread_attribute().expression();
                self.lower_wrapper_expr(node, expr, false, false, next)
            }
            Kind::PartiallyEmittedExpression => {
                let inner = node.as_partially_emitted_expression().expression();
                self.lower_wrapper_expr(node, inner, false, false, next)
            }
            Kind::Unknown | Kind::SyntheticExpression | Kind::SyntheticReferenceExpression => {
                // Parser error-recovery placeholder (`Kind::Unknown`) and compiler-internal synthetic
                // expressions emitted during recovery rather than user-written runtime expressions.
                self.lower_children_unknown(
                    node,
                    UnknownReason::UnmodeledConstruct,
                    format!(
                        "Compiler synthetic or error-recovery placeholder {:?} has no direct runtime syntax.",
                        node.kind()
                    ),
                    next,
                )
            }
            // Non-expression token/trivia/clause kinds encountered via generic child traversal
            Kind::EndOfFile
            | Kind::SingleLineCommentTrivia
            | Kind::MultiLineCommentTrivia
            | Kind::NewLineTrivia
            | Kind::WhitespaceTrivia
            | Kind::ConflictMarkerTrivia
            | Kind::NonTextFileMarkerTrivia
            | Kind::JsxText
            | Kind::JsxTextAllWhiteSpaces
            | Kind::TemplateHead
            | Kind::TemplateMiddle
            | Kind::TemplateTail
            | Kind::OpenBraceToken
            | Kind::CloseBraceToken
            | Kind::OpenParenToken
            | Kind::CloseParenToken
            | Kind::OpenBracketToken
            | Kind::CloseBracketToken
            | Kind::DotToken
            | Kind::DotDotDotToken
            | Kind::SemicolonToken
            | Kind::CommaToken
            | Kind::QuestionDotToken
            | Kind::LessThanToken
            | Kind::LessThanSlashToken
            | Kind::GreaterThanToken
            | Kind::LessThanEqualsToken
            | Kind::GreaterThanEqualsToken
            | Kind::EqualsEqualsToken
            | Kind::ExclamationEqualsToken
            | Kind::EqualsEqualsEqualsToken
            | Kind::ExclamationEqualsEqualsToken
            | Kind::EqualsGreaterThanToken
            | Kind::PlusToken
            | Kind::MinusToken
            | Kind::AsteriskToken
            | Kind::AsteriskAsteriskToken
            | Kind::SlashToken
            | Kind::PercentToken
            | Kind::PlusPlusToken
            | Kind::MinusMinusToken
            | Kind::LessThanLessThanToken
            | Kind::GreaterThanGreaterThanToken
            | Kind::GreaterThanGreaterThanGreaterThanToken
            | Kind::AmpersandToken
            | Kind::BarToken
            | Kind::CaretToken
            | Kind::ExclamationToken
            | Kind::TildeToken
            | Kind::AmpersandAmpersandToken
            | Kind::BarBarToken
            | Kind::QuestionToken
            | Kind::ColonToken
            | Kind::AtToken
            | Kind::QuestionQuestionToken
            | Kind::BacktickToken
            | Kind::HashToken
            | Kind::EqualsToken
            | Kind::PlusEqualsToken
            | Kind::MinusEqualsToken
            | Kind::AsteriskEqualsToken
            | Kind::AsteriskAsteriskEqualsToken
            | Kind::SlashEqualsToken
            | Kind::PercentEqualsToken
            | Kind::LessThanLessThanEqualsToken
            | Kind::GreaterThanGreaterThanEqualsToken
            | Kind::GreaterThanGreaterThanGreaterThanEqualsToken
            | Kind::AmpersandEqualsToken
            | Kind::BarEqualsToken
            | Kind::BarBarEqualsToken
            | Kind::AmpersandAmpersandEqualsToken
            | Kind::QuestionQuestionEqualsToken
            | Kind::CaretEqualsToken
            | Kind::JSDocCommentTextToken
            | Kind::BreakKeyword
            | Kind::CaseKeyword
            | Kind::CatchKeyword
            | Kind::ClassKeyword
            | Kind::ConstKeyword
            | Kind::ContinueKeyword
            | Kind::DebuggerKeyword
            | Kind::DefaultKeyword
            | Kind::DeleteKeyword
            | Kind::DoKeyword
            | Kind::ElseKeyword
            | Kind::EnumKeyword
            | Kind::ExportKeyword
            | Kind::ExtendsKeyword
            | Kind::FinallyKeyword
            | Kind::ForKeyword
            | Kind::FunctionKeyword
            | Kind::IfKeyword
            | Kind::ImportKeyword
            | Kind::InKeyword
            | Kind::InstanceOfKeyword
            | Kind::NewKeyword
            | Kind::ReturnKeyword
            | Kind::SwitchKeyword
            | Kind::ThrowKeyword
            | Kind::TryKeyword
            | Kind::TypeOfKeyword
            | Kind::VarKeyword
            | Kind::VoidKeyword
            | Kind::WhileKeyword
            | Kind::WithKeyword
            | Kind::ImplementsKeyword
            | Kind::InterfaceKeyword
            | Kind::LetKeyword
            | Kind::PackageKeyword
            | Kind::PrivateKeyword
            | Kind::ProtectedKeyword
            | Kind::PublicKeyword
            | Kind::StaticKeyword
            | Kind::YieldKeyword
            | Kind::AbstractKeyword
            | Kind::AccessorKeyword
            | Kind::AsKeyword
            | Kind::AssertsKeyword
            | Kind::AssertKeyword
            | Kind::AnyKeyword
            | Kind::AsyncKeyword
            | Kind::AwaitKeyword
            | Kind::BooleanKeyword
            | Kind::ConstructorKeyword
            | Kind::DeclareKeyword
            | Kind::GetKeyword
            | Kind::ImmediateKeyword
            | Kind::InferKeyword
            | Kind::IntrinsicKeyword
            | Kind::IsKeyword
            | Kind::KeyOfKeyword
            | Kind::ModuleKeyword
            | Kind::NamespaceKeyword
            | Kind::NeverKeyword
            | Kind::OutKeyword
            | Kind::ReadonlyKeyword
            | Kind::RequireKeyword
            | Kind::NumberKeyword
            | Kind::ObjectKeyword
            | Kind::SatisfiesKeyword
            | Kind::SetKeyword
            | Kind::StringKeyword
            | Kind::SymbolKeyword
            | Kind::TypeKeyword
            | Kind::UndefinedKeyword
            | Kind::UniqueKeyword
            | Kind::UnknownKeyword
            | Kind::UsingKeyword
            | Kind::FromKeyword
            | Kind::GlobalKeyword
            | Kind::BigIntKeyword
            | Kind::OverrideKeyword
            | Kind::OfKeyword
            | Kind::DeferKeyword
            | Kind::SourceKeyword
            | Kind::QualifiedName
            | Kind::TypeParameter
            | Kind::Parameter
            | Kind::PropertySignature
            | Kind::PropertyDeclaration
            | Kind::MethodSignature
            | Kind::MethodDeclaration
            | Kind::ClassStaticBlockDeclaration
            | Kind::Constructor
            | Kind::GetAccessor
            | Kind::SetAccessor
            | Kind::CallSignature
            | Kind::ConstructSignature
            | Kind::IndexSignature
            | Kind::TypePredicate
            | Kind::TypeReference
            | Kind::FunctionType
            | Kind::ConstructorType
            | Kind::TypeQuery
            | Kind::TypeLiteral
            | Kind::ArrayType
            | Kind::TupleType
            | Kind::OptionalType
            | Kind::RestType
            | Kind::UnionType
            | Kind::IntersectionType
            | Kind::ConditionalType
            | Kind::InferType
            | Kind::ParenthesizedType
            | Kind::ThisType
            | Kind::TypeOperator
            | Kind::IndexedAccessType
            | Kind::MappedType
            | Kind::LiteralType
            | Kind::NamedTupleMember
            | Kind::TemplateLiteralType
            | Kind::TemplateLiteralTypeSpan
            | Kind::ImportType
            | Kind::ObjectBindingPattern
            | Kind::ArrayBindingPattern
            | Kind::BindingElement
            | Kind::SemicolonClassElement
            | Kind::Block
            | Kind::EmptyStatement
            | Kind::VariableStatement
            | Kind::ExpressionStatement
            | Kind::IfStatement
            | Kind::DoStatement
            | Kind::WhileStatement
            | Kind::ForStatement
            | Kind::ForInStatement
            | Kind::ForOfStatement
            | Kind::ContinueStatement
            | Kind::BreakStatement
            | Kind::ReturnStatement
            | Kind::WithStatement
            | Kind::SwitchStatement
            | Kind::LabeledStatement
            | Kind::ThrowStatement
            | Kind::TryStatement
            | Kind::DebuggerStatement
            | Kind::VariableDeclaration
            | Kind::VariableDeclarationList
            | Kind::InterfaceDeclaration
            | Kind::TypeAliasDeclaration
            | Kind::EnumDeclaration
            | Kind::ModuleDeclaration
            | Kind::ModuleBlock
            | Kind::CaseBlock
            | Kind::NamespaceExportDeclaration
            | Kind::ImportEqualsDeclaration
            | Kind::ImportDeclaration
            | Kind::ImportClause
            | Kind::NamespaceImport
            | Kind::NamedImports
            | Kind::ImportSpecifier
            | Kind::ExportAssignment
            | Kind::ExportDeclaration
            | Kind::NamedExports
            | Kind::NamespaceExport
            | Kind::ExportSpecifier
            | Kind::MissingDeclaration
            | Kind::ExternalModuleReference
            | Kind::CaseClause
            | Kind::DefaultClause
            | Kind::HeritageClause
            | Kind::CatchClause
            | Kind::ImportAttributes
            | Kind::ImportAttribute
            | Kind::PropertyAssignment
            | Kind::ShorthandPropertyAssignment
            | Kind::EnumMember
            | Kind::SourceFile
            | Kind::JSDocTypeExpression
            | Kind::JSDocNameReference
            | Kind::JSDocAllType
            | Kind::JSDocNullableType
            | Kind::JSDocNonNullableType
            | Kind::JSDocOptionalType
            | Kind::JSDocVariadicType
            | Kind::JSDoc
            | Kind::JSDocText
            | Kind::JSDocTypeLiteral
            | Kind::JSDocSignature
            | Kind::JSDocLink
            | Kind::JSDocLinkCode
            | Kind::JSDocLinkPlain
            | Kind::JSDocUnknownTag
            | Kind::JSDocAugmentsTag
            | Kind::JSDocImplementsTag
            | Kind::JSDocDeprecatedTag
            | Kind::JSDocPublicTag
            | Kind::JSDocPrivateTag
            | Kind::JSDocProtectedTag
            | Kind::JSDocReadonlyTag
            | Kind::JSDocOverrideTag
            | Kind::JSDocCallbackTag
            | Kind::JSDocOverloadTag
            | Kind::JSDocParameterTag
            | Kind::JSDocReturnTag
            | Kind::JSDocThisTag
            | Kind::JSDocTypeTag
            | Kind::JSDocTemplateTag
            | Kind::JSDocTypedefTag
            | Kind::JSDocSeeTag
            | Kind::JSDocPropertyTag
            | Kind::JSDocThrowsTag
            | Kind::JSDocSatisfiesTag
            | Kind::JSDocImportTag
            | Kind::SyntaxList
            | Kind::JSTypeAliasDeclaration
            | Kind::JSImportDeclaration
            | Kind::NotEmittedStatement
            | Kind::NotEmittedTypeElement
            | Kind::JsxAttributes
            | Kind::JsxOpeningElement
            | Kind::JsxClosingElement
            | Kind::JsxOpeningFragment
            | Kind::JsxClosingFragment
            | Kind::JsxNamespacedName
            | Kind::Count => self.lower_children_passthrough(node, next),
        }
    }

    fn emit_leaf_expr(
        &mut self,
        node: P<Node>,
        op: IrOp,
        next: VertexId,
        with_semantics: bool,
    ) -> (VertexId, ValueId) {
        let val = self.alloc_value();
        let v = self.new_vertex(Some(node), op, Some(val), with_semantics);
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, val);
        (v, val)
    }

    fn lower_wrapper_expr(
        &mut self,
        node: P<Node>,
        inner: P<Node>,
        as_const: bool,
        non_null_assert: bool,
        next: VertexId,
    ) -> (VertexId, ValueId) {
        let out_val = self.alloc_value();
        let v = self.new_vertex(
            Some(node),
            IrOp::ValueForward {
                source: out_val,
                as_const,
                non_null_assert,
            },
            Some(out_val),
            true,
        );
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);
        let (entry, inner_val) = self.lower_expr_to(inner, v);
        if let Some(vert) = self.vertices.get_mut(v.0 as usize)
            && let IrOp::ValueForward { source, .. } = &mut vert.op
        {
            *source = inner_val;
        }
        if as_const {
            let unwrapped = unwrap_expr_wrappers(inner);
            let inner_lsid = self.intern_node(unwrapped).local_id();
            if let Some(&inner_vid) = self.expr_to_vertex.get(&inner_lsid)
                && let Some(inner_vert) = self.vertices.get_mut(inner_vid.0 as usize)
                && let IrOp::ObjectAlloc { as_const: flag, .. }
                | IrOp::ArrayAlloc { as_const: flag, .. }
                | IrOp::Literal { as_const: flag, .. } = &mut inner_vert.op
            {
                *flag = true;
            }
        }
        (entry, out_val)
    }

    fn lower_unary_op_expr(
        &mut self,
        node: P<Node>,
        op: UnaryOp,
        operand: P<Node>,
        next: VertexId,
    ) -> (VertexId, ValueId) {
        let out_val = self.alloc_value();
        let v = self.new_vertex(
            Some(node),
            IrOp::Unary {
                op,
                operand: out_val,
            },
            Some(out_val),
            true,
        );
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);
        let (entry, operand_val) = self.lower_expr_to(operand, v);
        if let Some(vert) = self.vertices.get_mut(v.0 as usize)
            && let IrOp::Unary { operand, .. } = &mut vert.op
        {
            *operand = operand_val;
        }
        (entry, out_val)
    }

    fn lower_prefix_unary(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let p = node.as_prefix_unary_expression();
        let op_kind = p.operator();
        if matches!(op_kind, Kind::PlusPlusToken | Kind::MinusMinusToken) {
            let assign_op = if op_kind == Kind::PlusPlusToken {
                AssignOp::Increment
            } else {
                AssignOp::Decrement
            };
            return self.lower_inc_dec_expr(node, p.operand(), assign_op, next);
        }
        let op = if op_kind == Kind::ExclamationToken {
            UnaryOp::LogicalNot
        } else if op_kind == Kind::TildeToken {
            UnaryOp::BitwiseNot
        } else if op_kind == Kind::MinusToken {
            UnaryOp::Minus
        } else {
            UnaryOp::Plus
        };
        self.lower_unary_op_expr(node, op, p.operand(), next)
    }

    fn lower_postfix_unary(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let p = node.as_postfix_unary_expression();
        let assign_op = if p.operator() == Kind::MinusMinusToken {
            AssignOp::Decrement
        } else {
            AssignOp::Increment
        };
        self.lower_inc_dec_expr(node, p.operand(), assign_op, next)
    }

    fn lower_inc_dec_expr(
        &mut self,
        node: P<Node>,
        operand: P<Node>,
        assign_op: AssignOp,
        next: VertexId,
    ) -> (VertexId, ValueId) {
        let mut wrappers = Vec::new();
        let mut cur = operand;
        while matches!(
            cur.kind(),
            Kind::ParenthesizedExpression
                | Kind::NonNullExpression
                | Kind::AsExpression
                | Kind::TypeAssertionExpression
                | Kind::SatisfiesExpression
        ) {
            wrappers.push(cur);
            cur = unwrap_one_wrapper(cur);
        }
        if matches!(
            cur.kind(),
            Kind::Identifier | Kind::PropertyAccessExpression | Kind::ElementAccessExpression
        ) {
            let out_val = self.alloc_value();
            let step_val = self.alloc_value();
            let (target_base, assign_v) =
                self.prepare_assign_target(node, cur, assign_op, out_val, next);
            let lit_v = self.new_vertex(
                None,
                IrOp::Literal {
                    kind: LiteralKind::Number("1".to_string()),
                    as_const: false,
                },
                Some(step_val),
                false,
            );
            self.add_edge(lit_v, assign_v, ExplicitEdgeKind::Fallthrough);
            self.patch_assign_value(assign_v, step_val);
            for w in wrappers {
                self.bind_expr_maps(w, assign_v, out_val);
            }
            let entry = self.lower_assign_target_prefix(target_base, lit_v);
            return (entry, out_val);
        }
        let unary_op = if assign_op == AssignOp::Increment {
            UnaryOp::Increment
        } else {
            UnaryOp::Decrement
        };
        self.lower_unary_op_expr(node, unary_op, operand, next)
    }

    fn lower_conditional_expr(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let cond_expr = node.as_conditional_expression();
        let out_val = self.alloc_value();
        let join = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
        self.add_edge(join, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, join, out_val);

        let (true_entry, true_val) = self.lower_expr_to(cond_expr.when_true(), join);
        let (false_entry, false_val) = self.lower_expr_to(cond_expr.when_false(), join);
        self.record_value_sources(out_val, &[true_val, false_val]);
        let entry =
            self.build_conditional_branches(node, cond_expr.condition(), true_entry, false_entry);
        // Ensure the ConditionalExpression itself resolves to `entry` in `syntax_to_vertex` / `expr_to_vertex`
        self.bind_expr_maps(node, entry, out_val);
        (entry, out_val)
    }

    fn lower_binary_expr(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let bin = node.as_binary_expression();
        let op_kind = bin.operator_token().kind();
        if matches!(
            op_kind,
            Kind::AmpersandAmpersandEqualsToken
                | Kind::BarBarEqualsToken
                | Kind::QuestionQuestionEqualsToken
        ) {
            return self.lower_logical_assign_expr(node, bin.left(), bin.right(), op_kind, next);
        }
        if matches!(op_kind, Kind::AmpersandAmpersandToken | Kind::BarBarToken) {
            let branch_kind = if op_kind == Kind::AmpersandAmpersandToken {
                IrBranchKind::LogicalAnd
            } else {
                IrBranchKind::LogicalOr
            };
            return self.lower_short_circuit_value_expr(
                branch_kind,
                node,
                bin.left(),
                bin.right(),
                next,
            );
        }
        if op_kind == Kind::QuestionQuestionToken {
            return self.lower_short_circuit_value_expr(
                IrBranchKind::NullishCoalescing,
                node,
                bin.left(),
                bin.right(),
                next,
            );
        }
        if let Some(assign_op) = classify_assign_op(op_kind) {
            return self.lower_assignment_expr(node, bin.left(), bin.right(), assign_op, next);
        }
        let bin_op = classify_binary_op(op_kind);
        let out_val = self.alloc_value();
        let v = self.new_vertex(
            Some(node),
            IrOp::Binary {
                op: bin_op,
                left: out_val,
                right: out_val,
            },
            Some(out_val),
            true,
        );
        if matches!(bin_op, BinaryOp::In | BinaryOp::InstanceOf) {
            self.attach_may_throw(v, node, MayThrowReason::DynamicOperator);
        }
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);
        let (right_entry, right_val) = self.lower_expr_to(bin.right(), v);
        let (left_entry, left_val) = self.lower_expr_to(bin.left(), right_entry);
        if let Some(vert) = self.vertices.get_mut(v.0 as usize)
            && let IrOp::Binary { left, right, .. } = &mut vert.op
        {
            *left = left_val;
            *right = right_val;
        }
        (left_entry, out_val)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Preserves short-circuit branches and only writes LHS on the non-short-circuit path."
    )]
    fn lower_logical_assign_expr(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        right: P<Node>,
        op_kind: Kind,
        next: VertexId,
    ) -> (VertexId, ValueId) {
        let branch_kind = if op_kind == Kind::AmpersandAmpersandEqualsToken {
            IrBranchKind::LogicalAnd
        } else if op_kind == Kind::BarBarEqualsToken {
            IrBranchKind::LogicalOr
        } else {
            IrBranchKind::NullishCoalescing
        };
        let out_val = self.alloc_value();
        let join = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
        self.add_edge(join, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, join, out_val);

        // Unwrap paren/non-null wrappers on LHS while preserving expr_to_vertex mappings.
        let mut wrappers = Vec::new();
        let mut target = left;
        while matches!(
            target.kind(),
            Kind::ParenthesizedExpression
                | Kind::NonNullExpression
                | Kind::AsExpression
                | Kind::TypeAssertionExpression
                | Kind::SatisfiesExpression
        ) {
            wrappers.push(target);
            target = unwrap_one_wrapper(target);
        }

        let expr_loc = self.loc(node);
        let left_loc = self.loc(left);
        let short_branch = self.push_branch(
            branch_kind,
            IrBranchDirection::ShortCircuit,
            left,
            left_loc.clone(),
            expr_loc.clone(),
            None,
        );
        let cont_branch = self.push_branch(
            branch_kind,
            IrBranchDirection::Continue,
            left,
            left_loc,
            expr_loc,
            None,
        );

        let gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(gate, join, ExplicitEdgeKind::Branch(short_branch));

        if target.kind() == Kind::PropertyAccessExpression {
            let pa = target.as_property_access_expression();
            let base = pa.expression();
            let name_node = pa.name();
            let key_name = node_text(name_node).unwrap_or_default();
            let key = if name_node.kind() == Kind::PrivateIdentifier {
                IrPropKey::PrivateIdent(key_name)
            } else {
                IrPropKey::Named(key_name)
            };
            let is_super = base.kind() == Kind::SuperKeyword;
            let base_val_slot = self.alloc_value();

            let setter_kind = self
                .bridge
                .is_some_and(|b| b.has_set_accessor(target))
                .then_some(ImplicitAccessorKind::Setter);
            let write_v = self.new_vertex(
                Some(node),
                IrOp::PropWrite {
                    object: base_val_slot,
                    key: key.clone(),
                    op: AssignOp::Assign,
                    value: out_val,
                    is_super,
                    implicit_call_kind: setter_kind,
                },
                Some(out_val),
                true,
            );
            self.attach_may_throw(write_v, node, MayThrowReason::PropertyAccess);
            let write_cont = self.maybe_wrap_implicit_setter(
                node,
                setter_kind,
                base_val_slot,
                out_val,
                write_v,
                join,
            );
            self.add_edge(write_v, write_cont, ExplicitEdgeKind::Fallthrough);
            let (right_entry, right_val) = self.lower_expr_to(right, write_v);
            self.patch_assign_value(write_v, right_val);
            self.add_edge(gate, right_entry, ExplicitEdgeKind::Branch(cont_branch));

            let getter_kind = self
                .bridge
                .is_some_and(|b| b.has_get_accessor(target))
                .then_some(ImplicitAccessorKind::Getter);
            let read_val = self.alloc_value();
            let read_v = self.new_vertex(
                Some(target),
                IrOp::PropRead {
                    object: base_val_slot,
                    key,
                    is_super,
                    implicit_call_kind: getter_kind,
                },
                Some(read_val),
                true,
            );
            self.attach_may_throw(read_v, target, MayThrowReason::PropertyAccess);
            let read_cont =
                self.maybe_wrap_implicit_getter(target, getter_kind, base_val_slot, read_val, gate);
            self.add_edge(read_v, read_cont, ExplicitEdgeKind::Fallthrough);
            self.bind_expr_maps(target, read_v, read_val);
            for w in wrappers {
                self.bind_expr_maps(w, read_v, read_val);
            }

            let (base_entry, base_val) = self.lower_expr_to(base, read_v);
            if let Some(vert) = self.vertices.get_mut(read_v.0 as usize)
                && let IrOp::PropRead { object, .. } = &mut vert.op
            {
                *object = base_val;
            }
            if let Some(vert) = self.vertices.get_mut(write_v.0 as usize)
                && let IrOp::PropWrite { object, .. } = &mut vert.op
            {
                *object = base_val;
            }
            if write_cont != join
                && let Some(vert) = self.vertices.get_mut(write_cont.0 as usize)
                && let IrOp::Call {
                    callee,
                    receiver,
                    args,
                    ..
                } = &mut vert.op
            {
                *callee = base_val;
                *receiver = Some(base_val);
                *args = vec![IrCallArg::Positional(right_val)];
            }
            if read_cont != gate
                && let Some(vert) = self.vertices.get_mut(read_cont.0 as usize)
                && let IrOp::Call { receiver, .. } = &mut vert.op
            {
                *receiver = Some(base_val);
            }
            self.record_value_sources(out_val, &[read_val, right_val]);
            self.bind_expr_maps(node, join, out_val);
            return (base_entry, out_val);
        }

        if target.kind() == Kind::ElementAccessExpression {
            let ea = target.as_element_access_expression();
            let base = ea.expression();
            let arg = ea.argument_expression();
            let is_super = base.kind() == Kind::SuperKeyword;
            let static_name = prop_key_text(arg);
            let base_slot = self.alloc_value();
            let key_slot = self.alloc_value();

            let write_v = self.new_vertex(
                Some(node),
                IrOp::PropWrite {
                    object: base_slot,
                    key: IrPropKey::Computed {
                        key: key_slot,
                        static_name: static_name.clone(),
                    },
                    op: AssignOp::Assign,
                    value: out_val,
                    is_super,
                    implicit_call_kind: None,
                },
                Some(out_val),
                true,
            );
            self.attach_may_throw(write_v, node, MayThrowReason::PropertyAccess);
            self.add_edge(write_v, join, ExplicitEdgeKind::Fallthrough);
            let (right_entry, right_val) = self.lower_expr_to(right, write_v);
            self.patch_assign_value(write_v, right_val);
            self.add_edge(gate, right_entry, ExplicitEdgeKind::Branch(cont_branch));

            let read_val = self.alloc_value();
            let read_v = self.new_vertex(
                Some(target),
                IrOp::PropRead {
                    object: base_slot,
                    key: IrPropKey::Computed {
                        key: key_slot,
                        static_name,
                    },
                    is_super,
                    implicit_call_kind: None,
                },
                Some(read_val),
                true,
            );
            self.attach_may_throw(read_v, target, MayThrowReason::PropertyAccess);
            self.add_edge(read_v, gate, ExplicitEdgeKind::Fallthrough);
            self.bind_expr_maps(target, read_v, read_val);
            for w in wrappers {
                self.bind_expr_maps(w, read_v, read_val);
            }

            let (arg_entry, arg_val) = self.lower_expr_to(arg, read_v);
            let (base_entry, base_val) = self.lower_expr_to(base, arg_entry);
            if let Some(vert) = self.vertices.get_mut(read_v.0 as usize)
                && let IrOp::PropRead {
                    object,
                    key: IrPropKey::Computed { key, .. },
                    ..
                } = &mut vert.op
            {
                *object = base_val;
                *key = arg_val;
            }
            if let Some(vert) = self.vertices.get_mut(write_v.0 as usize)
                && let IrOp::PropWrite {
                    object,
                    key: IrPropKey::Computed { key, .. },
                    ..
                } = &mut vert.op
            {
                *object = base_val;
                *key = arg_val;
            }
            self.bind_expr_maps(node, join, out_val);
            return (base_entry, out_val);
        }

        let write_v =
            self.build_assign_target_vertex(node, target, AssignOp::Assign, out_val, join);
        let (right_entry, right_val) = self.lower_expr_to(right, write_v);
        self.patch_assign_value(write_v, right_val);
        self.add_edge(gate, right_entry, ExplicitEdgeKind::Branch(cont_branch));

        let (read_entry, read_val) = self.lower_expr_to(target, gate);
        self.record_value_sources(out_val, &[read_val, right_val]);
        for w in wrappers {
            self.bind_expr_maps(w, read_entry, read_val);
        }
        self.bind_expr_maps(node, join, out_val);
        (read_entry, out_val)
    }

    fn lower_short_circuit_value_expr(
        &mut self,
        branch_kind: IrBranchKind,
        node: P<Node>,
        left: P<Node>,
        right: P<Node>,
        next: VertexId,
    ) -> (VertexId, ValueId) {
        let out_val = self.alloc_value();
        let join = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
        self.add_edge(join, next, ExplicitEdgeKind::Fallthrough);

        let (right_entry, right_val) = self.lower_expr_to(right, join);
        let expr_loc = self.loc(node);
        let left_loc = self.loc(left);
        let short_branch = self.push_branch(
            branch_kind,
            IrBranchDirection::ShortCircuit,
            left,
            left_loc.clone(),
            expr_loc.clone(),
            None,
        );
        let cont_branch = self.push_branch(
            branch_kind,
            IrBranchDirection::Continue,
            left,
            left_loc,
            expr_loc,
            None,
        );

        let gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(gate, join, ExplicitEdgeKind::Branch(short_branch));
        self.add_edge(gate, right_entry, ExplicitEdgeKind::Branch(cont_branch));

        let (left_entry, left_val) = self.lower_expr_to(left, gate);
        self.record_value_sources(out_val, &[left_val, right_val]);
        self.bind_expr_maps(node, join, out_val);
        (left_entry, out_val)
    }

    fn patch_assign_value(&mut self, assign_v: VertexId, rhs_val: ValueId) {
        if let Some(vert) = self.vertices.get_mut(assign_v.0 as usize) {
            if let IrOp::AssignLocal { value, .. } = &mut vert.op {
                *value = rhs_val;
            } else if let IrOp::AssignSymbol { value, .. } = &mut vert.op {
                *value = rhs_val;
            } else if let IrOp::PropWrite { value, .. } = &mut vert.op {
                *value = rhs_val;
            }
        }
    }

    fn lower_assignment_expr(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        right: P<Node>,
        op: AssignOp,
        next: VertexId,
    ) -> (VertexId, ValueId) {
        if op == AssignOp::Assign && is_destructuring_assign_target(left) {
            let out_val = self.alloc_value();
            let rhs_slot = self.alloc_value();
            let dest_entry = self.lower_assign_destructure_or_target(node, left, rhs_slot, next);
            let fwd = self.new_vertex(
                Some(node),
                IrOp::ValueForward {
                    source: rhs_slot,
                    as_const: false,
                    non_null_assert: false,
                },
                Some(rhs_slot),
                false,
            );
            self.add_edge(fwd, dest_entry, ExplicitEdgeKind::Fallthrough);
            self.bind_expr_maps(node, fwd, out_val);
            let (right_entry, right_val) = self.lower_expr_to(right, fwd);
            if let Some(v) = self.vertices.get_mut(fwd.0 as usize)
                && let IrOp::ValueForward { source, .. } = &mut v.op
            {
                *source = right_val;
            }
            return (right_entry, out_val);
        }

        let out_val = self.alloc_value();
        let (target_base, assign_v) = self.prepare_assign_target(node, left, op, out_val, next);
        let (right_entry, right_val) = self.lower_expr_to(right, assign_v);
        self.patch_assign_value(assign_v, right_val);
        let lhs_entry = self.lower_assign_target_prefix(target_base, right_entry);
        (lhs_entry, out_val)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Lowers object, array, and scalar assignment targets with default branches uniformly."
    )]
    fn lower_assign_destructure_or_target(
        &mut self,
        ctx_node: P<Node>,
        lhs: P<Node>,
        rhs_val: ValueId,
        mut cont: VertexId,
    ) -> VertexId {
        let lhs_kind = lhs.kind();
        if lhs_kind == Kind::ParenthesizedExpression {
            let inner = lhs.as_parenthesized_expression().expression();
            let entry = self.lower_assign_destructure_or_target(ctx_node, inner, rhs_val, cont);
            self.bind_expr_maps(lhs, entry, rhs_val);
            entry
        } else if lhs_kind == Kind::ObjectLiteralExpression {
            let obj = lhs.as_object_literal_expression();
            let mut omitted_static_keys = Vec::new();
            let mut has_dynamic_omitted = false;
            for &prop in obj.properties().nodes() {
                let prop_kind = prop.kind();
                if prop_kind == Kind::ShorthandPropertyAssignment {
                    if let Some(k) = prop_key_text(prop.as_shorthand_property_assignment().name()) {
                        omitted_static_keys.push(k);
                    }
                } else if prop_kind == Kind::PropertyAssignment {
                    let name_node = prop.as_property_assignment().name();
                    if name_node.kind() == Kind::ComputedPropertyName {
                        if let Some(k) = prop_key_text(name_node) {
                            omitted_static_keys.push(k);
                        } else {
                            has_dynamic_omitted = true;
                        }
                    } else if let Some(k) = prop_key_text(name_node) {
                        omitted_static_keys.push(k);
                    }
                }
            }

            for &prop in obj.properties().nodes().iter().rev() {
                let prop_kind = prop.kind();
                if prop_kind == Kind::SpreadAssignment {
                    let sa = prop.as_spread_assignment();
                    let rest_val = self.alloc_value();
                    let target_entry = self.lower_assign_destructure_or_target(
                        prop,
                        sa.expression(),
                        rest_val,
                        cont,
                    );
                    let rest_v = self.new_vertex(
                        Some(prop),
                        IrOp::ObjectRest {
                            source: rhs_val,
                            omitted_static_keys: omitted_static_keys.clone(),
                            has_dynamic_omitted,
                        },
                        Some(rest_val),
                        true,
                    );
                    self.attach_may_throw(rest_v, prop, MayThrowReason::PropertyAccess);
                    self.add_edge(rest_v, target_entry, ExplicitEdgeKind::Fallthrough);
                    self.bind_expr_maps(prop, rest_v, rest_val);
                    cont = rest_v;
                } else if prop_kind == Kind::ShorthandPropertyAssignment {
                    let sp = prop.as_shorthand_property_assignment();
                    let name_node = sp.name();
                    let key_name = node_text(name_node).unwrap_or_default();
                    let raw_prop_val = self.alloc_value();
                    let next_after_read = if let Some(def_expr) = sp.object_assignment_initializer()
                    {
                        let eff_slot = self.alloc_value();
                        let target_entry = self
                            .lower_assign_destructure_or_target(prop, name_node, eff_slot, cont);
                        let eff_fwd = self.new_vertex(
                            None,
                            IrOp::ValueForward {
                                source: eff_slot,
                                as_const: false,
                                non_null_assert: false,
                            },
                            Some(eff_slot),
                            false,
                        );
                        self.add_edge(eff_fwd, target_entry, ExplicitEdgeKind::Fallthrough);
                        let (def_entry, def_out) =
                            self.lower_default_value_branch(prop, raw_prop_val, def_expr, eff_fwd);
                        if let Some(v) = self.vertices.get_mut(eff_fwd.0 as usize)
                            && let IrOp::ValueForward { source, .. } = &mut v.op
                        {
                            *source = def_out;
                        }
                        def_entry
                    } else {
                        self.lower_assign_destructure_or_target(prop, name_node, raw_prop_val, cont)
                    };
                    let read_v = self.new_vertex(
                        Some(prop),
                        IrOp::PropRead {
                            object: rhs_val,
                            key: IrPropKey::Named(key_name),
                            is_super: false,
                            implicit_call_kind: None,
                        },
                        Some(raw_prop_val),
                        true,
                    );
                    self.attach_may_throw(read_v, prop, MayThrowReason::PropertyAccess);
                    self.add_edge(read_v, next_after_read, ExplicitEdgeKind::Fallthrough);
                    cont = read_v;
                } else if prop_kind == Kind::PropertyAssignment {
                    let pa = prop.as_property_assignment();
                    let name_node = pa.name();
                    let init = pa.initializer();
                    let raw_prop_val = self.alloc_value();
                    let (target_node, default_opt) = if init.kind() == Kind::BinaryExpression
                        && init.as_binary_expression().operator_token().kind() == Kind::EqualsToken
                    {
                        let b = init.as_binary_expression();
                        (b.left(), Some((init, b.right())))
                    } else {
                        (init, None)
                    };

                    let next_after_read = if let Some((bin_node, def_expr)) = default_opt {
                        let eff_slot = self.alloc_value();
                        let target_entry = self.lower_assign_destructure_or_target(
                            prop,
                            target_node,
                            eff_slot,
                            cont,
                        );
                        let eff_fwd = self.new_vertex(
                            None,
                            IrOp::ValueForward {
                                source: eff_slot,
                                as_const: false,
                                non_null_assert: false,
                            },
                            Some(eff_slot),
                            false,
                        );
                        self.add_edge(eff_fwd, target_entry, ExplicitEdgeKind::Fallthrough);
                        let (def_entry, def_out) = self.lower_default_value_branch(
                            bin_node,
                            raw_prop_val,
                            def_expr,
                            eff_fwd,
                        );
                        if let Some(v) = self.vertices.get_mut(eff_fwd.0 as usize)
                            && let IrOp::ValueForward { source, .. } = &mut v.op
                        {
                            *source = def_out;
                        }
                        self.bind_expr_maps(bin_node, def_entry, def_out);
                        def_entry
                    } else {
                        self.lower_assign_destructure_or_target(
                            prop,
                            target_node,
                            raw_prop_val,
                            cont,
                        )
                    };

                    if name_node.kind() == Kind::ComputedPropertyName {
                        let static_name = prop_key_text(name_node);
                        let key_slot = self.alloc_value();
                        let read_v = self.new_vertex(
                            Some(prop),
                            IrOp::PropRead {
                                object: rhs_val,
                                key: IrPropKey::Computed {
                                    key: key_slot,
                                    static_name,
                                },
                                is_super: false,
                                implicit_call_kind: None,
                            },
                            Some(raw_prop_val),
                            true,
                        );
                        self.attach_may_throw(read_v, prop, MayThrowReason::PropertyAccess);
                        self.add_edge(read_v, next_after_read, ExplicitEdgeKind::Fallthrough);
                        if let Some(key_expr) = name_node.expression() {
                            let (key_entry, key_val) = self.lower_expr_to(key_expr, read_v);
                            if let Some(v) = self.vertices.get_mut(read_v.0 as usize)
                                && let IrOp::PropRead {
                                    key: IrPropKey::Computed { key, .. },
                                    ..
                                } = &mut v.op
                            {
                                *key = key_val;
                            }
                            cont = key_entry;
                        } else {
                            cont = read_v;
                        }
                    } else {
                        let key_name = prop_key_text(name_node).unwrap_or_default();
                        let read_v = self.new_vertex(
                            Some(prop),
                            IrOp::PropRead {
                                object: rhs_val,
                                key: IrPropKey::Named(key_name),
                                is_super: false,
                                implicit_call_kind: None,
                            },
                            Some(raw_prop_val),
                            true,
                        );
                        self.attach_may_throw(read_v, prop, MayThrowReason::PropertyAccess);
                        self.add_edge(read_v, next_after_read, ExplicitEdgeKind::Fallthrough);
                        cont = read_v;
                    }
                }
            }
            self.bind_expr_maps(lhs, cont, rhs_val);
            cont
        } else if lhs_kind == Kind::ArrayLiteralExpression {
            let arr = lhs.as_array_literal_expression();
            let iter_val = self.alloc_value();
            let close_v = self.new_vertex(
                Some(lhs),
                IrOp::IteratorClose {
                    iterator: iter_val,
                    is_async: false,
                },
                None,
                false,
            );
            self.attach_may_throw(close_v, lhs, MayThrowReason::IteratorStep);
            self.add_edge(close_v, cont, ExplicitEdgeKind::Fallthrough);
            cont = close_v;

            for &elem in arr.elements().nodes().iter().rev() {
                if elem.kind() == Kind::OmittedExpression {
                    let skip_val = self.alloc_value();
                    let next_v = self.new_vertex(
                        Some(elem),
                        IrOp::IteratorNext {
                            iterator: iter_val,
                            is_async: false,
                        },
                        Some(skip_val),
                        false,
                    );
                    self.attach_may_throw(next_v, elem, MayThrowReason::IteratorStep);
                    self.add_edge(next_v, cont, ExplicitEdgeKind::Fallthrough);
                    self.bind_expr_maps(elem, next_v, skip_val);
                    cont = next_v;
                    continue;
                }
                if elem.kind() == Kind::SpreadElement {
                    let se = elem.as_spread_element();
                    let rest_val = self.alloc_value();
                    let target_entry = self.lower_assign_destructure_or_target(
                        elem,
                        se.expression(),
                        rest_val,
                        cont,
                    );
                    let rest_v = self.new_vertex(
                        Some(elem),
                        IrOp::IteratorRest {
                            iterator: iter_val,
                            is_async: false,
                        },
                        Some(rest_val),
                        true,
                    );
                    self.attach_may_throw(rest_v, elem, MayThrowReason::IteratorStep);
                    self.add_edge(rest_v, target_entry, ExplicitEdgeKind::Fallthrough);
                    self.bind_expr_maps(elem, rest_v, rest_val);
                    cont = rest_v;
                    continue;
                }

                let raw_elem_val = self.alloc_value();
                let (target_node, default_opt) = if elem.kind() == Kind::BinaryExpression
                    && elem.as_binary_expression().operator_token().kind() == Kind::EqualsToken
                {
                    let b = elem.as_binary_expression();
                    (b.left(), Some((elem, b.right())))
                } else {
                    (elem, None)
                };
                let after_next = if let Some((bin_node, def_expr)) = default_opt {
                    let eff_slot = self.alloc_value();
                    let target_entry = self.lower_assign_destructure_or_target(
                        bin_node,
                        target_node,
                        eff_slot,
                        cont,
                    );
                    let eff_fwd = self.new_vertex(
                        None,
                        IrOp::ValueForward {
                            source: eff_slot,
                            as_const: false,
                            non_null_assert: false,
                        },
                        Some(eff_slot),
                        false,
                    );
                    self.add_edge(eff_fwd, target_entry, ExplicitEdgeKind::Fallthrough);
                    let (def_entry, def_out) =
                        self.lower_default_value_branch(bin_node, raw_elem_val, def_expr, eff_fwd);
                    if let Some(v) = self.vertices.get_mut(eff_fwd.0 as usize)
                        && let IrOp::ValueForward { source, .. } = &mut v.op
                    {
                        *source = def_out;
                    }
                    self.bind_expr_maps(bin_node, def_entry, def_out);
                    def_entry
                } else {
                    self.lower_assign_destructure_or_target(elem, target_node, raw_elem_val, cont)
                };
                let next_v = self.new_vertex(
                    Some(elem),
                    IrOp::IteratorNext {
                        iterator: iter_val,
                        is_async: false,
                    },
                    Some(raw_elem_val),
                    true,
                );
                self.attach_may_throw(next_v, elem, MayThrowReason::IteratorStep);
                self.add_edge(next_v, after_next, ExplicitEdgeKind::Fallthrough);
                cont = next_v;
            }

            let init_v = self.new_vertex(
                Some(lhs),
                IrOp::IteratorInit {
                    iterable: rhs_val,
                    is_async: false,
                },
                Some(iter_val),
                true,
            );
            self.attach_may_throw(init_v, lhs, MayThrowReason::IteratorStep);
            self.add_edge(init_v, cont, ExplicitEdgeKind::Fallthrough);
            self.bind_expr_maps(lhs, init_v, iter_val);
            init_v
        } else {
            let (target_base, assign_v) =
                self.prepare_assign_target(ctx_node, lhs, AssignOp::Assign, rhs_val, cont);
            self.lower_assign_target_prefix(target_base, assign_v)
        }
    }

    fn maybe_wrap_implicit_getter(
        &mut self,
        node: P<Node>,
        kind: Option<ImplicitAccessorKind>,
        receiver_val: ValueId,
        raw_prop_val: ValueId,
        next: VertexId,
    ) -> VertexId {
        if kind != Some(ImplicitAccessorKind::Getter) {
            return next;
        }
        let call_val = self.alloc_value();
        let call_v = self.new_vertex(
            Some(node),
            IrOp::Call {
                call_kind: IrCallKind::ImplicitGetter,
                callee: raw_prop_val,
                receiver: Some(receiver_val),
                args: Vec::new(),
            },
            Some(call_val),
            true,
        );
        self.attach_may_throw(call_v, node, MayThrowReason::Call);
        self.add_edge(call_v, next, ExplicitEdgeKind::Fallthrough);
        call_v
    }

    fn maybe_wrap_implicit_setter(
        &mut self,
        node: P<Node>,
        kind: Option<ImplicitAccessorKind>,
        receiver_val: ValueId,
        rhs_val: ValueId,
        prop_write_v: VertexId,
        next: VertexId,
    ) -> VertexId {
        if kind != Some(ImplicitAccessorKind::Setter) {
            return next;
        }
        let call_val = self.alloc_value();
        let call_v = self.new_vertex(
            Some(node),
            IrOp::Call {
                call_kind: IrCallKind::ImplicitSetter,
                callee: receiver_val,
                receiver: Some(receiver_val),
                args: vec![IrCallArg::Positional(rhs_val)],
            },
            Some(call_val),
            true,
        );
        self.attach_may_throw(call_v, node, MayThrowReason::Call);
        self.add_edge(call_v, next, ExplicitEdgeKind::Fallthrough);
        let _ = prop_write_v;
        call_v
    }

    fn prepare_assign_target(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        op: AssignOp,
        out_val: ValueId,
        next: VertexId,
    ) -> (AssignTargetPrefix, VertexId) {
        if left.kind() == Kind::PropertyAccessExpression {
            let pa = left.as_property_access_expression();
            let base = pa.expression();
            let name_node = pa.name();
            let key_name = node_text(name_node).unwrap_or_default();
            let key = if name_node.kind() == Kind::PrivateIdentifier {
                IrPropKey::PrivateIdent(key_name)
            } else {
                IrPropKey::Named(key_name)
            };
            let is_super = base.kind() == Kind::SuperKeyword;
            let implicit_call_kind = self
                .bridge
                .is_some_and(|b| b.has_set_accessor(left))
                .then_some(ImplicitAccessorKind::Setter);
            let setter_call_v = if implicit_call_kind == Some(ImplicitAccessorKind::Setter) {
                let cv = self.new_vertex(
                    Some(node),
                    IrOp::Call {
                        call_kind: IrCallKind::ImplicitSetter,
                        callee: out_val,
                        receiver: Some(out_val),
                        args: vec![IrCallArg::Positional(out_val)],
                    },
                    Some(out_val),
                    true,
                );
                self.attach_may_throw(cv, node, MayThrowReason::Call);
                self.add_edge(cv, next, ExplicitEdgeKind::Fallthrough);
                Some(cv)
            } else {
                None
            };
            let write_next = setter_call_v.unwrap_or(next);
            let assign_v = self.new_vertex(
                Some(node),
                IrOp::PropWrite {
                    object: out_val,
                    key,
                    op,
                    value: out_val,
                    is_super,
                    implicit_call_kind,
                },
                Some(out_val),
                true,
            );
            self.attach_may_throw(assign_v, node, MayThrowReason::PropertyAccess);
            self.add_edge(assign_v, write_next, ExplicitEdgeKind::Fallthrough);
            let map_v = setter_call_v.unwrap_or(assign_v);
            self.bind_expr_maps(node, map_v, out_val);
            self.bind_expr_maps(left, map_v, out_val);
            return (
                AssignTargetPrefix::Prop {
                    base,
                    assign_v,
                    setter_call_v,
                },
                assign_v,
            );
        }

        if left.kind() == Kind::ElementAccessExpression {
            let ea = left.as_element_access_expression();
            let base = ea.expression();
            let arg = ea.argument_expression();
            let is_super = base.kind() == Kind::SuperKeyword;
            let static_name = prop_key_text(arg);
            let assign_v = self.new_vertex(
                Some(node),
                IrOp::PropWrite {
                    object: out_val,
                    key: IrPropKey::Computed {
                        key: out_val,
                        static_name,
                    },
                    op,
                    value: out_val,
                    is_super,
                    implicit_call_kind: None,
                },
                Some(out_val),
                true,
            );
            self.attach_may_throw(assign_v, node, MayThrowReason::PropertyAccess);
            self.add_edge(assign_v, next, ExplicitEdgeKind::Fallthrough);
            self.bind_expr_maps(node, assign_v, out_val);
            self.bind_expr_maps(left, assign_v, out_val);
            return (
                AssignTargetPrefix::Element {
                    base,
                    arg,
                    assign_v,
                },
                assign_v,
            );
        }

        let assign_v = self.build_assign_target_vertex(node, left, op, out_val, next);
        (AssignTargetPrefix::Other(left), assign_v)
    }

    fn lower_assign_target_prefix(
        &mut self,
        prefix: AssignTargetPrefix,
        cont: VertexId,
    ) -> VertexId {
        match prefix {
            AssignTargetPrefix::Prop {
                base,
                assign_v,
                setter_call_v,
            } => {
                let (base_entry, base_val) = self.lower_expr_to(base, cont);
                let rhs_val_seen = if let Some(vert) = self.vertices.get_mut(assign_v.0 as usize)
                    && let IrOp::PropWrite { object, value, .. } = &mut vert.op
                {
                    *object = base_val;
                    Some(*value)
                } else {
                    None
                };
                if let Some(cv) = setter_call_v
                    && let Some(vert) = self.vertices.get_mut(cv.0 as usize)
                    && let IrOp::Call {
                        callee,
                        receiver,
                        args,
                        ..
                    } = &mut vert.op
                {
                    *callee = base_val;
                    *receiver = Some(base_val);
                    if let Some(rv) = rhs_val_seen {
                        *args = vec![IrCallArg::Positional(rv)];
                    }
                }
                base_entry
            }
            AssignTargetPrefix::Element {
                base,
                arg,
                assign_v,
            } => {
                let (arg_entry, arg_val) = self.lower_expr_to(arg, cont);
                let (base_entry, base_val) = self.lower_expr_to(base, arg_entry);
                if let Some(vert) = self.vertices.get_mut(assign_v.0 as usize)
                    && let IrOp::PropWrite {
                        object,
                        key: IrPropKey::Computed { key, .. },
                        ..
                    } = &mut vert.op
                {
                    *object = base_val;
                    *key = arg_val;
                }
                base_entry
            }
            AssignTargetPrefix::Other(left) => {
                let (lhs_entry, _) = self.lower_expr_to(left, cont);
                lhs_entry
            }
        }
    }

    fn build_assign_target_vertex(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        op: AssignOp,
        out_val: ValueId,
        next: VertexId,
    ) -> VertexId {
        let assign_op = if left.kind() == Kind::Identifier {
            self.resolve_binding(left).map_or_else(
                || {
                    let sid = self.intern_node(left);
                    IrOp::AssignSymbol {
                        target_syntax: sid,
                        op,
                        value: out_val,
                        is_init: false,
                    }
                },
                |binding| IrOp::AssignLocal {
                    target: binding,
                    op,
                    value: out_val,
                    is_init: false,
                },
            )
        } else {
            let sid = self.intern_node(left);
            IrOp::AssignSymbol {
                target_syntax: sid,
                op,
                value: out_val,
                is_init: false,
            }
        };
        let v = self.new_vertex(Some(node), assign_op, Some(out_val), true);
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);
        v
    }

    fn lower_property_access(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        if is_optional_chain(node) {
            return self.lower_optional_chain_spine(node, next, None);
        }
        self.lower_property_access_with_chain(node, next, None)
    }

    fn lower_property_access_with_chain(
        &mut self,
        node: P<Node>,
        next: VertexId,
        active_chain_join: Option<VertexId>,
    ) -> (VertexId, ValueId) {
        let pa = node.as_property_access_expression();
        let base = pa.expression();
        let name_node = pa.name();
        let key_name = node_text(name_node).unwrap_or_default();
        let key = if name_node.kind() == Kind::PrivateIdentifier {
            IrPropKey::PrivateIdent(key_name)
        } else {
            IrPropKey::Named(key_name)
        };
        let is_super = base.kind() == Kind::SuperKeyword;
        let out_val = self.alloc_value();
        let implicit_call_kind = self
            .bridge
            .is_some_and(|b| b.has_get_accessor(node))
            .then_some(ImplicitAccessorKind::Getter);

        let build_read = |this: &mut Self, after_read: VertexId| -> (VertexId, VertexId, ValueId) {
            let base_slot = this.alloc_value();
            let raw_val = this.alloc_value();
            let getter_call_v = if implicit_call_kind == Some(ImplicitAccessorKind::Getter) {
                let cv = this.new_vertex(
                    Some(node),
                    IrOp::Call {
                        call_kind: IrCallKind::ImplicitGetter,
                        callee: raw_val,
                        receiver: Some(base_slot),
                        args: Vec::new(),
                    },
                    Some(out_val),
                    true,
                );
                this.attach_may_throw(cv, node, MayThrowReason::Call);
                this.add_edge(cv, after_read, ExplicitEdgeKind::Fallthrough);
                Some(cv)
            } else {
                None
            };
            let read_next = getter_call_v.unwrap_or(after_read);
            let read_val = if getter_call_v.is_some() {
                raw_val
            } else {
                out_val
            };
            let read_v = this.new_vertex(
                Some(node),
                IrOp::PropRead {
                    object: base_slot,
                    key,
                    is_super,
                    implicit_call_kind,
                },
                Some(read_val),
                true,
            );
            this.attach_may_throw(read_v, node, MayThrowReason::PropertyAccess);
            this.add_edge(read_v, read_next, ExplicitEdgeKind::Fallthrough);
            let map_v = getter_call_v.unwrap_or(read_v);
            this.bind_expr_maps(node, map_v, out_val);
            (read_v, map_v, base_slot)
        };

        if let Some(tail_join) = active_chain_join {
            if pa.question_dot_token().is_some() {
                return self.lower_optional_guard(
                    node,
                    base,
                    out_val,
                    tail_join,
                    |this, base_val| {
                        let (read_v, _, _) = build_read(this, next);
                        this.patch_prop_read_object(read_v, base_val);
                        read_v
                    },
                );
            }
            let (read_v, _, _) = build_read(self, next);
            let (base_entry, base_val) = self.lower_chain_base(base, read_v, tail_join);
            self.patch_prop_read_object(read_v, base_val);
            return (base_entry, out_val);
        }

        let (read_v, _, _) = build_read(self, next);
        let (base_entry, base_val) = self.lower_expr_to(base, read_v);
        self.patch_prop_read_object(read_v, base_val);
        (base_entry, out_val)
    }

    fn patch_prop_read_object(&mut self, read_v: VertexId, base_val: ValueId) {
        let mut getter_next = None;
        if let Some(vert) = self.vertices.get_mut(read_v.0 as usize)
            && let IrOp::PropRead {
                object,
                implicit_call_kind,
                ..
            } = &mut vert.op
        {
            *object = base_val;
            if *implicit_call_kind == Some(ImplicitAccessorKind::Getter) {
                getter_next = vert.explicit_edges.first().map(|e| e.target);
            }
        }
        if let Some(cv) = getter_next
            && let Some(vert) = self.vertices.get_mut(cv.0 as usize)
            && let IrOp::Call { receiver, .. } = &mut vert.op
        {
            *receiver = Some(base_val);
        }
    }

    fn lower_element_access(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        if is_optional_chain(node) {
            return self.lower_optional_chain_spine(node, next, None);
        }
        self.lower_element_access_with_chain(node, next, None)
    }

    fn lower_element_access_with_chain(
        &mut self,
        node: P<Node>,
        next: VertexId,
        active_chain_join: Option<VertexId>,
    ) -> (VertexId, ValueId) {
        let ea = node.as_element_access_expression();
        let base = ea.expression();
        let arg = ea.argument_expression();
        let is_super = base.kind() == Kind::SuperKeyword;
        let out_val = self.alloc_value();
        let static_name = prop_key_text(arg);

        let read_v = self.new_vertex(
            Some(node),
            IrOp::PropRead {
                object: out_val,
                key: IrPropKey::Computed {
                    key: out_val,
                    static_name,
                },
                is_super,
                implicit_call_kind: None,
            },
            Some(out_val),
            true,
        );
        self.attach_may_throw(read_v, node, MayThrowReason::PropertyAccess);
        self.add_edge(read_v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, read_v, out_val);

        let (arg_entry, arg_val) = self.lower_expr_to(arg, read_v);
        if let Some(vert) = self.vertices.get_mut(read_v.0 as usize)
            && let IrOp::PropRead {
                key: IrPropKey::Computed { key, .. },
                ..
            } = &mut vert.op
        {
            *key = arg_val;
        }

        if let Some(tail_join) = active_chain_join {
            if ea.question_dot_token().is_some() {
                return self.lower_optional_guard(
                    node,
                    base,
                    out_val,
                    tail_join,
                    |this, base_val| {
                        if let Some(vert) = this.vertices.get_mut(read_v.0 as usize)
                            && let IrOp::PropRead { object, .. } = &mut vert.op
                        {
                            *object = base_val;
                        }
                        arg_entry
                    },
                );
            }
            let (base_entry, base_val) = self.lower_chain_base(base, arg_entry, tail_join);
            if let Some(vert) = self.vertices.get_mut(read_v.0 as usize)
                && let IrOp::PropRead { object, .. } = &mut vert.op
            {
                *object = base_val;
            }
            return (base_entry, out_val);
        }

        let (base_entry, base_val) = self.lower_expr_to(base, arg_entry);
        if let Some(vert) = self.vertices.get_mut(read_v.0 as usize)
            && let IrOp::PropRead { object, .. } = &mut vert.op
        {
            *object = base_val;
        }
        (base_entry, out_val)
    }

    fn lower_call_expr(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        if is_optional_chain(node) {
            return self.lower_optional_chain_spine(node, next, None);
        }
        self.lower_call_expr_with_chain(node, next, None)
    }

    fn lower_call_expr_with_chain(
        &mut self,
        node: P<Node>,
        next: VertexId,
        active_chain_join: Option<VertexId>,
    ) -> (VertexId, ValueId) {
        let call = node.as_call_expression();
        let callee_node = call.expression();
        let args_nodes = call.arguments().nodes();
        let out_val = self.alloc_value();
        let call_kind = if callee_node.kind() == Kind::SuperKeyword {
            IrCallKind::SuperCall
        } else {
            IrCallKind::Explicit
        };

        let call_after = if call_kind == IrCallKind::SuperCall
            && let Some(class_node) = self.active_ctor_class.take()
        {
            let init_entry = self.lower_class_instance_init(class_node, Some(self.root_node), next);
            self.active_ctor_class = Some(class_node);
            init_entry
        } else {
            next
        };

        let call_v = self.new_vertex(
            Some(node),
            IrOp::Call {
                call_kind,
                callee: out_val,
                receiver: None,
                args: Vec::new(),
            },
            Some(out_val),
            true,
        );
        self.attach_may_throw(call_v, node, MayThrowReason::Call);
        self.add_edge(call_v, call_after, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, call_v, out_val);

        let (args_entry, arg_vals) = self.lower_call_args(args_nodes, call_v);
        if let Some(vert) = self.vertices.get_mut(call_v.0 as usize)
            && let IrOp::Call { args, .. } = &mut vert.op
        {
            *args = arg_vals;
        }

        if let Some(tail_join) = active_chain_join {
            if call.question_dot_token().is_some() {
                return self.lower_optional_guard(
                    node,
                    callee_node,
                    out_val,
                    tail_join,
                    |this, callee_val| {
                        if let Some(vert) = this.vertices.get_mut(call_v.0 as usize)
                            && let IrOp::Call { callee, .. } = &mut vert.op
                        {
                            *callee = callee_val;
                        }
                        args_entry
                    },
                );
            }
            let (callee_entry, callee_val) =
                self.lower_chain_base(callee_node, args_entry, tail_join);
            let receiver_val = self.find_member_receiver_value(callee_node);
            if let Some(vert) = self.vertices.get_mut(call_v.0 as usize)
                && let IrOp::Call {
                    callee, receiver, ..
                } = &mut vert.op
            {
                *callee = callee_val;
                *receiver = receiver_val;
            }
            return (callee_entry, out_val);
        }

        let (callee_entry, callee_val) = self.lower_expr_to(callee_node, args_entry);
        let receiver_val = self.find_member_receiver_value(callee_node);
        if let Some(vert) = self.vertices.get_mut(call_v.0 as usize)
            && let IrOp::Call {
                callee, receiver, ..
            } = &mut vert.op
        {
            *callee = callee_val;
            *receiver = receiver_val;
        }
        (callee_entry, out_val)
    }

    fn find_member_receiver_value(&self, callee_node: P<Node>) -> Option<ValueId> {
        let unwrapped = unwrap_expr_wrappers(callee_node);
        let unwrapped_kind = unwrapped.kind();
        let base = if unwrapped_kind == Kind::PropertyAccessExpression {
            unwrapped.as_property_access_expression().expression()
        } else if unwrapped_kind == Kind::ElementAccessExpression {
            unwrapped.as_element_access_expression().expression()
        } else {
            return None;
        };
        let lsid = self.node_to_syntax.get(&base).map(|sid| sid.local_id())?;
        self.expr_to_value.get(&lsid).copied()
    }

    fn lower_optional_chain_spine(
        &mut self,
        node: P<Node>,
        next: VertexId,
        existing_join: Option<VertexId>,
    ) -> (VertexId, ValueId) {
        let out_val = self.alloc_value();
        let (tail_join, owns_join) = existing_join.map_or_else(
            || {
                let join = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
                self.add_edge(join, next, ExplicitEdgeKind::Fallthrough);
                (join, true)
            },
            |j| (j, false),
        );
        let cont_next = if owns_join { tail_join } else { next };

        let node_kind = node.kind();
        let (entry, val) = if node_kind == Kind::PropertyAccessExpression {
            self.lower_property_access_with_chain(node, cont_next, Some(tail_join))
        } else if node_kind == Kind::ElementAccessExpression {
            self.lower_element_access_with_chain(node, cont_next, Some(tail_join))
        } else if node_kind == Kind::CallExpression {
            self.lower_call_expr_with_chain(node, cont_next, Some(tail_join))
        } else if node_kind == Kind::NonNullExpression {
            let inner = node.as_non_null_expression().expression();
            let v = self.new_vertex(
                Some(node),
                IrOp::ValueForward {
                    source: out_val,
                    as_const: false,
                    non_null_assert: true,
                },
                Some(out_val),
                true,
            );
            self.add_edge(v, cont_next, ExplicitEdgeKind::Fallthrough);
            let (inner_entry, inner_val) = self.lower_chain_base(inner, v, tail_join);
            if let Some(vert) = self.vertices.get_mut(v.0 as usize)
                && let IrOp::ValueForward { source, .. } = &mut vert.op
            {
                *source = inner_val;
            }
            self.bind_expr_maps(node, v, out_val);
            (inner_entry, out_val)
        } else {
            self.lower_expr_to(node, cont_next)
        };
        if owns_join {
            self.record_value_sources(out_val, &[val]);
            self.bind_expr_maps(node, tail_join, val);
        }
        (entry, val)
    }

    fn lower_chain_base(
        &mut self,
        base: P<Node>,
        next: VertexId,
        tail_join: VertexId,
    ) -> (VertexId, ValueId) {
        if is_optional_chain(base) {
            self.lower_optional_chain_spine(base, next, Some(tail_join))
        } else {
            self.lower_expr_to(base, next)
        }
    }

    fn lower_optional_guard<F>(
        &mut self,
        node: P<Node>,
        guarded_expr: P<Node>,
        out_val: ValueId,
        tail_join: VertexId,
        build_cont: F,
    ) -> (VertexId, ValueId)
    where
        F: FnOnce(&mut Self, ValueId) -> VertexId,
    {
        let expr_loc = self.loc(node);
        let base_loc = self.loc(guarded_expr);
        let short_branch = self.push_branch(
            IrBranchKind::OptionalChain,
            IrBranchDirection::ShortCircuit,
            guarded_expr,
            base_loc.clone(),
            expr_loc.clone(),
            None,
        );
        let cont_branch = self.push_branch(
            IrBranchKind::OptionalChain,
            IrBranchDirection::Continue,
            guarded_expr,
            base_loc,
            expr_loc,
            None,
        );

        let gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        let (base_entry, base_val) = self.lower_expr_to(guarded_expr, gate);
        let cont_entry = build_cont(self, base_val);

        self.add_edge(gate, tail_join, ExplicitEdgeKind::Branch(short_branch));
        self.add_edge(gate, cont_entry, ExplicitEdgeKind::Branch(cont_branch));
        (base_entry, out_val)
    }

    fn lower_new_expr(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let new_expr = node.as_new_expression();
        let callee_node = new_expr.expression();
        let args_nodes = new_expr
            .arguments()
            .as_deref()
            .map_or(&[][..], tsrs_ast::NodeList::nodes);
        let out_val = self.alloc_value();
        let new_v = self.new_vertex(
            Some(node),
            IrOp::Construct {
                callee: out_val,
                args: Vec::new(),
                is_new_target: false,
            },
            Some(out_val),
            true,
        );
        self.attach_may_throw(new_v, node, MayThrowReason::Construct);
        self.add_edge(new_v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, new_v, out_val);
        let (args_entry, arg_vals) = self.lower_call_args(args_nodes, new_v);
        let (callee_entry, callee_val) = self.lower_expr_to(callee_node, args_entry);
        if let Some(vert) = self.vertices.get_mut(new_v.0 as usize)
            && let IrOp::Construct { callee, args, .. } = &mut vert.op
        {
            *callee = callee_val;
            *args = arg_vals;
        }
        (callee_entry, out_val)
    }

    fn lower_tagged_template(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let tt = node.as_tagged_template_expression();
        let out_val = self.alloc_value();
        let call_v = self.new_vertex(
            Some(node),
            IrOp::Call {
                call_kind: IrCallKind::TaggedTemplate,
                callee: out_val,
                receiver: None,
                args: Vec::new(),
            },
            Some(out_val),
            true,
        );
        self.attach_may_throw(call_v, node, MayThrowReason::Call);
        self.add_edge(call_v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, call_v, out_val);
        let (tmpl_entry, tmpl_val) = self.lower_expr_to(tt.template(), call_v);
        let (tag_entry, tag_val) = self.lower_expr_to(tt.tag(), tmpl_entry);
        if let Some(vert) = self.vertices.get_mut(call_v.0 as usize)
            && let IrOp::Call { callee, args, .. } = &mut vert.op
        {
            *callee = tag_val;
            *args = vec![IrCallArg::Positional(tmpl_val)];
        }
        (tag_entry, out_val)
    }

    fn lower_call_args(
        &mut self,
        args: &[P<Node>],
        mut cont: VertexId,
    ) -> (VertexId, Vec<IrCallArg>) {
        let mut out = Vec::with_capacity(args.len());
        for &arg in args.iter().rev() {
            let is_spread = arg.kind() == Kind::SpreadElement;
            let (entry, val) = self.lower_expr_to(arg, cont);
            cont = entry;
            out.push(if is_spread {
                IrCallArg::Spread(val)
            } else {
                IrCallArg::Positional(val)
            });
        }
        out.reverse();
        (cont, out)
    }

    fn lower_template_expr(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let tmpl = node.as_template_expression();
        let out_val = self.alloc_value();
        let v = self.new_vertex(
            Some(node),
            IrOp::TemplateString { spans: Vec::new() },
            Some(out_val),
            true,
        );
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);

        let mut cont = v;
        let mut span_vals = Vec::new();
        for &span in tmpl.template_spans().nodes().iter().rev() {
            let expr = span.as_template_span().expression();
            let (entry, val) = self.lower_expr_to(expr, cont);
            cont = entry;
            span_vals.push(val);
        }
        span_vals.reverse();
        if let Some(vert) = self.vertices.get_mut(v.0 as usize)
            && let IrOp::TemplateString { spans } = &mut vert.op
        {
            *spans = span_vals;
        }
        (cont, out_val)
    }

    fn lower_object_literal(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let obj = node.as_object_literal_expression();
        let out_val = self.alloc_value();
        let v = self.new_vertex(
            Some(node),
            IrOp::ObjectAlloc {
                props: Vec::new(),
                as_const: false,
            },
            Some(out_val),
            true,
        );
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);

        let mut cont = v;
        let mut lowered_props = Vec::new();
        for &prop in obj.properties().nodes().iter().rev() {
            let prop_kind = prop.kind();
            if prop_kind == Kind::PropertyAssignment {
                let pa = prop.as_property_assignment();
                let name_node = pa.name();
                let (val_entry, val_id) = self.lower_expr_to(pa.initializer(), cont);
                cont = val_entry;
                let key = if name_node.kind() == Kind::ComputedPropertyName {
                    let static_name = prop_key_text(name_node);
                    let key_val = if let Some(expr) = name_node.expression() {
                        let (ke, kv) = self.lower_expr_to(expr, cont);
                        cont = ke;
                        kv
                    } else {
                        self.alloc_value()
                    };
                    IrPropKey::Computed {
                        key: key_val,
                        static_name,
                    }
                } else {
                    IrPropKey::Named(prop_key_text(name_node).unwrap_or_default())
                };
                self.record_syntax_vertex(prop, cont);
                lowered_props.push(IrObjectProp::KeyValue { key, value: val_id });
            } else if prop_kind == Kind::ShorthandPropertyAssignment {
                let sp = prop.as_shorthand_property_assignment();
                let name_node = sp.name();
                let after_name = sp.object_assignment_initializer().map_or(cont, |err_init| {
                    let (init_entry, _) = self.lower_expr_to(err_init, cont);
                    init_entry
                });
                let (entry, val_id) = self.lower_expr_to(name_node, after_name);
                cont = entry;
                self.record_syntax_vertex(prop, cont);
                lowered_props.push(IrObjectProp::KeyValue {
                    key: IrPropKey::Named(node_text(name_node).unwrap_or_default()),
                    value: val_id,
                });
            } else if prop_kind == Kind::SpreadAssignment {
                let sa = prop.as_spread_assignment();
                let (entry, val_id) = self.lower_expr_to(sa.expression(), cont);
                cont = entry;
                self.record_syntax_vertex(prop, cont);
                lowered_props.push(IrObjectProp::Spread(val_id));
            } else if matches!(
                prop_kind,
                Kind::MethodDeclaration | Kind::GetAccessor | Kind::SetAccessor
            ) {
                let sid = self.intern_node(prop);
                let captures = self.collect_nested_captures(prop);
                let fn_val = self.alloc_value();
                let fv = self.new_vertex(
                    Some(prop),
                    IrOp::FunctionLiteral {
                        syntax: sid,
                        captures,
                    },
                    Some(fn_val),
                    true,
                );
                self.add_edge(fv, cont, ExplicitEdgeKind::Fallthrough);
                cont = fv;
                let key = prop.name().and_then(prop_key_text).unwrap_or_default();
                lowered_props.push(IrObjectProp::KeyValue {
                    key: IrPropKey::Named(key),
                    value: fn_val,
                });
            } else {
                let (entry, _) = self.lower_expr_to(prop, cont);
                cont = entry;
            }
        }
        lowered_props.reverse();
        if let Some(vert) = self.vertices.get_mut(v.0 as usize)
            && let IrOp::ObjectAlloc { props, .. } = &mut vert.op
        {
            *props = lowered_props;
        }
        (cont, out_val)
    }

    fn lower_array_literal(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let arr = node.as_array_literal_expression();
        let out_val = self.alloc_value();
        let v = self.new_vertex(
            Some(node),
            IrOp::ArrayAlloc {
                elems: Vec::new(),
                as_const: false,
            },
            Some(out_val),
            true,
        );
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);

        let mut cont = v;
        let mut lowered_elems = Vec::new();
        for &elem in arr.elements().nodes().iter().rev() {
            let elem_kind = elem.kind();
            if elem_kind == Kind::OmittedExpression {
                let (entry, _) = self.lower_expr_to(elem, cont);
                cont = entry;
                lowered_elems.push(IrArrayElem::Elision);
            } else if elem_kind == Kind::SpreadElement {
                let (entry, val) = self.lower_expr_to(elem, cont);
                cont = entry;
                lowered_elems.push(IrArrayElem::Spread(val));
            } else {
                let (entry, val) = self.lower_expr_to(elem, cont);
                cont = entry;
                lowered_elems.push(IrArrayElem::Value(val));
            }
        }
        lowered_elems.reverse();
        if let Some(vert) = self.vertices.get_mut(v.0 as usize)
            && let IrOp::ArrayAlloc { elems, .. } = &mut vert.op
        {
            *elems = lowered_elems;
        }
        (cont, out_val)
    }

    fn lower_decorator_expr(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let dec = node.as_decorator();
        let out_val = self.alloc_value();
        let call_v = self.new_vertex(
            Some(node),
            IrOp::Call {
                call_kind: IrCallKind::DecoratorApply,
                callee: out_val,
                receiver: None,
                args: Vec::new(),
            },
            Some(out_val),
            true,
        );
        self.attach_may_throw(call_v, node, MayThrowReason::Call);
        self.add_edge(call_v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, call_v, out_val);
        let (expr_entry, expr_val) = self.lower_expr_to(dec.expression(), call_v);
        if let Some(v) = self.vertices.get_mut(call_v.0 as usize)
            && let IrOp::Call { callee, .. } = &mut v.op
        {
            *callee = expr_val;
        }
        (expr_entry, out_val)
    }

    fn lower_class_stmt(&mut self, node: P<Node>, next: VertexId) -> VertexId {
        let (entry, _) = self.lower_class_expr(node, next);
        entry
    }

    fn lower_class_expr(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let sid = self.intern_node(node);
        let out_val = self.alloc_value();
        let cls_v = self.new_vertex(
            Some(node),
            IrOp::ClassLiteral { syntax: sid },
            Some(out_val),
            true,
        );
        self.add_edge(cls_v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, cls_v, out_val);
        let mut cont = cls_v;

        // Evaluate heritage `extends` expression at class definition time (for ClassExpression/ClassDeclaration).
        let heritage_clauses = if node.kind() == Kind::ClassDeclaration {
            node.as_class_declaration().heritage_clauses()
        } else {
            node.as_class_expression().heritage_clauses()
        };
        if let Some(clauses) = heritage_clauses {
            for &clause in clauses.nodes().iter().rev() {
                let hc = clause.as_heritage_clause();
                if hc.token() == Kind::ExtendsKeyword {
                    for &ty_expr in hc.types().nodes().iter().rev() {
                        let base_expr = if ty_expr.kind() == Kind::ExpressionWithTypeArguments {
                            ty_expr.as_expression_with_type_arguments().expression()
                        } else {
                            ty_expr
                        };
                        let (entry, _) = self.lower_expr_to(base_expr, cont);
                        cont = entry;
                    }
                }
            }
        }

        // Evaluate class and member decorators in source order at class definition time.
        for &dec in node.decorators().iter().rev() {
            let (entry, _) = self.lower_decorator_expr(dec, cont);
            cont = entry;
        }
        (cont, out_val)
    }

    fn lower_class_instance_init(
        &mut self,
        class_node: P<Node>,
        ctor_node: Option<P<Node>>,
        mut cont: VertexId,
    ) -> VertexId {
        // Instance fields execute in declaration order after parameter properties.
        for &member in class_node.members().iter().rev() {
            if member.kind() == Kind::PropertyDeclaration {
                let sem = extract_decl_semantics(member);
                if !sem.is_static {
                    cont = self.lower_class_field_write(member, None, cont);
                }
            }
        }

        // Constructor parameter properties (`public`/`private`/`protected`/`readonly`) run right after `super(...)`.
        if let Some(ctor) = ctor_node {
            for &param in ctor.parameters().iter().rev() {
                if has_parameter_property_modifier(param)
                    && let Some(name_node) = param.name()
                    && name_node.kind() == Kind::Identifier
                {
                    let prop_name = node_text(name_node).unwrap_or_default();
                    let this_val = self.alloc_value();
                    let param_val = self.alloc_value();
                    let write_val = self.alloc_value();
                    let read_op = self.resolve_binding(name_node).map_or_else(
                        || IrOp::ReadSymbol {
                            ident_syntax: self.intern_node(param),
                        },
                        |binding| IrOp::ReadLocal { binding },
                    );
                    let this_v = self.new_vertex(None, IrOp::ThisRef, Some(this_val), false);
                    let read_v = self.new_vertex(None, read_op, Some(param_val), false);
                    let write_v = self.new_vertex(
                        Some(param),
                        IrOp::PropWrite {
                            object: this_val,
                            key: IrPropKey::Named(prop_name),
                            op: AssignOp::Assign,
                            value: param_val,
                            is_super: false,
                            implicit_call_kind: None,
                        },
                        Some(write_val),
                        true,
                    );
                    self.attach_may_throw(write_v, param, MayThrowReason::PropertyAccess);
                    self.add_edge(this_v, read_v, ExplicitEdgeKind::Fallthrough);
                    self.add_edge(read_v, write_v, ExplicitEdgeKind::Fallthrough);
                    self.add_edge(write_v, cont, ExplicitEdgeKind::Fallthrough);
                    cont = this_v;
                }
            }
        }
        cont
    }

    fn lower_class_field_write(
        &mut self,
        prop_decl: P<Node>,
        static_class: Option<P<Node>>,
        cont: VertexId,
    ) -> VertexId {
        let prop = prop_decl.as_property_declaration();
        let name_node = prop.name();
        let Some(init) = prop.initializer() else {
            if name_node.kind() == Kind::ComputedPropertyName
                && let Some(key_expr) = name_node.expression()
            {
                let (entry, _) = self.lower_expr_to(key_expr, cont);
                return entry;
            }
            return cont;
        };

        let obj_val = self.alloc_value();
        let obj_v = if let Some(cls) = static_class {
            let sid = self.intern_node(cls);
            self.new_vertex(
                None,
                IrOp::ReadSymbol { ident_syntax: sid },
                Some(obj_val),
                false,
            )
        } else {
            self.new_vertex(None, IrOp::ThisRef, Some(obj_val), false)
        };

        let write_val = self.alloc_value();
        let mut key = if name_node.kind() == Kind::PrivateIdentifier {
            IrPropKey::PrivateIdent(node_text(name_node).unwrap_or_default())
        } else if name_node.kind() == Kind::ComputedPropertyName {
            IrPropKey::Computed {
                key: write_val,
                static_name: prop_key_text(name_node),
            }
        } else {
            IrPropKey::Named(prop_key_text(name_node).unwrap_or_default())
        };

        let key_slot = write_val;
        let _ = key_slot;
        let write_v = self.new_vertex(
            Some(prop_decl),
            IrOp::PropWrite {
                object: obj_val,
                key: key.clone(),
                op: AssignOp::Assign,
                value: write_val,
                is_super: false,
                implicit_call_kind: None,
            },
            Some(write_val),
            true,
        );
        self.attach_may_throw(write_v, prop_decl, MayThrowReason::PropertyAccess);
        self.add_edge(write_v, cont, ExplicitEdgeKind::Fallthrough);

        let (init_entry, init_val) = self.lower_expr_to(init, write_v);
        let mut after_obj = init_entry;
        if name_node.kind() == Kind::ComputedPropertyName
            && let Some(key_expr) = name_node.expression()
        {
            let (ke, kv) = self.lower_expr_to(key_expr, after_obj);
            if let IrPropKey::Computed { key: k, .. } = &mut key {
                *k = kv;
            }
            after_obj = ke;
        }
        if let Some(vert) = self.vertices.get_mut(write_v.0 as usize)
            && let IrOp::PropWrite {
                key: slot_key,
                value,
                ..
            } = &mut vert.op
        {
            *slot_key = key;
            *value = init_val;
        }
        self.add_edge(obj_v, after_obj, ExplicitEdgeKind::Fallthrough);
        obj_v
    }

    fn lower_jsx_element(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let jsx = node.as_jsx_element();
        let opening = jsx.opening_element();
        let open = opening.as_jsx_opening_element();
        let closing = jsx.closing_element();
        let out_val = self.alloc_value();
        let v = self.new_vertex(
            Some(node),
            IrOp::JsxElement {
                tag_name: None,
                tag_expr: None,
                props: Vec::new(),
                children: Vec::new(),
            },
            Some(out_val),
            true,
        );
        self.attach_may_throw(v, node, MayThrowReason::Call);
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);
        self.record_syntax_vertex(opening, v);
        self.record_syntax_vertex(closing, v);
        if closing.kind() == Kind::JsxClosingElement {
            self.bind_jsx_closing_tag_maps(closing.as_jsx_closing_element().tag_name(), v, out_val);
        }

        let (children_entry, child_vals) = self.lower_jsx_children(jsx.children().nodes(), v);
        let (props_entry, props, tag_name, tag_expr) =
            self.lower_jsx_opening_parts(open.tag_name(), open.attributes(), children_entry);
        if let Some(vert) = self.vertices.get_mut(v.0 as usize)
            && let IrOp::JsxElement {
                tag_name: tn,
                tag_expr: te,
                props: ps,
                children: cs,
            } = &mut vert.op
        {
            *tn = tag_name;
            *te = tag_expr;
            *ps = props;
            *cs = child_vals;
        }
        (props_entry, out_val)
    }

    fn bind_jsx_closing_tag_maps(&mut self, tag_node: P<Node>, vid: VertexId, val: ValueId) {
        let tk = tag_node.kind();
        if tk == Kind::PropertyAccessExpression {
            self.bind_expr_maps(tag_node, vid, val);
            let base = tag_node.as_property_access_expression().expression();
            self.bind_jsx_closing_tag_maps(base, vid, val);
        } else if matches!(
            tk,
            Kind::ThisKeyword | Kind::SuperKeyword | Kind::Identifier
        ) {
            self.bind_expr_maps(tag_node, vid, val);
        } else {
            self.record_syntax_vertex(tag_node, vid);
        }
    }

    fn lower_jsx_self_closing(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let jsx = node.as_jsx_self_closing_element();
        let out_val = self.alloc_value();
        let v = self.new_vertex(
            Some(node),
            IrOp::JsxElement {
                tag_name: None,
                tag_expr: None,
                props: Vec::new(),
                children: Vec::new(),
            },
            Some(out_val),
            true,
        );
        self.attach_may_throw(v, node, MayThrowReason::Call);
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);

        let (entry, props, tag_name, tag_expr) =
            self.lower_jsx_opening_parts(jsx.tag_name(), jsx.attributes(), v);
        if let Some(vert) = self.vertices.get_mut(v.0 as usize)
            && let IrOp::JsxElement {
                tag_name: tn,
                tag_expr: te,
                props: ps,
                ..
            } = &mut vert.op
        {
            *tn = tag_name;
            *te = tag_expr;
            *ps = props;
        }
        (entry, out_val)
    }

    fn lower_jsx_fragment(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let frag = node.as_jsx_fragment();
        let out_val = self.alloc_value();
        let v = self.new_vertex(
            Some(node),
            IrOp::JsxElement {
                tag_name: None,
                tag_expr: None,
                props: Vec::new(),
                children: Vec::new(),
            },
            Some(out_val),
            true,
        );
        self.add_edge(v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, v, out_val);
        self.record_syntax_vertex(frag.opening_fragment(), v);
        self.record_syntax_vertex(frag.closing_fragment(), v);

        let (entry, child_vals) = self.lower_jsx_children(frag.children().nodes(), v);
        if let Some(vert) = self.vertices.get_mut(v.0 as usize)
            && let IrOp::JsxElement { children, .. } = &mut vert.op
        {
            *children = child_vals;
        }
        (entry, out_val)
    }

    fn lower_jsx_children(
        &mut self,
        children: &[P<Node>],
        mut cont: VertexId,
    ) -> (VertexId, Vec<ValueId>) {
        let mut vals = Vec::new();
        for &child in children.iter().rev() {
            if matches!(child.kind(), Kind::JsxText | Kind::JsxTextAllWhiteSpaces) {
                self.record_syntax_vertex(child, cont);
            } else {
                let (entry, val) = self.lower_expr_to(child, cont);
                cont = entry;
                vals.push(val);
            }
        }
        vals.reverse();
        (cont, vals)
    }

    fn lower_jsx_opening_parts(
        &mut self,
        tag_node: P<Node>,
        attrs_node: P<Node>,
        mut cont: VertexId,
    ) -> (VertexId, Vec<IrObjectProp>, Option<String>, Option<ValueId>) {
        let mut props = Vec::new();
        let attrs = attrs_node.as_jsx_attributes().properties().nodes();
        for &attr in attrs.iter().rev() {
            if attr.kind() == Kind::JsxSpreadAttribute {
                let (entry, val) = self.lower_expr_to(attr, cont);
                cont = entry;
                props.push(IrObjectProp::Spread(val));
            } else if attr.kind() == Kind::JsxAttribute {
                let name_text = node_text(attr.as_jsx_attribute().name()).unwrap_or_default();
                let (entry, val) = self.lower_expr_to(attr, cont);
                cont = entry;
                props.push(IrObjectProp::KeyValue {
                    key: IrPropKey::Named(name_text),
                    value: val,
                });
            }
        }
        props.reverse();
        self.record_syntax_vertex(attrs_node, cont);

        let tag_text = node_text(tag_node);
        let tag_kind = tag_node.kind();
        let is_component_ref =
            if matches!(tag_kind, Kind::PropertyAccessExpression | Kind::ThisKeyword) {
                true
            } else if tag_kind == Kind::Identifier {
                tag_text
                    .as_deref()
                    .and_then(|s| s.chars().next())
                    .is_some_and(|ch| ch.is_ascii_uppercase() || ch == '_' || ch == '$')
            } else {
                false
            };
        let tag_expr = if is_component_ref {
            let (entry, val) = self.lower_expr_to(tag_node, cont);
            cont = entry;
            Some(val)
        } else {
            self.record_syntax_vertex(tag_node, cont);
            None
        };
        (cont, props, tag_text, tag_expr)
    }

    fn collect_nested_captures(&mut self, fn_node: P<Node>) -> Vec<CapturedBinding> {
        let mut by_binding: FxHashMap<LocalBindingId, CapturedBinding> = FxHashMap::default();
        self.scan_captures_walk(fn_node, fn_node, &mut by_binding);
        let mut captures: Vec<CapturedBinding> = by_binding.into_values().collect();
        captures.sort_by_key(|c| c.outer_binding.map_or(u32::MAX, |b| b.0));
        captures
    }

    fn scan_captures_walk(
        &mut self,
        node: P<Node>,
        closure_root: P<Node>,
        out: &mut FxHashMap<LocalBindingId, CapturedBinding>,
    ) {
        if is_non_runtime_type_node(node) {
            return;
        }
        if node.kind() == Kind::Identifier
            && is_value_ident_usage(node)
            && let Some(sym) = self.bridge.and_then(|b| b.symbol_for_node_raw(node))
            && let Some(&outer_id) = self.binding_by_raw_symbol.get(&sym)
        {
            let is_write = is_ident_write_site(node);
            if let Some(existing) = out.get_mut(&outer_id) {
                existing.is_written_in_closure |= is_write;
            } else if let Some(binding_info) = self.bindings.get(outer_id.0 as usize) {
                out.insert(
                    outer_id,
                    CapturedBinding {
                        name: binding_info.name.clone(),
                        mutability: binding_info.mutability,
                        outer_binding: Some(outer_id),
                        decl_syntax: binding_info.syntax,
                        is_written_in_closure: is_write,
                        is_written_in_outer_after_capture: false,
                    },
                );
            }
        }
        if node != closure_root && node.kind() == Kind::PropertyAccessExpression {
            self.scan_captures_walk(
                node.as_property_access_expression().expression(),
                closure_root,
                out,
            );
            return;
        }
        node.for_each_child(&mut |child| {
            self.scan_captures_walk(child, closure_root, out);
            false
        });
    }

    fn collect_own_captured_bindings(&mut self, fn_node: P<Node>) -> Vec<CapturedBinding> {
        let fn_start = fn_node.pos().source_offset().unwrap_or(0);
        let fn_end = fn_node.end().source_offset().unwrap_or(fn_start);
        let fn_sf = get_source_file_of_node(fn_node);
        let mut by_decl: FxHashMap<P<Node>, CapturedBinding> = FxHashMap::default();
        self.scan_own_captures_walk(fn_node, fn_sf, fn_start, fn_end, &mut by_decl);
        by_decl.into_values().collect()
    }

    fn scan_own_captures_walk(
        &mut self,
        node: P<Node>,
        fn_sf: Option<P<SourceFile>>,
        fn_start: u32,
        fn_end: u32,
        out: &mut FxHashMap<P<Node>, CapturedBinding>,
    ) {
        if is_non_runtime_type_node(node) {
            return;
        }
        if node.kind() == Kind::Identifier
            && is_value_ident_usage(node)
            && let Some(sym) = self.bridge.and_then(|b| b.symbol_for_node_raw(node))
            && let Some(decl) = sym
                .value_declaration()
                .or_else(|| sym.declarations().first().copied())
            && get_source_file_of_node(decl) == fn_sf
        {
            let decl_start = decl.pos().source_offset().unwrap_or(0);
            let outside_fn = decl_start < fn_start || decl_start >= fn_end;
            let is_local_outer = outside_fn
                && matches!(decl.kind(), Kind::VariableDeclaration | Kind::Parameter)
                && decl.parent().is_some_and(|p| p.kind() != Kind::SourceFile);
            if is_local_outer {
                let is_write = is_ident_write_site(node);
                if let Some(existing) = out.get_mut(&decl) {
                    existing.is_written_in_closure |= is_write;
                } else {
                    let decl_syntax = self.intern_node(decl);
                    let mutability = if decl.kind() == Kind::Parameter {
                        BindingMutability::Parameter
                    } else {
                        classify_decl_mutability(decl)
                    };
                    let name = node_text(node).unwrap_or_default();
                    out.insert(
                        decl,
                        CapturedBinding {
                            name,
                            mutability,
                            outer_binding: None,
                            decl_syntax,
                            is_written_in_closure: is_write,
                            is_written_in_outer_after_capture: false,
                        },
                    );
                }
            }
        }
        if node.kind() == Kind::PropertyAccessExpression {
            self.scan_own_captures_walk(
                node.as_property_access_expression().expression(),
                fn_sf,
                fn_start,
                fn_end,
                out,
            );
            return;
        }
        node.for_each_child(&mut |child| {
            self.scan_own_captures_walk(child, fn_sf, fn_start, fn_end, out);
            false
        });
    }

    fn lower_children_unknown(
        &mut self,
        node: P<Node>,
        reason: UnknownReason,
        message: String,
        next: VertexId,
    ) -> (VertexId, ValueId) {
        let out_val = self.alloc_value();
        let here = self.new_vertex(
            Some(node),
            IrOp::Unknown {
                reason,
                message,
                location: self.loc(node),
                evaluated_children: Vec::new(),
            },
            Some(out_val),
            false,
        );
        self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, here, out_val);

        let mut children = Vec::new();
        node.for_each_child(&mut |child| {
            children.push(child);
            false
        });
        let mut cont = here;
        let mut child_vals = Vec::new();
        for &child in children.iter().rev() {
            if is_function_like_declaration(Some(child))
                || matches!(child.kind(), Kind::ClassDeclaration | Kind::ClassExpression)
                || is_non_runtime_type_node(child)
            {
                continue;
            }
            let (entry, val) = self.lower_expr_to(child, cont);
            cont = entry;
            child_vals.push(val);
        }
        child_vals.reverse();
        if let Some(vert) = self.vertices.get_mut(here.0 as usize)
            && let IrOp::Unknown {
                evaluated_children, ..
            } = &mut vert.op
        {
            *evaluated_children = child_vals;
        }
        (cont, out_val)
    }

    fn lower_children_passthrough(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let out_val = self.alloc_value();
        let here = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), false);
        self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, here, out_val);

        let mut children = Vec::new();
        node.for_each_child(&mut |child| {
            children.push(child);
            false
        });
        let mut cont = here;
        for &child in children.iter().rev() {
            let (entry, _) = self.lower_expr_to(child, cont);
            cont = entry;
        }
        (cont, out_val)
    }
}

fn is_non_runtime_type_node(node: P<Node>) -> bool {
    node.kind() != Kind::ExpressionWithTypeArguments && is_type_node(node)
}

const fn empty_vertex(id: VertexId, op: IrOp) -> CfgVertex {
    CfgVertex {
        id,
        syntax: None,
        local_syntax: None,
        symbol: None,
        ty: None,
        signature: None,
        location: None,
        value: None,
        op,
        explicit_edges: Vec::new(),
        may_throw_edge: None,
        enclosing_finallies: Vec::new(),
    }
}

fn classify_scope_kind(node: P<Node>) -> IrScopeKind {
    let kind = node.kind();
    if matches!(
        kind,
        Kind::FunctionDeclaration | Kind::FunctionExpression | Kind::ArrowFunction
    ) {
        IrScopeKind::Function
    } else if kind == Kind::MethodDeclaration {
        IrScopeKind::Method
    } else if kind == Kind::Constructor {
        IrScopeKind::Constructor
    } else if kind == Kind::GetAccessor {
        IrScopeKind::Getter
    } else if kind == Kind::SetAccessor {
        IrScopeKind::Setter
    } else if kind == Kind::ClassStaticBlockDeclaration {
        IrScopeKind::ClassStaticInit
    } else if kind == Kind::SourceFile {
        IrScopeKind::ModuleTopLevel
    } else {
        IrScopeKind::LocalBlock
    }
}

fn extract_decl_semantics(node: P<Node>) -> DeclarationSemantics {
    let flags = get_combined_modifier_flags(node);
    let is_ecma_private = node
        .name()
        .is_some_and(|name_node| name_node.kind() == Kind::PrivateIdentifier);
    let visibility = if is_ecma_private {
        Some(MemberVisibility::EcmaPrivate)
    } else if flags.intersects(ModifierFlags::Private) {
        Some(MemberVisibility::TsPrivate)
    } else if flags.intersects(ModifierFlags::Protected) {
        Some(MemberVisibility::Protected)
    } else if flags.intersects(ModifierFlags::Public) {
        Some(MemberVisibility::Public)
    } else {
        None
    };

    let kind = node.kind();
    let is_generator = if kind == Kind::FunctionDeclaration {
        node.as_function_declaration().asterisk_token().is_some()
    } else if kind == Kind::FunctionExpression {
        node.as_function_expression().asterisk_token().is_some()
    } else if kind == Kind::MethodDeclaration {
        node.as_method_declaration().asterisk_token().is_some()
    } else if kind == Kind::ArrowFunction {
        node.as_arrow_function().asterisk_token().is_some()
    } else {
        false
    };

    DeclarationSemantics {
        is_exported: flags.intersects(ModifierFlags::Export),
        is_default_export: flags.intersects(ModifierFlags::Default),
        is_readonly: flags.intersects(ModifierFlags::Readonly),
        is_override: flags.intersects(ModifierFlags::Override),
        is_static: flags.intersects(ModifierFlags::Static),
        is_abstract: flags.intersects(ModifierFlags::Abstract),
        is_async: flags.intersects(ModifierFlags::Async),
        is_generator,
        visibility,
    }
}

fn classify_decl_mutability(decl: P<Node>) -> BindingMutability {
    let flags = tsrs_ast::get_combined_node_flags(decl);
    if flags.contains(NodeFlags::AwaitUsing) {
        BindingMutability::AwaitUsing
    } else if flags.intersects(NodeFlags::Using) {
        BindingMutability::Using
    } else if flags.intersects(NodeFlags::Const) {
        BindingMutability::Const
    } else if flags.intersects(NodeFlags::Let) {
        BindingMutability::Let
    } else {
        BindingMutability::Var
    }
}

const fn classify_assign_op(kind: Kind) -> Option<AssignOp> {
    if matches!(kind, Kind::EqualsToken) {
        Some(AssignOp::Assign)
    } else if matches!(kind, Kind::PlusEqualsToken) {
        Some(AssignOp::AddAssign)
    } else if matches!(kind, Kind::MinusEqualsToken) {
        Some(AssignOp::SubAssign)
    } else if matches!(kind, Kind::AsteriskEqualsToken) {
        Some(AssignOp::MulAssign)
    } else if matches!(kind, Kind::SlashEqualsToken) {
        Some(AssignOp::DivAssign)
    } else if matches!(kind, Kind::PercentEqualsToken) {
        Some(AssignOp::ModAssign)
    } else if matches!(kind, Kind::AsteriskAsteriskEqualsToken) {
        Some(AssignOp::ExpAssign)
    } else if matches!(kind, Kind::AmpersandEqualsToken) {
        Some(AssignOp::BitAndAssign)
    } else if matches!(kind, Kind::BarEqualsToken) {
        Some(AssignOp::BitOrAssign)
    } else if matches!(kind, Kind::CaretEqualsToken) {
        Some(AssignOp::BitXorAssign)
    } else if matches!(kind, Kind::LessThanLessThanEqualsToken) {
        Some(AssignOp::ShlAssign)
    } else if matches!(kind, Kind::GreaterThanGreaterThanEqualsToken) {
        Some(AssignOp::ShrAssign)
    } else if matches!(kind, Kind::GreaterThanGreaterThanGreaterThanEqualsToken) {
        Some(AssignOp::UShrAssign)
    } else {
        None
    }
}

fn unwrap_one_wrapper(node: P<Node>) -> P<Node> {
    let kind = node.kind();
    if kind == Kind::ParenthesizedExpression {
        node.as_parenthesized_expression().expression()
    } else if kind == Kind::AsExpression {
        node.as_as_expression().expression()
    } else if kind == Kind::TypeAssertionExpression {
        node.as_type_assertion().expression()
    } else if kind == Kind::SatisfiesExpression {
        node.as_satisfies_expression().expression()
    } else if kind == Kind::NonNullExpression {
        node.as_non_null_expression().expression()
    } else {
        node
    }
}

fn is_destructuring_assign_target(node: P<Node>) -> bool {
    let unwrapped = unwrap_expr_wrappers(node);
    matches!(
        unwrapped.kind(),
        Kind::ObjectLiteralExpression | Kind::ArrayLiteralExpression
    )
}

fn class_has_extends(class_node: P<Node>) -> bool {
    let clauses = if class_node.kind() == Kind::ClassDeclaration {
        class_node.as_class_declaration().heritage_clauses()
    } else if class_node.kind() == Kind::ClassExpression {
        class_node.as_class_expression().heritage_clauses()
    } else {
        None
    };
    clauses.is_some_and(|list| {
        list.nodes()
            .iter()
            .any(|c| c.as_heritage_clause().token() == Kind::ExtendsKeyword)
    })
}

fn subtree_contains_super_call(node: P<Node>) -> bool {
    if node.kind() == Kind::CallExpression
        && node.as_call_expression().expression().kind() == Kind::SuperKeyword
    {
        return true;
    }
    if is_function_like_declaration(Some(node))
        || matches!(node.kind(), Kind::ClassDeclaration | Kind::ClassExpression)
    {
        return false;
    }
    let mut found = false;
    node.for_each_child(&mut |child| {
        if subtree_contains_super_call(child) {
            found = true;
            return true;
        }
        false
    });
    found
}

fn has_parameter_property_modifier(param: P<Node>) -> bool {
    let flags = get_combined_modifier_flags(param);
    flags.intersects(
        ModifierFlags::Public
            | ModifierFlags::Protected
            | ModifierFlags::Private
            | ModifierFlags::Readonly,
    )
}

fn extract_param_property_init(
    bridge: &SemanticModelBridge<'_>,
    param: P<Node>,
) -> Option<ClassFieldInit> {
    if !has_parameter_property_modifier(param) {
        return None;
    }
    let sem = extract_decl_semantics(param);
    let name = param.name().and_then(node_text).unwrap_or_default();
    let syntax = bridge.intern_syntax(param).unwrap_or(SyntaxId(0));
    Some(ClassFieldInit {
        name,
        syntax,
        is_static: false,
        is_readonly: sem.is_readonly,
        is_override: sem.is_override,
        visibility: sem.visibility.or(Some(MemberVisibility::Public)),
        has_initializer: true,
    })
}

fn is_value_ident_usage(ident: P<Node>) -> bool {
    let Some(parent) = ident.parent() else {
        return false;
    };
    let pk = parent.kind();
    if pk == Kind::PropertyAccessExpression
        && parent.as_property_access_expression().name() == ident
    {
        false
    } else if matches!(
        pk,
        Kind::VariableDeclaration | Kind::Parameter | Kind::BindingElement
    ) {
        parent.name() != Some(ident)
    } else {
        !(matches!(
            pk,
            Kind::PropertyAssignment
                | Kind::MethodDeclaration
                | Kind::GetAccessor
                | Kind::SetAccessor
        ) && parent.name() == Some(ident))
    }
}

fn is_ident_write_site(ident: P<Node>) -> bool {
    let mut cur = ident;
    while let Some(parent) = cur.parent() {
        let pk = parent.kind();
        if matches!(
            pk,
            Kind::ParenthesizedExpression
                | Kind::NonNullExpression
                | Kind::AsExpression
                | Kind::TypeAssertionExpression
                | Kind::SatisfiesExpression
                | Kind::ShorthandPropertyAssignment
                | Kind::PropertyAssignment
                | Kind::ObjectLiteralExpression
                | Kind::ArrayLiteralExpression
                | Kind::SpreadElement
                | Kind::SpreadAssignment
        ) {
            cur = parent;
        } else if pk == Kind::PrefixUnaryExpression {
            return matches!(
                parent.as_prefix_unary_expression().operator(),
                Kind::PlusPlusToken | Kind::MinusMinusToken
            );
        } else if pk == Kind::PostfixUnaryExpression {
            return matches!(
                parent.as_postfix_unary_expression().operator(),
                Kind::PlusPlusToken | Kind::MinusMinusToken
            );
        } else if pk == Kind::BinaryExpression {
            let bin = parent.as_binary_expression();
            if bin.left() != cur {
                return false;
            }
            let op = bin.operator_token().kind();
            return classify_assign_op(op).is_some()
                || matches!(
                    op,
                    Kind::AmpersandAmpersandEqualsToken
                        | Kind::BarBarEqualsToken
                        | Kind::QuestionQuestionEqualsToken
                );
        } else if matches!(pk, Kind::ForInStatement | Kind::ForOfStatement) {
            return parent.as_for_in_or_of_statement().initializer() == cur;
        } else {
            return false;
        }
    }
    false
}

const fn classify_binary_op(kind: Kind) -> BinaryOp {
    if matches!(kind, Kind::PlusToken) {
        BinaryOp::Add
    } else if matches!(kind, Kind::MinusToken) {
        BinaryOp::Sub
    } else if matches!(kind, Kind::AsteriskToken) {
        BinaryOp::Mul
    } else if matches!(kind, Kind::SlashToken) {
        BinaryOp::Div
    } else if matches!(kind, Kind::PercentToken) {
        BinaryOp::Mod
    } else if matches!(kind, Kind::AsteriskAsteriskToken) {
        BinaryOp::Exp
    } else if matches!(kind, Kind::EqualsEqualsToken) {
        BinaryOp::Eq
    } else if matches!(kind, Kind::ExclamationEqualsToken) {
        BinaryOp::Neq
    } else if matches!(kind, Kind::EqualsEqualsEqualsToken) {
        BinaryOp::StrictEq
    } else if matches!(kind, Kind::ExclamationEqualsEqualsToken) {
        BinaryOp::StrictNeq
    } else if matches!(kind, Kind::LessThanToken) {
        BinaryOp::Lt
    } else if matches!(kind, Kind::LessThanEqualsToken) {
        BinaryOp::Lte
    } else if matches!(kind, Kind::GreaterThanToken) {
        BinaryOp::Gt
    } else if matches!(kind, Kind::GreaterThanEqualsToken) {
        BinaryOp::Gte
    } else if matches!(kind, Kind::AmpersandToken) {
        BinaryOp::BitAnd
    } else if matches!(kind, Kind::BarToken) {
        BinaryOp::BitOr
    } else if matches!(kind, Kind::CaretToken) {
        BinaryOp::BitXor
    } else if matches!(kind, Kind::LessThanLessThanToken) {
        BinaryOp::Shl
    } else if matches!(kind, Kind::GreaterThanGreaterThanToken) {
        BinaryOp::Shr
    } else if matches!(kind, Kind::GreaterThanGreaterThanGreaterThanToken) {
        BinaryOp::UShr
    } else if matches!(kind, Kind::InKeyword) {
        BinaryOp::In
    } else if matches!(kind, Kind::InstanceOfKeyword) {
        BinaryOp::InstanceOf
    } else {
        BinaryOp::Comma
    }
}

fn static_bool_value(node: P<Node>) -> Option<bool> {
    let kind = node.kind();
    if kind == Kind::TrueKeyword {
        Some(true)
    } else if kind == Kind::FalseKeyword {
        Some(false)
    } else if kind == Kind::ParenthesizedExpression {
        static_bool_value(node.as_parenthesized_expression().expression())
    } else if kind == Kind::PrefixUnaryExpression {
        let prefix = node.as_prefix_unary_expression();
        if prefix.operator() == Kind::ExclamationToken {
            static_bool_value(prefix.operand()).map(|val| !val)
        } else {
            None
        }
    } else {
        None
    }
}
