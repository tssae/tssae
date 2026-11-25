use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicUsize, Ordering},
};

use serde_json::{Value, json};
use tssae_adapter_api::{
    ADAPTER_ABI_VERSION, Adapter, AdapterError, AdapterManifest, Context, Evidence,
    ExplainGapResolution, ExplainGapResolutionStatus, ExplainLocalGap, FactContribution,
    FactExplainTrace, FactQuery, FactRequest, Gap, NodeKind, NodeRef,
};

// Acceptance checks use this state to verify that snapshot updates reuse the loaded library.
static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct ComplementFixtureAdapter;

impl Adapter for ComplementFixtureAdapter {
    fn manifest(&self) -> AdapterManifest {
        AdapterManifest {
            abi_version: ADAPTER_ABI_VERSION,
            id: "third-party-fixture-complement".to_string(),
            version: "1.0.0".to_string(),
            fact_type_ids: vec![
                "fixture.call-source".to_string(),
                "fixture.derived-consumer".to_string(),
            ],
        }
    }

    fn extract<'a>(
        &self,
        context: &'a Context<'_>,
        request: &FactRequest<'a>,
    ) -> Result<Vec<FactContribution<'a>>, AdapterError> {
        match request.fact_type_id {
            "fixture.call-source" => extract_call_source(context, request),
            "fixture.derived-consumer" => extract_derived_consumer(context, request),
            other => Err(AdapterError::UnsupportedFactType {
                adapter_id: "third-party-fixture-complement".to_string(),
                fact_type_id: other.to_string(),
            }),
        }
    }
}

fn extract_call_source<'a>(
    context: &'a Context<'_>,
    request: &FactRequest<'a>,
) -> Result<Vec<FactContribution<'a>>, AdapterError> {
    let invocation = INVOCATIONS.fetch_add(1, Ordering::Relaxed) + 1;
    let supplemental_input = context.read_additional_input("fixture.inputs.json")?;
    let annotation_input = serde_json::from_str::<String>(&supplemental_input)
        .unwrap_or_else(|_| supplemental_input.clone());
    let mode = request
        .condition
        .as_ref()
        .and_then(|condition| condition.get("mode"))
        .and_then(Value::as_str);
    let conflict = mode == Some("conflict");
    let targeted_conflict = mode == Some("targeted-conflict")
        && request.targets.is_some()
        && annotation_input == "host-installed-input";
    let input = if conflict || targeted_conflict {
        "complement-conflict-input"
    } else {
        supplemental_input.as_str()
    };
    let mut contributions = Vec::new();

    for call in &request.calls {
        if call.kind()? != NodeKind::CallExpression {
            continue;
        }
        if let Some(targets) = &request.targets
            && !targets.contains(call)
        {
            continue;
        }

        let text = call.text()?.unwrap_or_default();
        if !text.starts_with("getProduct(") {
            continue;
        }
        let location = call.location()?;
        let mut contribution = FactContribution::new(
            *call,
            json!({
                "text": text,
                "input": input,
                "annotation": format!("complement:{annotation_input}"),
                "secondaryInvocation": invocation,
            }),
        );
        if let Some(at) = location {
            contribution.evidence.push(Evidence {
                kind: "source".to_string(),
                message: "Complement fixture adapter read this call from the active snapshot."
                    .to_string(),
                at: Some(at),
                model_id: None,
                declaration: None,
            });
        }
        contributions.push(contribution);
    }

    Ok(contributions)
}

fn extract_derived_consumer<'a>(
    context: &'a Context<'_>,
    request: &FactRequest<'a>,
) -> Result<Vec<FactContribution<'a>>, AdapterError> {
    let mut contributions = Vec::new();
    if let Some(targets) = &request.targets {
        for &target in targets {
            if let Some(call) = resolve_consumer_call(target)?
                && let Some(contribution) =
                    build_consumer_contribution(context, target, call, request.explain)?
            {
                contributions.push(contribution);
            }
        }
        return Ok(contributions);
    }

    let mut covered_calls = Vec::new();
    for &variable in &request.variables {
        if let Some(call) = resolve_consumer_call(variable)?
            && let Some(contribution) =
                build_consumer_contribution(context, variable, call, request.explain)?
        {
            covered_calls.push(call);
            contributions.push(contribution);
        }
    }
    for &call in &request.calls {
        if covered_calls.contains(&call) {
            continue;
        }
        if is_consume_fixture_source_call(call)?
            && let Some(contribution) =
                build_consumer_contribution(context, call, call, request.explain)?
        {
            contributions.push(contribution);
        }
    }
    Ok(contributions)
}

