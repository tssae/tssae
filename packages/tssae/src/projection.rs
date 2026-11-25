use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tsrs_ast::{
    Kind, Node, SourceFile, Symbol, SymbolFlags, get_source_file_of_node,
    is_function_like_declaration,
};
use tsrs_checker::{LiteralValue, Signature, Type, TypeDataTag, TypeFlags, TypeFormatFlags};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::{
    P,
    context::{CheckerLifetime, Context, with_checker_lifetime},
};
use tsrs_scanner::{
    get_ecma_line_and_utf16_character_of_position, get_ecma_line_starts, get_token_pos_of_node,
};

use crate::call_effects::{
    CallEffectsRow, EffectsModelInput, InstanceCleanupScopeRow, run_call_effects,
    run_instance_cleanup,
};
use crate::calls_values::{
    CallTargetsRow, DeclarationCallersRow, ValuePropagationResultPayload, node_text, prop_key_text,
    run_callees_of, run_callers_of, run_value_propagation, unwrap_expr_wrappers,
};
use crate::cfg::GapItem;
use crate::instance_identity::{
    InstanceModelInput, InstanceProjectionRow, InstanceResolutionRow, InstanceSelect,
    run_project_instances, run_resolve_instances,
};
use crate::native_adapter;
use crate::observe::{EngineStepDetail, ObserveCollector};
use crate::package_impl::{ImplementationResolutionRow, run_resolve_implementations};
use crate::projects::{is_project_source_file, relative_to_cwd, resolve_against_cwd};
use crate::serve::{
    BranchCoverageItem, FunctionBranchesItem, HandlePayload, RpcErr, SnapshotState,
    run_branch_coverage, run_list_branches,
};

/// Three-state projected field distinguishing omitted keys, explicit `null`, and present values.
#[derive(Default)]
enum ProjectedField<T> {
    #[default]
    Omitted,
    Value(Option<T>),
}

impl<T> ProjectedField<T> {
    const fn is_omitted(&self) -> bool {
        matches!(self, Self::Omitted)
    }

    fn from_selected(selected: bool, build: impl FnOnce() -> Option<T>) -> Self {
        if selected {
            Self::Value(build())
        } else {
            Self::Omitted
        }
    }

    const fn null_if_selected(selected: bool) -> Self {
        if selected {
            Self::Value(None)
        } else {
            Self::Omitted
        }
    }
}

