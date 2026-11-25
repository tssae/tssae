use std::collections::BTreeMap;

use rustc_hash::FxHashSet;
use serde_json::{Map, Value, json};
use verule_adapter_api::{
    AdapterError, Context, Evidence, ExplainGapResolutionStatus, ExplainLocalGap, FactContribution,
    FactExplainTrace, FactQuery, FactRequest, Gap, NodeKind, NodeRef, QueriedFact, SourceLocation,
};

const SCHEMA_FIELDS_FACT: &str = "verule.validator.schema-fields";
const TABLE_COLUMNS_FACT: &str = "verule.drizzle.table-columns";
const VALIDATOR_FIELD_FACT: &str = "verule.validator.field";

#[derive(Clone)]
struct FieldState {
    name: String,
    resolved: bool,
    requirement: Option<String>,
    presence: Option<String>,
    nullable: Option<bool>,
    base_nullable: Option<bool>,
    from_default: Option<bool>,
    evidence: Vec<Evidence>,
    gaps: Vec<Gap>,
    local_gaps: Vec<ExplainLocalGap>,
}

struct RefineEntry<'a> {
    name: String,
    prop_location: Option<SourceLocation>,
    expr: NodeRef<'a>,
}

/// Extracts `ValidatorSchemaFieldsFact` contributions for verified `"drizzle-zod"` `createSelectSchema` calls.
pub fn extract_schema_fields<'a>(
    context: &'a Context<'_>,
    request: &FactRequest<'a>,
) -> Result<Vec<FactContribution<'a>>, AdapterError> {
    let factory_bindings = collect_factory_bindings(&request.variables)?;
    let candidates = collect_candidate_calls(request)?;

    let mut contributions = Vec::new();
    for call in candidates {
        if !is_verified_create_select_schema(call, &factory_bindings)? {
            continue;
        }
        contributions.push(build_schema_contribution(context, call, request.explain)?);
    }
    Ok(contributions)
}

fn collect_candidate_calls<'a>(
    request: &FactRequest<'a>,
) -> Result<Vec<NodeRef<'a>>, AdapterError> {
    if let Some(targets) = &request.targets {
        let mut calls = Vec::new();
        for &target in targets {
            let unwrapped = unwrap(target)?;
            match unwrapped.kind()? {
                NodeKind::CallExpression => calls.push(unwrapped),
                NodeKind::VariableDeclaration => {
                    if let Some(init) = unwrapped.safe_initializer()? {
                        let init = unwrap(init)?;
                        if init.kind()? == NodeKind::CallExpression {
                            calls.push(init);
                        }
                    }
                }
                _ => {}
            }
        }
        return Ok(calls);
    }

    let mut calls = Vec::new();
    for &call in &request.calls {
        if call.kind()? == NodeKind::CallExpression {
            calls.push(call);
        }
    }
    Ok(calls)
}

fn collect_factory_bindings<'a>(
    variables: &[NodeRef<'a>],
) -> Result<FxHashSet<NodeRef<'a>>, AdapterError> {
    let mut bindings = FxHashSet::default();
    for &var in variables {
        if var.kind()? != NodeKind::VariableDeclaration {
            continue;
        }
        let Some(init) = var.safe_initializer()? else {
            continue;
        };
        if !is_verified_create_schema_factory_expr(init, &mut Vec::new())? {
            continue;
        }
        let Some(name_node) = var.name()? else {
            continue;
        };
        // Destructured `const { createSelectSchema } = createSchemaFactory()` binds `BindingElement` children.
        for child in name_node.children()? {
            if child.kind()? != NodeKind::BindingElement {
                continue;
            }
            if binding_element_property_name(child)?.as_deref() == Some("createSelectSchema") {
                bindings.insert(child);
            }
        }
    }
    Ok(bindings)
}

fn binding_element_property_name(binding: NodeRef<'_>) -> Result<Option<String>, AdapterError> {
    let name_node = binding.name()?;
    let init_node = binding.initializer()?;
    for child in binding.children()? {
        if Some(child) == init_node {
            continue;
        }
        if Some(child) != name_node {
            return static_property_name(child);
        }
    }
    name_node.map(text).transpose()
}

