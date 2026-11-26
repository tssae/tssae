use std::collections::VecDeque;

use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use tssae_common::{
    BranchId, DiagnosticLocation, EvidenceItem, FinallyId, GapItem, SyntaxId, UnknownReason,
    VertexId,
};

use crate::ir::{ExplicitEdge, ExplicitEdgeKind, FunctionIr, IrOp, MayThrowEdge, MayThrowReason};

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

/// Independent tri-state outcome evaluating implicit `mayThrow` exception paths.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ImplicitExceptionCoverageOutcome {
    #[serde(rename_all = "camelCase")]
    Proved { evidence: Vec<EvidenceItem> },
    #[serde(rename_all = "camelCase")]
    Disproved {
        may_throw_sites: Vec<DiagnosticLocation>,
        counterexample_path: BranchBypassPath,
        evidence: Vec<EvidenceItem>,
    },
    #[serde(rename_all = "camelCase")]
    Inconclusive {
        may_throw_sites: Vec<DiagnosticLocation>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

/// Tri-state outcome produced by evaluating branch coverage over a [`FunctionIr`] CFG.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BranchCoverageOutcome {
    Proved {
        target_reachable: bool,
        implicit_exceptions: ImplicitExceptionCoverageOutcome,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        target_reachable: bool,
        implicit_exceptions: ImplicitExceptionCoverageOutcome,
        counterexample_path: BranchBypassPath,
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        target_reachable: bool,
        implicit_exceptions: ImplicitExceptionCoverageOutcome,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

/// Evaluates explicit branch coverage and implicit exception bypass coverage over a [`FunctionIr`] CFG.
///
/// Main explicit path exploration follows only `ExplicitEdge`s (including explicit `throw`):
/// - If `target_syntax` is unreachable in modeled explicit control flow, main outcome is
///   `Proved { target_reachable: false }`.
/// - If a known explicit bypass path (zero `Unknown` vertices) reaches the target, main outcome is
///   `Disproved` with that concrete `counterexample_path`.
/// - If all explicit bypass paths pass through at least one `Unknown` vertex, main outcome is
///   `Inconclusive` reporting ONLY `Unknown` vertices on actual bypass paths (`fwd && bwd`).
/// - If no explicit path bypasses `selected_branches`, main outcome is `Proved { target_reachable: true }`.
///
/// Independently, `implicit_exceptions` evaluates whether any path containing at least one
/// `MayThrowEdge` can reach `target_syntax` without passing `selected_branches`.
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
        let evidence = gaps_to_evidence(&combined_gaps);
        return BranchCoverageOutcome::Inconclusive {
            target_reachable: false,
            implicit_exceptions: ImplicitExceptionCoverageOutcome::Inconclusive {
                may_throw_sites: Vec::new(),
                gaps: combined_gaps.clone(),
                evidence: evidence.clone(),
            },
            gaps: combined_gaps,
            evidence,
        };
    };

    let reachable_all = reachable_explicit_vertices(ir, &FxHashSet::default());
    let target_idx = vertex_index(target_vid);
    let target_reachable = reachable_all.get(target_idx).copied().unwrap_or(false);

    if !combined_gaps.is_empty() {
        let evidence = gaps_to_evidence(&combined_gaps);
        return BranchCoverageOutcome::Inconclusive {
            target_reachable,
            implicit_exceptions: ImplicitExceptionCoverageOutcome::Inconclusive {
                may_throw_sites: Vec::new(),
                gaps: combined_gaps.clone(),
                evidence: evidence.clone(),
            },
            gaps: combined_gaps,
            evidence,
        };
    }

    let cut_set: FxHashSet<BranchId> = selected_branches
        .iter()
        .copied()
        .filter(|id| usize::try_from(id.0).is_ok_and(|idx| idx < ir.branches.len()))
        .collect();

    let graph = explore_bypass_graph(ir, target_vid, &cut_set);
    let implicit_exceptions = evaluate_implicit_outcome(ir, &graph, target_location);

    if !target_reachable {
        return BranchCoverageOutcome::Proved {
            target_reachable: false,
            implicit_exceptions,
            evidence: vec![EvidenceItem::simple(
                "source",
                "Target is unreachable from function entry in modeled control flow.".to_string(),
                Some(target_location.clone()),
            )],
        };
    }

    if let Some(witness_edges) = graph.known_explicit_witness() {
        let counterexample_path = build_bypass_path(ir, &witness_edges, target_location);
        let evidence = counterexample_path.evidence.clone();
        return BranchCoverageOutcome::Disproved {
            target_reachable: true,
            implicit_exceptions,
            counterexample_path,
            evidence,
        };
    }

    if !graph.unknown_explicit_targets.is_empty() {
        let (gaps, _) =
            collect_critical_path_artifacts(ir, &graph, &graph.unknown_explicit_targets, false);
        let evidence = gaps_to_evidence(&gaps);
        return BranchCoverageOutcome::Inconclusive {
            target_reachable: true,
            implicit_exceptions,
            gaps,
            evidence,
        };
    }

    let evidence = build_covered_evidence(ir, selected_branches);
    BranchCoverageOutcome::Proved {
        target_reachable: true,
        implicit_exceptions,
        evidence,
    }
}