impl<T: Serialize> Serialize for ProjectedField<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Omitted => serializer.serialize_none(),
            Self::Value(val) => val.serialize(serializer),
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum QueryStep {
    FindSyntax {
        #[serde(default)]
        files: Option<Vec<String>>,
        #[serde(default)]
        kinds: Option<Vec<String>>,
        #[serde(default)]
        range: Option<SourceRangeFilter>,
        #[serde(default, rename = "textIncludes")]
        text_includes: Option<Vec<String>>,
        #[serde(default, rename = "calleeNames")]
        callee_names: Option<Vec<String>>,
    },
    TypeAtLocation {
        targets: Vec<String>,
    },
    ResolveDeclarations {
        targets: Vec<String>,
        #[serde(default)]
        select: Option<DeclarationSelect>,
    },
    ResolveCalls {
        targets: Vec<String>,
        #[serde(default)]
        select: Option<CallSelect>,
    },
    ProjectCalls {
        targets: Vec<String>,
        select: CallSelect,
    },
    ProjectSignatures {
        targets: Vec<String>,
        select: SignatureSelect,
    },
    ProjectTypes {
        targets: Vec<String>,
        select: TypeSelect,
    },
    IsAssignableTo {
        pairs: Vec<AssignabilityPairInput>,
    },
    SyntaxOf {
        targets: Vec<String>,
        #[serde(default)]
        select: Option<SyntaxLookupSelect>,
    },
    ListBranches {
        targets: Vec<String>,
    },
    BranchCoverage {
        #[serde(default)]
        function: Option<String>,
        #[serde(default)]
        scope: Option<String>,
        branches: Vec<String>,
        targets: Vec<String>,
    },
    CallersOf {
        targets: Vec<String>,
        #[serde(default, rename = "lookupFiles")]
        lookup_files: Option<Vec<String>>,
    },
    CalleesOf {
        targets: Vec<String>,
    },
    ValuePropagation {
        #[serde(default)]
        propagation: Option<String>,
        #[serde(default, rename = "includeMemberAccess")]
        include_member_access: bool,
        from: Vec<String>,
        to: Vec<String>,
        #[serde(default, rename = "lookupFiles")]
        lookup_files: Option<Vec<String>>,
    },
    ResolveInstances {
        targets: Vec<String>,
        #[serde(default)]
        models: Vec<InstanceModelInput>,
        #[serde(default, rename = "lookupFiles")]
        lookup_files: Option<Vec<String>>,
    },
    ProjectInstances {
        targets: Vec<String>,
        #[serde(default)]
        select: Option<InstanceSelect>,
    },
    ResolveImplementations {
        targets: Vec<String>,
    },
    CallEffects {
        targets: Vec<String>,
        #[serde(default = "default_true", rename = "followCallbacks")]
        follow_callbacks: bool,
        #[serde(default = "default_true", rename = "includeReads")]
        include_reads: bool,
        #[serde(default = "default_true", rename = "includeWrites")]
        include_writes: bool,
        #[serde(default, rename = "maxDepth")]
        max_depth: Option<usize>,
        #[serde(default)]
        models: Vec<EffectsModelInput>,
    },
    InstanceCleanup {
        scopes: Vec<String>,
        #[serde(default)]
        targets: Vec<String>,
        #[serde(default)]
        models: Vec<EffectsModelInput>,
        #[serde(default, rename = "lookupFiles")]
        lookup_files: Option<Vec<String>>,
    },
    ExtractFacts {
        #[serde(rename = "factTypeId")]
        fact_type_id: String,
        #[serde(rename = "providerId")]
        provider_id: String,
        #[serde(default, rename = "reportProviderId")]
        report_provider_id: Option<String>,
        #[serde(default)]
        targets: Option<Vec<String>>,
        #[serde(default)]
        condition: Option<Value>,
        #[serde(default)]
        explain: bool,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRangeFilter {
    file: String,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    start_column: Option<u32>,
    #[serde(default)]
    end_line: Option<u32>,
    #[serde(default)]
    end_column: Option<u32>,
    #[serde(default)]
    start: Option<u32>,
    #[serde(default)]
    end: Option<u32>,
    #[serde(default, rename = "match")]
    match_mode: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "JSON projection selection flags map 1:1 to SDK select keys."
)]
pub struct DeclarationSelect {
    #[serde(default)]
    name: bool,
    #[serde(default)]
    container_name: bool,
    #[serde(default)]
    kind: bool,
    #[serde(default)]
    location: bool,
    #[serde(default)]
    syntax: bool,
    #[serde(default)]
    is_external: bool,
    #[serde(default)]
    declarations: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "JSON projection selection flags map 1:1 to SDK select keys."
)]
pub struct CallSelect {
    #[serde(default)]
    location: bool,
    #[serde(default)]
    syntax: bool,
    #[serde(default)]
    callee: bool,
    #[serde(default)]
    callee_declaration: bool,
    #[serde(default)]
    signature: bool,
    #[serde(default)]
    arguments: bool,
    #[serde(default)]
    instantiated_parameter_types: Option<ParameterIndexSelection>,
    #[serde(default)]
    return_type: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "JSON projection selection flags map 1:1 to SDK select keys."
)]
pub struct SignatureSelect {
    #[serde(default)]
    signature_text: bool,
    #[serde(default)]
    declaration: bool,
    #[serde(default)]
    location: bool,
    #[serde(default)]
    syntax: bool,
    #[serde(default)]
    parameters: bool,
    #[serde(default)]
    min_argument_count: bool,
    #[serde(default)]
    has_rest_parameter: bool,
    #[serde(default)]
    instantiated_parameter_types: Option<ParameterIndexSelection>,
    #[serde(default)]
    return_type: bool,
    #[serde(default)]
    is_instantiated: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "JSON projection selection flags map 1:1 to SDK select keys."
)]
pub struct TypeSelect {
    #[serde(default)]
    type_text: bool,
    #[serde(default)]
    is_union: bool,
    #[serde(default)]
    is_intersection: bool,
    #[serde(default)]
    is_literal: bool,
    #[serde(default)]
    literal_value: bool,
    #[serde(default)]
    union_members: bool,
    #[serde(default)]
    union_member_types: bool,
    #[serde(default)]
    properties: bool,
    #[serde(default)]
    type_arguments: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "JSON projection selection flags map 1:1 to SDK select keys."
)]
pub struct SyntaxLookupSelect {
    #[serde(default)]
    kind: bool,
    #[serde(default)]
    text: bool,
    #[serde(default)]
    location: bool,
    #[serde(default)]
    expression: bool,
    #[serde(default)]
    arguments: bool,
    #[serde(default)]
    name: bool,
    #[serde(default)]
    initializer: bool,
    #[serde(default)]
    body: bool,
    #[serde(default)]
    parameters: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ParameterIndexSelection {
    All(bool),
    Indices(Vec<usize>),
}

impl ParameterIndexSelection {
    fn requested_indices(&self, param_count: usize) -> Option<Vec<usize>> {
        match self {
            Self::All(false) => None,
            Self::All(true) => Some((0..param_count).collect()),
            Self::Indices(indices) => Some(indices.clone()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignabilityPairInput {
    source: String,
    target: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryBatchResult {
    results: Vec<StepResult>,
}

#[derive(Serialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
enum StepResult {
    FindSyntax {
        items: Vec<SyntaxItem>,
    },
    TypeAtLocation {
        items: Vec<TypeItem>,
    },
    ResolveDeclarations {
        items: Vec<DeclarationRow>,
    },
    ResolveCalls {
        items: Vec<CallResolutionRow>,
    },
    ProjectCalls {
        items: Vec<CallProjectionRow>,
    },
    ProjectSignatures {
        items: Vec<SignatureProjectionRow>,
    },
    ProjectTypes {
        items: Vec<TypeProjectionRow>,
    },
    IsAssignableTo {
        items: Vec<AssignabilityRow>,
    },
    SyntaxOf {
        items: Vec<SyntaxLookupRow>,
    },
    ListBranches {
        items: Vec<FunctionBranchesItem>,
    },
    BranchCoverage {
        items: Vec<BranchCoverageItem>,
    },
    CallersOf {
        items: Vec<DeclarationCallersRow>,
    },
    CalleesOf {
        items: Vec<CallTargetsRow>,
    },
    ValuePropagation {
        items: Vec<ValuePropagationResultPayload>,
    },
    ResolveInstances {
        items: Vec<InstanceResolutionRow>,
    },
    ProjectInstances {
        items: Vec<InstanceProjectionRow>,
    },
    ResolveImplementations {
        items: Vec<ImplementationResolutionRow>,
    },
    CallEffects {
        items: Vec<CallEffectsRow>,
    },
    InstanceCleanup {
        items: Vec<InstanceCleanupScopeRow>,
    },
    NativeFacts {
        items: Vec<tssae_adapter_api::NativeContribution>,
        additional_inputs: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        adapter_inputs: Vec<tssae_adapter_api::NativeAdapterInputAttribution>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sub_queries: Vec<native_adapter::NativeSubQueryTrace>,
    },
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub start: u32,
    pub end: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyntaxItem {
    #[serde(rename = "ref")]
    node_ref: String,
    kind: String,
    text: String,
    location: SourceLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    expression: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initializer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TypeSummaryPayload {
    #[serde(rename = "ref")]
    type_ref: String,
    type_text: String,
    is_union: bool,
    is_intersection: bool,
    is_literal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    literal_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    union_members: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TypeItem {
    target: String,
    type_ref: String,
    type_text: String,
    is_union: bool,
    is_intersection: bool,
    is_literal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    literal_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    union_members: Option<Vec<String>>,
    gaps: Vec<GapItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstantiatedParameterPayload {
    index: usize,
    parameter_name: Option<String>,
    is_rest: bool,
    #[serde(rename = "type")]
    type_summary: TypeSummaryPayload,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignatureParameterPayload {
    name: String,
    declaration: Option<String>,
    optional: bool,
    rest: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TypePropertyPayload {
    name: String,
    optional: bool,
    declaration: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeclarationRow {
    target: String,
    declaration: Option<String>,
    gaps: Vec<GapItem>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    name: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    container_name: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    kind: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    location: ProjectedField<SourceLocation>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    syntax: ProjectedField<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_external: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    declarations: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CallResolutionRow {
    target: String,
    call: Option<String>,
    gaps: Vec<GapItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<SourceLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    syntax: Option<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    callee: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    callee_declaration: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    signature: ProjectedField<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instantiated_parameter_types: Option<Vec<InstantiatedParameterPayload>>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    return_type: ProjectedField<TypeSummaryPayload>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CallProjectionRow {
    call: String,
    gaps: Vec<GapItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<SourceLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    syntax: Option<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    callee: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    callee_declaration: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    signature: ProjectedField<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instantiated_parameter_types: Option<Vec<InstantiatedParameterPayload>>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    return_type: ProjectedField<TypeSummaryPayload>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignatureProjectionRow {
    signature: String,
    gaps: Vec<GapItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature_text: Option<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    declaration: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    location: ProjectedField<SourceLocation>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    syntax: ProjectedField<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<Vec<SignatureParameterPayload>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_argument_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_rest_parameter: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instantiated_parameter_types: Option<Vec<InstantiatedParameterPayload>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    return_type: Option<TypeSummaryPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_instantiated: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TypeProjectionRow {
    #[serde(rename = "type")]
    type_ref: String,
    gaps: Vec<GapItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    type_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_union: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_intersection: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_literal: Option<bool>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    literal_value: ProjectedField<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    union_members: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    union_member_types: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    properties: Option<Vec<TypePropertyPayload>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    type_arguments: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AssignabilityRow {
    source: String,
    target: String,
    assignable: bool,
    gaps: Vec<GapItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyntaxLookupRow {
    target: String,
    syntax: Option<String>,
    gaps: Vec<GapItem>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    kind: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    text: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    location: ProjectedField<SourceLocation>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    expression: ProjectedField<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<Vec<String>>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    name: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    initializer: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    body: ProjectedField<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<Vec<String>>,
}

pub fn execute_query_batch(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    steps: Vec<QueryStep>,
    observer: &mut ObserveCollector,
) -> Result<QueryBatchResult, RpcErr> {
    // Hold one CheckerLifetime::API instance across every step in the batch so
    // handles, signatures, instantiated types, and assignability share one checker.
    let ctx = with_checker_lifetime(&Context::background(), CheckerLifetime::API);
    let mut checker = program.get_type_checker(&ctx);
    let mut results = Vec::with_capacity(steps.len());

    for step in steps {
        results.push(execute_step(
            snapshot,
            project_id,
            program,
            &mut checker,
            step,
            observer,
        )?);
    }

    drop(checker);
    Ok(QueryBatchResult { results })
}

fn execute_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    step: QueryStep,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    let step_start = observer.start_step();
    match step {
        QueryStep::FindSyntax {
            files,
            kinds,
            range,
            text_includes,
            callee_names,
        } => execute_find_syntax_step(
            snapshot,
            project_id,
            program,
            files.as_deref(),
            kinds.as_deref(),
            range.as_ref(),
            text_includes.as_deref(),
            callee_names.as_deref(),
            step_start,
            observer,
        ),
        QueryStep::TypeAtLocation { targets } => execute_type_at_location_step(
            snapshot, project_id, checker, &targets, step_start, observer,
        ),
        other @ (QueryStep::ResolveDeclarations { .. }
        | QueryStep::ResolveCalls { .. }
        | QueryStep::ProjectCalls { .. }
        | QueryStep::ProjectSignatures { .. }
        | QueryStep::ProjectTypes { .. }
        | QueryStep::IsAssignableTo { .. }
        | QueryStep::SyntaxOf { .. }) => execute_projection_step(
            snapshot, project_id, program, checker, other, step_start, observer,
        ),
        QueryStep::ListBranches { targets } => {
            execute_list_branches_step(snapshot, project_id, &targets, step_start, observer)
        }
        QueryStep::BranchCoverage {
            function,
            scope,
            branches,
            targets,
        } => execute_branch_coverage_step(
            snapshot,
            project_id,
            function.as_deref(),
            scope.as_deref(),
            &branches,
            &targets,
            step_start,
            observer,
        ),
        QueryStep::CallersOf {
            targets,
            lookup_files,
        } => execute_call_graph_step(
            snapshot,
            project_id,
            program,
            checker,
            &targets,
            true,
            lookup_files.as_deref(),
            step_start,
            observer,
        ),
        QueryStep::CalleesOf { targets } => execute_call_graph_step(
            snapshot, project_id, program, checker, &targets, false, None, step_start, observer,
        ),
        QueryStep::ValuePropagation {
            propagation,
            include_member_access,
            from,
            to,
            lookup_files,
        } => execute_value_propagation_step(
            snapshot,
            project_id,
            program,
            checker,
            propagation.as_deref(),
            include_member_access,
            &from,
            &to,
            lookup_files.as_deref(),
            step_start,
            observer,
        ),
        other @ (QueryStep::ResolveInstances { .. }
        | QueryStep::ProjectInstances { .. }
        | QueryStep::ResolveImplementations { .. }
        | QueryStep::CallEffects { .. }
        | QueryStep::InstanceCleanup { .. }
        | QueryStep::ExtractFacts { .. }) => execute_extended_step(
            snapshot, project_id, program, checker, other, step_start, observer,
        ),
    }
}

fn execute_extended_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    step: QueryStep,
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    match step {
        QueryStep::ResolveInstances {
            targets,
            models,
            lookup_files,
        } => {
            let items = run_resolve_instances(
                snapshot,
                project_id,
                program,
                checker,
                &targets,
                &models,
                lookup_files.as_deref(),
            )?;
            observer.record_step("resolveInstances", step_start, items.len(), Vec::new());
            Ok(StepResult::ResolveInstances { items })
        }
        QueryStep::ProjectInstances { targets, select } => {
            let items =
                run_project_instances(snapshot, project_id, &targets, &select.unwrap_or_default())?;
            observer.record_step("projectInstances", step_start, items.len(), Vec::new());
            Ok(StepResult::ProjectInstances { items })
        }
        QueryStep::ResolveImplementations { targets } => {
            let items =
                run_resolve_implementations(snapshot, project_id, program, checker, &targets)?;
            observer.record_step(
                "resolveImplementations",
                step_start,
                items.len(),
                Vec::new(),
            );
            Ok(StepResult::ResolveImplementations { items })
        }
        QueryStep::ExtractFacts {
            fact_type_id,
            provider_id,
            report_provider_id,
            targets,
            condition,
            explain,
        } => {
            let target_list = targets.as_deref().unwrap_or(&[]);
            let outcome = native_adapter::extract(
                snapshot,
                project_id,
                program,
                checker,
                &provider_id,
                report_provider_id.as_deref(),
                &fact_type_id,
                target_list,
                targets.is_some(),
                condition.as_ref(),
                explain,
                step_start,
                observer,
            )?;
            Ok(StepResult::NativeFacts {
                items: outcome.output.contributions,
                additional_inputs: outcome.output.additional_inputs,
                adapter_inputs: outcome.output.adapter_inputs,
                sub_queries: outcome.sub_queries,
            })
        }
        other => execute_effects_step(
            snapshot, project_id, program, checker, other, step_start, observer,
        ),
    }
}

fn execute_effects_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    step: QueryStep,
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    match step {
        QueryStep::CallEffects {
            targets,
            follow_callbacks,
            include_reads,
            include_writes,
            max_depth,
            models,
        } => {
            let items = run_call_effects(
                snapshot,
                project_id,
                program,
                checker,
                &targets,
                follow_callbacks,
                include_reads,
                include_writes,
                max_depth,
                &models,
            )?;
            observer.record_step("callEffects", step_start, items.len(), Vec::new());
            Ok(StepResult::CallEffects { items })
        }
        QueryStep::InstanceCleanup {
            scopes,
            targets,
            models,
            lookup_files,
        } => {
            let items = run_instance_cleanup(
                snapshot,
                project_id,
                program,
                checker,
                &scopes,
                &targets,
                &models,
                lookup_files.as_deref(),
            )?;
            observer.record_step("instanceCleanup", step_start, items.len(), Vec::new());
            Ok(StepResult::InstanceCleanup { items })
        }
        _ => unreachable!(),
    }
}

fn execute_projection_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    step: QueryStep,
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    match step {
        QueryStep::ResolveDeclarations { targets, select } => {
            let items = run_resolve_declarations(
                snapshot,
                project_id,
                program,
                checker,
                &targets,
                &select.unwrap_or_default(),
            )?;
            observer.record_step("resolveDeclarations", step_start, items.len(), Vec::new());
            Ok(StepResult::ResolveDeclarations { items })
        }
        QueryStep::ResolveCalls { targets, select } => {
            let items = run_resolve_calls(
                snapshot,
                project_id,
                checker,
                &targets,
                &select.unwrap_or_default(),
            )?;
            observer.record_step("resolveCalls", step_start, items.len(), Vec::new());
            Ok(StepResult::ResolveCalls { items })
        }
        QueryStep::ProjectCalls { targets, select } => {
            let items = run_project_calls(snapshot, project_id, checker, &targets, &select)?;
            observer.record_step("projectCalls", step_start, items.len(), Vec::new());
            Ok(StepResult::ProjectCalls { items })
        }
        QueryStep::ProjectSignatures { targets, select } => {
            let items = run_project_signatures(snapshot, project_id, checker, &targets, &select)?;
            observer.record_step("projectSignatures", step_start, items.len(), Vec::new());
            Ok(StepResult::ProjectSignatures { items })
        }
        QueryStep::ProjectTypes { targets, select } => {
            let items = run_project_types(snapshot, project_id, checker, &targets, &select)?;
            observer.record_step("projectTypes", step_start, items.len(), Vec::new());
            Ok(StepResult::ProjectTypes { items })
        }
        QueryStep::IsAssignableTo { pairs } => {
            let items = run_is_assignable_to(snapshot, project_id, checker, &pairs)?;
            observer.record_step("isAssignableTo", step_start, items.len(), Vec::new());
            Ok(StepResult::IsAssignableTo { items })
        }
        QueryStep::SyntaxOf { targets, select } => {
            let items = run_syntax_of(snapshot, project_id, &targets, &select.unwrap_or_default())?;
            observer.record_step("syntaxOf", step_start, items.len(), Vec::new());
            Ok(StepResult::SyntaxOf { items })
        }
        _ => Err(RpcErr::new(
            "INTERNAL_ERROR",
            "Unexpected non-projection query step.".to_string(),
        )),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Forwards call-graph query context alongside timing and observation collector."
)]
fn execute_call_graph_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
    is_callers: bool,
    lookup_files: Option<&[String]>,
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    if is_callers {
        let items = run_callers_of(
            snapshot,
            project_id,
            program,
            checker,
            targets,
            lookup_files,
        )?;
        observer.record_step("callersOf", step_start, items.len(), Vec::new());
        Ok(StepResult::CallersOf { items })
    } else {
        let items = run_callees_of(snapshot, project_id, program, checker, targets)?;
        observer.record_step("calleesOf", step_start, items.len(), Vec::new());
        Ok(StepResult::CalleesOf { items })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Forwards value propagation inputs alongside timing and observation collector."
)]
fn execute_value_propagation_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    propagation: Option<&str>,
    include_member_access: bool,
    from: &[String],
    to: &[String],
    lookup_files: Option<&[String]>,
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    let result = run_value_propagation(
        snapshot,
        project_id,
        program,
        checker,
        propagation,
        include_member_access,
        from,
        to,
        lookup_files,
    )?;
    observer.record_step(
        "valuePropagation",
        step_start,
        result.results.len(),
        Vec::new(),
    );
    Ok(StepResult::ValuePropagation {
        items: vec![result],
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Forwards optional findSyntax filters alongside timing and observation context."
)]
fn execute_find_syntax_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    files: Option<&[String]>,
    kinds: Option<&[String]>,
    range: Option<&SourceRangeFilter>,
    text_includes: Option<&[String]>,
    callee_names: Option<&[String]>,
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    let items = run_find_syntax(
        snapshot,
        project_id,
        program,
        files,
        kinds,
        range,
        text_includes,
        callee_names,
    )?;
    let raw_details = if observer.is_detail() {
        items
            .iter()
            .map(|item| EngineStepDetail {
                kind: "candidate",
                file: Some(item.location.file.clone()),
                line: Some(item.location.line),
                column: Some(item.location.column),
                summary: format!("{} {}", item.kind, item.text),
            })
            .collect()
    } else {
        Vec::new()
    };
    observer.record_step("findSyntax", step_start, items.len(), raw_details);
    Ok(StepResult::FindSyntax { items })
}

fn execute_type_at_location_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    targets: &[String],
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    let items = run_type_at_location(snapshot, project_id, checker, targets)?;
    let raw_details = if observer.is_detail() {
        items
            .iter()
            .map(|item| EngineStepDetail {
                kind: "typeAtLocation",
                file: None,
                line: None,
                column: None,
                summary: format!("{} -> {}", item.target, item.type_text),
            })
            .collect()
    } else {
        Vec::new()
    };
    observer.record_step("typeAtLocation", step_start, items.len(), raw_details);
    Ok(StepResult::TypeAtLocation { items })
}

fn execute_list_branches_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    targets: &[String],
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    let items = run_list_branches(snapshot, project_id, targets)?;
    let raw_details = if observer.is_detail() {
        items
            .iter()
            .map(|item| EngineStepDetail {
                kind: "listBranches",
                file: None,
                line: None,
                column: None,
                summary: format!(
                    "{} -> {} branch(es), {} gap(s)",
                    item.target,
                    item.branches.len(),
                    item.gaps.len()
                ),
            })
            .collect()
    } else {
        Vec::new()
    };
    observer.record_step("listBranches", step_start, items.len(), raw_details);
    Ok(StepResult::ListBranches { items })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Executes branchCoverage step with optional function or local scope handle."
)]
fn execute_branch_coverage_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    function: Option<&str>,
    scope: Option<&str>,
    branches: &[String],
    targets: &[String],
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    let items = run_branch_coverage(snapshot, project_id, function, scope, branches, targets)?;
    let raw_details = if observer.is_detail() {
        items
            .iter()
            .map(|item| EngineStepDetail {
                kind: "branchCoverage",
                file: None,
                line: None,
                column: None,
                summary: format!("{} -> {}", item.target, item.status),
            })
            .collect()
    } else {
        Vec::new()
    };
    observer.record_step("branchCoverage", step_start, items.len(), raw_details);
    Ok(StepResult::BranchCoverage { items })
}

#[derive(Clone, Copy)]
enum RangeMatchMode {
    Within,
    Intersects,
}

#[derive(Clone, Copy)]
struct ResolvedByteRange {
    start: u32,
    end: u32,
    mode: RangeMatchMode,
}

fn line_col_to_byte_offset(sf: P<SourceFile>, line1: u32, col1: Option<u32>, is_end: bool) -> u32 {
    let text = sf.text();
    let text_len = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let line_starts = get_ecma_line_starts(sf.get());
    let line0 = usize::try_from(line1.saturating_sub(1)).unwrap_or(0);
    if line0 >= line_starts.len() {
        return text_len;
    }

    let line_start = line_starts[line0];
    let next_line_start = line_starts.get(line0 + 1).copied().unwrap_or(text_len);

    if is_end && col1.is_none() {
        return next_line_start;
    }

    let target_utf16_col = col1.unwrap_or(1).saturating_sub(1);
    let start_idx = usize::try_from(line_start).unwrap_or(0);
    let end_idx = usize::try_from(next_line_start)
        .unwrap_or(text.len())
        .min(text.len());
    let line_slice = text.get(start_idx..end_idx).unwrap_or("");

    let mut utf16_acc = 0u32;
    for (byte_rel, ch) in line_slice.char_indices() {
        if utf16_acc >= target_utf16_col {
            return line_start + u32::try_from(byte_rel).unwrap_or(0);
        }
        utf16_acc += u32::try_from(ch.len_utf16()).unwrap_or(1);
    }

    next_line_start
}

fn resolve_byte_range(
    sf: P<SourceFile>,
    filter: &SourceRangeFilter,
) -> Result<ResolvedByteRange, RpcErr> {
    let text_len = u32::try_from(sf.text().len()).unwrap_or(u32::MAX);
    let start = filter.start.map_or_else(
        || {
            filter.start_line.map_or(0, |line1| {
                line_col_to_byte_offset(sf, line1, filter.start_column, false)
            })
        },
        |offset| offset.min(text_len),
    );

    let end = filter.end.map_or_else(
        || {
            filter.end_line.map_or(text_len, |line1| {
                line_col_to_byte_offset(sf, line1, filter.end_column, true)
            })
        },
        |offset| offset.min(text_len),
    );

    if start > end {
        return Err(RpcErr::new(
            "INVALID_PARAMS",
            format!("Invalid source range: start ({start}) exceeds end ({end})."),
        ));
    }

    let mode = match filter.match_mode.as_deref() {
        None | Some("within") => RangeMatchMode::Within,
        Some("intersects") => RangeMatchMode::Intersects,
        Some(other) => {
            return Err(RpcErr::new(
                "INVALID_PARAMS",
                format!("Unsupported range match mode: {other}"),
            ));
        }
    };

    Ok(ResolvedByteRange { start, end, mode })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Query filtering extends the existing find-syntax parameters without changing the RPC shape."
)]
fn run_find_syntax(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    files: Option<&[String]>,
    kinds: Option<&[String]>,
    range: Option<&SourceRangeFilter>,
    text_includes: Option<&[String]>,
    callee_names: Option<&[String]>,
) -> Result<Vec<SyntaxItem>, RpcErr> {
    let kinds_slice = kinds.unwrap_or(&[]);
    if kinds_slice.is_empty() && range.is_none() {
        return Err(RpcErr::new(
            "INVALID_PARAMS",
            "findSyntax requires at least one syntax kind or a source range.".to_string(),
        ));
    }

    let mut target_kinds = Vec::with_capacity(kinds_slice.len());
    for kind_name in kinds_slice {
        let Some(kind) = parse_syntax_kind(kind_name) else {
            return Err(RpcErr::new(
                "INVALID_PARAMS",
                format!("Unsupported syntax kind: {kind_name}"),
            ));
        };
        target_kinds.push((kind, kind_name.as_str()));
    }

    let text_filter = syntax_needles(text_includes);
    let callee_filter = syntax_needles(callee_names);
    if (text_includes.is_some() && text_filter.is_empty())
        || (callee_names.is_some() && callee_filter.is_empty())
    {
        return Ok(Vec::new());
    }

    let range_file =
        range.map(|r| relative_to_cwd(&snapshot.cwd, &resolve_against_cwd(&snapshot.cwd, &r.file)));

    let file_filter: Option<Vec<String>> = files.map(|list| {
        list.iter()
            .map(|f| relative_to_cwd(&snapshot.cwd, &resolve_against_cwd(&snapshot.cwd, f)))
            .collect()
    });

    let explicit_file_targeted = range.is_some() || file_filter.is_some();
    let mut source_files: Vec<(String, P<SourceFile>)> = program
        .get_source_files()
        .iter()
        .copied()
        .filter(|&sf| {
            if explicit_file_targeted {
                !program.is_source_file_default_library(sf.path())
            } else {
                is_project_source_file(program, sf)
            }
        })
        .filter_map(|sf| {
            if !text_filter.is_empty() {
                let sf_text = sf.text();
                if !text_filter.iter().any(|needle| sf_text.contains(needle)) {
                    return None;
                }
            }
            if !callee_filter.is_empty() {
                let sf_text = sf.text();
                if !callee_filter.iter().any(|name| sf_text.contains(name)) {
                    return None;
                }
            }
            let rel = relative_to_cwd(&snapshot.cwd, sf.file_name());
            if let Some(target_file) = &range_file
                && target_file != &rel
            {
                return None;
            }
            if let Some(filter) = &file_filter
                && !filter.iter().any(|item| item == &rel)
            {
                return None;
            }
            Some((rel, sf))
        })
        .collect();
    source_files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut items = Vec::new();
    for (rel_file, sf) in source_files {
        let byte_range = match range {
            Some(filter) => Some(resolve_byte_range(sf, filter)?),
            None => None,
        };
        collect_matching_nodes(
            snapshot,
            project_id,
            sf,
            &rel_file,
            sf.as_node(),
            &target_kinds,
            byte_range,
            &text_filter,
            &callee_filter,
            &mut items,
        );
    }
    Ok(items)
}

fn syntax_needles(values: Option<&[String]>) -> Vec<&str> {
    values
        .unwrap_or(&[])
        .iter()
        .map(String::as_str)
        .filter(|needle| !needle.is_empty())
        .collect()
}

fn call_callee_name(node: P<Node>) -> Option<String> {
    if !matches!(node.kind(), Kind::CallExpression | Kind::NewExpression) {
        return None;
    }
    let expr = unwrap_expr_wrappers(node.expression()?);
    match expr.kind() {
        Kind::Identifier => node_text(expr),
        Kind::PropertyAccessExpression => node_text(expr.as_property_access_expression().name()),
        Kind::ElementAccessExpression => {
            let arg =
                unwrap_expr_wrappers(expr.as_element_access_expression().argument_expression());
            matches!(
                arg.kind(),
                Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
            )
            .then(|| prop_key_text(arg))
            .flatten()
        }
        _ => None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "AST range collector passes immutable filter context alongside mutable output."
)]
fn collect_matching_nodes(
    snapshot: &mut SnapshotState,
    project_id: &str,
    sf: P<SourceFile>,
    rel_file: &str,
    node: P<Node>,
    target_kinds: &[(Kind, &str)],
    range: Option<ResolvedByteRange>,
    text_filter: &[&str],
    callee_filter: &[&str],
    out: &mut Vec<SyntaxItem>,
) {
    let full_start = node.pos().source_offset().unwrap_or(0);
    let full_end = node.end().source_offset().unwrap_or(full_start);
    if let Some(r) = range
        && (full_end < r.start || full_start > r.end)
    {
        return;
    }
    if !text_filter.is_empty() || !callee_filter.is_empty() {
        let start_idx = usize::try_from(full_start).unwrap_or(0);
        let end_idx = usize::try_from(full_end).unwrap_or(start_idx);
        if let Some(sub) = sf.text().get(start_idx..end_idx) {
            if !text_filter.is_empty() && !text_filter.iter().any(|needle| sub.contains(needle)) {
                return;
            }
            if !callee_filter.is_empty() && !callee_filter.iter().any(|name| sub.contains(name)) {
                return;
            }
        }
    }

    let kind_label_opt = if target_kinds.is_empty() {
        (node.kind() != Kind::SourceFile).then(|| format!("{:?}", node.kind()))
    } else {
        target_kinds
            .iter()
            .find(|(k, _)| *k == node.kind())
            .map(|(_, label)| (*label).to_string())
    };

    if let Some(kind_label) = kind_label_opt {
        let matches_text = text_filter.is_empty() || {
            let tok_start = get_token_pos_of_node(node, sf, false);
            let tok_end = full_end.max(tok_start);
            let s_idx = usize::try_from(tok_start).unwrap_or(0);
            let e_idx = usize::try_from(tok_end).unwrap_or(s_idx);
            sf.text()
                .get(s_idx..e_idx)
                .is_some_and(|slice| text_filter.iter().any(|needle| slice.contains(needle)))
        };
        let matches_callee = callee_filter.is_empty()
            || call_callee_name(node).is_some_and(|name| callee_filter.contains(&name.as_str()));
        if matches_text && matches_callee {
            let location = node_location(sf, rel_file, node);
            let matches_range = match range {
                None => true,
                Some(r) => match r.mode {
                    RangeMatchMode::Within => location.start >= r.start && location.end <= r.end,
                    RangeMatchMode::Intersects => location.end > r.start && location.start < r.end,
                },
            };
            if matches_range {
                out.push(build_syntax_item(
                    snapshot, project_id, sf, node, kind_label, location,
                ));
            }
        }
    }

    node.for_each_child(&mut |child| {
        collect_matching_nodes(
            snapshot,
            project_id,
            sf,
            rel_file,
            child,
            target_kinds,
            range,
            text_filter,
            callee_filter,
            out,
        );
        false
    });
}

pub fn safe_expression(node: P<Node>) -> Option<P<Node>> {
    match node.kind() {
        Kind::PropertyAccessExpression
        | Kind::ElementAccessExpression
        | Kind::ParenthesizedExpression
        | Kind::CallExpression
        | Kind::NewExpression
        | Kind::ExpressionWithTypeArguments
        | Kind::ComputedPropertyName
        | Kind::NonNullExpression
        | Kind::TypeAssertionExpression
        | Kind::AsExpression
        | Kind::SatisfiesExpression
        | Kind::TypeOfExpression
        | Kind::SpreadAssignment
        | Kind::SpreadElement
        | Kind::TemplateSpan
        | Kind::DeleteExpression
        | Kind::VoidExpression
        | Kind::AwaitExpression
        | Kind::YieldExpression
        | Kind::IfStatement
        | Kind::DoStatement
        | Kind::WhileStatement
        | Kind::WithStatement
        | Kind::ForInStatement
        | Kind::ForOfStatement
        | Kind::SwitchStatement
        | Kind::CaseClause
        | Kind::ExpressionStatement
        | Kind::ReturnStatement
        | Kind::ThrowStatement
        | Kind::ExportAssignment
        | Kind::Decorator
        | Kind::JsxExpression
        | Kind::JsxSpreadAttribute => node.expression(),
        _ => None,
    }
}

pub fn safe_initializer(node: P<Node>) -> Option<P<Node>> {
    match node.kind() {
        Kind::VariableDeclaration
        | Kind::Parameter
        | Kind::BindingElement
        | Kind::PropertyDeclaration
        | Kind::PropertySignature
        | Kind::PropertyAssignment
        | Kind::EnumMember
        | Kind::ForStatement
        | Kind::ForInStatement
        | Kind::ForOfStatement
        | Kind::JsxAttribute => node.initializer(),
        _ => None,
    }
}

fn safe_arguments(node: P<Node>) -> Option<&'static [P<Node>]> {
    matches!(node.kind(), Kind::CallExpression | Kind::NewExpression).then(|| node.arguments())
}

fn safe_parameters(node: P<Node>) -> Option<&'static [P<Node>]> {
    (is_function_like_declaration(Some(node)) || node.function_like_data().is_some())
        .then(|| node.parameters())
}

fn build_syntax_item(
    snapshot: &mut SnapshotState,
    project_id: &str,
    sf: P<SourceFile>,
    node: P<Node>,
    kind_label: String,
    location: SourceLocation,
) -> SyntaxItem {
    let text = slice_source_text(sf, location.start, location.end);
    let node_ref = snapshot.alloc_syntax(project_id, node);

    let expression = safe_expression(node).map(|expr| snapshot.alloc_syntax(project_id, expr));

    let arguments = safe_arguments(node).map(|args| {
        args.iter()
            .map(|&arg| snapshot.alloc_syntax(project_id, arg))
            .collect()
    });

    let name = node
        .name()
        .map(|name_node| snapshot.alloc_syntax(project_id, name_node));

    let initializer =
        safe_initializer(node).map(|init_node| snapshot.alloc_syntax(project_id, init_node));

    let body = node
        .body()
        .map(|body_node| snapshot.alloc_syntax(project_id, body_node));

    let parameters = safe_parameters(node).map(|params| {
        params
            .iter()
            .map(|&param| snapshot.alloc_syntax(project_id, param))
            .collect()
    });

    SyntaxItem {
        node_ref,
        kind: kind_label,
        text,
        location,
        expression,
        arguments,
        name,
        initializer,
        body,
        parameters,
    }
}

fn is_error_or_unresolved_type(ty: P<Type>) -> bool {
    if ty.data_tag() == TypeDataTag::Intrinsic {
        let name = ty.as_intrinsic_type().intrinsic_name();
        return matches!(name, "error" | "unresolved");
    }
    ty.flags().intersects(TypeFlags::Any) && ty.alias().is_some()
}

struct ComputedTypeSummary {
    ty: P<Type>,
    type_text: String,
    is_union: bool,
    is_intersection: bool,
    is_literal: bool,
    literal_value: Option<Value>,
    union_members: Option<Vec<String>>,
}

fn summarize_type(
    checker: &mut CheckerHandle,
    ty: P<Type>,
    enclosing: Option<P<Node>>,
) -> ComputedTypeSummary {
    let type_text = checker.type_to_string_ex_exported(ty, enclosing, TypeFormatFlags::None, None);
    let is_union = ty.is_union();
    let is_intersection = ty.is_intersection();
    let is_boolean_literal = ty.flags().contains(TypeFlags::BooleanLiteral);
    let is_literal = ty.is_string_literal() || ty.is_number_literal() || is_boolean_literal;

    let literal_value = if ty.is_string_literal() || ty.is_number_literal() {
        ty.as_literal_type().value().and_then(|val| match val {
            LiteralValue::String(s) => Some(Value::String(s.to_string())),
            LiteralValue::Number(num) => serde_json::Number::from_f64(num.0).map(Value::Number),
            LiteralValue::Boolean(b) => Some(Value::Bool(b)),
            LiteralValue::BigInt(_) => None,
        })
    } else if is_boolean_literal {
        match type_text.as_str() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        }
    } else {
        None
    };

    let union_members = if is_union {
        Some(
            ty.types()
                .iter()
                .map(|&member| {
                    checker.type_to_string_ex_exported(
                        member,
                        enclosing,
                        TypeFormatFlags::None,
                        None,
                    )
                })
                .collect(),
        )
    } else {
        None
    };

    ComputedTypeSummary {
        ty,
        type_text,
        is_union,
        is_intersection,
        is_literal,
        literal_value,
        union_members,
    }
}

fn alloc_type_summary(
    snapshot: &mut SnapshotState,
    project_id: &str,
    summary: ComputedTypeSummary,
    enclosing: Option<P<Node>>,
    gap_at: Option<&SourceLocation>,
    gaps: &mut Vec<GapItem>,
) -> TypeSummaryPayload {
    if is_error_or_unresolved_type(summary.ty) {
        gaps.push(GapItem {
            kind: "unresolved-type",
            message: format!(
                "Type resolved to an error or unresolved type ({}).",
                summary.type_text
            ),
            at: gap_at.cloned(),
        });
    }
    let type_ref = snapshot.alloc_type(project_id, summary.ty, enclosing);
    TypeSummaryPayload {
        type_ref,
        type_text: summary.type_text,
        is_union: summary.is_union,
        is_intersection: summary.is_intersection,
        is_literal: summary.is_literal,
        literal_value: summary.literal_value,
        union_members: summary.union_members,
    }
}

fn run_type_at_location(
    snapshot: &mut SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    targets: &[String],
) -> Result<Vec<TypeItem>, RpcErr> {
    let mut nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_syntax_node(project_id, handle_id)?;
        nodes.push((handle_id.clone(), node));
    }

    let mut items = Vec::with_capacity(nodes.len());
    for (target_id, node) in nodes {
        let ty = checker.get_type_at_location(node);
        let enclosing = node.parent().or(Some(node));
        let summary = summarize_type(checker, ty, enclosing);
        let mut gaps = Vec::new();
        let node_loc = node_location_relative(&snapshot.cwd, node);
        let payload = alloc_type_summary(
            snapshot,
            project_id,
            summary,
            enclosing,
            node_loc.as_ref(),
            &mut gaps,
        );
        items.push(TypeItem {
            target: target_id,
            type_ref: payload.type_ref,
            type_text: payload.type_text,
            is_union: payload.is_union,
            is_intersection: payload.is_intersection,
            is_literal: payload.is_literal,
            literal_value: payload.literal_value,
            union_members: payload.union_members,
            gaps,
        });
    }

    Ok(items)
}

fn unwrap_aliased_symbol(checker: &mut CheckerHandle, mut sym: P<Symbol>) -> P<Symbol> {
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

struct ResolvedDeclarationTarget {
    primary_decl: Option<P<Node>>,
    all_decls: Vec<P<Node>>,
    symbol: Option<P<Symbol>>,
}

fn is_unresolved_signature(checker: &mut CheckerHandle, sig: P<Signature>) -> bool {
    if sig == checker.get_unknown_signature() {
        return true;
    }
    if sig.declaration().is_none()
        && sig.target().is_none()
        && sig.parameters().is_empty()
        && !sig.has_rest_parameter()
    {
        let ret = checker.get_return_type_of_signature_exported(sig);
        return ret == checker.get_any_type() || is_error_or_unresolved_type(ret);
    }
    false
}

fn resolve_symbol_for_node(
    checker: &mut CheckerHandle,
    node: P<Node>,
) -> ResolvedDeclarationTarget {
    if matches!(node.kind(), Kind::CallExpression | Kind::NewExpression) {
        let sig = checker.get_resolved_signature_exported(node);
        let sig_decl = if is_unresolved_signature(checker, sig) {
            None
        } else {
            sig.declaration()
                .or_else(|| sig.target().and_then(|t| t.declaration()))
        };

        let callee_sym = node
            .expression()
            .and_then(|expr| checker.get_symbol_at_location_exported(expr))
            .map(|s| unwrap_aliased_symbol(checker, s))
            .filter(|&s| s != checker.get_unknown_symbol());

        let all_decls = callee_sym.map_or_default(|s| s.declarations().to_vec());
        let primary_decl = sig_decl.or_else(|| {
            callee_sym.and_then(|s| {
                s.value_declaration()
                    .or_else(|| s.declarations().first().copied())
            })
        });

        return ResolvedDeclarationTarget {
            primary_decl,
            all_decls,
            symbol: callee_sym,
        };
    }

    let raw_sym = if node.kind() == Kind::ShorthandPropertyAssignment {
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
        checker.get_symbol_at_location_exported(node).or_else(|| {
            node.name()
                .and_then(|name_node| checker.get_symbol_at_location_exported(name_node))
        })
    };

    let sym = raw_sym
        .map(|s| unwrap_aliased_symbol(checker, s))
        .filter(|&s| s != checker.get_unknown_symbol());

    if let Some(resolved_sym) = sym {
        let all_decls = resolved_sym.declarations().to_vec();
        let primary_decl = resolved_sym
            .value_declaration()
            .or_else(|| all_decls.first().copied());
        return ResolvedDeclarationTarget {
            primary_decl,
            all_decls,
            symbol: Some(resolved_sym),
        };
    }

    if is_declaration_node(node) {
        return ResolvedDeclarationTarget {
            primary_decl: Some(node),
            all_decls: vec![node],
            symbol: node.symbol(),
        };
    }

    ResolvedDeclarationTarget {
        primary_decl: None,
        all_decls: Vec::new(),
        symbol: None,
    }
}

fn is_declaration_node(node: P<Node>) -> bool {
    matches!(
        node.kind(),
        Kind::FunctionDeclaration
            | Kind::VariableDeclaration
            | Kind::Parameter
            | Kind::MethodDeclaration
            | Kind::MethodSignature
            | Kind::ClassDeclaration
            | Kind::InterfaceDeclaration
            | Kind::TypeAliasDeclaration
            | Kind::EnumDeclaration
            | Kind::PropertyDeclaration
            | Kind::PropertySignature
            | Kind::PropertyAssignment
            | Kind::ShorthandPropertyAssignment
            | Kind::GetAccessor
            | Kind::SetAccessor
            | Kind::Constructor
    )
}

#[derive(Clone, Copy)]
enum TargetInput {
    Node(P<Node>),
    Signature(P<Signature>, Option<P<Node>>),
    Declaration(P<Node>, Option<P<Symbol>>),
}

fn run_resolve_declarations(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
    select: &DeclarationSelect,
) -> Result<Vec<DeclarationRow>, RpcErr> {
    let mut resolved_inputs = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let entry = snapshot.get_entry(project_id, handle_id)?;
        let target_payload = match entry.payload {
            HandlePayload::Syntax(node) | HandlePayload::Call(node) => TargetInput::Node(node),
            HandlePayload::Signature { sig, call_node } => TargetInput::Signature(sig, call_node),
            HandlePayload::Declaration { node, symbol } => TargetInput::Declaration(node, symbol),
            HandlePayload::Type { .. } => {
                return Err(RpcErr::new(
                    "INVALID_REFERENCE_KIND",
                    format!(
                        "Program reference {handle_id} is a type reference and cannot resolve a declaration."
                    ),
                ));
            }
            HandlePayload::Branch(_) | HandlePayload::Instance(_) => {
                return Err(RpcErr::new(
                    "INVALID_REFERENCE_KIND",
                    format!("Program reference {handle_id} cannot resolve a declaration."),
                ));
            }
        };
        resolved_inputs.push((handle_id.clone(), target_payload));
    }

    let mut items = Vec::with_capacity(resolved_inputs.len());
    for (target_id, target_input) in resolved_inputs {
        items.push(evaluate_declaration_row(
            snapshot,
            project_id,
            program,
            checker,
            target_id,
            target_input,
            select,
        ));
    }
    Ok(items)
}

fn resolve_target_input(
    cwd: &str,
    checker: &mut CheckerHandle,
    target_input: TargetInput,
) -> (ResolvedDeclarationTarget, Option<SourceLocation>) {
    match target_input {
        TargetInput::Node(node) => {
            let loc = node_location_relative(cwd, node);
            (resolve_symbol_for_node(checker, node), loc)
        }
        TargetInput::Signature(sig, call_node) => {
            let loc = call_node.and_then(|n| node_location_relative(cwd, n));
            let decl = if is_unresolved_signature(checker, sig) {
                None
            } else {
                sig.declaration()
                    .or_else(|| sig.target().and_then(|t| t.declaration()))
            };
            let all_decls = decl.into_iter().collect();
            let sym = decl.and_then(|d| {
                d.symbol().or_else(|| {
                    d.name()
                        .and_then(|n| checker.get_symbol_at_location_exported(n))
                })
            });
            (
                ResolvedDeclarationTarget {
                    primary_decl: decl,
                    all_decls,
                    symbol: sym,
                },
                loc,
            )
        }
        TargetInput::Declaration(node, symbol) => {
            let loc = node_location_relative(cwd, node);
            let all_decls = symbol
                .map(|s| s.declarations().to_vec())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| vec![node]);
            (
                ResolvedDeclarationTarget {
                    primary_decl: Some(node),
                    all_decls,
                    symbol,
                },
                loc,
            )
        }
    }
}

fn resolve_declaration_container_name(decl: P<Node>) -> Option<String> {
    let mut current = decl.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            Kind::ClassDeclaration
                | Kind::ClassExpression
                | Kind::InterfaceDeclaration
                | Kind::ModuleDeclaration
                | Kind::TypeAliasDeclaration
                | Kind::EnumDeclaration
                | Kind::VariableDeclaration
        ) && let Some(name_node) = parent.name()
            && let Some(source_file) = get_source_file_of_node(name_node)
        {
            let location = node_location(source_file, "", name_node);
            let name = slice_source_text(source_file, location.start, location.end);
            if !name.is_empty() {
                return Some(name);
            }
        }
        current = parent.parent();
    }
    None
}

fn evaluate_declaration_row(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    target_id: String,
    target_input: TargetInput,
    select: &DeclarationSelect,
) -> DeclarationRow {
    let (resolved, target_loc) = resolve_target_input(&snapshot.cwd, checker, target_input);

    let mut gaps = Vec::new();
    let declaration = if let Some(decl_node) = resolved.primary_decl {
        Some(snapshot.alloc_declaration(project_id, decl_node, resolved.symbol))
    } else {
        gaps.push(GapItem {
            kind: "unresolved-declaration",
            message: "Could not resolve target to a declaration.".to_string(),
            at: target_loc,
        });
        None
    };

    let name = ProjectedField::from_selected(select.name, || {
        resolved.symbol.map(|s| s.name().to_string()).or_else(|| {
            resolved.primary_decl.and_then(|decl| {
                decl.name().and_then(|name_node| {
                    get_source_file_of_node(name_node).map(|sf| {
                        let loc = node_location(sf, "", name_node);
                        slice_source_text(sf, loc.start, loc.end)
                    })
                })
            })
        })
    });

    let container_name = ProjectedField::from_selected(select.container_name, || {
        resolved
            .primary_decl
            .and_then(resolve_declaration_container_name)
    });

    let kind = ProjectedField::from_selected(select.kind, || {
        resolved
            .primary_decl
            .map(|decl| format!("{:?}", decl.kind()))
    });

    let location = ProjectedField::from_selected(select.location, || {
        resolved
            .primary_decl
            .and_then(|decl| node_location_relative(&snapshot.cwd, decl))
    });

    let syntax = ProjectedField::from_selected(select.syntax, || {
        resolved
            .primary_decl
            .map(|decl| snapshot.alloc_syntax(project_id, decl))
    });

    let is_external = select.is_external.then(|| {
        resolved.primary_decl.is_some_and(|decl| {
            get_source_file_of_node(decl).is_some_and(|sf| !is_project_source_file(program, sf))
        })
    });

    let declarations = select.declarations.then(|| {
        resolved
            .all_decls
            .iter()
            .map(|&decl| snapshot.alloc_declaration(project_id, decl, resolved.symbol))
            .collect()
    });

    DeclarationRow {
        target: target_id,
        declaration,
        gaps,
        name,
        container_name,
        kind,
        location,
        syntax,
        is_external,
        declarations,
    }
}

struct ProjectedCallFields {
    gaps: Vec<GapItem>,
    location: Option<SourceLocation>,
    syntax: Option<String>,
    callee: ProjectedField<String>,
    callee_declaration: ProjectedField<String>,
    signature: ProjectedField<String>,
    arguments: Option<Vec<String>>,
    instantiated_parameter_types: Option<Vec<InstantiatedParameterPayload>>,
    return_type: ProjectedField<TypeSummaryPayload>,
}

fn evaluate_call_fields(
    snapshot: &mut SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    node: P<Node>,
    select: &CallSelect,
) -> ProjectedCallFields {
    let call_loc = node_location_relative(&snapshot.cwd, node);
    let mut gaps = Vec::new();

    let location = if select.location {
        call_loc.clone()
    } else {
        None
    };

    let syntax = select
        .syntax
        .then(|| snapshot.alloc_syntax(project_id, node));

    let callee = ProjectedField::from_selected(select.callee, || {
        node.expression()
            .map(|expr| snapshot.alloc_syntax(project_id, expr))
    });

    let callee_declaration = ProjectedField::from_selected(select.callee_declaration, || {
        let resolved = resolve_symbol_for_node(checker, node);
        if let Some(decl) = resolved.primary_decl {
            Some(snapshot.alloc_declaration(project_id, decl, resolved.symbol))
        } else {
            gaps.push(GapItem {
                kind: "unresolved-declaration",
                message: "Could not resolve callee declaration for call.".to_string(),
                at: call_loc.clone(),
            });
            None
        }
    });

    let needs_sig =
        select.signature || select.instantiated_parameter_types.is_some() || select.return_type;
    let resolved_sig = if needs_sig {
        let sig = checker.get_resolved_signature_exported(node);
        if is_unresolved_signature(checker, sig) {
            gaps.push(GapItem {
                kind: "unresolved-signature",
                message: "Call signature could not be resolved.".to_string(),
                at: call_loc.clone(),
            });
            None
        } else {
            Some(sig)
        }
    } else {
        None
    };

    let signature = ProjectedField::from_selected(select.signature, || {
        resolved_sig.map(|sig| snapshot.alloc_signature(project_id, sig, Some(node)))
    });

    let arguments = select.arguments.then(|| {
        node.arguments()
            .iter()
            .map(|&arg| snapshot.alloc_syntax(project_id, arg))
            .collect()
    });

    let instantiated_parameter_types =
        select
            .instantiated_parameter_types
            .as_ref()
            .map(|param_sel| {
                let Some(sig) = resolved_sig else {
                    return Vec::new();
                };
                collect_instantiated_param_types(
                    snapshot,
                    project_id,
                    checker,
                    sig,
                    Some(node),
                    param_sel,
                    call_loc.as_ref(),
                    &mut gaps,
                )
            });

    let return_type = ProjectedField::from_selected(select.return_type, || {
        resolved_sig.map(|sig| {
            let ret_ty = checker.get_return_type_of_signature_exported(sig);
            let summary = summarize_type(checker, ret_ty, Some(node));
            alloc_type_summary(
                snapshot,
                project_id,
                summary,
                Some(node),
                call_loc.as_ref(),
                &mut gaps,
            )
        })
    });

    ProjectedCallFields {
        gaps,
        location,
        syntax,
        callee,
        callee_declaration,
        signature,
        arguments,
        instantiated_parameter_types,
        return_type,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Parameter type extraction needs checker, snapshot, signature, and gap output."
)]
fn collect_instantiated_param_types(
    snapshot: &mut SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    sig: P<Signature>,
    enclosing: Option<P<Node>>,
    param_sel: &ParameterIndexSelection,
    gap_at: Option<&SourceLocation>,
    gaps: &mut Vec<GapItem>,
) -> Vec<InstantiatedParameterPayload> {
    let params = sig.parameters();
    let Some(indices) = param_sel.requested_indices(params.len()) else {
        return Vec::new();
    };
    let has_rest = sig.has_rest_parameter();
    let mut out = Vec::with_capacity(indices.len());

    for idx in indices {
        if idx >= params.len() && !has_rest {
            gaps.push(GapItem {
                kind: "parameter-out-of-bounds",
                message: format!(
                    "Requested parameter index {idx}, but signature has {} parameter(s).",
                    params.len()
                ),
                at: gap_at.cloned(),
            });
            continue;
        }
        let param_symbol = params.get(idx).copied().or_else(|| {
            if has_rest {
                params.last().copied()
            } else {
                None
            }
        });
        let param_name = param_symbol.map(|s| s.name().to_string());
        let is_rest = has_rest && idx + 1 >= params.len();
        let param_ty = checker.get_type_at_position_exported(sig, idx);
        let summary = summarize_type(checker, param_ty, enclosing);
        let type_summary =
            alloc_type_summary(snapshot, project_id, summary, enclosing, gap_at, gaps);
        out.push(InstantiatedParameterPayload {
            index: idx,
            parameter_name: param_name,
            is_rest,
            type_summary,
        });
    }

    out
}

fn run_resolve_calls(
    snapshot: &mut SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    targets: &[String],
    select: &CallSelect,
) -> Result<Vec<CallResolutionRow>, RpcErr> {
    let mut nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_syntax_node(project_id, handle_id)?;
        nodes.push((handle_id.clone(), node));
    }

