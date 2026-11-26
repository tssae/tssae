use std::collections::VecDeque;

use rustc_hash::{FxHashMap, FxHashSet};
use tsrs_ast::{Kind, Node, SourceFile, is_function_like_declaration, is_type_node};
use tsrs_core::P;

use crate::serve::{SourceLocation, node_location, slice_source_text};
pub use tssae_common::{EvidenceItem, GapItem};

#[derive(Clone)]
pub struct BranchDescriptor {
    pub kind: &'static str,
    pub direction: &'static str,
    pub condition_node: P<Node>,
    pub condition_text: String,
    pub location: SourceLocation,
    pub statement_location: SourceLocation,
}

#[derive(Clone, Copy)]
pub enum CoverageStatus {
    Covered,
    Bypass,
    Unreachable,
    Unknown,
}

impl CoverageStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Covered => "covered",
            Self::Bypass => "bypass",
            Self::Unreachable => "unreachable",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CleanupCfgStatus {
    Covered,
    Partial,
    NotCleaned,
    Unknown,
}

pub struct CleanupCfgOutcome {
    pub status: CleanupCfgStatus,
    pub cleaned_on_all_paths: bool,
    pub cleaned_on_any_path: bool,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

pub struct CoverageOutcome {
    pub status: CoverageStatus,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

#[derive(Clone, Copy)]
enum EdgeKind {
    Ordinary,
    Branch(usize),
    SwitchNoMatch(usize),
}

pub struct FunctionCfg {
    fn_location: SourceLocation,
    entry: usize,
    successors: Vec<Vec<(usize, EdgeKind)>>,
    ast_to_cfg: FxHashMap<P<Node>, usize>,
    pub branches: Vec<BranchDescriptor>,
    switch_locations: Vec<SourceLocation>,
    pub gaps: Vec<GapItem>,
}

pub fn resolve_enclosing_function(node: P<Node>) -> Option<P<Node>> {
    let mut current = node.parent();
    while let Some(candidate) = current {
        if is_function_like_declaration(Some(candidate)) {
            return Some(candidate);
        }
        if matches!(
            candidate.kind(),
            Kind::ClassDeclaration | Kind::ClassExpression
        ) {
            return None;
        }
        current = candidate.parent();
    }
    None
}

pub fn normalize_function_node(node: P<Node>) -> Option<P<Node>> {
    if is_function_like_declaration(Some(node)) {
        return Some(node);
    }
    if node.kind() == Kind::Block
        && let Some(parent) = node.parent()
        && is_function_like_declaration(Some(parent))
        && parent.body() == Some(node)
    {
        return Some(parent);
    }
    None
}

pub fn normalize_cfg_scope_node(node: P<Node>) -> Option<P<Node>> {
    if let Some(fn_node) = normalize_function_node(node) {
        return Some(fn_node);
    }
    if node.kind() == Kind::Block || tsrs_ast::is_statement(node) {
        return Some(node);
    }
    None
}

impl FunctionCfg {
    pub fn build(sf: P<SourceFile>, rel_file: &str, fn_node: P<Node>) -> Result<Self, GapItem> {
        let fn_location = node_location(sf, rel_file, fn_node);
        let Some(body) = fn_node.body() else {
            return Err(GapItem {
                kind: "missing-function-body",
                message: "Function declaration has no body to analyze.".to_string(),
                at: Some(fn_location),
            });
        };

        let mut builder = CfgBuilder {
            sf,
            rel_file,
            successors: vec![Vec::new()], // vertex 0 is @exit
            ast_to_cfg: FxHashMap::default(),
            branches: Vec::new(),
            switch_locations: Vec::new(),
            gaps: Vec::new(),
        };

        let mut entry = if body.kind() == Kind::Block {
            builder.stmt(body, 0, None, None)
        } else {
            builder.expr_val(body, 0)
        };

        for &param in fn_node.parameters().iter().rev() {
            if let Some(init) = param.initializer() {
                entry = builder.expr_val(init, entry);
            }
        }

        Ok(Self {
            fn_location,
            entry,
            successors: builder.successors,
            ast_to_cfg: builder.ast_to_cfg,
            branches: builder.branches,
            switch_locations: builder.switch_locations,
            gaps: builder.gaps,
        })
    }

