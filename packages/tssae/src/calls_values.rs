use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use tsrs_ast::{
    Kind, Node, SourceFile, Symbol, SymbolFlags, get_source_file_of_node,
    is_function_like_declaration,
};
use tsrs_checker::{Signature, TypeFlags};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;

use crate::cfg::{GapItem, resolve_enclosing_function};
use crate::package_impl::check_call_missing_package_gap;
use crate::projects::{is_project_source_file, relative_to_cwd};
use crate::serve::{SourceLocation, node_location, slice_source_text};

#[derive(Clone)]
#[expect(
    dead_code,
    reason = "Fields retained while legacy valuePropagation/call_effects/instance_identity await Tickets 10-12 migration."
)]
pub struct ResolvedTargetDescriptor {
    pub decl_node: P<Node>,
    pub fn_node: Option<P<Node>>,
    pub symbol: Option<P<Symbol>>,
    pub name: Option<String>,
    pub location: Option<SourceLocation>,
    pub is_external: bool,
    pub is_default_library: bool,
    pub reason: String,
}

#[derive(Clone)]
pub struct CallResolutionOutcome {
    pub targets: Vec<ResolvedTargetDescriptor>,
    pub gaps: Vec<GapItem>,
}

#[derive(Clone)]
#[expect(
    dead_code,
    reason = "Fields retained while legacy instance_identity awaits Ticket 11 migration."
)]
pub struct IndexedCallSite {
    pub call_node: P<Node>,
    pub location: SourceLocation,
    pub caller_decl: Option<(P<Node>, Option<P<Symbol>>)>,
    pub resolution: CallResolutionOutcome,
}

pub struct ProjectCallGraph {
    pub sites: Vec<IndexedCallSite>,
    pub by_symbol: FxHashMap<P<Symbol>, Vec<usize>>,
    pub by_decl: FxHashMap<P<Node>, Vec<usize>>,
}

pub fn unwrap_aliased_symbol(checker: &mut CheckerHandle, mut sym: P<Symbol>) -> P<Symbol> {
    let mut seen = Vec::new();
    while sym.flags().intersects(SymbolFlags::Alias) && !seen.contains(&sym) {
        seen.push(sym);
        let next = checker.get_aliased_symbol(sym);
        if next == sym || next == checker.get_unknown_symbol() {
            break;
        }
        sym = next;
    }
    sym
}

pub fn unwrap_expr_wrappers(mut node: P<Node>) -> P<Node> {
    loop {
        match node.kind() {
            Kind::ParenthesizedExpression => {
                node = node.as_parenthesized_expression().expression();
            }
            Kind::AsExpression => {
                node = node.as_as_expression().expression();
            }
            Kind::TypeAssertionExpression => {
                node = node.as_type_assertion().expression();
            }
            Kind::SatisfiesExpression => {
                node = node.as_satisfies_expression().expression();
            }
            Kind::NonNullExpression => {
                node = node.as_non_null_expression().expression();
            }
            _ => return node,
        }
    }
}

pub fn node_location_rel(cwd: &str, node: P<Node>) -> Option<SourceLocation> {
    let sf = get_source_file_of_node(node)?;
    let rel = relative_to_cwd(cwd, sf.file_name());
    Some(node_location(sf, &rel, node))
}

pub fn node_text(node: P<Node>) -> Option<String> {
    let sf = get_source_file_of_node(node)?;
    let loc = node_location(sf, "", node);
    Some(slice_source_text(sf, loc.start, loc.end))
}

pub fn canonical_symbol_for_node(checker: &mut CheckerHandle, node: P<Node>) -> Option<P<Symbol>> {
    let raw = if node.kind() == Kind::ShorthandPropertyAssignment {
        checker
            .get_shorthand_assignment_value_symbol(Some(node))
            .or_else(|| checker.get_symbol_at_location_exported(node))
    } else if let Some(parent) = node.parent()
        && parent.kind() == Kind::ShorthandPropertyAssignment
    {
        checker
            .get_shorthand_assignment_value_symbol(Some(parent))
            .or_else(|| checker.get_symbol_at_location_exported(node))
    } else {
        checker
            .get_symbol_at_location_exported(node)
            .or_else(|| {
                node.name()
                    .and_then(|name_node| checker.get_symbol_at_location_exported(name_node))
            })
            .or_else(|| node.symbol())
    };
    raw.map(|s| unwrap_aliased_symbol(checker, s))
        .filter(|&s| s != checker.get_unknown_symbol())
}