fn resolve_consumer_call(node: NodeRef<'_>) -> Result<Option<NodeRef<'_>>, AdapterError> {
    let current = node.unwrap_expression()?.unwrap_or(node);
    match current.kind()? {
        NodeKind::CallExpression => Ok(is_consume_fixture_source_call(current)?.then_some(current)),
        NodeKind::VariableDeclaration => {
            let Some(init) = current.safe_initializer()? else {
                return Ok(None);
            };
            let init = init.unwrap_expression()?.unwrap_or(init);
            if init.kind()? == NodeKind::CallExpression && is_consume_fixture_source_call(init)? {
                Ok(Some(init))
            } else {
                Ok(None)
            }
        }
        NodeKind::Identifier => {
            let Some(decl) = current.declaration_node()? else {
                return Ok(None);
            };
            if decl == current {
                return Ok(None);
            }
            resolve_consumer_call(decl)
        }
        _ => Ok(None),
    }
}

fn is_consume_fixture_source_call(call: NodeRef<'_>) -> Result<bool, AdapterError> {
    if call.kind()? != NodeKind::CallExpression {
        return Ok(false);
    }
    let Some(callee) = call.expression()? else {
        return Ok(false);
    };
    let callee = callee.unwrap_expression()?.unwrap_or(callee);
    let text = callee.text()?.unwrap_or_default();
    Ok(text == "consumeFixtureSource" || text.ends_with(".consumeFixtureSource"))
}

struct DerivedConsumerState {
    source_name: String,
    fields: BTreeMap<String, String>,
    unknown_fields: BTreeSet<String>,
    field_gaps: BTreeMap<String, Vec<Gap>>,
    shape_gaps: Vec<Gap>,
    has_dynamic_fields: bool,
    evidence: Vec<Evidence>,
    source_providers: BTreeSet<String>,
    field_gap_origins: BTreeMap<String, ExplainLocalGap>,
    shape_local_gaps: Vec<ExplainLocalGap>,
    resolved_local_gaps: Vec<ExplainLocalGap>,
}

fn build_consumer_contribution<'a>(
    context: &'a Context<'_>,
    subject: NodeRef<'a>,
    call: NodeRef<'a>,
    explain: bool,
) -> Result<Option<FactContribution<'a>>, AdapterError> {
    let args = call.arguments()?;
    let Some(&raw_source_arg) = args.first() else {
        return Ok(None);
    };
    let source_arg = raw_source_arg
        .unwrap_expression()?
        .unwrap_or(raw_source_arg);
    let target_a = if source_arg.kind()? == NodeKind::Identifier {
        source_arg.declaration_node()?.unwrap_or(source_arg)
    } else {
        source_arg
    };

    let queried = context.query_facts(&FactQuery {
        fact_type_id: "fixture.source-spec",
        provider_id: None,
        condition: None,
        targets: Some(&[target_a]),
    })?;

    let mut state = init_state_from_source_facts(&queried, source_arg, explain)?;
    if let Some(&override_arg) = args.get(1) {
        apply_consumer_overrides(override_arg, explain, &mut state)?;
    }

    let consumer_name = match subject.kind()? {
        NodeKind::VariableDeclaration => subject
            .name()?
            .and_then(|name| name.text().transpose())
            .transpose()?
            .unwrap_or_else(|| "consumer".to_string()),
        _ => subject.text()?.unwrap_or_else(|| "consumer".to_string()),
    };

    let subject_location = subject.location()?;
    if let Some(at) = subject_location.clone() {
        state.evidence.push(Evidence {
            kind: "source".to_string(),
            message: format!(
                "Fixture consumer \"{consumer_name}\" derived from source \"{}\".",
                state.source_name
            ),
            at: Some(at),
            model_id: None,
            declaration: None,
        });
    }

    let mut all_gaps = state.shape_gaps;
    for gaps in state.field_gaps.into_values() {
        all_gaps.extend(gaps);
    }

    let required_fields = collect_fields_by_mode(&state.fields, "required");
    let optional_fields = collect_fields_by_mode(&state.fields, "optional");
    let defaulted_fields = collect_fields_by_mode(&state.fields, "defaulted");
    let unknown_fields: Vec<String> = state.unknown_fields.into_iter().collect();
    let has_dynamic_fields =
        state.has_dynamic_fields || !unknown_fields.is_empty() || !all_gaps.is_empty();
    let source_providers: Vec<String> = state.source_providers.into_iter().collect();

    let mut local_gaps = Vec::new();
    if explain {
        local_gaps.extend(state.resolved_local_gaps);
        for (field_name, mut unresolved) in state.field_gap_origins {
            unresolved.status = ExplainGapResolutionStatus::Unresolved;
            if unresolved.impact.is_none() {
                unresolved.impact = Some(format!(
                    "Field \"{field_name}\" remained unknown and affected derived consumer \"{consumer_name}\"."
                ));
            }
            local_gaps.push(unresolved);
        }
        for mut shape_gap in state.shape_local_gaps {
            if shape_gap.subject_location.is_none() {
                shape_gap.subject_location.clone_from(&subject_location);
            }
            local_gaps.push(shape_gap);
        }
    }

    let mut contribution = FactContribution::new(
        subject,
        json!({
            "consumerName": consumer_name,
            "sourceName": state.source_name,
            "fields": state.fields,
            "requiredFields": required_fields,
            "optionalFields": optional_fields,
            "defaultedFields": defaulted_fields,
            "unknownFields": unknown_fields,
            "hasDynamicFields": has_dynamic_fields,
            "sourceProviders": source_providers,
        }),
    );
    contribution.evidence = state.evidence;
    contribution.gaps = all_gaps;
    if explain && !local_gaps.is_empty() {
        contribution.explain_trace = Some(FactExplainTrace { local_gaps });
    }
    Ok(Some(contribution))
}

