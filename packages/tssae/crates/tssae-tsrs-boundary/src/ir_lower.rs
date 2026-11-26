use std::sync::Arc;

use rustc_hash::FxHashMap;
use tsrs_ast::{
    Kind, ModifierFlags, Node, NodeFlags, SourceFile, get_combined_modifier_flags,
    get_source_file_of_node, is_const_type_reference, is_function_like_declaration, is_type_node,
};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;
use tssae_analysis::ir::{
    AssignOp, BinaryOp, BindingMutability, CfgVertex, DeclarationSemantics, ExplicitEdge,
    ExplicitEdgeKind, FunctionIr, IrArrayElem, IrBranchDirection, IrBranchKind, IrBranchRecord,
    IrCallArg, IrCallKind, IrLocalBinding, IrObjectProp, IrOp, IrParameter, IrPropKey, IrScopeKind,
    LiteralKind, MemberVisibility, UnaryOp,
};
use tssae_common::{
    BranchId, DiagnosticLocation, FunctionIrId, GapItem, LocalBindingId, LocalSyntaxId, SymbolId,
    SyntaxId, UnknownReason, ValueId, VertexId,
};

use crate::semantic_facade::SemanticModelBridge;
use crate::syntax_view::{node_location, node_text, prop_key_text, relative_to_cwd};

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

/// Normalizes a function-like node, block, or statement suitable as a CFG root scope.
#[must_use]
pub fn normalize_cfg_scope_node(node: P<Node>) -> Option<P<Node>> {
    if let Some(fn_node) = normalize_function_node(node) {
        return Some(fn_node);
    }
    if node.kind() == Kind::Block || tsrs_ast::is_statement(node) {
        return Some(node);
    }
    None
}

