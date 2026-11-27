use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use tssae_common::SourceLocation;
use tssae_common::{GapItem, UnknownReason};
use tssae_tsrs_boundary::{
    BoundaryCallProjection, BoundaryChecker, BoundaryDeclarationTarget, BoundaryNode,
    BoundaryProgram, BoundarySemanticFacade, BoundaryTypeSummary, FacadeCallSelect,
    FacadeDeclarationSelect, FacadeParameterIndexSelection, FacadeSignatureSelect,
    FacadeTypeSelect, FindSyntaxFilter, SourceRangeSpec, SyntaxLookupOptions, SyntaxView,
};

use crate::ir_calls::{CallTargetsRow, DeclarationCallersRow, run_callees_of, run_callers_of};
use crate::ir_effects::{
    CallEffectsRow, InstanceCleanupScopeRow, run_call_effects, run_instance_cleanup,
};
use crate::ir_impls::{ImplementationResolutionRow, run_resolve_implementations};
use crate::ir_instances::{
    InstanceProjectionRow, InstanceResolutionRow, InstanceSelect, run_project_instances,
    run_resolve_instances,
};
use crate::ir_values::{ValuePropagationResultPayload, run_value_propagation};
use crate::observe::{EngineStepDetail, ObserveCollector};
use crate::serve::{
    BranchCoverageItem, BranchCoverageRequest, FunctionBranchesItem, HandlePayload, RpcErr,
    SnapshotState, run_branch_coverage, run_list_branches,
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

    fn from_option(selected: bool, value: Option<T>) -> Self {
        if selected {
            Self::Value(value)
        } else {
            Self::Omitted
        }
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
    },
    InstanceCleanup {
        scopes: Vec<String>,
        #[serde(default)]
        targets: Vec<String>,
        #[serde(default, rename = "lookupFiles")]
        lookup_files: Option<Vec<String>>,
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
    package_name: bool,
    #[serde(default)]
    export_name: bool,
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
    fn to_facade(&self) -> FacadeParameterIndexSelection {
        match self {
            Self::All(all) => FacadeParameterIndexSelection::All(*all),
            Self::Indices(indices) => FacadeParameterIndexSelection::Indices(indices.clone()),
        }
    }

    fn requested_indices(&self, param_count: usize) -> Option<Vec<usize>> {
        self.to_facade().requested_indices(param_count)
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
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    package_name: ProjectedField<String>,
    #[serde(skip_serializing_if = "ProjectedField::is_omitted")]
    export_name: ProjectedField<String>,
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
    program: BoundaryProgram,
    steps: Vec<QueryStep>,
    observer: &mut ObserveCollector,
) -> Result<QueryBatchResult, RpcErr> {
    program.with_type_checker(|checker| {
        let mut results = Vec::with_capacity(steps.len());
        for step in steps {
            results.push(execute_step(
                snapshot, project_id, program, checker, step, observer,
            )?);
        }
        Ok(QueryBatchResult { results })
    })
}

fn execute_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
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
            snapshot, project_id, program, checker, &targets, step_start, observer,
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
        QueryStep::ListBranches { targets } => execute_list_branches_step(
            snapshot, project_id, program, checker, &targets, step_start, observer,
        ),
        QueryStep::BranchCoverage {
            function,
            scope,
            branches,
            targets,
        } => execute_branch_coverage_step(
            snapshot,
            project_id,
            program,
            checker,
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
        | QueryStep::InstanceCleanup { .. }) => execute_extended_step(
            snapshot, project_id, program, checker, other, step_start, observer,
        ),
    }
}

