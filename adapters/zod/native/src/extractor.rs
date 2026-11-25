use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use verule_adapter_api::{
    AdapterError, Context, Evidence, ExplainGapResolution, ExplainGapResolutionStatus,
    ExplainLocalGap, FactContribution, FactExplainTrace, FactQuery, FactRequest, Gap, NodeKind,
    NodeRef, QueriedFact, SourceLocation,
};

const INPUT_FACT: &str = "verule.validator.input-fields";
const OUTPUT_FACT: &str = "verule.validator.output-fields";
const FIELD_FACT: &str = "verule.validator.field";
const SCHEMA_FIELDS_FACT: &str = "verule.validator.schema-fields";
const VALIDATOR_METHODS: [&str; 4] = ["parse", "safeParse", "parseAsync", "safeParseAsync"];
const SCHEMA_FACTORY_METHODS: [&str; 18] = [
    "object",
    "strictObject",
    "looseObject",
    "record",
    "union",
    "discriminatedUnion",
    "intersection",
    "extend",
    "merge",
    "pick",
    "omit",
    "partial",
    "required",
    "transform",
    "refine",
    "superRefine",
    "pipe",
    "openapi",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputRequirement {
    Required,
    Optional,
    Defaulted,
}

impl InputRequirement {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::Defaulted => "defaulted",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputPresence {
    Required,
    Optional,
}

impl OutputPresence {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
        }
    }
}

#[derive(Clone, Debug)]
struct InputFieldState {
    requirement: InputRequirement,
    nullable: bool,
    evidence: Vec<Evidence>,
}

impl InputFieldState {
    const fn required(evidence: Vec<Evidence>) -> Self {
        Self {
            requirement: InputRequirement::Required,
            nullable: false,
            evidence,
        }
    }
}

#[derive(Clone, Debug)]
struct OutputFieldState {
    presence: OutputPresence,
    nullable: bool,
    from_default: bool,
    evidence: Vec<Evidence>,
}

impl OutputFieldState {
    const fn required(evidence: Vec<Evidence>) -> Self {
        Self {
            presence: OutputPresence::Required,
            nullable: false,
            from_default: false,
            evidence,
        }
    }
}

trait SideField: Clone {
    fn evidence(&self) -> &[Evidence];
    fn first_evidence_location(&self) -> Option<SourceLocation> {
        self.evidence().first().and_then(|ev| ev.at.clone())
    }
    fn is_definitely_required(&self) -> bool;
    fn conclusion_summary(&self, name: &str) -> String;
    fn intersect(&self, other: &Self) -> Self;
    fn union_with(all_present: bool, fields: &[&Self]) -> Self;
}

impl SideField for InputFieldState {
    fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }

    fn is_definitely_required(&self) -> bool {
        self.requirement == InputRequirement::Required && !self.nullable
    }

    fn conclusion_summary(&self, name: &str) -> String {
        format!(
            "field \"{name}\": {} (nullable={})",
            self.requirement.as_str(),
            self.nullable
        )
    }

    fn intersect(&self, other: &Self) -> Self {
        Self {
            requirement: if self.requirement == InputRequirement::Required
                || other.requirement == InputRequirement::Required
            {
                InputRequirement::Required
            } else if self.requirement == InputRequirement::Defaulted
                || other.requirement == InputRequirement::Defaulted
            {
                InputRequirement::Defaulted
            } else {
                InputRequirement::Optional
            },
            nullable: self.nullable && other.nullable,
            evidence: dedupe_evidence([self.evidence.clone(), other.evidence.clone()].concat()),
        }
    }

    fn union_with(all_present: bool, fields: &[&Self]) -> Self {
        let required = all_present
            && fields
                .iter()
                .all(|field| field.requirement == InputRequirement::Required);
        let all_defaulted = all_present
            && fields
                .iter()
                .all(|field| field.requirement == InputRequirement::Defaulted);
        Self {
            requirement: if required {
                InputRequirement::Required
            } else if all_defaulted {
                InputRequirement::Defaulted
            } else {
                InputRequirement::Optional
            },
            nullable: fields.iter().any(|field| field.nullable),
            evidence: dedupe_evidence(
                fields
                    .iter()
                    .flat_map(|field| field.evidence.clone())
                    .collect(),
            ),
        }
    }
}

impl SideField for OutputFieldState {
    fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }

    fn is_definitely_required(&self) -> bool {
        self.presence == OutputPresence::Required && !self.nullable
    }

    fn conclusion_summary(&self, name: &str) -> String {
        format!(
            "field \"{name}\": {} (nullable={})",
            self.presence.as_str(),
            self.nullable
        )
    }

    fn intersect(&self, other: &Self) -> Self {
        Self {
            presence: if self.presence == OutputPresence::Required
                || other.presence == OutputPresence::Required
            {
                OutputPresence::Required
            } else {
                OutputPresence::Optional
            },
            nullable: self.nullable && other.nullable,
            from_default: self.from_default || other.from_default,
            evidence: dedupe_evidence([self.evidence.clone(), other.evidence.clone()].concat()),
        }
    }

    fn union_with(all_present: bool, fields: &[&Self]) -> Self {
        let output_required = all_present
            && fields
                .iter()
                .all(|field| field.presence == OutputPresence::Required);
        Self {
            presence: if output_required {
                OutputPresence::Required
            } else {
                OutputPresence::Optional
            },
            nullable: fields.iter().any(|field| field.nullable),
            from_default: output_required && fields.iter().any(|field| field.from_default),
            evidence: dedupe_evidence(
                fields
                    .iter()
                    .flat_map(|field| field.evidence.clone())
                    .collect(),
            ),
        }
    }
}

#[derive(Clone, Debug)]
struct FieldSideResult<F> {
    field: Option<F>,
    has_dynamic_fields: bool,
    gaps: Vec<Gap>,
    local_gaps: Vec<ExplainLocalGap>,
}