fn evaluate_implicit_outcome(
    ir: &FunctionIr,
    graph: &BypassGraph<'_>,
    target_location: &DiagnosticLocation,
) -> ImplicitExceptionCoverageOutcome {
    if let Some(witness_edges) = graph.known_implicit_witness() {
        let counterexample_path = build_bypass_path(ir, &witness_edges, target_location);
        let (_, mut may_throw_sites) =
            collect_critical_path_artifacts(ir, graph, &graph.known_implicit_targets, true);
        if may_throw_sites.is_empty() {
            for &(from_vid, edge) in &witness_edges {
                if matches!(edge, WitnessEdge::MayThrow(_))
                    && let Some(loc) = vertex_location(ir, from_vid)
                {
                    push_unique_location(&mut may_throw_sites, loc);
                }
            }
        }
        let mut evidence = Vec::new();
        for site_loc in &may_throw_sites {
            evidence.push(EvidenceItem::simple(
                "source",
                "Potential implicit exception site can bypass selected branches and reach target."
                    .to_string(),
                Some(site_loc.clone()),
            ));
        }
        for ev in &counterexample_path.evidence {
            if !evidence.contains(ev) {
                evidence.push(ev.clone());
            }
        }
        return ImplicitExceptionCoverageOutcome::Disproved {
            may_throw_sites,
            counterexample_path,
            evidence,
        };
    }

    if !graph.unknown_implicit_targets.is_empty() {
        let (gaps, may_throw_sites) =
            collect_critical_path_artifacts(ir, graph, &graph.unknown_implicit_targets, false);
        let mut evidence = Vec::new();
        for site_loc in &may_throw_sites {
            evidence.push(EvidenceItem::simple(
                "source",
                "Potential implicit exception site lies on an inconclusive bypass path."
                    .to_string(),
                Some(site_loc.clone()),
            ));
        }
        evidence.extend(gaps_to_evidence(&gaps));
        return ImplicitExceptionCoverageOutcome::Inconclusive {
            may_throw_sites,
            gaps,
            evidence,
        };
    }

    ImplicitExceptionCoverageOutcome::Proved {
        evidence: vec![EvidenceItem::simple(
            "source",
            "No implicit exception path can bypass selected branches to reach target.".to_string(),
            Some(target_location.clone()),
        )],
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

fn vertex_location(ir: &FunctionIr, vid: VertexId) -> Option<DiagnosticLocation> {
    ir.vertices
        .get(vertex_index(vid))
        .and_then(|v| v.location.clone())
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct StackId(usize);

type CompletionStack = Vec<(FinallyId, VertexId)>;

#[derive(Default)]
struct StackArena {
    stacks: Vec<CompletionStack>,
    intern: FxHashMap<CompletionStack, StackId>,
}

impl StackArena {
    fn new() -> Self {
        let mut arena = Self::default();
        arena.intern(Vec::new());
        arena
    }

    fn intern(&mut self, stack: CompletionStack) -> StackId {
        if let Some(&id) = self.intern.get(&stack) {
            return id;
        }
        let id = StackId(self.stacks.len());
        self.stacks.push(stack.clone());
        self.intern.insert(stack, id);
        id
    }

    fn transition_explicit(
        &mut self,
        ir: &FunctionIr,
        stack_id: StackId,
        edge: &ExplicitEdge,
    ) -> Option<StackId> {
        let mut next_stack = self.stacks[stack_id.0].clone();
        match edge.kind {
            ExplicitEdgeKind::FinallyResume { finally_id, resume } => {
                if let Some(pos) = next_stack.iter().rposition(|&(fid, _)| fid == finally_id) {
                    if next_stack[pos].1 != resume {
                        return None;
                    }
                    next_stack.truncate(pos);
                }
            }
            ExplicitEdgeKind::FinallyEnter { finally_id, resume } => {
                if let Some(pos) = next_stack.iter().position(|&(fid, _)| fid == finally_id) {
                    next_stack.truncate(pos);
                }
                next_stack.push((finally_id, resume));
            }
            ExplicitEdgeKind::Fallthrough
            | ExplicitEdgeKind::Branch(_)
            | ExplicitEdgeKind::SwitchNoMatch { .. }
            | ExplicitEdgeKind::LoopBack
            | ExplicitEdgeKind::Break { .. }
            | ExplicitEdgeKind::Continue { .. }
            | ExplicitEdgeKind::Return
            | ExplicitEdgeKind::ExplicitThrow
            | ExplicitEdgeKind::ExitMerge => {}
        }
        retain_enclosing_finallies(ir, edge.target, &mut next_stack);
        Some(self.intern(next_stack))
    }

    fn transition_may_throw(
        &mut self,
        ir: &FunctionIr,
        stack_id: StackId,
        target: VertexId,
    ) -> StackId {
        let mut next_stack = self.stacks[stack_id.0].clone();
        retain_enclosing_finallies(ir, target, &mut next_stack);
        self.intern(next_stack)
    }
}

fn retain_enclosing_finallies(ir: &FunctionIr, target: VertexId, stack: &mut CompletionStack) {
    if stack.is_empty() {
        return;
    }
    let Some(target_vertex) = ir.vertices.get(vertex_index(target)) else {
        stack.clear();
        return;
    };
    if target_vertex.enclosing_finallies.is_empty() {
        stack.clear();
    } else {
        stack.retain(|(fid, _)| target_vertex.enclosing_finallies.contains(fid));
    }
}

fn reachable_explicit_vertices(ir: &FunctionIr, cut_set: &FxHashSet<BranchId>) -> Vec<bool> {
    let mut visited_vertices = vec![false; ir.vertices.len()];
    let mut visited_states: FxHashSet<(VertexId, StackId)> = FxHashSet::default();
    let mut stacks = StackArena::new();
    let mut queue = VecDeque::new();

    let empty_stack = StackId(0);
    if vertex_index(ir.entry) < visited_vertices.len() {
        visited_vertices[vertex_index(ir.entry)] = true;
        visited_states.insert((ir.entry, empty_stack));
        queue.push_back((ir.entry, empty_stack));
    }

    while let Some((u, stack_id)) = queue.pop_front() {
        let Some(vertex) = ir.vertices.get(vertex_index(u)) else {
            continue;
        };
        for edge in &vertex.explicit_edges {
            if let ExplicitEdgeKind::Branch(branch_id) = edge.kind
                && cut_set.contains(&branch_id)
            {
                continue;
            }
            let Some(next_stack) = stacks.transition_explicit(ir, stack_id, edge) else {
                continue;
            };
            let next_idx = vertex_index(edge.target);
            if next_idx < visited_vertices.len() {
                visited_vertices[next_idx] = true;
                if visited_states.insert((edge.target, next_stack)) {
                    queue.push_back((edge.target, next_stack));
                }
            }
        }
    }

    visited_vertices
}

#[derive(Clone, Copy)]
enum WitnessEdge<'a> {
    Explicit(&'a ExplicitEdge),
    MayThrow(MayThrowEdge),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SearchKey {
    vid: VertexId,
    stack: StackId,
    used_may_throw: bool,
    has_unknown: bool,
}

struct SearchNode<'a> {
    key: SearchKey,
    parent: Option<(usize, WitnessEdge<'a>)>,
}

struct ReverseTransition<'a> {
    from_state: usize,
    edge: WitnessEdge<'a>,
}

struct BypassGraph<'a> {
    nodes: Vec<SearchNode<'a>>,
    rev_adj: Vec<Vec<ReverseTransition<'a>>>,
    known_explicit_target: Option<usize>,
    unknown_explicit_targets: Vec<usize>,
    known_implicit_targets: Vec<usize>,
    unknown_implicit_targets: Vec<usize>,
}

impl<'a> BypassGraph<'a> {
    fn reconstruct_witness(&self, mut state_idx: usize) -> Vec<(VertexId, WitnessEdge<'a>)> {
        let mut path = Vec::new();
        while let Some((prev_idx, edge)) = self.nodes[state_idx].parent {
            path.push((self.nodes[prev_idx].key.vid, edge));
            state_idx = prev_idx;
        }
        path.reverse();
        path
    }

    fn known_explicit_witness(&self) -> Option<Vec<(VertexId, WitnessEdge<'a>)>> {
        self.known_explicit_target
            .map(|idx| self.reconstruct_witness(idx))
    }

    fn known_implicit_witness(&self) -> Option<Vec<(VertexId, WitnessEdge<'a>)>> {
        self.known_implicit_targets
            .first()
            .copied()
            .map(|idx| self.reconstruct_witness(idx))
    }
}

struct BypassGraphBuilder<'a> {
    target_vid: VertexId,
    stacks: StackArena,
    key_to_idx: FxHashMap<SearchKey, usize>,
    nodes: Vec<SearchNode<'a>>,
    rev_adj: Vec<Vec<ReverseTransition<'a>>>,
    queue: VecDeque<usize>,
    known_explicit_target: Option<usize>,
    unknown_explicit_targets: Vec<usize>,
    known_implicit_targets: Vec<usize>,
    unknown_implicit_targets: Vec<usize>,
}

impl<'a> BypassGraphBuilder<'a> {
    fn new(target_vid: VertexId) -> Self {
        Self {
            target_vid,
            stacks: StackArena::new(),
            key_to_idx: FxHashMap::default(),
            nodes: Vec::new(),
            rev_adj: Vec::new(),
            queue: VecDeque::new(),
            known_explicit_target: None,
            unknown_explicit_targets: Vec::new(),
            known_implicit_targets: Vec::new(),
            unknown_implicit_targets: Vec::new(),
        }
    }

    fn record_target(&mut self, key: SearchKey, idx: usize) {
        if key.vid != self.target_vid {
            return;
        }
        match (key.used_may_throw, key.has_unknown) {
            (false, false) => {
                if self.known_explicit_target.is_none() {
                    self.known_explicit_target = Some(idx);
                }
            }
            (false, true) => self.unknown_explicit_targets.push(idx),
            (true, false) => self.known_implicit_targets.push(idx),
            (true, true) => self.unknown_implicit_targets.push(idx),
        }
    }

    fn push_edge(&mut self, from_idx: usize, next_key: SearchKey, witness: WitnessEdge<'a>) {
        let next_idx = if let Some(&existing) = self.key_to_idx.get(&next_key) {
            existing
        } else {
            let new_idx = self.nodes.len();
            self.key_to_idx.insert(next_key, new_idx);
            self.nodes.push(SearchNode {
                key: next_key,
                parent: Some((from_idx, witness)),
            });
            self.rev_adj.push(Vec::new());
            self.record_target(next_key, new_idx);
            self.queue.push_back(new_idx);
            new_idx
        };
        self.rev_adj[next_idx].push(ReverseTransition {
            from_state: from_idx,
            edge: witness,
        });
    }
}

fn explore_bypass_graph<'a>(
    ir: &'a FunctionIr,
    target_vid: VertexId,
    cut_set: &FxHashSet<BranchId>,
) -> BypassGraph<'a> {
    let mut builder = BypassGraphBuilder::new(target_vid);

    let start_key = SearchKey {
        vid: ir.entry,
        stack: StackId(0),
        used_may_throw: false,
        has_unknown: is_unknown_vertex(ir, ir.entry),
    };
    if vertex_index(ir.entry) < ir.vertices.len() {
        builder.key_to_idx.insert(start_key, 0);
        builder.nodes.push(SearchNode {
            key: start_key,
            parent: None,
        });
        builder.rev_adj.push(Vec::new());
        builder.record_target(start_key, 0);
        builder.queue.push_back(0);
    }

    while let Some(u_idx) = builder.queue.pop_front() {
        let u_key = builder.nodes[u_idx].key;
        let Some(vertex) = ir.vertices.get(vertex_index(u_key.vid)) else {
            continue;
        };

        for edge in &vertex.explicit_edges {
            if let ExplicitEdgeKind::Branch(branch_id) = edge.kind
                && cut_set.contains(&branch_id)
            {
                continue;
            }
            if vertex_index(edge.target) >= ir.vertices.len() {
                continue;
            }
            let Some(next_stack) = builder.stacks.transition_explicit(ir, u_key.stack, edge) else {
                continue;
            };
            let next_key = SearchKey {
                vid: edge.target,
                stack: next_stack,
                used_may_throw: u_key.used_may_throw,
                has_unknown: u_key.has_unknown || is_unknown_vertex(ir, edge.target),
            };
            builder.push_edge(u_idx, next_key, WitnessEdge::Explicit(edge));
        }

        if let Some(may_throw) = vertex.may_throw_edge
            && vertex_index(may_throw.target) < ir.vertices.len()
        {
            let next_stack = builder
                .stacks
                .transition_may_throw(ir, u_key.stack, may_throw.target);
            let next_key = SearchKey {
                vid: may_throw.target,
                stack: next_stack,
                used_may_throw: true,
                has_unknown: u_key.has_unknown || is_unknown_vertex(ir, may_throw.target),
            };
            builder.push_edge(u_idx, next_key, WitnessEdge::MayThrow(may_throw));
        }
    }

    BypassGraph {
        nodes: builder.nodes,
        rev_adj: builder.rev_adj,
        known_explicit_target: builder.known_explicit_target,
        unknown_explicit_targets: builder.unknown_explicit_targets,
        known_implicit_targets: builder.known_implicit_targets,
        unknown_implicit_targets: builder.unknown_implicit_targets,
    }
}

