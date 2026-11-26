use std::collections::BTreeSet;

use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use tsrs_ast::{
    Kind, Node, SourceFile, Symbol, get_source_file_of_node, is_function_like_declaration,
    is_type_node,
};
use tsrs_checker::TypeFormatFlags;
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;

use crate::calls_values::{
    canonical_symbol_for_node, node_byte_span, node_location_rel, node_text,
    pick_preferred_impl_decl, prop_key_text, resolve_call_targets,
    resolve_property_in_object_literal, unwrap_expr_wrappers,
};
use crate::cfg::{
    CleanupCfgStatus, EvidenceItem, GapItem, normalize_function_node, resolve_enclosing_function,
};
use crate::instance_identity::{
    CallFrame, CalleeDeclMetadata, InstanceHandleData, InstanceModelBehaviorInput,
    InstanceModelInput, InstanceModelMatchInput, InstanceResolver, ResolvedOrigin,
    collect_callee_decl_metadata, extract_call_receiver, is_external_declaration,
    score_model_match,
};
use crate::projects::{is_project_source_file, relative_to_cwd};
use crate::serve::{HandlePayload, RpcErr, SnapshotState, SourceLocation};

const MAX_EFFECT_CALL_DEPTH: usize = 12;

#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum CleanupModelTargetInput {
    Receiver(String),
    Argument {
        #[serde(rename = "argumentIndex")]
        argument_index: usize,
    },
}

impl CleanupModelTargetInput {
    fn resolve_node(&self, receiver: Option<P<Node>>, args: &[P<Node>]) -> Option<P<Node>> {
        match self {
            Self::Receiver(label) if label == "receiver" => receiver,
            Self::Receiver(_) => None,
            Self::Argument { argument_index } => args.get(*argument_index).copied(),
        }
    }
}