    pub fn build_scope(
        sf: P<SourceFile>,
        rel_file: &str,
        scope_node: P<Node>,
    ) -> Result<Self, GapItem> {
        if let Some(fn_node) = normalize_function_node(scope_node) {
            return Self::build(sf, rel_file, fn_node);
        }
        if scope_node.kind() != Kind::Block && !tsrs_ast::is_statement(scope_node) {
            return Err(GapItem {
                kind: "invalid-cfg-scope",
                message: "CFG scope node must be a function, block, or statement.".to_string(),
                at: Some(node_location(sf, rel_file, scope_node)),
            });
        }

        let scope_location = node_location(sf, rel_file, scope_node);
        let mut builder = CfgBuilder {
            sf,
            rel_file,
            successors: vec![Vec::new()], // vertex 0 is @exit of local scope
            ast_to_cfg: FxHashMap::default(),
            branches: Vec::new(),
            switch_locations: Vec::new(),
            gaps: Vec::new(),
        };

        // Inside a local statement/block scope (such as a loop body), unlabeled break/continue
        // exit the current local scope cleanly without requiring outer loop back-edges.
        let entry = builder.stmt(scope_node, 0, Some(0), Some(0));

        Ok(Self {
            fn_location: scope_location,
            entry,
            successors: builder.successors,
            ast_to_cfg: builder.ast_to_cfg,
            branches: builder.branches,
            switch_locations: builder.switch_locations,
            gaps: builder.gaps,
        })
    }

    pub fn evaluate_coverage(
        &self,
        target_node: P<Node>,
        target_location: &SourceLocation,
        selected_edges: &[usize],
        extra_gaps: &[GapItem],
    ) -> CoverageOutcome {
        let mut combined_gaps = self.gaps.clone();
        combined_gaps.extend(extra_gaps.iter().cloned());

        let Some(&target_vertex) = self.ast_to_cfg.get(&target_node) else {
            if combined_gaps.is_empty() {
                combined_gaps.push(GapItem {
                    kind: "target-outside-cfg",
                    message: "Target node is not part of the modeled function body control flow."
                        .to_string(),
                    at: Some(target_location.clone()),
                });
            }
            return CoverageOutcome {
                status: CoverageStatus::Unknown,
                evidence: Vec::new(),
                gaps: combined_gaps,
            };
        };

        if !combined_gaps.is_empty() {
            let mut evidence = Vec::new();
            for gap in &combined_gaps {
                evidence.push(EvidenceItem::simple(
                    "note",
                    gap.message.clone(),
                    gap.at.clone(),
                ));
            }
            return CoverageOutcome {
                status: CoverageStatus::Unknown,
                evidence,
                gaps: combined_gaps,
            };
        }

        let reachable_all = self.reachable(&FxHashSet::default());
        if !reachable_all[target_vertex] {
            return CoverageOutcome {
                status: CoverageStatus::Unreachable,
                evidence: vec![EvidenceItem::simple(
                    "source",
                    "Target is unreachable from function entry in modeled control flow."
                        .to_string(),
                    Some(target_location.clone()),
                )],
                gaps: Vec::new(),
            };
        }

        let mut cut_set = FxHashSet::default();
        for &edge_idx in selected_edges {
            if edge_idx < self.branches.len() {
                cut_set.insert(edge_idx);
            }
        }

        if !cut_set.is_empty() {
            let reachable_without_cut = self.reachable(&cut_set);
            if !reachable_without_cut[target_vertex] {
                let mut evidence = Vec::new();
                let mut seen = FxHashSet::default();
                for &edge_idx in selected_edges {
                    if edge_idx < self.branches.len() && seen.insert(edge_idx) {
                        let branch = &self.branches[edge_idx];
                        evidence.push(EvidenceItem::simple(
                            "source",
                            format!(
                                "Path covered via {} branch of \"{}\".",
                                branch.direction, branch.condition_text
                            ),
                            Some(branch.location.clone()),
                        ));
                    }
                }
                return CoverageOutcome {
                    status: CoverageStatus::Covered,
                    evidence,
                    gaps: Vec::new(),
                };
            }
        }

        let witness = self
            .find_bypass_witness(target_vertex, &cut_set)
            .unwrap_or_default();
        let evidence = self.build_bypass_evidence(&witness, target_location);
        CoverageOutcome {
            status: CoverageStatus::Bypass,
            evidence,
            gaps: Vec::new(),
        }
    }

