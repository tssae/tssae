use std::{collections::BTreeMap, sync::Arc};

use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use tsrs_ast::{
    Kind, Node, SourceFile, Symbol, get_source_file_of_node, is_function_like_declaration,
};
use tsrs_checker::{TypeFlags, TypeFormatFlags};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;

use crate::calls_values::{
    ProjectCallGraph, canonical_symbol_for_node, collect_return_expressions, enclosing_block,
    enclosing_loop, extract_callable_from_decl, get_or_build_call_graph, is_ancestor_of,
    is_direct_block_statement, node_byte_span, node_location_rel, node_text,
    pick_preferred_impl_decl, prop_key_text, resolve_call_targets,
    resolve_property_in_object_literal, scoped_project_cache_key, unwrap_expr_wrappers,
};
use crate::cfg::{EvidenceItem, GapItem, resolve_enclosing_function};
use crate::package_impl::{
    PackageImplCache, PackageImplLookup, check_call_missing_package_gap,
    resolve_impl_for_call_target,
};
use crate::projects::{is_project_source_file, relative_to_cwd};
use crate::serve::{RpcErr, SnapshotState, SourceLocation, node_location};

const MAX_TRACE_DEPTH: usize = 24;
const MAX_CALL_STACK_DEPTH: usize = 12;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceModelMatchInput {
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub member: Option<String>,
    #[serde(default)]
    pub module: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceModelBehaviorInput {
    pub kind: String,
    #[serde(rename = "match")]
    pub match_rule: InstanceModelMatchInput,
    #[serde(default)]
    pub argument_index: Option<usize>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceModelInput {
    pub id: String,
    #[serde(default)]
    pub instance_behaviors: Vec<InstanceModelBehaviorInput>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "JSON projection selection flags map 1:1 to SDK select keys."
)]
pub struct InstanceSelect {
    #[serde(default)]
    pub instance_key: bool,
    #[serde(default)]
    pub origin_kind: bool,
    #[serde(default)]
    pub allocation_location: bool,
    #[serde(default)]
    pub origin_syntax: bool,
    #[serde(default)]
    pub model_id: bool,
    #[serde(default)]
    pub evidence: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedInstanceInfoPayload {
    #[serde(rename = "ref")]
    pub instance_ref: String,
    pub instance_key: String,
    pub origin_kind: &'static str,
    pub allocation_location: Option<SourceLocation>,
    pub origin_syntax: Option<String>,
    pub model_id: Option<String>,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceResolutionRow {
    pub target: String,
    pub instance: Option<String>,
    pub instance_key: Option<String>,
    pub allocation_location: Option<SourceLocation>,
    pub origin_kind: Option<&'static str>,
    pub instances: Vec<ResolvedInstanceInfoPayload>,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

#[derive(Default)]
pub enum ProjectedInstanceField<T> {
    #[default]
    Omitted,
    Value(Option<T>),
}

impl<T> ProjectedInstanceField<T> {
    const fn is_omitted(&self) -> bool {
        matches!(self, Self::Omitted)
    }

    fn from_selected(selected: bool, value: Option<T>) -> Self {
        if selected {
            Self::Value(value)
        } else {
            Self::Omitted
        }
    }
}

impl<T: Serialize> Serialize for ProjectedInstanceField<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Omitted => serializer.serialize_none(),
            Self::Value(val) => val.serialize(serializer),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceProjectionRow {
    pub instance: String,
    pub gaps: Vec<GapItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "ProjectedInstanceField::is_omitted")]
    pub allocation_location: ProjectedInstanceField<SourceLocation>,
    #[serde(skip_serializing_if = "ProjectedInstanceField::is_omitted")]
    pub origin_syntax: ProjectedInstanceField<String>,
    #[serde(skip_serializing_if = "ProjectedInstanceField::is_omitted")]
    pub model_id: ProjectedInstanceField<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<EvidenceItem>>,
}

#[derive(Clone)]
pub struct InstanceHandleData {
    pub instance_key: String,
    pub origin_kind: &'static str,
    pub allocation_location: Option<SourceLocation>,
    pub origin_syntax: Option<P<Node>>,
    pub model_id: Option<String>,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Clone)]
pub struct FieldWriteSite {
    pub assign_node: P<Node>,
    pub recv_node: P<Node>,
    pub rhs_node: P<Node>,
    pub enclosing_fn: Option<P<Node>>,
    pub enclosing_class: Option<P<Node>>,
    pub start: u32,
    pub end: u32,
    pub is_unconditional: bool,
    pub block: Option<P<Node>>,
}

pub struct FieldWriteIndex {
    pub writes_by_field: BTreeMap<String, Vec<FieldWriteSite>>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CallFrame {
    pub call_node: P<Node>,
    pub callee_fn: Option<P<Node>>,
    pub receiver_expr: Option<P<Node>>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct VisitKey {
    node: P<Node>,
    call_stack: Vec<P<Node>>,
    field_name: Option<String>,
}

#[derive(Clone)]
pub struct ResolvedOrigin {
    pub instance_key: String,
    pub origin_kind: &'static str,
    pub origin_node: Option<P<Node>>,
    pub allocation_location: Option<SourceLocation>,
    pub model_id: Option<String>,
    pub evidence: Vec<EvidenceItem>,
    pub captured_stack: Vec<CallFrame>,
}

#[derive(Default)]
pub struct TraceOutcome {
    pub origins: Vec<ResolvedOrigin>,
    pub gaps: Vec<GapItem>,
}

impl TraceOutcome {
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

    fn merge(&mut self, other: Self) {
        for origin in other.origins {
            if !self
                .origins
                .iter()
                .any(|existing| existing.instance_key == origin.instance_key)
            {
                self.origins.push(origin);
            }
        }
        for gap in other.gaps {
            self.push_gap(gap);
        }
    }
}

fn format_node_span_key(cwd: &str, node: P<Node>) -> String {
    get_source_file_of_node(node).map_or_else(
        || {
            let (start, end) = node_byte_span(node);
            format!("<unknown>:{start}..{end}")
        },
        |sf| {
            let rel = relative_to_cwd(cwd, sf.file_name());
            let loc = node_location(sf, &rel, node);
            format!("{}:{}..{}", loc.file, loc.start, loc.end)
        },
    )
}

fn format_context_suffix(cwd: &str, call_stack: &[CallFrame]) -> String {
    if call_stack.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = call_stack
        .iter()
        .map(|frame| format_node_span_key(cwd, frame.call_node))
        .collect();
    format!("@ctx[{}]", parts.join("->"))
}

fn format_alloc_key(cwd: &str, prefix: &str, node: P<Node>, call_stack: &[CallFrame]) -> String {
    let span_key = format_node_span_key(cwd, node);
    let ctx = format_context_suffix(cwd, call_stack);
    format!("{prefix}:{span_key}{ctx}")
}

fn resolve_enclosing_class(node: P<Node>) -> Option<P<Node>> {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if matches!(p.kind(), Kind::ClassDeclaration | Kind::ClassExpression) {
            return Some(p);
        }
        cur = p.parent();
    }
    None
}

fn collect_field_writes_in_node(node: P<Node>, out: &mut BTreeMap<String, Vec<FieldWriteSite>>) {
    if node.kind() == Kind::BinaryExpression {
        let bin = node.as_binary_expression();
        if bin.operator_token().kind() == Kind::EqualsToken {
            let lhs = unwrap_expr_wrappers(bin.left());
            let mut matched_field: Option<(P<Node>, String)> = None;
            if lhs.kind() == Kind::PropertyAccessExpression {
                let pa = lhs.as_property_access_expression();
                if let Some(prop_name) = node_text(pa.name()) {
                    matched_field = Some((pa.expression(), prop_name));
                }
            } else if lhs.kind() == Kind::ElementAccessExpression {
                let elem = lhs.as_element_access_expression();
                let arg = unwrap_expr_wrappers(elem.argument_expression());
                if matches!(
                    arg.kind(),
                    Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
                ) && let Some(prop_name) = prop_key_text(arg)
                {
                    matched_field = Some((elem.expression(), prop_name));
                }
            }
            if let Some((recv_node, field_name)) = matched_field {
                let (start, end) = node_byte_span(node);
                let block = enclosing_block(node);
                let is_unconditional = block.is_some_and(|b| is_direct_block_statement(node, b));
                out.entry(field_name).or_default().push(FieldWriteSite {
                    assign_node: node,
                    recv_node,
                    rhs_node: bin.right(),
                    enclosing_fn: resolve_enclosing_function(node),
                    enclosing_class: resolve_enclosing_class(node),
                    start,
                    end,
                    is_unconditional,
                    block,
                });
            }
        }
    }
    node.for_each_child(&mut |child| {
        collect_field_writes_in_node(child, out);
        false
    });
}

pub fn get_or_build_field_index(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    lookup_files: Option<&[String]>,
) -> Arc<FieldWriteIndex> {
    let cache_key = scoped_project_cache_key(project_id, lookup_files);
    if let Some(cached) = snapshot.field_write_index.get(&cache_key) {
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

    let mut writes_by_field = BTreeMap::new();
    for (_rel, sf) in source_files {
        collect_field_writes_in_node(sf.as_node(), &mut writes_by_field);
    }
    for writes in writes_by_field.values_mut() {
        writes.sort_by_key(|w| w.start);
    }

    let index = Arc::new(FieldWriteIndex { writes_by_field });
    snapshot
        .field_write_index
        .insert(cache_key, Arc::clone(&index));
    index
}

struct VarWriteSite {
    rhs: P<Node>,
    write_node: P<Node>,
    start: u32,
    end: u32,
    is_unconditional: bool,
    block: Option<P<Node>>,
}

fn collect_var_writes_in_scope(
    checker: &mut CheckerHandle,
    sym: P<Symbol>,
    scope: P<Node>,
    is_root: bool,
    out: &mut Vec<VarWriteSite>,
) {
    if !is_root && is_function_like_declaration(Some(scope)) {
        return;
    }
    if scope.kind() == Kind::VariableDeclaration
        && let Some(init) = scope.initializer()
        && let Some(name_node) = scope.name()
        && name_node.kind() == Kind::Identifier
        && canonical_symbol_for_node(checker, name_node) == Some(sym)
    {
        let (start, end) = node_byte_span(scope);
        let block = enclosing_block(scope);
        let is_unconditional = block.is_some_and(|b| is_direct_block_statement(scope, b));
        out.push(VarWriteSite {
            rhs: init,
            write_node: scope,
            start,
            end,
            is_unconditional,
            block,
        });
    } else if scope.kind() == Kind::BinaryExpression {
        let bin = scope.as_binary_expression();
        if bin.operator_token().kind() == Kind::EqualsToken {
            let lhs = unwrap_expr_wrappers(bin.left());
            if lhs.kind() == Kind::Identifier
                && canonical_symbol_for_node(checker, lhs) == Some(sym)
            {
                let (start, end) = node_byte_span(scope);
                let block = enclosing_block(scope);
                let is_unconditional = block.is_some_and(|b| is_direct_block_statement(scope, b));
                out.push(VarWriteSite {
                    rhs: bin.right(),
                    write_node: scope,
                    start,
                    end,
                    is_unconditional,
                    block,
                });
            }
        }
    }
    scope.for_each_child(&mut |child| {
        collect_var_writes_in_scope(checker, sym, child, false, out);
        false
    });
}

fn compute_read_anchor(read_node: P<Node>, decl_scope: P<Node>) -> P<Node> {
    let read_fn = resolve_enclosing_function(read_node);
    let decl_fn = resolve_enclosing_function(decl_scope);
    if read_fn == decl_fn {
        return read_node;
    }
    // Walk up from read_node until its parent is inside decl_scope (i.e. the closure anchor).
    let mut cur = read_node;
    while let Some(p) = cur.parent() {
        if resolve_enclosing_function(p) == decl_fn || p == decl_scope {
            if is_function_like_declaration(Some(cur)) {
                return cur;
            }
            if p == decl_scope {
                return cur;
            }
        }
        cur = p;
    }
    read_node
}

fn select_reaching_writes(mut writes: Vec<VarWriteSite>, anchor: P<Node>) -> Vec<VarWriteSite> {
    writes.sort_by_key(|w| w.start);
    let (anchor_start, _) = node_byte_span(anchor);
    let anchor_loop = enclosing_loop(anchor);

    let mut candidate_indices = Vec::new();
    for (idx, w) in writes.iter().enumerate() {
        if w.start <= anchor_start
            || (anchor_loop.is_some() && anchor_loop == enclosing_loop(w.write_node))
        {
            candidate_indices.push(idx);
        }
    }

    let latest_dominating = candidate_indices.iter().copied().rfind(|&idx| {
        let w = &writes[idx];
        w.is_unconditional
            && w.end <= anchor_start
            && w.block.is_some_and(|b| is_ancestor_of(b, anchor))
    });

    let start_idx = latest_dominating.unwrap_or(0);
    let selected: Vec<usize> = candidate_indices
        .into_iter()
        .filter(|&idx| idx >= start_idx)
        .collect();

    writes
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| selected.contains(idx))
        .map(|(_, w)| w)
        .collect()
}

pub fn is_external_declaration(program: &'static Program, decl: P<Node>) -> bool {
    get_source_file_of_node(decl)
        .is_some_and(|sf| sf.is_declaration_file() || !is_project_source_file(program, sf))
}

fn extract_npm_package_name(rel_path: &str) -> Option<String> {
    let (_, after) = rel_path.rsplit_once("node_modules/")?;
    let mut parts = after.split('/');
    let first = parts.next()?;
    if first.starts_with('@') {
        let second = parts.next()?;
        Some(format!("{first}/{second}"))
    } else {
        Some(first.to_string())
    }
}

fn module_matches(filter: &str, rel_path: &str, abs_path: &str) -> bool {
    if rel_path == filter || abs_path.ends_with(filter) {
        return true;
    }
    if let Some(pkg) = extract_npm_package_name(rel_path)
        && pkg == filter
    {
        return true;
    }
    rel_path.starts_with(&format!("{filter}/")) || rel_path.contains(&format!("/{filter}/"))
}

pub struct CalleeDeclMetadata {
    pub member_name: Option<String>,
    pub container_names: Vec<String>,
    pub qualified_names: Vec<String>,
    pub display_label: String,
    pub rel_file: String,
    pub abs_file: String,
}

fn strip_generic_args(raw: &str) -> &str {
    raw.split('<').next().unwrap_or(raw).trim()
}

pub fn collect_callee_decl_metadata(
    cwd: &str,
    checker: &mut CheckerHandle,
    decl: P<Node>,
    call_node: P<Node>,
) -> CalleeDeclMetadata {
    let member_name = decl
        .name()
        .and_then(node_text)
        .or_else(|| canonical_symbol_for_node(checker, decl).map(|s| s.name().to_string()))
        .or_else(|| {
            call_node.expression().and_then(|expr| {
                let unwrapped = unwrap_expr_wrappers(expr);
                if unwrapped.kind() == Kind::PropertyAccessExpression {
                    node_text(unwrapped.as_property_access_expression().name())
                } else {
                    node_text(unwrapped)
                }
            })
        });

    let mut container_names = Vec::new();
    let mut cur = decl.parent();
    while let Some(p) = cur {
        if matches!(
            p.kind(),
            Kind::ClassDeclaration
                | Kind::ClassExpression
                | Kind::InterfaceDeclaration
                | Kind::ModuleDeclaration
                | Kind::TypeAliasDeclaration
                | Kind::VariableDeclaration
        ) && let Some(name) = p.name().and_then(node_text)
            && !container_names.contains(&name)
        {
            container_names.push(name);
        }
        cur = p.parent();
    }

    if let Some(callee_expr) = call_node.expression() {
        let unwrapped = unwrap_expr_wrappers(callee_expr);
        if unwrapped.kind() == Kind::PropertyAccessExpression {
            let recv = unwrapped.as_property_access_expression().expression();
            let recv_ty = checker.get_type_at_location(recv);
            if let Some(type_sym) = recv_ty.symbol() {
                let name = type_sym.name().to_string();
                if !name.is_empty() && name != "__type" && !container_names.contains(&name) {
                    container_names.push(name);
                }
            }
            let type_str = checker.type_to_string_ex_exported(
                recv_ty,
                Some(recv),
                TypeFormatFlags::None,
                None,
            );
            let base = strip_generic_args(&type_str);
            if !base.is_empty()
                && !base.starts_with('{')
                && base != "any"
                && base != "unknown"
                && !container_names.iter().any(|c| c == base)
            {
                container_names.push(base.to_string());
            }
            if let Some(recv_name) = node_text(recv)
                && recv.kind() == Kind::Identifier
                && !container_names.contains(&recv_name)
            {
                container_names.push(recv_name);
            }
        }
    }

    let mut qualified_names = Vec::new();
    if let Some(ref mem) = member_name {
        for c in &container_names {
            let q = format!("{c}.{mem}");
            if !qualified_names.contains(&q) {
                qualified_names.push(q);
            }
        }
    }

    let display_label = qualified_names
        .first()
        .cloned()
        .or_else(|| member_name.clone())
        .unwrap_or_else(|| "<anonymous>".to_string());

    let (rel_file, abs_file) = get_source_file_of_node(decl).map_or_else(
        || (String::new(), String::new()),
        |sf| {
            let abs = sf.file_name().to_string();
            let rel = relative_to_cwd(cwd, &abs);
            (rel, abs)
        },
    );

    CalleeDeclMetadata {
        member_name,
        container_names,
        qualified_names,
        display_label,
        rel_file,
        abs_file,
    }
}

pub fn score_model_match(
    rule: &InstanceModelMatchInput,
    meta: &CalleeDeclMetadata,
) -> Option<usize> {
    let mut score = 0;

    if let Some(ref module_filter) = rule.module {
        if !module_matches(module_filter, &meta.rel_file, &meta.abs_file) {
            return None;
        }
        score += 1;
    }

    if let Some(ref container) = rule.container {
        if !meta.container_names.iter().any(|c| c == container) {
            return None;
        }
        score += 1;
    }

    if let Some(ref member) = rule.member {
        if meta.member_name.as_deref() != Some(member.as_str()) {
            return None;
        }
        score += 1;
    }

    if let Some(ref sym) = rule.symbol {
        if sym.contains('.') {
            if !meta.qualified_names.iter().any(|q| q == sym) {
                return None;
            }
        } else if meta.member_name.as_deref() != Some(sym.as_str())
            && !meta.qualified_names.iter().any(|q| q == sym)
        {
            return None;
        }
        score += 1;
    }

    (score > 0).then_some(score)
}

enum ModelMatchOutcome<'a> {
    Matched {
        model_id: &'a str,
        behavior: &'a InstanceModelBehaviorInput,
        declaration_label: String,
    },
    Conflict(GapItem),
    NoMatch,
}

fn find_matching_model<'a>(
    models: &'a [InstanceModelInput],
    meta: &CalleeDeclMetadata,
    call_loc: Option<&SourceLocation>,
) -> ModelMatchOutcome<'a> {
    let mut best_score = 0usize;
    let mut best_matches: Vec<(&'a str, &'a InstanceModelBehaviorInput)> = Vec::new();

    for model in models {
        for behavior in &model.instance_behaviors {
            if let Some(score) = score_model_match(&behavior.match_rule, meta) {
                if score > best_score {
                    best_score = score;
                    best_matches.clear();
                    best_matches.push((model.id.as_str(), behavior));
                } else if score == best_score {
                    best_matches.push((model.id.as_str(), behavior));
                }
            }
        }
    }

    let Some(&(first_model, first_behavior)) = best_matches.first() else {
        return ModelMatchOutcome::NoMatch;
    };

    for &(other_model, other_behavior) in best_matches.iter().skip(1) {
        let same_kind = first_behavior.kind == other_behavior.kind;
        let same_arg = first_behavior.argument_index.unwrap_or(0)
            == other_behavior.argument_index.unwrap_or(0);
        if !same_kind || (first_behavior.kind == "forwards-argument" && !same_arg) {
            return ModelMatchOutcome::Conflict(GapItem {
                kind: "conflicting-instance-model",
                message: format!(
                    "Models '{first_model}' and '{other_model}' declare conflicting instance behaviors for '{}'.",
                    meta.display_label
                ),
                at: call_loc.cloned(),
            });
        }
    }

    ModelMatchOutcome::Matched {
        model_id: first_model,
        behavior: first_behavior,
        declaration_label: meta.display_label.clone(),
    }
}