fn execute_extended_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    step: QueryStep,
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    match step {
        QueryStep::ResolveInstances {
            targets,
            lookup_files,
        } => {
            let items = run_resolve_instances(
                snapshot,
                project_id,
                program,
                checker,
                &targets,
                lookup_files.as_deref(),
                observer,
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
        other => execute_effects_step(
            snapshot, project_id, program, checker, other, step_start, observer,
        ),
    }
}

fn execute_effects_step(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
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
                observer,
            )?;
            observer.record_step("callEffects", step_start, items.len(), Vec::new());
            Ok(StepResult::CallEffects { items })
        }
        QueryStep::InstanceCleanup {
            scopes,
            targets,
            lookup_files,
        } => {
            let items = run_instance_cleanup(
                snapshot,
                project_id,
                program,
                checker,
                &scopes,
                &targets,
                lookup_files.as_deref(),
                observer,
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
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
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
                program,
                checker,
                &targets,
                &select.unwrap_or_default(),
            )?;
            observer.record_step("resolveCalls", step_start, items.len(), Vec::new());
            Ok(StepResult::ResolveCalls { items })
        }
        QueryStep::ProjectCalls { targets, select } => {
            let items =
                run_project_calls(snapshot, project_id, program, checker, &targets, &select)?;
            observer.record_step("projectCalls", step_start, items.len(), Vec::new());
            Ok(StepResult::ProjectCalls { items })
        }
        QueryStep::ProjectSignatures { targets, select } => {
            let items =
                run_project_signatures(snapshot, project_id, program, checker, &targets, &select)?;
            observer.record_step("projectSignatures", step_start, items.len(), Vec::new());
            Ok(StepResult::ProjectSignatures { items })
        }
        QueryStep::ProjectTypes { targets, select } => {
            let items =
                run_project_types(snapshot, project_id, program, checker, &targets, &select)?;
            observer.record_step("projectTypes", step_start, items.len(), Vec::new());
            Ok(StepResult::ProjectTypes { items })
        }
        QueryStep::IsAssignableTo { pairs } => {
            let items = run_is_assignable_to(snapshot, project_id, program, checker, &pairs)?;
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
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
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
            observer,
        )?;
        observer.record_step("callersOf", step_start, items.len(), Vec::new());
        Ok(StepResult::CallersOf { items })
    } else {
        let items = run_callees_of(snapshot, project_id, program, checker, targets, observer)?;
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
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
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
        observer,
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
    program: BoundaryProgram,
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
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    let items = run_type_at_location(snapshot, project_id, program, checker, targets)?;
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
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    let items = run_list_branches(snapshot, project_id, program, checker, targets, observer)?;
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
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    function: Option<&str>,
    scope: Option<&str>,
    branches: &[String],
    targets: &[String],
    step_start: Option<Instant>,
    observer: &mut ObserveCollector,
) -> Result<StepResult, RpcErr> {
    let items = run_branch_coverage(
        snapshot,
        checker,
        &BranchCoverageRequest {
            project_id,
            program,
            function_handle: function,
            scope_handle: scope,
            branch_handles: branches,
            target_handles: targets,
        },
        observer,
    )?;
    let raw_details = if observer.is_detail() {
        items
            .iter()
            .map(|item| EngineStepDetail {
                kind: "branchCoverage",
                file: None,
                line: None,
                column: None,
                summary: format!("{} -> {}", item.target(), item.status_str()),
            })
            .collect()
    } else {
        Vec::new()
    };
    observer.record_step("branchCoverage", step_start, items.len(), raw_details);
    Ok(StepResult::BranchCoverage { items })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Query filtering extends the existing find-syntax parameters without changing the RPC shape."
)]
fn run_find_syntax(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    files: Option<&[String]>,
    kinds: Option<&[String]>,
    range: Option<&SourceRangeFilter>,
    text_includes: Option<&[String]>,
    callee_names: Option<&[String]>,
) -> Result<Vec<SyntaxItem>, RpcErr> {
    let syntax_view = SyntaxView::new(&snapshot.cwd);
    let range_spec = range.map(|r| SourceRangeSpec {
        file: &r.file,
        start_line: r.start_line,
        start_column: r.start_column,
        end_line: r.end_line,
        end_column: r.end_column,
        start: r.start,
        end: r.end,
        match_mode: r.match_mode.as_deref(),
    });
    let matches = syntax_view
        .find_boundary_syntax(
            program,
            &FindSyntaxFilter {
                files,
                kinds,
                range: range_spec,
                text_includes,
                callee_names,
            },
        )
        .map_err(|err| RpcErr::new(err.code, err.message))?;

    let mut items = Vec::with_capacity(matches.len());
    for matched in matches {
        let node_ref = snapshot.alloc_syntax(project_id, matched.node);
        let expression = matched
            .children
            .expression
            .map(|expr| snapshot.alloc_syntax(project_id, expr));
        let arguments = matched.children.arguments.map(|args| {
            args.into_iter()
                .map(|arg| snapshot.alloc_syntax(project_id, arg))
                .collect()
        });
        let name = matched
            .children
            .name
            .map(|name_node| snapshot.alloc_syntax(project_id, name_node));
        let initializer = matched
            .children
            .initializer
            .map(|init_node| snapshot.alloc_syntax(project_id, init_node));
        let body = matched
            .children
            .body
            .map(|body_node| snapshot.alloc_syntax(project_id, body_node));
        let parameters = matched.children.parameters.map(|params| {
            params
                .into_iter()
                .map(|param| snapshot.alloc_syntax(project_id, param))
                .collect()
        });
        items.push(SyntaxItem {
            node_ref,
            kind: matched.kind,
            text: matched.text,
            location: matched.location,
            expression,
            arguments,
            name,
            initializer,
            body,
            parameters,
        });
    }
    Ok(items)
}

