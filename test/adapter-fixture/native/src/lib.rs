use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicUsize, Ordering},
};

use serde_json::json;
use verule_adapter_api::{
    ADAPTER_ABI_VERSION, Adapter, AdapterError, AdapterManifest, Context, Evidence,
    ExplainGapResolutionStatus, ExplainLocalGap, FactContribution, FactExplainTrace, FactRequest,
    Gap, NodeKind, NodeRef, SourceLocation,
};

static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);

const FIXTURE_ABI_VERSION: u32 = if cfg!(feature = "wrong-abi") {
    ADAPTER_ABI_VERSION + 1
} else {
    ADAPTER_ABI_VERSION
};

#[derive(Default)]
struct ThirdPartyFixtureAdapter;

impl Adapter for ThirdPartyFixtureAdapter {
    fn manifest(&self) -> AdapterManifest {
        AdapterManifest {
            abi_version: FIXTURE_ABI_VERSION,
            id: "third-party-fixture".to_string(),
            version: "1.0.0".to_string(),
            fact_type_ids: vec![
                "fixture.call-source".to_string(),
                "fixture.source-spec".to_string(),
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
            "fixture.source-spec" => extract_source_spec(request),
            other => Err(AdapterError::UnsupportedFactType {
                adapter_id: "third-party-fixture".to_string(),
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
        let location = call.location()?;
        let mut contribution = FactContribution::new(
            *call,
            json!({
                "text": text,
                "input": supplemental_input.clone(),
                "invocation": invocation,
            }),
        );
        if let Some(at) = location {
            contribution.evidence.push(Evidence {
                kind: "source".to_string(),
                message: "Fixture adapter read this call from the active snapshot.".to_string(),
                at: Some(at),
                model_id: None,
                declaration: None,
            });
        }
        contributions.push(contribution);
    }
    Ok(contributions)
}

fn extract_source_spec<'a>(
    request: &FactRequest<'a>,
) -> Result<Vec<FactContribution<'a>>, AdapterError> {
    let mut contributions = Vec::new();
    if let Some(targets) = &request.targets {
        for &target in targets {
            if let Some(call) = resolve_source_spec_call(target)?
                && let Some(contribution) =
                    build_source_spec_contribution(target, call, request.explain)?
            {
                contributions.push(contribution);
            }
        }
        return Ok(contributions);
    }

    let mut covered_calls = Vec::new();
    for &variable in &request.variables {
        if let Some(call) = resolve_source_spec_call(variable)?
            && let Some(contribution) =
                build_source_spec_contribution(variable, call, request.explain)?
        {
            covered_calls.push(call);
            contributions.push(contribution);
        }
    }
    for &call in &request.calls {
        if covered_calls.contains(&call) {
            continue;
        }
        if is_define_fixture_source_call(call)?
            && let Some(contribution) = build_source_spec_contribution(call, call, request.explain)?
        {
            contributions.push(contribution);
        }
    }
    Ok(contributions)
}

fn resolve_source_spec_call(node: NodeRef<'_>) -> Result<Option<NodeRef<'_>>, AdapterError> {
    let current = node.unwrap_expression()?.unwrap_or(node);
    match current.kind()? {
        NodeKind::CallExpression => Ok(is_define_fixture_source_call(current)?.then_some(current)),
        NodeKind::VariableDeclaration => {
            let Some(init) = current.safe_initializer()? else {
                return Ok(None);
            };
            let init = init.unwrap_expression()?.unwrap_or(init);
            if init.kind()? == NodeKind::CallExpression && is_define_fixture_source_call(init)? {
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
            resolve_source_spec_call(decl)
        }
        _ => Ok(None),
    }
}

fn is_define_fixture_source_call(call: NodeRef<'_>) -> Result<bool, AdapterError> {
    if call.kind()? != NodeKind::CallExpression {
        return Ok(false);
    }
    let Some(callee) = call.expression()? else {
        return Ok(false);
    };
    let callee = callee.unwrap_expression()?.unwrap_or(callee);
    let text = callee.text()?.unwrap_or_default();
    Ok(text == "defineFixtureSource" || text.ends_with(".defineFixtureSource"))
}

fn build_source_spec_contribution<'a>(
    subject: NodeRef<'a>,
    call: NodeRef<'a>,
    explain: bool,
) -> Result<Option<FactContribution<'a>>, AdapterError> {
    let args = call.arguments()?;
    if args.len() < 2 {
        return Ok(None);
    }
    let Some(source_name) = read_string_literal(args[0])? else {
        return Ok(None);
    };

    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut unknown_fields: BTreeSet<String> = BTreeSet::new();
    let mut has_dynamic_fields = false;
    let mut evidence = Vec::new();
    let mut gaps = Vec::new();
    let mut local_gaps = Vec::new();
    let call_location = call.location()?;

    if let Some(at) = call_location.clone() {
        evidence.push(Evidence {
            kind: "source".to_string(),
            message: format!("Fixture source \"{source_name}\" defined here."),
            at: Some(at),
            model_id: None,
            declaration: None,
        });
    }

    if let Some(obj) = resolve_object_literal(args[1])? {
        collect_source_spec_fields(
            &source_name,
            obj,
            call_location.as_ref(),
            explain,
            &mut fields,
            &mut unknown_fields,
            &mut has_dynamic_fields,
            &mut evidence,
            &mut gaps,
            &mut local_gaps,
        )?;
    } else {
        has_dynamic_fields = true;
        let gap = Gap {
            kind: "dynamic-fixture-fields".to_string(),
            message: format!(
                "Fixture source \"{source_name}\" fields expression could not be statically resolved."
            ),
            at: args[1].location()?,
        };
        if explain {
            local_gaps.push(ExplainLocalGap {
                status: ExplainGapResolutionStatus::Unresolved,
                fact_type_id: "fixture.source-spec".to_string(),
                provider_id: "third-party-fixture".to_string(),
                field: None,
                gap: gap.clone(),
                subject_location: call_location.clone(),
                resolved_by: None,
                impact: Some(format!(
                    "Fixture source \"{source_name}\" fields could not be statically resolved."
                )),
            });
        }
        gaps.push(gap);
    }

    let required_fields = collect_fields_by_mode(&fields, "required");
    let optional_fields = collect_fields_by_mode(&fields, "optional");
    let defaulted_fields = collect_fields_by_mode(&fields, "defaulted");
    let unknown_list: Vec<String> = unknown_fields.into_iter().collect();

    let mut contribution = FactContribution::new(
        subject,
        json!({
            "sourceName": source_name,
            "fields": fields,
            "requiredFields": required_fields,
            "optionalFields": optional_fields,
            "defaultedFields": defaulted_fields,
            "unknownFields": unknown_list,
            "hasDynamicFields": has_dynamic_fields,
        }),
    );
    contribution.evidence = evidence;
    contribution.gaps = gaps;
    if explain && !local_gaps.is_empty() {
        contribution.explain_trace = Some(FactExplainTrace { local_gaps });
    }
    Ok(Some(contribution))
}

#[expect(
    clippy::too_many_arguments,
    reason = "Collects static fields, gaps, and optional explain traces in one pass."
)]
fn collect_source_spec_fields(
    source_name: &str,
    obj: NodeRef<'_>,
    call_location: Option<&SourceLocation>,
    explain: bool,
    fields: &mut BTreeMap<String, String>,
    unknown_fields: &mut BTreeSet<String>,
    has_dynamic_fields: &mut bool,
    evidence: &mut Vec<Evidence>,
    gaps: &mut Vec<Gap>,
    local_gaps: &mut Vec<ExplainLocalGap>,
) -> Result<(), AdapterError> {
    for prop in obj.properties()? {
        if prop.kind()? != NodeKind::PropertyAssignment {
            *has_dynamic_fields = true;
            let gap = Gap {
                kind: "dynamic-fixture-fields".to_string(),
                message: format!(
                    "Fixture source \"{source_name}\" contains dynamic field members."
                ),
                at: prop.location()?,
            };
            if explain {
                local_gaps.push(ExplainLocalGap {
                    status: ExplainGapResolutionStatus::Unresolved,
                    fact_type_id: "fixture.source-spec".to_string(),
                    provider_id: "third-party-fixture".to_string(),
                    field: None,
                    gap: gap.clone(),
                    subject_location: call_location.cloned(),
                    resolved_by: None,
                    impact: Some(format!(
                        "Dynamic member in fixture source \"{source_name}\" left shape incomplete."
                    )),
                });
            }
            gaps.push(gap);
            continue;
        }
        let Some(key) = read_property_name(prop)? else {
            *has_dynamic_fields = true;
            let gap = Gap {
                kind: "unresolved-fixture-field".to_string(),
                message: format!("Fixture source \"{source_name}\" has a dynamic property key."),
                at: prop.location()?,
            };
            if explain {
                local_gaps.push(ExplainLocalGap {
                    status: ExplainGapResolutionStatus::Unresolved,
                    fact_type_id: "fixture.source-spec".to_string(),
                    provider_id: "third-party-fixture".to_string(),
                    field: None,
                    gap: gap.clone(),
                    subject_location: call_location.cloned(),
                    resolved_by: None,
                    impact: Some(format!(
                        "Dynamic property key in fixture source \"{source_name}\" could not be resolved."
                    )),
                });
            }
            gaps.push(gap);
            continue;
        };
        let init = match prop.safe_initializer()? {
            Some(node) => Some(node),
            None => prop.initializer()?,
        };
        if let Some(init_node) = init
            && let Some(mode) = read_string_literal(init_node)?
            && matches!(mode.as_str(), "required" | "optional" | "defaulted")
        {
            fields.insert(key.clone(), mode.clone());
            if let Some(at) = prop.location()? {
                evidence.push(Evidence {
                    kind: "source".to_string(),
                    message: format!(
                        "Fixture source field \"{key}\" declared as \"{mode}\" in \"{source_name}\"."
                    ),
                    at: Some(at),
                    model_id: None,
                    declaration: None,
                });
            }
        } else {
            unknown_fields.insert(key.clone());
            *has_dynamic_fields = true;
            let gap = Gap {
                kind: "unresolved-fixture-field".to_string(),
                message: format!(
                    "Fixture source field \"{key}\" in \"{source_name}\" could not be statically resolved."
                ),
                at: init.map_or_else(|| prop.location(), NodeRef::location)?,
            };
            if explain {
                local_gaps.push(ExplainLocalGap {
                    status: ExplainGapResolutionStatus::Unresolved,
                    fact_type_id: "fixture.source-spec".to_string(),
                    provider_id: "third-party-fixture".to_string(),
                    field: Some(key.clone()),
                    gap: gap.clone(),
                    subject_location: call_location.cloned(),
                    resolved_by: None,
                    impact: Some(format!(
                        "Field \"{key}\" in \"{source_name}\" remained unknown after evaluation."
                    )),
                });
            }
            gaps.push(gap);
        }
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

verule_adapter_api::export_adapter!(ThirdPartyFixtureAdapter);