impl<F> Default for FieldSideResult<F> {
    fn default() -> Self {
        Self {
            field: None,
            has_dynamic_fields: false,
            gaps: Vec::new(),
            local_gaps: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct FieldResult {
    input: FieldSideResult<InputFieldState>,
    output: FieldSideResult<OutputFieldState>,
    nullability_explicit: bool,
}

impl FieldResult {
    fn required(evidence: Vec<Evidence>) -> Self {
        Self {
            input: FieldSideResult {
                field: Some(InputFieldState::required(evidence.clone())),
                ..FieldSideResult::default()
            },
            output: FieldSideResult {
                field: Some(OutputFieldState::required(evidence)),
                ..FieldSideResult::default()
            },
            nullability_explicit: false,
        }
    }
}

#[derive(Clone, Debug)]
struct SchemaSide<F> {
    fields: BTreeMap<String, F>,
    field_gaps: BTreeMap<String, Vec<Gap>>,
    field_gap_origins: BTreeMap<String, ExplainLocalGap>,
    shape_local_gaps: Vec<ExplainLocalGap>,
    resolved_local_gaps: Vec<ExplainLocalGap>,
    explicit_dynamic: bool,
    gaps: Vec<Gap>,
}

impl<F> Default for SchemaSide<F> {
    fn default() -> Self {
        Self {
            fields: BTreeMap::new(),
            field_gaps: BTreeMap::new(),
            field_gap_origins: BTreeMap::new(),
            shape_local_gaps: Vec::new(),
            resolved_local_gaps: Vec::new(),
            explicit_dynamic: false,
            gaps: Vec::new(),
        }
    }
}

impl<F: SideField> SchemaSide<F> {
    fn has_dynamic_fields(&self) -> bool {
        self.explicit_dynamic || !self.gaps.is_empty() || !self.field_gaps.is_empty()
    }

    fn all_gaps(&self) -> Vec<Gap> {
        let mut combined = self.gaps.clone();
        for gaps in self.field_gaps.values() {
            combined.extend(gaps.clone());
        }
        dedupe_gaps(combined)
    }

    fn apply_field_result(
        &mut self,
        name: String,
        result: FieldSideResult<F>,
        schema_evidence: &mut Vec<Evidence>,
    ) {
        for mut lg in result.local_gaps {
            if lg.field.is_none() {
                lg.field = Some(name.clone());
            }
            if lg.status == ExplainGapResolutionStatus::ResolvedByOverride {
                self.resolved_local_gaps.push(lg);
            }
        }
        if let Some(field) = result.field {
            schema_evidence.extend(field.evidence().to_vec());
            self.fields.insert(name.clone(), field);
            self.field_gap_origins.remove(&name);
            if result.gaps.is_empty() {
                self.field_gaps.remove(&name);
            } else {
                self.field_gaps.insert(name, result.gaps);
            }
        } else {
            self.fields.remove(&name);
            if let Some(first_gap) = result.gaps.first() {
                self.field_gap_origins.insert(
                    name.clone(),
                    ExplainLocalGap {
                        status: ExplainGapResolutionStatus::Unresolved,
                        fact_type_id: String::new(),
                        provider_id: "zod".to_string(),
                        field: Some(name.clone()),
                        gap: first_gap.clone(),
                        subject_location: None,
                        resolved_by: None,
                        impact: Some(format!(
                            "Field \"{name}\" remained unknown because its validator could not be statically resolved."
                        )),
                    },
                );
            }
            if !result.gaps.is_empty() {
                self.field_gaps.insert(name, result.gaps);
            }
        }
        self.explicit_dynamic |=
            result.has_dynamic_fields && self.gaps.is_empty() && self.field_gaps.is_empty();
    }

    fn filter_mask(&mut self, method: &str, mask: &Mask) {
        self.fields.retain(|name, _| {
            let included = mask.keys.contains(name);
            (method == "pick" && included) || (method == "omit" && !included)
        });
        self.field_gaps.retain(|name, _| {
            let included = mask.keys.contains(name);
            (method == "pick" && included) || (method == "omit" && !included)
        });
        self.field_gap_origins.retain(|name, _| {
            let included = mask.keys.contains(name);
            (method == "pick" && included) || (method == "omit" && !included)
        });
        self.explicit_dynamic |= mask.has_dynamic_fields && mask.gaps.is_empty();
        self.gaps.extend(mask.gaps.clone());
    }

    fn take_explain_trace(
        &mut self,
        fact_type_id: &str,
        subject_location: Option<&SourceLocation>,
    ) -> Option<FactExplainTrace> {
        let mut local_gaps = std::mem::take(&mut self.resolved_local_gaps);
        local_gaps.extend(std::mem::take(&mut self.field_gap_origins).into_values());
        local_gaps.extend(std::mem::take(&mut self.shape_local_gaps));
        if local_gaps.is_empty() {
            return None;
        }
        for entry in &mut local_gaps {
            if entry.fact_type_id.is_empty() {
                entry.fact_type_id = fact_type_id.to_string();
            }
            if entry.subject_location.is_none() {
                entry.subject_location = subject_location.cloned();
            }
        }
        Some(FactExplainTrace { local_gaps })
    }
}

#[derive(Clone, Debug, Default)]
struct Schema {
    recognized: bool,
    input: SchemaSide<InputFieldState>,
    output: SchemaSide<OutputFieldState>,
    evidence: Vec<Evidence>,
}

impl Schema {
    fn unrecognized() -> Self {
        Self::default()
    }

    fn unresolved(message: String, at: Option<SourceLocation>) -> Self {
        let gap = Gap {
            kind: "unresolved-schema".to_string(),
            message: message.clone(),
            at: at.clone(),
        };
        let local_gap = ExplainLocalGap {
            status: ExplainGapResolutionStatus::Unresolved,
            fact_type_id: String::new(),
            provider_id: "zod".to_string(),
            field: None,
            gap: gap.clone(),
            subject_location: at.clone(),
            resolved_by: None,
            impact: Some(message),
        };
        Self {
            recognized: true,
            input: SchemaSide {
                shape_local_gaps: vec![local_gap.clone()],
                gaps: vec![gap.clone()],
                ..SchemaSide::default()
            },
            output: SchemaSide {
                shape_local_gaps: vec![local_gap],
                gaps: vec![gap],
                ..SchemaSide::default()
            },
            evidence: vec![source_evidence("Unresolved Zod schema".to_string(), at)],
        }
    }

    fn add_evidence(&mut self, evidence: Evidence) {
        push_unique(&mut self.evidence, evidence);
    }

    fn add_gap(&mut self, gap: Gap) {
        push_unique(&mut self.input.gaps, gap.clone());
        push_unique(&mut self.output.gaps, gap);
    }

    fn apply_field_result(&mut self, name: String, result: FieldResult) {
        self.input
            .apply_field_result(name.clone(), result.input, &mut self.evidence);
        self.output
            .apply_field_result(name, result.output, &mut self.evidence);
    }

    fn has_dynamic_fields(&self) -> bool {
        self.input.has_dynamic_fields() || self.output.has_dynamic_fields()
    }

    fn is_empty_fields(&self) -> bool {
        self.input.fields.is_empty() && self.output.fields.is_empty()
    }

    fn has_field_gaps(&self) -> bool {
        !self.input.field_gaps.is_empty() || !self.output.field_gaps.is_empty()
    }
}

struct Site<'a> {
    subject: NodeRef<'a>,
    kind: &'static str,
    validator_method: Option<String>,
    schema_name: Option<String>,
    schema: Schema,
}

/// Extracts the shared validator facts directly from the compiler-independent snapshot API.
pub fn extract<'a>(
    context: &'a Context<'_>,
    request: &FactRequest<'a>,
) -> Result<Vec<FactContribution<'a>>, AdapterError> {
    if request.fact_type_id == FIELD_FACT {
        return extract_validator_fields(request);
    }

    let scope = request
        .condition
        .as_ref()
        .and_then(|condition| condition.get("scope"))
        .and_then(Value::as_str)
        .unwrap_or("all");
    let mut sites: Vec<Site<'a>> = Vec::new();

    if scope != "schemas" {
        for &call in &request.calls {
            if !is_target(request, call) {
                continue;
            }
            if let Some(site) = ZodEvaluator::validator_call(context, call)? {
                sites.push(site);
            }
        }
    }

    if scope != "calls" {
        sites.extend(schema_sites(context, request)?);
    }

    let mut contributions = Vec::with_capacity(sites.len());
    for mut site in sites {
        let subject_location = if request.explain {
            site.subject.location()?
        } else {
            None
        };
        let (value, gaps, explain_trace) = match request.fact_type_id {
            INPUT_FACT => (
                input_value(&site),
                site.schema.input.all_gaps(),
                if request.explain {
                    site.schema
                        .input
                        .take_explain_trace(request.fact_type_id, subject_location.as_ref())
                } else {
                    None
                },
            ),
            OUTPUT_FACT => (
                output_value(&site),
                site.schema.output.all_gaps(),
                if request.explain {
                    site.schema
                        .output
                        .take_explain_trace(request.fact_type_id, subject_location.as_ref())
                } else {
                    None
                },
            ),
            _ => {
                return Err(AdapterError::UnsupportedFactType {
                    adapter_id: "zod".to_string(),
                    fact_type_id: request.fact_type_id.to_string(),
                });
            }
        };
        let mut contribution = FactContribution::new(site.subject, value);
        contribution.evidence = dedupe_evidence(site.schema.evidence.clone());
        contribution.gaps = gaps;
        contribution.explain_trace = explain_trace;
        contributions.push(contribution);
    }
    Ok(contributions)
}

fn schema_sites<'a>(
    context: &'a Context<'_>,
    request: &FactRequest<'a>,
) -> Result<Vec<Site<'a>>, AdapterError> {
    let mut sites = Vec::new();
    let mut variable_initializer_locations = Vec::new();
    for &variable in &request.variables {
        if !is_target(request, variable) {
            continue;
        }
        if let Some(site) = ZodEvaluator::schema_variable(context, variable)? {
            if request.targets.is_none()
                && let Some(initializer) = variable.safe_initializer()?
                && let Some(location) = initializer.location()?
            {
                variable_initializer_locations.push(location);
            }
            sites.push(site);
        }
    }

    sites.extend(schema_call_sites(
        context,
        request,
        &variable_initializer_locations,
    )?);
    let target_sites = target_schema_sites(context, request, &sites)?;
    sites.extend(target_sites);
    Ok(sites)
}

fn schema_call_sites<'a>(
    context: &'a Context<'_>,
    request: &FactRequest<'a>,
    variable_initializer_locations: &[SourceLocation],
) -> Result<Vec<Site<'a>>, AdapterError> {
    let mut schema_calls = Vec::new();
    for &call in &request.calls {
        if !is_target(request, call) || !is_schema_call_candidate(request, call)? {
            continue;
        }
        if let Some(location) = call.location()? {
            schema_calls.push((call, location));
        }
    }
    schema_calls.sort_by(|(_, left), (_, right)| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| right.end.cmp(&left.end))
    });
    let mut emitted_schema_locations = Vec::new();
    let mut sites = Vec::new();
    for (call, location) in schema_calls {
        if request.targets.is_none()
            && variable_initializer_locations
                .iter()
                .any(|parent| location_is_within(&location, parent))
        {
            continue;
        }
        if emitted_schema_locations
            .iter()
            .any(|parent| location_is_within(&location, parent))
        {
            continue;
        }
        let method = call_parts(call)?.map(|(method, _)| method);
        if method
            .as_deref()
            .is_some_and(|method| VALIDATOR_METHODS.contains(&method))
        {
            continue;
        }
        let schema = ZodEvaluator::evaluate_schema(context, call, &mut Vec::new())?;
        if schema.recognized {
            emitted_schema_locations.push(location);
            sites.push(Site {
                subject: call,
                kind: "schema",
                validator_method: None,
                schema_name: None,
                schema,
            });
        }
    }
    Ok(sites)
}

fn target_schema_sites<'a>(
    context: &'a Context<'_>,
    request: &FactRequest<'a>,
    existing: &[Site<'a>],
) -> Result<Vec<Site<'a>>, AdapterError> {
    let mut sites = Vec::new();
    if let Some(targets) = &request.targets {
        for &target in targets {
            if existing.iter().any(|site| site.subject == target) {
                continue;
            }
            if target.kind()? == NodeKind::VariableDeclaration {
                continue;
            }
            if target.kind()? == NodeKind::CallExpression
                && call_parts(target)?
                    .is_some_and(|(method, _)| VALIDATOR_METHODS.contains(&method.as_str()))
            {
                continue;
            }
            let schema = ZodEvaluator::evaluate_schema(context, target, &mut Vec::new())?;
            if schema.recognized {
                sites.push(Site {
                    subject: target,
                    kind: "schema",
                    validator_method: None,
                    schema_name: if target.kind()? == NodeKind::Identifier {
                        Some(text(target)?)
                    } else {
                        None
                    },
                    schema,
                });
            }
        }
    }
    Ok(sites)
}

fn is_target(request: &FactRequest<'_>, node: NodeRef<'_>) -> bool {
    request
        .targets
        .as_ref()
        .is_none_or(|targets| targets.contains(&node))
}

fn is_schema_call_candidate(
    request: &FactRequest<'_>,
    call: NodeRef<'_>,
) -> Result<bool, AdapterError> {
    if call.kind()? != NodeKind::CallExpression {
        return Ok(false);
    }
    if request.targets.is_some() {
        return Ok(true);
    }
    let Some((method, _)) = call_parts(call)? else {
        return is_zod_object_type(call);
    };
    if SCHEMA_FACTORY_METHODS.contains(&method.as_str()) {
        return Ok(true);
    }
    is_zod_object_type(call)
}