fn alloc_type_summary_payload(
    snapshot: &mut SnapshotState,
    project_id: &str,
    summary: BoundaryTypeSummary,
) -> TypeSummaryPayload {
    let type_ref = snapshot.alloc_type(project_id, summary.ty, summary.enclosing);
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

fn alloc_type_summary_with_gap(
    snapshot: &mut SnapshotState,
    project_id: &str,
    summary: BoundaryTypeSummary,
    gaps: &mut Vec<GapItem>,
) -> TypeSummaryPayload {
    if summary.is_unresolved {
        gaps.push(GapItem::from_reason(
            UnknownReason::UnresolvedType,
            format!(
                "Type resolved to an error or unresolved type ({}).",
                summary.type_text
            ),
            summary.gap_at.clone(),
        ));
    }
    alloc_type_summary_payload(snapshot, project_id, summary)
}

fn alloc_instantiated_params(
    snapshot: &mut SnapshotState,
    project_id: &str,
    params: Vec<tssae_tsrs_boundary::BoundaryInstantiatedParameter>,
) -> Vec<InstantiatedParameterPayload> {
    params
        .into_iter()
        .map(|param| InstantiatedParameterPayload {
            index: param.index,
            parameter_name: param.parameter_name,
            is_rest: param.is_rest,
            type_summary: alloc_type_summary_payload(snapshot, project_id, param.type_summary),
        })
        .collect()
}

fn run_type_at_location(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
) -> Result<Vec<TypeItem>, RpcErr> {
    let mut nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_syntax_node(project_id, handle_id)?;
        nodes.push((handle_id.clone(), node));
    }

    let cwd = snapshot.cwd.clone();
    let mut facade = BoundarySemanticFacade::new(&cwd, program, checker);
    let summaries: Vec<_> = nodes
        .into_iter()
        .map(|(target_id, node)| (target_id, facade.type_at_location(node)))
        .collect();

    let mut items = Vec::with_capacity(summaries.len());
    for (target_id, summary) in summaries {
        let mut gaps = Vec::new();
        let payload = alloc_type_summary_with_gap(snapshot, project_id, summary, &mut gaps);
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

fn run_resolve_declarations(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
    select: &DeclarationSelect,
) -> Result<Vec<DeclarationRow>, RpcErr> {
    let mut resolved_inputs = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let entry = snapshot.get_entry(project_id, handle_id)?;
        let target_payload = match entry.payload {
            HandlePayload::Syntax(node) | HandlePayload::Call(node) => {
                BoundaryDeclarationTarget::Node(node)
            }
            HandlePayload::Signature { sig, call_node } => {
                BoundaryDeclarationTarget::Signature(sig, call_node)
            }
            HandlePayload::Declaration { node, symbol } => {
                BoundaryDeclarationTarget::Declaration(node, symbol)
            }
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

    let facade_select = FacadeDeclarationSelect {
        name: select.name,
        container_name: select.container_name,
        kind: select.kind,
        location: select.location,
        is_external: select.is_external,
        package_name: select.package_name,
        export_name: select.export_name,
        declarations: select.declarations,
    };

    let cwd = snapshot.cwd.clone();
    let mut facade = BoundarySemanticFacade::new(&cwd, program, checker);
    let resolutions: Vec<_> = resolved_inputs
        .into_iter()
        .map(|(target_id, target_input)| {
            (
                target_id,
                facade.resolve_declaration(target_input, &facade_select),
            )
        })
        .collect();

    let mut items = Vec::with_capacity(resolutions.len());
    for (target_id, resolved) in resolutions {
        let mut gaps = Vec::new();
        let declaration = if let Some(decl_node) = resolved.primary_decl {
            Some(snapshot.alloc_declaration(project_id, decl_node, resolved.symbol))
        } else {
            gaps.push(GapItem::from_reason(
                UnknownReason::UnresolvedDeclaration,
                "Could not resolve target to a declaration.".to_string(),
                resolved.target_loc,
            ));
            None
        };

        let syntax = ProjectedField::from_selected(select.syntax, || {
            resolved
                .primary_decl
                .map(|decl| snapshot.alloc_syntax(project_id, decl))
        });

        let declarations = select.declarations.then(|| {
            resolved
                .all_decls
                .iter()
                .map(|&decl| snapshot.alloc_declaration(project_id, decl, resolved.symbol))
                .collect()
        });

        items.push(DeclarationRow {
            target: target_id,
            declaration,
            gaps,
            name: ProjectedField::from_option(select.name, resolved.name),
            container_name: ProjectedField::from_option(
                select.container_name,
                resolved.container_name,
            ),
            kind: ProjectedField::from_option(select.kind, resolved.kind),
            location: ProjectedField::from_option(select.location, resolved.location),
            syntax,
            is_external: resolved.is_external,
            package_name: ProjectedField::from_option(select.package_name, resolved.package_name),
            export_name: ProjectedField::from_option(select.export_name, resolved.export_name),
            declarations,
        });
    }
    Ok(items)
}

impl CallSelect {
    fn to_facade(&self) -> FacadeCallSelect {
        FacadeCallSelect {
            location: self.location,
            callee: self.callee,
            callee_declaration: self.callee_declaration,
            signature: self.signature,
            arguments: self.arguments,
            instantiated_parameter_types: self
                .instantiated_parameter_types
                .as_ref()
                .map(ParameterIndexSelection::to_facade),
            return_type: self.return_type,
        }
    }
}

struct AllocatedCallFields {
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

fn allocate_call_fields(
    snapshot: &mut SnapshotState,
    project_id: &str,
    node: BoundaryNode,
    select: &CallSelect,
    fields: BoundaryCallProjection,
) -> AllocatedCallFields {
    let syntax = select
        .syntax
        .then(|| snapshot.alloc_syntax(project_id, node));

    let callee = ProjectedField::from_option(
        select.callee,
        fields
            .callee
            .map(|expr| snapshot.alloc_syntax(project_id, expr)),
    );

    let callee_declaration = ProjectedField::from_option(
        select.callee_declaration,
        fields
            .callee_declaration
            .map(|(decl, sym)| snapshot.alloc_declaration(project_id, decl, sym)),
    );

    let signature = ProjectedField::from_option(
        select.signature,
        fields
            .signature
            .map(|sig| snapshot.alloc_signature(project_id, sig, Some(node))),
    );

    let arguments = fields.arguments.map(|args| {
        args.into_iter()
            .map(|arg| snapshot.alloc_syntax(project_id, arg))
            .collect()
    });

    let instantiated_parameter_types = fields
        .instantiated_parameter_types
        .map(|params| alloc_instantiated_params(snapshot, project_id, params));

    let mut gaps = fields.gaps;
    let return_type = ProjectedField::from_option(
        select.return_type,
        fields
            .return_type
            .map(|summary| alloc_type_summary_with_gap(snapshot, project_id, summary, &mut gaps)),
    );

    AllocatedCallFields {
        gaps,
        location: fields.location,
        syntax,
        callee,
        callee_declaration,
        signature,
        arguments,
        instantiated_parameter_types,
        return_type,
    }
}

fn run_resolve_calls(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
    select: &CallSelect,
) -> Result<Vec<CallResolutionRow>, RpcErr> {
    let mut nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_syntax_node(project_id, handle_id)?;
        nodes.push((handle_id.clone(), node));
    }

    let facade_select = select.to_facade();
    let cwd = snapshot.cwd.clone();
    let mut facade = BoundarySemanticFacade::new(&cwd, program, checker);
    let resolved: Vec<_> = nodes
        .into_iter()
        .map(|(target_id, node)| (target_id, node, facade.resolve_call(node, &facade_select)))
        .collect();

    let mut items = Vec::with_capacity(resolved.len());
    for (target_id, node, outcome) in resolved {
        match outcome {
            Err(not_a_call) => {
                let gaps = vec![GapItem::from_reason(
                    UnknownReason::NotACall,
                    format!(
                        "Syntax node of kind {} is not a call or new expression.",
                        not_a_call.kind_label
                    ),
                    not_a_call.location.clone(),
                )];
                let syntax = select
                    .syntax
                    .then(|| snapshot.alloc_syntax(project_id, node));
                let location = if select.location {
                    not_a_call.location
                } else {
                    None
                };
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
            }
            Ok(fields) => {
                let call_ref = snapshot.alloc_call(project_id, node);
                let allocated = allocate_call_fields(snapshot, project_id, node, select, fields);
                items.push(CallResolutionRow {
                    target: target_id,
                    call: Some(call_ref),
                    gaps: allocated.gaps,
                    location: allocated.location,
                    syntax: allocated.syntax,
                    callee: allocated.callee,
                    callee_declaration: allocated.callee_declaration,
                    signature: allocated.signature,
                    arguments: allocated.arguments,
                    instantiated_parameter_types: allocated.instantiated_parameter_types,
                    return_type: allocated.return_type,
                });
            }
        }
    }

    Ok(items)
}

fn run_project_calls(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
    select: &CallSelect,
) -> Result<Vec<CallProjectionRow>, RpcErr> {
    let mut call_nodes = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let node = snapshot.get_call_node(project_id, handle_id)?;
        call_nodes.push((handle_id.clone(), node));
    }

    let facade_select = select.to_facade();
    let cwd = snapshot.cwd.clone();
    let mut facade = BoundarySemanticFacade::new(&cwd, program, checker);
    let projected: Vec<_> = call_nodes
        .into_iter()
        .map(|(call_id, node)| (call_id, node, facade.project_call(node, &facade_select)))
        .collect();

    let mut items = Vec::with_capacity(projected.len());
    for (call_id, node, fields) in projected {
        let allocated = allocate_call_fields(snapshot, project_id, node, select, fields);
        items.push(CallProjectionRow {
            call: call_id,
            gaps: allocated.gaps,
            location: allocated.location,
            syntax: allocated.syntax,
            callee: allocated.callee,
            callee_declaration: allocated.callee_declaration,
            signature: allocated.signature,
            arguments: allocated.arguments,
            instantiated_parameter_types: allocated.instantiated_parameter_types,
            return_type: allocated.return_type,
        });
    }

    Ok(items)
}

fn run_project_signatures(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
    select: &SignatureSelect,
) -> Result<Vec<SignatureProjectionRow>, RpcErr> {
    let mut sig_entries = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let (sig, call_node) = snapshot.get_signature(project_id, handle_id)?;
        sig_entries.push((handle_id.clone(), sig, call_node));
    }

    let facade_select = FacadeSignatureSelect {
        signature_text: select.signature_text,
        declaration: select.declaration,
        location: select.location,
        syntax: select.syntax,
        parameters: select.parameters,
        min_argument_count: select.min_argument_count,
        has_rest_parameter: select.has_rest_parameter,
        instantiated_parameter_types: select
            .instantiated_parameter_types
            .as_ref()
            .map(ParameterIndexSelection::to_facade),
        return_type: select.return_type,
        is_instantiated: select.is_instantiated,
    };

    let cwd = snapshot.cwd.clone();
    let mut facade = BoundarySemanticFacade::new(&cwd, program, checker);
    let projected: Vec<_> = sig_entries
        .into_iter()
        .map(|(sig_id, sig, call_node)| {
            (
                sig_id,
                facade.project_signature(sig, call_node, &facade_select),
            )
        })
        .collect();

    let mut items = Vec::with_capacity(projected.len());
    for (sig_id, fields) in projected {
        let declaration = ProjectedField::from_option(
            select.declaration,
            fields
                .declaration
                .map(|(decl, sym)| snapshot.alloc_declaration(project_id, decl, sym)),
        );
        let location = ProjectedField::from_option(select.location, fields.location);
        let syntax = ProjectedField::from_option(
            select.syntax,
            fields
                .syntax_node
                .map(|decl| snapshot.alloc_syntax(project_id, decl)),
        );
        let parameters = fields.parameters.map(|params| {
            params
                .into_iter()
                .map(|param| SignatureParameterPayload {
                    name: param.name,
                    declaration: param
                        .declaration_node
                        .map(|d| snapshot.alloc_declaration(project_id, d, Some(param.symbol))),
                    optional: param.optional,
                    rest: param.rest,
                })
                .collect()
        });
        let instantiated_parameter_types = fields
            .instantiated_parameter_types
            .map(|params| alloc_instantiated_params(snapshot, project_id, params));
        let mut gaps = fields.gaps;
        let return_type = fields
            .return_type
            .map(|summary| alloc_type_summary_with_gap(snapshot, project_id, summary, &mut gaps));

        items.push(SignatureProjectionRow {
            signature: sig_id,
            gaps,
            signature_text: fields.signature_text,
            declaration,
            location,
            syntax,
            parameters,
            min_argument_count: fields.min_argument_count,
            has_rest_parameter: fields.has_rest_parameter,
            instantiated_parameter_types,
            return_type,
            is_instantiated: fields.is_instantiated,
        });
    }
    Ok(items)
}

fn run_project_types(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    targets: &[String],
    select: &TypeSelect,
) -> Result<Vec<TypeProjectionRow>, RpcErr> {
    let mut type_entries = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let (ty, enclosing) = snapshot.get_type_entry(project_id, handle_id)?;
        type_entries.push((handle_id.clone(), ty, enclosing));
    }

    let facade_select = FacadeTypeSelect {
        type_text: select.type_text,
        is_union: select.is_union,
        is_intersection: select.is_intersection,
        is_literal: select.is_literal,
        literal_value: select.literal_value,
        union_members: select.union_members,
        union_member_types: select.union_member_types,
        properties: select.properties,
        type_arguments: select.type_arguments,
    };

    let cwd = snapshot.cwd.clone();
    let mut facade = BoundarySemanticFacade::new(&cwd, program, checker);
    let projected: Vec<_> = type_entries
        .into_iter()
        .map(|(type_id, ty, enclosing)| {
            (
                type_id,
                enclosing,
                facade.project_type(ty, enclosing, &facade_select),
            )
        })
        .collect();

    let mut items = Vec::with_capacity(projected.len());
    for (type_id, enclosing, proj) in projected {
        let summary = proj.summary;
        let mut gaps = Vec::new();
        if summary.is_unresolved {
            gaps.push(GapItem::from_reason(
                UnknownReason::UnresolvedType,
                format!(
                    "Type resolved to an error or unresolved type ({}).",
                    summary.type_text
                ),
                summary.gap_at,
            ));
        }

        let union_member_types = proj.union_member_types.map(|members| {
            members
                .into_iter()
                .map(|member| snapshot.alloc_type(project_id, member, enclosing))
                .collect()
        });

        let properties = proj.properties.map(|props| {
            props
                .into_iter()
                .map(|prop| TypePropertyPayload {
                    name: prop.name,
                    optional: prop.optional,
                    declaration: prop
                        .declaration_node
                        .map(|d| snapshot.alloc_declaration(project_id, d, Some(prop.symbol))),
                })
                .collect()
        });

        let type_arguments = proj.type_arguments.map(|args| {
            args.into_iter()
                .map(|arg_ty| snapshot.alloc_type(project_id, arg_ty, enclosing))
                .collect()
        });

        items.push(TypeProjectionRow {
            type_ref: type_id,
            gaps,
            type_text: select.type_text.then_some(summary.type_text),
            is_union: select.is_union.then_some(summary.is_union),
            is_intersection: select.is_intersection.then_some(summary.is_intersection),
            is_literal: select.is_literal.then_some(summary.is_literal),
            literal_value: ProjectedField::from_option(select.literal_value, summary.literal_value),
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
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
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

    let cwd = snapshot.cwd.clone();
    let outcomes: Vec<_> = {
        let mut facade = BoundarySemanticFacade::new(&cwd, program, checker);
        resolved
            .into_iter()
            .map(|(source_id, target_id, source_ty, target_ty, enclosing)| {
                (
                    source_id,
                    target_id,
                    facade.is_assignable_to(source_ty, target_ty, enclosing),
                )
            })
            .collect()
    };

    let mut items = Vec::with_capacity(outcomes.len());
    for (source_id, target_id, outcome) in outcomes {
        let mut gaps = Vec::new();
        if outcome.has_unresolved_type {
            gaps.push(GapItem::from_reason(
                UnknownReason::UnresolvedType,
                "Assignability check involves an error or unresolved type.".to_string(),
                outcome.gap_at,
            ));
        }
        items.push(AssignabilityRow {
            source: source_id,
            target: target_id,
            assignable: outcome.assignable,
            gaps,
        });
    }

    Ok(items)
}

fn allocate_syntax_lookup_row(
    snapshot: &mut SnapshotState,
    project_id: &str,
    syntax_view: &SyntaxView<'_>,
    lookup_opts: &SyntaxLookupOptions,
    select: &SyntaxLookupSelect,
    target_id: String,
    node_opt: Option<BoundaryNode>,
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

    let inspected = syntax_view.inspect_boundary_node(node, lookup_opts);
    let syntax_ref = snapshot.alloc_syntax(project_id, inspected.node);

    SyntaxLookupRow {
        target: target_id,
        syntax: Some(syntax_ref),
        gaps: Vec::new(),
        kind: ProjectedField::from_option(select.kind, inspected.kind),
        text: ProjectedField::from_option(select.text, inspected.text),
        location: ProjectedField::from_option(select.location, inspected.location),
        expression: ProjectedField::from_option(
            select.expression,
            inspected
                .expression
                .map(|e| snapshot.alloc_syntax(project_id, e)),
        ),
        arguments: inspected.arguments.map(|args| {
            args.into_iter()
                .map(|arg| snapshot.alloc_syntax(project_id, arg))
                .collect()
        }),
        name: ProjectedField::from_option(
            select.name,
            inspected.name.map(|n| snapshot.alloc_syntax(project_id, n)),
        ),
        initializer: ProjectedField::from_option(
            select.initializer,
            inspected
                .initializer
                .map(|i| snapshot.alloc_syntax(project_id, i)),
        ),
        body: ProjectedField::from_option(
            select.body,
            inspected.body.map(|b| snapshot.alloc_syntax(project_id, b)),
        ),
        parameters: inspected.parameters.map(|params| {
            params
                .into_iter()
                .map(|p| snapshot.alloc_syntax(project_id, p))
                .collect()
        }),
    }
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
            HandlePayload::Signature { sig, .. } => {
                BoundarySemanticFacade::raw_signature_declaration_node(sig)
            }
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

    let cwd = snapshot.cwd.clone();
    let syntax_view = SyntaxView::new(&cwd);
    let lookup_opts = SyntaxLookupOptions {
        kind: select.kind,
        text: select.text,
        location: select.location,
        expression: select.expression,
        arguments: select.arguments,
        name: select.name,
        initializer: select.initializer,
        body: select.body,
        parameters: select.parameters,
    };

    let mut items = Vec::with_capacity(resolved.len());
    for (target_id, node_opt) in resolved {
        items.push(allocate_syntax_lookup_row(
            snapshot,
            project_id,
            &syntax_view,
            &lookup_opts,
            select,
            target_id,
            node_opt,
        ));
    }
    Ok(items)
}
