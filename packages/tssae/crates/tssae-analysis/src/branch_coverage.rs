use std::collections::VecDeque;

use rustc_hash::FxHashSet;
use serde::Serialize;
use tssae_common::{
    BranchId, DiagnosticLocation, EvidenceItem, GapItem, SyntaxId, UnknownReason, VertexId,
};

use crate::ir::{ExplicitEdge, ExplicitEdgeKind, FunctionIr, IrOp};

/// Single control-flow anchor on a concrete counterexample bypass path.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlFlowPathStep {
    pub location: DiagnosticLocation,
    pub description: String,
}

/// Concrete explicit counterexample path bypassing all selected guard branches to reach a target.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchBypassPath {
    pub steps: Vec<ControlFlowPathStep>,
    pub evidence: Vec<EvidenceItem>,
}

/// Tri-state outcome produced by evaluating branch coverage over a [`FunctionIr`] CFG.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BranchCoverageOutcome {
    Proved {
        target_reachable: bool,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        target_reachable: bool,
        counterexample_path: BranchBypassPath,
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        target_reachable: bool,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

/// Evaluates whether every explicit control-flow path from scope entry to `target_syntax`
/// passes through at least one branch in `selected_branches`.
///
/// Path exploration tracks whether a bypass path passed through any `IrOp::Unknown` vertex:
/// - If `target_syntax` is unreachable in the modeled CFG, returns `Proved { target_reachable: false }`.
/// - If a completely known bypass path (with zero `Unknown` vertices) reaches the target,
///   returns `Disproved` with that path as `counterexample_path` even if another bypass path is unknown.
/// - If all bypass paths pass through at least one `Unknown` vertex, returns `Inconclusive`
///   and reports ONLY the `Unknown` vertices that lie on actual bypass paths (`fwd && bwd` in `G \ cut_set`).
/// - If no path (known or unknown) bypasses `selected_branches` to reach the target,
///   returns `Proved { target_reachable: true }` with zero gaps, ignoring unrelated `Unknown` constructs.
#[must_use]
pub fn evaluate_branch_coverage(
    ir: &FunctionIr,
    target_syntax: Option<SyntaxId>,
    target_location: &DiagnosticLocation,
    selected_branches: &[BranchId],
    extra_gaps: &[GapItem],
) -> BranchCoverageOutcome {
    let mut combined_gaps = extra_gaps.to_vec();

    let Some(target_vid) = target_syntax.and_then(|sid| ir.syntax_to_vertex.get(&sid).copied())
    else {
        if combined_gaps.is_empty() {
            combined_gaps.push(GapItem::from_reason(
                UnknownReason::TargetOutsideCfg,
                "Target node is not part of the modeled function body control flow.".to_string(),
                Some(target_location.clone()),
            ));
        }
        return BranchCoverageOutcome::Inconclusive {
            target_reachable: false,
            evidence: gaps_to_evidence(&combined_gaps),
            gaps: combined_gaps,
        };
    };

    let reachable_all = reachable_vertices(ir, &FxHashSet::default());
    let target_idx = vertex_index(target_vid);
    let target_reachable = reachable_all.get(target_idx).copied().unwrap_or(false);

    if !combined_gaps.is_empty() {
        return BranchCoverageOutcome::Inconclusive {
            target_reachable,
            evidence: gaps_to_evidence(&combined_gaps),
            gaps: combined_gaps,
        };
    }

    if !target_reachable {
        return BranchCoverageOutcome::Proved {
            target_reachable: false,
            evidence: vec![EvidenceItem::simple(
                "source",
                "Target is unreachable from function entry in modeled control flow.".to_string(),
                Some(target_location.clone()),
            )],
        };
    }

    let cut_set: FxHashSet<BranchId> = selected_branches
        .iter()
        .copied()
        .filter(|id| usize::try_from(id.0).is_ok_and(|idx| idx < ir.branches.len()))
        .collect();

    let search = search_bypass_states(ir, target_vid, &cut_set);

    if let Some(witness_edges) = search.known_bypass {
        let counterexample_path = build_bypass_path(ir, &witness_edges, target_location);
        let evidence = counterexample_path.evidence.clone();
        return BranchCoverageOutcome::Disproved {
            target_reachable: true,
            counterexample_path,
            evidence,
        };
    }

    if search.unknown_bypass_reachable {
        let gaps = collect_critical_path_gaps(ir, target_vid, &cut_set);
        let evidence = gaps_to_evidence(&gaps);
        return BranchCoverageOutcome::Inconclusive {
            target_reachable: true,
            gaps,
            evidence,
        };
    }

    let evidence = build_covered_evidence(ir, selected_branches);
    BranchCoverageOutcome::Proved {
        target_reachable: true,
        evidence,
    }
}

const fn vertex_index(vid: VertexId) -> usize {
    vid.0 as usize
}

fn is_unknown_vertex(ir: &FunctionIr, vid: VertexId) -> bool {
    ir.vertices
        .get(vertex_index(vid))
        .is_some_and(|v| matches!(v.op, IrOp::Unknown { .. }))
}

fn reachable_vertices(ir: &FunctionIr, cut_set: &FxHashSet<BranchId>) -> Vec<bool> {
    let mut visited = vec![false; ir.vertices.len()];
    let mut queue = VecDeque::new();
    let entry_idx = vertex_index(ir.entry);
    if entry_idx < visited.len() {
        visited[entry_idx] = true;
        queue.push_back(ir.entry);
    }

    while let Some(u) = queue.pop_front() {
        let Some(vertex) = ir.vertices.get(vertex_index(u)) else {
            continue;
        };
        for edge in &vertex.explicit_edges {
            if let ExplicitEdgeKind::Branch(branch_id) = edge.kind
                && cut_set.contains(&branch_id)
            {
                continue;
            }
            let next_idx = vertex_index(edge.target);
            if next_idx < visited.len() && !visited[next_idx] {
                visited[next_idx] = true;
                queue.push_back(edge.target);
            }
        }
    }

    visited
}

struct BypassSearchResult<'a> {
    known_bypass: Option<Vec<(VertexId, &'a ExplicitEdge)>>,
    unknown_bypass_reachable: bool,
}

type BypassParentEntry<'a> = [Option<(VertexId, bool, &'a ExplicitEdge)>; 2];