    let mut items = Vec::with_capacity(nodes.len());
    for (target_id, node) in nodes {
        if !matches!(node.kind(), Kind::CallExpression | Kind::NewExpression) {
            let loc = node_location_relative(&snapshot.cwd, node);
            let gaps = vec![GapItem {
                kind: "not-a-call",
                message: format!(
                    "Syntax node of kind {:?} is not a call or new expression.",
                    node.kind()
                ),
                at: loc.clone(),
            }];
            let syntax = select
                .syntax
                .then(|| snapshot.alloc_syntax(project_id, node));
            let location = if select.location { loc } else { None };
            let instantiated_parameter_types = select
                .instantiated_parameter_types
                .as_ref()
                .and_then(|s| s.requested_indices(0))
                .map(|_| Vec::new());

            items.push(CallResolutionRow {
                target: target_id,
                call: None,
                gaps,
                location,
                syntax,
                callee: ProjectedField::null_if_selected(select.callee),
                callee_declaration: ProjectedField::null_if_selected(select.callee_declaration),
                signature: ProjectedField::null_if_selected(select.signature),
                arguments: select.arguments.then(Vec::new),
                instantiated_parameter_types,
                return_type: ProjectedField::null_if_selected(select.return_type),
            });
            continue;
        }

        let call_ref = snapshot.alloc_call(project_id, node);
        let fields = evaluate_call_fields(snapshot, project_id, checker, node, select);
        items.push(CallResolutionRow {
            target: target_id,
            call: Some(call_ref),
            gaps: fields.gaps,
            location: fields.location,
            syntax: fields.syntax,
            callee: fields.callee,
            callee_declaration: fields.callee_declaration,
            signature: fields.signature,
            arguments: fields.arguments,
            instantiated_parameter_types: fields.instantiated_parameter_types,
            return_type: fields.return_type,
        });
    }