fn collect_critical_path_artifacts(
    ir: &FunctionIr,
    graph: &BypassGraph<'_>,
    target_states: &[usize],
    known_only: bool,
) -> (Vec<GapItem>, Vec<DiagnosticLocation>) {
    let mut bwd = vec![false; graph.nodes.len()];
    let mut queue = VecDeque::new();
    for &idx in target_states {
        if !bwd[idx] {
            bwd[idx] = true;
            queue.push_back(idx);
        }
    }

    let mut may_throw_sites = Vec::new();
    while let Some(state_idx) = queue.pop_front() {
        for rev in &graph.rev_adj[state_idx] {
            if known_only && graph.nodes[rev.from_state].key.has_unknown {
                continue;
            }
            if matches!(rev.edge, WitnessEdge::MayThrow(_))
                && let Some(loc) = vertex_location(ir, graph.nodes[rev.from_state].key.vid)
            {
                push_unique_location(&mut may_throw_sites, loc);
            }
            if !bwd[rev.from_state] {
                bwd[rev.from_state] = true;
                queue.push_back(rev.from_state);
            }
        }
    }

    let mut gaps = Vec::new();
    for (state_idx, is_critical) in bwd.iter().enumerate() {
        if !is_critical {
            continue;
        }
        let vid = graph.nodes[state_idx].key.vid;
        let Some(vertex) = ir.vertices.get(vertex_index(vid)) else {
            continue;
        };
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

    (gaps, may_throw_sites)
}

fn push_unique_location(out: &mut Vec<DiagnosticLocation>, loc: DiagnosticLocation) {
    if !out.contains(&loc) {
        out.push(loc);
    }
}

const fn may_throw_description(reason: MayThrowReason) -> &'static str {
    match reason {
        MayThrowReason::Call => "Call expression may throw an implicit exception",
        MayThrowReason::Construct => "Constructor call may throw an implicit exception",
        MayThrowReason::PropertyAccess => "Property access may throw an implicit exception",
        MayThrowReason::Await => "Await expression may reject and throw an implicit exception",
        MayThrowReason::Yield => "Yield delegation may throw an implicit exception",
        MayThrowReason::DynamicOperator => "Dynamic operator may throw an implicit exception",
        MayThrowReason::IteratorStep => "Iteration step may throw an implicit exception",
    }
}