pub fn extract_callable_from_decl(decl: P<Node>) -> Option<P<Node>> {
    if is_function_like_declaration(Some(decl)) {
        return Some(decl);
    }
    match decl.kind() {
        Kind::VariableDeclaration | Kind::PropertyDeclaration | Kind::PropertyAssignment => {
            decl.initializer().and_then(|init| {
                let inner = unwrap_expr_wrappers(init);
                is_function_like_declaration(Some(inner)).then_some(inner)
            })
        }
        Kind::ExportAssignment => decl.expression().and_then(|expr| {
            let inner = unwrap_expr_wrappers(expr);
            is_function_like_declaration(Some(inner)).then_some(inner)
        }),
        _ => None,
    }
}

pub fn pick_preferred_impl_decl(sym: P<Symbol>, sig_opt: Option<P<Signature>>) -> Option<P<Node>> {
    let mut candidates = Vec::new();
    for &d in sym.declarations() {
        candidates.push(d);
    }
    if let Some(vd) = sym.value_declaration()
        && !candidates.contains(&vd)
    {
        candidates.push(vd);
    }
    if let Some(sig) = sig_opt
        && let Some(sig_decl) = sig
            .declaration()
            .or_else(|| sig.target().and_then(|t| t.declaration()))
    {
        if sig_decl.body().is_some() {
            return Some(sig_decl);
        }
        if !candidates.contains(&sig_decl) {
            candidates.push(sig_decl);
        }
    }

    for &decl in &candidates {
        if let Some(fn_node) = extract_callable_from_decl(decl)
            && fn_node.body().is_some()
        {
            return Some(decl);
        }
    }
    sym.value_declaration()
        .or_else(|| candidates.into_iter().next())
}

fn collect_local_rhs_for_symbol(
    checker: &mut CheckerHandle,
    sym: P<Symbol>,
    decl_node: P<Node>,
) -> Vec<P<Node>> {
    let mut rhs_list = Vec::new();
    if decl_node.kind() == Kind::VariableDeclaration
        && let Some(init) = decl_node.initializer()
    {
        rhs_list.push(init);
    }

    let scope_root = resolve_enclosing_function(decl_node)
        .or_else(|| get_source_file_of_node(decl_node).map(|sf| sf.as_node()));
    if let Some(root) = scope_root {
        collect_assignments_in_scope(checker, sym, root, true, &mut rhs_list);
    }
    rhs_list
}

fn collect_assignments_in_scope(
    checker: &mut CheckerHandle,
    target_sym: P<Symbol>,
    node: P<Node>,
    is_root: bool,
    out: &mut Vec<P<Node>>,
) {
    if !is_root && is_function_like_declaration(Some(node)) {
        return;
    }
    if node.kind() == Kind::BinaryExpression {
        let bin = node.as_binary_expression();
        if bin.operator_token().kind() == Kind::EqualsToken {
            let left = unwrap_expr_wrappers(bin.left());
            if left.kind() == Kind::Identifier
                && canonical_symbol_for_node(checker, left) == Some(target_sym)
            {
                out.push(bin.right());
            }
        }
    }
    node.for_each_child(&mut |child| {
        collect_assignments_in_scope(checker, target_sym, child, false, out);
        false
    });
}

fn symbol_name(sym_opt: Option<P<Symbol>>, decl_opt: Option<P<Node>>) -> Option<String> {
    if let Some(sym) = sym_opt {
        let name = sym.name();
        if !name.is_empty() && name != "__function" && name != "unknown" {
            return Some(name.to_string());
        }
    }
    decl_opt.and_then(|d| d.name().and_then(node_text))
}

const fn is_dynamic_signature_decl_kind(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Parameter
            | Kind::PropertySignature
            | Kind::MethodSignature
            | Kind::CallSignature
            | Kind::FunctionType
            | Kind::ConstructSignature
            | Kind::IndexSignature
    )
}

fn is_unresolved_sig(checker: &mut CheckerHandle, sig: P<Signature>) -> bool {
    if sig == checker.get_unknown_signature() {
        return true;
    }
    if sig.declaration().is_none()
        && sig.target().is_none()
        && sig.parameters().is_empty()
        && !sig.has_rest_parameter()
    {
        let ret = checker.get_return_type_of_signature_exported(sig);
        return ret == checker.get_any_type();
    }
    false
}