    Ok(items)
}

fn run_project_calls(
    snapshot: &mut SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    targets: &[String],
    select: &CallSelect,
) -> Result<Vec<CallProjectionRow>, RpcErr> {
    let mut call_nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_call_node(project_id, handle_id)?;
        call_nodes.push((handle_id.clone(), node));
    }

    let mut items = Vec::with_capacity(call_nodes.len());
    for (call_id, node) in call_nodes {
        let fields = evaluate_call_fields(snapshot, project_id, checker, node, select);
        items.push(CallProjectionRow {
            call: call_id,
            gaps: fields.gaps,
            location: fields.location,
            syntax: fields.syntax,
            callee: fields.callee,
            callee_declaration: fields.callee_declaration,
            signature: fields.signature,
            arguments: fields.arguments,
            instantiated_parameter_types: fields.instantiated_parameter_types,
            return_type: fields.return_type,
        });
    }

    Ok(items)
}

fn run_project_signatures(
    snapshot: &mut SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    targets: &[String],
    select: &SignatureSelect,
) -> Result<Vec<SignatureProjectionRow>, RpcErr> {
    let mut sig_entries = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let (sig, call_node) = snapshot.get_signature(project_id, handle_id)?;
        sig_entries.push((handle_id.clone(), sig, call_node));
    }

    let mut items = Vec::with_capacity(sig_entries.len());
    for (sig_id, sig, call_node) in sig_entries {
        items.push(evaluate_signature_row(
            snapshot, project_id, checker, sig_id, sig, call_node, select,
        ));
    }
    Ok(items)
}