fn search_bypass_states<'a>(
    ir: &'a FunctionIr,
    target_vid: VertexId,
    cut_set: &FxHashSet<BranchId>,
) -> BypassSearchResult<'a> {
    let n = ir.vertices.len();
    let mut visited = vec![[false; 2]; n];
    let mut parent: Vec<BypassParentEntry<'a>> = vec![[None, None]; n];
    let mut queue = VecDeque::new();

    let start_unknown = is_unknown_vertex(ir, ir.entry);
    let entry_idx = vertex_index(ir.entry);
    if entry_idx < n {
        visited[entry_idx][usize::from(start_unknown)] = true;
        queue.push_back((ir.entry, start_unknown));
    }

    while let Some((u, u_unknown)) = queue.pop_front() {
        if u == target_vid && !u_unknown {
            break;
        }
        let Some(vertex) = ir.vertices.get(vertex_index(u)) else {
            continue;
        };
        for edge in &vertex.explicit_edges {
            if let ExplicitEdgeKind::Branch(branch_id) = edge.kind
                && cut_set.contains(&branch_id)
            {
                continue;
            }
            let next_unknown = u_unknown || is_unknown_vertex(ir, edge.target);
            let next_idx = vertex_index(edge.target);
            let flag_idx = usize::from(next_unknown);
            if next_idx < n && !visited[next_idx][flag_idx] {
                visited[next_idx][flag_idx] = true;
                parent[next_idx][flag_idx] = Some((u, u_unknown, edge));
                queue.push_back((edge.target, next_unknown));
            }
        }
    }

    let target_idx = vertex_index(target_vid);
    let known_bypass = if target_idx < n && visited[target_idx][0] {
        let mut path = Vec::new();
        let mut cursor = (target_vid, false);
        while let Some((prev_v, prev_unknown, edge)) =
            parent[vertex_index(cursor.0)][usize::from(cursor.1)]
        {
            path.push((prev_v, edge));
            cursor = (prev_v, prev_unknown);
        }
        path.reverse();
        Some(path)
    } else {
        None
    };

    let unknown_bypass_reachable = target_idx < n && visited[target_idx][1];

    BypassSearchResult {
        known_bypass,
        unknown_bypass_reachable,
    }
}