pub fn resolve_property_in_object_literal(obj_lit: P<Node>, prop_name: &str) -> Option<P<Node>> {
    for &prop in obj_lit.as_object_literal_expression().properties().nodes() {
        match prop.kind() {
            Kind::PropertyAssignment => {
                let pa = prop.as_property_assignment();
                if node_text(pa.name()).as_deref() == Some(prop_name) {
                    return Some(pa.initializer());
                }
            }
            Kind::ShorthandPropertyAssignment => {
                if let Some(name_node) = prop.name()
                    && node_text(name_node).as_deref() == Some(prop_name)
                {
                    return Some(name_node);
                }
            }
            Kind::MethodDeclaration => {
                if let Some(name_node) = prop.name()
                    && node_text(name_node).as_deref() == Some(prop_name)
                {
                    return Some(prop);
                }
            }
            _ => {}
        }
    }
    None
}

fn sort_key_for_node(cwd: &str, node: P<Node>) -> (String, u32, u32) {
    get_source_file_of_node(node).map_or_else(
        || (String::new(), 0, 0),
        |sf| {
            let rel = relative_to_cwd(cwd, sf.file_name());
            let loc = node_location(sf, &rel, node);
            (loc.file, loc.start, loc.end)
        },
    )
}

struct CalleeResolver<'a> {
    cwd: &'a str,
    program: &'static Program,
    checker: &'a mut CheckerHandle,
    call_loc: Option<SourceLocation>,
    visited_exprs: FxHashSet<P<Node>>,
    targets: Vec<ResolvedTargetDescriptor>,
    seen_targets: FxHashSet<P<Node>>,
    gaps: Vec<GapItem>,
}

impl<'a> CalleeResolver<'a> {
    fn new(
        cwd: &'a str,
        program: &'static Program,
        checker: &'a mut CheckerHandle,
        call_node: P<Node>,
    ) -> Self {
        let call_loc = node_location_rel(cwd, call_node);
        Self {
            cwd,
            program,
            checker,
            call_loc,
            visited_exprs: FxHashSet::default(),
            targets: Vec::new(),
            seen_targets: FxHashSet::default(),
            gaps: Vec::new(),
        }
    }

    fn push_gap(&mut self, kind: &'static str, message: String) {
        let dup = self
            .gaps
            .iter()
            .any(|g| g.kind == kind && g.message == message);
        if !dup {
            self.gaps.push(GapItem {
                kind,
                message,
                at: self.call_loc.clone(),
            });
        }
    }

    fn add_concrete_target(
        &mut self,
        decl_node: P<Node>,
        fn_node: Option<P<Node>>,
        symbol: Option<P<Symbol>>,
        reason: String,
    ) {
        if !self.seen_targets.insert(decl_node) {
            return;
        }
        let sf_opt = get_source_file_of_node(decl_node);
        let is_external = sf_opt.is_some_and(|sf| !is_project_source_file(self.program, sf));
        let is_default_library =
            sf_opt.is_some_and(|sf| self.program.is_source_file_default_library(sf.path()));
        let location = node_location_rel(self.cwd, decl_node);
        let name = symbol_name(symbol, Some(decl_node));

        self.targets.push(ResolvedTargetDescriptor {
            decl_node,
            fn_node,
            symbol,
            name,
            location,
            is_external,
            is_default_library,
            reason,
        });
    }