fn evaluate_signature_row(
    snapshot: &mut SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    sig_id: String,
    sig: P<Signature>,
    call_node: Option<P<Node>>,
    select: &SignatureSelect,
) -> SignatureProjectionRow {
    let mut gaps = Vec::new();
    let decl_node = sig
        .declaration()
        .or_else(|| sig.target().and_then(|t| t.declaration()));
    let enclosing = call_node.or(decl_node);
    let gap_at = enclosing.and_then(|n| node_location_relative(&snapshot.cwd, n));

    let signature_text = select
        .signature_text
        .then(|| checker.signature_to_string_ex(sig, enclosing, TypeFormatFlags::None, None));

    let declaration = ProjectedField::from_selected(select.declaration, || {
        decl_node.map(|decl| {
            let sym = decl.symbol().or_else(|| {
                decl.name()
                    .and_then(|n| checker.get_symbol_at_location_exported(n))
            });
            snapshot.alloc_declaration(project_id, decl, sym)
        })
    });

    let location = ProjectedField::from_selected(select.location, || {
        decl_node.and_then(|decl| node_location_relative(&snapshot.cwd, decl))
    });

    let syntax = ProjectedField::from_selected(select.syntax, || {
        decl_node.map(|decl| snapshot.alloc_syntax(project_id, decl))
    });

    let parameters = select
        .parameters
        .then(|| build_signature_parameters(snapshot, project_id, sig));

    let min_argument_count = select.min_argument_count.then(|| sig.min_argument_count());
    let has_rest_parameter = select.has_rest_parameter.then(|| sig.has_rest_parameter());

    let instantiated_parameter_types = select.instantiated_parameter_types.as_ref().map(|sel| {
        collect_instantiated_param_types(
            snapshot,
            project_id,
            checker,
            sig,
            enclosing,
            sel,
            gap_at.as_ref(),
            &mut gaps,
        )
    });

    let return_type = select.return_type.then(|| {
        let ret_ty = checker.get_return_type_of_signature_exported(sig);
        let summary = summarize_type(checker, ret_ty, enclosing);
        alloc_type_summary(
            snapshot,
            project_id,
            summary,
            enclosing,
            gap_at.as_ref(),
            &mut gaps,
        )
    });

    let is_instantiated = select.is_instantiated.then(|| sig.target().is_some());

    SignatureProjectionRow {
        signature: sig_id,
        gaps,
        signature_text,
        declaration,
        location,
        syntax,
        parameters,
        min_argument_count,
        has_rest_parameter,
        instantiated_parameter_types,
        return_type,
        is_instantiated,
    }
}