fn location_is_within(inner: &SourceLocation, outer: &SourceLocation) -> bool {
    inner.file == outer.file && inner.start >= outer.start && inner.end <= outer.end
}

fn input_value(site: &Site) -> Value {
    let mut required = Vec::new();
    let mut optional = Vec::new();
    let mut defaulted = Vec::new();
    let mut fields = Map::new();
    for (name, field) in &site.schema.input.fields {
        match field.requirement {
            InputRequirement::Required => required.push(name.clone()),
            InputRequirement::Optional => optional.push(name.clone()),
            InputRequirement::Defaulted => {
                optional.push(name.clone());
                defaulted.push(name.clone());
            }
        }
        let mut value = json!({
            "name": name,
            "requirement": field.requirement.as_str(),
            "nullable": field.nullable,
        });
        if !field.evidence.is_empty() {
            value["evidence"] = json!(dedupe_evidence(field.evidence.clone()));
        }
        fields.insert(name.clone(), value);
    }
    let mut value = json!({
        "siteKind": site.kind,
        "requiredFields": required,
        "optionalFields": optional,
        "defaultedFields": defaulted,
        "fields": fields,
        "hasDynamicFields": site.schema.input.has_dynamic_fields(),
    });
    add_site_metadata(&mut value, site);
    value
}

fn output_value(site: &Site) -> Value {
    let mut present = Vec::new();
    let mut optional = Vec::new();
    let mut defaulted = Vec::new();
    let mut fields = Map::new();
    for (name, field) in &site.schema.output.fields {
        match field.presence {
            OutputPresence::Required => present.push(name.clone()),
            OutputPresence::Optional => optional.push(name.clone()),
        }
        if field.from_default {
            defaulted.push(name.clone());
        }
        let mut value = json!({
            "name": name,
            "presence": field.presence.as_str(),
            "nullable": field.nullable,
            "fromDefault": field.from_default,
        });
        if !field.evidence.is_empty() {
            value["evidence"] = json!(dedupe_evidence(field.evidence.clone()));
        }
        fields.insert(name.clone(), value);
    }
    let mut value = json!({
        "siteKind": site.kind,
        "presentFields": present,
        "optionalFields": optional,
        "defaultedFields": defaulted,
        "fields": fields,
        "hasDynamicFields": site.schema.output.has_dynamic_fields(),
    });
    add_site_metadata(&mut value, site);
    value
}

fn add_site_metadata(value: &mut Value, site: &Site) {
    if let Some(method) = &site.validator_method {
        value["validatorMethod"] = json!(method);
    }
    if let Some(name) = &site.schema_name {
        value["schemaName"] = json!(name);
    }
}

struct ZodEvaluator;