fn collect_critical_path_gaps(
    ir: &FunctionIr,
    target_vid: VertexId,
    cut_set: &FxHashSet<BranchId>,
) -> Vec<GapItem> {
    let fwd = reachable_vertices(ir, cut_set);

    let mut reverse_adj: Vec<Vec<VertexId>> = vec![Vec::new(); ir.vertices.len()];
    for vertex in &ir.vertices {
        for edge in &vertex.explicit_edges {
            if let ExplicitEdgeKind::Branch(branch_id) = edge.kind
                && cut_set.contains(&branch_id)
            {
                continue;
            }
            let target_idx = vertex_index(edge.target);
            if target_idx < reverse_adj.len() {
                reverse_adj[target_idx].push(vertex.id);
            }
        }
    }

    let mut bwd = vec![false; ir.vertices.len()];
    let mut queue = VecDeque::new();
    let target_idx = vertex_index(target_vid);
    if target_idx < bwd.len() {
        bwd[target_idx] = true;
        queue.push_back(target_vid);
    }
    while let Some(v) = queue.pop_front() {
        for &prev in &reverse_adj[vertex_index(v)] {
            let prev_idx = vertex_index(prev);
            if !bwd[prev_idx] {
                bwd[prev_idx] = true;
                queue.push_back(prev);
            }
        }
    }

    let mut gaps = Vec::new();
    for (idx, vertex) in ir.vertices.iter().enumerate() {
        if !(fwd[idx] && bwd[idx]) {
            continue;
        }
        if let IrOp::Unknown {
            reason,
            message,
            location,
            ..
        } = &vertex.op
        {
            let gap = GapItem::from_reason(*reason, message.clone(), Some(location.clone()));
            if !gaps
                .iter()
                .any(|existing: &GapItem| existing.kind == gap.kind && existing.at == gap.at)
            {
                gaps.push(gap);
            }
        }
    }
    gaps
}

fn build_bypass_path(
    ir: &FunctionIr,
    witness_edges: &[(VertexId, &ExplicitEdge)],
    target_location: &DiagnosticLocation,
) -> BranchBypassPath {
    let mut steps = Vec::new();
    let mut evidence = Vec::new();

    for &(from_vid, edge) in witness_edges {
        match &edge.kind {
            ExplicitEdgeKind::Branch(branch_id) => {
                if let Some(branch) = ir.branches.get(usize::try_from(branch_id.0).unwrap_or(0)) {
                    let dir = branch.direction.as_str();
                    steps.push(ControlFlowPathStep {
                        location: branch.location.clone(),
                        description: format!("Take {dir} branch of \"{}\"", branch.condition_text),
                    });
                    evidence.push(EvidenceItem::simple(
                        "source",
                        format!(
                            "Bypass path takes {dir} branch of \"{}\".",
                            branch.condition_text
                        ),
                        Some(branch.location.clone()),
                    ));
                }
            }
            ExplicitEdgeKind::SwitchNoMatch { switch_location } => {
                steps.push(ControlFlowPathStep {
                    location: switch_location.clone(),
                    description:
                        "Switch statement has no default clause; unmatched values bypass all cases"
                            .to_string(),
                });
                evidence.push(EvidenceItem::simple(
                    "source",
                    "Switch statement has no default clause; unmatched values bypass all cases."
                        .to_string(),
                    Some(switch_location.clone()),
                ));
            }
            ExplicitEdgeKind::ExplicitThrow => {
                if let Some(loc) = ir
                    .vertices
                    .get(vertex_index(from_vid))
                    .and_then(|v| v.location.as_ref())
                {
                    steps.push(ControlFlowPathStep {
                        location: loc.clone(),
                        description: "Explicit throw transfers control".to_string(),
                    });
                }
            }
            ExplicitEdgeKind::Fallthrough
            | ExplicitEdgeKind::LoopBack
            | ExplicitEdgeKind::Break { .. }
            | ExplicitEdgeKind::Continue { .. }
            | ExplicitEdgeKind::Return
            | ExplicitEdgeKind::ExitMerge => {}
        }
    }

    if evidence.is_empty() {
        steps.push(ControlFlowPathStep {
            location: ir.scope_location.clone(),
            description: "Enter scope directly without passing selected branches".to_string(),
        });
        evidence.push(EvidenceItem::simple(
            "source",
            "Control flow reaches target directly from function entry without passing selected branches.".to_string(),
            Some(ir.scope_location.clone()),
        ));
    }

    steps.push(ControlFlowPathStep {
        location: target_location.clone(),
        description: "Reach target without passing selected branches".to_string(),
    });
    evidence.push(EvidenceItem::simple(
        "source",
        "Target reached along bypass path without passing selected branches.".to_string(),
        Some(target_location.clone()),
    ));

    BranchBypassPath { steps, evidence }
}