    pub fn evaluate_cleanup_coverage(
        &self,
        disposal_nodes: &[P<Node>],
        dynamic_nodes: &[P<Node>],
        scope_location: &SourceLocation,
    ) -> CleanupCfgOutcome {
        let mut disposal_vertices = FxHashSet::default();
        for &node in disposal_nodes {
            if let Some(&v) = self.ast_to_cfg.get(&node) {
                disposal_vertices.insert(v);
            }
        }
        let mut dynamic_vertices = FxHashSet::default();
        for &node in dynamic_nodes {
            if let Some(&v) = self.ast_to_cfg.get(&node) {
                dynamic_vertices.insert(v);
            }
        }

        let reachable_all = self.reachable(&FxHashSet::default());
        let cleaned_on_any_path = disposal_vertices.iter().any(|&v| reachable_all[v]);

        if !self.gaps.is_empty() {
            let evidence = self
                .gaps
                .iter()
                .map(|g| EvidenceItem::simple("note", g.message.clone(), g.at.clone()))
                .collect();
            return CleanupCfgOutcome {
                status: CleanupCfgStatus::Unknown,
                cleaned_on_all_paths: false,
                cleaned_on_any_path,
                evidence,
                gaps: self.gaps.clone(),
            };
        }

        let reachable_without_disposal = self.reachable_blocking(&disposal_vertices);
        if !reachable_without_disposal[0] {
            return CleanupCfgOutcome {
                status: CleanupCfgStatus::Covered,
                cleaned_on_all_paths: true,
                cleaned_on_any_path: true,
                evidence: Vec::new(),
                gaps: Vec::new(),
            };
        }

        let has_dynamic_on_bypass = dynamic_vertices
            .iter()
            .any(|&v| reachable_without_disposal[v]);
        let witness = self
            .find_bypass_witness_blocking(&disposal_vertices)
            .unwrap_or_default();
        let evidence = self.build_bypass_evidence(&witness, scope_location);

        let status = if has_dynamic_on_bypass {
            CleanupCfgStatus::Unknown
        } else if cleaned_on_any_path {
            CleanupCfgStatus::Partial
        } else {
            CleanupCfgStatus::NotCleaned
        };

        CleanupCfgOutcome {
            status,
            cleaned_on_all_paths: false,
            cleaned_on_any_path,
            evidence,
            gaps: Vec::new(),
        }
    }

    fn reachable_blocking(&self, blocked_vertices: &FxHashSet<usize>) -> Vec<bool> {
        let mut visited = vec![false; self.successors.len()];
        let mut queue = VecDeque::new();
        visited[self.entry] = true;
        queue.push_back(self.entry);

        while let Some(u) = queue.pop_front() {
            if blocked_vertices.contains(&u) {
                continue;
            }
            for &(v, _) in &self.successors[u] {
                if !visited[v] {
                    visited[v] = true;
                    queue.push_back(v);
                }
            }
        }
        visited
    }

    fn find_bypass_witness_blocking(
        &self,
        blocked_vertices: &FxHashSet<usize>,
    ) -> Option<Vec<EdgeKind>> {
        let mut visited = vec![false; self.successors.len()];
        let mut parent: Vec<Option<(usize, EdgeKind)>> = vec![None; self.successors.len()];
        let mut queue = VecDeque::new();

        visited[self.entry] = true;
        queue.push_back(self.entry);

        while let Some(u) = queue.pop_front() {
            if u == 0 {
                let mut edges = Vec::new();
                let mut cursor = u;
                while let Some((prev, edge)) = parent[cursor] {
                    edges.push(edge);
                    cursor = prev;
                }
                edges.reverse();
                return Some(edges);
            }
            if blocked_vertices.contains(&u) {
                continue;
            }
            for &(v, edge) in &self.successors[u] {
                if !visited[v] {
                    visited[v] = true;
                    parent[v] = Some((u, edge));
                    queue.push_back(v);
                }
            }
        }

        None
    }

    fn reachable(&self, cut_set: &FxHashSet<usize>) -> Vec<bool> {
        let mut visited = vec![false; self.successors.len()];
        let mut queue = VecDeque::new();
        visited[self.entry] = true;
        queue.push_back(self.entry);

        while let Some(u) = queue.pop_front() {
            for &(v, edge) in &self.successors[u] {
                if let EdgeKind::Branch(idx) = edge
                    && cut_set.contains(&idx)
                {
                    continue;
                }
                if !visited[v] {
                    visited[v] = true;
                    queue.push_back(v);
                }
            }
        }
        visited
    }

    fn find_bypass_witness(
        &self,
        target_vertex: usize,
        cut_set: &FxHashSet<usize>,
    ) -> Option<Vec<EdgeKind>> {
        let mut visited = vec![false; self.successors.len()];
        let mut parent: Vec<Option<(usize, EdgeKind)>> = vec![None; self.successors.len()];
        let mut queue = VecDeque::new();

        visited[self.entry] = true;
        queue.push_back(self.entry);

        while let Some(u) = queue.pop_front() {
            if u == target_vertex {
                let mut edges = Vec::new();
                let mut cursor = u;
                while let Some((prev, edge)) = parent[cursor] {
                    edges.push(edge);
                    cursor = prev;
                }
                edges.reverse();
                return Some(edges);
            }

            for &(v, edge) in &self.successors[u] {
                if let EdgeKind::Branch(idx) = edge
                    && cut_set.contains(&idx)
                {
                    continue;
                }
                if !visited[v] {
                    visited[v] = true;
                    parent[v] = Some((u, edge));
                    queue.push_back(v);
                }
            }
        }

        None
    }