fn build_signature_parameters(
    snapshot: &mut SnapshotState,
    project_id: &str,
    sig: P<Signature>,
) -> Vec<SignatureParameterPayload> {
    let params = sig.parameters();
    let has_rest = sig.has_rest_parameter();
    let mut param_items = Vec::with_capacity(params.len());
    for (idx, &param_sym) in params.iter().enumerate() {
        let param_decl = param_sym
            .value_declaration()
            .or_else(|| param_sym.declarations().first().copied());
        let is_rest = has_rest && idx + 1 == params.len();
        let is_optional = param_sym.flags().intersects(SymbolFlags::Optional)
            || param_decl.is_some_and(|d| {
                d.kind() == Kind::Parameter
                    && (d.as_parameter_declaration().question_token().is_some()
                        || d.initializer().is_some())
            });
        let decl_ref =
            param_decl.map(|d| snapshot.alloc_declaration(project_id, d, Some(param_sym)));
        param_items.push(SignatureParameterPayload {
            name: param_sym.name().to_string(),
            declaration: decl_ref,
            optional: is_optional,
            rest: is_rest,
        });
    }
    param_items
}

fn run_project_types(
    snapshot: &mut SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    targets: &[String],
    select: &TypeSelect,
) -> Result<Vec<TypeProjectionRow>, RpcErr> {
    let mut type_entries = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let (ty, enclosing) = snapshot.get_type_entry(project_id, handle_id)?;
        type_entries.push((handle_id.clone(), ty, enclosing));
    }

    let mut items = Vec::with_capacity(type_entries.len());
    for (type_id, ty, enclosing) in type_entries {
        let summary = summarize_type(checker, ty, enclosing);
        let mut gaps = Vec::new();
        if is_error_or_unresolved_type(summary.ty) {
            gaps.push(GapItem {
                kind: "unresolved-type",
                message: format!(
                    "Type resolved to an error or unresolved type ({}).",
                    summary.type_text
                ),
                at: enclosing.and_then(|n| node_location_relative(&snapshot.cwd, n)),
            });
        }

        let union_member_types = select.union_member_types.then(|| {
            if ty.is_union() {
                ty.types()
                    .iter()
                    .map(|&member| snapshot.alloc_type(project_id, member, enclosing))
                    .collect()
            } else {
                Vec::new()
            }
        });

        let properties = select.properties.then(|| {
            checker
                .get_properties_of_type_exported(ty)
                .iter()
                .map(|&prop_sym| {
                    let decl = prop_sym
                        .value_declaration()
                        .or_else(|| prop_sym.declarations().first().copied())
                        .map(|d| snapshot.alloc_declaration(project_id, d, Some(prop_sym)));
                    TypePropertyPayload {
                        name: prop_sym.name().to_string(),
                        optional: prop_sym.flags().intersects(SymbolFlags::Optional),
                        declaration: decl,
                    }
                })
                .collect()
        });

        let type_arguments = select.type_arguments.then(|| {
            let args = if ty.try_as_type_reference().is_some() {
                checker.get_type_arguments_exported(ty)
            } else {
                &[]
            };
            args.iter()
                .map(|&arg_ty| snapshot.alloc_type(project_id, arg_ty, enclosing))
                .collect()
        });

        items.push(TypeProjectionRow {
            type_ref: type_id,
            gaps,
            type_text: select.type_text.then_some(summary.type_text),
            is_union: select.is_union.then_some(summary.is_union),
            is_intersection: select.is_intersection.then_some(summary.is_intersection),
            is_literal: select.is_literal.then_some(summary.is_literal),
            literal_value: ProjectedField::from_selected(select.literal_value, || {
                summary.literal_value
            }),
            union_members: select
                .union_members
                .then(|| summary.union_members.unwrap_or_default()),
            union_member_types,
            properties,
            type_arguments,
        });
    }

    Ok(items)
}