    fn resolve_expr(&mut self, raw_expr: P<Node>, via_alias: bool) {
        let expr = unwrap_expr_wrappers(raw_expr);
        if !self.visited_exprs.insert(expr) {
            return;
        }

        if is_function_like_declaration(Some(expr)) {
            let sym = canonical_symbol_for_node(self.checker, expr);
            let reason = if via_alias {
                "Resolved function literal via local binding".to_string()
            } else {
                "Direct function expression".to_string()
            };
            self.add_concrete_target(expr, Some(expr), sym, reason);
            return;
        }

        match expr.kind() {
            Kind::ConditionalExpression => {
                let cond = expr.as_conditional_expression();
                self.resolve_expr(cond.when_true(), true);
                self.resolve_expr(cond.when_false(), true);
                return;
            }
            Kind::BinaryExpression => {
                let bin = expr.as_binary_expression();
                let op = bin.operator_token().kind();
                if matches!(
                    op,
                    Kind::BarBarToken | Kind::QuestionQuestionToken | Kind::AmpersandAmpersandToken
                ) {
                    self.resolve_expr(bin.left(), true);
                    self.resolve_expr(bin.right(), true);
                    return;
                }
            }
            Kind::ElementAccessExpression => {
                let elem = expr.as_element_access_expression();
                let prop_key = unwrap_expr_wrappers(elem.argument_expression());
                if matches!(
                    prop_key.kind(),
                    Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
                ) {
                    let key_name = node_text(prop_key)
                        .map(|s| s.trim_matches(&['\'', '"', '`'][..]).to_string());
                    if let Some(name) = key_name
                        && self.try_resolve_object_prop(elem.expression(), &name)
                    {
                        return;
                    }
                }
                self.push_gap(
                    "dynamic-callee",
                    "Computed property call target cannot be statically resolved.".to_string(),
                );
                return;
            }
            Kind::PropertyAccessExpression => {
                let prop_acc = expr.as_property_access_expression();
                let prop_name = node_text(prop_acc.name());
                if let Some(ref name) = prop_name
                    && self.try_resolve_object_prop(prop_acc.expression(), name)
                {
                    return;
                }
            }
            _ => {}
        }

        if let Some(sym) = canonical_symbol_for_node(self.checker, expr)
            && let Some(decl) = pick_preferred_impl_decl(sym, None)
        {
            self.handle_symbol_decl(expr, sym, decl, via_alias);
            return;
        }

        let callee_ty = self.checker.get_type_at_location(expr);
        if callee_ty
            .flags()
            .intersects(TypeFlags::Any | TypeFlags::Unknown)
        {
            self.push_gap(
                "dynamic-callee",
                format!(
                    "Call target has dynamic '{}' type and cannot be statically resolved.",
                    if callee_ty.flags().intersects(TypeFlags::Any) {
                        "any"
                    } else {
                        "unknown"
                    }
                ),
            );
        } else {
            self.push_gap(
                "dynamic-callee",
                "Call target expression cannot be resolved to a static function declaration."
                    .to_string(),
            );
        }
    }

    fn try_resolve_object_prop(&mut self, obj_expr: P<Node>, prop_name: &str) -> bool {
        let unwrapped = unwrap_expr_wrappers(obj_expr);
        if unwrapped.kind() == Kind::ObjectLiteralExpression {
            if let Some(prop_val) = resolve_property_in_object_literal(unwrapped, prop_name) {
                self.resolve_expr(prop_val, true);
                return true;
            }
            return false;
        }
        if unwrapped.kind() == Kind::Identifier
            && let Some(sym) = canonical_symbol_for_node(self.checker, unwrapped)
            && let Some(decl) = pick_preferred_impl_decl(sym, None)
            && decl.kind() == Kind::VariableDeclaration
            && let Some(init) = decl.initializer()
        {
            let init_unwrapped = unwrap_expr_wrappers(init);
            if init_unwrapped.kind() == Kind::ObjectLiteralExpression
                && let Some(prop_val) =
                    resolve_property_in_object_literal(init_unwrapped, prop_name)
            {
                self.resolve_expr(prop_val, true);
                return true;
            }
        }
        false
    }

    fn handle_symbol_decl(
        &mut self,
        expr: P<Node>,
        sym: P<Symbol>,
        decl: P<Node>,
        via_alias: bool,
    ) {
        if is_function_like_declaration(Some(decl)) {
            let reason = if via_alias {
                "Resolved target via import or local alias".to_string()
            } else {
                "Direct symbol target".to_string()
            };
            self.add_concrete_target(decl, Some(decl), Some(sym), reason);
            return;
        }

        if matches!(
            decl.kind(),
            Kind::PropertyAssignment | Kind::PropertyDeclaration | Kind::ExportAssignment
        ) && let Some(fn_node) = extract_callable_from_decl(decl)
        {
            self.add_concrete_target(
                decl,
                Some(fn_node),
                Some(sym),
                "Resolved callable property or default export".to_string(),
            );
            return;
        }

        if decl.kind() == Kind::VariableDeclaration {
            let rhs_list = collect_local_rhs_for_symbol(self.checker, sym, decl);
            if !rhs_list.is_empty() {
                for rhs in rhs_list {
                    self.resolve_expr(rhs, true);
                }
                return;
            }
        }

        if is_dynamic_signature_decl_kind(decl.kind()) {
            if decl.kind() == Kind::Parameter {
                let rhs_list = collect_local_rhs_for_symbol(self.checker, sym, decl);
                for rhs in rhs_list {
                    self.resolve_expr(rhs, true);
                }
            }
            let text = node_text(expr).unwrap_or_else(|| "<callee>".to_string());
            self.push_gap(
                "dynamic-callee",
                format!(
                    "Call target \"{text}\" resolves to {:?} without a static implementation body.",
                    decl.kind()
                ),
            );
            return;
        }

        self.push_gap(
            "dynamic-callee",
            format!(
                "Call target resolves to {:?} which is not a statically known function.",
                decl.kind()
            ),
        );
    }