    fn build_bypass_evidence(
        &self,
        witness_edges: &[EdgeKind],
        target_location: &SourceLocation,
    ) -> Vec<EvidenceItem> {
        let mut evidence = Vec::new();
        for &edge in witness_edges {
            match edge {
                EdgeKind::Ordinary => {}
                EdgeKind::Branch(idx) => {
                    if let Some(branch) = self.branches.get(idx) {
                        evidence.push(EvidenceItem::simple(
                            "source",
                            format!(
                                "Bypass path takes {} branch of \"{}\".",
                                branch.direction, branch.condition_text
                            ),
                            Some(branch.location.clone()),
                        ));
                    }
                }
                EdgeKind::SwitchNoMatch(sw_idx) => {
                    if let Some(loc) = self.switch_locations.get(sw_idx) {
                        evidence.push(EvidenceItem::simple(
                            "source",
                            "Switch statement has no default clause; unmatched values bypass all cases.".to_string(),
                            Some(loc.clone()),
                        ));
                    }
                }
            }
        }

        if evidence.is_empty() {
            evidence.push(EvidenceItem::simple(
                "source",
                "Control flow reaches target directly from function entry without passing selected branches.".to_string(),
                Some(self.fn_location.clone()),
            ));
        }

        evidence.push(EvidenceItem::simple(
            "source",
            "Target reached along bypass path without passing selected branches.".to_string(),
            Some(target_location.clone()),
        ));
        evidence
    }
}

struct CfgBuilder<'a> {
    sf: P<SourceFile>,
    rel_file: &'a str,
    successors: Vec<Vec<(usize, EdgeKind)>>,
    ast_to_cfg: FxHashMap<P<Node>, usize>,
    branches: Vec<BranchDescriptor>,
    switch_locations: Vec<SourceLocation>,
    gaps: Vec<GapItem>,
}

impl CfgBuilder<'_> {
    fn new_vertex(&mut self) -> usize {
        let id = self.successors.len();
        self.successors.push(Vec::new());
        id
    }

    fn add_edge(&mut self, from: usize, to: usize, kind: EdgeKind) {
        self.successors[from].push((to, kind));
    }

    fn loc(&self, node: P<Node>) -> SourceLocation {
        node_location(self.sf, self.rel_file, node)
    }

    fn text_of(&self, loc: &SourceLocation) -> String {
        slice_source_text(self.sf, loc.start, loc.end)
    }

    fn push_branch(
        &mut self,
        kind: &'static str,
        direction: &'static str,
        condition_node: P<Node>,
        location: SourceLocation,
        statement_location: SourceLocation,
        condition_text_override: Option<String>,
    ) -> usize {
        let idx = self.branches.len();
        let condition_text = condition_text_override.unwrap_or_else(|| self.text_of(&location));
        self.branches.push(BranchDescriptor {
            kind,
            direction,
            condition_node,
            condition_text,
            location,
            statement_location,
        });
        idx
    }

    fn record_gap(&mut self, kind: &'static str, message: impl Into<String>, node: P<Node>) {
        self.gaps.push(GapItem {
            kind,
            message: message.into(),
            at: Some(self.loc(node)),
        });
    }

    fn sequence(
        &mut self,
        statements: &[P<Node>],
        mut next: usize,
        break_to: Option<usize>,
        continue_to: Option<usize>,
    ) -> usize {
        for &stmt_node in statements.iter().rev() {
            next = self.stmt(stmt_node, next, break_to, continue_to);
        }
        next
    }