fn run_is_assignable_to(
    snapshot: &SnapshotState,
    project_id: &str,
    checker: &mut CheckerHandle,
    pairs: &[AssignabilityPairInput],
) -> Result<Vec<AssignabilityRow>, RpcErr> {
    let mut resolved = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let (source_ty, source_enclosing) = snapshot.get_type_entry(project_id, &pair.source)?;
        let (target_ty, target_enclosing) = snapshot.get_type_entry(project_id, &pair.target)?;
        resolved.push((
            pair.source.clone(),
            pair.target.clone(),
            source_ty,
            target_ty,
            source_enclosing.or(target_enclosing),
        ));
    }

    let mut items = Vec::with_capacity(resolved.len());
    for (source_id, target_id, source_ty, target_ty, enclosing) in resolved {
        let mut gaps = Vec::new();
        if is_error_or_unresolved_type(source_ty) || is_error_or_unresolved_type(target_ty) {
            gaps.push(GapItem {
                kind: "unresolved-type",
                message: "Assignability check involves an error or unresolved type.".to_string(),
                at: enclosing.and_then(|n| node_location_relative(&snapshot.cwd, n)),
            });
        }
        let assignable = checker.is_type_assignable_to_exported(source_ty, target_ty);
        items.push(AssignabilityRow {
            source: source_id,
            target: target_id,
            assignable,
            gaps,
        });
    }

    Ok(items)
}

