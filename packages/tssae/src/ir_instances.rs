use std::sync::Arc;

use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use tsrs_ast::Node;
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;
use tssae_analysis::{
    call_resolution::IrCallGraph,
    instance_identity::{
        InstanceOriginKind, InstanceResolutionOutcome, InstanceTargetSpec, IrFieldWriteIndex,
        ResolveInstancesBatchOutcome, ResolvedInstanceOrigin, resolve_instances_on_ir,
    },
};
use tssae_common::{
    EvidenceItem, FileId, GapItem, LocalSyntaxId, SemanticModel, SummarySource, SyntaxId,
};
use tssae_tsrs_boundary::normalize_path;

use crate::ir_calls::SnapshotIrEnv;
use crate::observe::ObserveCollector;
use crate::serve::{HandlePayload, RpcErr, SnapshotState, SourceLocation};

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
    pub summary_id: bool,
    #[serde(default)]
    pub evidence: bool,
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

#[derive(Clone)]
pub struct InstanceHandleData {
    pub instance_key: String,
    pub origin_kind: &'static str,
    pub allocation_location: Option<SourceLocation>,
    pub origin_syntax: Option<P<Node>>,
    pub summary_id: Option<String>,
    pub summary_source: Option<SummarySource>,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedInstanceInfoPayload {
    #[serde(rename = "ref")]
    pub instance_ref: String,
    pub instance_key: String,
    pub origin_kind: InstanceOriginKind,
    pub allocation_location: Option<SourceLocation>,
    pub origin_syntax: Option<String>,
    pub summary_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_source: Option<SummarySource>,
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum InstanceResolutionRow {
    #[serde(rename = "proved", rename_all = "camelCase")]
    Proved {
        target: String,
        instance: Option<String>,
        instance_key: Option<String>,
        allocation_location: Option<SourceLocation>,
        origin_kind: Option<InstanceOriginKind>,
        instances: Vec<ResolvedInstanceInfoPayload>,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "disproved", rename_all = "camelCase")]
    Disproved {
        target: String,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "inconclusive", rename_all = "camelCase")]
    Inconclusive {
        target: String,
        instances: Vec<ResolvedInstanceInfoPayload>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
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
    pub summary_id: ProjectedInstanceField<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_source: Option<SummarySource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<EvidenceItem>>,
}

struct TargetHandleInput {
    handle_id: String,
    node: P<Node>,
    symbol: Option<P<tsrs_ast::Symbol>>,
}

struct MaterializedOrigin {
    instance_key: String,
    origin_kind: InstanceOriginKind,
    allocation_location: Option<SourceLocation>,
    origin_node: Option<P<Node>>,
    summary_id: Option<String>,
    summary_source: Option<SummarySource>,
    evidence: Vec<EvidenceItem>,
}

enum MaterializedOutcome {
    Proved {
        origins: Vec<MaterializedOrigin>,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        origins: Vec<MaterializedOrigin>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

fn collect_target_inputs(
    snapshot: &SnapshotState,
    project_id: &str,
    handles: &[String],
) -> Result<Vec<TargetHandleInput>, RpcErr> {
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
                        "Program reference {handle_id} cannot be resolved as an instance target."
                    ),
                ));
            }
        };
        out.push(TargetHandleInput {
            handle_id: handle_id.clone(),
            node,
            symbol,
        });
    }
    Ok(out)
}

fn conflict_rpc_err(conflict: &tssae_common::SummaryConflictError) -> RpcErr {
    let details = serde_json::to_value(conflict).unwrap_or(serde_json::Value::Null);
    RpcErr::with_details("SUMMARY_CONFLICT", conflict.format_message(), details)
}

fn materialize_origin(
    origin: ResolvedInstanceOrigin,
    env: &SnapshotIrEnv<'_>,
) -> MaterializedOrigin {
    let origin_node = origin
        .origin_syntax
        .and_then(|syn| env.semantic.node_for_syntax(syn));
    MaterializedOrigin {
        instance_key: origin.instance_key,
        origin_kind: origin.origin_kind,
        allocation_location: origin.allocation_location,
        origin_node,
        summary_id: origin.summary_id,
        summary_source: origin.summary_source,
        evidence: origin.evidence,
    }
}

fn alloc_origin_payloads(
    snapshot: &mut SnapshotState,
    project_id: &str,
    origins: Vec<MaterializedOrigin>,
) -> Vec<ResolvedInstanceInfoPayload> {
    let mut payloads = Vec::with_capacity(origins.len());
    for origin in origins {
        let origin_syntax = origin
            .origin_node
            .map(|n| snapshot.alloc_syntax(project_id, n));
        let inst_ref = snapshot.alloc_instance(
            project_id,
            InstanceHandleData {
                instance_key: origin.instance_key.clone(),
                origin_kind: origin.origin_kind.as_str(),
                allocation_location: origin.allocation_location.clone(),
                origin_syntax: origin.origin_node,
                summary_id: origin.summary_id.clone(),
                summary_source: origin.summary_source,
                evidence: origin.evidence.clone(),
            },
        );
        payloads.push(ResolvedInstanceInfoPayload {
            instance_ref: inst_ref,
            instance_key: origin.instance_key,
            origin_kind: origin.origin_kind,
            allocation_location: origin.allocation_location,
            origin_syntax,
            summary_id: origin.summary_id,
            summary_source: origin.summary_source,
            evidence: origin.evidence,
        });
    }
    payloads
}

type InstanceBatchMaterialized = (
    Vec<(String, MaterializedOutcome)>,
    Option<tssae_common::SummaryConflictError>,
);