fn is_verified_create_select_schema<'a>(
    call: NodeRef<'a>,
    factory_bindings: &FxHashSet<NodeRef<'a>>,
) -> Result<bool, AdapterError> {
    if call.kind()? != NodeKind::CallExpression {
        return Ok(false);
    }
    let Some(callee) = call.expression()? else {
        return Ok(false);
    };
    let callee = unwrap(callee)?;
    if let Some(decl) = callee.declaration()?
        && decl.package_name.as_deref() == Some("drizzle-zod")
        && decl.export_name.as_deref() == Some("createSelectSchema")
    {
        return Ok(true);
    }

    if callee.kind()? == NodeKind::PropertyAccessExpression
        && let Some((prop, Some(receiver))) = property_access(callee)?
        && prop == "createSelectSchema"
    {
        return is_verified_create_schema_factory_expr(receiver, &mut Vec::new());
    }

    if callee.kind()? == NodeKind::Identifier
        && let Some(decl_node) = callee.declaration_node()?
    {
        if factory_bindings.contains(&decl_node) {
            return Ok(true);
        }
        if decl_node.kind()? == NodeKind::VariableDeclaration
            && let Some(init) = decl_node.safe_initializer()?
        {
            return is_verified_create_select_schema_expr(init, factory_bindings, &mut Vec::new());
        }
    }

    Ok(false)
}

fn is_verified_create_select_schema_expr<'a>(
    expr: NodeRef<'a>,
    factory_bindings: &FxHashSet<NodeRef<'a>>,
    visited: &mut Vec<NodeRef<'a>>,
) -> Result<bool, AdapterError> {
    let expr = unwrap(expr)?;
    if let Some(decl) = expr.declaration()?
        && decl.package_name.as_deref() == Some("drizzle-zod")
        && decl.export_name.as_deref() == Some("createSelectSchema")
    {
        return Ok(true);
    }
    if expr.kind()? == NodeKind::PropertyAccessExpression
        && let Some((prop, Some(receiver))) = property_access(expr)?
        && prop == "createSelectSchema"
    {
        return is_verified_create_schema_factory_expr(receiver, visited);
    }
    if expr.kind()? == NodeKind::Identifier
        && let Some(decl_node) = expr.declaration_node()?
        && !visited.contains(&decl_node)
    {
        if factory_bindings.contains(&decl_node) {
            return Ok(true);
        }
        visited.push(decl_node);
        let result = if let Some(init) = decl_node.safe_initializer()? {
            is_verified_create_select_schema_expr(init, factory_bindings, visited)?
        } else {
            false
        };
        visited.pop();
        return Ok(result);
    }
    Ok(false)
}

fn is_verified_create_schema_factory_expr<'a>(
    expr: NodeRef<'a>,
    visited: &mut Vec<NodeRef<'a>>,
) -> Result<bool, AdapterError> {
    let expr = unwrap(expr)?;
    match expr.kind()? {
        NodeKind::CallExpression => {
            let Some(callee) = expr.expression()? else {
                return Ok(false);
            };
            let callee = unwrap(callee)?;
            Ok(callee.declaration()?.is_some_and(|decl| {
                decl.package_name.as_deref() == Some("drizzle-zod")
                    && decl.export_name.as_deref() == Some("createSchemaFactory")
            }))
        }
        NodeKind::Identifier => {
            let Some(decl_node) = expr.declaration_node()? else {
                return Ok(false);
            };
            if visited.contains(&decl_node) {
                return Ok(false);
            }
            visited.push(decl_node);
            let result = if let Some(init) = decl_node.safe_initializer()? {
                is_verified_create_schema_factory_expr(init, visited)?
            } else {
                false
            };
            visited.pop();
            Ok(result)
        }
        _ => Ok(false),
    }
}