fn prepend_step(origins: &mut [ResolvedOrigin], step: &EvidenceItem) {
    for origin in origins {
        origin.evidence.push(step.clone());
    }
}

pub struct InstanceResolver<'a> {
    pub cwd: &'a str,
    pub program: &'static Program,
    pub checker: &'a mut CheckerHandle,
    pub snapshot: &'a mut SnapshotState,
    pub project_id: &'a str,
    pub lookup_files: Option<&'a [String]>,
    pub(crate) call_graph: Option<Arc<ProjectCallGraph>>,
    pub(crate) field_index: Option<Arc<FieldWriteIndex>>,
    pub package_impl_cache: PackageImplCache,
    pub models: &'a [InstanceModelInput],
}

impl InstanceResolver<'_> {
    fn call_graph(&mut self) -> Arc<ProjectCallGraph> {
        if let Some(graph) = &self.call_graph {
            return Arc::clone(graph);
        }
        let graph = get_or_build_call_graph(
            self.snapshot,
            self.project_id,
            self.program,
            self.checker,
            self.lookup_files,
        );
        self.call_graph = Some(Arc::clone(&graph));
        graph
    }

    fn field_index(&mut self) -> Arc<FieldWriteIndex> {
        if let Some(index) = &self.field_index {
            return Arc::clone(index);
        }
        let index = get_or_build_field_index(
            self.snapshot,
            self.project_id,
            self.program,
            self.lookup_files,
        );
        self.field_index = Some(Arc::clone(&index));
        index
    }

    fn resolve_target(&mut self, node: P<Node>) -> TraceOutcome {
        let mut active = Vec::new();
        let mut out = self.resolve_node(node, &[], node, 0, &mut active);
        out.origins
            .sort_by(|a, b| a.instance_key.cmp(&b.instance_key));
        out
    }

    pub fn resolve_with_stack(&mut self, node: P<Node>, call_stack: &[CallFrame]) -> TraceOutcome {
        let mut active = Vec::new();
        let mut out = self.resolve_node(node, call_stack, node, 0, &mut active);
        out.origins
            .sort_by(|a, b| a.instance_key.cmp(&b.instance_key));
        out
    }

    fn resolve_node(
        &mut self,
        raw_node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let mut out = TraceOutcome::default();
        if depth >= MAX_TRACE_DEPTH {
            out.push_gap(GapItem {
                kind: "trace-depth-limit",
                message: "Instance identity resolution reached maximum recursion depth."
                    .to_string(),
                at: node_location_rel(self.cwd, raw_node),
            });
            return out;
        }

        let node = unwrap_expr_wrappers(raw_node);
        let key = VisitKey {
            node,
            call_stack: call_stack.iter().map(|f| f.call_node).collect(),
            field_name: None,
        };
        if active.contains(&key) {
            out.push_gap(GapItem {
                kind: "instance-cycle",
                message: "Cyclic reference encountered while tracing instance identity."
                    .to_string(),
                at: node_location_rel(self.cwd, node),
            });
            return out;
        }
        active.push(key);
        let result = self.resolve_node_inner(node, call_stack, read_anchor, depth, active);
        active.pop();
        result
    }

    fn resolve_node_inner(
        &mut self,
        node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        match node.kind() {
            Kind::AwaitExpression => {
                if let Some(expr) = node.expression() {
                    let mut res =
                        self.resolve_node(expr, call_stack, read_anchor, depth + 1, active);
                    if let Some(loc) = node_location_rel(self.cwd, node) {
                        prepend_step(
                            &mut res.origins,
                            &EvidenceItem::simple(
                                "source",
                                "Unwrapped awaited value".to_string(),
                                Some(loc),
                            ),
                        );
                    }
                    return res;
                }
            }
            Kind::ReturnStatement => {
                if let Some(expr) = node.as_return_statement().expression() {
                    return self.resolve_node(expr, call_stack, read_anchor, depth + 1, active);
                }
            }
            Kind::ConditionalExpression => {
                let cond = node.as_conditional_expression();
                let mut combined =
                    self.resolve_node(cond.when_true(), call_stack, read_anchor, depth + 1, active);
                combined.merge(self.resolve_node(
                    cond.when_false(),
                    call_stack,
                    read_anchor,
                    depth + 1,
                    active,
                ));
                return combined;
            }
            Kind::BinaryExpression => {
                if let Some(res) =
                    self.resolve_binary_expression(node, call_stack, read_anchor, depth, active)
                {
                    return res;
                }
            }
            Kind::ObjectLiteralExpression => {
                return self.make_allocation_outcome(node, call_stack, "Created object literal");
            }
            Kind::ArrayLiteralExpression => {
                return self.make_allocation_outcome(node, call_stack, "Created array literal");
            }
            Kind::ArrowFunction | Kind::FunctionExpression => {
                return self.make_closure_outcome(node, call_stack);
            }
            Kind::NewExpression => {
                return self.resolve_new_expression(node, call_stack, depth, active);
            }
            Kind::CallExpression => {
                return self.resolve_call_expression(node, call_stack, read_anchor, depth, active);
            }
            Kind::ThisKeyword => {
                return self.resolve_this_keyword(node, call_stack, read_anchor, depth, active);
            }
            Kind::PropertyAccessExpression | Kind::ElementAccessExpression => {
                return self.resolve_member_access(node, call_stack, read_anchor, depth, active);
            }
            Kind::VariableDeclaration => {
                return self.resolve_variable_decl(
                    node,
                    node,
                    call_stack,
                    read_anchor,
                    depth,
                    active,
                );
            }
            Kind::Parameter => {
                return self.resolve_parameter(node, call_stack, depth, active);
            }
            Kind::BindingElement => {
                return self.resolve_binding_element(node, call_stack, read_anchor, depth, active);
            }
            Kind::PropertyDeclaration => {
                if let Some(init) = node.initializer() {
                    return self.resolve_node(init, call_stack, read_anchor, depth + 1, active);
                }
            }
            Kind::Identifier | Kind::ShorthandPropertyAssignment => {
                return self.resolve_identifier(node, call_stack, read_anchor, depth, active);
            }
            _ => {}
        }

        let mut out = TraceOutcome::default();
        out.push_gap(GapItem {
            kind: "unsupported-instance-expression",
            message: format!(
                "Expression kind {:?} does not resolve to a tracked resource instance.",
                node.kind()
            ),
            at: node_location_rel(self.cwd, node),
        });
        out
    }

    fn resolve_binary_expression(
        &mut self,
        node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> Option<TraceOutcome> {
        let bin = node.as_binary_expression();
        let op = bin.operator_token().kind();
        if op == Kind::EqualsToken {
            return Some(self.resolve_node(
                bin.right(),
                call_stack,
                read_anchor,
                depth + 1,
                active,
            ));
        }
        if matches!(
            op,
            Kind::BarBarToken | Kind::QuestionQuestionToken | Kind::AmpersandAmpersandToken
        ) {
            let mut combined =
                self.resolve_node(bin.left(), call_stack, read_anchor, depth + 1, active);
            combined.merge(self.resolve_node(
                bin.right(),
                call_stack,
                read_anchor,
                depth + 1,
                active,
            ));
            return Some(combined);
        }
        None
    }

    fn resolve_member_access(
        &mut self,
        node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        if node.kind() == Kind::PropertyAccessExpression {
            let pa = node.as_property_access_expression();
            let Some(prop_name) = node_text(pa.name()) else {
                let mut out = TraceOutcome::default();
                out.push_gap(GapItem {
                    kind: "unresolved-property",
                    message: "Property access has no static member name.".to_string(),
                    at: node_location_rel(self.cwd, node),
                });
                return out;
            };
            return self.resolve_property_access(
                node,
                pa.expression(),
                &prop_name,
                call_stack,
                read_anchor,
                depth,
                active,
            );
        }

        let elem = node.as_element_access_expression();
        let arg = unwrap_expr_wrappers(elem.argument_expression());
        if matches!(
            arg.kind(),
            Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
        ) && let Some(prop_name) = prop_key_text(arg)
        {
            return self.resolve_property_access(
                node,
                elem.expression(),
                &prop_name,
                call_stack,
                read_anchor,
                depth,
                active,
            );
        }
        let mut out = TraceOutcome::default();
        out.push_gap(GapItem {
            kind: "dynamic-property-access",
            message: "Computed element access cannot be statically resolved to a fixed field."
                .to_string(),
            at: node_location_rel(self.cwd, node),
        });
        out
    }

    fn make_allocation_outcome(
        &self,
        node: P<Node>,
        call_stack: &[CallFrame],
        message: &str,
    ) -> TraceOutcome {
        let loc = node_location_rel(self.cwd, node);
        let key = format_alloc_key(self.cwd, "alloc", node, call_stack);
        TraceOutcome {
            origins: vec![ResolvedOrigin {
                instance_key: key,
                origin_kind: "allocation",
                origin_node: Some(node),
                allocation_location: loc.clone(),
                model_id: None,
                evidence: vec![EvidenceItem::simple("source", message.to_string(), loc)],
                captured_stack: call_stack.to_vec(),
            }],
            gaps: Vec::new(),
        }
    }

    fn make_closure_outcome(&self, node: P<Node>, call_stack: &[CallFrame]) -> TraceOutcome {
        let loc = node_location_rel(self.cwd, node);
        let key = format_alloc_key(self.cwd, "closure", node, call_stack);
        TraceOutcome {
            origins: vec![ResolvedOrigin {
                instance_key: key,
                origin_kind: "allocation",
                origin_node: Some(node),
                allocation_location: loc.clone(),
                model_id: None,
                evidence: vec![EvidenceItem::simple(
                    "source",
                    "Created closure capturing enclosing instance bindings".to_string(),
                    loc,
                )],
                captured_stack: call_stack.to_vec(),
            }],
            gaps: Vec::new(),
        }
    }

    fn resolve_new_expression(
        &mut self,
        node: P<Node>,
        call_stack: &[CallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let new_expr = node.as_new_expression();
        let callee_expr = unwrap_expr_wrappers(new_expr.expression());
        let loc = node_location_rel(self.cwd, node);
        let sym_opt = canonical_symbol_for_node(self.checker, callee_expr);
        let decl_opt = sym_opt.and_then(|s| pick_preferred_impl_decl(s, None));

        if let Some(decl) = decl_opt {
            let meta = collect_callee_decl_metadata(self.cwd, self.checker, decl, node);
            match find_matching_model(self.models, &meta, loc.as_ref()) {
                ModelMatchOutcome::Conflict(gap) => {
                    return TraceOutcome {
                        origins: Vec::new(),
                        gaps: vec![gap],
                    };
                }
                ModelMatchOutcome::Matched {
                    model_id,
                    behavior,
                    declaration_label,
                } => {
                    return self.apply_model_behavior(
                        node,
                        model_id,
                        behavior,
                        &declaration_label,
                        call_stack,
                        depth,
                        active,
                    );
                }
                ModelMatchOutcome::NoMatch => {}
            }
        }

        let callee_ty = self.checker.get_type_at_location(callee_expr);
        if decl_opt.is_none()
            && callee_ty
                .flags()
                .intersects(TypeFlags::Any | TypeFlags::Unknown)
        {
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "dynamic-callee",
                    message:
                        "Constructor target has dynamic type and cannot be statically resolved."
                            .to_string(),
                    at: loc,
                }],
            };
        }

        let class_name = decl_opt
            .and_then(|d| d.name().and_then(node_text))
            .or_else(|| node_text(callee_expr))
            .unwrap_or_else(|| "instance".to_string());
        self.make_allocation_outcome(
            node,
            call_stack,
            &format!("Allocated new {class_name} instance"),
        )
    }

    fn resolve_this_keyword(
        &mut self,
        node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        if let Some((top, rest)) = call_stack.split_last()
            && let Some(recv) = top.receiver_expr
        {
            if recv == top.call_node && recv.kind() == Kind::NewExpression {
                return self.make_allocation_outcome(
                    recv,
                    rest,
                    "Constructed class instance receiver 'this'",
                );
            }
            return self.resolve_node(recv, rest, read_anchor, depth + 1, active);
        }

        if let Some(class_node) = resolve_enclosing_class(node) {
            let loc = node_location_rel(self.cwd, class_node);
            let class_name = class_node
                .name()
                .and_then(node_text)
                .unwrap_or_else(|| "<class>".to_string());
            let key = format!("this:{}", format_node_span_key(self.cwd, class_node));
            return TraceOutcome {
                origins: vec![ResolvedOrigin {
                    instance_key: key,
                    origin_kind: "allocation",
                    origin_node: Some(class_node),
                    allocation_location: loc.clone(),
                    model_id: None,
                    evidence: vec![EvidenceItem::simple(
                        "source",
                        format!("Class receiver 'this' of '{class_name}'"),
                        loc,
                    )],
                    captured_stack: Vec::new(),
                }],
                gaps: Vec::new(),
            };
        }

        TraceOutcome {
            origins: Vec::new(),
            gaps: vec![GapItem {
                kind: "unresolved-this",
                message: "'this' reference has no statically known enclosing class receiver."
                    .to_string(),
                at: node_location_rel(self.cwd, node),
            }],
        }
    }

    fn resolve_identifier(
        &mut self,
        node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let Some(sym) = canonical_symbol_for_node(self.checker, node) else {
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "unresolved-symbol",
                    message: format!(
                        "Identifier '{}' could not be resolved to a declaration.",
                        node_text(node).unwrap_or_default()
                    ),
                    at: node_location_rel(self.cwd, node),
                }],
            };
        };

        let Some(decl) = pick_preferred_impl_decl(sym, None) else {
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "unresolved-declaration",
                    message: format!("Symbol '{}' has no declaration.", sym.name()),
                    at: node_location_rel(self.cwd, node),
                }],
            };
        };

        match decl.kind() {
            Kind::Parameter => {
                let mut res = self.resolve_parameter(decl, call_stack, depth, active);
                if decl != node
                    && let Some(loc) = node_location_rel(self.cwd, node)
                {
                    prepend_step(
                        &mut res.origins,
                        &EvidenceItem::simple(
                            "source",
                            format!("Read parameter '{}'", sym.name()),
                            Some(loc),
                        ),
                    );
                }
                res
            }
            Kind::VariableDeclaration => {
                self.resolve_variable_decl(decl, node, call_stack, read_anchor, depth, active)
            }
            Kind::BindingElement => {
                self.resolve_binding_element(decl, call_stack, read_anchor, depth, active)
            }
            Kind::PropertyDeclaration => {
                if let Some(init) = decl.initializer() {
                    self.resolve_node(init, call_stack, read_anchor, depth + 1, active)
                } else {
                    TraceOutcome {
                        origins: Vec::new(),
                        gaps: vec![GapItem {
                            kind: "uninitialized-property",
                            message: format!("Property '{}' has no initializer.", sym.name()),
                            at: node_location_rel(self.cwd, decl),
                        }],
                    }
                }
            }
            k if is_function_like_declaration(Some(decl))
                || matches!(k, Kind::FunctionDeclaration | Kind::MethodDeclaration) =>
            {
                let fn_node = extract_callable_from_decl(decl).unwrap_or(decl);
                self.make_closure_outcome(fn_node, call_stack)
            }
            _ => TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "unsupported-declaration",
                    message: format!(
                        "Declaration '{}' ({:?}) does not hold a trackable resource instance.",
                        sym.name(),
                        decl.kind()
                    ),
                    at: node_location_rel(self.cwd, decl),
                }],
            },
        }
    }

    fn resolve_variable_decl(
        &mut self,
        decl: P<Node>,
        read_node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let decl_fn = resolve_enclosing_function(decl);
        let read_fn = resolve_enclosing_function(read_node);

        // Module-level variables are evaluated once per module, independent of caller call stacks.
        let effective_stack: &[CallFrame] = if decl_fn.is_none() {
            &[]
        } else if read_fn == decl_fn {
            call_stack
        } else {
            // Crossing out of an inner closure to an enclosing function variable:
            // trim frames belonging to the inner closure itself, keeping outer context.
            call_stack
                .iter()
                .rposition(|f| f.callee_fn == decl_fn)
                .map_or(call_stack, |pos| &call_stack[..=pos])
        };

        let sym_opt = decl
            .name()
            .and_then(|n| canonical_symbol_for_node(self.checker, n))
            .or_else(|| canonical_symbol_for_node(self.checker, decl));

        let Some(sym) = sym_opt else {
            if let Some(init) = decl.initializer() {
                return self.resolve_node(init, effective_stack, decl, depth + 1, active);
            }
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "unresolved-symbol",
                    message: "Variable declaration has no resolvable symbol.".to_string(),
                    at: node_location_rel(self.cwd, decl),
                }],
            };
        };

        let scope = decl_fn
            .or_else(|| get_source_file_of_node(decl).map(|sf| sf.as_node()))
            .unwrap_or(decl);

        let mut writes = Vec::new();
        collect_var_writes_in_scope(self.checker, sym, scope, true, &mut writes);
        let anchor = if read_node == decl {
            decl
        } else {
            compute_read_anchor(read_node, scope)
        };
        let reaching = select_reaching_writes(writes, anchor);

        if reaching.is_empty() {
            if let Some(init) = decl.initializer() {
                return self.resolve_node(init, effective_stack, decl, depth + 1, active);
            }
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "uninitialized-binding",
                    message: format!(
                        "Variable '{}' has no initializer or reaching assignment.",
                        sym.name()
                    ),
                    at: node_location_rel(self.cwd, read_node),
                }],
            };
        }

        let mut combined = TraceOutcome::default();
        for w in reaching {
            let mut sub =
                self.resolve_node(w.rhs, effective_stack, w.write_node, depth + 1, active);
            if read_node != decl {
                if let Some(loc) = node_location_rel(self.cwd, decl) {
                    let msg = if read_fn == decl_fn {
                        format!("Bound to '{}'", sym.name())
                    } else {
                        format!("Captured '{}' in closure", sym.name())
                    };
                    prepend_step(
                        &mut sub.origins,
                        &EvidenceItem::simple("source", msg, Some(loc)),
                    );
                }
                if let Some(read_loc) = node_location_rel(self.cwd, read_node) {
                    prepend_step(
                        &mut sub.origins,
                        &EvidenceItem::simple(
                            "source",
                            format!("Referenced alias '{}'", sym.name()),
                            Some(read_loc),
                        ),
                    );
                }
            }
            combined.merge(sub);
        }
        let _ = read_anchor;
        combined
    }

    fn resolve_parameter(
        &mut self,
        param_decl: P<Node>,
        call_stack: &[CallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let Some(fn_node) = resolve_enclosing_function(param_decl) else {
            return self.make_symbolic_parameter_outcome(param_decl);
        };
        let Some(param_idx) = fn_node.parameters().iter().position(|&p| p == param_decl) else {
            return self.make_symbolic_parameter_outcome(param_decl);
        };
        let param_name = param_decl
            .name()
            .and_then(node_text)
            .unwrap_or_else(|| format!("arg{param_idx}"));
        let fn_name = fn_node
            .name()
            .and_then(node_text)
            .unwrap_or_else(|| "<anonymous>".to_string());

        // Find the matching active frame for `fn_node` on the call stack (from innermost out).
        if let Some(frame_pos) = call_stack
            .iter()
            .rposition(|f| f.callee_fn == Some(fn_node))
        {
            let frame = &call_stack[frame_pos];
            let rest_stack = &call_stack[..frame_pos];
            if let Some(&arg) = frame.call_node.arguments().get(param_idx) {
                let mut res = self.resolve_node(arg, rest_stack, arg, depth + 1, active);
                if let Some(loc) = node_location_rel(self.cwd, frame.call_node) {
                    prepend_step(
                        &mut res.origins,
                        &EvidenceItem::simple(
                            "source",
                            format!("Passed as argument '{param_name}' to '{fn_name}'"),
                            Some(loc),
                        ),
                    );
                }
                return res;
            }
            if let Some(default_init) = param_decl.initializer() {
                return self.resolve_node(default_init, rest_stack, param_decl, depth + 1, active);
            }
            return self.make_symbolic_parameter_outcome(param_decl);
        }

        // Empty or non-matching stack: check direct callers of `fn_node` in the project call graph.
        if call_stack.is_empty() {
            let fn_sym = canonical_symbol_for_node(self.checker, fn_node);
            let graph = self.call_graph();
            let mut site_indices = Vec::new();
            if let Some(sym) = fn_sym
                && let Some(indices) = graph.by_symbol.get(&sym)
            {
                site_indices.extend_from_slice(indices);
            }
            if let Some(indices) = graph.by_decl.get(&fn_node) {
                for &idx in indices {
                    if !site_indices.contains(&idx) {
                        site_indices.push(idx);
                    }
                }
            }
            site_indices.sort_unstable();
            if !site_indices.is_empty() {
                let mut combined = TraceOutcome::default();
                for idx in site_indices {
                    let call_node = graph.sites[idx].call_node;
                    if let Some(&arg) = call_node.arguments().get(param_idx) {
                        let mut sub = self.resolve_node(arg, &[], arg, depth + 1, active);
                        if let Some(loc) = node_location_rel(self.cwd, call_node) {
                            prepend_step(
                                &mut sub.origins,
                                &EvidenceItem::simple(
                                    "source",
                                    format!("Passed as argument '{param_name}' to '{fn_name}'"),
                                    Some(loc),
                                ),
                            );
                        }
                        combined.merge(sub);
                    }
                }
                if !combined.origins.is_empty() || !combined.gaps.is_empty() {
                    return combined;
                }
            }
        }

        self.make_symbolic_parameter_outcome(param_decl)
    }

    fn make_symbolic_parameter_outcome(&self, param_decl: P<Node>) -> TraceOutcome {
        let loc = node_location_rel(self.cwd, param_decl);
        let name = param_decl
            .name()
            .and_then(node_text)
            .unwrap_or_else(|| "param".to_string());
        let key = format!("param:{}", format_node_span_key(self.cwd, param_decl));
        TraceOutcome {
            origins: vec![ResolvedOrigin {
                instance_key: key,
                origin_kind: "parameter",
                origin_node: Some(param_decl),
                allocation_location: loc.clone(),
                model_id: None,
                evidence: vec![EvidenceItem::simple(
                    "source",
                    format!("Unbound function parameter '{name}'"),
                    loc,
                )],
                captured_stack: Vec::new(),
            }],
            gaps: Vec::new(),
        }
    }

    fn resolve_binding_element(
        &mut self,
        be_node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let be = be_node.as_binding_element();
        if be.dot_dot_dot_token().is_some() {
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "unsupported-rest-binding",
                    message: "Rest destructuring binding does not hold a single resource field."
                        .to_string(),
                    at: node_location_rel(self.cwd, be_node),
                }],
            };
        }
        let Some(prop_name) = be
            .property_name()
            .and_then(prop_key_text)
            .or_else(|| be.name().and_then(node_text))
        else {
            return TraceOutcome::default();
        };
        let Some(pattern) = be_node.parent() else {
            return TraceOutcome::default();
        };
        let Some(owner) = pattern.parent() else {
            return TraceOutcome::default();
        };
        match owner.kind() {
            Kind::VariableDeclaration => {
                if let Some(init) = owner.initializer() {
                    return self.resolve_property_access(
                        be_node,
                        init,
                        &prop_name,
                        call_stack,
                        read_anchor,
                        depth,
                        active,
                    );
                }
            }
            Kind::Parameter => {
                let host_res = self.resolve_parameter(owner, call_stack, depth, active);
                return self.resolve_property_on_hosts(
                    be_node,
                    None,
                    &prop_name,
                    host_res,
                    call_stack,
                    read_anchor,
                    depth,
                    active,
                );
            }
            _ => {}
        }
        TraceOutcome::default()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Resolves source callees, package implementations, closure callees, and fallback models."
    )]
    fn resolve_call_expression(
        &mut self,
        call_node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let loc = node_location_rel(self.cwd, call_node);
        if call_stack.len() >= MAX_CALL_STACK_DEPTH {
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "call-stack-depth-limit",
                    message: "Call stack depth limit reached while resolving factory return."
                        .to_string(),
                    at: loc,
                }],
            };
        }

        let Some(callee_expr) = call_node.expression() else {
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "dynamic-callee",
                    message: "Call expression has no callee.".to_string(),
                    at: loc,
                }],
            };
        };

        let unwrapped_callee = unwrap_expr_wrappers(callee_expr);
        let receiver_expr = extract_call_receiver(unwrapped_callee);

        // First check AST/symbol call target resolution for source implementations & aliases.
        let outcome = resolve_call_targets(self.cwd, self.program, self.checker, call_node);
        if !outcome.targets.is_empty() {
            let mut combined = TraceOutcome::default();
            for target in outcome.targets {
                let has_source_body = target.fn_node.is_some_and(|fn_node| {
                    fn_node.body().is_some()
                        && !get_source_file_of_node(fn_node)
                            .is_some_and(|sf| sf.is_declaration_file())
                });
                let sub = if has_source_body {
                    let fn_node = target.fn_node.expect("checked has_source_body");
                    self.enter_source_function(
                        call_node,
                        fn_node,
                        receiver_expr,
                        target.name.as_deref(),
                        call_stack,
                        depth,
                        active,
                    )
                } else {
                    match resolve_impl_for_call_target(
                        &self.package_impl_cache,
                        self.cwd,
                        self.program,
                        self.checker,
                        call_node,
                        Some(target.decl_node),
                        target.fn_node,
                    ) {
                        PackageImplLookup::ResolvedExternal { fn_node, .. }
                        | PackageImplLookup::SourceBody { fn_node, .. } => self
                            .enter_source_function(
                                call_node,
                                fn_node,
                                receiver_expr,
                                target.name.as_deref(),
                                call_stack,
                                depth,
                                active,
                            ),
                        lookup => self.resolve_external_or_unbodied_target_with_lookup(
                            call_node,
                            target.decl_node,
                            Some(&lookup),
                            call_stack,
                            depth,
                            active,
                        ),
                    }
                };
                combined.merge(sub);
            }
            for gap in outcome.gaps {
                combined.push_gap(gap);
            }
            return combined;
        }

        // Check if callee_expr resolves to a closure produced by a local variable / factory
        // (e.g., `const g1 = makeGetter(); g1();`).
        if let Some(closure_outcome) = self.try_resolve_closure_callee(
            call_node,
            unwrapped_callee,
            receiver_expr,
            call_stack,
            read_anchor,
            depth,
            active,
        ) {
            return closure_outcome;
        }

        self.resolve_call_fallback(
            call_node,
            unwrapped_callee,
            outcome.gaps,
            loc,
            call_stack,
            depth,
            active,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Fallback call resolution checks resolved signature, symbol declaration, or dynamic gap."
    )]
    fn resolve_call_fallback(
        &mut self,
        call_node: P<Node>,
        unwrapped_callee: P<Node>,
        outcome_gaps: Vec<GapItem>,
        loc: Option<SourceLocation>,
        call_stack: &[CallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        // Fallback: check TypeScript's resolved signature (handles interface/class `.d.ts` methods
        // like `IMenuService.createMenu`, `this._register`, `store.add`, `Event.once`).
        let sig = self.checker.get_resolved_signature_exported(call_node);
        if sig != self.checker.get_unknown_signature()
            && let Some(sig_decl) = sig
                .declaration()
                .or_else(|| sig.target().and_then(|t| t.declaration()))
            && !matches!(sig_decl.kind(), Kind::FunctionType | Kind::Parameter)
        {
            return self.resolve_external_or_unbodied_target(
                call_node, sig_decl, call_stack, depth, active,
            );
        }

        // Also check symbol directly (e.g. declared variable/property signature in `.d.ts`).
        if let Some(sym) = canonical_symbol_for_node(self.checker, unwrapped_callee)
            && let Some(decl) = pick_preferred_impl_decl(sym, None)
            && is_external_declaration(self.program, decl)
        {
            return self
                .resolve_external_or_unbodied_target(call_node, decl, call_stack, depth, active);
        }

        let mut combined = TraceOutcome::default();
        if let Some(missing_gap) =
            check_call_missing_package_gap(self.program, self.checker, self.cwd, call_node)
        {
            combined.push_gap(missing_gap);
            return combined;
        }
        for gap in outcome_gaps {
            combined.push_gap(gap);
        }
        if combined.gaps.is_empty() {
            combined.push_gap(GapItem {
                kind: "dynamic-callee",
                message: "Call target cannot be statically resolved.".to_string(),
                at: loc,
            });
        }
        combined
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Closure call resolution forwards callee expression, receiver, stack, anchor, and guards."
    )]
    fn try_resolve_closure_callee(
        &mut self,
        call_node: P<Node>,
        callee_expr: P<Node>,
        receiver_expr: Option<P<Node>>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> Option<TraceOutcome> {
        let callee_res = self.resolve_node(callee_expr, call_stack, read_anchor, depth + 1, active);
        let mut found_closure = false;
        let mut combined = TraceOutcome::default();
        for origin in callee_res.origins {
            if let Some(fn_node) = origin.origin_node
                && is_function_like_declaration(Some(fn_node))
                && fn_node.body().is_some()
            {
                found_closure = true;
                let sub = self.enter_source_function(
                    call_node,
                    fn_node,
                    receiver_expr,
                    None,
                    &origin.captured_stack,
                    depth,
                    active,
                );
                combined.merge(sub);
            }
        }
        if found_closure {
            for gap in callee_res.gaps {
                combined.push_gap(gap);
            }
            Some(combined)
        } else {
            None
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Enters a source function body with a new CallFrame and collects return instances."
    )]
    fn enter_source_function(
        &mut self,
        call_node: P<Node>,
        fn_node: P<Node>,
        receiver_expr: Option<P<Node>>,
        fn_name_opt: Option<&str>,
        call_stack: &[CallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let returns = collect_return_expressions(fn_node);
        if returns.is_empty() {
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "no-return-expression",
                    message: format!(
                        "Function '{}' has no return expression to provide an instance.",
                        fn_name_opt
                            .map(str::to_string)
                            .or_else(|| fn_node.name().and_then(node_text))
                            .unwrap_or_else(|| "<anonymous>".to_string())
                    ),
                    at: node_location_rel(self.cwd, call_node),
                }],
            };
        }

        let mut next_stack = call_stack.to_vec();
        next_stack.push(CallFrame {
            call_node,
            callee_fn: Some(fn_node),
            receiver_expr,
        });

        let fn_label = fn_name_opt
            .map(str::to_string)
            .or_else(|| fn_node.name().and_then(node_text))
            .unwrap_or_else(|| "<anonymous>".to_string());
        let call_loc = node_location_rel(self.cwd, call_node);

        let mut combined = TraceOutcome::default();
        for ret_expr in returns {
            let mut sub = self.resolve_node(ret_expr, &next_stack, ret_expr, depth + 1, active);
            if let Some(ref loc) = call_loc {
                prepend_step(
                    &mut sub.origins,
                    &EvidenceItem::simple(
                        "source",
                        format!("Returned from call to '{fn_label}'"),
                        Some(loc.clone()),
                    ),
                );
            }
            combined.merge(sub);
        }
        combined
    }

    fn resolve_external_or_unbodied_target(
        &mut self,
        call_node: P<Node>,
        decl: P<Node>,
        call_stack: &[CallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let lookup = resolve_impl_for_call_target(
            &self.package_impl_cache,
            self.cwd,
            self.program,
            self.checker,
            call_node,
            Some(decl),
            None,
        );
        if let PackageImplLookup::ResolvedExternal { fn_node, .. }
        | PackageImplLookup::SourceBody { fn_node, .. } = lookup
        {
            let receiver_expr = call_node
                .expression()
                .map(unwrap_expr_wrappers)
                .and_then(extract_call_receiver);
            return self.enter_source_function(
                call_node,
                fn_node,
                receiver_expr,
                decl.name().and_then(node_text).as_deref(),
                call_stack,
                depth,
                active,
            );
        }
        self.resolve_external_or_unbodied_target_with_lookup(
            call_node,
            decl,
            Some(&lookup),
            call_stack,
            depth,
            active,
        )
    }

    fn resolve_external_or_unbodied_target_with_lookup(
        &mut self,
        call_node: P<Node>,
        decl: P<Node>,
        lookup: Option<&PackageImplLookup>,
        call_stack: &[CallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let call_loc = node_location_rel(self.cwd, call_node);
        let meta = collect_callee_decl_metadata(self.cwd, self.checker, decl, call_node);

        match find_matching_model(self.models, &meta, call_loc.as_ref()) {
            ModelMatchOutcome::Conflict(gap) => TraceOutcome {
                origins: Vec::new(),
                gaps: vec![gap],
            },
            ModelMatchOutcome::Matched {
                model_id,
                behavior,
                declaration_label,
            } => self.apply_model_behavior(
                call_node,
                model_id,
                behavior,
                &declaration_label,
                call_stack,
                depth,
                active,
            ),
            ModelMatchOutcome::NoMatch => {
                let (kind, message) = match lookup {
                    Some(PackageImplLookup::MissingPackage { message, .. }) => {
                        ("missing-package", message.clone())
                    }
                    Some(PackageImplLookup::Ambiguous { message, .. }) => {
                        ("ambiguous-package-implementation", message.clone())
                    }
                    Some(PackageImplLookup::DeclarationOnly { message, .. }) => (
                        "missing-instance-model",
                        format!(
                            "{message} No matching project model is configured for '{}'.",
                            meta.display_label
                        ),
                    ),
                    _ => {
                        let is_ext = is_external_declaration(self.program, decl);
                        if is_ext {
                            (
                                "missing-instance-model",
                                format!(
                                    "External declaration '{}' has no source implementation body and no matching project model.",
                                    meta.display_label
                                ),
                            )
                        } else {
                            (
                                "dynamic-callee",
                                format!(
                                    "Call target '{}' has no static implementation body and no matching project model.",
                                    meta.display_label
                                ),
                            )
                        }
                    }
                };
                TraceOutcome {
                    origins: Vec::new(),
                    gaps: vec![GapItem {
                        kind,
                        message,
                        at: call_loc,
                    }],
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Applies matched Model behavior (allocation vs forwarding) with source context."
    )]
    fn apply_model_behavior(
        &mut self,
        call_node: P<Node>,
        model_id: &str,
        behavior: &InstanceModelBehaviorInput,
        declaration_label: &str,
        call_stack: &[CallFrame],
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let call_loc = node_location_rel(self.cwd, call_node);
        match behavior.kind.as_str() {
            "allocates-fresh-instance" => {
                let ctx_suffix = format_context_suffix(self.cwd, call_stack);
                let span_key = format_node_span_key(self.cwd, call_node);
                let instance_key =
                    format!("model:{model_id}:{declaration_label}@{span_key}{ctx_suffix}");
                let message = behavior.description.clone().unwrap_or_else(|| {
                    format!(
                        "Model '{model_id}' declares '{declaration_label}' allocates a fresh instance"
                    )
                });
                TraceOutcome {
                    origins: vec![ResolvedOrigin {
                        instance_key,
                        origin_kind: "model-allocation",
                        origin_node: Some(call_node),
                        allocation_location: call_loc.clone(),
                        model_id: Some(model_id.to_string()),
                        evidence: vec![EvidenceItem::model(
                            message,
                            call_loc,
                            model_id.to_string(),
                            declaration_label.to_string(),
                        )],
                        captured_stack: call_stack.to_vec(),
                    }],
                    gaps: Vec::new(),
                }
            }
            "forwards-argument" => {
                let arg_idx = behavior.argument_index.unwrap_or(0);
                let Some(&arg) = call_node.arguments().get(arg_idx) else {
                    return TraceOutcome {
                        origins: Vec::new(),
                        gaps: vec![GapItem {
                            kind: "missing-forwarded-argument",
                            message: format!(
                                "Model '{model_id}' expects argument #{arg_idx} on '{declaration_label}', but call passed fewer arguments."
                            ),
                            at: call_loc,
                        }],
                    };
                };
                let mut res = self.resolve_node(arg, call_stack, arg, depth + 1, active);
                let message = behavior.description.clone().unwrap_or_else(|| {
                    format!(
                        "Model '{model_id}' declares '{declaration_label}' forwards argument #{arg_idx}"
                    )
                });
                let ev = EvidenceItem::model(
                    message,
                    call_loc,
                    model_id.to_string(),
                    declaration_label.to_string(),
                );
                for origin in &mut res.origins {
                    if origin.model_id.is_none() {
                        origin.model_id = Some(model_id.to_string());
                    }
                    origin.evidence.push(ev.clone());
                }
                res
            }
            other => TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "unsupported-instance-model",
                    message: format!(
                        "Unsupported instance behavior kind '{other}' in model '{model_id}'."
                    ),
                    at: call_loc,
                }],
            },
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Property read resolution needs read node, receiver, field name, stack, anchor, and guards."
    )]
    fn resolve_property_access(
        &mut self,
        read_node: P<Node>,
        recv_expr: P<Node>,
        prop_name: &str,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let key = VisitKey {
            node: read_node,
            call_stack: call_stack.iter().map(|f| f.call_node).collect(),
            field_name: Some(prop_name.to_string()),
        };
        if active.contains(&key) {
            return TraceOutcome {
                origins: Vec::new(),
                gaps: vec![GapItem {
                    kind: "instance-cycle",
                    message: format!("Cyclic field lookup on '.{prop_name}'."),
                    at: node_location_rel(self.cwd, read_node),
                }],
            };
        }
        active.push(key);
        let host_res = self.resolve_node(recv_expr, call_stack, read_anchor, depth + 1, active);
        let out = self.resolve_property_on_hosts(
            read_node,
            Some(recv_expr),
            prop_name,
            host_res,
            call_stack,
            read_anchor,
            depth,
            active,
        );
        active.pop();
        out
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Resolves a field across each resolved host instance and records field-read evidence."
    )]
    fn resolve_property_on_hosts(
        &mut self,
        read_node: P<Node>,
        recv_expr_opt: Option<P<Node>>,
        prop_name: &str,
        host_res: TraceOutcome,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let mut combined = TraceOutcome {
            origins: Vec::new(),
            gaps: host_res.gaps,
        };
        let read_loc = node_location_rel(self.cwd, read_node);
        let read_enclosing_fn = resolve_enclosing_function(read_node);

        for host in host_res.origins {
            let mut matched_for_host = TraceOutcome::default();

            if let Some(origin_node) = host.origin_node
                && origin_node.kind() == Kind::ObjectLiteralExpression
                && let Some(prop_val) = resolve_property_in_object_literal(origin_node, prop_name)
            {
                let mut sub =
                    self.resolve_node(prop_val, &host.captured_stack, prop_val, depth + 1, active);
                if let Some(ref loc) = read_loc {
                    prepend_step(
                        &mut sub.origins,
                        &EvidenceItem::simple(
                            "source",
                            format!("Read field '.{prop_name}'"),
                            Some(loc.clone()),
                        ),
                    );
                }
                matched_for_host.merge(sub);
            }

            let host_class_opt = host.origin_node.and_then(|n| self.resolve_host_class(n));
            if let Some(class_node) = host_class_opt {
                let class_res = self.resolve_class_host_member(
                    &host,
                    class_node,
                    prop_name,
                    read_loc.as_ref(),
                    depth,
                    active,
                );
                matched_for_host.merge(class_res);
            }

            let write_matches = self.find_matching_field_writes(
                &host,
                host_class_opt,
                prop_name,
                read_node,
                call_stack,
                read_anchor,
                read_enclosing_fn,
                recv_expr_opt,
                depth,
                active,
            );
            for (write_site, write_stack) in write_matches {
                let mut sub = self.resolve_node(
                    write_site.rhs_node,
                    &write_stack,
                    write_site.assign_node,
                    depth + 1,
                    active,
                );
                if let Some(wloc) = node_location_rel(self.cwd, write_site.assign_node) {
                    prepend_step(
                        &mut sub.origins,
                        &EvidenceItem::simple(
                            "source",
                            format!("Stored in field '.{prop_name}'"),
                            Some(wloc),
                        ),
                    );
                }
                if let Some(ref rloc) = read_loc {
                    prepend_step(
                        &mut sub.origins,
                        &EvidenceItem::simple(
                            "source",
                            format!("Read field '.{prop_name}'"),
                            Some(rloc.clone()),
                        ),
                    );
                }
                matched_for_host.merge(sub);
            }

            if matched_for_host.origins.is_empty() && matched_for_host.gaps.is_empty() {
                matched_for_host.push_gap(GapItem {
                    kind: "unresolved-field-origin",
                    message: format!(
                        "Field '.{prop_name}' has no known initialization or assignment in analyzed sources."
                    ),
                    at: read_loc.clone(),
                });
            }

            combined.merge(matched_for_host);
        }

        combined
    }

    fn resolve_class_host_member(
        &mut self,
        host: &ResolvedOrigin,
        class_node: P<Node>,
        prop_name: &str,
        read_loc: Option<&SourceLocation>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> TraceOutcome {
        let mut matched = TraceOutcome::default();
        let ctor_stack: Vec<CallFrame> = match host.origin_node {
            Some(new_node) if new_node.kind() == Kind::NewExpression => {
                let mut s = host.captured_stack.clone();
                s.push(CallFrame {
                    call_node: new_node,
                    callee_fn: find_class_constructor(class_node),
                    receiver_expr: Some(new_node),
                });
                s
            }
            _ => Vec::new(),
        };
        if let Some(prop_init) = find_class_property_initializer(class_node, prop_name) {
            let mut sub = self.resolve_node(prop_init, &ctor_stack, prop_init, depth + 1, active);
            if let Some(loc) = read_loc {
                prepend_step(
                    &mut sub.origins,
                    &EvidenceItem::simple(
                        "source",
                        format!("Read initialized class field '.{prop_name}'"),
                        Some(loc.clone()),
                    ),
                );
            }
            matched.merge(sub);
        }
        if let Some(getter_node) = find_class_getter(class_node, prop_name) {
            for ret in collect_return_expressions(getter_node) {
                let mut getter_stack = ctor_stack.clone();
                if let Some(new_node) = host.origin_node
                    && new_node.kind() == Kind::NewExpression
                {
                    getter_stack.push(CallFrame {
                        call_node: new_node,
                        callee_fn: Some(getter_node),
                        receiver_expr: Some(new_node),
                    });
                }
                let mut sub = self.resolve_node(ret, &getter_stack, ret, depth + 1, active);
                if let Some(loc) = read_loc {
                    prepend_step(
                        &mut sub.origins,
                        &EvidenceItem::simple(
                            "source",
                            format!("Read getter '.{prop_name}'"),
                            Some(loc.clone()),
                        ),
                    );
                }
                matched.merge(sub);
            }
        }
        matched
    }

    fn resolve_host_class(&mut self, origin_node: P<Node>) -> Option<P<Node>> {
        if matches!(
            origin_node.kind(),
            Kind::ClassDeclaration | Kind::ClassExpression
        ) {
            return Some(origin_node);
        }
        if origin_node.kind() == Kind::NewExpression {
            let new_expr = origin_node.as_new_expression();
            let callee = unwrap_expr_wrappers(new_expr.expression());
            if let Some(sym) = canonical_symbol_for_node(self.checker, callee) {
                for &d in sym.declarations() {
                    if matches!(d.kind(), Kind::ClassDeclaration | Kind::ClassExpression) {
                        return Some(d);
                    }
                }
                if let Some(vd) = sym.value_declaration()
                    && matches!(vd.kind(), Kind::ClassDeclaration | Kind::ClassExpression)
                {
                    return Some(vd);
                }
            }
        }
        None
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Matches imperative property writes to a resolved host instance with flow sensitivity."
    )]
    fn find_matching_field_writes(
        &mut self,
        host: &ResolvedOrigin,
        host_class_opt: Option<P<Node>>,
        prop_name: &str,
        read_node: P<Node>,
        call_stack: &[CallFrame],
        read_anchor: P<Node>,
        _read_enclosing_fn: Option<P<Node>>,
        _recv_expr_opt: Option<P<Node>>,
        depth: usize,
        active: &mut Vec<VisitKey>,
    ) -> Vec<(FieldWriteSite, Vec<CallFrame>)> {
        let field_index = self.field_index();
        let Some(all_writes) = field_index.writes_by_field.get(prop_name) else {
            return Vec::new();
        };

        let mut timeline_reads = Vec::with_capacity(1 + call_stack.len());
        let (r_start, _) = node_byte_span(read_node);
        let (a_start, _) = node_byte_span(read_anchor);
        timeline_reads.push(TimelineRead {
            node: read_node,
            enclosing_fn: resolve_enclosing_function(read_node),
            start: r_start.max(a_start),
            loop_node: enclosing_loop(read_node),
        });
        for frame in call_stack {
            let (f_start, _) = node_byte_span(frame.call_node);
            timeline_reads.push(TimelineRead {
                node: frame.call_node,
                enclosing_fn: resolve_enclosing_function(frame.call_node),
                start: f_start,
                loop_node: enclosing_loop(frame.call_node),
            });
        }

        let mut matched = Vec::new();
        for site in all_writes {
            self.collect_matches_for_write_site(
                site,
                host,
                host_class_opt,
                &timeline_reads,
                depth,
                active,
                &mut matched,
            );
        }

        let mut latest_cutoffs: Vec<(P<Node>, u32)> = Vec::new();
        for m in &matched {
            if let Some(pos) = m.local_order_pos
                && pos.is_unconditional
                && let Some(tl) = timeline_reads.iter().find(|tl| tl.node == pos.tl_node)
                && pos.end <= tl.start
                && pos.block.is_some_and(|b| is_ancestor_of(b, pos.tl_node))
            {
                if let Some(entry) = latest_cutoffs.iter_mut().find(|(n, _)| *n == pos.tl_node) {
                    if pos.start > entry.1 {
                        entry.1 = pos.start;
                    }
                } else {
                    latest_cutoffs.push((pos.tl_node, pos.start));
                }
            }
        }

        matched
            .into_iter()
            .filter(|m| {
                m.local_order_pos.is_none_or(|pos| {
                    latest_cutoffs
                        .iter()
                        .find(|&&(n, _)| n == pos.tl_node)
                        .is_none_or(|&(_, cutoff)| pos.start >= cutoff)
                })
            })
            .map(|m| (m.site, m.stack))
            .collect()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Evaluates constructor, method, helper-parameter, and direct receiver field writes."
    )]
    fn collect_matches_for_write_site(
        &mut self,
        site: &FieldWriteSite,
        host: &ResolvedOrigin,
        host_class_opt: Option<P<Node>>,
        timeline_reads: &[TimelineRead],
        depth: usize,
        active: &mut Vec<VisitKey>,
        matched: &mut Vec<MatchedWrite>,
    ) {
        let write_recv = unwrap_expr_wrappers(site.recv_node);

        if write_recv.kind() == Kind::ThisKeyword
            && let Some(host_class) = host_class_opt
            && site.enclosing_class == Some(host_class)
        {
            self.match_this_field_write(
                site,
                host,
                host_class,
                timeline_reads,
                depth,
                active,
                matched,
            );
            return;
        }

        if let Some(param_decl) = self.resolve_to_parameter_decl(write_recv)
            && let Some(w_fn) = site.enclosing_fn
            && let Some(param_idx) = w_fn.parameters().iter().position(|&p| p == param_decl)
        {
            self.match_param_field_write(
                site,
                host,
                w_fn,
                param_idx,
                timeline_reads,
                depth,
                active,
                matched,
            );
            return;
        }

        let recv_res = self.resolve_node(write_recv, &[], write_recv, depth + 1, active);
        if recv_res
            .origins
            .iter()
            .any(|o| o.instance_key == host.instance_key)
        {
            let order = match_timeline_order(
                site.assign_node,
                site.enclosing_fn,
                site.start,
                site.end,
                site.is_unconditional,
                site.block,
                timeline_reads,
            );
            if matches!(order, TimelineMatch::AfterRead) {
                return;
            }
            matched.push(MatchedWrite {
                site: site.clone(),
                stack: Vec::new(),
                local_order_pos: order.into_pos(),
            });
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Matches `this.field = rhs` writes inside constructors and class methods."
    )]
    fn match_this_field_write(
        &mut self,
        site: &FieldWriteSite,
        host: &ResolvedOrigin,
        host_class: P<Node>,
        timeline_reads: &[TimelineRead],
        depth: usize,
        active: &mut Vec<VisitKey>,
        matched: &mut Vec<MatchedWrite>,
    ) {
        if let Some(w_fn) = site.enclosing_fn
            && w_fn.kind() == Kind::Constructor
            && let Some(origin_node) = host.origin_node
            && origin_node.kind() == Kind::NewExpression
        {
            let event_node = host
                .captured_stack
                .first()
                .map_or(origin_node, |f| f.call_node);
            let (ev_start, ev_end) = node_byte_span(event_node);
            let ev_block = enclosing_block(event_node);
            let ev_uncond = ev_block.is_some_and(|b| is_direct_block_statement(event_node, b));
            let order = match_timeline_order(
                event_node,
                resolve_enclosing_function(event_node),
                ev_start,
                ev_end,
                ev_uncond,
                ev_block,
                timeline_reads,
            );
            if matches!(order, TimelineMatch::AfterRead) {
                return;
            }
            let mut stack = host.captured_stack.clone();
            stack.push(CallFrame {
                call_node: origin_node,
                callee_fn: Some(w_fn),
                receiver_expr: Some(origin_node),
            });
            matched.push(MatchedWrite {
                site: site.clone(),
                stack,
                local_order_pos: order.into_pos(),
            });
            return;
        }

        if host.origin_node == Some(host_class) {
            let order = match_timeline_order(
                site.assign_node,
                site.enclosing_fn,
                site.start,
                site.end,
                site.is_unconditional,
                site.block,
                timeline_reads,
            );
            if matches!(order, TimelineMatch::AfterRead) {
                return;
            }
            matched.push(MatchedWrite {
                site: site.clone(),
                stack: Vec::new(),
                local_order_pos: order.into_pos(),
            });
            return;
        }

        if let Some(w_fn) = site.enclosing_fn {
            for call_node in self.collect_callers_for_fn(w_fn) {
                let Some(callee) = call_node.expression() else {
                    continue;
                };
                let Some(recv) = extract_call_receiver(unwrap_expr_wrappers(callee)) else {
                    continue;
                };
                let recv_res = self.resolve_node(recv, &[], recv, depth + 1, active);
                if !recv_res
                    .origins
                    .iter()
                    .any(|o| o.instance_key == host.instance_key)
                {
                    continue;
                }
                let caller_fn = resolve_enclosing_function(call_node);
                let (c_start, c_end) = node_byte_span(call_node);
                let c_block = enclosing_block(call_node);
                let c_uncond = c_block.is_some_and(|b| is_direct_block_statement(call_node, b));
                let order = match_timeline_order(
                    call_node,
                    caller_fn,
                    c_start,
                    c_end,
                    c_uncond,
                    c_block,
                    timeline_reads,
                );
                if matches!(order, TimelineMatch::AfterRead) {
                    continue;
                }
                matched.push(MatchedWrite {
                    site: site.clone(),
                    stack: vec![CallFrame {
                        call_node,
                        callee_fn: Some(w_fn),
                        receiver_expr: Some(recv),
                    }],
                    local_order_pos: order.into_pos(),
                });
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Matches `param.field = rhs` writes inside helper functions across callers."
    )]
    fn match_param_field_write(
        &mut self,
        site: &FieldWriteSite,
        host: &ResolvedOrigin,
        w_fn: P<Node>,
        param_idx: usize,
        timeline_reads: &[TimelineRead],
        depth: usize,
        active: &mut Vec<VisitKey>,
        matched: &mut Vec<MatchedWrite>,
    ) {
        for call_node in self.collect_callers_for_fn(w_fn) {
            let Some(&arg) = call_node.arguments().get(param_idx) else {
                continue;
            };
            let arg_res = self.resolve_node(arg, &[], arg, depth + 1, active);
            if !arg_res
                .origins
                .iter()
                .any(|o| o.instance_key == host.instance_key)
            {
                continue;
            }
            let caller_fn = resolve_enclosing_function(call_node);
            let (c_start, c_end) = node_byte_span(call_node);
            let c_block = enclosing_block(call_node);
            let c_uncond = c_block.is_some_and(|b| is_direct_block_statement(call_node, b));
            let order = match_timeline_order(
                call_node,
                caller_fn,
                c_start,
                c_end,
                c_uncond,
                c_block,
                timeline_reads,
            );
            if matches!(order, TimelineMatch::AfterRead) {
                continue;
            }
            matched.push(MatchedWrite {
                site: site.clone(),
                stack: vec![CallFrame {
                    call_node,
                    callee_fn: Some(w_fn),
                    receiver_expr: None,
                }],
                local_order_pos: order.into_pos(),
            });
        }
    }

    fn resolve_to_parameter_decl(&mut self, expr: P<Node>) -> Option<P<Node>> {
        let unwrapped = unwrap_expr_wrappers(expr);
        if unwrapped.kind() != Kind::Identifier {
            return None;
        }
        let sym = canonical_symbol_for_node(self.checker, unwrapped)?;
        let decl = pick_preferred_impl_decl(sym, None)?;
        (decl.kind() == Kind::Parameter).then_some(decl)
    }

    fn collect_callers_for_fn(&mut self, fn_node: P<Node>) -> Vec<P<Node>> {
        let graph = self.call_graph();
        let mut indices = Vec::new();
        if let Some(sym) = canonical_symbol_for_node(self.checker, fn_node)
            && let Some(list) = graph.by_symbol.get(&sym)
        {
            indices.extend_from_slice(list);
        }
        if let Some(list) = graph.by_decl.get(&fn_node) {
            indices.extend_from_slice(list);
        }
        indices.sort_unstable();
        indices.dedup();
        indices
            .into_iter()
            .map(|idx| graph.sites[idx].call_node)
            .collect()
    }
}