fn append_witness_step(
    ir: &FunctionIr,
    from_vid: VertexId,
    edge: WitnessEdge<'_>,
    steps: &mut Vec<ControlFlowPathStep>,
    evidence: &mut Vec<EvidenceItem>,
) {
    match edge {
        WitnessEdge::Explicit(explicit) => match &explicit.kind {
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
                if let Some(loc) = vertex_location(ir, from_vid) {
                    steps.push(ControlFlowPathStep {
                        location: loc.clone(),
                        description: "Explicit throw transfers control".to_string(),
                    });
                    evidence.push(EvidenceItem::simple(
                        "source",
                        "Explicit throw transfers control along bypass path.".to_string(),
                        Some(loc),
                    ));
                }
            }
            ExplicitEdgeKind::Break { label } => {
                if let Some(loc) = vertex_location(ir, from_vid) {
                    let desc = label.as_ref().map_or_else(
                        || "Break transfers control".to_string(),
                        |lbl| format!("Break to label \"{lbl}\" transfers control"),
                    );
                    steps.push(ControlFlowPathStep {
                        location: loc,
                        description: format!("{desc}."),
                    });
                }
            }
            ExplicitEdgeKind::Continue { label } => {
                if let Some(loc) = vertex_location(ir, from_vid) {
                    let desc = label.as_ref().map_or_else(
                        || "Continue jumps to loop step".to_string(),
                        |lbl| format!("Continue to label \"{lbl}\" jumps to loop step"),
                    );
                    steps.push(ControlFlowPathStep {
                        location: loc,
                        description: desc,
                    });
                }
            }
            ExplicitEdgeKind::Fallthrough
            | ExplicitEdgeKind::LoopBack
            | ExplicitEdgeKind::Return
            | ExplicitEdgeKind::FinallyEnter { .. }
            | ExplicitEdgeKind::FinallyResume { .. }
            | ExplicitEdgeKind::ExitMerge => {}
        },
        WitnessEdge::MayThrow(may_throw) => {
            if let Some(loc) = vertex_location(ir, from_vid) {
                let desc = may_throw_description(may_throw.reason);
                steps.push(ControlFlowPathStep {
                    location: loc.clone(),
                    description: desc.to_string(),
                });
                evidence.push(EvidenceItem::simple(
                    "source",
                    format!("{desc}."),
                    Some(loc),
                ));
            }
        }
    }
}

