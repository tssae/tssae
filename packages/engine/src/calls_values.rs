use std::{
    collections::{BTreeSet, VecDeque},
    sync::Arc,
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use tsrs_ast::{
    Kind, Node, SourceFile, Symbol, SymbolFlags, get_source_file_of_node,
    is_function_like_declaration,
};
use tsrs_checker::{Signature, TypeFlags};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;

use crate::cfg::{EvidenceItem, GapItem, resolve_enclosing_function};
use crate::package_impl::{
    PackageImplCache, PackageImplLookup, ResolvedImplTargetPayload, check_call_missing_package_gap,
    resolve_impl_for_call_target,
};
use crate::projects::{is_project_source_file, relative_to_cwd};
use crate::serve::{SourceLocation, node_location, slice_source_text};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSiteParameterBindingPayload {
    pub argument_index: usize,
    pub argument: String,
    pub parameter_name: Option<String>,
    pub parameter_declaration: Option<String>,
    pub is_rest: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectCallInfoPayload {
    pub call: String,
    pub syntax: String,
    pub location: SourceLocation,
    pub caller_declaration: Option<String>,
    pub parameter_bindings: Vec<CallSiteParameterBindingPayload>,
    pub return_target: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationCallersRow {
    pub declaration: String,
    pub calls: Vec<String>,
    pub call_sites: Vec<DirectCallInfoPayload>,
    pub gaps: Vec<GapItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallTargetInfoPayload {
    pub declaration: String,
    pub name: Option<String>,
    pub location: Option<SourceLocation>,
    pub is_external: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation: Option<ResolvedImplTargetPayload>,
    pub parameter_bindings: Vec<CallSiteParameterBindingPayload>,
    pub return_expressions: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallTargetsRow {
    pub call: String,
    pub targets: Vec<String>,
    pub resolved_targets: Vec<CallTargetInfoPayload>,
    pub return_target: String,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

#[derive(Clone)]
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

fn build_parameter_bindings(
    snapshot: &mut crate::serve::SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    call_node: P<Node>,
    fn_node: Option<P<Node>>,
) -> Vec<CallSiteParameterBindingPayload> {
    let args = call_node.arguments();
    let params = fn_node.map_or(&[][..], |f| f.parameters());
    let mut bindings = Vec::with_capacity(args.len());

    for (idx, &arg) in args.iter().enumerate() {
        let arg_ref = snapshot.alloc_syntax(project_id, arg);
        let matched_param = params.get(idx).copied().or_else(|| {
            params.last().copied().filter(|last| {
                last.kind() == Kind::Parameter
                    && last
                        .as_parameter_declaration()
                        .dot_dot_dot_token()
                        .is_some()
            })
        });
        let (param_name, param_decl_ref, is_rest) =
            matched_param.map_or((None, None, false), |p| {
                let is_rest = p.kind() == Kind::Parameter
                    && p.as_parameter_declaration().dot_dot_dot_token().is_some();
                let sym = canonical_symbol_for_node(checker, p);
                let name = p
                    .name()
                    .and_then(node_text)
                    .or_else(|| sym.map(|s| s.name().to_string()));
                let decl_ref = snapshot.alloc_declaration(project_id, p, sym);
                (name, Some(decl_ref), is_rest)
            });
        bindings.push(CallSiteParameterBindingPayload {
            argument_index: idx,
            argument: arg_ref,
            parameter_name: param_name,
            parameter_declaration: param_decl_ref,
            is_rest,
        });
    }

    bindings
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

pub fn run_callers_of(
    snapshot: &mut crate::serve::SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
    lookup_files: Option<&[String]>,
) -> Result<Vec<DeclarationCallersRow>, crate::serve::RpcErr> {
    let mut decl_inputs = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let entry = snapshot.get_entry(project_id, handle_id)?;
        let crate::serve::HandlePayload::Declaration { node, symbol } = entry.payload else {
            return Err(crate::serve::RpcErr::new(
                "INVALID_REFERENCE_KIND",
                format!("Program reference {handle_id} is not a declaration reference."),
            ));
        };
        let canonical_sym = symbol
            .map(|s| unwrap_aliased_symbol(checker, s))
            .or_else(|| canonical_symbol_for_node(checker, node));
        decl_inputs.push((handle_id.clone(), node, canonical_sym));
    }

    let graph = get_or_build_call_graph(snapshot, project_id, program, checker, lookup_files);
    let mut rows = Vec::with_capacity(decl_inputs.len());

    for (decl_id, decl_node, sym_opt) in decl_inputs {
        let mut gaps = Vec::new();
        if is_dynamic_signature_decl_kind(decl_node.kind()) {
            let loc = node_location_rel(&snapshot.cwd, decl_node);
            gaps.push(GapItem {
                kind: "dynamic-callee",
                message: format!(
                    "Cannot exhaustively determine callers of {:?} without concrete dispatch targets.",
                    decl_node.kind()
                ),
                at: loc,
            });
        }

        let mut matching_indices = BTreeSet::new();
        if let Some(sym) = sym_opt
            && let Some(indices) = graph.by_symbol.get(&sym)
        {
            matching_indices.extend(indices.iter().copied());
        }
        if let Some(indices) = graph.by_decl.get(&decl_node) {
            matching_indices.extend(indices.iter().copied());
        }

        let mut calls = Vec::with_capacity(matching_indices.len());
        let mut call_sites = Vec::with_capacity(matching_indices.len());
        let target_fn_node = extract_callable_from_decl(decl_node);

        for idx in matching_indices {
            let site = &graph.sites[idx];
            let call_ref = snapshot.alloc_call(project_id, site.call_node);
            let syntax_ref = snapshot.alloc_syntax(project_id, site.call_node);
            let caller_declaration = site
                .caller_decl
                .map(|(fn_node, sym)| snapshot.alloc_declaration(project_id, fn_node, sym));
            let matched_fn = site
                .resolution
                .targets
                .iter()
                .find(|t| t.decl_node == decl_node || (sym_opt.is_some() && t.symbol == sym_opt))
                .and_then(|t| t.fn_node)
                .or(target_fn_node);
            let parameter_bindings =
                build_parameter_bindings(snapshot, project_id, checker, site.call_node, matched_fn);

            calls.push(call_ref.clone());
            call_sites.push(DirectCallInfoPayload {
                call: call_ref,
                syntax: syntax_ref.clone(),
                location: site.location.clone(),
                caller_declaration,
                parameter_bindings,
                return_target: syntax_ref,
            });
        }

        rows.push(DeclarationCallersRow {
            declaration: decl_id,
            calls,
            call_sites,
            gaps,
        });
    }

    Ok(rows)
}

#[expect(
    clippy::too_many_lines,
    reason = "Resolves direct call targets alongside cross-package implementation lookup and evidence."
)]
pub fn run_callees_of(
    snapshot: &mut crate::serve::SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
) -> Result<Vec<CallTargetsRow>, crate::serve::RpcErr> {
    let mut call_nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_call_node(project_id, handle_id)?;
        call_nodes.push((handle_id.clone(), node));
    }

    let cache = snapshot.package_impl_cache.clone();
    let cwd = snapshot.cwd.clone();
    let mut rows = Vec::with_capacity(call_nodes.len());
    for (call_id, call_node) in call_nodes {
        let mut outcome = resolve_call_targets(&cwd, program, checker, call_node);
        let return_target = snapshot.alloc_syntax(project_id, call_node);

        let mut target_refs = Vec::with_capacity(outcome.targets.len());
        let mut resolved_targets = Vec::with_capacity(outcome.targets.len());
        let mut evidence = Vec::new();

        for target in outcome.targets {
            let decl_ref = snapshot.alloc_declaration(project_id, target.decl_node, target.symbol);
            let is_decl_only_node = target.fn_node.is_none_or(|f| f.body().is_none())
                || get_source_file_of_node(target.decl_node)
                    .is_some_and(|sf| sf.is_declaration_file());

            let (effective_fn, implementation) = if is_decl_only_node {
                match resolve_impl_for_call_target(
                    &cache,
                    &cwd,
                    program,
                    checker,
                    call_node,
                    Some(target.decl_node),
                    target.fn_node,
                ) {
                    PackageImplLookup::ResolvedExternal { fn_node, info, .. }
                    | PackageImplLookup::SourceBody { fn_node, info } => {
                        evidence.push(EvidenceItem::simple(
                            "source",
                            format!(
                                "Resolved {} implementation '{}' for '{}'",
                                info.resolution_mode,
                                info.file,
                                target.name.as_deref().unwrap_or("<anonymous>")
                            ),
                            info.location.clone(),
                        ));
                        (Some(fn_node), Some(info))
                    }
                    PackageImplLookup::DeclarationOnly { message, .. } => {
                        outcome.gaps.push(GapItem {
                            kind: "missing-package-implementation",
                            message,
                            at: node_location_rel(&cwd, call_node),
                        });
                        (target.fn_node, None)
                    }
                    PackageImplLookup::MissingPackage { message, .. } => {
                        outcome.gaps.push(GapItem {
                            kind: "missing-package",
                            message,
                            at: node_location_rel(&cwd, call_node),
                        });
                        (target.fn_node, None)
                    }
                    PackageImplLookup::Ambiguous { message, .. } => {
                        outcome.gaps.push(GapItem {
                            kind: "ambiguous-package-implementation",
                            message,
                            at: node_location_rel(&cwd, call_node),
                        });
                        (target.fn_node, None)
                    }
                    PackageImplLookup::Unimplemented { .. } => (target.fn_node, None),
                }
            } else {
                let impl_info = match resolve_impl_for_call_target(
                    &cache,
                    &cwd,
                    program,
                    checker,
                    call_node,
                    Some(target.decl_node),
                    target.fn_node,
                ) {
                    PackageImplLookup::SourceBody { info, .. }
                    | PackageImplLookup::ResolvedExternal { info, .. } => Some(info),
                    _ => None,
                };
                (target.fn_node, impl_info)
            };

            let parameter_bindings =
                build_parameter_bindings(snapshot, project_id, checker, call_node, effective_fn);
            let return_expressions = effective_fn
                .map_or_default(collect_return_expressions)
                .into_iter()
                .map(|expr| snapshot.alloc_syntax(project_id, expr))
                .collect();

            if let Some(loc) = target.location.clone() {
                evidence.push(EvidenceItem::simple(
                    "source",
                    format!(
                        "{}: {}",
                        target.reason,
                        target.name.as_deref().unwrap_or("<anonymous>")
                    ),
                    Some(loc),
                ));
            }

            target_refs.push(decl_ref.clone());
            resolved_targets.push(CallTargetInfoPayload {
                declaration: decl_ref,
                name: target.name,
                location: target.location,
                is_external: target.is_external,
                implementation,
                parameter_bindings,
                return_expressions,
            });
        }

        rows.push(CallTargetsRow {
            call: call_id,
            targets: target_refs,
            resolved_targets,
            return_target,
            evidence,
            gaps: outcome.gaps,
        });
    }

    Ok(rows)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValuePropagationStepPayload {
    pub kind: &'static str,
    pub description: String,
    pub from_location: SourceLocation,
    pub to_location: SourceLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted_properties: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValuePropagationPathPayload {
    pub from: String,
    pub to: String,
    pub steps: Vec<ValuePropagationStepPayload>,
    pub evidence: Vec<EvidenceItem>,
    pub source_path: Vec<String>,
    pub omitted_properties: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValuePropagationTargetRow {
    pub from: String,
    pub to: String,
    pub reachable: bool,
    pub paths: Vec<ValuePropagationPathPayload>,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValuePropagationResultPayload {
    pub propagation: &'static str,
    pub results: Vec<ValuePropagationTargetRow>,
    pub gaps: Vec<GapItem>,
}

const MAX_PROP_STATES: usize = 4096;
const MAX_CALL_STACK_DEPTH: usize = 16;
const MAX_PATHS_PER_PAIR: usize = 8;

#[derive(Clone, PartialEq, Eq, Hash, Default)]
struct ValueFieldState {
    field_path: Vec<String>,
    source_path: Vec<String>,
    omitted_properties: BTreeSet<String>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TraceKey {
    node: P<Node>,
    fields: ValueFieldState,
    call_stack: Vec<P<Node>>,
    is_gap_tainted: bool,
}

#[derive(Clone)]
struct TraceQueueItem {
    key: TraceKey,
    steps: Vec<ValuePropagationStepPayload>,
    gap_indices: BTreeSet<usize>,
}

#[derive(Clone, Copy)]
struct EndpointSpec {
    node: P<Node>,
    symbol: Option<P<Symbol>>,
}

struct WriteSpan {
    block: Option<P<Node>>,
    start: u32,
    end: u32,
    is_unconditional: bool,
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

fn is_identifier_write_only(id_node: P<Node>) -> bool {
    let Some(parent) = id_node.parent() else {
        return false;
    };
    match parent.kind() {
        Kind::VariableDeclaration
        | Kind::Parameter
        | Kind::BindingElement
        | Kind::FunctionDeclaration
        | Kind::MethodDeclaration
        | Kind::ClassDeclaration
        | Kind::InterfaceDeclaration
        | Kind::TypeAliasDeclaration
        | Kind::ImportSpecifier
        | Kind::ImportClause
        | Kind::NamespaceImport
        | Kind::ExportSpecifier
        | Kind::PropertyAssignment => parent.name() == Some(id_node),
        Kind::PropertyAccessExpression => parent.as_property_access_expression().name == id_node,
        Kind::BinaryExpression => {
            let bin = parent.as_binary_expression();
            bin.operator_token().kind() == Kind::EqualsToken
                && unwrap_expr_wrappers(bin.left()) == id_node
        }
        _ => false,
    }
}

fn collect_writes_for_symbol_in_scope(
    checker: &mut CheckerHandle,
    sym: P<Symbol>,
    scope: P<Node>,
    is_root: bool,
    out: &mut Vec<WriteSpan>,
) {
    if !is_root && is_function_like_declaration(Some(scope)) {
        return;
    }
    if scope.kind() == Kind::VariableDeclaration
        && scope.initializer().is_some()
        && let Some(name_node) = scope.name()
        && name_node.kind() == Kind::Identifier
        && canonical_symbol_for_node(checker, name_node) == Some(sym)
    {
        let (start, end) = node_byte_span(scope);
        let blk = enclosing_block(scope);
        let is_unconditional = blk.is_some_and(|b| is_direct_block_statement(scope, b));
        out.push(WriteSpan {
            block: blk,
            start,
            end,
            is_unconditional,
        });
    } else if scope.kind() == Kind::BinaryExpression {
        let bin = scope.as_binary_expression();
        if bin.operator_token().kind() == Kind::EqualsToken {
            let lhs = unwrap_expr_wrappers(bin.left());
            if lhs.kind() == Kind::Identifier
                && canonical_symbol_for_node(checker, lhs) == Some(sym)
            {
                let (start, end) = node_byte_span(scope);
                let blk = enclosing_block(scope);
                let is_unconditional = blk.is_some_and(|b| is_direct_block_statement(scope, b));
                out.push(WriteSpan {
                    block: blk,
                    start,
                    end,
                    is_unconditional,
                });
            }
        }
    }
    scope.for_each_child(&mut |child| {
        collect_writes_for_symbol_in_scope(checker, sym, child, false, out);
        false
    });
}

fn collect_symbol_reads_in_subtree(
    checker: &mut CheckerHandle,
    sym: P<Symbol>,
    node: P<Node>,
    out: &mut Vec<P<Node>>,
) {
    if node.kind() == Kind::Identifier
        && !is_identifier_write_only(node)
        && canonical_symbol_for_node(checker, node) == Some(sym)
    {
        out.push(node);
    } else if node.kind() == Kind::ShorthandPropertyAssignment
        && let Some(name_node) = node.name()
        && canonical_symbol_for_node(checker, node) == Some(sym)
    {
        out.push(name_node);
    }
    node.for_each_child(&mut |child| {
        collect_symbol_reads_in_subtree(checker, sym, child, out);
        false
    });
}

fn collect_reachable_reads(
    checker: &mut CheckerHandle,
    sym: P<Symbol>,
    def_node: P<Node>,
) -> Vec<P<Node>> {
    let def_scope = resolve_enclosing_function(def_node)
        .or_else(|| get_source_file_of_node(def_node).map(|sf| sf.as_node()));
    let Some(scope) = def_scope else {
        return Vec::new();
    };

    let mut writes = Vec::new();
    collect_writes_for_symbol_in_scope(checker, sym, scope, true, &mut writes);

    let mut all_reads = Vec::new();
    collect_symbol_reads_in_subtree(checker, sym, scope, &mut all_reads);

    let (def_start, def_end) = node_byte_span(def_node);
    let def_loop = enclosing_loop(def_node);

    let mut out = Vec::new();
    for read in all_reads {
        let read_fn = resolve_enclosing_function(read);
        let def_fn = resolve_enclosing_function(def_node);
        let (read_start, _) = node_byte_span(read);

        if read_fn != def_fn {
            // Closure capture: check whether an unconditional write before the closure creation
            // already killed `def_node` in `def_fn`.
            let closure_anchor = read_fn.unwrap_or(read);
            let (closure_start, _) = node_byte_span(closure_anchor);
            let killed = writes.iter().any(|w| {
                w.is_unconditional
                    && w.start > def_end
                    && w.end <= closure_start
                    && w.block.is_some_and(|b| is_ancestor_of(b, closure_anchor))
            });
            if !killed {
                out.push(read);
            }
            continue;
        }

        if read_start < def_start {
            let same_loop = def_loop.is_some() && def_loop == enclosing_loop(read);
            if !same_loop {
                continue;
            }
        }

        let killed = writes.iter().any(|w| {
            w.is_unconditional
                && w.start > def_end
                && w.end <= read_start
                && w.block.is_some_and(|b| is_ancestor_of(b, read))
        });
        if !killed {
            out.push(read);
        }
    }

    out
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

fn is_decl_name_identifier(node: P<Node>) -> Option<P<Node>> {
    if node.kind() != Kind::Identifier {
        return None;
    }
    let parent = node.parent()?;
    if matches!(
        parent.kind(),
        Kind::VariableDeclaration | Kind::Parameter | Kind::BindingElement
    ) && parent.name() == Some(node)
    {
        return Some(parent);
    }
    None
}

fn make_step(
    cwd: &str,
    kind: &'static str,
    description: String,
    from_node: P<Node>,
    to_node: P<Node>,
    property: Option<String>,
    omitted_properties: Option<Vec<String>>,
) -> Option<ValuePropagationStepPayload> {
    let from_location = node_location_rel(cwd, from_node)?;
    let to_location = node_location_rel(cwd, to_node)?;
    Some(ValuePropagationStepPayload {
        kind,
        description,
        from_location,
        to_location,
        property,
        omitted_properties,
    })
}

struct PropagationRunner<'a> {
    snapshot: &'a mut crate::serve::SnapshotState,
    project_id: &'a str,
    cwd: String,
    program: &'static Program,
    checker: &'a mut CheckerHandle,
    lookup_files: Option<Vec<String>>,
    include_member_access: bool,
    call_graph: Option<Arc<ProjectCallGraph>>,
    package_impl_cache: PackageImplCache,
    all_gaps: Vec<GapItem>,
    target_endpoints: Vec<(String, EndpointSpec)>,
    found_paths: Vec<Vec<ValuePropagationPathPayload>>,
    target_gaps: Vec<BTreeSet<usize>>,
}

impl<'a> PropagationRunner<'a> {
    #[expect(
        clippy::too_many_arguments,
        reason = "Initializes propagation runner state for a batch of endpoints."
    )]
    fn new(
        snapshot: &'a mut crate::serve::SnapshotState,
        project_id: &'a str,
        program: &'static Program,
        checker: &'a mut CheckerHandle,
        lookup_files: Option<&[String]>,
        include_member_access: bool,
        package_impl_cache: PackageImplCache,
        target_endpoints: Vec<(String, EndpointSpec)>,
    ) -> Self {
        let n = target_endpoints.len();
        let cwd = snapshot.cwd.clone();
        Self {
            snapshot,
            project_id,
            cwd,
            program,
            checker,
            lookup_files: lookup_files.map(<[String]>::to_vec),
            include_member_access,
            call_graph: None,
            package_impl_cache,
            all_gaps: Vec::new(),
            target_endpoints,
            found_paths: (0..n).map(|_| Vec::new()).collect(),
            target_gaps: vec![BTreeSet::new(); n],
        }
    }

    fn call_graph(&mut self) -> Arc<ProjectCallGraph> {
        if let Some(graph) = &self.call_graph {
            return Arc::clone(graph);
        }
        let graph = get_or_build_call_graph(
            self.snapshot,
            self.project_id,
            self.program,
            self.checker,
            self.lookup_files.as_deref(),
        );
        self.call_graph = Some(Arc::clone(&graph));
        graph
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

    fn enqueue(
        queue: &mut VecDeque<TraceQueueItem>,
        visited: &mut FxHashSet<TraceKey>,
        key: TraceKey,
        steps: Vec<ValuePropagationStepPayload>,
        gap_indices: BTreeSet<usize>,
    ) {
        if visited.len() < MAX_PROP_STATES && visited.insert(key.clone()) {
            queue.push_back(TraceQueueItem {
                key,
                steps,
                gap_indices,
            });
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Queues one BFS transition with updated field state, call stack, step log, and gap indices."
    )]
    fn enqueue_step(
        &self,
        queue: &mut VecDeque<TraceQueueItem>,
        visited: &mut FxHashSet<TraceKey>,
        from_node: P<Node>,
        to_node: P<Node>,
        fields: ValueFieldState,
        call_stack: Vec<P<Node>>,
        kind: &'static str,
        description: String,
        property: Option<String>,
        omitted_properties: Option<Vec<String>>,
        mut steps: Vec<ValuePropagationStepPayload>,
        gap_indices: BTreeSet<usize>,
    ) {
        if let Some(step) = make_step(
            &self.cwd,
            kind,
            description,
            from_node,
            to_node,
            property,
            omitted_properties,
        ) {
            steps.push(step);
        }
        let is_gap_tainted = !gap_indices.is_empty();
        Self::enqueue(
            queue,
            visited,
            TraceKey {
                node: to_node,
                fields,
                call_stack,
                is_gap_tainted,
            },
            steps,
            gap_indices,
        );
    }

    fn check_matches(&mut self, from_id: &str, item: &TraceQueueItem) {
        if !item.key.fields.field_path.is_empty() {
            return;
        }
        if !self.include_member_access && !item.key.fields.source_path.is_empty() {
            return;
        }
        for (idx, (to_id, spec)) in self.target_endpoints.iter().enumerate() {
            let matched = item.key.node == spec.node
                || (spec.node.kind() == Kind::BindingElement
                    && spec.node.name() == Some(item.key.node));
            if !matched {
                continue;
            }
            for &gap_idx in &item.gap_indices {
                self.target_gaps[idx].insert(gap_idx);
            }
            if item.gap_indices.is_empty() && self.found_paths[idx].len() < MAX_PATHS_PER_PAIR {
                let mut evidence = Vec::with_capacity(item.steps.len() + 1);
                if let Some(first) = item.steps.first() {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        "Origin value starts here".to_string(),
                        Some(first.from_location.clone()),
                    ));
                } else if let Some(loc) = node_location_rel(&self.cwd, item.key.node) {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        "Origin value matches target directly".to_string(),
                        Some(loc),
                    ));
                }
                for step in &item.steps {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        step.description.clone(),
                        Some(step.to_location.clone()),
                    ));
                }
                self.found_paths[idx].push(ValuePropagationPathPayload {
                    from: from_id.to_string(),
                    to: to_id.clone(),
                    steps: item.steps.clone(),
                    evidence,
                    source_path: item.key.fields.source_path.clone(),
                    omitted_properties: item
                        .key
                        .fields
                        .omitted_properties
                        .iter()
                        .cloned()
                        .collect(),
                });
            }
        }
    }

    fn seed_from_endpoint(
        &mut self,
        from_spec: EndpointSpec,
        queue: &mut VecDeque<TraceQueueItem>,
        visited: &mut FxHashSet<TraceKey>,
    ) {
        let decl_like = is_decl_name_identifier(from_spec.node).unwrap_or(from_spec.node);
        if matches!(
            decl_like.kind(),
            Kind::VariableDeclaration | Kind::Parameter | Kind::BindingElement
        ) {
            let sym_opt = from_spec
                .symbol
                .or_else(|| canonical_symbol_for_node(self.checker, decl_like));
            Self::enqueue(
                queue,
                visited,
                TraceKey {
                    node: decl_like,
                    fields: ValueFieldState::default(),
                    call_stack: Vec::new(),
                    is_gap_tainted: false,
                },
                Vec::new(),
                BTreeSet::new(),
            );
            if let Some(sym) = sym_opt {
                for read in collect_reachable_reads(self.checker, sym, decl_like) {
                    self.enqueue_step(
                        queue,
                        visited,
                        decl_like,
                        read,
                        ValueFieldState::default(),
                        Vec::new(),
                        "alias",
                        format!("Read value from '{}'", sym.name()),
                        None,
                        None,
                        Vec::new(),
                        BTreeSet::new(),
                    );
                }
            }
            return;
        }

        if is_function_like_declaration(Some(from_spec.node)) {
            Self::enqueue(
                queue,
                visited,
                TraceKey {
                    node: from_spec.node,
                    fields: ValueFieldState::default(),
                    call_stack: Vec::new(),
                    is_gap_tainted: false,
                },
                Vec::new(),
                BTreeSet::new(),
            );
            if let Some(sym) = from_spec
                .symbol
                .or_else(|| canonical_symbol_for_node(self.checker, from_spec.node))
            {
                for read in collect_reachable_reads(self.checker, sym, from_spec.node) {
                    self.enqueue_step(
                        queue,
                        visited,
                        from_spec.node,
                        read,
                        ValueFieldState::default(),
                        Vec::new(),
                        "alias",
                        format!("Reference function '{}'", sym.name()),
                        None,
                        None,
                        Vec::new(),
                        BTreeSet::new(),
                    );
                }
            }
            return;
        }

        Self::enqueue(
            queue,
            visited,
            TraceKey {
                node: from_spec.node,
                fields: ValueFieldState::default(),
                call_stack: Vec::new(),
                is_gap_tainted: false,
            },
            Vec::new(),
            BTreeSet::new(),
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Dispatches value-preserving AST transitions in one self-contained BFS step."
    )]
    fn step_forward(
        &mut self,
        item: &TraceQueueItem,
        queue: &mut VecDeque<TraceQueueItem>,
        visited: &mut FxHashSet<TraceKey>,
        encountered_gaps: &mut BTreeSet<usize>,
    ) {
        let node = item.key.node;
        let Some(parent) = node.parent() else {
            return;
        };

        match parent.kind() {
            Kind::ParenthesizedExpression => {
                self.enqueue_step(
                    queue,
                    visited,
                    node,
                    parent,
                    item.key.fields.clone(),
                    item.key.call_stack.clone(),
                    "parenthesized",
                    "Value passes through parenthesized expression".to_string(),
                    None,
                    None,
                    item.steps.clone(),
                    item.gap_indices.clone(),
                );
            }
            Kind::AsExpression
            | Kind::TypeAssertionExpression
            | Kind::SatisfiesExpression
            | Kind::NonNullExpression => {
                if parent.expression() == Some(node) {
                    self.enqueue_step(
                        queue,
                        visited,
                        node,
                        parent,
                        item.key.fields.clone(),
                        item.key.call_stack.clone(),
                        "typeAssertion",
                        "Value passes through type assertion".to_string(),
                        None,
                        None,
                        item.steps.clone(),
                        item.gap_indices.clone(),
                    );
                }
            }
            Kind::AwaitExpression => {
                if parent.expression() == Some(node) {
                    self.enqueue_step(
                        queue,
                        visited,
                        node,
                        parent,
                        item.key.fields.clone(),
                        item.key.call_stack.clone(),
                        "awaitUnwrap",
                        "Value unwrapped via await".to_string(),
                        None,
                        None,
                        item.steps.clone(),
                        item.gap_indices.clone(),
                    );
                }
            }
            Kind::ConditionalExpression => {
                let cond = parent.as_conditional_expression();
                if cond.when_true() == node || cond.when_false() == node {
                    self.enqueue_step(
                        queue,
                        visited,
                        node,
                        parent,
                        item.key.fields.clone(),
                        item.key.call_stack.clone(),
                        "conditionalBranch",
                        "Value flows through conditional branch".to_string(),
                        None,
                        None,
                        item.steps.clone(),
                        item.gap_indices.clone(),
                    );
                }
            }
            Kind::BinaryExpression => {
                let bin = parent.as_binary_expression();
                let op = bin.operator_token().kind();
                if op == Kind::EqualsToken && bin.right() == node {
                    let lhs = unwrap_expr_wrappers(bin.left());
                    if lhs.kind() == Kind::Identifier
                        && let Some(sym) = canonical_symbol_for_node(self.checker, lhs)
                    {
                        self.enqueue_step(
                            queue,
                            visited,
                            node,
                            lhs,
                            item.key.fields.clone(),
                            item.key.call_stack.clone(),
                            "assignment",
                            format!("Assigned value to '{}'", sym.name()),
                            None,
                            None,
                            item.steps.clone(),
                            item.gap_indices.clone(),
                        );
                        if let Some(decl) = pick_preferred_impl_decl(sym, None) {
                            self.enqueue_step(
                                queue,
                                visited,
                                node,
                                decl,
                                item.key.fields.clone(),
                                item.key.call_stack.clone(),
                                "assignment",
                                format!("Assigned value to '{}'", sym.name()),
                                None,
                                None,
                                item.steps.clone(),
                                item.gap_indices.clone(),
                            );
                        }
                        for read in collect_reachable_reads(self.checker, sym, parent) {
                            self.enqueue_step(
                                queue,
                                visited,
                                node,
                                read,
                                item.key.fields.clone(),
                                item.key.call_stack.clone(),
                                "assignment",
                                format!("Read reassigned value of '{}'", sym.name()),
                                None,
                                None,
                                item.steps.clone(),
                                item.gap_indices.clone(),
                            );
                        }
                    }
                } else if matches!(
                    op,
                    Kind::BarBarToken | Kind::QuestionQuestionToken | Kind::AmpersandAmpersandToken
                ) && (bin.left() == node || bin.right() == node)
                {
                    self.enqueue_step(
                        queue,
                        visited,
                        node,
                        parent,
                        item.key.fields.clone(),
                        item.key.call_stack.clone(),
                        "conditionalBranch",
                        "Value flows through logical operator branch".to_string(),
                        None,
                        None,
                        item.steps.clone(),
                        item.gap_indices.clone(),
                    );
                }
            }
            Kind::VariableDeclaration => {
                if parent.initializer() == Some(node)
                    && let Some(name_node) = parent.name()
                {
                    self.bind_pattern(
                        node,
                        parent,
                        name_node,
                        &item.key.fields,
                        &item.key.call_stack,
                        &item.steps,
                        &item.gap_indices,
                        "variableInit",
                        None,
                        queue,
                        visited,
                    );
                }
            }
            Kind::PropertyAssignment => {
                let pa = parent.as_property_assignment();
                if pa.initializer() == node
                    && let Some(obj_lit) = parent.parent()
                    && obj_lit.kind() == Kind::ObjectLiteralExpression
                    && let Some(prop_name) = prop_key_text(pa.name())
                {
                    let mut next_fields = item.key.fields.clone();
                    next_fields.field_path.push(prop_name.clone());
                    self.enqueue_step(
                        queue,
                        visited,
                        node,
                        obj_lit,
                        next_fields,
                        item.key.call_stack.clone(),
                        "propertyWrite",
                        format!("Stored value in object property '{prop_name}'"),
                        Some(prop_name),
                        None,
                        item.steps.clone(),
                        item.gap_indices.clone(),
                    );
                }
            }
            Kind::ShorthandPropertyAssignment => {
                if parent.name() == Some(node)
                    && let Some(obj_lit) = parent.parent()
                    && obj_lit.kind() == Kind::ObjectLiteralExpression
                    && let Some(prop_name) = prop_key_text(node)
                {
                    let mut next_fields = item.key.fields.clone();
                    next_fields.field_path.push(prop_name.clone());
                    self.enqueue_step(
                        queue,
                        visited,
                        node,
                        obj_lit,
                        next_fields,
                        item.key.call_stack.clone(),
                        "propertyWrite",
                        format!("Stored value in shorthand property '{prop_name}'"),
                        Some(prop_name),
                        None,
                        item.steps.clone(),
                        item.gap_indices.clone(),
                    );
                }
            }
            Kind::SpreadAssignment => {
                if parent.as_spread_assignment().expression() == node
                    && let Some(obj_lit) = parent.parent()
                    && obj_lit.kind() == Kind::ObjectLiteralExpression
                    && !item.key.fields.field_path.is_empty()
                {
                    self.enqueue_step(
                        queue,
                        visited,
                        node,
                        obj_lit,
                        item.key.fields.clone(),
                        item.key.call_stack.clone(),
                        "alias",
                        "Spread object properties into literal".to_string(),
                        None,
                        None,
                        item.steps.clone(),
                        item.gap_indices.clone(),
                    );
                }
            }
            Kind::PropertyAccessExpression => {
                let prop_acc = parent.as_property_access_expression();
                if prop_acc.expression == node
                    && let Some(prop_name) = node_text(prop_acc.name)
                {
                    if !item.key.fields.field_path.is_empty() {
                        if item
                            .key
                            .fields
                            .field_path
                            .last()
                            .is_some_and(|k| k == &prop_name)
                        {
                            let mut next_fields = item.key.fields.clone();
                            next_fields.field_path.pop();
                            self.enqueue_step(
                                queue,
                                visited,
                                node,
                                parent,
                                next_fields,
                                item.key.call_stack.clone(),
                                "propertyRead",
                                format!("Read property '.{prop_name}'"),
                                Some(prop_name),
                                None,
                                item.steps.clone(),
                                item.gap_indices.clone(),
                            );
                        }
                    } else if self.include_member_access
                        && !item.key.fields.omitted_properties.contains(&prop_name)
                    {
                        let mut next_fields = item.key.fields.clone();
                        next_fields.source_path.push(prop_name.clone());
                        next_fields.omitted_properties.clear();
                        self.enqueue_step(
                            queue,
                            visited,
                            node,
                            parent,
                            next_fields,
                            item.key.call_stack.clone(),
                            "propertyRead",
                            format!("Read property '.{prop_name}'"),
                            Some(prop_name),
                            None,
                            item.steps.clone(),
                            item.gap_indices.clone(),
                        );
                    }
                }
            }
            Kind::ElementAccessExpression => {
                let elem = parent.as_element_access_expression();
                if elem.expression == node {
                    let arg = unwrap_expr_wrappers(elem.argument_expression);
                    if matches!(
                        arg.kind(),
                        Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
                    ) {
                        if let Some(prop_name) = prop_key_text(arg) {
                            if !item.key.fields.field_path.is_empty() {
                                if item
                                    .key
                                    .fields
                                    .field_path
                                    .last()
                                    .is_some_and(|k| k == &prop_name)
                                {
                                    let mut next_fields = item.key.fields.clone();
                                    next_fields.field_path.pop();
                                    self.enqueue_step(
                                        queue,
                                        visited,
                                        node,
                                        parent,
                                        next_fields,
                                        item.key.call_stack.clone(),
                                        "propertyRead",
                                        format!("Read element property ['{prop_name}']"),
                                        Some(prop_name),
                                        None,
                                        item.steps.clone(),
                                        item.gap_indices.clone(),
                                    );
                                }
                            } else if self.include_member_access
                                && !item.key.fields.omitted_properties.contains(&prop_name)
                            {
                                let mut next_fields = item.key.fields.clone();
                                next_fields.source_path.push(prop_name.clone());
                                next_fields.omitted_properties.clear();
                                self.enqueue_step(
                                    queue,
                                    visited,
                                    node,
                                    parent,
                                    next_fields,
                                    item.key.call_stack.clone(),
                                    "propertyRead",
                                    format!("Read element property ['{prop_name}']"),
                                    Some(prop_name),
                                    None,
                                    item.steps.clone(),
                                    item.gap_indices.clone(),
                                );
                            }
                        }
                    } else if !item.key.fields.field_path.is_empty()
                        && let Some(loc) = node_location_rel(&self.cwd, parent)
                    {
                        let gap_idx = self.record_gap(GapItem {
                            kind: "dynamic-property-access",
                            message: "Computed element access prevents static property tracking."
                                .to_string(),
                            at: Some(loc),
                        });
                        encountered_gaps.insert(gap_idx);
                    }
                }
            }
            Kind::ReturnStatement => {
                if parent.expression() == Some(node)
                    && let Some(fn_node) = resolve_enclosing_function(parent)
                {
                    self.step_return(node, fn_node, item, queue, visited);
                }
            }
            Kind::ArrowFunction => {
                if parent.body() == Some(node) {
                    self.step_return(node, parent, item, queue, visited);
                }
            }
            Kind::CallExpression | Kind::NewExpression => {
                if let Some(arg_index) = parent.arguments().iter().position(|&a| a == node) {
                    self.step_call_argument(
                        node,
                        parent,
                        arg_index,
                        item,
                        queue,
                        visited,
                        encountered_gaps,
                    );
                }
            }
            _ => {}
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Binding propagation handles identifier and object/rest destructuring patterns uniformly."
    )]
    fn bind_pattern(
        &mut self,
        source_node: P<Node>,
        decl_node: P<Node>,
        pattern_node: P<Node>,
        fields: &ValueFieldState,
        call_stack: &[P<Node>],
        steps: &[ValuePropagationStepPayload],
        gap_indices: &BTreeSet<usize>,
        step_kind: &'static str,
        step_property: Option<String>,
        queue: &mut VecDeque<TraceQueueItem>,
        visited: &mut FxHashSet<TraceKey>,
    ) {
        if pattern_node.kind() == Kind::Identifier {
            if let Some(sym) = canonical_symbol_for_node(self.checker, pattern_node) {
                let desc = if step_kind == "destructure"
                    && let Some(prop) = step_property.as_deref()
                {
                    format!("Destructured property '{prop}' to '{}'", sym.name())
                } else {
                    format!("Bound value to '{}'", sym.name())
                };
                self.enqueue_step(
                    queue,
                    visited,
                    source_node,
                    decl_node,
                    fields.clone(),
                    call_stack.to_vec(),
                    step_kind,
                    desc.clone(),
                    step_property.clone(),
                    None,
                    steps.to_vec(),
                    gap_indices.clone(),
                );
                self.enqueue_step(
                    queue,
                    visited,
                    source_node,
                    pattern_node,
                    fields.clone(),
                    call_stack.to_vec(),
                    step_kind,
                    desc.clone(),
                    step_property.clone(),
                    None,
                    steps.to_vec(),
                    gap_indices.clone(),
                );
                let (read_from, read_kind, read_desc, read_steps) = if step_kind == "destructure" {
                    let mut next_steps = steps.to_vec();
                    if let Some(step) = make_step(
                        &self.cwd,
                        "destructure",
                        desc,
                        source_node,
                        decl_node,
                        step_property,
                        None,
                    ) {
                        next_steps.push(step);
                    }
                    (
                        decl_node,
                        "alias",
                        format!("Read destructured value '{}'", sym.name()),
                        next_steps,
                    )
                } else {
                    (
                        source_node,
                        step_kind,
                        format!("Read bound value '{}'", sym.name()),
                        steps.to_vec(),
                    )
                };
                for read in collect_reachable_reads(self.checker, sym, decl_node) {
                    self.enqueue_step(
                        queue,
                        visited,
                        read_from,
                        read,
                        fields.clone(),
                        call_stack.to_vec(),
                        read_kind,
                        read_desc.clone(),
                        None,
                        None,
                        read_steps.clone(),
                        gap_indices.clone(),
                    );
                }
            }
            return;
        }

        if pattern_node.kind() != Kind::ObjectBindingPattern {
            return;
        }

        let elements = pattern_node.elements();
        let mut omitted_key_set = BTreeSet::new();
        for &elem in elements {
            if elem.kind() != Kind::BindingElement {
                continue;
            }
            let be = elem.as_binding_element();
            if be.dot_dot_dot_token().is_some() {
                continue;
            }
            if let Some(key) = be
                .property_name()
                .and_then(prop_key_text)
                .or_else(|| be.name().and_then(node_text))
            {
                omitted_key_set.insert(key);
            }
        }
        let omitted_keys: Vec<String> = omitted_key_set.into_iter().collect();

        for &elem in elements {
            if elem.kind() != Kind::BindingElement {
                continue;
            }
            let be = elem.as_binding_element();
            let Some(target_name) = be.name() else {
                continue;
            };
            if be.dot_dot_dot_token().is_some() {
                let survives = fields
                    .field_path
                    .last()
                    .is_none_or(|k| !omitted_keys.iter().any(|om| om == k));
                if !survives {
                    continue;
                }
                let mut next_fields = fields.clone();
                next_fields
                    .omitted_properties
                    .extend(omitted_keys.iter().cloned());
                let omitted_desc = if omitted_keys.is_empty() {
                    "Rest destructuring preserves remaining object properties".to_string()
                } else {
                    format!(
                        "Rest destructuring preserves remaining properties (omitting {})",
                        omitted_keys.join(", ")
                    )
                };
                if target_name.kind() == Kind::Identifier
                    && let Some(sym) = canonical_symbol_for_node(self.checker, target_name)
                {
                    self.enqueue_step(
                        queue,
                        visited,
                        source_node,
                        elem,
                        next_fields.clone(),
                        call_stack.to_vec(),
                        "restDestructure",
                        omitted_desc.clone(),
                        None,
                        Some(omitted_keys.clone()),
                        steps.to_vec(),
                        gap_indices.clone(),
                    );
                    self.enqueue_step(
                        queue,
                        visited,
                        source_node,
                        target_name,
                        next_fields.clone(),
                        call_stack.to_vec(),
                        "restDestructure",
                        omitted_desc.clone(),
                        None,
                        Some(omitted_keys.clone()),
                        steps.to_vec(),
                        gap_indices.clone(),
                    );
                    let mut steps_with_rest = steps.to_vec();
                    if let Some(rest_step) = make_step(
                        &self.cwd,
                        "restDestructure",
                        omitted_desc,
                        source_node,
                        elem,
                        None,
                        Some(omitted_keys.clone()),
                    ) {
                        steps_with_rest.push(rest_step);
                    }
                    for read in collect_reachable_reads(self.checker, sym, elem) {
                        self.enqueue_step(
                            queue,
                            visited,
                            elem,
                            read,
                            next_fields.clone(),
                            call_stack.to_vec(),
                            "restDestructure",
                            format!("Read rest-destructured '{}'", sym.name()),
                            None,
                            None,
                            steps_with_rest.clone(),
                            gap_indices.clone(),
                        );
                    }
                }
                continue;
            }

            let Some(key) = be.property_name().and_then(prop_key_text).or_else(|| {
                target_name
                    .kind()
                    .eq(&Kind::Identifier)
                    .then(|| node_text(target_name))
                    .flatten()
            }) else {
                continue;
            };
            let next_fields = if !fields.field_path.is_empty() {
                if fields.field_path.last().is_some_and(|k| k == &key) {
                    let mut nf = fields.clone();
                    nf.field_path.pop();
                    Some(nf)
                } else {
                    None
                }
            } else if self.include_member_access && !fields.omitted_properties.contains(&key) {
                let mut nf = fields.clone();
                nf.source_path.push(key.clone());
                nf.omitted_properties.clear();
                Some(nf)
            } else {
                None
            };
            if let Some(next_fields) = next_fields {
                if target_name.kind() == Kind::Identifier {
                    self.bind_pattern(
                        source_node,
                        elem,
                        target_name,
                        &next_fields,
                        call_stack,
                        steps,
                        gap_indices,
                        "destructure",
                        Some(key),
                        queue,
                        visited,
                    );
                } else {
                    let mut steps_with_destructure = steps.to_vec();
                    if let Some(step) = make_step(
                        &self.cwd,
                        "destructure",
                        format!("Destructured property '{key}'"),
                        source_node,
                        elem,
                        Some(key),
                        None,
                    ) {
                        steps_with_destructure.push(step);
                    }
                    self.bind_pattern(
                        elem,
                        elem,
                        target_name,
                        &next_fields,
                        call_stack,
                        &steps_with_destructure,
                        gap_indices,
                        "destructure",
                        None,
                        queue,
                        visited,
                    );
                }
            }
        }
    }

    fn step_return(
        &mut self,
        ret_expr: P<Node>,
        fn_node: P<Node>,
        item: &TraceQueueItem,
        queue: &mut VecDeque<TraceQueueItem>,
        visited: &mut FxHashSet<TraceKey>,
    ) {
        let fn_name = symbol_name(
            canonical_symbol_for_node(self.checker, fn_node),
            Some(fn_node),
        )
        .unwrap_or_else(|| "<anonymous>".to_string());

        if let Some((&top_call, rest_stack)) = item.key.call_stack.split_last() {
            self.enqueue_step(
                queue,
                visited,
                ret_expr,
                top_call,
                item.key.fields.clone(),
                rest_stack.to_vec(),
                "returnValue",
                format!("Return value from '{fn_name}' to call site"),
                None,
                None,
                item.steps.clone(),
                item.gap_indices.clone(),
            );
            return;
        }

        let fn_sym = canonical_symbol_for_node(self.checker, fn_node);
        let graph = self.call_graph();
        let mut call_indices = BTreeSet::new();
        if let Some(sym) = fn_sym
            && let Some(indices) = graph.by_symbol.get(&sym)
        {
            call_indices.extend(indices.iter().copied());
        }
        if let Some(indices) = graph.by_decl.get(&fn_node) {
            call_indices.extend(indices.iter().copied());
        }
        let call_nodes = call_indices
            .into_iter()
            .map(|idx| graph.sites[idx].call_node)
            .collect::<Vec<_>>();
        for call_node in call_nodes {
            self.enqueue_step(
                queue,
                visited,
                ret_expr,
                call_node,
                item.key.fields.clone(),
                Vec::new(),
                "returnValue",
                format!("Return value from '{fn_name}' to caller"),
                None,
                None,
                item.steps.clone(),
                item.gap_indices.clone(),
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Call argument transition resolves package implementations, records gaps, and updates call stack."
    )]
    fn step_call_argument(
        &mut self,
        arg_node: P<Node>,
        call_node: P<Node>,
        arg_index: usize,
        item: &TraceQueueItem,
        queue: &mut VecDeque<TraceQueueItem>,
        visited: &mut FxHashSet<TraceKey>,
        encountered_gaps: &mut BTreeSet<usize>,
    ) {
        let resolution = resolve_call_targets(&self.cwd, self.program, self.checker, call_node);
        let return_used = call_node
            .parent()
            .is_some_and(|p| p.kind() != Kind::ExpressionStatement);

        for gap in resolution.gaps {
            let gap_idx = self.record_gap(gap);
            encountered_gaps.insert(gap_idx);
            let mut next_gaps = item.gap_indices.clone();
            next_gaps.insert(gap_idx);
            Self::enqueue(
                queue,
                visited,
                TraceKey {
                    node: call_node,
                    fields: item.key.fields.clone(),
                    call_stack: item.key.call_stack.clone(),
                    is_gap_tainted: !next_gaps.is_empty(),
                },
                item.steps.clone(),
                next_gaps,
            );
        }

        for target in resolution.targets {
            let resolved_impl_fn = if let Some(fn_node) = target.fn_node
                && fn_node.body().is_some()
                && !get_source_file_of_node(fn_node).is_some_and(|sf| sf.is_declaration_file())
            {
                Ok(fn_node)
            } else if target.is_default_library && !return_used {
                continue;
            } else {
                match resolve_impl_for_call_target(
                    &self.package_impl_cache,
                    &self.cwd,
                    self.program,
                    self.checker,
                    call_node,
                    Some(target.decl_node),
                    target.fn_node,
                ) {
                    PackageImplLookup::ResolvedExternal { fn_node, .. }
                    | PackageImplLookup::SourceBody { fn_node, .. } => Ok(fn_node),
                    PackageImplLookup::MissingPackage { message, .. } => {
                        Err(("missing-package", message))
                    }
                    PackageImplLookup::Ambiguous { message, .. } => {
                        Err(("ambiguous-package-implementation", message))
                    }
                    PackageImplLookup::DeclarationOnly { message, .. } => {
                        Err(("missing-function-body", message))
                    }
                    PackageImplLookup::Unimplemented { .. } => {
                        let name = target.name.as_deref().unwrap_or("<callee>");
                        Err((
                            "missing-function-body",
                            format!(
                                "Callee '{name}' has no implementation body; value propagation across this call cannot be verified."
                            ),
                        ))
                    }
                }
            };

            match resolved_impl_fn {
                Ok(fn_node) => {
                    if item.key.call_stack.len() >= MAX_CALL_STACK_DEPTH {
                        continue;
                    }
                    let params = fn_node.parameters();
                    let Some(param) = params.get(arg_index).copied() else {
                        continue;
                    };
                    if param.kind() == Kind::Parameter
                        && param
                            .as_parameter_declaration()
                            .dot_dot_dot_token()
                            .is_some()
                    {
                        continue;
                    }
                    let mut next_stack = item.key.call_stack.clone();
                    next_stack.push(call_node);
                    if let Some(param_name_node) = param.name() {
                        self.bind_pattern(
                            arg_node,
                            param,
                            param_name_node,
                            &item.key.fields,
                            &next_stack,
                            &item.steps,
                            &item.gap_indices,
                            "callArgument",
                            None,
                            queue,
                            visited,
                        );
                    }
                }
                Err((kind, message)) => {
                    let loc = node_location_rel(&self.cwd, call_node);
                    let gap_idx = self.record_gap(GapItem {
                        kind,
                        message,
                        at: loc,
                    });
                    encountered_gaps.insert(gap_idx);
                    let mut next_gaps = item.gap_indices.clone();
                    next_gaps.insert(gap_idx);
                    Self::enqueue(
                        queue,
                        visited,
                        TraceKey {
                            node: call_node,
                            fields: item.key.fields.clone(),
                            call_stack: item.key.call_stack.clone(),
                            is_gap_tainted: !next_gaps.is_empty(),
                        },
                        item.steps.clone(),
                        next_gaps,
                    );
                }
            }
        }
    }

    fn trace_from(
        &mut self,
        from_id: &str,
        from_spec: EndpointSpec,
        to_ids: &[String],
    ) -> Vec<ValuePropagationTargetRow> {
        self.found_paths.iter_mut().for_each(Vec::clear);
        self.target_gaps.iter_mut().for_each(BTreeSet::clear);

        let mut queue = VecDeque::new();
        let mut visited = FxHashSet::default();
        let mut encountered_gaps = BTreeSet::new();

        self.seed_from_endpoint(from_spec, &mut queue, &mut visited);

        while let Some(item) = queue.pop_front() {
            self.check_matches(from_id, &item);
            self.step_forward(&item, &mut queue, &mut visited, &mut encountered_gaps);
        }

        let from_fn = resolve_enclosing_function(from_spec.node);

        let mut rows = Vec::with_capacity(self.target_endpoints.len());
        for (idx, to_id) in to_ids.iter().enumerate() {
            let paths = std::mem::take(&mut self.found_paths[idx]);
            let reachable = !paths.is_empty();
            let evidence = paths.first().map_or_default(|p| p.evidence.clone());

            let mut gap_set = std::mem::take(&mut self.target_gaps[idx]);
            if !reachable {
                let to_fn = resolve_enclosing_function(self.target_endpoints[idx].1.node);
                if to_fn != from_fn {
                    gap_set.extend(encountered_gaps.iter().copied());
                }
            }
            let gaps = gap_set
                .into_iter()
                .filter_map(|gi| self.all_gaps.get(gi).cloned())
                .collect();

            rows.push(ValuePropagationTargetRow {
                from: from_id.to_string(),
                to: to_id.clone(),
                reachable,
                paths,
                evidence,
                gaps,
            });
        }

        rows
    }
}

fn resolve_endpoint_spec(
    snapshot: &crate::serve::SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    handle_id: &str,
) -> Result<EndpointSpec, crate::serve::RpcErr> {
    let entry = snapshot.get_entry(project_id, handle_id)?;
    match entry.payload {
        crate::serve::HandlePayload::Syntax(node) | crate::serve::HandlePayload::Call(node) => {
            let symbol = canonical_symbol_for_node(checker, node);
            Ok(EndpointSpec { node, symbol })
        }
        crate::serve::HandlePayload::Declaration { node, symbol } => {
            let sym = symbol
                .map(|s| unwrap_aliased_symbol(checker, s))
                .or_else(|| canonical_symbol_for_node(checker, node));
            Ok(EndpointSpec { node, symbol: sym })
        }
        crate::serve::HandlePayload::Signature { .. }
        | crate::serve::HandlePayload::Type { .. }
        | crate::serve::HandlePayload::Branch(_)
        | crate::serve::HandlePayload::Instance(_) => Err(crate::serve::RpcErr::new(
            "INVALID_REFERENCE_KIND",
            format!(
                "Program reference {handle_id} cannot be used as a value propagation endpoint."
            ),
        )),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Forwards propagation endpoints, mode, and reverse-search file scope to one query runner."
)]
pub fn run_value_propagation(
    snapshot: &mut crate::serve::SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    propagation: Option<&str>,
    include_member_access: bool,
    from_handles: &[String],
    to_handles: &[String],
    lookup_files: Option<&[String]>,
) -> Result<ValuePropagationResultPayload, crate::serve::RpcErr> {
    if let Some(mode) = propagation
        && mode != "value"
    {
        return Err(crate::serve::RpcErr::new(
            "INVALID_PARAMS",
            format!("Unsupported propagation mode: {mode}"),
        ));
    }

    let mut from_specs = Vec::with_capacity(from_handles.len());
    for handle_id in from_handles {
        from_specs.push((
            handle_id.clone(),
            resolve_endpoint_spec(snapshot, project_id, checker, handle_id)?,
        ));
    }

    let mut to_specs = Vec::with_capacity(to_handles.len());
    let mut to_ids = Vec::with_capacity(to_handles.len());
    for handle_id in to_handles {
        let spec = resolve_endpoint_spec(snapshot, project_id, checker, handle_id)?;
        to_ids.push(handle_id.clone());
        to_specs.push((handle_id.clone(), spec));
    }

    let package_impl_cache = snapshot.package_impl_cache.clone();
    let mut runner = PropagationRunner::new(
        snapshot,
        project_id,
        program,
        checker,
        lookup_files,
        include_member_access,
        package_impl_cache,
        to_specs,
    );

    let mut results = Vec::with_capacity(from_specs.len() * to_ids.len());
    for (from_id, from_spec) in from_specs {
        results.extend(runner.trace_from(&from_id, from_spec, &to_ids));
    }

    Ok(ValuePropagationResultPayload {
        propagation: "value",
        results,
        gaps: runner.all_gaps,
    })
}