fn resolve_cleanup_target_node(
    spec: Option<&CleanupModelTargetInput>,
    default_receiver: bool,
    receiver: Option<P<Node>>,
    args: &[P<Node>],
) -> Option<P<Node>> {
    match spec {
        Some(target) => target.resolve_node(receiver, args),
        None if default_receiver => receiver,
        None => args.first().copied(),
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupModelOperationInput {
    pub kind: String,
    #[serde(rename = "match")]
    pub match_rule: InstanceModelMatchInput,
    #[serde(default)]
    pub target: Option<CleanupModelTargetInput>,
    #[serde(default)]
    pub container_target: Option<CleanupModelTargetInput>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleEventBehaviorInput {
    pub kind: String,
    #[serde(rename = "match")]
    pub match_rule: InstanceModelMatchInput,
    #[serde(default)]
    pub callback_argument_index: Option<usize>,
    #[serde(default)]
    pub event_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectsModelInput {
    pub id: String,
    #[serde(default)]
    pub instance_behaviors: Vec<InstanceModelBehaviorInput>,
    #[serde(default)]
    pub cleanup_operations: Vec<CleanupModelOperationInput>,
    #[serde(default)]
    pub lifecycle_events: Vec<LifecycleEventBehaviorInput>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardConditionPayload {
    pub branch_syntax: String,
    pub condition: String,
    pub condition_text: String,
    pub direction: &'static str,
    pub location: SourceLocation,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldEffectPayload {
    pub kind: &'static str,
    pub property_name: String,
    pub operator: Option<&'static str>,
    pub syntax: String,
    pub target_syntax: String,
    pub receiver_syntax: String,
    pub receiver_declaration: Option<String>,
    pub value_syntax: Option<String>,
    pub value_text: Option<String>,
    pub location: SourceLocation,
    pub via_call: Option<String>,
    pub enclosing_function: Option<String>,
    pub enclosing_block: Option<String>,
    pub cfg_scope: Option<String>,
    pub guards: Vec<GuardConditionPayload>,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallbackInvocationPayload {
    pub kind: &'static str,
    pub call: String,
    pub outer_call: String,
    pub syntax: String,
    pub callback_argument: String,
    pub parameter_index: Option<usize>,
    pub target_declaration: Option<String>,
    pub helper_declaration: Option<String>,
    pub resolved_targets: Vec<String>,
    pub enclosing_function: Option<String>,
    pub enclosing_block: Option<String>,
    pub cfg_scope: Option<String>,
    pub location: SourceLocation,
    pub outer_call_location: SourceLocation,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallEffectsRow {
    pub target: String,
    pub field_reads: Vec<FieldEffectPayload>,
    pub field_writes: Vec<FieldEffectPayload>,
    pub effects: Vec<FieldEffectPayload>,
    pub callback_invocations: Vec<CallbackInvocationPayload>,
    pub calls: Vec<String>,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupSitePayload {
    pub instance: String,
    pub instance_key: String,
    pub allocation_location: Option<SourceLocation>,
    pub operation_kind: String,
    pub syntax: String,
    pub location: SourceLocation,
    pub via_call: Option<String>,
    pub via_call_location: Option<SourceLocation>,
    pub model_id: String,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInstanceCleanupRow {
    pub instance: String,
    pub instance_key: String,
    pub allocation_location: Option<SourceLocation>,
    pub status: &'static str,
    pub cleaned_on_all_paths: bool,
    pub cleaned_on_any_path: bool,
    pub cleanup_sites: Vec<CleanupSitePayload>,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceCleanupScopeRow {
    pub scope: String,
    pub resolved_functions: Vec<String>,
    pub target_results: Vec<TargetInstanceCleanupRow>,
    pub disposed_instances: Vec<CleanupSitePayload>,
    pub registered_instances: Vec<CleanupSitePayload>,
    pub detached_instances: Vec<CleanupSitePayload>,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

fn to_instance_models(models: &[EffectsModelInput]) -> Vec<InstanceModelInput> {
    models
        .iter()
        .filter(|m| !m.instance_behaviors.is_empty())
        .map(|m| InstanceModelInput {
            id: m.id.clone(),
            instance_behaviors: m.instance_behaviors.clone(),
        })
        .collect()
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
    let dup = evidence.iter().any(|e| {
        e.kind == item.kind
            && e.message == item.message
            && e.at.as_ref().map(|a| (&a.file, a.start, a.end))
                == item.at.as_ref().map(|a| (&a.file, a.start, a.end))
    });
    if !dup {
        evidence.push(item);
    }
}

fn resolve_target_node(
    snapshot: &SnapshotState,
    project_id: &str,
    handle_id: &str,
) -> Result<P<Node>, RpcErr> {
    let entry = snapshot.get_entry(project_id, handle_id)?;
    match entry.payload {
        HandlePayload::Syntax(node)
        | HandlePayload::Call(node)
        | HandlePayload::Declaration { node, .. } => Ok(node),
        HandlePayload::Signature { .. }
        | HandlePayload::Type { .. }
        | HandlePayload::Branch(_)
        | HandlePayload::Instance(_) => Err(RpcErr::new(
            "INVALID_REFERENCE_KIND",
            format!("Program reference {handle_id} cannot be analyzed for call effects."),
        )),
    }
}

fn find_matching_cleanup_op<'a>(
    models: &'a [EffectsModelInput],
    meta: &CalleeDeclMetadata,
    allowed_kinds: &[&str],
) -> Option<(&'a str, &'a CleanupModelOperationInput)> {
    let mut best_score = 0usize;
    let mut best: Option<(&'a str, &'a CleanupModelOperationInput)> = None;

    for model in models {
        for op in &model.cleanup_operations {
            if !allowed_kinds.contains(&op.kind.as_str()) {
                continue;
            }
            if let Some(score) = score_model_match(&op.match_rule, meta)
                && score > best_score
            {
                best_score = score;
                best = Some((model.id.as_str(), op));
            }
        }
    }
    best
}

fn find_matching_lifecycle_event<'a>(
    models: &'a [EffectsModelInput],
    meta: &CalleeDeclMetadata,
) -> Option<(&'a str, &'a LifecycleEventBehaviorInput)> {
    let mut best_score = 0usize;
    let mut best: Option<(&'a str, &'a LifecycleEventBehaviorInput)> = None;

    for model in models {
        for ev in &model.lifecycle_events {
            if ev.kind == "lifecycle-end-callback"
                && let Some(score) = score_model_match(&ev.match_rule, meta)
                && score > best_score
            {
                best_score = score;
                best = Some((model.id.as_str(), ev));
            }
        }
    }
    best
}

fn collect_member_write_metadata(
    cwd: &str,
    checker: &mut CheckerHandle,
    recv: P<Node>,
    prop_name: &str,
    target_node: P<Node>,
) -> CalleeDeclMetadata {
    let mut container_names = Vec::new();
    let recv_ty = checker.get_type_at_location(recv);
    let mut decl_file: Option<(String, String)> = None;

    if let Some(type_sym) = recv_ty.symbol() {
        let name = type_sym.name().to_string();
        if !name.is_empty() && name != "__type" {
            container_names.push(name);
        }
        if let Some(decl) = pick_preferred_impl_decl(type_sym, None)
            && let Some(sf) = get_source_file_of_node(decl)
        {
            let abs = sf.file_name().to_string();
            let rel = relative_to_cwd(cwd, &abs);
            decl_file = Some((rel, abs));
        }
    }

    let type_str =
        checker.type_to_string_ex_exported(recv_ty, Some(recv), TypeFormatFlags::None, None);
    let base = type_str.split('<').next().unwrap_or(&type_str).trim();
    if !base.is_empty()
        && !base.starts_with('{')
        && base != "any"
        && base != "unknown"
        && !container_names.iter().any(|c| c == base)
    {
        container_names.push(base.to_string());
    }

    let mut qualified_names = Vec::new();
    for c in &container_names {
        let q = format!("{c}.{prop_name}");
        if !qualified_names.contains(&q) {
            qualified_names.push(q);
        }
    }

    let (rel_file, abs_file) = decl_file.unwrap_or_else(|| {
        get_source_file_of_node(target_node).map_or_else(
            || (String::new(), String::new()),
            |sf| {
                let abs = sf.file_name().to_string();
                let rel = relative_to_cwd(cwd, &abs);
                (rel, abs)
            },
        )
    });

    let display_label = qualified_names
        .first()
        .cloned()
        .unwrap_or_else(|| prop_name.to_string());

    CalleeDeclMetadata {
        member_name: Some(prop_name.to_string()),
        container_names,
        qualified_names,
        display_label,
        rel_file,
        abs_file,
    }
}

fn collect_call_callee_metadata(
    cwd: &str,
    checker: &mut CheckerHandle,
    call_node: P<Node>,
) -> Vec<(P<Node>, CalleeDeclMetadata)> {
    let mut out = Vec::new();
    let mut seen_decls = FxHashSet::default();

    if let Some(callee_expr) = call_node.expression() {
        let unwrapped = unwrap_expr_wrappers(callee_expr);
        if let Some(sym) = canonical_symbol_for_node(checker, unwrapped)
            && let Some(decl) = pick_preferred_impl_decl(sym, None)
            && seen_decls.insert(decl)
        {
            out.push((
                decl,
                collect_callee_decl_metadata(cwd, checker, decl, call_node),
            ));
        }
    }

    let sig = checker.get_resolved_signature_exported(call_node);
    if sig != checker.get_unknown_signature()
        && let Some(sig_decl) = sig
            .declaration()
            .or_else(|| sig.target().and_then(|t| t.declaration()))
        && seen_decls.insert(sig_decl)
    {
        out.push((
            sig_decl,
            collect_callee_decl_metadata(cwd, checker, sig_decl, call_node),
        ));
    }

    out
}

fn extract_member_access(node: P<Node>) -> Option<(P<Node>, P<Node>, String)> {
    let unwrapped = unwrap_expr_wrappers(node);
    match unwrapped.kind() {
        Kind::PropertyAccessExpression => {
            let pa = unwrapped.as_property_access_expression();
            let prop_name = node_text(pa.name())?;
            Some((unwrapped, pa.expression(), prop_name))
        }
        Kind::ElementAccessExpression => {
            let elem = unwrapped.as_element_access_expression();
            let arg = unwrap_expr_wrappers(elem.argument_expression());
            if matches!(
                arg.kind(),
                Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
            ) && let Some(prop_name) = prop_key_text(arg)
            {
                return Some((unwrapped, elem.expression(), prop_name));
            }
            None
        }
        _ => None,
    }
}

const fn assignment_operator_str(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::EqualsToken => Some("="),
        Kind::BarEqualsToken => Some("|="),
        Kind::AmpersandEqualsToken => Some("&="),
        Kind::CaretEqualsToken => Some("^="),
        Kind::PlusEqualsToken => Some("+="),
        Kind::MinusEqualsToken => Some("-="),
        Kind::AsteriskEqualsToken => Some("*="),
        Kind::SlashEqualsToken => Some("/="),
        Kind::PercentEqualsToken => Some("%="),
        Kind::AsteriskAsteriskEqualsToken => Some("**="),
        Kind::LessThanLessThanEqualsToken => Some("<<="),
        Kind::GreaterThanGreaterThanEqualsToken => Some(">>="),
        Kind::GreaterThanGreaterThanGreaterThanEqualsToken => Some(">>>="),
        Kind::AmpersandAmpersandEqualsToken => Some("&&="),
        Kind::BarBarEqualsToken => Some("||="),
        Kind::QuestionQuestionEqualsToken => Some("??="),
        _ => None,
    }
}

#[derive(Clone)]
struct RegistrationSite {
    call_node: P<Node>,
    enclosing_fn: Option<P<Node>>,
    container_expr: P<Node>,
    item_expr: P<Node>,
    model_id: String,
    description: String,
}

#[derive(Clone)]
struct SlotWriteSite {
    write_node: P<Node>,
    enclosing_fn: Option<P<Node>>,
    slot_expr: P<Node>,
    value_expr: P<Node>,
    model_id: String,
    description: String,
    start: u32,
}

struct CleanupProjectIndex {
    registrations: Vec<RegistrationSite>,
    slot_writes: Vec<SlotWriteSite>,
}

impl CleanupProjectIndex {
    fn build(
        cwd: &str,
        program: &'static Program,
        checker: &mut CheckerHandle,
        models: &[EffectsModelInput],
        lookup_files: Option<&[String]>,
    ) -> Self {
        let mut registrations = Vec::new();
        let mut slot_writes = Vec::new();
        let include_registrations = models.iter().any(|model| {
            model
                .cleanup_operations
                .iter()
                .any(|operation| operation.kind == "registers-into-container")
        });
        let include_slot_writes = models.iter().any(|model| {
            model
                .cleanup_operations
                .iter()
                .any(|operation| operation.kind == "replaces-previous-value")
        });
        let lookup_file_set =
            lookup_files.map(|files| files.iter().map(String::as_str).collect::<FxHashSet<_>>());

        let mut source_files: Vec<(String, P<SourceFile>)> = program
            .get_source_files()
            .iter()
            .copied()
            .filter(|&sf| is_project_source_file(program, sf))
            .map(|sf| (relative_to_cwd(cwd, sf.file_name()), sf))
            .filter(|(file, _)| {
                lookup_file_set
                    .as_ref()
                    .is_none_or(|lookup_files| lookup_files.contains(file.as_str()))
            })
            .collect();
        source_files.sort_by(|a, b| a.0.cmp(&b.0));

        for (_rel, sf) in source_files {
            collect_cleanup_index_in_node(
                cwd,
                checker,
                models,
                include_registrations,
                include_slot_writes,
                sf.as_node(),
                &mut registrations,
                &mut slot_writes,
            );
        }

        Self {
            registrations,
            slot_writes,
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Recursive cleanup indexing carries file scope, operation filters, and both result lists."
)]
fn collect_cleanup_index_in_node(
    cwd: &str,
    checker: &mut CheckerHandle,
    models: &[EffectsModelInput],
    include_registrations: bool,
    include_slot_writes: bool,
    node: P<Node>,
    registrations: &mut Vec<RegistrationSite>,
    slot_writes: &mut Vec<SlotWriteSite>,
) {
    match node.kind() {
        Kind::CallExpression if include_registrations => {
            let metas = collect_call_callee_metadata(cwd, checker, node);
            let recv = node
                .expression()
                .and_then(|e| extract_call_receiver(unwrap_expr_wrappers(e)));
            let args = node.arguments();
            for (_, meta) in &metas {
                if let Some((model_id, op)) =
                    find_matching_cleanup_op(models, meta, &["registers-into-container"])
                {
                    if let (Some(container_expr), Some(item_expr)) = (
                        resolve_cleanup_target_node(op.container_target.as_ref(), true, recv, args),
                        resolve_cleanup_target_node(op.target.as_ref(), false, recv, args),
                    ) {
                        registrations.push(RegistrationSite {
                            call_node: node,
                            enclosing_fn: resolve_enclosing_function(node),
                            container_expr,
                            item_expr,
                            model_id: model_id.to_string(),
                            description: op.description.clone().unwrap_or_else(|| {
                                format!("Registered into container via '{}'", meta.display_label)
                            }),
                        });
                    }
                    break;
                }
            }
        }
        Kind::BinaryExpression if include_slot_writes => {
            let bin = node.as_binary_expression();
            if bin.operator_token().kind() == Kind::EqualsToken
                && let Some((_, recv_expr, prop_name)) = extract_member_access(bin.left())
            {
                let meta = collect_member_write_metadata(cwd, checker, recv_expr, &prop_name, node);
                if let Some((model_id, op)) =
                    find_matching_cleanup_op(models, &meta, &["replaces-previous-value"])
                {
                    let (start, _) = node_byte_span(node);
                    slot_writes.push(SlotWriteSite {
                        write_node: node,
                        enclosing_fn: resolve_enclosing_function(node),
                        slot_expr: recv_expr,
                        value_expr: bin.right(),
                        model_id: model_id.to_string(),
                        description: op.description.clone().unwrap_or_else(|| {
                            format!("Assigned held value on '{}'", meta.display_label)
                        }),
                        start,
                    });
                }
            }
        }
        _ => {}
    }

    node.for_each_child(&mut |child| {
        collect_cleanup_index_in_node(
            cwd,
            checker,
            models,
            include_registrations,
            include_slot_writes,
            child,
            registrations,
            slot_writes,
        );
        false
    });
}

#[derive(Clone)]
struct RecordedCleanupAction {
    origin: ResolvedOrigin,
    operation_kind: String,
    top_site_node: P<Node>,
    actual_site_node: P<Node>,
    location: SourceLocation,
    via_call: Option<P<Node>>,
    model_id: String,
    evidence: Vec<EvidenceItem>,
}

#[derive(Clone)]
struct RecordedDynamicGap {
    top_site_node: P<Node>,
    gap: GapItem,
}

#[derive(Clone)]
struct ResolvedCallable {
    fn_node: P<Node>,
    captured_stack: Vec<CallFrame>,
}

struct EffectsEngine<'a> {
    snapshot: &'a mut SnapshotState,
    project_id: &'a str,
    lookup_files: Option<&'a [String]>,
    cwd: String,
    program: &'static Program,
    checker: &'a mut CheckerHandle,
    instance_models: Vec<InstanceModelInput>,
    models: &'a [EffectsModelInput],
    cleanup_index: Option<CleanupProjectIndex>,
    cleanup_index_enabled: bool,
}

impl<'a> EffectsEngine<'a> {
    fn new(
        snapshot: &'a mut SnapshotState,
        project_id: &'a str,
        lookup_files: Option<&'a [String]>,
        program: &'static Program,
        checker: &'a mut CheckerHandle,
        models: &'a [EffectsModelInput],
        build_cleanup_index: bool,
    ) -> Self {
        let cwd = snapshot.cwd.clone();
        let has_registration_models = models.iter().any(|model| {
            model
                .cleanup_operations
                .iter()
                .any(|operation| operation.kind == "registers-into-container")
        });
        let has_slot_models = models.iter().any(|model| {
            model
                .cleanup_operations
                .iter()
                .any(|operation| operation.kind == "replaces-previous-value")
        });
        let cleanup_index_enabled =
            build_cleanup_index && (has_registration_models || has_slot_models);
        let instance_models = to_instance_models(models);
        Self {
            snapshot,
            project_id,
            lookup_files,
            cwd,
            program,
            checker,
            instance_models,
            models,
            cleanup_index: None,
            cleanup_index_enabled,
        }
    }

    fn cleanup_index(&mut self) -> Option<&CleanupProjectIndex> {
        if self.cleanup_index.is_none() && self.cleanup_index_enabled {
            self.cleanup_index = Some(CleanupProjectIndex::build(
                &self.cwd,
                self.program,
                self.checker,
                self.models,
                self.lookup_files,
            ));
        }
        self.cleanup_index.as_ref()
    }

    fn resolve_expr_instances(
        &mut self,
        expr: P<Node>,
        call_stack: &[CallFrame],
    ) -> Vec<ResolvedOrigin> {
        let mut resolver = InstanceResolver {
            cwd: &self.cwd,
            program: self.program,
            checker: self.checker,
            snapshot: self.snapshot,
            project_id: self.project_id,
            lookup_files: self.lookup_files,
            call_graph: None,
            field_index: None,
            models: &self.instance_models,
        };
        resolver.resolve_with_stack(expr, call_stack).origins
    }

    fn resolve_callable_expr(
        &mut self,
        raw_expr: P<Node>,
        call_stack: &[CallFrame],
    ) -> (Vec<ResolvedCallable>, Vec<GapItem>) {
        let expr = unwrap_expr_wrappers(raw_expr);
        if is_function_like_declaration(Some(expr)) && expr.body().is_some() {
            return (
                vec![ResolvedCallable {
                    fn_node: expr,
                    captured_stack: call_stack.to_vec(),
                }],
                Vec::new(),
            );
        }

        let outcome = InstanceResolver {
            cwd: &self.cwd,
            program: self.program,
            checker: self.checker,
            snapshot: self.snapshot,
            project_id: self.project_id,
            lookup_files: self.lookup_files,
            call_graph: None,
            field_index: None,
            models: &self.instance_models,
        }
        .resolve_with_stack(expr, call_stack);
        let mut callables = Vec::new();
        let mut seen = FxHashSet::default();
        for origin in outcome.origins {
            if let Some(origin_node) = origin.origin_node
                && is_function_like_declaration(Some(origin_node))
                && origin_node.body().is_some()
                && seen.insert(origin_node)
            {
                callables.push(ResolvedCallable {
                    fn_node: origin_node,
                    captured_stack: origin.captured_stack,
                });
            }
        }
        (callables, outcome.gaps)
    }

    fn resolve_call_callees_fallback(
        &mut self,
        unwrapped: P<Node>,
        call_stack: &[CallFrame],
        callables: &mut Vec<ResolvedCallable>,
        seen: &mut FxHashSet<P<Node>>,
        gaps: &mut Vec<GapItem>,
    ) {
        if let Some((_, recv_expr, prop_name)) = extract_member_access(unwrapped) {
            let recv_origins = self.resolve_expr_instances(recv_expr, call_stack);
            for origin in recv_origins {
                if let Some(origin_node) = origin.origin_node
                    && origin_node.kind() == Kind::ObjectLiteralExpression
                    && let Some(prop_val) =
                        resolve_property_in_object_literal(origin_node, &prop_name)
                {
                    let (prop_callables, prop_gaps) =
                        self.resolve_callable_expr(prop_val, &origin.captured_stack);
                    for c in prop_callables {
                        if seen.insert(c.fn_node) {
                            callables.push(c);
                        }
                    }
                    for g in prop_gaps {
                        push_unique_gap(gaps, g);
                    }
                }
            }
        } else {
            let (expr_callables, expr_gaps) = self.resolve_callable_expr(unwrapped, call_stack);
            for c in expr_callables {
                if seen.insert(c.fn_node) {
                    callables.push(c);
                }
            }
            if !callables.is_empty() {
                for g in expr_gaps {
                    push_unique_gap(gaps, g);
                }
            }
        }
    }

    fn resolve_call_callees(
        &mut self,
        call_node: P<Node>,
        call_stack: &[CallFrame],
    ) -> (Vec<ResolvedCallable>, Option<usize>, Vec<GapItem>) {
        let Some(callee_expr) = call_node.expression() else {
            return (
                Vec::new(),
                None,
                vec![GapItem {
                    kind: "dynamic-callee",
                    message: "Call expression has no callee expression.".to_string(),
                    at: node_location_rel(&self.cwd, call_node),
                }],
            );
        };

        let unwrapped = unwrap_expr_wrappers(callee_expr);

        // 1. Check if callee is a parameter of an active CallFrame (callback parameter, e.g. `fn(...args)`).
        if unwrapped.kind() == Kind::Identifier
            && let Some(sym) = canonical_symbol_for_node(self.checker, unwrapped)
            && let Some(decl) = pick_preferred_impl_decl(sym, None)
            && decl.kind() == Kind::Parameter
            && let Some(enclosing_fn) = resolve_enclosing_function(decl)
            && let Some(param_idx) = enclosing_fn.parameters().iter().position(|&p| p == decl)
            && let Some(frame_pos) = call_stack
                .iter()
                .rposition(|f| f.callee_fn == Some(enclosing_fn))
        {
            let frame = &call_stack[frame_pos];
            let parent_stack = &call_stack[..frame_pos];
            if let Some(&arg_expr) = frame.call_node.arguments().get(param_idx) {
                let (callables, mut gaps) = self.resolve_callable_expr(arg_expr, parent_stack);
                if callables.is_empty() {
                    let arg_text = node_text(arg_expr).unwrap_or_else(|| "callback".to_string());
                    push_unique_gap(
                        &mut gaps,
                        GapItem {
                            kind: "dynamic-callee",
                            message: format!(
                                "Callback argument '{arg_text}' is invoked synchronously but its concrete body cannot be statically resolved."
                            ),
                            at: node_location_rel(&self.cwd, call_node),
                        },
                    );
                }
                return (callables, Some(param_idx), gaps);
            }
        }

        // 2. Standard static AST/symbol call target resolution.
        let outcome = resolve_call_targets(&self.cwd, self.program, self.checker, call_node);
        let mut callables = Vec::new();
        let mut seen = FxHashSet::default();
        let mut gaps = Vec::new();

        for target in outcome.targets {
            if let Some(fn_node) = target.fn_node
                && fn_node.body().is_some()
                && !is_external_declaration(self.program, fn_node)
                && seen.insert(fn_node)
            {
                callables.push(ResolvedCallable {
                    fn_node,
                    captured_stack: call_stack.to_vec(),
                });
            }
        }

        // 3. Fallback: object literal method returned by helper or closure variable (e.g. `wrapper.dispose()`).
        if callables.is_empty() {
            self.resolve_call_callees_fallback(
                unwrapped,
                call_stack,
                &mut callables,
                &mut seen,
                &mut gaps,
            );
        }

        for g in outcome.gaps {
            push_unique_gap(&mut gaps, g);
        }

        (callables, None, gaps)
    }

    fn expand_container_children(
        &mut self,
        container_origin: &ResolvedOrigin,
        dispose_node: P<Node>,
        dispose_sf: Option<P<SourceFile>>,
        dispose_start: u32,
        detached_pairs: &BTreeSet<(String, String)>,
    ) -> Vec<(ResolvedOrigin, String, EvidenceItem)> {
        let mut children = Vec::new();
        let Some((registrations, slot_writes)) = self
            .cleanup_index()
            .map(|idx| (idx.registrations.clone(), idx.slot_writes.clone()))
        else {
            return children;
        };

        let container_fn = container_origin
            .origin_node
            .and_then(resolve_enclosing_function);

        for reg in registrations {
            let mut stacks: Vec<Vec<CallFrame>> = vec![Vec::new()];
            if !container_origin.captured_stack.is_empty() && reg.enclosing_fn == container_fn {
                stacks.push(container_origin.captured_stack.clone());
            }
            for stack in stacks {
                let reg_containers = self.resolve_expr_instances(reg.container_expr, &stack);
                if !reg_containers
                    .iter()
                    .any(|o| o.instance_key == container_origin.instance_key)
                {
                    continue;
                }
                let reg_items = self.resolve_expr_instances(reg.item_expr, &stack);
                let reg_loc = node_location_rel(&self.cwd, reg.call_node);
                for item in reg_items {
                    if detached_pairs.contains(&(
                        container_origin.instance_key.clone(),
                        item.instance_key.clone(),
                    )) {
                        continue;
                    }
                    children.push((
                        item,
                        reg.model_id.clone(),
                        EvidenceItem::model(
                            reg.description.clone(),
                            reg_loc.clone(),
                            reg.model_id.clone(),
                            "container-registration".to_string(),
                        ),
                    ));
                }
            }
        }

        for sw in slot_writes {
            if sw.write_node == dispose_node {
                continue;
            }
            if let (Some(d_sf), Some(s_sf)) = (dispose_sf, get_source_file_of_node(sw.write_node))
                && d_sf == s_sf
                && sw.start > dispose_start
            {
                continue;
            }
            let mut stacks: Vec<Vec<CallFrame>> = vec![Vec::new()];
            if !container_origin.captured_stack.is_empty() && sw.enclosing_fn == container_fn {
                stacks.push(container_origin.captured_stack.clone());
            }
            for stack in stacks {
                let sw_slots = self.resolve_expr_instances(sw.slot_expr, &stack);
                if !sw_slots
                    .iter()
                    .any(|o| o.instance_key == container_origin.instance_key)
                {
                    continue;
                }
                let sw_items = self.resolve_expr_instances(sw.value_expr, &stack);
                let sw_loc = node_location_rel(&self.cwd, sw.write_node);
                for item in sw_items {
                    if detached_pairs.contains(&(
                        container_origin.instance_key.clone(),
                        item.instance_key.clone(),
                    )) {
                        continue;
                    }
                    children.push((
                        item,
                        sw.model_id.clone(),
                        EvidenceItem::model(
                            sw.description.clone(),
                            sw_loc.clone(),
                            sw.model_id.clone(),
                            "slot-held-value".to_string(),
                        ),
                    ));
                }
            }
        }

        children
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Recursive cleanup traversal carries scope, call stack, top site anchor, and output accumulators."
    )]
    fn collect_cleanup_in_fn(
        &mut self,
        fn_node: P<Node>,
        call_stack: &[CallFrame],
        top_site_override: Option<P<Node>>,
        via_call: Option<P<Node>>,
        chain_evidence: &[EvidenceItem],
        depth: usize,
        visited: &mut FxHashSet<(P<Node>, Vec<P<Node>>)>,
        disposals: &mut Vec<RecordedCleanupAction>,
        registrations: &mut Vec<RecordedCleanupAction>,
        detachments: &mut Vec<RecordedCleanupAction>,
        detached_pairs: &mut BTreeSet<(String, String)>,
        dynamic_gaps: &mut Vec<RecordedDynamicGap>,
    ) {
        if depth >= MAX_EFFECT_CALL_DEPTH {
            if let Some(top) = top_site_override {
                dynamic_gaps.push(RecordedDynamicGap {
                    top_site_node: top,
                    gap: GapItem {
                        kind: "call-depth-limit",
                        message: "Call effect traversal reached maximum recursion depth."
                            .to_string(),
                        at: node_location_rel(&self.cwd, fn_node),
                    },
                });
            }
            return;
        }
        let stack_key: Vec<P<Node>> = call_stack.iter().map(|f| f.call_node).collect();
        if !visited.insert((fn_node, stack_key)) {
            return;
        }
        let Some(body) = fn_node.body() else {
            return;
        };
        self.collect_cleanup_in_ast(
            body,
            true,
            call_stack,
            top_site_override,
            via_call,
            chain_evidence,
            depth,
            visited,
            disposals,
            registrations,
            detachments,
            detached_pairs,
            dynamic_gaps,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "AST cleanup walk inspects calls, property writes, and transitive helpers in source order."
    )]
    fn collect_cleanup_in_ast(
        &mut self,
        node: P<Node>,
        is_body_root: bool,
        call_stack: &[CallFrame],
        top_site_override: Option<P<Node>>,
        via_call: Option<P<Node>>,
        chain_evidence: &[EvidenceItem],
        depth: usize,
        visited: &mut FxHashSet<(P<Node>, Vec<P<Node>>)>,
        disposals: &mut Vec<RecordedCleanupAction>,
        registrations: &mut Vec<RecordedCleanupAction>,
        detachments: &mut Vec<RecordedCleanupAction>,
        detached_pairs: &mut BTreeSet<(String, String)>,
        dynamic_gaps: &mut Vec<RecordedDynamicGap>,
    ) {
        if !is_body_root
            && (is_function_like_declaration(Some(node))
                || matches!(node.kind(), Kind::ClassDeclaration | Kind::ClassExpression)
                || is_type_node(node))
        {
            return;
        }

        // Visit children first so nested call arguments (if any) execute before outer expression.
        let mut children = Vec::new();
        node.for_each_child(&mut |child| {
            children.push(child);
            false
        });
        for child in children {
            self.collect_cleanup_in_ast(
                child,
                false,
                call_stack,
                top_site_override,
                via_call,
                chain_evidence,
                depth,
                visited,
                disposals,
                registrations,
                detachments,
                detached_pairs,
                dynamic_gaps,
            );
        }

        let top_site = top_site_override.unwrap_or(node);

        match node.kind() {
            Kind::CallExpression => {
                let metas = collect_call_callee_metadata(&self.cwd, self.checker, node);
                let recv = node
                    .expression()
                    .and_then(|e| extract_call_receiver(unwrap_expr_wrappers(e)));
                let args = node.arguments();
                let call_loc = node_location_rel(&self.cwd, node);
                let mut matched_model_cleanup = false;

                for (_, meta) in &metas {
                    if let Some((model_id, op)) = find_matching_cleanup_op(
                        self.models,
                        meta,
                        &[
                            "disposes-target",
                            "registers-into-container",
                            "detaches-from-container",
                        ],
                    ) {
                        matched_model_cleanup = true;
                        let default_target_is_recv = op.kind == "disposes-target";
                        if let Some(target_expr) = resolve_cleanup_target_node(
                            op.target.as_ref(),
                            default_target_is_recv,
                            recv,
                            args,
                        ) {
                            let target_origins =
                                self.resolve_expr_instances(target_expr, call_stack);
                            let model_ev = EvidenceItem::model(
                                op.description.clone().unwrap_or_else(|| {
                                    format!(
                                        "Model '{model_id}' declares '{}' performs '{}'",
                                        meta.display_label, op.kind
                                    )
                                }),
                                call_loc.clone(),
                                model_id.to_string(),
                                meta.display_label.clone(),
                            );

                            match op.kind.as_str() {
                                "disposes-target" => {
                                    let (dispose_start, _) = node_byte_span(node);
                                    let dispose_sf = get_source_file_of_node(node);
                                    for origin in target_origins {
                                        let mut ev = chain_evidence.to_vec();
                                        ev.push(model_ev.clone());
                                        if let Some(ref loc) = call_loc {
                                            disposals.push(RecordedCleanupAction {
                                                origin: origin.clone(),
                                                operation_kind: "disposes-target".to_string(),
                                                top_site_node: top_site,
                                                actual_site_node: node,
                                                location: loc.clone(),
                                                via_call: via_call.or_else(|| {
                                                    (top_site != node).then_some(top_site)
                                                }),
                                                model_id: model_id.to_string(),
                                                evidence: ev.clone(),
                                            });
                                        }
                                        let children = self.expand_container_children(
                                            &origin,
                                            node,
                                            dispose_sf,
                                            dispose_start,
                                            detached_pairs,
                                        );
                                        for (child_origin, child_model, reg_ev) in children {
                                            if let Some(ref loc) = call_loc {
                                                let mut child_ev = ev.clone();
                                                child_ev.push(reg_ev);
                                                disposals.push(RecordedCleanupAction {
                                                    origin: child_origin,
                                                    operation_kind: "disposes-target".to_string(),
                                                    top_site_node: top_site,
                                                    actual_site_node: node,
                                                    location: loc.clone(),
                                                    via_call: via_call.or_else(|| {
                                                        (top_site != node).then_some(top_site)
                                                    }),
                                                    model_id: child_model,
                                                    evidence: child_ev,
                                                });
                                            }
                                        }
                                    }
                                }
                                "registers-into-container" => {
                                    for origin in target_origins {
                                        if let Some(ref loc) = call_loc {
                                            let mut ev = chain_evidence.to_vec();
                                            ev.push(model_ev.clone());
                                            registrations.push(RecordedCleanupAction {
                                                origin,
                                                operation_kind: "registers-into-container"
                                                    .to_string(),
                                                top_site_node: top_site,
                                                actual_site_node: node,
                                                location: loc.clone(),
                                                via_call,
                                                model_id: model_id.to_string(),
                                                evidence: ev,
                                            });
                                        }
                                    }
                                }
                                "detaches-from-container" => {
                                    let container_keys: Vec<String> = resolve_cleanup_target_node(
                                        op.container_target.as_ref(),
                                        true,
                                        recv,
                                        args,
                                    )
                                    .map_or_default(|ce| {
                                        self.resolve_expr_instances(ce, call_stack)
                                            .into_iter()
                                            .map(|o| o.instance_key)
                                            .collect()
                                    });
                                    for origin in target_origins {
                                        for ckey in &container_keys {
                                            detached_pairs.insert((
                                                ckey.clone(),
                                                origin.instance_key.clone(),
                                            ));
                                        }
                                        if let Some(ref loc) = call_loc {
                                            let mut ev = chain_evidence.to_vec();
                                            ev.push(model_ev.clone());
                                            detachments.push(RecordedCleanupAction {
                                                origin,
                                                operation_kind: "detaches-from-container"
                                                    .to_string(),
                                                top_site_node: top_site,
                                                actual_site_node: node,
                                                location: loc.clone(),
                                                via_call,
                                                model_id: model_id.to_string(),
                                                evidence: ev,
                                            });
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        break;
                    }
                }

                // Follow source implementations (helpers, wrappers, callbacks) regardless of whether
                // an outer wrapper `.dispose()` matched a model, so `{ dispose: () => store.dispose() }` expands.
                let (callees, _, call_gaps) = self.resolve_call_callees(node, call_stack);
                for callee in callees {
                    let fn_label = callee
                        .fn_node
                        .name()
                        .and_then(node_text)
                        .unwrap_or_else(|| {
                            node.expression()
                                .and_then(node_text)
                                .unwrap_or_else(|| "<callback>".to_string())
                        });
                    let mut next_stack = callee.captured_stack;
                    next_stack.push(CallFrame {
                        call_node: node,
                        callee_fn: Some(callee.fn_node),
                        receiver_expr: recv,
                    });
                    let mut next_ev = chain_evidence.to_vec();
                    next_ev.push(EvidenceItem::simple(
                        "source",
                        format!("Called '{fn_label}'"),
                        call_loc.clone(),
                    ));
                    self.collect_cleanup_in_fn(
                        callee.fn_node,
                        &next_stack,
                        Some(top_site),
                        Some(via_call.unwrap_or(node)),
                        &next_ev,
                        depth + 1,
                        visited,
                        disposals,
                        registrations,
                        detachments,
                        detached_pairs,
                        dynamic_gaps,
                    );
                }
                if !matched_model_cleanup {
                    for gap in call_gaps {
                        dynamic_gaps.push(RecordedDynamicGap {
                            top_site_node: top_site,
                            gap,
                        });
                    }
                }
            }
            Kind::BinaryExpression => {
                let bin = node.as_binary_expression();
                if bin.operator_token().kind() == Kind::EqualsToken
                    && let Some((_, recv_expr, prop_name)) = extract_member_access(bin.left())
                {
                    let meta = collect_member_write_metadata(
                        &self.cwd,
                        self.checker,
                        recv_expr,
                        &prop_name,
                        node,
                    );
                    if let Some((model_id, op)) =
                        find_matching_cleanup_op(self.models, &meta, &["replaces-previous-value"])
                    {
                        let slot_origins = self.resolve_expr_instances(recv_expr, call_stack);
                        let rhs_keys: BTreeSet<String> = self
                            .resolve_expr_instances(bin.right(), call_stack)
                            .into_iter()
                            .map(|o| o.instance_key)
                            .collect();
                        let (write_start, _) = node_byte_span(node);
                        let write_sf = get_source_file_of_node(node);
                        let write_loc = node_location_rel(&self.cwd, node);

                        let model_ev = EvidenceItem::model(
                            op.description.clone().unwrap_or_else(|| {
                                format!(
                                    "Model '{model_id}' declares writing '{}' replaces and disposes previous value",
                                    meta.display_label
                                )
                            }),
                            write_loc.clone(),
                            model_id.to_string(),
                            meta.display_label.clone(),
                        );

                        if let Some(slot_writes) =
                            self.cleanup_index().map(|idx| idx.slot_writes.clone())
                        {
                            for slot_origin in &slot_origins {
                                for sw in &slot_writes {
                                    if sw.write_node == node {
                                        continue;
                                    }
                                    if let (Some(w_sf), Some(s_sf)) =
                                        (write_sf, get_source_file_of_node(sw.write_node))
                                        && w_sf == s_sf
                                        && sw.start > write_start
                                    {
                                        continue;
                                    }
                                    let sw_slots = self.resolve_expr_instances(sw.slot_expr, &[]);
                                    if !sw_slots
                                        .iter()
                                        .any(|o| o.instance_key == slot_origin.instance_key)
                                    {
                                        continue;
                                    }
                                    let prev_items =
                                        self.resolve_expr_instances(sw.value_expr, &[]);
                                    let sw_loc = node_location_rel(&self.cwd, sw.write_node);
                                    for prev in prev_items {
                                        if rhs_keys.contains(&prev.instance_key) {
                                            continue;
                                        }
                                        if let Some(ref loc) = write_loc {
                                            let mut ev = chain_evidence.to_vec();
                                            ev.push(EvidenceItem::simple(
                                                "source",
                                                format!(
                                                    "Previous value assigned to '{}' here",
                                                    meta.display_label
                                                ),
                                                sw_loc.clone(),
                                            ));
                                            ev.push(model_ev.clone());
                                            disposals.push(RecordedCleanupAction {
                                                origin: prev,
                                                operation_kind: "replaces-previous-value"
                                                    .to_string(),
                                                top_site_node: top_site,
                                                actual_site_node: node,
                                                location: loc.clone(),
                                                via_call,
                                                model_id: model_id.to_string(),
                                                evidence: ev,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn resolve_scope_functions(
        &mut self,
        scope_node: P<Node>,
    ) -> (Vec<ResolvedCallable>, Vec<EvidenceItem>, Vec<GapItem>) {
        if let Some(fn_node) = normalize_function_node(scope_node) {
            return (
                vec![ResolvedCallable {
                    fn_node,
                    captured_stack: Vec::new(),
                }],
                Vec::new(),
                Vec::new(),
            );
        }

        if scope_node.kind() == Kind::CallExpression {
            let metas = collect_call_callee_metadata(&self.cwd, self.checker, scope_node);
            let call_loc = node_location_rel(&self.cwd, scope_node);
            for (_, meta) in &metas {
                if let Some((model_id, ev_model)) = find_matching_lifecycle_event(self.models, meta)
                {
                    let cb_idx = ev_model.callback_argument_index.unwrap_or(0);
                    let mut evidence = vec![EvidenceItem::model(
                        ev_model.description.clone().unwrap_or_else(|| {
                            let event_suffix = ev_model
                                .event_name
                                .as_deref()
                                .map_or(String::new(), |n| format!(" ({n})"));
                            format!(
                                "Model '{model_id}' declares '{}' argument #{cb_idx} as lifecycle-end callback{event_suffix}",
                                meta.display_label
                            )
                        }),
                        call_loc.clone(),
                        model_id.to_string(),
                        meta.display_label.clone(),
                    )];
                    let Some(&cb_arg) = scope_node.arguments().get(cb_idx) else {
                        return (
                            Vec::new(),
                            evidence,
                            vec![GapItem {
                                kind: "missing-lifecycle-callback",
                                message: format!(
                                    "Lifecycle event '{}' expected callback at argument #{cb_idx}.",
                                    meta.display_label
                                ),
                                at: call_loc,
                            }],
                        );
                    };
                    let (callables, mut gaps) = self.resolve_callable_expr(cb_arg, &[]);
                    if callables.is_empty() {
                        push_unique_gap(
                            &mut gaps,
                            GapItem {
                                kind: "dynamic-lifecycle-callback",
                                message: format!(
                                    "Lifecycle-end callback for '{}' could not be statically resolved to a function body.",
                                    meta.display_label
                                ),
                                at: node_location_rel(&self.cwd, cb_arg),
                            },
                        );
                    } else if let Some(c) = callables.first()
                        && let Some(cb_loc) = node_location_rel(&self.cwd, c.fn_node)
                    {
                        evidence.push(EvidenceItem::simple(
                            "source",
                            "Resolved lifecycle-end callback function".to_string(),
                            Some(cb_loc),
                        ));
                    }
                    return (callables, evidence, gaps);
                }
            }

            let (callables, _, gaps) = self.resolve_call_callees(scope_node, &[]);
            let recv = scope_node
                .expression()
                .and_then(|e| extract_call_receiver(unwrap_expr_wrappers(e)));
            let wrapped: Vec<ResolvedCallable> = callables
                .into_iter()
                .map(|c| {
                    let mut next = c.captured_stack;
                    next.push(CallFrame {
                        call_node: scope_node,
                        callee_fn: Some(c.fn_node),
                        receiver_expr: recv,
                    });
                    ResolvedCallable {
                        fn_node: c.fn_node,
                        captured_stack: next,
                    }
                })
                .collect();
            return (wrapped, Vec::new(), gaps);
        }

        let (callables, gaps) = self.resolve_callable_expr(scope_node, &[]);
        (callables, Vec::new(), gaps)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Collects field reads/writes, callback invocations, and cross-file evidence across calls."
    )]
    fn collect_effects_in_fn(
        &mut self,
        fn_node: P<Node>,
        call_stack: &[CallFrame],
        opts: EffectTraversalOptions,
        depth: usize,
        via_call: Option<P<Node>>,
        chain_evidence: &[EvidenceItem],
        visited: &mut FxHashSet<(P<Node>, Vec<P<Node>>)>,
        out: &mut CollectedCallEffects,
    ) {
        if depth >= opts.max_depth {
            push_unique_gap(
                &mut out.gaps,
                GapItem {
                    kind: "call-depth-limit",
                    message: "Call effect traversal reached maximum recursion depth.".to_string(),
                    at: node_location_rel(&self.cwd, fn_node),
                },
            );
            return;
        }
        let stack_key: Vec<P<Node>> = call_stack.iter().map(|f| f.call_node).collect();
        if !visited.insert((fn_node, stack_key)) {
            return;
        }
        let Some(body) = fn_node.body() else {
            return;
        };
        self.collect_effects_in_ast(
            body,
            true,
            fn_node,
            call_stack,
            opts,
            depth,
            via_call,
            chain_evidence,
            visited,
            out,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Collects field reads/writes, structural branch guards, and transitive callback effects."
    )]
    fn collect_effects_in_ast(
        &mut self,
        node: P<Node>,
        is_body_root: bool,
        enclosing_fn: P<Node>,
        call_stack: &[CallFrame],
        opts: EffectTraversalOptions,
        depth: usize,
        via_call: Option<P<Node>>,
        chain_evidence: &[EvidenceItem],
        visited: &mut FxHashSet<(P<Node>, Vec<P<Node>>)>,
        out: &mut CollectedCallEffects,
    ) {
        if !is_body_root
            && (is_function_like_declaration(Some(node))
                || matches!(node.kind(), Kind::ClassDeclaration | Kind::ClassExpression)
                || is_type_node(node))
        {
            return;
        }

        match node.kind() {
            Kind::BinaryExpression => {
                let bin = node.as_binary_expression();
                if let Some(op_str) = assignment_operator_str(bin.operator_token().kind())
                    && let Some((_, recv_expr, prop_name)) = extract_member_access(bin.left())
                {
                    if opts.include_writes
                        && let Some(loc) = node_location_rel(&self.cwd, node)
                    {
                        let mut ev = chain_evidence.to_vec();
                        ev.push(EvidenceItem::simple(
                            "source",
                            format!("Wrote field '.{prop_name}' using '{op_str}'"),
                            Some(loc.clone()),
                        ));
                        out.writes.push(self.build_raw_field_effect(
                            "write",
                            prop_name,
                            Some(op_str),
                            node,
                            unwrap_expr_wrappers(bin.left()),
                            recv_expr,
                            Some(bin.right()),
                            node_text(bin.right()),
                            loc,
                            via_call,
                            enclosing_fn,
                            ev,
                        ));
                    }
                    // Still inspect receiver (e.g. `a.b.c = 1` reads `a.b`) and RHS (`bin.right()`).
                    if opts.include_reads {
                        self.collect_effects_in_ast(
                            recv_expr,
                            false,
                            enclosing_fn,
                            call_stack,
                            opts,
                            depth,
                            via_call,
                            chain_evidence,
                            visited,
                            out,
                        );
                    }
                    self.collect_effects_in_ast(
                        bin.right(),
                        false,
                        enclosing_fn,
                        call_stack,
                        opts,
                        depth,
                        via_call,
                        chain_evidence,
                        visited,
                        out,
                    );
                    return;
                }
            }
            Kind::PrefixUnaryExpression | Kind::PostfixUnaryExpression => {
                let (op_kind, operand) = if node.kind() == Kind::PrefixUnaryExpression {
                    let p = node.as_prefix_unary_expression();
                    (p.operator(), p.operand())
                } else {
                    let p = node.as_postfix_unary_expression();
                    (p.operator(), p.operand())
                };
                if matches!(op_kind, Kind::PlusPlusToken | Kind::MinusMinusToken)
                    && let Some((_, recv_expr, prop_name)) = extract_member_access(operand)
                {
                    let op_str = if op_kind == Kind::PlusPlusToken {
                        "++"
                    } else {
                        "--"
                    };
                    if opts.include_writes
                        && let Some(loc) = node_location_rel(&self.cwd, node)
                    {
                        let mut ev = chain_evidence.to_vec();
                        ev.push(EvidenceItem::simple(
                            "source",
                            format!("Updated field '.{prop_name}' using '{op_str}'"),
                            Some(loc.clone()),
                        ));
                        out.writes.push(self.build_raw_field_effect(
                            "write",
                            prop_name,
                            Some(op_str),
                            node,
                            unwrap_expr_wrappers(operand),
                            recv_expr,
                            None,
                            None,
                            loc,
                            via_call,
                            enclosing_fn,
                            ev,
                        ));
                    }
                    return;
                }
            }
            Kind::PropertyAccessExpression | Kind::ElementAccessExpression => {
                if opts.include_reads
                    && !is_direct_call_callee(node)
                    && let Some((_, recv_expr, prop_name)) = extract_member_access(node)
                    && let Some(loc) = node_location_rel(&self.cwd, node)
                {
                    let mut ev = chain_evidence.to_vec();
                    ev.push(EvidenceItem::simple(
                        "source",
                        format!("Read field '.{prop_name}'"),
                        Some(loc.clone()),
                    ));
                    out.reads.push(self.build_raw_field_effect(
                        "read",
                        prop_name,
                        None,
                        node,
                        node,
                        recv_expr,
                        None,
                        None,
                        loc,
                        via_call,
                        enclosing_fn,
                        ev,
                    ));
                }
            }
            Kind::CallExpression => {
                out.calls.push(node);
                let call_loc = node_location_rel(&self.cwd, node);
                let recv = node
                    .expression()
                    .and_then(|e| extract_call_receiver(unwrap_expr_wrappers(e)));

                // Visit arguments and receiver (for nested reads/calls) before entering callee.
                let mut children = Vec::new();
                node.for_each_child(&mut |child| {
                    children.push(child);
                    false
                });
                for child in children {
                    self.collect_effects_in_ast(
                        child,
                        false,
                        enclosing_fn,
                        call_stack,
                        opts,
                        depth,
                        via_call,
                        chain_evidence,
                        visited,
                        out,
                    );
                }

                let (callees, callback_param_idx, call_gaps) =
                    self.resolve_call_callees(node, call_stack);

                self.record_callback_invocation(
                    node,
                    enclosing_fn,
                    call_stack,
                    &callees,
                    callback_param_idx,
                    call_loc.as_ref(),
                    chain_evidence,
                    out,
                );

                for g in call_gaps {
                    push_unique_gap(&mut out.gaps, g);
                }

                if opts.follow_callbacks || callback_param_idx.is_none() {
                    for callee in callees {
                        let fn_label =
                            callee
                                .fn_node
                                .name()
                                .and_then(node_text)
                                .unwrap_or_else(|| {
                                    node.expression()
                                        .and_then(node_text)
                                        .unwrap_or_else(|| "<callback>".to_string())
                                });
                        let mut next_stack = callee.captured_stack;
                        next_stack.push(CallFrame {
                            call_node: node,
                            callee_fn: Some(callee.fn_node),
                            receiver_expr: recv,
                        });
                        let mut next_ev = chain_evidence.to_vec();
                        next_ev.push(EvidenceItem::simple(
                            "source",
                            format!("Entered call to '{fn_label}'"),
                            call_loc.clone(),
                        ));
                        self.collect_effects_in_fn(
                            callee.fn_node,
                            &next_stack,
                            opts,
                            depth + 1,
                            Some(via_call.unwrap_or(node)),
                            &next_ev,
                            visited,
                            out,
                        );
                    }
                }
                return;
            }
            _ => {}
        }

        let mut children = Vec::new();
        node.for_each_child(&mut |child| {
            children.push(child);
            false
        });
        for child in children {
            self.collect_effects_in_ast(
                child,
                false,
                enclosing_fn,
                call_stack,
                opts,
                depth,
                via_call,
                chain_evidence,
                visited,
                out,
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Constructs normalized RawFieldEffect with canonical receiver declaration and local CFG scope."
    )]
    fn build_raw_field_effect(
        &mut self,
        kind: &'static str,
        property_name: String,
        operator: Option<&'static str>,
        syntax: P<Node>,
        target_syntax: P<Node>,
        receiver_syntax: P<Node>,
        value_syntax: Option<P<Node>>,
        value_text: Option<String>,
        location: SourceLocation,
        via_call: Option<P<Node>>,
        enclosing_fn: P<Node>,
        evidence: Vec<EvidenceItem>,
    ) -> RawFieldEffect {
        let receiver_declaration = resolve_decl_for_expr(self.checker, receiver_syntax);
        let enclosing_block = resolve_enclosing_block(syntax, enclosing_fn);
        let cfg_scope = Some(resolve_cfg_scope(syntax, enclosing_fn));
        RawFieldEffect {
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
            enclosing_fn: Some(enclosing_fn),
            enclosing_block,
            cfg_scope,
            guards: collect_structural_guards(&self.cwd, syntax, enclosing_fn),
            evidence,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Dispatches between indirect helper parameter invocations and direct local callable invocations."
    )]
    fn record_callback_invocation(
        &mut self,
        call_node: P<Node>,
        enclosing_fn: P<Node>,
        call_stack: &[CallFrame],
        callees: &[ResolvedCallable],
        callback_param_idx: Option<usize>,
        call_loc: Option<&SourceLocation>,
        chain_evidence: &[EvidenceItem],
        out: &mut CollectedCallEffects,
    ) {
        let Some(loc) = call_loc else {
            return;
        };

        if let Some(param_idx) = callback_param_idx {
            self.record_indirect_callback_invocation(
                call_node,
                enclosing_fn,
                call_stack,
                callees,
                param_idx,
                loc,
                chain_evidence,
                out,
            );
            return;
        }

        if callees.is_empty() {
            self.record_direct_callback_invocation(
                call_node,
                enclosing_fn,
                loc,
                chain_evidence,
                out,
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Records an indirect callback invocation through a helper parameter with outer call and helper provenance."
    )]
    fn record_indirect_callback_invocation(
        &mut self,
        call_node: P<Node>,
        enclosing_fn: P<Node>,
        call_stack: &[CallFrame],
        callees: &[ResolvedCallable],
        param_idx: usize,
        loc: &SourceLocation,
        chain_evidence: &[EvidenceItem],
        out: &mut CollectedCallEffects,
    ) {
        let Some(frame) = call_stack
            .iter()
            .rfind(|f| f.callee_fn == Some(enclosing_fn))
        else {
            return;
        };
        let Some(&cb_arg) = frame.call_node.arguments().get(param_idx) else {
            return;
        };
        let Some(outer_loc) = node_location_rel(&self.cwd, frame.call_node) else {
            return;
        };

        let fn_name = enclosing_fn
            .name()
            .and_then(node_text)
            .unwrap_or_else(|| "<helper>".to_string());
        let arg_txt = node_text(cb_arg).unwrap_or_else(|| "callback".to_string());
        let helper_decl = resolve_decl_for_fn(self.checker, enclosing_fn);
        let helper_decl_loc = node_location_rel(&self.cwd, helper_decl.0)
            .or_else(|| node_location_rel(&self.cwd, enclosing_fn));
        let target_declaration = resolve_decl_for_expr(self.checker, cb_arg);
        let resolved_targets = callees
            .iter()
            .map(|c| resolve_decl_for_fn(self.checker, c.fn_node))
            .collect();
        let outer_enclosing_fn = resolve_enclosing_function(frame.call_node);
        let enclosing_block =
            outer_enclosing_fn.and_then(|f| resolve_enclosing_block(frame.call_node, f));
        let cfg_scope = outer_enclosing_fn.map(|f| resolve_cfg_scope(frame.call_node, f));

        let mut cb_ev: Vec<EvidenceItem> = chain_evidence
            .iter()
            .filter(|item| {
                item.at.as_ref().is_none_or(|a| {
                    a.file != outer_loc.file || a.start != outer_loc.start || a.end != outer_loc.end
                })
            })
            .cloned()
            .collect();
        cb_ev.push(EvidenceItem::simple(
            "source",
            format!("Passed callback argument '{arg_txt}' to helper '{fn_name}'"),
            Some(outer_loc.clone()),
        ));
        if let Some(h_loc) = helper_decl_loc {
            cb_ev.push(EvidenceItem::simple(
                "source",
                format!("Helper '{fn_name}' accepts callback parameter #{param_idx}"),
                Some(h_loc),
            ));
        }
        cb_ev.push(EvidenceItem::simple(
            "source",
            format!("Helper '{fn_name}' invokes callback parameter #{param_idx} ('{arg_txt}')"),
            Some(loc.clone()),
        ));

        out.callback_invocations.push(RawCallbackInvocation {
            kind: "indirect",
            call_node,
            outer_call: frame.call_node,
            callback_argument: cb_arg,
            parameter_index: Some(param_idx),
            target_declaration,
            helper_declaration: Some(helper_decl),
            resolved_targets,
            enclosing_function: outer_enclosing_fn,
            enclosing_block,
            cfg_scope,
            location: loc.clone(),
            outer_call_location: outer_loc,
            evidence: cb_ev,
        });
    }

    fn record_direct_callback_invocation(
        &mut self,
        call_node: P<Node>,
        enclosing_fn: P<Node>,
        loc: &SourceLocation,
        chain_evidence: &[EvidenceItem],
        out: &mut CollectedCallEffects,
    ) {
        let Some(callee_expr) = call_node.expression() else {
            return;
        };
        let Some((decl, sym_opt)) = resolve_decl_for_expr(self.checker, callee_expr) else {
            return;
        };
        if is_external_declaration(self.program, decl)
            || !matches!(
                decl.kind(),
                Kind::VariableDeclaration
                    | Kind::Parameter
                    | Kind::BindingElement
                    | Kind::PropertyDeclaration
                    | Kind::PropertySignature
            )
        {
            return;
        }

        let arg_txt = node_text(callee_expr).unwrap_or_else(|| "callback".to_string());
        let mut cb_ev = chain_evidence.to_vec();
        cb_ev.push(EvidenceItem::simple(
            "source",
            format!("Direct invocation of callable '{arg_txt}'"),
            Some(loc.clone()),
        ));
        let enclosing_block = resolve_enclosing_block(call_node, enclosing_fn);
        let cfg_scope = Some(resolve_cfg_scope(call_node, enclosing_fn));
        out.callback_invocations.push(RawCallbackInvocation {
            kind: "direct",
            call_node,
            outer_call: call_node,
            callback_argument: callee_expr,
            parameter_index: None,
            target_declaration: Some((decl, sym_opt)),
            helper_declaration: None,
            resolved_targets: Vec::new(),
            enclosing_function: Some(enclosing_fn),
            enclosing_block,
            cfg_scope,
            location: loc.clone(),
            outer_call_location: loc.clone(),
            evidence: cb_ev,
        });
    }
}

type DeclTarget = (P<Node>, Option<P<Symbol>>);

fn resolve_decl_for_expr(checker: &mut CheckerHandle, expr: P<Node>) -> Option<DeclTarget> {
    let unwrapped = unwrap_expr_wrappers(expr);
    if let Some(sym) = canonical_symbol_for_node(checker, unwrapped)
        && let Some(decl) = pick_preferred_impl_decl(sym, None)
    {
        return Some((decl, Some(sym)));
    }
    if is_function_like_declaration(Some(unwrapped)) {
        return Some((unwrapped, unwrapped.symbol()));
    }
    None
}

fn resolve_decl_for_fn(checker: &mut CheckerHandle, fn_node: P<Node>) -> DeclTarget {
    if matches!(
        fn_node.kind(),
        Kind::ArrowFunction | Kind::FunctionExpression
    ) {
        let mut cur = fn_node;
        while let Some(parent) = cur.parent() {
            if matches!(
                parent.kind(),
                Kind::ParenthesizedExpression
                    | Kind::AsExpression
                    | Kind::SatisfiesExpression
                    | Kind::TypeAssertionExpression
                    | Kind::NonNullExpression
            ) {
                cur = parent;
                continue;
            }
            if matches!(
                parent.kind(),
                Kind::VariableDeclaration | Kind::PropertyAssignment | Kind::PropertyDeclaration
            ) && let Some(sym) = canonical_symbol_for_node(checker, parent)
            {
                return (parent, Some(sym));
            }
            break;
        }
    }
    if let Some(sym) = canonical_symbol_for_node(checker, fn_node) {
        let decl = pick_preferred_impl_decl(sym, None).unwrap_or(fn_node);
        return (decl, Some(sym));
    }
    (fn_node, fn_node.symbol())
}

fn resolve_enclosing_block(node: P<Node>, enclosing_fn: P<Node>) -> Option<P<Node>> {
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        if parent.kind() == Kind::Block {
            return Some(parent);
        }
        if parent == enclosing_fn {
            break;
        }
        cur = parent;
    }
    None
}

fn resolve_cfg_scope(node: P<Node>, enclosing_fn: P<Node>) -> P<Node> {
    let mut cur = node;
    let mut highest_clean_block: Option<P<Node>> = None;
    while let Some(parent) = cur.parent() {
        if matches!(
            parent.kind(),
            Kind::ForStatement
                | Kind::ForInStatement
                | Kind::ForOfStatement
                | Kind::WhileStatement
                | Kind::DoStatement
                | Kind::TryStatement
                | Kind::LabeledStatement
        ) {
            return highest_clean_block.unwrap_or(enclosing_fn);
        }
        if parent == enclosing_fn {
            return enclosing_fn;
        }
        if parent.kind() == Kind::Block {
            highest_clean_block = Some(parent);
        }
        cur = parent;
    }
    enclosing_fn
}

#[derive(Clone, Copy)]
struct EffectTraversalOptions {
    follow_callbacks: bool,
    include_reads: bool,
    include_writes: bool,
    max_depth: usize,
}

fn is_direct_call_callee(node: P<Node>) -> bool {
    let mut cur = node;
    while let Some(p) = cur.parent() {
        if matches!(
            p.kind(),
            Kind::ParenthesizedExpression
                | Kind::AsExpression
                | Kind::TypeAssertionExpression
                | Kind::SatisfiesExpression
                | Kind::NonNullExpression
        ) {
            cur = p;
            continue;
        }
        return p.kind() == Kind::CallExpression && p.expression() == Some(cur);
    }
    false
}

struct RawGuardCondition {
    branch_syntax: P<Node>,
    condition: P<Node>,
    condition_text: String,
    direction: &'static str,
    location: SourceLocation,
}

struct RawFieldEffect {
    kind: &'static str,
    property_name: String,
    operator: Option<&'static str>,
    syntax: P<Node>,
    target_syntax: P<Node>,
    receiver_syntax: P<Node>,
    receiver_declaration: Option<DeclTarget>,
    value_syntax: Option<P<Node>>,
    value_text: Option<String>,
    location: SourceLocation,
    via_call: Option<P<Node>>,
    enclosing_fn: Option<P<Node>>,
    enclosing_block: Option<P<Node>>,
    cfg_scope: Option<P<Node>>,
    guards: Vec<RawGuardCondition>,
    evidence: Vec<EvidenceItem>,
}

struct RawCallbackInvocation {
    kind: &'static str,
    call_node: P<Node>,
    outer_call: P<Node>,
    callback_argument: P<Node>,
    parameter_index: Option<usize>,
    target_declaration: Option<DeclTarget>,
    helper_declaration: Option<DeclTarget>,
    resolved_targets: Vec<DeclTarget>,
    enclosing_function: Option<P<Node>>,
    enclosing_block: Option<P<Node>>,
    cfg_scope: Option<P<Node>>,
    location: SourceLocation,
    outer_call_location: SourceLocation,
    evidence: Vec<EvidenceItem>,
}

#[derive(Default)]
struct CollectedCallEffects {
    reads: Vec<RawFieldEffect>,
    writes: Vec<RawFieldEffect>,
    callback_invocations: Vec<RawCallbackInvocation>,
    calls: Vec<P<Node>>,
    gaps: Vec<GapItem>,
}

fn stmt_always_exits(stmt: P<Node>) -> bool {
    match stmt.kind() {
        Kind::ReturnStatement
        | Kind::ThrowStatement
        | Kind::ContinueStatement
        | Kind::BreakStatement => true,
        Kind::Block => stmt.statements().iter().copied().any(stmt_always_exits),
        _ => false,
    }
}

fn collect_structural_guards(
    cwd: &str,
    node: P<Node>,
    enclosing_fn: P<Node>,
) -> Vec<RawGuardCondition> {
    let mut guards = Vec::new();
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        if parent == enclosing_fn {
            break;
        }
        if parent.kind() == Kind::IfStatement {
            let if_stmt = parent.as_if_statement();
            let cond = if_stmt.expression();
            if let Some(cond_loc) = node_location_rel(cwd, cond) {
                let cond_text = node_text(cond).unwrap_or_default();
                if if_stmt.then_statement() == cur {
                    guards.push(RawGuardCondition {
                        branch_syntax: parent,
                        condition: cond,
                        condition_text: cond_text,
                        direction: "true",
                        location: cond_loc,
                    });
                } else if if_stmt.else_statement() == Some(cur) {
                    guards.push(RawGuardCondition {
                        branch_syntax: parent,
                        condition: cond,
                        condition_text: cond_text,
                        direction: "false",
                        location: cond_loc,
                    });
                }
            }
        } else if parent.kind() == Kind::Block {
            let stmts = parent.statements();
            if let Some(pos) = stmts.iter().position(|&s| s == cur) {
                for &prev in stmts[..pos].iter().rev() {
                    if prev.kind() == Kind::IfStatement {
                        let prev_if = prev.as_if_statement();
                        if prev_if.else_statement().is_none()
                            && stmt_always_exits(prev_if.then_statement())
                        {
                            let cond = prev_if.expression();
                            if let Some(cond_loc) = node_location_rel(cwd, cond) {
                                guards.push(RawGuardCondition {
                                    branch_syntax: prev,
                                    condition: cond,
                                    condition_text: node_text(cond).unwrap_or_default(),
                                    direction: "false",
                                    location: cond_loc,
                                });
                            }
                        }
                    }
                }
            }
        }
        cur = parent;
    }
    guards.reverse();
    guards
}

fn alloc_cleanup_site_payload(
    snapshot: &mut SnapshotState,
    project_id: &str,
    action: &RecordedCleanupAction,
) -> CleanupSitePayload {
    let inst_ref = snapshot.alloc_instance(
        project_id,
        InstanceHandleData {
            instance_key: action.origin.instance_key.clone(),
            origin_kind: action.origin.origin_kind,
            allocation_location: action.origin.allocation_location.clone(),
            origin_syntax: action.origin.origin_node,
            model_id: action.origin.model_id.clone(),
            evidence: action.origin.evidence.clone(),
        },
    );
    let syntax_ref = snapshot.alloc_syntax(project_id, action.actual_site_node);
    let via_call = action.via_call.map(|c| snapshot.alloc_call(project_id, c));
    let via_call_location = action
        .via_call
        .and_then(|c| node_location_rel(&snapshot.cwd, c));
    CleanupSitePayload {
        instance: inst_ref,
        instance_key: action.origin.instance_key.clone(),
        allocation_location: action.origin.allocation_location.clone(),
        operation_kind: action.operation_kind.clone(),
        syntax: syntax_ref,
        location: action.location.clone(),
        via_call,
        via_call_location,
        model_id: action.model_id.clone(),
        evidence: action.evidence.clone(),
    }
}

fn alloc_field_effect_payload(
    snapshot: &mut SnapshotState,
    project_id: &str,
    raw: &RawFieldEffect,
) -> FieldEffectPayload {
    let syntax = snapshot.alloc_syntax(project_id, raw.syntax);
    let target_syntax = snapshot.alloc_syntax(project_id, raw.target_syntax);
    let receiver_syntax = snapshot.alloc_syntax(project_id, raw.receiver_syntax);
    let receiver_declaration = raw
        .receiver_declaration
        .map(|(node, sym)| snapshot.alloc_declaration(project_id, node, sym));
    let value_syntax = raw
        .value_syntax
        .map(|v| snapshot.alloc_syntax(project_id, v));
    let via_call = raw.via_call.map(|c| snapshot.alloc_call(project_id, c));
    let enclosing_function = raw
        .enclosing_fn
        .map(|f| snapshot.alloc_syntax(project_id, f));
    let enclosing_block = raw
        .enclosing_block
        .map(|b| snapshot.alloc_syntax(project_id, b));
    let cfg_scope = raw.cfg_scope.map(|s| snapshot.alloc_syntax(project_id, s));
    let guards = raw
        .guards
        .iter()
        .map(|g| GuardConditionPayload {
            branch_syntax: snapshot.alloc_syntax(project_id, g.branch_syntax),
            condition: snapshot.alloc_syntax(project_id, g.condition),
            condition_text: g.condition_text.clone(),
            direction: g.direction,
            location: g.location.clone(),
        })
        .collect();

    FieldEffectPayload {
        kind: raw.kind,
        property_name: raw.property_name.clone(),
        operator: raw.operator,
        syntax,
        target_syntax,
        receiver_syntax,
        receiver_declaration,
        value_syntax,
        value_text: raw.value_text.clone(),
        location: raw.location.clone(),
        via_call,
        enclosing_function,
        enclosing_block,
        cfg_scope,
        guards,
        evidence: raw.evidence.clone(),
    }
}

struct ScopeEvalData {
    scope_handle: String,
    scope_loc: Option<SourceLocation>,
    resolved_fns: Vec<P<Node>>,
    disposals: Vec<RecordedCleanupAction>,
    registrations: Vec<RecordedCleanupAction>,
    detachments: Vec<RecordedCleanupAction>,
    dynamic_gaps: Vec<RecordedDynamicGap>,
    scope_evidence: Vec<EvidenceItem>,
    scope_gaps: Vec<GapItem>,
}

#[expect(
    clippy::too_many_lines,
    reason = "Evaluates cleanup coverage across resolved scope functions and target instances."
)]
#[expect(
    clippy::too_many_arguments,
    reason = "Forwards scope, target, model, and candidate-file inputs to cleanup analysis."
)]
pub fn run_instance_cleanup(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    scopes: &[String],
    targets: &[String],
    models: &[EffectsModelInput],
    lookup_files: Option<&[String]>,
) -> Result<Vec<InstanceCleanupScopeRow>, RpcErr> {
    let mut scope_nodes = Vec::with_capacity(scopes.len());
    for handle_id in scopes {
        scope_nodes.push((
            handle_id.clone(),
            resolve_target_node(snapshot, project_id, handle_id)?,
        ));
    }

    let mut target_instances = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let data = snapshot.get_instance(project_id, handle_id)?.clone();
        target_instances.push((handle_id.clone(), data));
    }

    let mut scope_results = Vec::with_capacity(scope_nodes.len());
    let mut eval_items = Vec::with_capacity(scope_nodes.len());
    {
        let mut engine = EffectsEngine::new(
            snapshot,
            project_id,
            lookup_files,
            program,
            checker,
            models,
            true,
        );
        for (scope_handle, scope_node) in scope_nodes {
            let scope_loc = node_location_rel(&engine.cwd, scope_node);
            let (callables, scope_evidence, scope_gaps) =
                engine.resolve_scope_functions(scope_node);
            let mut disposals = Vec::new();
            let mut registrations = Vec::new();
            let mut detachments = Vec::new();
            let mut detached_pairs = BTreeSet::new();
            let mut dynamic_gaps = Vec::new();
            let mut visited = FxHashSet::default();
            let mut resolved_fns = Vec::new();

            for callable in callables {
                resolved_fns.push(callable.fn_node);
                engine.collect_cleanup_in_fn(
                    callable.fn_node,
                    &callable.captured_stack,
                    None,
                    None,
                    &scope_evidence,
                    0,
                    &mut visited,
                    &mut disposals,
                    &mut registrations,
                    &mut detachments,
                    &mut detached_pairs,
                    &mut dynamic_gaps,
                );
            }

            eval_items.push(ScopeEvalData {
                scope_handle,
                scope_loc,
                resolved_fns,
                disposals,
                registrations,
                detachments,
                dynamic_gaps,
                scope_evidence,
                scope_gaps,
            });
        }
        drop(engine);
    }

    for item in eval_items {
        let mut cfgs = Vec::new();
        let mut cfg_build_gaps = Vec::new();
        for &fn_node in &item.resolved_fns {
            match snapshot.get_or_build_cfg(project_id, fn_node) {
                Ok(cfg) => cfgs.push((fn_node, cfg)),
                Err(gap) => push_unique_gap(&mut cfg_build_gaps, gap),
            }
        }

        let mut combined_gaps = item.scope_gaps.clone();
        for dg in &item.dynamic_gaps {
            push_unique_gap(&mut combined_gaps, dg.gap.clone());
        }
        for g in &cfg_build_gaps {
            push_unique_gap(&mut combined_gaps, g.clone());
        }

        let mut combined_evidence = item.scope_evidence.clone();
        for d in &item.disposals {
            for ev in &d.evidence {
                push_unique_evidence(&mut combined_evidence, ev.clone());
            }
        }

        let mut target_rows = Vec::with_capacity(target_instances.len());
        let fallback_loc = item.scope_loc.clone().unwrap_or(SourceLocation {
            file: String::new(),
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
            start: 0,
            end: 0,
        });

        for (target_handle, target_data) in &target_instances {
            let matching_disposals: Vec<&RecordedCleanupAction> = item
                .disposals
                .iter()
                .filter(|d| d.origin.instance_key == target_data.instance_key)
                .collect();

            let mut target_gaps = combined_gaps.clone();
            let mut target_evidence = Vec::new();
            let mut cleanup_sites = Vec::new();
            for d in &matching_disposals {
                cleanup_sites.push(alloc_cleanup_site_payload(snapshot, project_id, d));
                for ev in &d.evidence {
                    push_unique_evidence(&mut target_evidence, ev.clone());
                }
            }

            if cfgs.is_empty() {
                target_rows.push(TargetInstanceCleanupRow {
                    instance: target_handle.clone(),
                    instance_key: target_data.instance_key.clone(),
                    allocation_location: target_data.allocation_location.clone(),
                    status: if target_gaps.is_empty() {
                        "not-cleaned"
                    } else {
                        "unknown"
                    },
                    cleaned_on_all_paths: false,
                    cleaned_on_any_path: false,
                    cleanup_sites,
                    evidence: target_evidence,
                    gaps: target_gaps,
                });
                continue;
            }

            let mut all_covered = true;
            let mut any_cleaned = false;
            let mut any_unknown = false;

            for (fn_node, cfg) in &cfgs {
                let disposal_nodes: Vec<P<Node>> = matching_disposals
                    .iter()
                    .filter(|d| {
                        resolve_enclosing_function(d.top_site_node) == Some(*fn_node)
                            || d.top_site_node == *fn_node
                    })
                    .map(|d| d.top_site_node)
                    .collect();
                let dynamic_nodes: Vec<P<Node>> = item
                    .dynamic_gaps
                    .iter()
                    .filter(|dg| resolve_enclosing_function(dg.top_site_node) == Some(*fn_node))
                    .map(|dg| dg.top_site_node)
                    .collect();

                let outcome =
                    cfg.evaluate_cleanup_coverage(&disposal_nodes, &dynamic_nodes, &fallback_loc);
                for g in &outcome.gaps {
                    push_unique_gap(&mut target_gaps, g.clone());
                }
                for ev in &outcome.evidence {
                    push_unique_evidence(&mut target_evidence, ev.clone());
                }
                any_cleaned |= outcome.cleaned_on_any_path;
                all_covered &= outcome.cleaned_on_all_paths;
                if outcome.status == CleanupCfgStatus::Unknown {
                    any_unknown = true;
                }
            }

            let status = if all_covered {
                "covered"
            } else if any_unknown {
                "unknown"
            } else if any_cleaned {
                "partial"
            } else {
                "not-cleaned"
            };

            // When not cleaned via CFG, discard unreachable cleanup sites from reported sites if none were reachable.
            if !any_cleaned {
                cleanup_sites.clear();
            }

            target_rows.push(TargetInstanceCleanupRow {
                instance: target_handle.clone(),
                instance_key: target_data.instance_key.clone(),
                allocation_location: target_data.allocation_location.clone(),
                status,
                cleaned_on_all_paths: all_covered,
                cleaned_on_any_path: any_cleaned,
                cleanup_sites,
                evidence: target_evidence,
                gaps: target_gaps,
            });
        }

        // Deduplicate disposed/registered/detached instances by (instance_key, location).
        let mut disposed_payloads = Vec::new();
        let mut seen_disposed = BTreeSet::new();
        for d in &item.disposals {
            let key = (
                d.origin.instance_key.clone(),
                d.location.file.clone(),
                d.location.start,
                d.location.end,
            );
            if seen_disposed.insert(key) {
                disposed_payloads.push(alloc_cleanup_site_payload(snapshot, project_id, d));
            }
        }

        let mut registered_payloads = Vec::new();
        let mut seen_reg = BTreeSet::new();
        for r in &item.registrations {
            let key = (
                r.origin.instance_key.clone(),
                r.location.file.clone(),
                r.location.start,
                r.location.end,
            );
            if seen_reg.insert(key) {
                registered_payloads.push(alloc_cleanup_site_payload(snapshot, project_id, r));
            }
        }

        let mut detached_payloads = Vec::new();
        let mut seen_det = BTreeSet::new();
        for det in &item.detachments {
            let key = (
                det.origin.instance_key.clone(),
                det.location.file.clone(),
                det.location.start,
                det.location.end,
            );
            if seen_det.insert(key) {
                detached_payloads.push(alloc_cleanup_site_payload(snapshot, project_id, det));
            }
        }

        let resolved_fn_refs: Vec<String> = item
            .resolved_fns
            .iter()
            .map(|&f| snapshot.alloc_syntax(project_id, f))
            .collect();

        scope_results.push(InstanceCleanupScopeRow {
            scope: item.scope_handle,
            resolved_functions: resolved_fn_refs,
            target_results: target_rows,
            disposed_instances: disposed_payloads,
            registered_instances: registered_payloads,
            detached_instances: detached_payloads,
            evidence: combined_evidence,
            gaps: combined_gaps,
        });
    }

    Ok(scope_results)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Executes callEffects step for target functions or call sites with traversal options."
)]
pub fn run_call_effects(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
    follow_callbacks: bool,
    include_reads: bool,
    include_writes: bool,
    max_depth: Option<usize>,
    models: &[EffectsModelInput],
) -> Result<Vec<CallEffectsRow>, RpcErr> {
    let mut target_nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        target_nodes.push((
            handle_id.clone(),
            resolve_target_node(snapshot, project_id, handle_id)?,
        ));
    }

    let opts = EffectTraversalOptions {
        follow_callbacks,
        include_reads,
        include_writes,
        max_depth: max_depth.unwrap_or(MAX_EFFECT_CALL_DEPTH),
    };
    let mut collected_items = Vec::with_capacity(target_nodes.len());
    {
        let mut engine =
            EffectsEngine::new(snapshot, project_id, None, program, checker, models, false);

        for (target_handle, target_node) in target_nodes {
            let mut collected = CollectedCallEffects::default();
            let mut visited = FxHashSet::default();

            if let Some(fn_node) = normalize_function_node(target_node) {
                engine.collect_effects_in_fn(
                    fn_node,
                    &[],
                    opts,
                    0,
                    None,
                    &[],
                    &mut visited,
                    &mut collected,
                );
            } else if target_node.kind() == Kind::CallExpression {
                let enclosing = resolve_enclosing_function(target_node).unwrap_or(target_node);
                engine.collect_effects_in_ast(
                    target_node,
                    false,
                    enclosing,
                    &[],
                    opts,
                    0,
                    None,
                    &[],
                    &mut visited,
                    &mut collected,
                );
            } else {
                let (callables, _, gaps) = engine.resolve_call_callees(target_node, &[]);
                for g in gaps {
                    push_unique_gap(&mut collected.gaps, g);
                }
                let resolved = if callables.is_empty() {
                    let (expr_callables, expr_gaps) =
                        engine.resolve_callable_expr(target_node, &[]);
                    for g in expr_gaps {
                        push_unique_gap(&mut collected.gaps, g);
                    }
                    expr_callables
                } else {
                    callables
                };
                for c in resolved {
                    engine.collect_effects_in_fn(
                        c.fn_node,
                        &c.captured_stack,
                        opts,
                        0,
                        None,
                        &[],
                        &mut visited,
                        &mut collected,
                    );
                }
            }

            collected_items.push((target_handle, collected));
        }
        drop(engine);
    }

    Ok(build_call_effects_rows(
        snapshot,
        project_id,
        collected_items,
    ))
}

fn build_call_effects_rows(
    snapshot: &mut SnapshotState,
    project_id: &str,
    collected_items: Vec<(String, CollectedCallEffects)>,
) -> Vec<CallEffectsRow> {
    let mut rows = Vec::with_capacity(collected_items.len());
    for (target_handle, collected) in collected_items {
        let mut field_reads = Vec::with_capacity(collected.reads.len());
        let mut field_writes = Vec::with_capacity(collected.writes.len());
        let mut all_effects = Vec::with_capacity(collected.reads.len() + collected.writes.len());
        let mut combined_evidence = Vec::new();

        for r in &collected.reads {
            let payload = alloc_field_effect_payload(snapshot, project_id, r);
            for ev in &payload.evidence {
                push_unique_evidence(&mut combined_evidence, ev.clone());
            }
            all_effects.push(payload.clone());
            field_reads.push(payload);
        }
        for w in &collected.writes {
            let payload = alloc_field_effect_payload(snapshot, project_id, w);
            for ev in &payload.evidence {
                push_unique_evidence(&mut combined_evidence, ev.clone());
            }
            all_effects.push(payload.clone());
            field_writes.push(payload);
        }

        let callback_invocations = collected
            .callback_invocations
            .into_iter()
            .map(|cb| {
                for ev in &cb.evidence {
                    push_unique_evidence(&mut combined_evidence, ev.clone());
                }
                let call = snapshot.alloc_call(project_id, cb.call_node);
                let outer_call = snapshot.alloc_call(project_id, cb.outer_call);
                let syntax = snapshot.alloc_syntax(project_id, cb.call_node);
                let callback_argument = snapshot.alloc_syntax(project_id, cb.callback_argument);
                let target_declaration = cb
                    .target_declaration
                    .map(|(node, sym)| snapshot.alloc_declaration(project_id, node, sym));
                let helper_declaration = cb
                    .helper_declaration
                    .map(|(node, sym)| snapshot.alloc_declaration(project_id, node, sym));
                let resolved_targets = cb
                    .resolved_targets
                    .into_iter()
                    .map(|(t, sym)| snapshot.alloc_declaration(project_id, t, sym))
                    .collect();
                let enclosing_function = cb
                    .enclosing_function
                    .map(|f| snapshot.alloc_syntax(project_id, f));
                let enclosing_block = cb
                    .enclosing_block
                    .map(|b| snapshot.alloc_syntax(project_id, b));
                let cfg_scope = cb.cfg_scope.map(|s| snapshot.alloc_syntax(project_id, s));
                CallbackInvocationPayload {
                    kind: cb.kind,
                    call,
                    outer_call,
                    syntax,
                    callback_argument,
                    parameter_index: cb.parameter_index,
                    target_declaration,
                    helper_declaration,
                    resolved_targets,
                    enclosing_function,
                    enclosing_block,
                    cfg_scope,
                    location: cb.location,
                    outer_call_location: cb.outer_call_location,
                    evidence: cb.evidence,
                }
            })
            .collect();

        let calls = collected
            .calls
            .into_iter()
            .map(|c| snapshot.alloc_call(project_id, c))
            .collect();

        rows.push(CallEffectsRow {
            target: target_handle,
            field_reads,
            field_writes,
            effects: all_effects,
            callback_invocations,
            calls,
            evidence: combined_evidence,
            gaps: collected.gaps,
        });
    }

    rows
}