#[derive(Clone, Copy)]
struct TimelineRead {
    node: P<Node>,
    enclosing_fn: Option<P<Node>>,
    start: u32,
    loop_node: Option<P<Node>>,
}

#[derive(Clone, Copy)]
struct WriteOrderPos {
    tl_node: P<Node>,
    start: u32,
    end: u32,
    is_unconditional: bool,
    block: Option<P<Node>>,
}

struct MatchedWrite {
    site: FieldWriteSite,
    stack: Vec<CallFrame>,
    local_order_pos: Option<WriteOrderPos>,
}

enum TimelineMatch {
    DifferentFunction,
    AfterRead,
    BeforeRead(WriteOrderPos),
}

impl TimelineMatch {
    const fn into_pos(self) -> Option<WriteOrderPos> {
        match self {
            Self::BeforeRead(pos) => Some(pos),
            Self::DifferentFunction | Self::AfterRead => None,
        }
    }
}

fn match_timeline_order(
    write_event_node: P<Node>,
    write_fn: Option<P<Node>>,
    w_start: u32,
    w_end: u32,
    w_uncond: bool,
    w_block: Option<P<Node>>,
    timeline_reads: &[TimelineRead],
) -> TimelineMatch {
    for tl in timeline_reads {
        if write_fn == tl.enclosing_fn {
            if w_start > tl.start
                && !(tl.loop_node.is_some() && tl.loop_node == enclosing_loop(write_event_node))
            {
                return TimelineMatch::AfterRead;
            }
            return TimelineMatch::BeforeRead(WriteOrderPos {
                tl_node: tl.node,
                start: w_start,
                end: w_end,
                is_unconditional: w_uncond,
                block: w_block,
            });
        }
    }
    TimelineMatch::DifferentFunction
}