impl ZodEvaluator {
    fn validator_call<'a>(
        context: &'a Context<'_>,
        call: NodeRef<'a>,
    ) -> Result<Option<Site<'a>>, AdapterError> {
        if call.kind()? != NodeKind::CallExpression {
            return Ok(None);
        }
        let Some((method, Some(receiver))) = call_parts(call)? else {
            return Ok(None);
        };
        if !VALIDATOR_METHODS.contains(&method.as_str()) {
            return Ok(None);
        }
        let schema = Self::evaluate_schema(context, receiver, &mut Vec::new())?;
        if !schema.recognized {
            return Ok(None);
        }
        let mut schema = schema;
        schema.add_evidence(source_evidence(
            format!("Validator call .{method}()"),
            call.location()?,
        ));
        Ok(Some(Site {
            subject: call,
            kind: "call",
            validator_method: Some(method),
            schema_name: if receiver.kind()? == NodeKind::Identifier {
                Some(text(receiver)?)
            } else {
                None
            },
            schema,
        }))
    }

    fn schema_variable<'a>(
        context: &'a Context<'_>,
        variable: NodeRef<'a>,
    ) -> Result<Option<Site<'a>>, AdapterError> {
        if variable.kind()? != NodeKind::VariableDeclaration {
            return Ok(None);
        }
        let Some(initializer) = variable.safe_initializer()? else {
            return Ok(None);
        };
        if !matches!(
            unwrap(initializer)?.kind()?,
            NodeKind::CallExpression | NodeKind::Identifier | NodeKind::PropertyAccessExpression
        ) {
            return Ok(None);
        }
        let schema = Self::evaluate_schema(context, initializer, &mut Vec::new())?;
        if !schema.recognized {
            return Ok(None);
        }
        let schema_name = variable.name()?.map(text).transpose()?;
        Ok(Some(Site {
            subject: variable,
            kind: "schema",
            validator_method: None,
            schema_name,
            schema,
        }))
    }

    fn evaluate_schema<'a>(
        context: &'a Context<'_>,
        expression: NodeRef<'a>,
        visited: &mut Vec<NodeRef<'a>>,
    ) -> Result<Schema, AdapterError> {
        let expression = unwrap(expression)?;
        match expression.kind()? {
            NodeKind::Identifier => Self::evaluate_identifier(context, expression, visited),
            NodeKind::PropertyAccessExpression => {
                let Some((name, receiver)) = property_access(expression)? else {
                    return Ok(Schema::unrecognized());
                };
                if name == "shape"
                    && let Some(receiver) = receiver
                {
                    return Self::evaluate_schema(context, receiver, visited);
                }
                if is_zod_type(expression)? {
                    Ok(Schema::unresolved(
                        format!(
                            "Zod schema reference \"{}\" has no statically resolvable object shape.",
                            text(expression)?
                        ),
                        expression.location()?,
                    ))
                } else {
                    Ok(Schema::unrecognized())
                }
            }
            NodeKind::CallExpression => Self::evaluate_schema_call(context, expression, visited),
            _ if is_zod_type(expression)? => Ok(Schema::unresolved(
                format!(
                    "Zod schema expression \"{}\" could not be statically expanded.",
                    text(expression)?
                ),
                expression.location()?,
            )),
            _ => Ok(Schema::unrecognized()),
        }
    }

    fn evaluate_identifier<'a>(
        context: &'a Context<'_>,
        identifier: NodeRef<'a>,
        visited: &mut Vec<NodeRef<'a>>,
    ) -> Result<Schema, AdapterError> {
        if let Some(declaration) = identifier.declaration_node()? {
            if visited.contains(&declaration) {
                return Ok(Schema::unresolved(
                    format!(
                        "Recursive Zod schema reference \"{}\" stops static expansion.",
                        text(identifier)?
                    ),
                    identifier.location()?,
                ));
            }
            visited.push(declaration);
            let result = if let Some(initializer) = declaration.safe_initializer()? {
                Self::evaluate_schema(context, initializer, visited)?
            } else {
                Schema::unrecognized()
            };
            visited.pop();
            if result.recognized {
                return Ok(result);
            }
        }
        if is_zod_type(identifier)? {
            return Ok(Schema::unresolved(
                format!(
                    "Zod schema \"{}\" has no statically resolvable local definition.",
                    text(identifier)?
                ),
                identifier.location()?,
            ));
        }
        Ok(Schema::unrecognized())
    }

    fn evaluate_schema_call<'a>(
        context: &'a Context<'_>,
        call: NodeRef<'a>,
        visited: &mut Vec<NodeRef<'a>>,
    ) -> Result<Schema, AdapterError> {
        let Some((method, receiver)) = call_parts(call)? else {
            return Ok(Schema::unrecognized());
        };
        let arguments = call.arguments()?;
        let location = call.location()?;

        if matches!(method.as_str(), "object" | "strictObject" | "looseObject")
            && receiver.is_some_and(is_zod_namespace)
        {
            let mut schema = if let Some(shape) = arguments.first() {
                Self::evaluate_shape(context, *shape, visited)?
            } else {
                Schema {
                    recognized: true,
                    ..Schema::default()
                }
            };
            schema.recognized = true;
            schema.add_evidence(source_evidence(
                format!("Zod object schema defined via z.{method}()"),
                location,
            ));
            return Ok(schema);
        }

        if matches!(method.as_str(), "record" | "union" | "discriminatedUnion")
            && receiver.is_some_and(is_zod_namespace)
        {
            if method == "record" {
                let mut schema = Schema {
                    recognized: true,
                    ..Schema::default()
                };
                schema.input.explicit_dynamic = true;
                schema.output.explicit_dynamic = true;
                schema.add_gap(Gap {
                    kind: "dynamic-schema-shape".to_string(),
                    message:
                        "Schema uses z.record() whose object keys cannot be statically enumerated."
                            .to_string(),
                    at: location.clone(),
                });
                schema.add_evidence(source_evidence(
                    "Dynamic Zod record schema".to_string(),
                    location,
                ));
                return Ok(schema);
            }
            return Self::evaluate_union(context, call, method.as_str(), &arguments, visited);
        }

        if matches!(method.as_str(), "intersection" | "pipe")
            && receiver.is_some_and(is_zod_namespace)
        {
            return Self::evaluate_static_combinator(
                context, &method, &arguments, location, visited,
            );
        }

        if let Some(receiver) = receiver
            && let Some(schema) =
                Self::evaluate_composition(context, call, &method, receiver, &arguments, visited)?
        {
            return Ok(schema);
        }

        // Delegate external Zod-typed schema converters (such as drizzle-zod) to composable providers.
        if is_zod_type(call)? {
            let targets = [call];
            let external = context.query_facts(&FactQuery {
                fact_type_id: SCHEMA_FIELDS_FACT,
                provider_id: None,
                condition: None,
                targets: Some(&targets),
            })?;
            if let Some(schema_fact) = external.into_iter().next() {
                return Ok(schema_from_external_fact(schema_fact));
            }
            return Ok(Schema::unresolved(
                format!(
                    "Zod schema call \"{}\" could not be statically expanded.",
                    text(call)?
                ),
                location,
            ));
        }

        Ok(Schema::unrecognized())
    }

    fn evaluate_static_combinator<'a>(
        context: &'a Context<'_>,
        method: &str,
        arguments: &[NodeRef<'a>],
        location: Option<SourceLocation>,
        visited: &mut Vec<NodeRef<'a>>,
    ) -> Result<Schema, AdapterError> {
        let (Some(first), Some(second)) = (arguments.first(), arguments.get(1)) else {
            return Ok(Schema::unresolved(
                format!("z.{method}() requires two statically resolvable schemas."),
                location,
            ));
        };
        let left = Self::evaluate_schema(context, *first, visited)?;
        let right = Self::evaluate_schema(context, *second, visited)?;
        if method == "intersection" {
            return Ok(intersection_schema(
                location,
                left,
                right,
                "z.intersection()",
            ));
        }
        if left.recognized && right.recognized {
            return Ok(pipe_schema(location, &left, &right, "z.pipe()"));
        }
        if left.recognized {
            return Ok(pipe_unresolved_output(
                location,
                second.location()?,
                &left,
                "z.pipe()",
            ));
        }
        Ok(Schema::unresolved(
            "z.pipe() requires two statically resolvable schemas.".to_string(),
            location,
        ))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Composition methods mutate one schema summary and share common evidence handling."
    )]
    fn evaluate_composition<'a>(
        context: &'a Context<'_>,
        call: NodeRef<'a>,
        method: &str,
        receiver: NodeRef<'a>,
        arguments: &[NodeRef<'a>],
        visited: &mut Vec<NodeRef<'a>>,
    ) -> Result<Option<Schema>, AdapterError> {
        let location = call.location()?;
        let base = Self::evaluate_schema(context, receiver, visited)?;
        if !base.recognized {
            return Ok(None);
        }
        let mut result = base;
        result.add_evidence(source_evidence(
            format!("Schema composed with .{method}()"),
            location.clone(),
        ));

        match method {
            "extend" => {
                if let Some(shape) = arguments.first() {
                    let extension = Self::evaluate_shape(context, *shape, visited)?;
                    merge_schema(&mut result, extension, ".extend()", location.as_ref());
                }
            }
            "merge" => {
                if let Some(other) = arguments.first() {
                    let other = Self::evaluate_schema(context, *other, visited)?;
                    if other.recognized {
                        merge_schema(&mut result, other, ".merge()", location.as_ref());
                    } else {
                        result.add_gap(Gap {
                            kind: "unresolved-schema".to_string(),
                            message: "Argument to .merge() could not be statically resolved as a Zod object schema.".to_string(),
                            at: location,
                        });
                    }
                }
            }
            "pick" | "omit" => {
                if let Some(mask_expression) = arguments.first() {
                    let mask = Self::evaluate_mask(*mask_expression, visited)?;
                    result.input.filter_mask(method, &mask);
                    result.output.filter_mask(method, &mask);
                } else {
                    result.add_gap(Gap {
                        kind: "dynamic-schema-mask".to_string(),
                        message: format!(".{method}() called without a static field mask."),
                        at: location,
                    });
                }
            }
            "partial" | "required" => {
                let mask = if let Some(mask_expression) = arguments.first() {
                    if mask_expression.kind()? == NodeKind::UndefinedKeyword {
                        None
                    } else {
                        Some(Self::evaluate_mask(*mask_expression, visited)?)
                    }
                } else {
                    None
                };
                for (name, field) in &mut result.input.fields {
                    if mask.as_ref().is_none_or(|mask| mask.keys.contains(name)) {
                        field.requirement = if method == "partial" {
                            InputRequirement::Optional
                        } else {
                            InputRequirement::Required
                        };
                    }
                }
                for (name, field) in &mut result.output.fields {
                    if mask.as_ref().is_none_or(|mask| mask.keys.contains(name)) {
                        field.presence = if method == "partial" {
                            OutputPresence::Optional
                        } else {
                            OutputPresence::Required
                        };
                        field.from_default = false;
                    }
                }
                if let Some(mask) = mask {
                    result.input.explicit_dynamic |=
                        mask.has_dynamic_fields && mask.gaps.is_empty();
                    result.output.explicit_dynamic |=
                        mask.has_dynamic_fields && mask.gaps.is_empty();
                    result.input.gaps.extend(mask.gaps.clone());
                    result.output.gaps.extend(mask.gaps);
                }
            }
            "and" => {
                if let Some(other) = arguments.first() {
                    let other = Self::evaluate_schema(context, *other, visited)?;
                    result = intersection_schema(location, result, other, ".and()");
                }
            }
            "or" => {
                if let Some(other) = arguments.first() {
                    let other = Self::evaluate_schema(context, *other, visited)?;
                    result = union_schema(location, &[result, other], ".or()");
                }
            }
            "pipe" => {
                if let Some(output_expression) = arguments.first() {
                    let output = Self::evaluate_schema(context, *output_expression, visited)?;
                    if output.recognized {
                        result = pipe_schema(location, &result, &output, ".pipe()");
                    } else {
                        result = pipe_unresolved_output(
                            location,
                            output_expression.location()?,
                            &result,
                            ".pipe()",
                        );
                    }
                } else {
                    result = pipe_unresolved_output(location, None, &result, ".pipe()");
                }
            }
            "transform" | "refine" | "superRefine" | "check" | "openapi" | "describe" | "meta"
            | "brand" | "readonly" => {}
            _ => return Ok(None),
        }
        result.recognized = true;
        Ok(Some(result))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Property kinds are handled together to preserve one pass and consistent dynamic gaps."
    )]
    fn evaluate_shape<'a>(
        context: &'a Context<'_>,
        expression: NodeRef<'a>,
        visited: &mut Vec<NodeRef<'a>>,
    ) -> Result<Schema, AdapterError> {
        let expression = unwrap(expression)?;
        if expression.kind()? == NodeKind::Identifier
            && let Some(declaration) = expression.declaration_node()?
            && !visited.contains(&declaration)
        {
            visited.push(declaration);
            let result = if let Some(initializer) = declaration.safe_initializer()? {
                Self::evaluate_shape(context, initializer, visited)?
            } else {
                Schema::unrecognized()
            };
            visited.pop();
            if result.recognized {
                return Ok(result);
            }
        }
        if expression.kind()? != NodeKind::ObjectLiteralExpression {
            if let Some((name, Some(receiver))) = property_access(expression)?
                && name == "shape"
            {
                return Self::evaluate_schema(context, receiver, visited);
            }
            return Ok(Schema::unresolved(
                format!(
                    "Zod object shape \"{}\" is not a statically inspectable object literal.",
                    text(expression)?
                ),
                expression.location()?,
            ));
        }

        let mut schema = Schema {
            recognized: true,
            ..Schema::default()
        };
        schema.add_evidence(source_evidence(
            "Zod object field shape".to_string(),
            expression.location()?,
        ));
        for property in expression.properties()? {
            match property.kind()? {
                NodeKind::PropertyAssignment => {
                    let Some(name_node) = property.name()? else {
                        schema.add_gap(dynamic_gap(
                            "dynamic-schema-field",
                            "Zod object field has no statically inspectable name.".to_string(),
                            property.location()?,
                        ));
                        continue;
                    };
                    let Some(name) = static_property_name(name_node)? else {
                        schema.add_gap(dynamic_gap(
                            "dynamic-schema-field",
                            format!(
                                "Computed property name \"{}\" in Zod schema cannot be statically resolved.",
                                text(name_node)?
                            ),
                            name_node.location()?,
                        ));
                        continue;
                    };
                    let field_location = property.location()?;
                    let field_evidence = vec![source_evidence(
                        format!("Field \"{name}\" defined in Zod schema"),
                        field_location,
                    )];
                    let field = if let Some(initializer) = property.safe_initializer()? {
                        Self::evaluate_field(initializer, field_evidence, None, visited)?
                    } else {
                        let missing_gap = Gap {
                            kind: "unresolved-schema-field".to_string(),
                            message: format!(
                                "Field \"{name}\" has no inspectable schema initializer."
                            ),
                            at: property.location()?,
                        };
                        FieldResult {
                            input: FieldSideResult {
                                has_dynamic_fields: true,
                                gaps: vec![missing_gap.clone()],
                                ..FieldSideResult::default()
                            },
                            output: FieldSideResult {
                                has_dynamic_fields: true,
                                gaps: vec![missing_gap],
                                ..FieldSideResult::default()
                            },
                            nullability_explicit: false,
                        }
                    };
                    schema.apply_field_result(name, field);
                }
                NodeKind::ShorthandPropertyAssignment => {
                    let Some(name_node) = property.name()? else {
                        schema.add_gap(dynamic_gap(
                            "dynamic-schema-field",
                            "Zod shorthand field has no statically inspectable name.".to_string(),
                            property.location()?,
                        ));
                        continue;
                    };
                    let name = text(name_node)?;
                    let field_evidence = vec![source_evidence(
                        format!("Field \"{name}\" defined in Zod schema"),
                        property.location()?,
                    )];
                    let field = if let Some(declaration) = property.declaration_node()? {
                        if let Some(initializer) = declaration.safe_initializer()? {
                            Self::evaluate_field(initializer, field_evidence, None, visited)?
                        } else {
                            unresolved_field(property.location()?)
                        }
                    } else {
                        unresolved_field(property.location()?)
                    };
                    schema.apply_field_result(name, field);
                }
                NodeKind::SpreadAssignment => {
                    let expression = property.expression()?.or_else(|| {
                        property
                            .children()
                            .ok()
                            .and_then(|children| children.last().copied())
                    });
                    if let Some(expression) = expression {
                        let spread = Self::evaluate_shape(context, expression, visited)?;
                        if spread.recognized
                            && spread.has_dynamic_fields()
                            && spread.is_empty_fields()
                            && !spread.has_field_gaps()
                        {
                            schema.add_gap(dynamic_gap(
                                "dynamic-schema-spread",
                                format!(
                                    "Spread \"{}\" in Zod schema shape cannot be statically resolved.",
                                    text(property)?
                                ),
                                property.location()?,
                            ));
                            schema.evidence.extend(spread.evidence);
                        } else if spread.recognized {
                            let spread_loc = property.location()?;
                            merge_schema(&mut schema, spread, "spread", spread_loc.as_ref());
                        } else {
                            schema.add_gap(dynamic_gap(
                                "dynamic-schema-spread",
                                format!("Spread \"{}\" in Zod schema shape cannot be statically resolved.", text(property)?),
                                property.location()?,
                            ));
                        }
                    } else {
                        schema.add_gap(dynamic_gap(
                            "dynamic-schema-spread",
                            format!(
                                "Spread \"{}\" in Zod schema shape cannot be statically resolved.",
                                text(property)?
                            ),
                            property.location()?,
                        ));
                    }
                }
                _ => schema.add_gap(dynamic_gap(
                    "dynamic-schema-field",
                    format!(
                        "Unsupported dynamic member \"{}\" in Zod schema shape.",
                        text(property)?
                    ),
                    property.location()?,
                )),
            }
        }
        schema.evidence = dedupe_evidence(schema.evidence);
        schema.input.gaps = dedupe_gaps(schema.input.gaps);
        schema.output.gaps = dedupe_gaps(schema.output.gaps);
        Ok(schema)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Field modifiers are evaluated together so their input and output effects stay explicit."
    )]
    fn evaluate_field<'a>(
        expression: NodeRef<'a>,
        evidence: Vec<Evidence>,
        callback_param: Option<&str>,
        visited: &mut Vec<NodeRef<'a>>,
    ) -> Result<FieldResult, AdapterError> {
        let expression = unwrap(expression)?;
        if expression.kind()? == NodeKind::Identifier {
            if callback_param.is_some_and(|param| text(expression).ok().as_deref() == Some(param)) {
                return Ok(FieldResult::required(evidence));
            }
            if let Some(declaration) = expression.declaration_node()?
                && !visited.contains(&declaration)
            {
                visited.push(declaration);
                let result = if let Some(initializer) = declaration.safe_initializer()? {
                    Self::evaluate_field(initializer, evidence, callback_param, visited)?
                } else {
                    unresolved_field(expression.location()?)
                };
                visited.pop();
                return Ok(result);
            }
            return Ok(unresolved_field(expression.location()?));
        }

        if expression.kind()? == NodeKind::CallExpression
            && let Some((method, Some(receiver))) = call_parts(expression)?
        {
            if matches!(
                method.as_str(),
                "optional"
                    | "nullish"
                    | "nullable"
                    | "default"
                    | "prefault"
                    | "catch"
                    | "nonoptional"
            ) {
                let mut inner =
                    Self::evaluate_field(receiver, evidence.clone(), callback_param, visited)?;
                let modifier_at = expression.location()?;
                apply_or_resolve_modifier(
                    &mut inner,
                    method.as_str(),
                    evidence,
                    modifier_at.as_ref(),
                );
                return Ok(inner);
            }
            if matches!(
                method.as_str(),
                "transform"
                    | "refine"
                    | "superRefine"
                    | "check"
                    | "openapi"
                    | "describe"
                    | "meta"
                    | "brand"
                    | "readonly"
            ) {
                return Self::evaluate_field(receiver, evidence, callback_param, visited);
            }
            if method == "pipe" {
                let left =
                    Self::evaluate_field(receiver, evidence.clone(), callback_param, visited)?;
                if let Some(output) = expression.arguments()?.first() {
                    let right = Self::evaluate_field(*output, evidence, callback_param, visited)?;
                    let mut out_side = right.output;
                    if let Some(out_field) = &mut out_side.field {
                        if let Some(in_out_field) = &left.output.field {
                            out_field.from_default |= in_out_field.from_default;
                            out_field.evidence.extend(in_out_field.evidence.clone());
                            out_field.evidence =
                                dedupe_evidence(std::mem::take(&mut out_field.evidence));
                        } else if !left.output.gaps.is_empty() {
                            let pipe_at = expression.location()?;
                            for prior_gap in left.output.gaps {
                                out_side.local_gaps.push(ExplainLocalGap {
                                    status: ExplainGapResolutionStatus::ResolvedByOverride,
                                    fact_type_id: String::new(),
                                    provider_id: "zod".to_string(),
                                    field: None,
                                    gap: prior_gap,
                                    subject_location: None,
                                    resolved_by: Some(ExplainGapResolution {
                                        provider_id: "zod".to_string(),
                                        operation: ".pipe()".to_string(),
                                        conclusion: format!(
                                            "pipe: {} (nullable={})",
                                            out_field.presence.as_str(),
                                            out_field.nullable
                                        ),
                                        at: out_field
                                            .first_evidence_location()
                                            .or_else(|| pipe_at.clone()),
                                    }),
                                    impact: Some(
                                        "Prior unknown field validator output was resolved by .pipe()."
                                            .to_string(),
                                    ),
                                });
                            }
                        }
                    } else {
                        out_side.gaps = [left.output.gaps, out_side.gaps].concat();
                        out_side.has_dynamic_fields |= left.output.has_dynamic_fields;
                    }
                    out_side.local_gaps = [left.output.local_gaps, out_side.local_gaps].concat();
                    return Ok(FieldResult {
                        input: left.input,
                        output: out_side,
                        nullability_explicit: right.nullability_explicit,
                    });
                }
            }
            if method == "preprocess"
                && let Some(schema) = expression.arguments()?.last()
            {
                return Self::evaluate_field(*schema, evidence, callback_param, visited);
            }
            if matches!(method.as_str(), "record" | "any" | "unknown" | "custom")
                && is_zod_namespace(receiver)
            {
                let gap = dynamic_gap(
                    "dynamic-schema-field",
                    format!(
                        "Field uses dynamic Zod type z.{method}() whose constraints cannot be statically determined."
                    ),
                    expression.location()?,
                );
                return Ok(FieldResult {
                    input: FieldSideResult {
                        field: Some(InputFieldState::required(evidence.clone())),
                        has_dynamic_fields: true,
                        gaps: vec![gap.clone()],
                        local_gaps: Vec::new(),
                    },
                    output: FieldSideResult {
                        field: Some(OutputFieldState::required(evidence)),
                        has_dynamic_fields: true,
                        gaps: vec![gap],
                        local_gaps: Vec::new(),
                    },
                    nullability_explicit: false,
                });
            }
            if is_zod_namespace(receiver) {
                return Ok(FieldResult::required(evidence));
            }
            // Method chains like `z.string().min(1)` or `runtimeFn().refine(...)` depend on their receiver.
            if is_zod_type(expression)? {
                return Self::evaluate_field(receiver, evidence, callback_param, visited);
            }
        }

        Ok(unresolved_field(expression.location()?))
    }

    fn evaluate_mask<'a>(
        expression: NodeRef<'a>,
        visited: &mut Vec<NodeRef<'a>>,
    ) -> Result<Mask, AdapterError> {
        let expression = unwrap(expression)?;
        if expression.kind()? == NodeKind::Identifier
            && let Some(declaration) = expression.declaration_node()?
            && !visited.contains(&declaration)
        {
            visited.push(declaration);
            let result = if let Some(initializer) = declaration.safe_initializer()? {
                Self::evaluate_mask(initializer, visited)?
            } else {
                Mask::dynamic(expression.location()?)
            };
            visited.pop();
            return Ok(result);
        }
        if expression.kind()? != NodeKind::ObjectLiteralExpression {
            return Ok(Mask::dynamic(expression.location()?));
        }
        let mut mask = Mask::default();
        for property in expression.properties()? {
            if property.kind()? != NodeKind::PropertyAssignment {
                mask.has_dynamic_fields = true;
                mask.gaps.push(dynamic_gap(
                    "dynamic-schema-mask",
                    "Schema field mask contains a dynamic member.".to_string(),
                    property.location()?,
                ));
                continue;
            }
            let (Some(name_node), Some(value_node)) =
                (property.name()?, property.safe_initializer()?)
            else {
                mask.has_dynamic_fields = true;
                continue;
            };
            let Some(name) = static_property_name(name_node)? else {
                mask.has_dynamic_fields = true;
                mask.gaps.push(dynamic_gap(
                    "dynamic-schema-mask",
                    "Schema field mask contains a dynamic key.".to_string(),
                    name_node.location()?,
                ));
                continue;
            };
            match unwrap(value_node)?.kind()? {
                NodeKind::TrueKeyword => {
                    mask.keys.insert(name);
                }
                NodeKind::FalseKeyword => {}
                _ => {
                    mask.has_dynamic_fields = true;
                    mask.gaps.push(dynamic_gap(
                        "dynamic-schema-mask",
                        format!("Schema field mask for \"{name}\" is not a static boolean."),
                        property.location()?,
                    ));
                }
            }
        }
        Ok(mask)
    }

    fn evaluate_union<'a>(
        context: &'a Context<'_>,
        call: NodeRef<'a>,
        method: &str,
        arguments: &[NodeRef<'a>],
        visited: &mut Vec<NodeRef<'a>>,
    ) -> Result<Schema, AdapterError> {
        let argument_index = usize::from(method == "discriminatedUnion");
        let Some(options) = arguments.get(argument_index) else {
            return Ok(Schema::unresolved(
                "Zod union has no statically resolvable option array.".to_string(),
                call.location()?,
            ));
        };
        let options = unwrap(*options)?;
        if options.kind()? != NodeKind::ArrayLiteralExpression {
            return Ok(Schema::unresolved(
                "Zod union options are not a static array literal.".to_string(),
                options.location()?,
            ));
        }
        let mut branches = Vec::new();
        for branch in options.children()? {
            let schema = Self::evaluate_schema(context, branch, visited)?;
            branches.push(schema);
        }
        Ok(union_schema(call.location()?, &branches, method))
    }
}