    fn stmt(
        &mut self,
        node: P<Node>,
        next: usize,
        break_to: Option<usize>,
        continue_to: Option<usize>,
    ) -> usize {
        let here = self.new_vertex();
        self.ast_to_cfg.insert(node, here);

        match node.kind() {
            Kind::Block => {
                let first = self.sequence(node.statements(), next, break_to, continue_to);
                self.add_edge(here, first, EdgeKind::Ordinary);
                here
            }
            Kind::IfStatement => self.stmt_if(node, here, next, break_to, continue_to),
            Kind::SwitchStatement => self.stmt_switch(node, here, next, continue_to),
            Kind::ReturnStatement => {
                self.add_edge(here, 0, EdgeKind::Ordinary);
                node.as_return_statement()
                    .expression()
                    .map_or(here, |expr| self.expr_val(expr, here))
            }
            Kind::ThrowStatement => {
                self.add_edge(here, 0, EdgeKind::Ordinary);
                let expr = node.as_throw_statement().expression();
                self.expr_val(expr, here)
            }
            Kind::BreakStatement => {
                if node.as_break_statement().label().is_none()
                    && let Some(target) = break_to
                {
                    self.add_edge(here, target, EdgeKind::Ordinary);
                } else {
                    self.record_gap(
                        "unmodeled-jump",
                        "Labeled or unstructured break statement is not precisely modeled.",
                        node,
                    );
                }
                here
            }
            Kind::ContinueStatement => {
                if node.as_continue_statement().label().is_none()
                    && let Some(target) = continue_to
                {
                    self.add_edge(here, target, EdgeKind::Ordinary);
                } else {
                    self.record_gap(
                        "unmodeled-jump",
                        "Loop continue statements are not precisely modeled.",
                        node,
                    );
                    self.add_edge(here, next, EdgeKind::Ordinary);
                }
                here
            }
            Kind::ExpressionStatement => {
                self.add_edge(here, next, EdgeKind::Ordinary);
                let expr = node.as_expression_statement().expression();
                self.expr_val(expr, here)
            }
            Kind::VariableStatement => self.stmt_variable(node, here, next),
            Kind::EmptyStatement
            | Kind::DebuggerStatement
            | Kind::FunctionDeclaration
            | Kind::ClassDeclaration
            | Kind::InterfaceDeclaration
            | Kind::TypeAliasDeclaration
            | Kind::EnumDeclaration
            | Kind::ImportDeclaration
            | Kind::ExportDeclaration
            | Kind::ModuleDeclaration => {
                self.add_edge(here, next, EdgeKind::Ordinary);
                here
            }
            Kind::TryStatement => self.stmt_try(node, here, next, break_to, continue_to),
            Kind::WhileStatement
            | Kind::DoStatement
            | Kind::ForStatement
            | Kind::ForInStatement
            | Kind::ForOfStatement
            | Kind::LabeledStatement => {
                self.stmt_unmodeled(node, here, next, break_to, continue_to)
            }
            _ => {
                self.record_gap(
                    "unmodeled-control-flow",
                    format!(
                        "Control flow for {:?} is not precisely modeled.",
                        node.kind()
                    ),
                    node,
                );
                self.add_edge(here, next, EdgeKind::Ordinary);
                here
            }
        }
    }

    fn stmt_if(
        &mut self,
        node: P<Node>,
        here: usize,
        next: usize,
        break_to: Option<usize>,
        continue_to: Option<usize>,
    ) -> usize {
        let if_stmt = node.as_if_statement();
        let cond = if_stmt.expression();
        let then_entry = self.stmt(if_stmt.then_statement(), next, break_to, continue_to);
        let else_entry = if_stmt.else_statement().map_or(next, |else_node| {
            self.stmt(else_node, next, break_to, continue_to)
        });

        let cond_loc = self.loc(cond);
        let stmt_loc = self.loc(node);
        let true_branch =
            self.push_branch("if", "true", cond, cond_loc.clone(), stmt_loc.clone(), None);
        let false_branch = self.push_branch("if", "false", cond, cond_loc, stmt_loc, None);

        let true_gate = self.new_vertex();
        self.add_edge(true_gate, then_entry, EdgeKind::Branch(true_branch));
        let false_gate = self.new_vertex();
        self.add_edge(false_gate, else_entry, EdgeKind::Branch(false_branch));

        let cond_entry = self.expr_bool(cond, true_gate, false_gate);
        self.add_edge(here, cond_entry, EdgeKind::Ordinary);
        here
    }

    fn stmt_switch(
        &mut self,
        node: P<Node>,
        here: usize,
        next: usize,
        continue_to: Option<usize>,
    ) -> usize {
        let sw = node.as_switch_statement();
        let disc = sw.expression();
        let clauses = sw.case_block().as_case_block().clauses().nodes();
        let dispatch = self.new_vertex();

        let mut fallthrough = next;
        let mut has_default = false;
        let mut clause_entries = Vec::with_capacity(clauses.len());
        for &clause in clauses.iter().rev() {
            has_default |= clause.kind() == Kind::DefaultClause;
            let body_entry =
                self.sequence(clause.statements(), fallthrough, Some(next), continue_to);
            self.ast_to_cfg.insert(clause, body_entry);
            clause_entries.push((clause, body_entry));
            fallthrough = body_entry;
        }
        clause_entries.reverse();

        for (clause, body_entry) in clause_entries {
            let clause_loc = self.loc(clause);
            if clause.kind() == Kind::CaseClause {
                let case_expr = clause.expression().unwrap_or(clause);
                let case_loc = self.loc(case_expr);
                let branch_idx =
                    self.push_branch("switchCase", "case", case_expr, case_loc, clause_loc, None);
                let case_entry = self.expr_val(case_expr, body_entry);
                self.add_edge(dispatch, case_entry, EdgeKind::Branch(branch_idx));
            } else {
                let branch_idx = self.push_branch(
                    "switchDefault",
                    "default",
                    disc,
                    clause_loc.clone(),
                    clause_loc,
                    Some("default".to_string()),
                );
                self.add_edge(dispatch, body_entry, EdgeKind::Branch(branch_idx));
            }
        }

        if !has_default {
            let sw_idx = self.switch_locations.len();
            self.switch_locations.push(self.loc(node));
            self.add_edge(dispatch, next, EdgeKind::SwitchNoMatch(sw_idx));
        }

        let disc_entry = self.expr_val(disc, dispatch);
        self.add_edge(here, disc_entry, EdgeKind::Ordinary);
        here
    }