pub fn extract_call_receiver(unwrapped_callee: P<Node>) -> Option<P<Node>> {
    match unwrapped_callee.kind() {
        Kind::PropertyAccessExpression => Some(
            unwrapped_callee
                .as_property_access_expression()
                .expression(),
        ),
        Kind::ElementAccessExpression => {
            Some(unwrapped_callee.as_element_access_expression().expression())
        }
        _ => None,
    }
}

fn find_class_constructor(class_node: P<Node>) -> Option<P<Node>> {
    class_node
        .members()
        .iter()
        .copied()
        .find(|m| m.kind() == Kind::Constructor && m.body().is_some())
}

fn find_class_property_initializer(class_node: P<Node>, prop_name: &str) -> Option<P<Node>> {
    for &member in class_node.members() {
        if member.kind() == Kind::PropertyDeclaration
            && member.name().and_then(node_text).as_deref() == Some(prop_name)
            && let Some(init) = member.initializer()
        {
            return Some(init);
        }
    }
    None
}

fn find_class_getter(class_node: P<Node>, prop_name: &str) -> Option<P<Node>> {
    class_node.members().iter().copied().find(|&member| {
        member.kind() == Kind::GetAccessor
            && member.name().and_then(node_text).as_deref() == Some(prop_name)
            && member.body().is_some()
    })
}