fn apply_or_resolve_modifier(
    inner: &mut FieldResult,
    method: &str,
    evidence: Vec<Evidence>,
    modifier_at: Option<&SourceLocation>,
) {
    match method {
        "nullable" => {
            inner.nullability_explicit = true;
            if let Some(field) = &mut inner.input.field {
                field.nullable = true;
            }
            if let Some(field) = &mut inner.output.field {
                field.nullable = true;
            }
            return;
        }
        "optional" | "nullish" | "default" | "prefault" | "catch" | "nonoptional" => {}
        _ => return,
    }

    let (requirement, presence, nullable_opt, from_default) = match method {
        "optional" => (
            InputRequirement::Optional,
            OutputPresence::Optional,
            None,
            false,
        ),
        "nullish" => (
            InputRequirement::Optional,
            OutputPresence::Optional,
            Some(true),
            false,
        ),
        "default" | "prefault" | "catch" => (
            InputRequirement::Defaulted,
            OutputPresence::Required,
            None,
            true,
        ),
        "nonoptional" => (
            InputRequirement::Required,
            OutputPresence::Required,
            None,
            false,
        ),
        _ => return,
    };

    if let Some(nullable) = nullable_opt {
        inner.nullability_explicit = true;
        let _ = nullable;
    }

    if let Some(field) = &mut inner.input.field {
        field.requirement = requirement;
        if let Some(nullable) = nullable_opt {
            field.nullable = nullable;
        }
    } else {
        let field = InputFieldState {
            requirement,
            nullable: nullable_opt.unwrap_or(false),
            evidence: evidence.clone(),
        };
        record_side_modifier_resolution(
            &mut inner.input,
            method,
            &format!(
                "{}: {} (nullable={})",
                method,
                field.requirement.as_str(),
                field.nullable
            ),
            modifier_at,
        );
        inner.input.field = Some(field);
        inner.input.has_dynamic_fields = false;
    }

    if let Some(field) = &mut inner.output.field {
        field.presence = presence;
        field.from_default = from_default;
        if let Some(nullable) = nullable_opt {
            field.nullable = nullable;
        }
    } else {
        let field = OutputFieldState {
            presence,
            nullable: nullable_opt.unwrap_or(false),
            from_default,
            evidence,
        };
        record_side_modifier_resolution(
            &mut inner.output,
            method,
            &format!(
                "{}: {} (nullable={})",
                method,
                field.presence.as_str(),
                field.nullable
            ),
            modifier_at,
        );
        inner.output.field = Some(field);
        inner.output.has_dynamic_fields = false;
    }
}