fn build_schema_contribution<'a>(
    context: &'a Context<'_>,
    call: NodeRef<'a>,
    explain: bool,
) -> Result<FactContribution<'a>, AdapterError> {
    let call_loc = call.location()?;
    let args = call.arguments()?;

    let mut fields: BTreeMap<String, FieldState> = BTreeMap::new();
    let mut shape_gaps: Vec<Gap> = Vec::new();
    let mut schema_evidence: Vec<Evidence> = vec![source_evidence(
        "Zod schema derived from a Drizzle table via drizzle-zod".to_string(),
        call_loc.clone(),
    )];
    let mut has_dynamic_fields = false;

    if let Some(&table_arg) = args.first() {
        let table_targets = [table_arg];
        let table_facts = context.query_facts(&FactQuery {
            fact_type_id: TABLE_COLUMNS_FACT,
            provider_id: None,
            condition: None,
            targets: Some(&table_targets),
        })?;

        if let Some(table_fact) = table_facts.into_iter().next() {
            ingest_table_fact(
                table_fact,
                call_loc.clone(),
                &mut fields,
                &mut shape_gaps,
                &mut schema_evidence,
                &mut has_dynamic_fields,
            );
        } else {
            has_dynamic_fields = true;
            shape_gaps.push(Gap {
                kind: "unresolved-drizzle-table".to_string(),
                message: format!(
                    "Drizzle table argument \"{}\" to createSelectSchema() could not be statically resolved.",
                    text(table_arg)?
                ),
                at: table_arg.location()?,
            });
        }
    } else {
        has_dynamic_fields = true;
        shape_gaps.push(Gap {
            kind: "unresolved-drizzle-table".to_string(),
            message: "createSelectSchema() has no table argument.".to_string(),
            at: call_loc.clone(),
        });
    }

    if let Some(&refine_arg) = args.get(1)
        && unwrap(refine_arg)?.kind()? != NodeKind::UndefinedKeyword
    {
        apply_refinements(
            context,
            refine_arg,
            call_loc.clone(),
            &mut fields,
            &mut shape_gaps,
            &mut schema_evidence,
            &mut has_dynamic_fields,
        )?;
    }

    let mut all_gaps = shape_gaps.clone();
    let mut local_gaps = Vec::new();
    let mut fields_json = Map::new();
    for (name, field) in fields {
        if !field.resolved {
            has_dynamic_fields = true;
        }
        if explain {
            if !field.local_gaps.is_empty() {
                for mut lg in field.local_gaps {
                    if lg.field.is_none() {
                        lg.field = Some(name.clone());
                    }
                    if lg.subject_location.is_none() {
                        lg.subject_location = call_loc.clone();
                    }
                    local_gaps.push(lg);
                }
            } else if !field.resolved
                && let Some(first_gap) = field.gaps.first()
            {
                local_gaps.push(ExplainLocalGap {
                    status: ExplainGapResolutionStatus::Unresolved,
                    fact_type_id: SCHEMA_FIELDS_FACT.to_string(),
                    provider_id: "drizzle".to_string(),
                    field: Some(name.clone()),
                    gap: first_gap.clone(),
                    subject_location: call_loc.clone(),
                    resolved_by: None,
                    impact: Some(format!(
                        "Field \"{name}\" in createSelectSchema() could not be statically resolved."
                    )),
                });
            }
        }
        all_gaps.extend(field.gaps.clone());
        schema_evidence.extend(field.evidence.clone());
        let mut entry = Map::new();
        entry.insert("name".to_string(), json!(field.name));
        entry.insert("resolved".to_string(), json!(field.resolved));
        if let Some(req) = field.requirement {
            entry.insert("requirement".to_string(), json!(req));
        }
        if let Some(pres) = field.presence {
            entry.insert("presence".to_string(), json!(pres));
        }
        if let Some(nullable) = field.nullable {
            entry.insert("nullable".to_string(), json!(nullable));
        }
        if let Some(from_default) = field.from_default {
            entry.insert("fromDefault".to_string(), json!(from_default));
        }
        let deduped_field_ev = dedupe(field.evidence);
        if !deduped_field_ev.is_empty() {
            entry.insert("evidence".to_string(), json!(deduped_field_ev));
        }
        let deduped_field_gaps = dedupe(field.gaps);
        if !deduped_field_gaps.is_empty() {
            entry.insert("gaps".to_string(), json!(deduped_field_gaps));
        }
        fields_json.insert(name, Value::Object(entry));
    }

    let deduped_shape_gaps = dedupe(shape_gaps);
    if !deduped_shape_gaps.is_empty() {
        has_dynamic_fields = true;
        if explain {
            for shape_gap in &deduped_shape_gaps {
                local_gaps.push(ExplainLocalGap {
                    status: ExplainGapResolutionStatus::Unresolved,
                    fact_type_id: SCHEMA_FIELDS_FACT.to_string(),
                    provider_id: "drizzle".to_string(),
                    field: None,
                    gap: shape_gap.clone(),
                    subject_location: call_loc.clone(),
                    resolved_by: None,
                    impact: Some(shape_gap.message.clone()),
                });
            }
        }
    }

    let mut value = json!({
        "library": "drizzle-zod",
        "fields": fields_json,
        "hasDynamicFields": has_dynamic_fields,
    });
    if !deduped_shape_gaps.is_empty() {
        value["shapeGaps"] = json!(deduped_shape_gaps);
    }

    let mut contribution = FactContribution::new(call, value);
    contribution.evidence = dedupe(schema_evidence);
    contribution.gaps = dedupe(all_gaps);
    if explain && !local_gaps.is_empty() {
        contribution.explain_trace = Some(FactExplainTrace { local_gaps });
    }
    Ok(contribution)
}