    fn stmt_variable(&mut self, node: P<Node>, here: usize, next: usize) -> usize {
        self.add_edge(here, next, EdgeKind::Ordinary);
        let declarations = node
            .as_variable_statement()
            .declaration_list()
            .as_variable_declaration_list()
            .declarations()
            .nodes();
        let mut cont = here;
        for &decl in declarations.iter().rev() {
            let decl_vertex = self.new_vertex();
            self.ast_to_cfg.insert(decl, decl_vertex);
            self.add_edge(decl_vertex, cont, EdgeKind::Ordinary);
            cont = decl_vertex;
            if let Some(init) = decl.initializer() {
                cont = self.expr_val(init, cont);
            }
            if let Some(name_node) = decl.name() {
                cont = self.expr_val(name_node, cont);
            }
        }
        cont
    }

    fn stmt_try(
        &mut self,
        node: P<Node>,
        here: usize,
        next: usize,
        break_to: Option<usize>,
        continue_to: Option<usize>,
    ) -> usize {
        self.record_gap(
            "unmodeled-try-catch",
            "Control flow through try/catch/finally is not precisely modeled.",
            node,
        );
        let try_stmt = node.as_try_statement();
        let mut cont = next;
        if let Some(finally_block) = try_stmt.finally_block() {
            cont = self.stmt(finally_block, cont, break_to, continue_to);
        }
        let after_try = cont;
        let try_entry = self.stmt(try_stmt.try_block(), after_try, break_to, continue_to);
        self.add_edge(here, try_entry, EdgeKind::Ordinary);
        if let Some(catch_clause) = try_stmt.catch_clause() {
            let catch_entry = self.stmt(
                catch_clause.as_catch_clause().block(),
                after_try,
                break_to,
                continue_to,
            );
            self.add_edge(here, catch_entry, EdgeKind::Ordinary);
        }
        here
    }

    fn stmt_unmodeled(
        &mut self,
        node: P<Node>,
        here: usize,
        next: usize,
        break_to: Option<usize>,
        continue_to: Option<usize>,
    ) -> usize {
        if node.kind() == Kind::LabeledStatement {
            self.record_gap(
                "unmodeled-jump",
                "Labeled statements are not precisely modeled.",
                node,
            );
            let inner = node.as_labeled_statement().statement();
            let inner_entry = self.stmt(inner, next, break_to, continue_to);
            self.add_edge(here, inner_entry, EdgeKind::Ordinary);
        } else {
            self.record_gap(
                "unmodeled-loop",
                "Control flow through loops is not precisely modeled.",
                node,
            );
            self.scan_unmodeled_children(node, here, next, None, None);
        }
        here
    }

    fn scan_unmodeled_children(
        &mut self,
        node: P<Node>,
        here: usize,
        next: usize,
        break_to: Option<usize>,
        continue_to: Option<usize>,
    ) {
        let mut children = Vec::new();
        node.for_each_child(&mut |child| {
            children.push(child);
            false
        });
        let mut cont = next;
        for &child in children.iter().rev() {
            if is_function_like_declaration(Some(child))
                || matches!(child.kind(), Kind::ClassDeclaration | Kind::ClassExpression)
                || is_type_node(child)
            {
                continue;
            }
            cont = if tsrs_ast::is_statement(child) {
                self.stmt(child, cont, break_to, continue_to)
            } else {
                self.expr_val(child, cont)
            };
        }
        self.add_edge(here, cont, EdgeKind::Ordinary);
    }