fn run_syntax_of(
    snapshot: &mut SnapshotState,
    project_id: &str,
    targets: &[String],
    select: &SyntaxLookupSelect,
) -> Result<Vec<SyntaxLookupRow>, RpcErr> {
    let mut resolved = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let entry = snapshot.get_entry(project_id, handle_id)?;
        let node_opt = match entry.payload {
            HandlePayload::Syntax(node)
            | HandlePayload::Call(node)
            | HandlePayload::Declaration { node, .. } => Some(node),
            HandlePayload::Signature { sig, .. } => sig
                .declaration()
                .or_else(|| sig.target().and_then(|t| t.declaration())),
            HandlePayload::Type { .. } => {
                return Err(RpcErr::new(
                    "INVALID_REFERENCE_KIND",
                    format!(
                        "Program reference {handle_id} is a type reference and has no syntax node."
                    ),
                ));
            }
            HandlePayload::Branch(_) | HandlePayload::Instance(_) => {
                return Err(RpcErr::new(
                    "INVALID_REFERENCE_KIND",
                    format!("Program reference {handle_id} has no syntax node."),
                ));
            }
        };
        resolved.push((handle_id.clone(), node_opt));
    }

    let mut items = Vec::with_capacity(resolved.len());
    for (target_id, node_opt) in resolved {
        items.push(evaluate_syntax_lookup_row(
            snapshot, project_id, target_id, node_opt, select,
        ));
    }
    Ok(items)
}

fn evaluate_syntax_lookup_row(
    snapshot: &mut SnapshotState,
    project_id: &str,
    target_id: String,
    node_opt: Option<P<Node>>,
    select: &SyntaxLookupSelect,
) -> SyntaxLookupRow {
    let Some(node) = node_opt else {
        return SyntaxLookupRow {
            target: target_id,
            syntax: None,
            gaps: Vec::new(),
            kind: ProjectedField::null_if_selected(select.kind),
            text: ProjectedField::null_if_selected(select.text),
            location: ProjectedField::null_if_selected(select.location),
            expression: ProjectedField::null_if_selected(select.expression),
            arguments: select.arguments.then(Vec::new),
            name: ProjectedField::null_if_selected(select.name),
            initializer: ProjectedField::null_if_selected(select.initializer),
            body: ProjectedField::null_if_selected(select.body),
            parameters: select.parameters.then(Vec::new),
        };
    };

    let sf_opt = get_source_file_of_node(node);
    let syntax_ref = snapshot.alloc_syntax(project_id, node);
    let loc_opt = sf_opt.map(|sf| {
        let rel_file = relative_to_cwd(&snapshot.cwd, sf.file_name());
        node_location(sf, &rel_file, node)
    });

    let kind = ProjectedField::from_selected(select.kind, || Some(format!("{:?}", node.kind())));
    let text = ProjectedField::from_selected(select.text, || {
        sf_opt
            .zip(loc_opt.as_ref())
            .map(|(sf, loc)| slice_source_text(sf, loc.start, loc.end))
    });
    let location = ProjectedField::from_selected(select.location, || loc_opt);
    let expression = ProjectedField::from_selected(select.expression, || {
        safe_expression(node).map(|e| snapshot.alloc_syntax(project_id, e))
    });
    let arguments = select.arguments.then(|| {
        safe_arguments(node).map_or_else(Vec::new, |args| {
            args.iter()
                .map(|&arg| snapshot.alloc_syntax(project_id, arg))
                .collect()
        })
    });
    let name = ProjectedField::from_selected(select.name, || {
        node.name().map(|n| snapshot.alloc_syntax(project_id, n))
    });
    let initializer = ProjectedField::from_selected(select.initializer, || {
        safe_initializer(node).map(|i| snapshot.alloc_syntax(project_id, i))
    });
    let body = ProjectedField::from_selected(select.body, || {
        node.body().map(|b| snapshot.alloc_syntax(project_id, b))
    });
    let parameters = select.parameters.then(|| {
        safe_parameters(node).map_or_else(Vec::new, |params| {
            params
                .iter()
                .map(|&p| snapshot.alloc_syntax(project_id, p))
                .collect()
        })
    });

    SyntaxLookupRow {
        target: target_id,
        syntax: Some(syntax_ref),
        gaps: Vec::new(),
        kind,
        text,
        location,
        expression,
        arguments,
        name,
        initializer,
        body,
        parameters,
    }
}

fn node_location_relative(cwd: &str, node: P<Node>) -> Option<SourceLocation> {
    let sf = get_source_file_of_node(node)?;
    let rel_file = relative_to_cwd(cwd, sf.file_name());
    Some(node_location(sf, &rel_file, node))
}

pub fn node_location(sf: P<SourceFile>, rel_file: &str, node: P<Node>) -> SourceLocation {
    let start = get_token_pos_of_node(node, sf, false);
    let end = node.end().source_offset().unwrap_or(start).max(start);

    let (start_line0, start_col0) = get_ecma_line_and_utf16_character_of_position(sf.get(), start);
    let (end_line0, end_col0) = get_ecma_line_and_utf16_character_of_position(sf.get(), end);

    SourceLocation {
        file: rel_file.to_string(),
        line: u32::try_from(start_line0).unwrap_or(u32::MAX - 1) + 1,
        column: start_col0 + 1,
        end_line: u32::try_from(end_line0).unwrap_or(u32::MAX - 1) + 1,
        end_column: end_col0 + 1,
        start,
        end,
    }
}

pub fn slice_source_text(sf: P<SourceFile>, start: u32, end: u32) -> String {
    let text = sf.text();
    let start_idx = usize::try_from(start).unwrap_or(0);
    let end_idx = usize::try_from(end).unwrap_or(start_idx);
    if start_idx <= end_idx && end_idx <= text.len() {
        text.get(start_idx..end_idx).unwrap_or("").to_string()
    } else {
        String::new()
    }
}

fn parse_syntax_kind(name: &str) -> Option<Kind> {
    match name {
        "Identifier" => Some(Kind::Identifier),
        "CallExpression" => Some(Kind::CallExpression),
        "NewExpression" => Some(Kind::NewExpression),
        "ReturnStatement" => Some(Kind::ReturnStatement),
        "VariableDeclaration" => Some(Kind::VariableDeclaration),
        "BinaryExpression" => Some(Kind::BinaryExpression),
        "PropertyAccessExpression" => Some(Kind::PropertyAccessExpression),
        "IfStatement" => Some(Kind::IfStatement),
        "FunctionDeclaration" => Some(Kind::FunctionDeclaration),
        "ArrowFunction" => Some(Kind::ArrowFunction),
        "FunctionExpression" => Some(Kind::FunctionExpression),
        "MethodDeclaration" => Some(Kind::MethodDeclaration),
        "MethodSignature" => Some(Kind::MethodSignature),
        "CallSignature" => Some(Kind::CallSignature),
        "Constructor" => Some(Kind::Constructor),
        "FunctionType" => Some(Kind::FunctionType),
        "StringLiteral" => Some(Kind::StringLiteral),
        "NumericLiteral" => Some(Kind::NumericLiteral),
        "Parameter" => Some(Kind::Parameter),
        "PropertyAssignment" => Some(Kind::PropertyAssignment),
        "ShorthandPropertyAssignment" => Some(Kind::ShorthandPropertyAssignment),
        "PropertyDeclaration" => Some(Kind::PropertyDeclaration),
        "PropertySignature" => Some(Kind::PropertySignature),
        "ClassDeclaration" => Some(Kind::ClassDeclaration),
        "InterfaceDeclaration" => Some(Kind::InterfaceDeclaration),
        "TypeAliasDeclaration" => Some(Kind::TypeAliasDeclaration),
        "ImportDeclaration" => Some(Kind::ImportDeclaration),
        "ImportSpecifier" => Some(Kind::ImportSpecifier),
        "ExportSpecifier" => Some(Kind::ExportSpecifier),
        "Block" => Some(Kind::Block),
        "TypeReference" => Some(Kind::TypeReference),
        "SwitchStatement" => Some(Kind::SwitchStatement),
        "CaseClause" => Some(Kind::CaseClause),
        "DefaultClause" => Some(Kind::DefaultClause),
        "ConditionalExpression" => Some(Kind::ConditionalExpression),
        "ThrowStatement" => Some(Kind::ThrowStatement),
        "TryStatement" => Some(Kind::TryStatement),
        "ExpressionStatement" => Some(Kind::ExpressionStatement),
        "VariableStatement" => Some(Kind::VariableStatement),
        _ => None,
    }
}
