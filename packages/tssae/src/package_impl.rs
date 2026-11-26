use serde::Serialize;
use tsrs_ast::Node;
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::P;
use tssae_tsrs_boundary::{
    FacadeImplementationInput, FacadeImplementationOutcome, FacadeImplementationTarget,
    FirstPartyResolutionSource, SemanticFacade, SourceMapCache,
};

use crate::cfg::{EvidenceItem, GapItem};
use crate::serve::{HandlePayload, RpcErr, SnapshotState, SourceLocation};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedImplTargetPayload {
    pub file: String,
    pub location: SourceLocation,
    pub declaration: String,
    pub export_name: Option<String>,
    pub package_name: Option<String>,
    pub resolution_source: FirstPartyResolutionSource,
    pub declaration_file: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "status")]
pub enum ImplementationResolutionRow {
    #[serde(rename = "resolved", rename_all = "camelCase")]
    Resolved {
        target: String,
        implementation: ResolvedImplTargetPayload,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "external", rename_all = "camelCase")]
    External {
        target: String,
        package_name: Option<String>,
        declaration_file: Option<String>,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename = "inconclusive", rename_all = "camelCase")]
    Inconclusive {
        target: String,
        declaration_file: Option<String>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

pub fn to_resolved_impl_payload(
    snapshot: &mut SnapshotState,
    project_id: &str,
    target: FacadeImplementationTarget,
) -> ResolvedImplTargetPayload {
    let declaration = snapshot.alloc_declaration(
        project_id,
        target.declaration_node,
        target.declaration_symbol,
    );
    ResolvedImplTargetPayload {
        file: target.file,
        location: target.location,
        declaration,
        export_name: target.export_name,
        package_name: target.package_name,
        resolution_source: target.resolution_source,
        declaration_file: target.declaration_file,
    }
}

pub fn resolve_first_party_for_call_target(
    cwd: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    workspace_programs: &[&'static Program],
    sourcemap_cache: &SourceMapCache,
    decl_node: Option<P<Node>>,
    call_node: P<Node>,
) -> FacadeImplementationOutcome {
    let mut facade = SemanticFacade::new(cwd, program, checker);
    let input = decl_node.map_or(FacadeImplementationInput::Node(call_node), |decl| {
        FacadeImplementationInput::Declaration(decl, decl.symbol())
    });
    facade.resolve_implementation(input, workspace_programs, sourcemap_cache)
}

pub fn check_call_missing_package_gap(
    program: &'static Program,
    checker: &mut CheckerHandle,
    cwd: &str,
    call_node: P<Node>,
) -> Option<GapItem> {
    SemanticFacade::new(cwd, program, checker).check_call_missing_package_gap(call_node)
}

pub fn run_resolve_implementations(
    snapshot: &mut SnapshotState,
    project_id: &str,
    program: &'static Program,
    checker: &mut CheckerHandle,
    targets: &[String],
) -> Result<Vec<ImplementationResolutionRow>, RpcErr> {
    let mut inputs = Vec::with_capacity(targets.len());
    for handle_id in targets {
        let entry = snapshot.get_entry(project_id, handle_id)?;
        let input = match entry.payload {
            HandlePayload::Call(node) | HandlePayload::Syntax(node) => {
                FacadeImplementationInput::Node(node)
            }
            HandlePayload::Declaration { node, symbol } => {
                FacadeImplementationInput::Declaration(node, symbol)
            }
            _ => {
                return Err(RpcErr::new(
                    "INVALID_REFERENCE_KIND",
                    format!(
                        "Program reference {handle_id} cannot be used with resolveImplementations."
                    ),
                ));
            }
        };
        inputs.push((handle_id.clone(), input));
    }

    let cwd = snapshot.cwd.clone();
    let workspace_programs = snapshot.workspace_programs();
    let sourcemap_cache = snapshot.sourcemap_cache.clone();

    let mut outcomes = Vec::with_capacity(inputs.len());
    for (target_id, input) in inputs {
        let outcome = SemanticFacade::new(&cwd, program, checker).resolve_implementation(
            input,
            &workspace_programs,
            &sourcemap_cache,
        );
        outcomes.push((target_id, outcome));
    }

    let mut rows = Vec::with_capacity(outcomes.len());
    for (target_id, outcome) in outcomes {
        let row = match outcome {
            FacadeImplementationOutcome::Resolved(impl_target) => {
                let mut evidence = Vec::new();
                if let Some(ref decl_file) = impl_target.declaration_file {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        format!(
                            "Mapped first-party declaration '{}' to implementation '{}'",
                            decl_file, impl_target.file
                        ),
                        Some(impl_target.location.clone()),
                    ));
                } else {
                    evidence.push(EvidenceItem::simple(
                        "source",
                        format!("Implementation is available in '{}'", impl_target.file),
                        Some(impl_target.location.clone()),
                    ));
                }
                let implementation = to_resolved_impl_payload(snapshot, project_id, impl_target);
                ImplementationResolutionRow::Resolved {
                    target: target_id,
                    implementation,
                    evidence,
                }
            }
            FacadeImplementationOutcome::External {
                package_name,
                declaration_file,
                evidence,
            } => ImplementationResolutionRow::External {
                target: target_id,
                package_name,
                declaration_file,
                evidence,
            },
            FacadeImplementationOutcome::Inconclusive {
                declaration_file,
                gaps,
                evidence,
            } => ImplementationResolutionRow::Inconclusive {
                target: target_id,
                declaration_file,
                gaps,
                evidence,
            },
        };
        rows.push(row);
    }

    Ok(rows)
}