    fn expr_bool(&mut self, node: P<Node>, on_true: usize, on_false: usize) -> usize {
        if is_type_node(node) {
            let v = self.new_vertex();
            self.add_edge(v, on_true, EdgeKind::Ordinary);
            self.add_edge(v, on_false, EdgeKind::Ordinary);
            return v;
        }

        match node.kind() {
            Kind::ParenthesizedExpression => {
                let inner = node.as_parenthesized_expression().expression();
                let entry = self.expr_bool(inner, on_true, on_false);
                self.alias_ast_vertex(node, inner, entry);
                entry
            }
            Kind::PrefixUnaryExpression
                if node.as_prefix_unary_expression().operator() == Kind::ExclamationToken =>
            {
                let operand = node.as_prefix_unary_expression().operand();
                let entry = self.expr_bool(operand, on_false, on_true);
                self.ast_to_cfg.insert(node, entry);
                entry
            }
            Kind::BinaryExpression => {
                let bin = node.as_binary_expression();
                let op = bin.operator_token().kind();
                match op {
                    Kind::AmpersandAmpersandToken | Kind::AmpersandAmpersandEqualsToken => {
                        self.expr_logical_and(node, bin.left(), bin.right(), on_true, on_false)
                    }
                    Kind::BarBarToken | Kind::BarBarEqualsToken => {
                        self.expr_logical_or(node, bin.left(), bin.right(), on_true, on_false)
                    }
                    _ => self.expr_bool_leaf(node, on_true, on_false),
                }
            }
            Kind::ConditionalExpression => {
                let cond_expr = node.as_conditional_expression();
                let true_entry = self.expr_bool(cond_expr.when_true(), on_true, on_false);
                let false_entry = self.expr_bool(cond_expr.when_false(), on_true, on_false);
                self.build_conditional_branches(
                    node,
                    cond_expr.condition(),
                    true_entry,
                    false_entry,
                )
            }
            _ => self.expr_bool_leaf(node, on_true, on_false),
        }
    }

    fn alias_ast_vertex(&mut self, outer: P<Node>, inner: P<Node>, fallback: usize) {
        let vertex = self.ast_to_cfg.get(&inner).copied().unwrap_or(fallback);
        self.ast_to_cfg.insert(outer, vertex);
    }

    fn expr_logical_and(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        right: P<Node>,
        on_true: usize,
        on_false: usize,
    ) -> usize {
        let right_entry = self.expr_bool(right, on_true, on_false);
        let expr_loc = self.loc(node);
        let left_loc = self.loc(left);
        let cont_branch = self.push_branch(
            "logicalAnd",
            "continue",
            left,
            left_loc.clone(),
            expr_loc.clone(),
            None,
        );
        let short_branch =
            self.push_branch("logicalAnd", "shortCircuit", left, left_loc, expr_loc, None);

        let cont_gate = self.new_vertex();
        self.add_edge(cont_gate, right_entry, EdgeKind::Branch(cont_branch));
        let short_gate = self.new_vertex();
        self.add_edge(short_gate, on_false, EdgeKind::Branch(short_branch));

        let here = self.new_vertex();
        self.ast_to_cfg.insert(node, here);
        let left_entry = self.expr_bool(left, cont_gate, short_gate);
        self.add_edge(here, left_entry, EdgeKind::Ordinary);
        here
    }

    fn expr_logical_or(
        &mut self,
        node: P<Node>,
        left: P<Node>,
        right: P<Node>,
        on_true: usize,
        on_false: usize,
    ) -> usize {
        let right_entry = self.expr_bool(right, on_true, on_false);
        let expr_loc = self.loc(node);
        let left_loc = self.loc(left);
        let short_branch = self.push_branch(
            "logicalOr",
            "shortCircuit",
            left,
            left_loc.clone(),
            expr_loc.clone(),
            None,
        );
        let cont_branch = self.push_branch("logicalOr", "continue", left, left_loc, expr_loc, None);

        let short_gate = self.new_vertex();
        self.add_edge(short_gate, on_true, EdgeKind::Branch(short_branch));
        let cont_gate = self.new_vertex();
        self.add_edge(cont_gate, right_entry, EdgeKind::Branch(cont_branch));

        let here = self.new_vertex();
        self.ast_to_cfg.insert(node, here);
        let left_entry = self.expr_bool(left, short_gate, cont_gate);
        self.add_edge(here, left_entry, EdgeKind::Ordinary);
        here
    }

    fn build_conditional_branches(
        &mut self,
        node: P<Node>,
        cond: P<Node>,
        true_entry: usize,
        false_entry: usize,
    ) -> usize {
        let cond_loc = self.loc(cond);
        let stmt_loc = self.loc(node);
        let true_branch = self.push_branch(
            "conditional",
            "true",
            cond,
            cond_loc.clone(),
            stmt_loc.clone(),
            None,
        );
        let false_branch = self.push_branch("conditional", "false", cond, cond_loc, stmt_loc, None);

        let true_gate = self.new_vertex();
        self.add_edge(true_gate, true_entry, EdgeKind::Branch(true_branch));
        let false_gate = self.new_vertex();
        self.add_edge(false_gate, false_entry, EdgeKind::Branch(false_branch));

        let here = self.new_vertex();
        self.ast_to_cfg.insert(node, here);
        let cond_entry = self.expr_bool(cond, true_gate, false_gate);
        self.add_edge(here, cond_entry, EdgeKind::Ordinary);
        here
    }