fn build_covered_evidence(ir: &FunctionIr, selected_branches: &[BranchId]) -> Vec<EvidenceItem> {
    let mut evidence = Vec::new();
    let mut seen = FxHashSet::default();
    for &branch_id in selected_branches {
        if !seen.insert(branch_id) {
            continue;
        }
        let idx = usize::try_from(branch_id.0).unwrap_or(usize::MAX);
        if let Some(branch) = ir.branches.get(idx) {
            evidence.push(EvidenceItem::simple(
                "source",
                format!(
                    "Path covered via {} branch of \"{}\".",
                    branch.direction.as_str(),
                    branch.condition_text
                ),
                Some(branch.location.clone()),
            ));
        }
    }
    evidence
}

fn gaps_to_evidence(gaps: &[GapItem]) -> Vec<EvidenceItem> {
    gaps.iter()
        .map(|gap| EvidenceItem::simple("note", gap.message.clone(), gap.at.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;
    use tssae_common::{FileId, FunctionIrId, LocalSyntaxId, SyntaxId};

    use super::*;
    use crate::ir::{
        CfgVertex, DeclarationSemantics, ExplicitEdge, IrBranchDirection, IrBranchKind,
        IrBranchRecord, IrScopeKind, LiteralKind,
    };

    fn loc(line: u32) -> DiagnosticLocation {
        DiagnosticLocation {
            file: "src/test.ts".to_string(),
            line,
            column: 1,
            end_line: line,
            end_column: 10,
            start: line * 10,
            end: line * 10 + 5,
        }
    }

    fn syn(n: u32) -> SyntaxId {
        SyntaxId::from_parts(FileId(0), LocalSyntaxId(n))
    }

    fn mk_vertex(id: u32, op: IrOp, edges: Vec<ExplicitEdge>) -> CfgVertex {
        CfgVertex {
            id: VertexId(id),
            syntax: None,
            local_syntax: None,
            symbol: None,
            ty: None,
            signature: None,
            location: None,
            value: None,
            op,
            explicit_edges: edges,
            may_throw_edge: None,
        }
    }

    fn base_ir(
        vertices: Vec<CfgVertex>,
        branches: Vec<IrBranchRecord>,
        syntax_to_vertex: FxHashMap<SyntaxId, VertexId>,
    ) -> FunctionIr {
        let last = u32::try_from(vertices.len() - 1).expect("len fits");
        FunctionIr {
            id: FunctionIrId {
                file: FileId(0),
                local_scope: 0,
            },
            root_syntax: syn(0),
            scope_kind: IrScopeKind::Function,
            scope_location: loc(1),
            decl_semantics: DeclarationSemantics::default(),
            parameters: Vec::new(),
            bindings: Vec::new(),
            vertices,
            entry: VertexId(0),
            normal_exit: VertexId(last - 2),
            exceptional_exit: VertexId(last - 1),
            exit: VertexId(last),
            syntax_to_vertex,
            expr_to_vertex: FxHashMap::default(),
            expr_to_value: FxHashMap::default(),
            branches,
            value_count: 1,
        }
    }

    fn mk_branch(id: BranchId, dir: IrBranchDirection, cond: &str, line: u32) -> IrBranchRecord {
        IrBranchRecord {
            id,
            kind: IrBranchKind::If,
            direction: dir,
            condition_syntax: syn(20 + id.0),
            condition_text: cond.to_string(),
            location: loc(line),
            statement_location: loc(line),
        }
    }

    fn mk_unknown(id: u32, reason: UnknownReason, msg: &str, line: u32, next: u32) -> CfgVertex {
        mk_vertex(
            id,
            IrOp::Unknown {
                reason,
                message: msg.to_string(),
                location: loc(line),
                evaluated_children: Vec::new(),
            },
            vec![ExplicitEdge {
                target: VertexId(next),
                kind: ExplicitEdgeKind::Fallthrough,
            }],
        )
    }

    #[test]
    fn unrelated_unknown_before_guard_or_after_target_does_not_degrade_proved() {
        let b0 = BranchId(0);
        let b1 = BranchId(1);
        let target_syn = syn(10);
        let mut s2v = FxHashMap::default();
        s2v.insert(target_syn, VertexId(3));

        let vertices = vec![
            mk_vertex(
                0,
                IrOp::Entry,
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_unknown(
                1,
                UnknownReason::UnmodeledLoop,
                "Unmodeled loop before guard",
                2,
                2,
            ),
            mk_vertex(
                2,
                IrOp::Branch {
                    condition: tssae_common::ValueId(0),
                },
                vec![
                    ExplicitEdge {
                        target: VertexId(3),
                        kind: ExplicitEdgeKind::Branch(b0),
                    },
                    ExplicitEdge {
                        target: VertexId(5),
                        kind: ExplicitEdgeKind::Branch(b1),
                    },
                ],
            ),
            mk_vertex(
                3,
                IrOp::Literal {
                    kind: LiteralKind::Undefined,
                    as_const: false,
                },
                vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_unknown(
                4,
                UnknownReason::WithStatement,
                "With statement after target",
                5,
                5,
            ),
            mk_vertex(
                5,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(7),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(
                6,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(7),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(7, IrOp::Exit, Vec::new()),
        ];

        let branches = vec![
            mk_branch(b0, IrBranchDirection::True, "ok", 3),
            mk_branch(b1, IrBranchDirection::False, "ok", 3),
        ];

        let ir = base_ir(vertices, branches, s2v);
        ir.validate_invariants().expect("valid CFG invariants");

        let outcome = evaluate_branch_coverage(&ir, Some(target_syn), &loc(4), &[b0], &[]);
        assert!(matches!(
            outcome,
            BranchCoverageOutcome::Proved {
                target_reachable: true,
                ..
            }
        ));
    }

    #[test]
    fn known_bypass_wins_over_parallel_unknown_path_and_critical_gaps_exclude_off_path_unknowns() {
        let b0 = BranchId(0);
        let b1 = BranchId(1);
        let b2 = BranchId(2);
        let target_syn = syn(10);
        let mut s2v = FxHashMap::default();
        s2v.insert(target_syn, VertexId(4));

        let vertices = vec![
            mk_vertex(
                0,
                IrOp::Entry,
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                1,
                IrOp::Branch {
                    condition: tssae_common::ValueId(0),
                },
                vec![
                    ExplicitEdge {
                        target: VertexId(2),
                        kind: ExplicitEdgeKind::Branch(b0),
                    },
                    ExplicitEdge {
                        target: VertexId(3),
                        kind: ExplicitEdgeKind::Branch(b1),
                    },
                    ExplicitEdge {
                        target: VertexId(5),
                        kind: ExplicitEdgeKind::Branch(b2),
                    },
                ],
            ),
            mk_unknown(2, UnknownReason::UnmodeledLoop, "Critical loop", 2, 4),
            mk_vertex(
                3,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(4),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_vertex(
                4,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(6),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            mk_unknown(5, UnknownReason::WithStatement, "Off-path with", 9, 6),
            mk_vertex(
                6,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(8),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(
                7,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(8),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(8, IrOp::Exit, Vec::new()),
        ];

        let branches = vec![
            mk_branch(b0, IrBranchDirection::True, "a", 2),
            mk_branch(b1, IrBranchDirection::False, "a", 3),
            mk_branch(b2, IrBranchDirection::False, "off", 9),
        ];

        let ir = base_ir(vertices, branches, s2v);
        ir.validate_invariants().expect("valid CFG invariants");

        let disproved = evaluate_branch_coverage(&ir, Some(target_syn), &loc(4), &[], &[]);
        assert!(matches!(
            disproved,
            BranchCoverageOutcome::Disproved {
                target_reachable: true,
                ..
            }
        ));

        let inconclusive = evaluate_branch_coverage(&ir, Some(target_syn), &loc(4), &[b1], &[]);
        match inconclusive {
            BranchCoverageOutcome::Inconclusive {
                target_reachable,
                gaps,
                ..
            } => {
                assert!(target_reachable);
                assert_eq!(gaps.len(), 1);
                assert_eq!(gaps[0].kind, "unmodeled-loop");
                assert_eq!(gaps[0].at, Some(loc(2)));
            }
            other => panic!("expected Inconclusive, got {other:?}"),
        }
    }
}