fn ingest_table_fact(
    table_fact: QueriedFact<'_>,
    call_loc: Option<SourceLocation>,
    fields: &mut BTreeMap<String, FieldState>,
    shape_gaps: &mut Vec<Gap>,
    schema_evidence: &mut Vec<Evidence>,
    has_dynamic_fields: &mut bool,
) {
    schema_evidence.extend(table_fact.evidence.clone());
    shape_gaps.extend(table_fact.gaps);
    if table_fact
        .value
        .get("hasDynamicColumns")
        .and_then(Value::as_bool)
        == Some(true)
    {
        *has_dynamic_fields = true;
    }

    let Some(columns) = table_fact.value.get("columns").and_then(Value::as_object) else {
        return;
    };
    for (col_name, col_val) in columns {
        let nullable = col_val
            .get("nullable")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let mut col_evidence = col_val
            .get("evidence")
            .and_then(|val| serde_json::from_value::<Vec<Evidence>>(val.clone()).ok())
            .unwrap_or_default();
        if col_evidence.is_empty() {
            col_evidence.extend(table_fact.evidence.clone());
        }
        col_evidence.push(source_evidence(
            format!("Field \"{col_name}\" included in Drizzle-derived Zod schema"),
            call_loc.clone(),
        ));
        fields.insert(
            col_name.clone(),
            FieldState {
                name: col_name.clone(),
                resolved: true,
                requirement: Some("required".to_string()),
                presence: Some("required".to_string()),
                nullable: Some(nullable),
                base_nullable: Some(nullable),
                from_default: Some(false),
                evidence: dedupe(col_evidence),
                gaps: Vec::new(),
                local_gaps: Vec::new(),
            },
        );
    }
}

fn apply_refinements<'a>(
    context: &'a Context<'_>,
    refine_arg: NodeRef<'a>,
    call_loc: Option<SourceLocation>,
    fields: &mut BTreeMap<String, FieldState>,
    shape_gaps: &mut Vec<Gap>,
    schema_evidence: &mut Vec<Evidence>,
    has_dynamic_fields: &mut bool,
) -> Result<(), AdapterError> {
    let mut entries = Vec::new();
    collect_refine_entries(
        refine_arg,
        &mut entries,
        fields,
        shape_gaps,
        has_dynamic_fields,
        &mut Vec::new(),
    )?;

    if entries.is_empty() {
        return Ok(());
    }

    let target_nodes: Vec<NodeRef<'a>> = entries.iter().map(|entry| entry.expr).collect();
    let field_facts = context.query_facts(&FactQuery {
        fact_type_id: VALIDATOR_FIELD_FACT,
        provider_id: None,
        condition: None,
        targets: Some(&target_nodes),
    })?;

    for entry in entries {
        let matched = field_facts.iter().find(|fact| fact.subject == entry.expr);
        apply_single_refinement(
            entry,
            matched,
            call_loc.clone(),
            fields,
            schema_evidence,
            has_dynamic_fields,
        );
    }

    Ok(())
}