/// Lowers a normalized function, block, or statement scope into a verified [`LoweredScope`].
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
    if normalized_fn.is_none()
        && scope_node.kind() != Kind::Block
        && !tsrs_ast::is_statement(scope_node)
    {
        return Err(GapItem::from_reason(
            UnknownReason::InvalidCfgScope,
            "CFG scope node must be a function, block, or statement.".to_string(),
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

    let bridge = SemanticModelBridge::new(cwd, program, checker);
    let scope_ir = {
        let mut lowerer = ScopeLowerer::new(
            sf,
            &rel_file,
            &bridge,
            scope_node,
            scope_location,
            local_scope,
        );

        if let Some(fn_node) = normalized_fn {
            let body = fn_node.body().ok_or_else(|| {
                GapItem::from_reason(
                    UnknownReason::MissingFunctionBody,
                    "Function declaration has no body to analyze.".to_string(),
                    Some(lowerer.scope_location.clone()),
                )
            })?;

            lowerer.collect_parameters(fn_node);

            let body_entry = if body.kind() == Kind::Block {
                lowerer.lower_stmt(body, lowerer.normal_exit, None, None)
            } else {
                let (expr_entry, expr_val) = lowerer.lower_expr_to(body, lowerer.normal_exit);
                let _ = expr_val;
                expr_entry
            };

            let mut start = body_entry;
            for &param in fn_node.parameters().iter().rev() {
                if let Some(init) = param.initializer() {
                    let (init_entry, _) = lowerer.lower_expr_to(init, start);
                    start = init_entry;
                }
            }
            lowerer.add_edge(lowerer.entry, start, ExplicitEdgeKind::Fallthrough);
        } else {
            // Inside a local statement/block scope (such as a loop body), unlabeled break/continue
            // exit the local scope cleanly without requiring outer loop back-edges.
            let entry_target = lowerer.lower_stmt(
                scope_node,
                lowerer.normal_exit,
                Some(lowerer.normal_exit),
                Some(lowerer.normal_exit),
            );
            lowerer.add_edge(lowerer.entry, entry_target, ExplicitEdgeKind::Fallthrough);
        }

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

struct ScopeLowerer<'a> {
    sf: P<SourceFile>,
    rel_file: &'a str,
    bridge: &'a SemanticModelBridge<'a>,
    root_node: P<Node>,
    scope_kind: IrScopeKind,
    scope_location: DiagnosticLocation,
    decl_semantics: DeclarationSemantics,
    local_scope: u32,
    vertices: Vec<CfgVertex>,
    entry: VertexId,
    normal_exit: VertexId,
    exceptional_exit: VertexId,
    exit: VertexId,
    parameters: Vec<IrParameter>,
    bindings: Vec<IrLocalBinding>,
    binding_by_symbol: FxHashMap<SymbolId, LocalBindingId>,
    binding_by_name: FxHashMap<String, LocalBindingId>,
    node_to_syntax: FxHashMap<P<Node>, SyntaxId>,
    syntax_to_node: FxHashMap<SyntaxId, P<Node>>,
    syntax_to_vertex: FxHashMap<SyntaxId, VertexId>,
    expr_to_vertex: FxHashMap<LocalSyntaxId, VertexId>,
    expr_to_value: FxHashMap<LocalSyntaxId, ValueId>,
    branches: Vec<IrBranchRecord>,
    next_value_id: u32,
}

impl<'a> ScopeLowerer<'a> {
    fn new(
        sf: P<SourceFile>,
        rel_file: &'a str,
        bridge: &'a SemanticModelBridge<'a>,
        root_node: P<Node>,
        scope_location: DiagnosticLocation,
        local_scope: u32,
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

        Self {
            sf,
            rel_file,
            bridge,
            root_node,
            scope_kind,
            scope_location,
            decl_semantics,
            local_scope,
            vertices,
            entry,
            normal_exit,
            exceptional_exit,
            exit,
            parameters: Vec::new(),
            bindings: Vec::new(),
            binding_by_symbol: FxHashMap::default(),
            binding_by_name: FxHashMap::default(),
            node_to_syntax: FxHashMap::default(),
            syntax_to_node: FxHashMap::default(),
            syntax_to_vertex: FxHashMap::default(),
            expr_to_vertex: FxHashMap::default(),
            expr_to_value: FxHashMap::default(),
            branches: Vec::new(),
            next_value_id: 0,
        }
    }

    fn finish(self) -> LoweredScope {
        let root_syntax = self
            .node_to_syntax
            .get(&self.root_node)
            .copied()
            .unwrap_or_else(|| {
                self.bridge
                    .intern_syntax(self.root_node)
                    .unwrap_or(SyntaxId(0))
            });
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
            parameters: self.parameters,
            bindings: self.bindings,
            vertices: self.vertices,
            entry: self.entry,
            normal_exit: self.normal_exit,
            exceptional_exit: self.exceptional_exit,
            exit: self.exit,
            syntax_to_vertex: self.syntax_to_vertex,
            expr_to_vertex: self.expr_to_vertex,
            expr_to_value: self.expr_to_value,
            branches: self.branches,
            value_count: self.next_value_id,
        };
        LoweredScope {
            ir: Arc::new(ir),
            node_to_syntax: self.node_to_syntax,
            syntax_to_node: self.syntax_to_node,
        }
    }

    fn intern_node(&mut self, node: P<Node>) -> SyntaxId {
        if let Some(&sid) = self.node_to_syntax.get(&node) {
            return sid;
        }
        let sid = self.bridge.intern_syntax(node).unwrap_or(SyntaxId(0));
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
        with_semantics: bool,
    ) -> VertexId {
        let id = VertexId(u32::try_from(self.vertices.len()).unwrap_or(u32::MAX));
        let (syntax, local_syntax, location, symbol, ty, signature) = if let Some(n) = node {
            let sid = self.intern_node(n);
            let lsid = sid.local_id();
            let loc = Some(self.loc(n));
            let (sym, ty, sig) = if with_semantics {
                (
                    self.bridge.symbol_id_for_node(n),
                    self.bridge.type_id_for_node(n),
                    self.bridge.signature_id_for_node(n),
                )
            } else {
                (None, None, None)
            };
            (Some(sid), Some(lsid), loc, sym, ty, sig)
        } else {
            (None, None, None, None, None, None)
        };

        self.vertices.push(CfgVertex {
            id,
            syntax,
            local_syntax,
            symbol,
            ty,
            signature,
            location,
            value,
            op,
            explicit_edges: Vec::new(),
            may_throw_edge: None,
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
            let binding = if param.name().is_some_and(|n| n.kind() == Kind::Identifier) {
                Some(self.register_binding(&name, BindingMutability::Parameter, param))
            } else {
                None
            };
            self.parameters.push(IrParameter {
                name,
                binding,
                syntax: sid,
                has_default: param_data.initializer().is_some(),
                is_rest: param_data.dot_dot_dot_token().is_some(),
            });
        }
    }

    fn register_binding(
        &mut self,
        name: &str,
        mutability: BindingMutability,
        decl_node: P<Node>,
    ) -> LocalBindingId {
        let sid = self.intern_node(decl_node);
        let symbol = self.bridge.symbol_id_for_node(decl_node);
        if let Some(sym) = symbol
            && let Some(&existing) = self.binding_by_symbol.get(&sym)
        {
            return existing;
        }
        let id = LocalBindingId(u32::try_from(self.bindings.len()).unwrap_or(u32::MAX));
        self.bindings.push(IrLocalBinding {
            id,
            name: name.to_string(),
            mutability,
            syntax: sid,
            symbol,
        });
        if let Some(sym) = symbol {
            self.binding_by_symbol.insert(sym, id);
        }
        if !name.is_empty() {
            self.binding_by_name.insert(name.to_string(), id);
        }
        id
    }

    fn resolve_binding(&self, ident_node: P<Node>) -> Option<LocalBindingId> {
        if let Some(sym) = self.bridge.symbol_id_for_node(ident_node)
            && let Some(&id) = self.binding_by_symbol.get(&sym)
        {
            return Some(id);
        }
        node_text(ident_node).and_then(|name| self.binding_by_name.get(&name).copied())
    }

    fn lower_sequence(
        &mut self,
        statements: &[P<Node>],
        mut next: VertexId,
        break_to: Option<VertexId>,
        continue_to: Option<VertexId>,
    ) -> VertexId {
        for &stmt_node in statements.iter().rev() {
            next = self.lower_stmt(stmt_node, next, break_to, continue_to);
        }
        next
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Exhaustive 353-variant tsrs_ast::Kind statement dispatcher."
    )]
    fn lower_stmt(
        &mut self,
        node: P<Node>,
        next: VertexId,
        break_to: Option<VertexId>,
        continue_to: Option<VertexId>,
    ) -> VertexId {
        match node.kind() {
            Kind::Block | Kind::ModuleBlock => {
                let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                let first = self.lower_sequence(node.statements(), next, break_to, continue_to);
                self.add_edge(here, first, ExplicitEdgeKind::Fallthrough);
                here
            }
            Kind::IfStatement => self.lower_if(node, next, break_to, continue_to),
            Kind::SwitchStatement => self.lower_switch(node, next, continue_to),
            Kind::ReturnStatement => {
                let expr_opt = node.as_return_statement().expression();
                let ret_vid =
                    self.new_vertex(Some(node), IrOp::Return { value: None }, None, false);
                self.add_edge(ret_vid, self.normal_exit, ExplicitEdgeKind::Return);
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
                let dummy = ValueId(0);
                let throw_vid =
                    self.new_vertex(Some(node), IrOp::Throw { exception: dummy }, None, false);
                self.add_edge(
                    throw_vid,
                    self.exceptional_exit,
                    ExplicitEdgeKind::ExplicitThrow,
                );
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
                if label.is_none()
                    && let Some(target) = break_to
                {
                    let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                    self.add_edge(here, target, ExplicitEdgeKind::Break { label: None });
                    here
                } else {
                    let here = self.new_vertex(
                        Some(node),
                        IrOp::Unknown {
                            reason: UnknownReason::UnmodeledJump,
                            message:
                                "Labeled or unstructured break statement is not precisely modeled."
                                    .to_string(),
                            location: self.loc(node),
                            evaluated_children: Vec::new(),
                        },
                        None,
                        false,
                    );
                    self.add_edge(here, next, ExplicitEdgeKind::Break { label });
                    here
                }
            }
            Kind::ContinueStatement => {
                let label = node.as_continue_statement().label().and_then(node_text);
                if label.is_none()
                    && let Some(target) = continue_to
                {
                    let here = self.new_vertex(Some(node), IrOp::NopMerge, None, false);
                    self.add_edge(here, target, ExplicitEdgeKind::Continue { label: None });
                    here
                } else {
                    let here = self.new_vertex(
                        Some(node),
                        IrOp::Unknown {
                            reason: UnknownReason::UnmodeledJump,
                            message: "Loop continue statements are not precisely modeled."
                                .to_string(),
                            location: self.loc(node),
                            evaluated_children: Vec::new(),
                        },
                        None,
                        false,
                    );
                    self.add_edge(here, next, ExplicitEdgeKind::Continue { label });
                    here
                }
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
            Kind::TryStatement => self.lower_try_unknown(node, next, break_to, continue_to),
            Kind::WhileStatement
            | Kind::DoStatement
            | Kind::ForStatement
            | Kind::ForInStatement
            | Kind::ForOfStatement => self.lower_loop_unknown(node, next),
            Kind::LabeledStatement => {
                let unknown_v = self.new_vertex(
                    Some(node),
                    IrOp::Unknown {
                        reason: UnknownReason::UnmodeledJump,
                        message: "Labeled statements are not precisely modeled.".to_string(),
                        location: self.loc(node),
                        evaluated_children: Vec::new(),
                    },
                    None,
                    false,
                );
                let inner = node.as_labeled_statement().statement();
                let inner_entry = self.lower_stmt(inner, next, break_to, continue_to);
                self.add_edge(unknown_v, inner_entry, ExplicitEdgeKind::Fallthrough);
                unknown_v
            }
            Kind::WithStatement => {
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
                let body_entry =
                    self.lower_stmt(with_stmt.statement(), next, break_to, continue_to);
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
                let here = self.new_vertex(
                    Some(node),
                    IrOp::FunctionLiteral { syntax: sid },
                    None,
                    true,
                );
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                here
            }
            Kind::ClassDeclaration => {
                let sid = self.intern_node(node);
                let here =
                    self.new_vertex(Some(node), IrOp::ClassLiteral { syntax: sid }, None, true);
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                here
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
            | Kind::Decorator
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

    fn lower_if(
        &mut self,
        node: P<Node>,
        next: VertexId,
        break_to: Option<VertexId>,
        continue_to: Option<VertexId>,
    ) -> VertexId {
        let if_stmt = node.as_if_statement();
        let cond = if_stmt.expression();
        let then_entry = self.lower_stmt(if_stmt.then_statement(), next, break_to, continue_to);
        let else_entry = if_stmt.else_statement().map_or(next, |else_node| {
            self.lower_stmt(else_node, next, break_to, continue_to)
        });

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

    fn lower_switch(
        &mut self,
        node: P<Node>,
        next: VertexId,
        continue_to: Option<VertexId>,
    ) -> VertexId {
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

        let mut fallthrough = next;
        let mut has_default = false;
        let mut clause_entries = Vec::with_capacity(clauses.len());
        for &clause in clauses.iter().rev() {
            has_default |= clause.kind() == Kind::DefaultClause;
            let body_entry =
                self.lower_sequence(clause.statements(), fallthrough, Some(next), continue_to);
            self.record_syntax_vertex(clause, body_entry);
            clause_entries.push((clause, body_entry));
            fallthrough = body_entry;
        }
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
            let decl_data = decl.as_variable_declaration();
            let name_node = decl.name();
            let init_opt = decl_data.initializer();

            let val_id = self.alloc_value();
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
            cont = decl_v;

            if let Some(nn) = name_node {
                self.record_syntax_vertex(nn, decl_v);
                cont = self.lower_binding_defaults(nn, cont);
            }
            if let Some(init) = init_opt {
                let (init_entry, init_val) = self.lower_expr_to(init, cont);
                if let Some(v) = self.vertices.get_mut(decl_v.0 as usize) {
                    if let IrOp::AssignLocal { value, .. } = &mut v.op {
                        *value = init_val;
                    } else if let IrOp::AssignSymbol { value, .. } = &mut v.op {
                        *value = init_val;
                    }
                }
                cont = init_entry;
            }
        }
        cont
    }

    fn lower_binding_defaults(&mut self, pattern: P<Node>, mut cont: VertexId) -> VertexId {
        let kind = pattern.kind();
        if kind == Kind::ObjectBindingPattern || kind == Kind::ArrayBindingPattern {
            for &elem in pattern.elements().iter().rev() {
                cont = self.lower_binding_defaults(elem, cont);
            }
            self.record_syntax_vertex(pattern, cont);
        } else if kind == Kind::BindingElement {
            let be = pattern.as_binding_element();
            if let Some(name) = be.name() {
                cont = self.lower_binding_defaults(name, cont);
            }
            if let Some(init) = be.initializer() {
                let (init_entry, _) = self.lower_expr_to(init, cont);
                cont = init_entry;
            }
            if let Some(prop_name) = be.property_name()
                && prop_name.kind() == Kind::ComputedPropertyName
                && let Some(expr) = prop_name.expression()
            {
                let (key_entry, _) = self.lower_expr_to(expr, cont);
                cont = key_entry;
            }
            self.record_syntax_vertex(pattern, cont);
        } else if kind == Kind::Identifier {
            let name = node_text(pattern).unwrap_or_default();
            let mutability = classify_decl_mutability(pattern);
            self.register_binding(&name, mutability, pattern);
        }
        cont
    }

    fn lower_try_unknown(
        &mut self,
        node: P<Node>,
        next: VertexId,
        break_to: Option<VertexId>,
        continue_to: Option<VertexId>,
    ) -> VertexId {
        let unknown_v = self.new_vertex(
            Some(node),
            IrOp::Unknown {
                reason: UnknownReason::UnmodeledTryCatch,
                message: "Control flow through try/catch/finally is not precisely modeled."
                    .to_string(),
                location: self.loc(node),
                evaluated_children: Vec::new(),
            },
            None,
            false,
        );
        let try_stmt = node.as_try_statement();
        let after_try = try_stmt
            .finally_block()
            .map_or(next, |fb| self.lower_stmt(fb, next, break_to, continue_to));
        let try_entry = self.lower_stmt(try_stmt.try_block(), after_try, break_to, continue_to);
        self.add_edge(unknown_v, try_entry, ExplicitEdgeKind::Fallthrough);

        if let Some(catch_clause) = try_stmt.catch_clause() {
            let catch_entry = self.lower_stmt(
                catch_clause.as_catch_clause().block(),
                after_try,
                break_to,
                continue_to,
            );
            self.add_edge(unknown_v, catch_entry, ExplicitEdgeKind::Fallthrough);
        }
        unknown_v
    }

    fn lower_loop_unknown(&mut self, node: P<Node>, next: VertexId) -> VertexId {
        let unknown_v = self.new_vertex(
            Some(node),
            IrOp::Unknown {
                reason: UnknownReason::UnmodeledLoop,
                message: "Control flow through loops is not precisely modeled.".to_string(),
                location: self.loc(node),
                evaluated_children: Vec::new(),
            },
            None,
            false,
        );

        let mut children = Vec::new();
        node.for_each_child(&mut |child| {
            children.push(child);
            false
        });

        let mut body_cont = next;
        for &child in children.iter().rev() {
            if is_function_like_declaration(Some(child))
                || matches!(child.kind(), Kind::ClassDeclaration | Kind::ClassExpression)
                || is_type_node(child)
            {
                continue;
            }
            body_cont = if tsrs_ast::is_statement(child) {
                self.lower_stmt(child, body_cont, None, None)
            } else {
                let (e, _) = self.lower_expr_to(child, body_cont);
                e
            };
        }

        self.add_edge(unknown_v, body_cont, ExplicitEdgeKind::Fallthrough);
        // Unmodeled loops can execute 0 times and exit directly to `next`.
        if body_cont != next {
            self.add_edge(unknown_v, next, ExplicitEdgeKind::Fallthrough);
        }
        unknown_v
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
                self.record_syntax_vertex(node, entry);
                entry
            }
            Kind::BinaryExpression => {
                let bin = node.as_binary_expression();
                let op_kind = bin.operator_token().kind();
                if matches!(
                    op_kind,
                    Kind::AmpersandAmpersandToken | Kind::AmpersandAmpersandEqualsToken
                ) {
                    self.lower_logical_and_bool(node, bin.left(), bin.right(), on_true, on_false)
                } else if matches!(op_kind, Kind::BarBarToken | Kind::BarBarEqualsToken) {
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
            | Kind::ComputedPropertyName
            | Kind::TypeParameter
            | Kind::Parameter
            | Kind::Decorator
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

        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, true);
        let left_entry = self.lower_bool_expr(left, cont_gate, short_gate);
        self.add_edge(here, left_entry, ExplicitEdgeKind::Fallthrough);
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

        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, true);
        let left_entry = self.lower_bool_expr(left, short_gate, cont_gate);
        self.add_edge(here, left_entry, ExplicitEdgeKind::Fallthrough);
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

        let here = self.new_vertex(Some(node), IrOp::NopMerge, None, true);
        let cond_entry = self.lower_bool_expr(cond, true_gate, false_gate);
        self.add_edge(here, cond_entry, ExplicitEdgeKind::Fallthrough);
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
        if is_type_node(node) {
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
                let sid = self.intern_node(node);
                let op = self
                    .resolve_binding(node)
                    .map_or(IrOp::ReadSymbol { ident_syntax: sid }, |binding| {
                        IrOp::ReadLocal { binding }
                    });
                self.emit_leaf_expr(node, op, next, true)
            }
            Kind::PrivateIdentifier => {
                let sid = self.intern_node(node);
                self.emit_leaf_expr(node, IrOp::ReadSymbol { ident_syntax: sid }, next, true)
            }
            Kind::FunctionDeclaration | Kind::FunctionExpression | Kind::ArrowFunction => {
                let sid = self.intern_node(node);
                self.emit_leaf_expr(node, IrOp::FunctionLiteral { syntax: sid }, next, true)
            }
            Kind::ClassDeclaration | Kind::ClassExpression => {
                let sid = self.intern_node(node);
                self.emit_leaf_expr(node, IrOp::ClassLiteral { syntax: sid }, next, true)
            }
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
                let nn = node.as_non_null_expression();
                self.lower_wrapper_expr(node, nn.expression(), false, true, next)
            }
            Kind::ExpressionWithTypeArguments => {
                let e = node.as_expression_with_type_arguments().expression();
                self.lower_wrapper_expr(node, e, false, false, next)
            }
            Kind::SpreadElement => {
                let e = node.as_spread_element().expression();
                self.lower_wrapper_expr(node, e, false, false, next)
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
            Kind::ObjectLiteralExpression => self.lower_object_literal(node, next),
            Kind::ArrayLiteralExpression => self.lower_array_literal(node, next),
            Kind::YieldExpression => {
                let val = self.alloc_value();
                let here = self.new_vertex(
                    Some(node),
                    IrOp::Unknown {
                        reason: UnknownReason::UnmodeledGenerator,
                        message: "Generator yield control flow is not precisely modeled."
                            .to_string(),
                        location: self.loc(node),
                        evaluated_children: Vec::new(),
                    },
                    Some(val),
                    true,
                );
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                self.bind_expr_maps(node, here, val);
                if let Some(inner) = node.as_yield_expression().expression() {
                    let (inner_entry, inner_val) = self.lower_expr_to(inner, here);
                    if let Some(v) = self.vertices.get_mut(here.0 as usize)
                        && let IrOp::Unknown {
                            evaluated_children, ..
                        } = &mut v.op
                    {
                        evaluated_children.push(inner_val);
                    }
                    (inner_entry, val)
                } else {
                    (here, val)
                }
            }
            Kind::AwaitExpression => {
                let val = self.alloc_value();
                let here = self.new_vertex(
                    Some(node),
                    IrOp::Unknown {
                        reason: UnknownReason::UnmodeledAwait,
                        message: "Async await suspension control flow is not precisely modeled."
                            .to_string(),
                        location: self.loc(node),
                        evaluated_children: Vec::new(),
                    },
                    Some(val),
                    true,
                );
                self.add_edge(here, next, ExplicitEdgeKind::Fallthrough);
                self.bind_expr_maps(node, here, val);
                let inner = node.as_await_expression().expression();
                let (inner_entry, inner_val) = self.lower_expr_to(inner, here);
                if let Some(v) = self.vertices.get_mut(here.0 as usize)
                    && let IrOp::Unknown {
                        evaluated_children, ..
                    } = &mut v.op
                {
                    evaluated_children.push(inner_val);
                }
                (inner_entry, val)
            }
            // Complex / unmodeled expressions and JSX: evaluate child expressions then emit Unknown
            Kind::JsxElement
            | Kind::JsxSelfClosingElement
            | Kind::JsxFragment
            | Kind::JsxExpression
            | Kind::JsxAttribute
            | Kind::JsxAttributes
            | Kind::JsxSpreadAttribute
            | Kind::JsxOpeningElement
            | Kind::JsxClosingElement
            | Kind::JsxOpeningFragment
            | Kind::JsxClosingFragment
            | Kind::JsxNamespacedName
            | Kind::SyntheticExpression
            | Kind::PartiallyEmittedExpression
            | Kind::SyntheticReferenceExpression => self.lower_children_unknown(
                node,
                UnknownReason::UnmodeledConstruct,
                format!(
                    "Expression kind {:?} is not precisely modeled.",
                    node.kind()
                ),
                next,
            ),
            // Any non-expression Kind encountered via generic child traversal
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
            | Kind::Decorator
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
        let op = if op_kind == Kind::ExclamationToken {
            UnaryOp::LogicalNot
        } else if op_kind == Kind::TildeToken {
            UnaryOp::BitwiseNot
        } else if op_kind == Kind::MinusToken {
            UnaryOp::Minus
        } else if op_kind == Kind::PlusPlusToken {
            UnaryOp::Increment
        } else if op_kind == Kind::MinusMinusToken {
            UnaryOp::Decrement
        } else {
            UnaryOp::Plus
        };
        self.lower_unary_op_expr(node, op, p.operand(), next)
    }

    fn lower_postfix_unary(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let p = node.as_postfix_unary_expression();
        let op = if p.operator() == Kind::MinusMinusToken {
            UnaryOp::Decrement
        } else {
            UnaryOp::Increment
        };
        self.lower_unary_op_expr(node, op, p.operand(), next)
    }

    fn lower_conditional_expr(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let cond_expr = node.as_conditional_expression();
        let out_val = self.alloc_value();
        let join = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
        self.add_edge(join, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, join, out_val);

        let (true_entry, _) = self.lower_expr_to(cond_expr.when_true(), join);
        let (false_entry, _) = self.lower_expr_to(cond_expr.when_false(), join);
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
            Kind::AmpersandAmpersandToken
                | Kind::AmpersandAmpersandEqualsToken
                | Kind::BarBarToken
                | Kind::BarBarEqualsToken
        ) {
            let out_val = self.alloc_value();
            let entry = self.lower_bool_expr(node, next, next);
            self.bind_expr_maps(node, entry, out_val);
            return (entry, out_val);
        }
        if matches!(
            op_kind,
            Kind::QuestionQuestionToken | Kind::QuestionQuestionEqualsToken
        ) {
            return self.lower_nullish_expr(node, bin.left(), bin.right(), next);
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

    fn lower_nullish_expr(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        right: P<Node>,
        next: VertexId,
    ) -> (VertexId, ValueId) {
        let (right_entry, _) = self.lower_expr_to(right, next);
        let expr_loc = self.loc(node);
        let left_loc = self.loc(left);
        let short_branch = self.push_branch(
            IrBranchKind::NullishCoalescing,
            IrBranchDirection::ShortCircuit,
            left,
            left_loc.clone(),
            expr_loc.clone(),
            None,
        );
        let cont_branch = self.push_branch(
            IrBranchKind::NullishCoalescing,
            IrBranchDirection::Continue,
            left,
            left_loc,
            expr_loc,
            None,
        );

        let gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        self.add_edge(gate, next, ExplicitEdgeKind::Branch(short_branch));
        self.add_edge(gate, right_entry, ExplicitEdgeKind::Branch(cont_branch));

        let out_val = self.alloc_value();
        let here = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
        let (left_entry, _) = self.lower_expr_to(left, gate);
        self.add_edge(here, left_entry, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, here, out_val);
        (here, out_val)
    }

    fn lower_assignment_expr(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        right: P<Node>,
        op: AssignOp,
        next: VertexId,
    ) -> (VertexId, ValueId) {
        let out_val = self.alloc_value();
        let assign_v = self.build_assign_target_vertex(node, left, op, out_val, next);
        let (right_entry, right_val) = self.lower_expr_to(right, assign_v);
        if let Some(vert) = self.vertices.get_mut(assign_v.0 as usize) {
            if let IrOp::AssignLocal { value, .. } = &mut vert.op {
                *value = right_val;
            } else if let IrOp::AssignSymbol { value, .. } = &mut vert.op {
                *value = right_val;
            } else if let IrOp::PropWrite { value, .. } = &mut vert.op {
                *value = right_val;
            }
        }
        let (lhs_entry, _) = self.lower_expr_to(left, right_entry);
        (lhs_entry, out_val)
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

        if pa.question_dot_token().is_some() {
            return self.lower_optional_chain(node, base, out_val, next, |this, join, base_val| {
                let read_v = this.new_vertex(
                    Some(node),
                    IrOp::PropRead {
                        object: base_val,
                        key,
                        is_super,
                    },
                    Some(out_val),
                    true,
                );
                this.add_edge(read_v, join, ExplicitEdgeKind::Fallthrough);
                read_v
            });
        }

        let read_v = self.new_vertex(
            Some(node),
            IrOp::PropRead {
                object: out_val,
                key,
                is_super,
            },
            Some(out_val),
            true,
        );
        self.add_edge(read_v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, read_v, out_val);
        let (base_entry, base_val) = self.lower_expr_to(base, read_v);
        if let Some(vert) = self.vertices.get_mut(read_v.0 as usize)
            && let IrOp::PropRead { object, .. } = &mut vert.op
        {
            *object = base_val;
        }
        (base_entry, out_val)
    }

    fn lower_element_access(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let ea = node.as_element_access_expression();
        let base = ea.expression();
        let arg = ea.argument_expression();
        let is_super = base.kind() == Kind::SuperKeyword;
        let out_val = self.alloc_value();
        let static_name = prop_key_text(arg);

        if ea.question_dot_token().is_some() {
            return self.lower_optional_chain(node, base, out_val, next, |this, join, base_val| {
                let read_v = this.new_vertex(
                    Some(node),
                    IrOp::PropRead {
                        object: base_val,
                        key: IrPropKey::Computed {
                            key: out_val,
                            static_name,
                        },
                        is_super,
                    },
                    Some(out_val),
                    true,
                );
                this.add_edge(read_v, join, ExplicitEdgeKind::Fallthrough);
                let (arg_entry, arg_val) = this.lower_expr_to(arg, read_v);
                if let Some(vert) = this.vertices.get_mut(read_v.0 as usize)
                    && let IrOp::PropRead {
                        key: IrPropKey::Computed { key, .. },
                        ..
                    } = &mut vert.op
                {
                    *key = arg_val;
                }
                arg_entry
            });
        }

        let read_v = self.new_vertex(
            Some(node),
            IrOp::PropRead {
                object: out_val,
                key: IrPropKey::Computed {
                    key: out_val,
                    static_name,
                },
                is_super,
            },
            Some(out_val),
            true,
        );
        self.add_edge(read_v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, read_v, out_val);
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
        (base_entry, out_val)
    }

    fn lower_call_expr(&mut self, node: P<Node>, next: VertexId) -> (VertexId, ValueId) {
        let call = node.as_call_expression();
        let callee_node = call.expression();
        let args_nodes = call.arguments().nodes();
        let out_val = self.alloc_value();
        let call_kind = if callee_node.kind() == Kind::SuperKeyword {
            IrCallKind::SuperCall
        } else {
            IrCallKind::Explicit
        };

        if call.question_dot_token().is_some() {
            return self.lower_optional_chain(
                node,
                callee_node,
                out_val,
                next,
                |this, join, callee_val| {
                    let call_v = this.new_vertex(
                        Some(node),
                        IrOp::Call {
                            call_kind,
                            callee: callee_val,
                            receiver: None,
                            args: Vec::new(),
                        },
                        Some(out_val),
                        true,
                    );
                    this.add_edge(call_v, join, ExplicitEdgeKind::Fallthrough);
                    let (args_entry, arg_vals) = this.lower_call_args(args_nodes, call_v);
                    if let Some(vert) = this.vertices.get_mut(call_v.0 as usize)
                        && let IrOp::Call { args, .. } = &mut vert.op
                    {
                        *args = arg_vals;
                    }
                    args_entry
                },
            );
        }

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
        self.add_edge(call_v, next, ExplicitEdgeKind::Fallthrough);
        self.bind_expr_maps(node, call_v, out_val);
        let (args_entry, arg_vals) = self.lower_call_args(args_nodes, call_v);
        let (callee_entry, callee_val) = self.lower_expr_to(callee_node, args_entry);
        if let Some(vert) = self.vertices.get_mut(call_v.0 as usize)
            && let IrOp::Call { callee, args, .. } = &mut vert.op
        {
            *callee = callee_val;
            *args = arg_vals;
        }
        (callee_entry, out_val)
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

    fn lower_optional_chain<F>(
        &mut self,
        node: P<Node>,
        guarded_expr: P<Node>,
        out_val: ValueId,
        next: VertexId,
        build_cont: F,
    ) -> (VertexId, ValueId)
    where
        F: FnOnce(&mut Self, VertexId, ValueId) -> VertexId,
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

        let join = self.new_vertex(Some(node), IrOp::NopMerge, Some(out_val), true);
        self.add_edge(join, next, ExplicitEdgeKind::Fallthrough);

        let gate = self.new_vertex(None, IrOp::NopMerge, None, false);
        let (base_entry, base_val) = self.lower_expr_to(guarded_expr, gate);
        let cont_entry = build_cont(self, join, base_val);

        self.add_edge(gate, join, ExplicitEdgeKind::Branch(short_branch));
        self.add_edge(gate, cont_entry, ExplicitEdgeKind::Branch(cont_branch));

        self.bind_expr_maps(node, join, out_val);
        (base_entry, out_val)
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
                let (entry, val_id) = self.lower_expr_to(name_node, cont);
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
                let fn_val = self.alloc_value();
                let fv = self.new_vertex(
                    Some(prop),
                    IrOp::FunctionLiteral { syntax: sid },
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
                || is_type_node(child)
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
    let visibility = if flags.intersects(ModifierFlags::Private) {
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