    fn finish(mut self, call_node: P<Node>) -> CallResolutionOutcome {
        if let Some(callee_expr) = call_node.expression() {
            self.resolve_expr(callee_expr, false);
        } else {
            self.push_gap(
                "dynamic-callee",
                "Call expression has no callee expression.".to_string(),
            );
        }

        // Fallback to checker's resolved signature when AST expression resolution didn't find a concrete target
        if self.targets.is_empty() {
            let sig = self.checker.get_resolved_signature_exported(call_node);
            if !is_unresolved_sig(self.checker, sig)
                && let Some(sig_decl) = sig
                    .declaration()
                    .or_else(|| sig.target().and_then(|t| t.declaration()))
                && !is_dynamic_signature_decl_kind(sig_decl.kind())
            {
                let sym = canonical_symbol_for_node(self.checker, sig_decl);
                let fn_node = extract_callable_from_decl(sig_decl);
                self.add_concrete_target(
                    sig_decl,
                    fn_node,
                    sym,
                    "Resolved via call signature declaration".to_string(),
                );
                self.gaps.clear();
            } else if let Some(missing_gap) =
                check_call_missing_package_gap(self.program, self.checker, self.cwd, call_node)
            {
                self.gaps.clear();
                self.gaps.push(missing_gap);
            }
        }

        let cwd = self.cwd;
        self.targets
            .sort_by_key(|t| sort_key_for_node(cwd, t.decl_node));

        CallResolutionOutcome {
            targets: self.targets,
            gaps: self.gaps,
        }
    }
}

pub fn resolve_call_targets(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    call_node: P<Node>,
) -> CallResolutionOutcome {
    CalleeResolver::new(cwd, program, checker, call_node).finish(call_node)
}

pub fn collect_return_expressions(fn_node: P<Node>) -> Vec<P<Node>> {
    let mut out = Vec::new();
    let Some(body) = fn_node.body() else {
        return out;
    };
    if body.kind() != Kind::Block {
        out.push(body);
        return out;
    }
    collect_returns_in_block(body, &mut out);
    out
}

fn collect_returns_in_block(node: P<Node>, out: &mut Vec<P<Node>>) {
    if is_function_like_declaration(Some(node)) {
        return;
    }
    if node.kind() == Kind::ReturnStatement {
        if let Some(expr) = node.as_return_statement().expression() {
            out.push(expr);
        }
        return;
    }
    node.for_each_child(&mut |child| {
        collect_returns_in_block(child, out);
        false
    });
}

pub fn get_or_build_call_graph(
    snapshot: &mut crate::serve::SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    lookup_files: Option<&[String]>,
) -> Arc<ProjectCallGraph> {
    let cache_key = scoped_project_cache_key(project_id, lookup_files);
    if let Some(cached) = snapshot.call_graph_cache.get(&cache_key) {
        return Arc::clone(cached);
    }

    let cwd = snapshot.cwd.clone();
    let lookup_file_set =
        lookup_files.map(|files| files.iter().map(String::as_str).collect::<FxHashSet<_>>());
    let mut source_files: Vec<(String, P<SourceFile>)> = program
        .get_source_files()
        .iter()
        .copied()
        .filter(|&sf| is_project_source_file(program, sf))
        .map(|sf| (relative_to_cwd(&cwd, sf.file_name()), sf))
        .filter(|(file, _)| {
            lookup_file_set
                .as_ref()
                .is_none_or(|lookup_files| lookup_files.contains(file.as_str()))
        })
        .collect();
    source_files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut sites = Vec::new();
    for (rel_file, sf) in source_files {
        collect_call_sites_in_node(
            &cwd,
            program,
            checker,
            sf,
            &rel_file,
            sf.as_node(),
            &mut sites,
        );
    }

    let mut by_symbol: FxHashMap<P<Symbol>, Vec<usize>> = FxHashMap::default();
    let mut by_decl: FxHashMap<P<Node>, Vec<usize>> = FxHashMap::default();
    for (idx, site) in sites.iter().enumerate() {
        for target in &site.resolution.targets {
            by_decl.entry(target.decl_node).or_default().push(idx);
            if let Some(sym) = target.symbol {
                by_symbol.entry(sym).or_default().push(idx);
                for &d in sym.declarations() {
                    by_decl.entry(d).or_default().push(idx);
                }
            }
        }
    }

    let graph = Arc::new(ProjectCallGraph {
        sites,
        by_symbol,
        by_decl,
    });
    snapshot
        .call_graph_cache
        .insert(cache_key, Arc::clone(&graph));
    graph
}