fn apply_single_refinement(
    entry: RefineEntry<'_>,
    matched: Option<&QueriedFact<'_>>,
    call_loc: Option<SourceLocation>,
    fields: &mut BTreeMap<String, FieldState>,
    schema_evidence: &mut Vec<Evidence>,
    has_dynamic_fields: &mut bool,
) {
    let base = fields.get(&entry.name).cloned();
    let mut evidence = base
        .as_ref()
        .map_or_else(Vec::new, |field| field.evidence.clone());
    evidence.push(source_evidence(
        format!(
            "Refinement for field \"{}\" in createSelectSchema()",
            entry.name
        ),
        entry.prop_location.clone().or(call_loc),
    ));

    let mut field_local_gaps = Vec::new();
    if let Some(trace) = matched.and_then(|fact| fact.explain_trace.as_ref()) {
        for lg in &trace.local_gaps {
            let mut cloned = lg.clone();
            if cloned.field.is_none() {
                cloned.field = Some(entry.name.clone());
            }
            field_local_gaps.push(cloned);
        }
    }

    if let Some(fact) = matched
        && fact.value.get("resolved").and_then(Value::as_bool) == Some(true)
    {
        evidence.extend(fact.evidence.clone());
        if let Some(val_ev) = fact
            .value
            .get("evidence")
            .and_then(|v| serde_json::from_value::<Vec<Evidence>>(v.clone()).ok())
        {
            evidence.extend(val_ev);
        }
        let mode = fact
            .value
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("replace");
        let requirement = fact
            .value
            .get("requirement")
            .and_then(Value::as_str)
            .unwrap_or("required")
            .to_string();
        let presence = fact
            .value
            .get("presence")
            .and_then(Value::as_str)
            .unwrap_or("required")
            .to_string();
        let field_nullable = fact
            .value
            .get("nullable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let from_default = fact
            .value
            .get("fromDefault")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let base_nullable = base.as_ref().and_then(|b| b.base_nullable);
        let final_nullable = if mode == "callback" {
            base_nullable.unwrap_or(false) || field_nullable
        } else {
            field_nullable
        };
        if !fact.gaps.is_empty() {
            *has_dynamic_fields = true;
        }
        schema_evidence.extend(evidence.clone());
        fields.insert(
            entry.name.clone(),
            FieldState {
                name: entry.name,
                resolved: true,
                requirement: Some(requirement),
                presence: Some(presence),
                nullable: Some(final_nullable),
                base_nullable,
                from_default: Some(from_default),
                evidence: dedupe(evidence),
                gaps: fact.gaps.clone(),
                local_gaps: field_local_gaps,
            },
        );
        return;
    }

    *has_dynamic_fields = true;
    let gaps = matched
        .filter(|fact| !fact.gaps.is_empty())
        .map_or_else(
            || {
                vec![Gap {
                    kind: "unresolved-schema-field".to_string(),
                    message: format!(
                        "Refinement for field \"{}\" in createSelectSchema() could not be statically resolved.",
                        entry.name
                    ),
                    at: entry.prop_location,
                }]
            },
            |fact| fact.gaps.clone(),
        );
    if let Some(fact) = matched {
        evidence.extend(fact.evidence.clone());
    }
    schema_evidence.extend(evidence.clone());
    fields.insert(
        entry.name.clone(),
        FieldState {
            name: entry.name,
            resolved: false,
            requirement: None,
            presence: None,
            nullable: None,
            base_nullable: base.and_then(|b| b.base_nullable),
            from_default: None,
            evidence: dedupe(evidence),
            gaps,
            local_gaps: field_local_gaps,
        },
    );
}

fn collect_refine_entries<'a>(
    expr: NodeRef<'a>,
    entries: &mut Vec<RefineEntry<'a>>,
    fields: &mut BTreeMap<String, FieldState>,
    shape_gaps: &mut Vec<Gap>,
    has_dynamic_fields: &mut bool,
    visited: &mut Vec<NodeRef<'a>>,
) -> Result<(), AdapterError> {
    let expr = unwrap(expr)?;
    if expr.kind()? == NodeKind::Identifier
        && let Some(decl_node) = expr.declaration_node()?
        && !visited.contains(&decl_node)
    {
        visited.push(decl_node);
        if let Some(init) = decl_node.safe_initializer()? {
            collect_refine_entries(
                init,
                entries,
                fields,
                shape_gaps,
                has_dynamic_fields,
                visited,
            )?;
            visited.pop();
            return Ok(());
        }
        visited.pop();
    }

    if expr.kind()? != NodeKind::ObjectLiteralExpression {
        *has_dynamic_fields = true;
        shape_gaps.push(Gap {
            kind: "dynamic-schema-shape".to_string(),
            message: format!(
                "createSelectSchema() refinement \"{}\" is not a static object literal.",
                text(expr)?
            ),
            at: expr.location()?,
        });
        return Ok(());
    }

    for property in expr.properties()? {
        match property.kind()? {
            NodeKind::PropertyAssignment => {
                let Some(name_node) = property.name()? else {
                    *has_dynamic_fields = true;
                    shape_gaps.push(Gap {
                        kind: "dynamic-schema-field".to_string(),
                        message: "createSelectSchema() refinement property has no static name."
                            .to_string(),
                        at: property.location()?,
                    });
                    continue;
                };
                let Some(name) = static_property_name(name_node)? else {
                    *has_dynamic_fields = true;
                    shape_gaps.push(Gap {
                        kind: "dynamic-schema-field".to_string(),
                        message: format!(
                            "Computed property name \"{}\" in createSelectSchema() refinement cannot be statically resolved.",
                            text(name_node)?
                        ),
                        at: name_node.location()?,
                    });
                    continue;
                };
                if let Some(init) = property.safe_initializer()? {
                    entries.push(RefineEntry {
                        name,
                        prop_location: property.location()?,
                        expr: init,
                    });
                } else {
                    mark_field_unresolved(name, property.location()?, fields, has_dynamic_fields);
                }
            }
            NodeKind::ShorthandPropertyAssignment => {
                let Some(name_node) = property.name()? else {
                    *has_dynamic_fields = true;
                    shape_gaps.push(Gap {
                        kind: "dynamic-schema-field".to_string(),
                        message: "Shorthand refinement property has no static name.".to_string(),
                        at: property.location()?,
                    });
                    continue;
                };
                let name = text(name_node)?;
                if let Some(decl) = property.declaration_node()?
                    && let Some(init) = decl.safe_initializer()?
                {
                    entries.push(RefineEntry {
                        name,
                        prop_location: property.location()?,
                        expr: init,
                    });
                } else {
                    mark_field_unresolved(name, property.location()?, fields, has_dynamic_fields);
                }
            }
            NodeKind::SpreadAssignment => {
                let spread_expr = property.expression()?.or_else(|| {
                    property
                        .children()
                        .ok()
                        .and_then(|children| children.last().copied())
                });
                if let Some(spread_expr) = spread_expr
                    && can_resolve_object_literal(spread_expr, visited)?
                {
                    collect_refine_entries(
                        spread_expr,
                        entries,
                        fields,
                        shape_gaps,
                        has_dynamic_fields,
                        visited,
                    )?;
                } else {
                    *has_dynamic_fields = true;
                    shape_gaps.push(Gap {
                        kind: "dynamic-schema-spread".to_string(),
                        message: format!(
                            "Spread \"{}\" in createSelectSchema() refinement cannot be statically resolved.",
                            text(property)?
                        ),
                        at: property.location()?,
                    });
                }
            }
            _ => {
                *has_dynamic_fields = true;
                shape_gaps.push(Gap {
                    kind: "dynamic-schema-field".to_string(),
                    message: format!(
                        "Unsupported dynamic member \"{}\" in createSelectSchema() refinement.",
                        text(property)?
                    ),
                    at: property.location()?,
                });
            }
        }
    }

    Ok(())
}