    fn expr_bool_leaf(&mut self, node: P<Node>, on_true: usize, on_false: usize) -> usize {
        let split = self.new_vertex();
        match static_bool_value(node) {
            Some(true) => self.add_edge(split, on_true, EdgeKind::Ordinary),
            Some(false) => self.add_edge(split, on_false, EdgeKind::Ordinary),
            None => {
                self.add_edge(split, on_true, EdgeKind::Ordinary);
                self.add_edge(split, on_false, EdgeKind::Ordinary);
            }
        }
        self.expr_val(node, split)
    }

    fn expr_val(&mut self, node: P<Node>, next: usize) -> usize {
        if is_type_node(node) {
            return next;
        }

        match node.kind() {
            Kind::FunctionDeclaration
            | Kind::FunctionExpression
            | Kind::ArrowFunction
            | Kind::ClassDeclaration
            | Kind::ClassExpression => {
                let here = self.new_vertex();
                self.ast_to_cfg.insert(node, here);
                self.add_edge(here, next, EdgeKind::Ordinary);
                here
            }
            Kind::ParenthesizedExpression => {
                let inner = node.as_parenthesized_expression().expression();
                let entry = self.expr_val(inner, next);
                self.alias_ast_vertex(node, inner, entry);
                entry
            }
            Kind::ConditionalExpression => {
                let cond_expr = node.as_conditional_expression();
                let true_entry = self.expr_val(cond_expr.when_true(), next);
                let false_entry = self.expr_val(cond_expr.when_false(), next);
                self.build_conditional_branches(
                    node,
                    cond_expr.condition(),
                    true_entry,
                    false_entry,
                )
            }
            Kind::BinaryExpression => {
                let bin = node.as_binary_expression();
                match bin.operator_token().kind() {
                    Kind::AmpersandAmpersandToken
                    | Kind::AmpersandAmpersandEqualsToken
                    | Kind::BarBarToken
                    | Kind::BarBarEqualsToken => self.expr_bool(node, next, next),
                    Kind::QuestionQuestionToken | Kind::QuestionQuestionEqualsToken => {
                        self.expr_nullish(node, bin.left(), bin.right(), next)
                    }
                    _ => self.expr_children(node, next),
                }
            }
            Kind::YieldExpression => {
                self.record_gap(
                    "unmodeled-generator",
                    "Generator yield control flow is not precisely modeled.",
                    node,
                );
                self.expr_children(node, next)
            }
            _ => self.expr_children(node, next),
        }
    }

    fn expr_nullish(&mut self, node: P<Node>, left: P<Node>, right: P<Node>, next: usize) -> usize {
        let right_entry = self.expr_val(right, next);
        let expr_loc = self.loc(node);
        let left_loc = self.loc(left);
        let short_branch = self.push_branch(
            "nullishCoalescing",
            "shortCircuit",
            left,
            left_loc.clone(),
            expr_loc.clone(),
            None,
        );
        let cont_branch = self.push_branch(
            "nullishCoalescing",
            "continue",
            left,
            left_loc,
            expr_loc,
            None,
        );

        let gate = self.new_vertex();
        self.add_edge(gate, next, EdgeKind::Branch(short_branch));
        self.add_edge(gate, right_entry, EdgeKind::Branch(cont_branch));

        let here = self.new_vertex();
        self.ast_to_cfg.insert(node, here);
        let left_entry = self.expr_val(left, gate);
        self.add_edge(here, left_entry, EdgeKind::Ordinary);
        here
    }

    fn expr_children(&mut self, node: P<Node>, next: usize) -> usize {
        let here = self.new_vertex();
        self.ast_to_cfg.insert(node, here);
        self.add_edge(here, next, EdgeKind::Ordinary);

        let mut children = Vec::new();
        node.for_each_child(&mut |child| {
            children.push(child);
            false
        });

        let mut cont = here;
        for &child in children.iter().rev() {
            cont = self.expr_val(child, cont);
        }
        cont
    }
}

fn static_bool_value(node: P<Node>) -> Option<bool> {
    match node.kind() {
        Kind::TrueKeyword => Some(true),
        Kind::FalseKeyword => Some(false),
        Kind::ParenthesizedExpression => {
            static_bool_value(node.as_parenthesized_expression().expression())
        }
        Kind::PrefixUnaryExpression => {
            let prefix = node.as_prefix_unary_expression();
            if prefix.operator() == Kind::ExclamationToken {
                static_bool_value(prefix.operand()).map(|val| !val)
            } else {
                None
            }
        }
        _ => None,
    }
}