#[expect(
    clippy::too_many_lines,
    reason = "Orchestrates target interning, IR resolution, call-graph caching, summary metrics, and handle allocation."
)]
pub fn run_resolve_instances(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
    lookup_files: Option<&[String]>,
    observer: &mut ObserveCollector,
) -> Result<Vec<InstanceResolutionRow>, RpcErr> {
    let target_inputs = collect_target_inputs(snapshot, project_id, targets)?;
    if target_inputs.is_empty() {
        return Ok(Vec::new());
    }

    let workspace_programs = snapshot.workspace_programs();
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

    let (materialized, conflict_opt): InstanceBatchMaterialized = {
        let mut env = SnapshotIrEnv::from_snapshot(
            snapshot,
            project_id,
            program,
            &workspace_programs,
            checker,
        );

        let specs: Vec<InstanceTargetSpec> = target_inputs
            .iter()
            .map(|input| {
                env.semantic.intern_syntax(input.node).map_or_else(
                    || InstanceTargetSpec {
                        syntax: SyntaxId::from_parts(FileId(u32::MAX), LocalSyntaxId(u32::MAX)),
                        symbol: None,
                    },
                    |syn| {
                        let sym = input
                            .symbol
                            .map(|s| env.semantic.intern_symbol(s))
                            .or_else(|| env.symbol_at_syntax(syn));
                        InstanceTargetSpec {
                            syntax: syn,
                            symbol: sym,
                        }
                    },
                )
            })
            .collect();

        let is_full_project = allowed_files.is_none();
        let call_target_files: Vec<FileId> = allowed_files.as_ref().map_or_else(
            || env.project_files().to_vec(),
            |allowed| {
                env.project_files()
                    .iter()
                    .copied()
                    .filter(|f| allowed.contains(f))
                    .collect()
            },
        );
        let cached_graph =
            env.ensure_call_graph(&summary_index, &call_target_files, is_full_project);
        if let Some(conflict) = &cached_graph.conflict {
            let stats = env.summary_stats.get();
            observer.record_summary_lookup_stats(&stats);
            let err = conflict_rpc_err(conflict);
            drop(env);
            return Err(err);
        }

        let extra_field_files: FxHashSet<FileId> =
            specs.iter().map(|t| t.syntax.file_id()).collect();
        let field_target_files: Vec<FileId> = allowed_files.as_ref().map_or_else(
            || env.project_files().to_vec(),
            |allowed| {
                env.project_files()
                    .iter()
                    .copied()
                    .filter(|f| allowed.contains(f) || extra_field_files.contains(f))
                    .collect()
            },
        );
        let cached_field_index = env.ensure_field_write_index(&field_target_files, is_full_project);

        let (batch_outcome, _, _): (
            ResolveInstancesBatchOutcome,
            Option<IrCallGraph>,
            Option<IrFieldWriteIndex>,
        ) = resolve_instances_on_ir(
            &specs,
            env.project_files(),
            allowed_files.as_ref(),
            &env,
            &env,
            &env,
            &summary_index,
            Some(cached_graph.as_ref()),
            Some(cached_field_index.as_ref()),
        );

        let stats = env.summary_stats.get();
        observer.record_summary_lookup_stats(&stats);

        let mapped = target_inputs
            .into_iter()
            .zip(batch_outcome.results)
            .map(|(input, outcome)| {
                let mat = match outcome {
                    InstanceResolutionOutcome::Proved { origins, evidence } => {
                        MaterializedOutcome::Proved {
                            origins: origins
                                .into_iter()
                                .map(|o| materialize_origin(o, &env))
                                .collect(),
                            evidence,
                        }
                    }
                    InstanceResolutionOutcome::Disproved { evidence } => {
                        MaterializedOutcome::Disproved { evidence }
                    }
                    InstanceResolutionOutcome::Inconclusive {
                        origins,
                        gaps,
                        evidence,
                    } => MaterializedOutcome::Inconclusive {
                        origins: origins
                            .into_iter()
                            .map(|o| materialize_origin(o, &env))
                            .collect(),
                        gaps,
                        evidence,
                    },
                };
                (input.handle_id, mat)
            })
            .collect();

        drop(env);
        (mapped, batch_outcome.conflict)
    };

    if let Some(conflict) = conflict_opt {
        return Err(conflict_rpc_err(&conflict));
    }

    let mut rows = Vec::with_capacity(materialized.len());
    for (target_id, outcome) in materialized {
        match outcome {
            MaterializedOutcome::Proved { origins, evidence } => {
                let payloads = alloc_origin_payloads(snapshot, project_id, origins);
                let single = (payloads.len() == 1).then(|| &payloads[0]);
                rows.push(InstanceResolutionRow::Proved {
                    target: target_id,
                    instance: single.map(|p| p.instance_ref.clone()),
                    instance_key: single.map(|p| p.instance_key.clone()),
                    allocation_location: single.and_then(|p| p.allocation_location.clone()),
                    origin_kind: single.map(|p| p.origin_kind),
                    instances: payloads,
                    evidence,
                });
            }
            MaterializedOutcome::Disproved { evidence } => {
                rows.push(InstanceResolutionRow::Disproved {
                    target: target_id,
                    evidence,
                });
            }
            MaterializedOutcome::Inconclusive {
                origins,
                gaps,
                evidence,
            } => {
                let payloads = alloc_origin_payloads(snapshot, project_id, origins);
                rows.push(InstanceResolutionRow::Inconclusive {
                    target: target_id,
                    instances: payloads,
                    gaps,
                    evidence,
                });
            }
        }
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
            summary_id: ProjectedInstanceField::from_selected(select.summary_id, data.summary_id),
            summary_source: select.summary_id.then_some(data.summary_source).flatten(),
            evidence: select.evidence.then_some(data.evidence),
        });
    }

    Ok(rows)
}