fn can_resolve_object_literal<'a>(
    expr: NodeRef<'a>,
    visited: &[NodeRef<'a>],
) -> Result<bool, AdapterError> {
    let expr = unwrap(expr)?;
    if expr.kind()? == NodeKind::ObjectLiteralExpression {
        return Ok(true);
    }
    if expr.kind()? == NodeKind::Identifier
        && let Some(decl_node) = expr.declaration_node()?
        && !visited.contains(&decl_node)
        && let Some(init) = decl_node.safe_initializer()?
    {
        let mut next_visited = visited.to_vec();
        next_visited.push(decl_node);
        return can_resolve_object_literal(init, &next_visited);
    }
    Ok(false)
}

fn mark_field_unresolved(
    name: String,
    location: Option<SourceLocation>,
    fields: &mut BTreeMap<String, FieldState>,
    has_dynamic_fields: &mut bool,
) {
    *has_dynamic_fields = true;
    let base_nullable = fields.get(&name).and_then(|b| b.base_nullable);
    fields.insert(
        name.clone(),
        FieldState {
            name: name.clone(),
            resolved: false,
            requirement: None,
            presence: None,
            nullable: None,
            base_nullable,
            from_default: None,
            evidence: Vec::new(),
            gaps: vec![Gap {
                kind: "unresolved-schema-field".to_string(),
                message: format!("Refinement for field \"{name}\" has no inspectable expression."),
                at: location,
            }],
            local_gaps: Vec::new(),
        },
    );
}