pub fn scoped_project_cache_key(project_id: &str, lookup_files: Option<&[String]>) -> String {
    let Some(files) = lookup_files else {
        return project_id.to_string();
    };
    let files_key = serde_json::to_string(files).expect("lookup file paths are serializable");
    format!("{project_id}\0{files_key}")
}

fn collect_call_sites_in_node(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    sf: P<SourceFile>,
    rel_file: &str,
    node: P<Node>,
    out: &mut Vec<IndexedCallSite>,
) {
    if matches!(node.kind(), Kind::CallExpression | Kind::NewExpression) {
        let location = node_location(sf, rel_file, node);
        let caller_decl = resolve_enclosing_function(node)
            .map(|fn_node| (fn_node, canonical_symbol_for_node(checker, fn_node)));
        let resolution = resolve_call_targets(cwd, program, checker, node);
        out.push(IndexedCallSite {
            call_node: node,
            location,
            caller_decl,
            resolution,
        });
    }

    node.for_each_child(&mut |child| {
        collect_call_sites_in_node(cwd, program, checker, sf, rel_file, child, out);
        false
    });
}

pub fn node_byte_span(node: P<Node>) -> (u32, u32) {
    let start = node.pos().source_offset().unwrap_or(0);
    let end = node.end().source_offset().unwrap_or(start).max(start);
    (start, end)
}

pub fn enclosing_block(node: P<Node>) -> Option<P<Node>> {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if matches!(p.kind(), Kind::Block | Kind::SourceFile) {
            return Some(p);
        }
        if is_function_like_declaration(Some(p)) {
            return p.body();
        }
        cur = p.parent();
    }
    None
}

pub fn is_direct_block_statement(node: P<Node>, block: P<Node>) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if p == block {
            return true;
        }
        if matches!(
            p.kind(),
            Kind::IfStatement
                | Kind::SwitchStatement
                | Kind::WhileStatement
                | Kind::DoStatement
                | Kind::ForStatement
                | Kind::ForInStatement
                | Kind::ForOfStatement
                | Kind::ConditionalExpression
                | Kind::TryStatement
                | Kind::CatchClause
        ) || is_function_like_declaration(Some(p))
        {
            return false;
        }
        if p.kind() == Kind::Block && p != block {
            return false;
        }
        cur = p.parent();
    }
    false
}

pub fn is_ancestor_of(ancestor: P<Node>, mut descendant: P<Node>) -> bool {
    loop {
        if descendant == ancestor {
            return true;
        }
        let Some(p) = descendant.parent() else {
            return false;
        };
        descendant = p;
    }
}

pub fn enclosing_loop(node: P<Node>) -> Option<P<Node>> {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if matches!(
            p.kind(),
            Kind::WhileStatement
                | Kind::DoStatement
                | Kind::ForStatement
                | Kind::ForInStatement
                | Kind::ForOfStatement
        ) {
            return Some(p);
        }
        if is_function_like_declaration(Some(p)) {
            return None;
        }
        cur = p.parent();
    }
    None
}

pub fn prop_key_text(node: P<Node>) -> Option<String> {
    if node.kind() == Kind::ComputedPropertyName {
        let expr = unwrap_expr_wrappers(node.expression()?);
        if matches!(
            expr.kind(),
            Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
        ) {
            return prop_key_text(expr);
        }
        return None;
    }
    let raw = node_text(node)?;
    if matches!(
        node.kind(),
        Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
    ) {
        Some(raw.trim_matches(&['\'', '"', '`'][..]).to_string())
    } else {
        Some(raw)
    }
}
