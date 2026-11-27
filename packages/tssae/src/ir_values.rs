use std::sync::Arc;

use rustc_hash::FxHashSet;
use serde::Serialize;
use tsrs_ast::Node;
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;
use tssae_analysis::{
    call_resolution::IrCallGraph,
    value_propagation::{
        ValueEndpointSpec, ValuePropagationOutcome, ValuePropagationPathData,
        ValuePropagationStepData, ValuePropagationTargetOutcome, trace_value_propagation,
    },
};
use tssae_common::{EvidenceItem, FileId, GapItem, LocalSyntaxId, SemanticModel, SyntaxId};
use tssae_tsrs_boundary::normalize_path;

use crate::ir_calls::SnapshotIrEnv;
use crate::observe::ObserveCollector;
use crate::serve::{HandlePayload, RpcErr, SnapshotState, SourceLocation};

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
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ValuePropagationTargetRow {
    #[serde(rename = "proved", rename_all = "camelCase")]
    Proved {
        from: String,
        to: String,
        paths: Vec<ValuePropagationPathPayload>,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "disproved", rename_all = "camelCase")]
    Disproved {
        from: String,
        to: String,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "inconclusive", rename_all = "camelCase")]
    Inconclusive {
        from: String,
        to: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        partial_paths: Vec<ValuePropagationPathPayload>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValuePropagationResultPayload {
    pub propagation: &'static str,
    pub results: Vec<ValuePropagationTargetRow>,
}

struct EndpointHandleInput {
    handle_id: String,
    node: P<Node>,
    symbol: Option<P<tsrs_ast::Symbol>>,
}

fn collect_endpoint_inputs(
    snapshot: &SnapshotState,
    project_id: &str,
    handles: &[String],
) -> Result<Vec<EndpointHandleInput>, RpcErr> {
    let mut out = Vec::with_capacity(handles.len());
    for handle_id in handles {
        let entry = snapshot.get_entry(project_id, handle_id)?;
        let (node, symbol) = match entry.payload {
            HandlePayload::Syntax(node) | HandlePayload::Call(node) => (node, None),
            HandlePayload::Declaration { node, symbol } => (node, symbol),
            HandlePayload::Signature { .. }
            | HandlePayload::Type { .. }
            | HandlePayload::Branch(_)
            | HandlePayload::Instance(_) => {
                return Err(RpcErr::new(
                    "INVALID_REFERENCE_KIND",
                    format!(
                        "Program reference {handle_id} cannot be used as a value propagation endpoint."
                    ),
                ));
            }
        };
        out.push(EndpointHandleInput {
            handle_id: handle_id.clone(),
            node,
            symbol,
        });
    }
    Ok(out)
}

fn intern_endpoints(
    env: &SnapshotIrEnv<'_>,
    inputs: &[EndpointHandleInput],
) -> Vec<ValueEndpointSpec> {
    let mut specs = Vec::with_capacity(inputs.len());
    for input in inputs {
        if let Some(syn) = env.semantic.intern_syntax(input.node) {
            let sym = input
                .symbol
                .map(|s| env.semantic.intern_symbol(s))
                .or_else(|| env.symbol_at_syntax(syn));
            specs.push(ValueEndpointSpec {
                syntax: syn,
                symbol: sym,
            });
        } else {
            specs.push(ValueEndpointSpec {
                syntax: SyntaxId::from_parts(FileId(u32::MAX), LocalSyntaxId(u32::MAX)),
                symbol: None,
            });
        }
    }
    specs
}

fn map_step(step: ValuePropagationStepData) -> ValuePropagationStepPayload {
    ValuePropagationStepPayload {
        kind: step.kind,
        description: step.description,
        from_location: step.from_location,
        to_location: step.to_location,
        property: step.property,
        omitted_properties: step.omitted_properties,
    }
}

fn map_path(
    from_id: &str,
    to_id: &str,
    path: ValuePropagationPathData,
) -> ValuePropagationPathPayload {
    ValuePropagationPathPayload {
        from: from_id.to_string(),
        to: to_id.to_string(),
        steps: path.steps.into_iter().map(map_step).collect(),
        evidence: path.evidence,
        source_path: path.source_path,
        omitted_properties: path.omitted_properties,
    }
}

fn build_target_row(
    from_id: &str,
    to_id: &str,
    target_outcome: ValuePropagationTargetOutcome,
) -> ValuePropagationTargetRow {
    match target_outcome {
        ValuePropagationTargetOutcome::Proved { paths, evidence } => {
            ValuePropagationTargetRow::Proved {
                from: from_id.to_string(),
                to: to_id.to_string(),
                paths: paths
                    .into_iter()
                    .map(|p| map_path(from_id, to_id, p))
                    .collect(),
                evidence,
            }
        }
        ValuePropagationTargetOutcome::Disproved { evidence } => {
            ValuePropagationTargetRow::Disproved {
                from: from_id.to_string(),
                to: to_id.to_string(),
                evidence,
            }
        }
        ValuePropagationTargetOutcome::Inconclusive {
            partial_paths,
            gaps,
            evidence,
        } => ValuePropagationTargetRow::Inconclusive {
            from: from_id.to_string(),
            to: to_id.to_string(),
            partial_paths: partial_paths
                .into_iter()
                .map(|p| map_path(from_id, to_id, p))
                .collect(),
            gaps,
            evidence,
        },
    }
}

fn conflict_rpc_err(conflict: &tssae_common::SummaryConflictError) -> RpcErr {
    let details = serde_json::to_value(conflict).unwrap_or(serde_json::Value::Null);
    RpcErr::with_details("SUMMARY_CONFLICT", conflict.format_message(), details)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Executes IR-backed tri-state value propagation with cached call graph and summary lookup reporting."
)]
pub fn run_value_propagation(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    propagation: Option<&str>,
    include_member_access: bool,
    from_handles: &[String],
    to_handles: &[String],
    lookup_files: Option<&[String]>,
    observer: &mut ObserveCollector,
) -> Result<ValuePropagationResultPayload, RpcErr> {
    if let Some(mode) = propagation
        && mode != "value"
    {
        return Err(RpcErr::new(
            "INVALID_PARAMS",
            format!("Unsupported propagation mode: {mode}"),
        ));
    }

    let from_inputs = collect_endpoint_inputs(snapshot, project_id, from_handles)?;
    let to_inputs = collect_endpoint_inputs(snapshot, project_id, to_handles)?;
    if from_inputs.is_empty() || to_inputs.is_empty() {
        return Ok(ValuePropagationResultPayload {
            propagation: "value",
            results: Vec::new(),
        });
    }

    let workspace_programs = snapshot.workspace_programs();
    let cached_graph = snapshot.ir_call_graph_cache.get(project_id).cloned();
    let summary_index = Arc::clone(&snapshot.summary_index);
    let file_ids = Arc::clone(&snapshot.file_ids);
    let allowed_files: Option<FxHashSet<FileId>> = lookup_files.map(|files| {
        let mut set = FxHashSet::default();
        if let Ok(mut reg) = file_ids.write() {
            for f in files {
                set.insert(reg.intern(&normalize_path(f)));
            }
        }
        set
    });

    let (outcome, newly_built_graph): (ValuePropagationOutcome, Option<IrCallGraph>) = {
        let env = SnapshotIrEnv::from_snapshot(
            snapshot,
            project_id,
            program,
            &workspace_programs,
            checker,
        );
        let from_specs = intern_endpoints(&env, &from_inputs);
        let to_specs = intern_endpoints(&env, &to_inputs);

        if let Some(g) = cached_graph.as_deref()
            && let Some(conflict) = &g.conflict
        {
            let stats = env.summary_stats.get();
            observer.record_summary_lookup_stats(&stats);
            let err = conflict_rpc_err(conflict);
            drop(env);
            return Err(err);
        }

        let result = trace_value_propagation(
            &from_specs,
            &to_specs,
            include_member_access,
            env.project_files(),
            allowed_files.as_ref(),
            &env,
            &env,
            &env,
            &summary_index,
            cached_graph.as_deref(),
        );
        let stats = env.summary_stats.get();
        observer.record_summary_lookup_stats(&stats);
        drop(env);
        result
    };

    if let Some(g) = newly_built_graph {
        let g_arc = Arc::new(g);
        if let Some(conflict) = &g_arc.conflict {
            return Err(conflict_rpc_err(conflict));
        }
        if allowed_files.is_none() {
            snapshot
                .ir_call_graph_cache
                .insert(project_id.to_string(), g_arc);
        }
    }

    if let Some(conflict) = &outcome.conflict {
        return Err(conflict_rpc_err(conflict));
    }

    let rows = outcome
        .results
        .into_iter()
        .enumerate()
        .map(|(idx, target_outcome)| {
            let from_idx = idx / to_inputs.len();
            let to_idx = idx % to_inputs.len();
            build_target_row(
                &from_inputs[from_idx].handle_id,
                &to_inputs[to_idx].handle_id,
                target_outcome,
            )
        })
        .collect();

    Ok(ValuePropagationResultPayload {
        propagation: "value",
        results: rows,
    })
}