fn build_bypass_path(
    ir: &FunctionIr,
    witness_edges: &[(VertexId, WitnessEdge<'_>)],
    target_location: &DiagnosticLocation,
) -> BranchBypassPath {
    let mut steps = Vec::new();
    let mut evidence = Vec::new();

    for &(from_vid, edge) in witness_edges {
        append_witness_step(ir, from_vid, edge, &mut steps, &mut evidence);
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
        IrBranchRecord, IrCallArg, IrCallKind, IrScopeKind, LiteralKind,
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
            enclosing_finallies: Vec::new(),
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
            type_guard: None,
            parameters: Vec::new(),
            bindings: Vec::new(),
            captured_bindings: Vec::new(),
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
                UnknownReason::WithStatement,
                "Unknown construct before guard",
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
                implicit_exceptions: ImplicitExceptionCoverageOutcome::Proved { .. },
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
            mk_unknown(2, UnknownReason::WithStatement, "Critical with", 2, 4),
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
                assert_eq!(gaps[0].kind, "with-statement");
                assert_eq!(gaps[0].at, Some(loc(2)));
            }
            other => panic!("expected Inconclusive, got {other:?}"),
        }
    }

    #[test]
    fn separates_explicit_unreachability_from_implicit_may_throw_bypass() {
        let target_syn = syn(10);
        let call_syn = syn(11);
        let mut s2v = FxHashMap::default();
        s2v.insert(target_syn, VertexId(2));

        let mut call_v = mk_vertex(
            1,
            IrOp::Call {
                call_kind: IrCallKind::Explicit,
                callee: tssae_common::ValueId(0),
                receiver: None,
                args: vec![IrCallArg::Positional(tssae_common::ValueId(0))],
            },
            vec![ExplicitEdge {
                target: VertexId(3),
                kind: ExplicitEdgeKind::Fallthrough,
            }],
        );
        call_v.location = Some(loc(3));
        call_v.syntax = Some(call_syn);
        call_v.may_throw_edge = Some(MayThrowEdge {
            target: VertexId(2),
            site_syntax: call_syn,
            reason: MayThrowReason::Call,
        });

        let mut target_v = mk_vertex(
            2,
            IrOp::NopMerge,
            vec![ExplicitEdge {
                target: VertexId(3),
                kind: ExplicitEdgeKind::Fallthrough,
            }],
        );
        target_v.location = Some(loc(5));

        let vertices = vec![
            mk_vertex(
                0,
                IrOp::Entry,
                vec![ExplicitEdge {
                    target: VertexId(1),
                    kind: ExplicitEdgeKind::Fallthrough,
                }],
            ),
            call_v,
            target_v,
            mk_vertex(
                3,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(5),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(
                4,
                IrOp::NopMerge,
                vec![ExplicitEdge {
                    target: VertexId(5),
                    kind: ExplicitEdgeKind::ExitMerge,
                }],
            ),
            mk_vertex(5, IrOp::Exit, Vec::new()),
        ];

        let ir = base_ir(vertices, Vec::new(), s2v);
        ir.validate_invariants().expect("valid CFG invariants");

        let outcome = evaluate_branch_coverage(&ir, Some(target_syn), &loc(5), &[], &[]);
        match outcome {
            BranchCoverageOutcome::Proved {
                target_reachable,
                implicit_exceptions,
                ..
            } => {
                assert!(!target_reachable);
                match implicit_exceptions {
                    ImplicitExceptionCoverageOutcome::Disproved {
                        may_throw_sites,
                        counterexample_path,
                        ..
                    } => {
                        assert_eq!(may_throw_sites, vec![loc(3)]);
                        assert!(counterexample_path.steps.len() >= 2);
                    }
                    other => panic!("expected Disproved implicit exceptions, got {other:?}"),
                }
            }
            other => panic!("expected Proved main outcome with unreachable catch, got {other:?}"),
        }
    }

    fn mk_finally_vertex(id: u32, f0: FinallyId, edges: Vec<ExplicitEdge>) -> CfgVertex {
        let mut v = mk_vertex(id, IrOp::NopMerge, edges);
        v.enclosing_finallies = vec![f0];
        v
    }

    #[test]
    fn finally_completion_stack_routes_return_and_normal_completions_separately() {
        let b0 = BranchId(0);
        let b1 = BranchId(1);
        let f0 = FinallyId(0);
        let target_syn = syn(10);
        let mut s2v = FxHashMap::default();
        s2v.insert(target_syn, VertexId(6));

        let edge = |target: u32, kind: ExplicitEdgeKind| ExplicitEdge {
            target: VertexId(target),
            kind,
        };
        let vertices = vec![
            mk_vertex(0, IrOp::Entry, vec![edge(1, ExplicitEdgeKind::Fallthrough)]),
            mk_vertex(
                1,
                IrOp::Branch {
                    condition: tssae_common::ValueId(0),
                },
                vec![
                    edge(2, ExplicitEdgeKind::Branch(b0)),
                    edge(3, ExplicitEdgeKind::Branch(b1)),
                ],
            ),
            mk_vertex(
                2,
                IrOp::Return { value: None },
                vec![edge(
                    4,
                    ExplicitEdgeKind::FinallyEnter {
                        finally_id: f0,
                        resume: VertexId(7),
                    },
                )],
            ),
            mk_vertex(
                3,
                IrOp::NopMerge,
                vec![edge(
                    4,
                    ExplicitEdgeKind::FinallyEnter {
                        finally_id: f0,
                        resume: VertexId(6),
                    },
                )],
            ),
            mk_finally_vertex(4, f0, vec![edge(5, ExplicitEdgeKind::Fallthrough)]),
            mk_finally_vertex(
                5,
                f0,
                vec![
                    edge(
                        7,
                        ExplicitEdgeKind::FinallyResume {
                            finally_id: f0,
                            resume: VertexId(7),
                        },
                    ),
                    edge(
                        6,
                        ExplicitEdgeKind::FinallyResume {
                            finally_id: f0,
                            resume: VertexId(6),
                        },
                    ),
                ],
            ),
            mk_vertex(
                6,
                IrOp::NopMerge,
                vec![edge(7, ExplicitEdgeKind::Fallthrough)],
            ),
            mk_vertex(
                7,
                IrOp::NopMerge,
                vec![edge(9, ExplicitEdgeKind::ExitMerge)],
            ),
            mk_vertex(
                8,
                IrOp::NopMerge,
                vec![edge(9, ExplicitEdgeKind::ExitMerge)],
            ),
            mk_vertex(9, IrOp::Exit, Vec::new()),
        ];

        let branches = vec![
            mk_branch(b0, IrBranchDirection::True, "!ok", 2),
            mk_branch(b1, IrBranchDirection::False, "!ok", 2),
        ];

        let ir = base_ir(vertices, branches, s2v);
        ir.validate_invariants().expect("valid CFG invariants");

        let outcome = evaluate_branch_coverage(&ir, Some(target_syn), &loc(6), &[b1], &[]);
        assert!(matches!(
            outcome,
            BranchCoverageOutcome::Proved {
                target_reachable: true,
                implicit_exceptions: ImplicitExceptionCoverageOutcome::Proved { .. },
                ..
            }
        ));
    }
}