fn push_consumer_shape_gap(
    state: &mut DerivedConsumerState,
    explain: bool,
    kind: &str,
    message: String,
    at: Option<tssae_adapter_api::SourceLocation>,
    impact: &str,
) {
    state.has_dynamic_fields = true;
    let gap = Gap {
        kind: kind.to_string(),
        message,
        at: at.clone(),
    };
    if explain {
        state.shape_local_gaps.push(ExplainLocalGap {
            status: ExplainGapResolutionStatus::Unresolved,
            fact_type_id: "fixture.derived-consumer".to_string(),
            provider_id: "third-party-fixture-complement".to_string(),
            field: None,
            gap: gap.clone(),
            subject_location: at,
            resolved_by: None,
            impact: Some(impact.to_string()),
        });
    }
    state.shape_gaps.push(gap);
}

fn init_state_from_source_facts(
    queried: &[tssae_adapter_api::QueriedFact<'_>],
    source_arg: NodeRef<'_>,
    explain: bool,
) -> Result<DerivedConsumerState, AdapterError> {
    let mut state = DerivedConsumerState {
        source_name: "unknown".to_string(),
        fields: BTreeMap::new(),
        unknown_fields: BTreeSet::new(),
        field_gaps: BTreeMap::new(),
        shape_gaps: Vec::new(),
        has_dynamic_fields: false,
        evidence: Vec::new(),
        source_providers: BTreeSet::new(),
        field_gap_origins: BTreeMap::new(),
        shape_local_gaps: Vec::new(),
        resolved_local_gaps: Vec::new(),
    };

    let Some(fact) = queried.first() else {
        push_consumer_shape_gap(
            &mut state,
            explain,
            "unresolved-fixture-source",
            "Could not resolve fixture source specification for consumer.".to_string(),
            source_arg.location()?,
            "Upstream fixture source specification could not be resolved.",
        );
        return Ok(state);
    };

    if let Some(name) = fact.value.get("sourceName").and_then(Value::as_str) {
        state.source_name = name.to_string();
    }
    if let Some(fields_obj) = fact.value.get("fields").and_then(Value::as_object) {
        for (key, val) in fields_obj {
            if let Some(mode) = val.as_str() {
                state.fields.insert(key.clone(), mode.to_string());
            }
        }
    }
    if let Some(unknowns) = fact.value.get("unknownFields").and_then(Value::as_array) {
        for item in unknowns {
            if let Some(field) = item.as_str() {
                state.unknown_fields.insert(field.to_string());
            }
        }
    }
    for gap in &fact.gaps {
        let matched_field = state
            .unknown_fields
            .iter()
            .find(|field| gap.message.contains(&format!("\"{field}\"")))
            .cloned();
        if let Some(field) = matched_field {
            state.field_gaps.entry(field).or_default().push(gap.clone());
        } else {
            state.shape_gaps.push(gap.clone());
        }
    }
    // Preserve shape-level dynamic status even if all field-level unknowns are later overridden.
    let source_has_dynamic = fact
        .value
        .get("hasDynamicFields")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !state.shape_gaps.is_empty()
        || (source_has_dynamic && state.unknown_fields.is_empty() && state.field_gaps.is_empty())
    {
        state.has_dynamic_fields = true;
    }

    if explain {
        record_source_explain_origins(fact, &mut state)?;
    }

    state.evidence.extend(fact.evidence.clone());
    state.source_providers.extend(fact.providers.clone());
    Ok(state)
}

fn record_source_explain_origins(
    fact: &tssae_adapter_api::QueriedFact<'_>,
    state: &mut DerivedConsumerState,
) -> Result<(), AdapterError> {
    let origin_location = fact.subject.location()?;
    let origin_provider = if fact.providers.is_empty() {
        "third-party-fixture".to_string()
    } else {
        fact.providers.join(",")
    };
    if let Some(trace) = &fact.explain_trace {
        for local in &trace.local_gaps {
            if let Some(field) = &local.field {
                state.field_gap_origins.insert(field.clone(), local.clone());
            } else {
                state.shape_local_gaps.push(local.clone());
            }
        }
    }
    for (field, gaps) in &state.field_gaps {
        if !state.field_gap_origins.contains_key(field)
            && let Some(gap) = gaps.first()
        {
            state.field_gap_origins.insert(
                field.clone(),
                ExplainLocalGap {
                    status: ExplainGapResolutionStatus::Unresolved,
                    fact_type_id: "fixture.source-spec".to_string(),
                    provider_id: origin_provider.clone(),
                    field: Some(field.clone()),
                    gap: gap.clone(),
                    subject_location: origin_location.clone(),
                    resolved_by: None,
                    impact: None,
                },
            );
        }
    }
    Ok(())
}

fn apply_consumer_overrides(
    override_arg: NodeRef<'_>,
    explain: bool,
    state: &mut DerivedConsumerState,
) -> Result<(), AdapterError> {
    let unwrapped = override_arg.unwrap_expression()?.unwrap_or(override_arg);
    if unwrapped.kind()? == NodeKind::UndefinedKeyword {
        return Ok(());
    }
    let Some(obj) = resolve_object_literal(unwrapped)? else {
        push_consumer_shape_gap(
            state,
            explain,
            "dynamic-fixture-fields",
            "Fixture consumer overrides could not be statically resolved.".to_string(),
            unwrapped.location()?,
            "Consumer overrides object could not be statically resolved.",
        );
        return Ok(());
    };
    for prop in obj.properties()? {
        apply_override_property(prop, explain, state)?;
    }
    Ok(())
}

fn apply_override_property(
    prop: NodeRef<'_>,
    explain: bool,
    state: &mut DerivedConsumerState,
) -> Result<(), AdapterError> {
    if prop.kind()? != NodeKind::PropertyAssignment {
        push_consumer_shape_gap(
            state,
            explain,
            "dynamic-fixture-fields",
            "Fixture consumer overrides contain dynamic members.".to_string(),
            prop.location()?,
            "Dynamic member in consumer overrides left shape incomplete.",
        );
        return Ok(());
    }
    let Some(key) = read_property_name(prop)? else {
        push_consumer_shape_gap(
            state,
            explain,
            "unresolved-fixture-field",
            "Fixture consumer override has a dynamic property key.".to_string(),
            prop.location()?,
            "Dynamic property key in consumer overrides could not be resolved.",
        );
        return Ok(());
    };
    let init = match prop.safe_initializer()? {
        Some(node) => Some(node),
        None => prop.initializer()?,
    };
    if let Some(init_node) = init
        && let Some(mode) = read_string_literal(init_node)?
        && matches!(mode.as_str(), "required" | "optional" | "defaulted")
    {
        state.fields.insert(key.clone(), mode.clone());
        state.unknown_fields.remove(&key);
        state.field_gaps.remove(&key);
        let prop_loc = prop.location()?;
        if explain && let Some(mut origin) = state.field_gap_origins.remove(&key) {
            origin.status = ExplainGapResolutionStatus::ResolvedByOverride;
            origin.resolved_by = Some(ExplainGapResolution {
                provider_id: "third-party-fixture-complement".to_string(),
                operation: "override".to_string(),
                conclusion: format!("{key}={mode}"),
                at: prop_loc.clone(),
            });
            origin.impact = Some(format!(
                "Initial unknown on field \"{key}\" was resolved to \"{mode}\" by consumer override."
            ));
            state.resolved_local_gaps.push(origin);
        }
        if let Some(at) = prop_loc {
            state.evidence.push(Evidence {
                kind: "source".to_string(),
                message: format!("Fixture consumer overrode field \"{key}\" to \"{mode}\"."),
                at: Some(at),
                model_id: None,
                declaration: None,
            });
        }
    } else {
        state.fields.remove(&key);
        state.unknown_fields.insert(key.clone());
        state.has_dynamic_fields = true;
        let gap = Gap {
            kind: "unresolved-fixture-field".to_string(),
            message: format!(
                "Fixture consumer override for field \"{key}\" could not be statically resolved."
            ),
            at: init.map_or_else(|| prop.location(), NodeRef::location)?,
        };
        if explain {
            state.field_gap_origins.insert(
                key.clone(),
                ExplainLocalGap {
                    status: ExplainGapResolutionStatus::Unresolved,
                    fact_type_id: "fixture.derived-consumer".to_string(),
                    provider_id: "third-party-fixture-complement".to_string(),
                    field: Some(key.clone()),
                    gap: gap.clone(),
                    subject_location: prop.location()?,
                    resolved_by: None,
                    impact: Some(format!(
                        "Consumer override for field \"{key}\" remained unresolved."
                    )),
                },
            );
        }
        state.field_gaps.insert(key, vec![gap]);
    }
    Ok(())
}

fn collect_fields_by_mode(fields: &BTreeMap<String, String>, mode: &str) -> Vec<String> {
    fields
        .iter()
        .filter(|(_, value)| value.as_str() == mode)
        .map(|(key, _)| key.clone())
        .collect()
}

fn resolve_object_literal(node: NodeRef<'_>) -> Result<Option<NodeRef<'_>>, AdapterError> {
    let current = node.unwrap_expression()?.unwrap_or(node);
    match current.kind()? {
        NodeKind::ObjectLiteralExpression => Ok(Some(current)),
        NodeKind::Identifier => {
            let Some(decl) = current.declaration_node()? else {
                return Ok(None);
            };
            let Some(init) = decl.safe_initializer()? else {
                return Ok(None);
            };
            let init = init.unwrap_expression()?.unwrap_or(init);
            Ok((init.kind()? == NodeKind::ObjectLiteralExpression).then_some(init))
        }
        _ => Ok(None),
    }
}

fn read_property_name(prop: NodeRef<'_>) -> Result<Option<String>, AdapterError> {
    let Some(name_node) = prop.name()? else {
        return Ok(None);
    };
    match name_node.kind()? {
        NodeKind::Identifier => Ok(name_node.text()?),
        NodeKind::StringLiteral | NodeKind::NoSubstitutionTemplateLiteral => {
            read_string_literal(name_node)
        }
        _ => Ok(None),
    }
}

fn read_string_literal(node: NodeRef<'_>) -> Result<Option<String>, AdapterError> {
    let node = node.unwrap_expression()?.unwrap_or(node);
    if !matches!(
        node.kind()?,
        NodeKind::StringLiteral | NodeKind::NoSubstitutionTemplateLiteral
    ) {
        return Ok(None);
    }
    let Some(raw) = node.text()? else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
            || (trimmed.starts_with('`') && trimmed.ends_with('`')))
    {
        Ok(Some(trimmed[1..trimmed.len() - 1].to_string()))
    } else {
        Ok(None)
    }
}

tssae_adapter_api::export_adapter!(ComplementFixtureAdapter);