fn record_side_modifier_resolution<F>(
    side: &mut FieldSideResult<F>,
    method: &str,
    conclusion: &str,
    modifier_at: Option<&SourceLocation>,
) {
    let prior_gaps = std::mem::take(&mut side.gaps);
    for prior_gap in prior_gaps {
        side.local_gaps.push(ExplainLocalGap {
            status: ExplainGapResolutionStatus::ResolvedByOverride,
            fact_type_id: String::new(),
            provider_id: "zod".to_string(),
            field: None,
            gap: prior_gap,
            subject_location: None,
            resolved_by: Some(ExplainGapResolution {
                provider_id: "zod".to_string(),
                operation: format!(".{method}()"),
                conclusion: conclusion.to_string(),
                at: modifier_at.cloned(),
            }),
            impact: Some(format!(
                "Prior unknown field validator was resolved by .{method}()."
            )),
        });
    }
}

fn extract_validator_fields<'a>(
    request: &FactRequest<'a>,
) -> Result<Vec<FactContribution<'a>>, AdapterError> {
    let Some(targets) = &request.targets else {
        return Ok(Vec::new());
    };
    let mut contributions = Vec::with_capacity(targets.len());
    for &target in targets {
        let expr = unwrap(target)?;
        let loc = expr.location()?;
        let base_evidence = vec![source_evidence(
            "Zod field validator expression".to_string(),
            loc,
        )];

        let (mode, result) = if matches!(
            expr.kind()?,
            NodeKind::ArrowFunction | NodeKind::FunctionExpression
        ) {
            ("callback", evaluate_callback_field(expr, base_evidence)?)
        } else {
            (
                "replace",
                ZodEvaluator::evaluate_field(expr, base_evidence, None, &mut Vec::new())?,
            )
        };

        let mut value = Map::new();
        let resolved = result.input.field.is_some() && result.output.field.is_some();
        value.insert("resolved".to_string(), json!(resolved));
        value.insert("mode".to_string(), json!(mode));
        let mut contribution_evidence = Vec::new();
        if let (Some(in_field), Some(out_field)) = (result.input.field, result.output.field) {
            value.insert(
                "requirement".to_string(),
                json!(in_field.requirement.as_str()),
            );
            value.insert("presence".to_string(), json!(out_field.presence.as_str()));
            value.insert(
                "nullable".to_string(),
                json!(in_field.nullable || out_field.nullable),
            );
            value.insert(
                "nullabilityExplicit".to_string(),
                json!(result.nullability_explicit),
            );
            value.insert("fromDefault".to_string(), json!(out_field.from_default));
            let deduped = dedupe_evidence([in_field.evidence, out_field.evidence].concat());
            if !deduped.is_empty() {
                value.insert("evidence".to_string(), json!(deduped));
                contribution_evidence = deduped;
            }
        }
        let mut contribution = FactContribution::new(target, Value::Object(value));
        contribution.evidence = contribution_evidence;
        contribution.gaps = dedupe_gaps([result.input.gaps, result.output.gaps].concat());
        if request.explain {
            let subject_loc = expr.location()?;
            let mut local_gaps = result.input.local_gaps;
            for entry in result.output.local_gaps {
                let duplicate = local_gaps.iter().any(|existing| {
                    existing.gap == entry.gap
                        && existing.resolved_by.as_ref().map(|r| (&r.operation, &r.at))
                            == entry.resolved_by.as_ref().map(|r| (&r.operation, &r.at))
                });
                if !duplicate {
                    local_gaps.push(entry);
                }
            }
            for entry in &mut local_gaps {
                if entry.fact_type_id.is_empty() {
                    entry.fact_type_id = FIELD_FACT.to_string();
                }
                if entry.subject_location.is_none() {
                    entry.subject_location.clone_from(&subject_loc);
                }
            }
            if !local_gaps.is_empty() {
                contribution.explain_trace = Some(FactExplainTrace { local_gaps });
            }
        }
        contributions.push(contribution);
    }
    Ok(contributions)
}

fn evaluate_callback_field(
    func: NodeRef<'_>,
    evidence: Vec<Evidence>,
) -> Result<FieldResult, AdapterError> {
    let mut param_name: Option<String> = None;
    for child in func.children()? {
        if child.kind()? == NodeKind::Parameter {
            param_name = child.name()?.map(text).transpose()?;
            break;
        }
    }
    let Some(param_name) = param_name else {
        return Ok(unresolved_field(func.location()?));
    };
    let Some(body) = func.body()? else {
        return Ok(unresolved_field(func.location()?));
    };
    let body_expr = if body.kind()? == NodeKind::Block {
        extract_single_return_expr(body)?
    } else {
        Some(body)
    };
    let Some(body_expr) = body_expr else {
        return Ok(unresolved_field(func.location()?));
    };
    ZodEvaluator::evaluate_field(
        body_expr,
        evidence,
        Some(param_name.as_str()),
        &mut Vec::new(),
    )
}