fn resolve_instance_target_node(
    snapshot: &SnapshotState,
    project_id: &str,
    handle_id: &str,
) -> Result<P<Node>, RpcErr> {
    let entry = snapshot.get_entry(project_id, handle_id)?;
    match entry.payload {
        crate::serve::HandlePayload::Syntax(node)
        | crate::serve::HandlePayload::Call(node)
        | crate::serve::HandlePayload::Declaration { node, .. } => Ok(node),
        crate::serve::HandlePayload::Signature { .. }
        | crate::serve::HandlePayload::Type { .. }
        | crate::serve::HandlePayload::Branch(_)
        | crate::serve::HandlePayload::Instance(_) => Err(RpcErr::new(
            "INVALID_REFERENCE_KIND",
            format!("Program reference {handle_id} cannot be resolved as an instance target."),
        )),
    }
}

pub fn run_resolve_instances(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
    models: &[InstanceModelInput],
    lookup_files: Option<&[String]>,
) -> Result<Vec<InstanceResolutionRow>, RpcErr> {
    let mut target_nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        target_nodes.push((
            handle_id.clone(),
            resolve_instance_target_node(snapshot, project_id, handle_id)?,
        ));
    }

    let cwd = snapshot.cwd.clone();
    let package_impl_cache = snapshot.package_impl_cache.clone();

    let mut outcomes = Vec::with_capacity(target_nodes.len());
    {
        let mut resolver = InstanceResolver {
            cwd: &cwd,
            program,
            checker,
            snapshot,
            project_id,
            lookup_files,
            call_graph: None,
            field_index: None,
            package_impl_cache,
            models,
        };
        for (target_id, node) in target_nodes {
            outcomes.push((target_id, resolver.resolve_target(node)));
        }
    }

    let mut rows = Vec::with_capacity(outcomes.len());
    for (target_id, outcome) in outcomes {
        let mut resolved_infos = Vec::with_capacity(outcome.origins.len());
        let mut combined_evidence = Vec::new();
        for origin in outcome.origins {
            let origin_syntax = origin
                .origin_node
                .map(|n| snapshot.alloc_syntax(project_id, n));
            let inst_ref = snapshot.alloc_instance(
                project_id,
                InstanceHandleData {
                    instance_key: origin.instance_key.clone(),
                    origin_kind: origin.origin_kind,
                    allocation_location: origin.allocation_location.clone(),
                    origin_syntax: origin.origin_node,
                    model_id: origin.model_id.clone(),
                    evidence: origin.evidence.clone(),
                },
            );
            for ev in &origin.evidence {
                combined_evidence.push(ev.clone());
            }
            resolved_infos.push(ResolvedInstanceInfoPayload {
                instance_ref: inst_ref,
                instance_key: origin.instance_key,
                origin_kind: origin.origin_kind,
                allocation_location: origin.allocation_location,
                origin_syntax,
                model_id: origin.model_id,
                evidence: origin.evidence,
            });
        }

        let primary = resolved_infos.first();
        rows.push(InstanceResolutionRow {
            target: target_id,
            instance: primary.map(|p| p.instance_ref.clone()),
            instance_key: primary.map(|p| p.instance_key.clone()),
            allocation_location: primary.and_then(|p| p.allocation_location.clone()),
            origin_kind: primary.map(|p| p.origin_kind),
            instances: resolved_infos,
            evidence: combined_evidence,
            gaps: outcome.gaps,
        });
    }

    Ok(rows)
}

pub fn run_project_instances(
    snapshot: &mut SnapshotState,
    project_id: &str,
    targets: &[String],
    select: &InstanceSelect,
) -> Result<Vec<InstanceProjectionRow>, RpcErr> {
    let mut entries = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let data = snapshot.get_instance(project_id, handle_id)?.clone();
        entries.push((handle_id.clone(), data));
    }

    let mut rows = Vec::with_capacity(entries.len());
    for (handle_id, data) in entries {
        let origin_syntax = ProjectedInstanceField::from_selected(
            select.origin_syntax,
            data.origin_syntax
                .map(|n| snapshot.alloc_syntax(project_id, n)),
        );
        rows.push(InstanceProjectionRow {
            instance: handle_id,
            gaps: Vec::new(),
            instance_key: select.instance_key.then_some(data.instance_key),
            origin_kind: select.origin_kind.then_some(data.origin_kind),
            allocation_location: ProjectedInstanceField::from_selected(
                select.allocation_location,
                data.allocation_location,
            ),
            origin_syntax,
            model_id: ProjectedInstanceField::from_selected(select.model_id, data.model_id),
            evidence: select.evidence.then_some(data.evidence),
        });
    }

    Ok(rows)
}