fn property_access(
    expression: NodeRef<'_>,
) -> Result<Option<(String, Option<NodeRef<'_>>)>, AdapterError> {
    if expression.kind()? != NodeKind::PropertyAccessExpression {
        return Ok(None);
    }
    let Some(name) = expression.name()? else {
        return Ok(None);
    };
    Ok(Some((text(name)?, expression.expression()?)))
}

fn static_property_name(name: NodeRef<'_>) -> Result<Option<String>, AdapterError> {
    match name.kind()? {
        NodeKind::Identifier => Ok(Some(text(name)?)),
        NodeKind::StringLiteral
        | NodeKind::NumericLiteral
        | NodeKind::NoSubstitutionTemplateLiteral => Ok(Some(strip_quotes(&text(name)?))),
        NodeKind::ComputedPropertyName => {
            let Some(expression) = name.expression()? else {
                return Ok(None);
            };
            let expression = unwrap(expression)?;
            if matches!(
                expression.kind()?,
                NodeKind::StringLiteral
                    | NodeKind::NumericLiteral
                    | NodeKind::NoSubstitutionTemplateLiteral
            ) {
                Ok(Some(strip_quotes(&text(expression)?)))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

fn strip_quotes(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let first = trimmed.as_bytes()[0];
        let last = *trimmed.as_bytes().last().unwrap_or(&0);
        if matches!((first, last), (b'\'', b'\'') | (b'"', b'"') | (b'`', b'`')) {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn unwrap(node: NodeRef<'_>) -> Result<NodeRef<'_>, AdapterError> {
    Ok(node.unwrap_expression()?.unwrap_or(node))
}

fn text(node: NodeRef<'_>) -> Result<String, AdapterError> {
    Ok(node.text()?.unwrap_or_default())
}

fn source_evidence(message: String, at: Option<SourceLocation>) -> Evidence {
    Evidence {
        kind: "source".to_string(),
        message,
        at,
        model_id: None,
        declaration: None,
    }
}

fn dedupe<T: PartialEq>(items: Vec<T>) -> Vec<T> {
    let mut output = Vec::new();
    for item in items {
        if !output.contains(&item) {
            output.push(item);
        }
    }
    output
}

pub fn fact_type_id() -> &'static str {
    SCHEMA_FIELDS_FACT
}