fn extract_single_return_expr(block: NodeRef<'_>) -> Result<Option<NodeRef<'_>>, AdapterError> {
    let mut found = None;
    for stmt in block.children()? {
        if stmt.kind()? == NodeKind::ReturnStatement {
            if found.is_some() {
                return Ok(None);
            }
            found = stmt.expression()?;
        } else {
            return Ok(None);
        }
    }
    Ok(found)
}

fn ingest_external_field(schema: &mut Schema, name: &str, entry: &Value, external_provider: &str) {
    let entry_gaps = entry
        .get("gaps")
        .and_then(|val| serde_json::from_value::<Vec<Gap>>(val.clone()).ok())
        .unwrap_or_default();
    if entry.get("resolved").and_then(Value::as_bool) == Some(true) {
        let requirement = match entry.get("requirement").and_then(Value::as_str) {
            Some("optional") => InputRequirement::Optional,
            Some("defaulted") => InputRequirement::Defaulted,
            _ => InputRequirement::Required,
        };
        let presence = match entry.get("presence").and_then(Value::as_str) {
            Some("optional") => OutputPresence::Optional,
            _ => OutputPresence::Required,
        };
        let nullable = entry
            .get("nullable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let from_default = entry
            .get("fromDefault")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let evidence = entry
            .get("evidence")
            .and_then(|val| serde_json::from_value::<Vec<Evidence>>(val.clone()).ok())
            .unwrap_or_default();
        schema.input.fields.insert(
            name.to_string(),
            InputFieldState {
                requirement,
                nullable,
                evidence: evidence.clone(),
            },
        );
        schema.output.fields.insert(
            name.to_string(),
            OutputFieldState {
                presence,
                nullable,
                from_default,
                evidence,
            },
        );
        if !entry_gaps.is_empty() {
            schema
                .input
                .field_gaps
                .insert(name.to_string(), entry_gaps.clone());
            schema
                .output
                .field_gaps
                .insert(name.to_string(), entry_gaps);
        }
        return;
    }

    let gaps = if entry_gaps.is_empty() {
        vec![Gap {
            kind: "unresolved-schema-field".to_string(),
            message: format!("Field \"{name}\" could not be statically resolved."),
            at: None,
        }]
    } else {
        entry_gaps
    };
    if let Some(first_gap) = gaps.first() {
        let origin = ExplainLocalGap {
            status: ExplainGapResolutionStatus::Unresolved,
            fact_type_id: SCHEMA_FIELDS_FACT.to_string(),
            provider_id: external_provider.to_string(),
            field: Some(name.to_string()),
            gap: first_gap.clone(),
            subject_location: None,
            resolved_by: None,
            impact: Some(format!(
                "Field \"{name}\" in external schema could not be statically resolved."
            )),
        };
        schema
            .input
            .field_gap_origins
            .insert(name.to_string(), origin.clone());
        schema
            .output
            .field_gap_origins
            .insert(name.to_string(), origin);
    }
    schema
        .input
        .field_gaps
        .insert(name.to_string(), gaps.clone());
    schema.output.field_gaps.insert(name.to_string(), gaps);
}

fn schema_from_external_fact(fact: QueriedFact<'_>) -> Schema {
    let external_provider = fact
        .providers
        .first()
        .cloned()
        .unwrap_or_else(|| "drizzle".to_string());
    let mut schema = Schema {
        recognized: true,
        evidence: fact.evidence,
        ..Schema::default()
    };

    if let Some(fields_obj) = fact.value.get("fields").and_then(Value::as_object) {
        for (name, entry) in fields_obj {
            ingest_external_field(&mut schema, name, entry, &external_provider);
        }
    }

    if let Some(trace) = fact.explain_trace {
        for local_gap in trace.local_gaps {
            if local_gap.status == ExplainGapResolutionStatus::ResolvedByOverride {
                schema.input.resolved_local_gaps.push(local_gap.clone());
                schema.output.resolved_local_gaps.push(local_gap);
            } else if let Some(field_name) = local_gap.field.clone() {
                schema
                    .input
                    .field_gap_origins
                    .insert(field_name.clone(), local_gap.clone());
                schema
                    .output
                    .field_gap_origins
                    .insert(field_name, local_gap);
            } else {
                schema.input.shape_local_gaps.push(local_gap.clone());
                schema.output.shape_local_gaps.push(local_gap);
            }
        }
    }

    let shape_gaps = if let Some(shape_gaps) = fact
        .value
        .get("shapeGaps")
        .and_then(|val| serde_json::from_value::<Vec<Gap>>(val.clone()).ok())
    {
        shape_gaps
    } else {
        let field_gap_set: Vec<Gap> = schema
            .input
            .field_gaps
            .values()
            .flatten()
            .cloned()
            .collect();
        fact.gaps
            .into_iter()
            .filter(|gap| !field_gap_set.contains(gap))
            .collect()
    };
    let shape_gaps = dedupe_gaps(shape_gaps);
    schema.input.gaps = shape_gaps.clone();
    schema.output.gaps = shape_gaps;

    if fact.value.get("hasDynamicFields").and_then(Value::as_bool) == Some(true)
        && schema.input.gaps.is_empty()
        && schema.input.field_gaps.is_empty()
    {
        schema.input.explicit_dynamic = true;
        schema.output.explicit_dynamic = true;
    }
    schema.evidence = dedupe_evidence(schema.evidence);
    schema
}

#[derive(Clone, Debug, Default)]
struct Mask {
    keys: BTreeSet<String>,
    has_dynamic_fields: bool,
    gaps: Vec<Gap>,
}

impl Mask {
    fn dynamic(at: Option<SourceLocation>) -> Self {
        Self {
            has_dynamic_fields: true,
            gaps: vec![Gap {
                kind: "dynamic-schema-mask".to_string(),
                message: "Schema field mask could not be statically inspected.".to_string(),
                at,
            }],
            ..Self::default()
        }
    }
}

fn union_side<F: SideField>(branches: &[&SchemaSide<F>], any_unrecognized: bool) -> SchemaSide<F> {
    let mut output = SchemaSide {
        explicit_dynamic: any_unrecognized || branches.iter().any(|branch| branch.explicit_dynamic),
        ..SchemaSide::default()
    };
    for branch in branches {
        output.gaps.extend(branch.gaps.clone());
        output
            .resolved_local_gaps
            .extend(branch.resolved_local_gaps.clone());
        output
            .shape_local_gaps
            .extend(branch.shape_local_gaps.clone());
        for (name, fgaps) in &branch.field_gaps {
            output
                .field_gaps
                .entry(name.clone())
                .or_default()
                .extend(fgaps.clone());
        }
        for (name, origin) in &branch.field_gap_origins {
            output
                .field_gap_origins
                .entry(name.clone())
                .or_insert_with(|| origin.clone());
        }
    }
    let names = branches
        .iter()
        .flat_map(|branch| branch.fields.keys().cloned())
        .collect::<BTreeSet<_>>();
    for name in names {
        if output.field_gaps.contains_key(&name)
            && branches
                .iter()
                .any(|b| b.field_gaps.contains_key(&name) && !b.fields.contains_key(&name))
        {
            continue;
        }
        let all_present = branches
            .iter()
            .all(|branch| branch.fields.contains_key(&name));
        let fields = branches
            .iter()
            .filter_map(|branch| branch.fields.get(&name))
            .collect::<Vec<_>>();
        output
            .fields
            .insert(name, F::union_with(all_present, &fields));
    }
    output.gaps = dedupe_gaps(output.gaps);
    output
}

fn union_schema(location: Option<SourceLocation>, branches: &[Schema], label: &str) -> Schema {
    if branches.is_empty() {
        return Schema::unresolved(
            format!("Empty {label} schema has indeterminate fields."),
            location,
        );
    }
    let any_unrecognized = branches.iter().any(|branch| !branch.recognized);
    let input_branches: Vec<_> = branches.iter().map(|b| &b.input).collect();
    let output_branches: Vec<_> = branches.iter().map(|b| &b.output).collect();
    let mut evidence: Vec<Evidence> = branches
        .iter()
        .flat_map(|branch| branch.evidence.clone())
        .collect();
    push_unique(
        &mut evidence,
        source_evidence(format!("Zod {label} schema"), location),
    );
    Schema {
        recognized: true,
        input: union_side(&input_branches, any_unrecognized),
        output: union_side(&output_branches, any_unrecognized),
        evidence: dedupe_evidence(evidence),
    }
}

fn resolve_intersection_gap<F: SideField>(
    known: &F,
    known_gaps: Option<&Vec<Gap>>,
    unknown_origin: Option<&ExplainLocalGap>,
    name: &str,
    label: &str,
    location: Option<&SourceLocation>,
    out: &mut SchemaSide<F>,
) {
    out.fields.insert(name.to_string(), known.clone());
    if let Some(fgaps) = known_gaps {
        out.field_gaps.insert(name.to_string(), fgaps.clone());
    }
    if let Some(mut origin) = unknown_origin.cloned() {
        let override_at = known
            .first_evidence_location()
            .or_else(|| location.cloned());
        origin.status = ExplainGapResolutionStatus::ResolvedByOverride;
        origin.resolved_by = Some(ExplainGapResolution {
            provider_id: "zod".to_string(),
            operation: label.to_string(),
            conclusion: known.conclusion_summary(name),
            at: override_at,
        });
        out.resolved_local_gaps.push(origin);
    }
}

fn intersect_one_sided_field<F: SideField>(
    known: &F,
    known_side: &SchemaSide<F>,
    other_side: &SchemaSide<F>,
    name: &str,
    label: &str,
    location: Option<&SourceLocation>,
    out: &mut SchemaSide<F>,
) {
    let known_gaps = known_side.field_gaps.get(name);
    let known_origin = known_side.field_gap_origins.get(name);
    let other_gaps = other_side.field_gaps.get(name);
    let other_origin = other_side.field_gap_origins.get(name);
    if other_gaps.is_some() {
        if known.is_definitely_required() {
            resolve_intersection_gap(known, known_gaps, other_origin, name, label, location, out);
        } else {
            let combined: Vec<Gap> = known_gaps
                .into_iter()
                .flatten()
                .chain(other_gaps.into_iter().flatten())
                .cloned()
                .collect();
            out.field_gaps
                .insert(name.to_string(), dedupe_gaps(combined));
            if let Some(origin) = other_origin.or(known_origin) {
                out.field_gap_origins
                    .insert(name.to_string(), origin.clone());
            }
        }
        return;
    }
    out.fields.insert(name.to_string(), known.clone());
    if let Some(fgaps) = known_gaps {
        out.field_gaps.insert(name.to_string(), fgaps.clone());
    }
    if let Some(origin) = known_origin {
        out.field_gap_origins
            .insert(name.to_string(), origin.clone());
    }
}

fn intersection_side<F: SideField>(
    location: Option<&SourceLocation>,
    left_recognized: bool,
    right_recognized: bool,
    left: &SchemaSide<F>,
    right: &SchemaSide<F>,
    label: &str,
) -> SchemaSide<F> {
    let mut out = SchemaSide {
        explicit_dynamic: !left_recognized
            || !right_recognized
            || left.explicit_dynamic
            || right.explicit_dynamic,
        shape_local_gaps: [
            left.shape_local_gaps.clone(),
            right.shape_local_gaps.clone(),
        ]
        .concat(),
        resolved_local_gaps: [
            left.resolved_local_gaps.clone(),
            right.resolved_local_gaps.clone(),
        ]
        .concat(),
        gaps: dedupe_gaps([left.gaps.clone(), right.gaps.clone()].concat()),
        ..SchemaSide::default()
    };

    let names: BTreeSet<String> = left
        .fields
        .keys()
        .chain(left.field_gaps.keys())
        .chain(right.fields.keys())
        .chain(right.field_gaps.keys())
        .cloned()
        .collect();

    for name in names {
        let lf = left.fields.get(&name);
        let rf = right.fields.get(&name);
        let lgaps = left.field_gaps.get(&name);
        let rgaps = right.field_gaps.get(&name);
        let lorigin = left.field_gap_origins.get(&name);
        let rorigin = right.field_gap_origins.get(&name);

        match (lf, rf) {
            (Some(l), Some(r)) => {
                out.fields.insert(name.clone(), l.intersect(r));
                let combined: Vec<Gap> = lgaps
                    .into_iter()
                    .flatten()
                    .chain(rgaps.into_iter().flatten())
                    .cloned()
                    .collect();
                if !combined.is_empty() {
                    out.field_gaps.insert(name, dedupe_gaps(combined));
                }
            }
            (Some(l), None) => {
                intersect_one_sided_field(l, left, right, &name, label, location, &mut out);
            }
            (None, Some(r)) => {
                intersect_one_sided_field(r, right, left, &name, label, location, &mut out);
            }
            (None, None) => {
                let combined: Vec<Gap> = lgaps
                    .into_iter()
                    .flatten()
                    .chain(rgaps.into_iter().flatten())
                    .cloned()
                    .collect();
                if !combined.is_empty() {
                    out.field_gaps.insert(name.clone(), dedupe_gaps(combined));
                }
                if let Some(origin) = lorigin.or(rorigin) {
                    out.field_gap_origins.insert(name, origin.clone());
                }
            }
        }
    }
    out
}

fn intersection_schema(
    location: Option<SourceLocation>,
    left: Schema,
    right: Schema,
    label: &str,
) -> Schema {
    let input = intersection_side(
        location.as_ref(),
        left.recognized,
        right.recognized,
        &left.input,
        &right.input,
        label,
    );
    let output = intersection_side(
        location.as_ref(),
        left.recognized,
        right.recognized,
        &left.output,
        &right.output,
        label,
    );
    let mut evidence = [left.evidence, right.evidence].concat();
    push_unique(
        &mut evidence,
        source_evidence(format!("Zod {label} schema"), location),
    );
    Schema {
        recognized: left.recognized || right.recognized,
        input,
        output,
        evidence: dedupe_evidence(evidence),
    }
}

fn pipe_schema(
    location: Option<SourceLocation>,
    input: &Schema,
    output: &Schema,
    label: &str,
) -> Schema {
    let input_side = input.input.clone();
    let mut output_side = output.output.clone();
    let mut resolved = input.output.resolved_local_gaps.clone();
    resolved.extend(std::mem::take(&mut output_side.resolved_local_gaps));
    for (name, mut origin) in input.output.field_gap_origins.clone() {
        if let Some(output_field) = output_side.fields.get(&name) {
            let override_at = output_field
                .first_evidence_location()
                .or_else(|| location.clone());
            origin.status = ExplainGapResolutionStatus::ResolvedByOverride;
            origin.resolved_by = Some(ExplainGapResolution {
                provider_id: "zod".to_string(),
                operation: label.to_string(),
                conclusion: output_field.conclusion_summary(&name),
                at: override_at,
            });
            resolved.push(origin);
        }
    }
    output_side.resolved_local_gaps = resolved;

    let mut evidence = [input.evidence.clone(), output.evidence.clone()].concat();
    push_unique(
        &mut evidence,
        source_evidence("Zod pipe schema".to_string(), location),
    );
    Schema {
        recognized: input.recognized || output.recognized,
        input: input_side,
        output: output_side,
        evidence: dedupe_evidence(evidence),
    }
}

fn pipe_unresolved_output(
    location: Option<SourceLocation>,
    target_location: Option<SourceLocation>,
    input: &Schema,
    label: &str,
) -> Schema {
    let target_at = target_location.or_else(|| location.clone());
    let unresolved_output = Schema::unresolved(
        format!("{label} output schema could not be statically resolved."),
        target_at,
    );
    pipe_schema(location, input, &unresolved_output, label)
}

fn merge_side<F: SideField>(
    target: &mut SchemaSide<F>,
    incoming: SchemaSide<F>,
    operation: &str,
    operation_at: Option<&SourceLocation>,
) {
    target
        .resolved_local_gaps
        .extend(incoming.resolved_local_gaps);
    target.shape_local_gaps.extend(incoming.shape_local_gaps);
    let mut incoming_known_fields = BTreeSet::new();
    for (name, field) in incoming.fields {
        incoming_known_fields.insert(name.clone());
        if let Some(mut origin) = target.field_gap_origins.remove(&name) {
            let override_at = field
                .first_evidence_location()
                .or_else(|| operation_at.cloned());
            origin.status = ExplainGapResolutionStatus::ResolvedByOverride;
            origin.resolved_by = Some(ExplainGapResolution {
                provider_id: "zod".to_string(),
                operation: operation.to_string(),
                conclusion: field.conclusion_summary(&name),
                at: override_at,
            });
            target.resolved_local_gaps.push(origin);
        }
        target.fields.insert(name.clone(), field);
        if let Some(fgaps) = incoming.field_gaps.get(&name) {
            target.field_gaps.insert(name, fgaps.clone());
        } else {
            target.field_gaps.remove(&name);
        }
    }
    for (name, fgaps) in incoming.field_gaps {
        if !incoming_known_fields.contains(&name) {
            target.fields.remove(&name);
            target.field_gap_origins.remove(&name);
            target.field_gaps.insert(name, fgaps);
        }
    }
    for (name, origin) in incoming.field_gap_origins {
        if !incoming_known_fields.contains(&name) {
            target.fields.remove(&name);
            target.field_gap_origins.insert(name, origin);
        }
    }
    target.explicit_dynamic |= incoming.explicit_dynamic;
    target.gaps.extend(incoming.gaps);
}

fn merge_schema(
    target: &mut Schema,
    incoming: Schema,
    operation: &str,
    operation_at: Option<&SourceLocation>,
) {
    merge_side(&mut target.input, incoming.input, operation, operation_at);
    merge_side(&mut target.output, incoming.output, operation, operation_at);
    target.evidence.extend(incoming.evidence);
}

fn call_parts(call: NodeRef<'_>) -> Result<Option<(String, Option<NodeRef<'_>>)>, AdapterError> {
    let Some(callee) = call.expression()? else {
        return Ok(None);
    };
    let callee = unwrap(callee)?;
    match callee.kind()? {
        NodeKind::PropertyAccessExpression => Ok(property_access(callee)?),
        NodeKind::Identifier => Ok(Some((text(callee)?, None))),
        _ => Ok(None),
    }
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

fn is_zod_namespace(node: NodeRef<'_>) -> bool {
    let Ok(kind) = node.kind() else {
        return false;
    };
    if kind == NodeKind::PropertyAccessExpression
        && let Ok(Some((name, Some(expr)))) = property_access(node)
        && name == "coerce"
    {
        return is_zod_namespace(expr);
    }
    if kind != NodeKind::Identifier {
        return false;
    }
    if node
        .text()
        .ok()
        .flatten()
        .is_some_and(|name| name == "z" || name == "zod")
    {
        return true;
    }
    node.declaration().ok().flatten().is_some_and(|decl| {
        decl.package_name
            .as_deref()
            .is_some_and(|pkg| pkg == "zod" || pkg.starts_with("zod/"))
    })
}

fn is_zod_type(node: NodeRef<'_>) -> Result<bool, AdapterError> {
    let type_text = node.type_text()?.unwrap_or_default();
    let lower = type_text.to_ascii_lowercase();
    Ok(lower.contains("zod") || lower.contains("$zod"))
}

fn is_zod_object_type(node: NodeRef<'_>) -> Result<bool, AdapterError> {
    let type_text = node.type_text()?.unwrap_or_default();
    let lower = type_text.to_ascii_lowercase();
    Ok(
        lower.contains("zodobject")
            || lower.contains("$zodobject")
            || lower.contains("buildschema"),
    )
}

fn unwrap(node: NodeRef<'_>) -> Result<NodeRef<'_>, AdapterError> {
    Ok(node.unwrap_expression()?.unwrap_or(node))
}

fn text(node: NodeRef<'_>) -> Result<String, AdapterError> {
    Ok(node.text()?.unwrap_or_default())
}

fn unresolved_field(at: Option<SourceLocation>) -> FieldResult {
    let gap = Gap {
        kind: "unresolved-schema-field".to_string(),
        message: "Zod field schema could not be statically resolved.".to_string(),
        at,
    };
    FieldResult {
        input: FieldSideResult {
            has_dynamic_fields: true,
            gaps: vec![gap.clone()],
            ..FieldSideResult::default()
        },
        output: FieldSideResult {
            has_dynamic_fields: true,
            gaps: vec![gap],
            ..FieldSideResult::default()
        },
        nullability_explicit: false,
    }
}

fn dynamic_gap(kind: &str, message: String, at: Option<SourceLocation>) -> Gap {
    Gap {
        kind: kind.to_string(),
        message,
        at,
    }
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

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn dedupe_evidence(items: Vec<Evidence>) -> Vec<Evidence> {
    let mut output = Vec::new();
    for item in items {
        push_unique(&mut output, item);
    }
    output
}

fn dedupe_gaps(items: Vec<Gap>) -> Vec<Gap> {
    let mut output = Vec::new();
    for item in items {
        push_unique(&mut output, item);
    }
    output
}
