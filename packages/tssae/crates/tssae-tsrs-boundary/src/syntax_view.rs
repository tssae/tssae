use std::{collections::BTreeSet, path::Path};

use rustc_hash::FxHashMap;
use serde::Serialize;
use tsrs_ast::{Kind, Node, SourceFile, get_source_file_of_node, is_function_like_declaration};
use tsrs_compiler::Program;
use tsrs_core::P;
use tsrs_scanner::{
    get_ecma_line_and_utf16_character_of_position, get_ecma_line_starts, get_token_pos_of_node,
};
use tssae_analysis::ir::{IrScopeKind, ScopeCatalogEntry, scope_ir_id};
use tssae_common::{DiagnosticLocation, FileId, LocalSyntaxId, SyntaxCatalog, SyntaxId};

/// Error raised when a syntax view query receives invalid range or kind parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxViewError {
    pub code: &'static str,
    pub message: String,
}

impl SyntaxViewError {
    #[must_use]
    pub const fn new(code: &'static str, message: String) -> Self {
        Self { code, message }
    }
}

/// Range matching behavior for [`SourceRangeSpec`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeMatchMode {
    Within,
    Intersects,
}

/// Source range filter parameters normalized at the boundary.
#[derive(Clone, Debug, Default)]
pub struct SourceRangeSpec<'a> {
    pub file: &'a str,
    pub start_line: Option<u32>,
    pub start_column: Option<u32>,
    pub end_line: Option<u32>,
    pub end_column: Option<u32>,
    pub start: Option<u32>,
    pub end: Option<u32>,
    pub match_mode: Option<&'a str>,
}

/// Filter options for [`SyntaxView::find_syntax`].
#[derive(Clone, Debug, Default)]
pub struct FindSyntaxFilter<'a> {
    pub files: Option<&'a [String]>,
    pub kinds: Option<&'a [String]>,
    pub range: Option<SourceRangeSpec<'a>>,
    pub text_includes: Option<&'a [String]>,
    pub callee_names: Option<&'a [String]>,
}

/// Direct child syntax nodes attached to a matched or inspected AST node.
#[derive(Clone, Debug)]
pub struct SyntaxChildren {
    pub expression: Option<P<Node>>,
    pub arguments: Option<Vec<P<Node>>>,
    pub name: Option<P<Node>>,
    pub initializer: Option<P<Node>>,
    pub body: Option<P<Node>>,
    pub parameters: Option<Vec<P<Node>>>,
}

/// One matched syntax node produced by [`SyntaxView::find_syntax`].
#[derive(Clone, Debug)]
pub struct SyntaxMatch {
    pub node: P<Node>,
    pub kind: String,
    pub text: String,
    pub location: DiagnosticLocation,
    pub children: SyntaxChildren,
}

/// Field selection flags for [`SyntaxView::inspect_node`].
#[derive(Clone, Copy, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Maps 1:1 to syntaxOf projection selection keys."
)]
pub struct SyntaxLookupOptions {
    pub kind: bool,
    pub text: bool,
    pub location: bool,
    pub expression: bool,
    pub arguments: bool,
    pub name: bool,
    pub initializer: bool,
    pub body: bool,
    pub parameters: bool,
}

/// Inspected properties of a single syntax node for `syntaxOf`.
#[derive(Clone, Debug)]
pub struct SyntaxLookupMatch {
    pub node: P<Node>,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub location: Option<DiagnosticLocation>,
    pub expression: Option<P<Node>>,
    pub arguments: Option<Vec<P<Node>>>,
    pub name: Option<P<Node>>,
    pub initializer: Option<P<Node>>,
    pub body: Option<P<Node>>,
    pub parameters: Option<Vec<P<Node>>>,
}

/// Comment syntax kind reported by `comments.scan`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CommentKind {
    Line,
    Block,
}

/// Standalone comment and its immediately following code line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedComment {
    pub kind: CommentKind,
    pub text: String,
    pub at: DiagnosticLocation,
    pub next_code_line: Option<u32>,
    pub is_jsx: bool,
}

/// Boundary view over AST syntax, diagnostic locations, source slicing, and comment scanning.
#[derive(Clone, Copy, Debug)]
pub struct SyntaxView<'a> {
    cwd: &'a str,
}

impl<'a> SyntaxView<'a> {
    #[must_use]
    pub const fn new(cwd: &'a str) -> Self {
        Self { cwd }
    }

    #[must_use]
    pub const fn cwd(&self) -> &'a str {
        self.cwd
    }

    #[must_use]
    pub fn relative_path(&self, file_path: &str) -> String {
        relative_to_cwd(self.cwd, file_path)
    }

    #[must_use]
    pub fn resolve_path(&self, candidate: &str) -> String {
        resolve_against_cwd(self.cwd, candidate)
    }

    #[must_use]
    pub fn node_location_relative(&self, node: P<Node>) -> Option<DiagnosticLocation> {
        node_location_relative(self.cwd, node)
    }

    #[must_use]
    pub fn node_location(
        &self,
        sf: P<SourceFile>,
        rel_file: &str,
        node: P<Node>,
    ) -> DiagnosticLocation {
        node_location(sf, rel_file, node)
    }

    #[must_use]
    pub fn span_location(
        &self,
        sf: P<SourceFile>,
        rel_file: &str,
        start: u32,
        end: u32,
    ) -> DiagnosticLocation {
        span_location(sf, rel_file, start, end)
    }

    #[must_use]
    pub fn node_kind_label(&self, node: P<Node>) -> String {
        format!("{:?}", node.kind())
    }

    #[must_use]
    pub fn classify_ir_scope(&self, node: P<Node>) -> Option<IrScopeKind> {
        match node.kind() {
            Kind::FunctionDeclaration | Kind::FunctionExpression | Kind::ArrowFunction => {
                Some(IrScopeKind::Function)
            }
            Kind::MethodDeclaration => Some(IrScopeKind::Method),
            Kind::Constructor => Some(IrScopeKind::Constructor),
            Kind::GetAccessor => Some(IrScopeKind::Getter),
            Kind::SetAccessor => Some(IrScopeKind::Setter),
            Kind::ClassStaticBlockDeclaration => Some(IrScopeKind::ClassStaticInit),
            Kind::SourceFile => Some(IrScopeKind::ModuleTopLevel),
            _ => None,
        }
    }

    /// Searches source files in `program` matching the given filters.
    ///
    /// # Errors
    /// Returns [`SyntaxViewError`] if `kinds` and `range` are both omitted, a kind name
    /// is unsupported, or the source range is invalid.
    pub fn find_syntax(
        &self,
        program: &Program,
        filter: &FindSyntaxFilter<'_>,
    ) -> Result<Vec<SyntaxMatch>, SyntaxViewError> {
        let kinds_slice = filter.kinds.unwrap_or(&[]);
        if kinds_slice.is_empty() && filter.range.is_none() {
            return Err(SyntaxViewError::new(
                "INVALID_PARAMS",
                "findSyntax requires at least one syntax kind or a source range.".to_string(),
            ));
        }

        let mut target_kinds = Vec::with_capacity(kinds_slice.len());
        for kind_name in kinds_slice {
            let Some(kind) = parse_syntax_kind(kind_name) else {
                return Err(SyntaxViewError::new(
                    "INVALID_PARAMS",
                    format!("Unsupported syntax kind: {kind_name}"),
                ));
            };
            target_kinds.push((kind, kind_name.as_str()));
        }

        let text_filter = syntax_needles(filter.text_includes);
        let callee_filter = syntax_needles(filter.callee_names);
        if (filter.text_includes.is_some() && text_filter.is_empty())
            || (filter.callee_names.is_some() && callee_filter.is_empty())
        {
            return Ok(Vec::new());
        }

        let range_file = filter
            .range
            .as_ref()
            .map(|r| self.relative_path(&self.resolve_path(r.file)));

        let file_filter: Option<Vec<String>> = filter.files.map(|list| {
            list.iter()
                .map(|f| self.relative_path(&self.resolve_path(f)))
                .collect()
        });

        let explicit_file_targeted = filter.range.is_some() || file_filter.is_some();
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
                let rel = self.relative_path(sf.file_name());
                if let Some(target_file) = &range_file
                    && target_file != &rel
                {
                    return None;
                }
                if let Some(allowed) = &file_filter
                    && !allowed.iter().any(|item| item == &rel)
                {
                    return None;
                }
                Some((rel, sf))
            })
            .collect();
        source_files.sort_by(|a, b| a.0.cmp(&b.0));

        let mut items = Vec::new();
        for (rel_file, sf) in source_files {
            let byte_range = match filter.range.as_ref() {
                Some(range_spec) => Some(resolve_byte_range(sf, range_spec)?),
                None => None,
            };
            collect_matching_nodes(
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

    #[must_use]
    pub fn inspect_node(&self, node: P<Node>, select: &SyntaxLookupOptions) -> SyntaxLookupMatch {
        let sf_opt = get_source_file_of_node(node);
        let loc_opt = if select.location || select.text {
            sf_opt.map(|sf| {
                let rel_file = self.relative_path(sf.file_name());
                node_location(sf, &rel_file, node)
            })
        } else {
            None
        };

        let kind = select.kind.then(|| format!("{:?}", node.kind()));
        let text = if select.text {
            sf_opt
                .zip(loc_opt.as_ref())
                .map(|(sf, loc)| slice_source_text(sf, loc.start, loc.end))
        } else {
            None
        };
        let location = if select.location { loc_opt } else { None };
        let expression = if select.expression {
            safe_expression(node)
        } else {
            None
        };
        let arguments = select
            .arguments
            .then(|| safe_arguments(node).map_or_else(Vec::new, <[P<Node>]>::to_vec));
        let name = if select.name { node.name() } else { None };
        let initializer = if select.initializer {
            safe_initializer(node)
        } else {
            None
        };
        let body = if select.body { node.body() } else { None };
        let parameters = select
            .parameters
            .then(|| safe_parameters(node).map_or_else(Vec::new, <[P<Node>]>::to_vec));

        SyntaxLookupMatch {
            node,
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

    /// Scans project source files in `program` for standalone comments, deduplicating by `seen_files`.
    pub fn scan_program_comments(
        &self,
        program: &Program,
        filter_set: Option<&BTreeSet<String>>,
        seen_files: &mut BTreeSet<String>,
        out: &mut Vec<ScannedComment>,
    ) {
        for &sf in program.get_source_files() {
            if !is_project_source_file(program, sf) {
                continue;
            }
            let rel_file = self.relative_path(sf.file_name());
            if let Some(allowed) = filter_set
                && !allowed.contains(&rel_file)
            {
                continue;
            }
            if !seen_files.insert(rel_file.clone()) {
                continue;
            }
            out.extend(scan_source_file_comments(sf, &rel_file));
        }
    }

    #[must_use]
    pub fn scan_source_file_comments(
        &self,
        sf: P<SourceFile>,
        rel_file: &str,
    ) -> Vec<ScannedComment> {
        scan_source_file_comments(sf, rel_file)
    }
}

/// Session-wide registry assigning monotonically increasing [`FileId`] handles to normalized paths.
#[derive(Default, Debug)]
pub struct FileIdRegistry {
    paths: Vec<String>,
    path_to_id: FxHashMap<String, FileId>,
}

impl FileIdRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, rel_file: &str) -> FileId {
        if let Some(&id) = self.path_to_id.get(rel_file) {
            return id;
        }
        let id = FileId(u32::try_from(self.paths.len()).unwrap_or(u32::MAX));
        self.paths.push(rel_file.to_string());
        self.path_to_id.insert(rel_file.to_string(), id);
        id
    }

    #[must_use]
    pub fn path(&self, file_id: FileId) -> Option<&str> {
        self.paths
            .get(usize::try_from(file_id.0).ok()?)
            .map(String::as_str)
    }
}

/// Snapshot-scoped deterministic pre-order AST index for a single `SourceFile`.
///
/// Rebuilding this index for an unchanged `SourceFile` yields identical [`LocalSyntaxId`]s and
/// scope slots without storing any `P<Node>` pointers inside the persistent `SnapshotIrCache`.
#[derive(Debug)]
pub struct FileSyntaxIndex {
    pub file_id: FileId,
    pub rel_file: String,
    pub source_file_addr: usize,
    pub node_to_local: FxHashMap<P<Node>, LocalSyntaxId>,
    pub local_to_node: Vec<P<Node>>,
    pub scope_catalog: Vec<ScopeCatalogEntry>,
}

impl FileSyntaxIndex {
    #[must_use]
    pub fn build(file_id: FileId, rel_file: String, sf: P<SourceFile>) -> Self {
        fn visit(
            file_id: FileId,
            node: P<Node>,
            node_to_local: &mut FxHashMap<P<Node>, LocalSyntaxId>,
            local_to_node: &mut Vec<P<Node>>,
            scope_catalog: &mut Vec<ScopeCatalogEntry>,
        ) {
            if let std::collections::hash_map::Entry::Vacant(slot) = node_to_local.entry(node) {
                let local = LocalSyntaxId(u32::try_from(local_to_node.len()).unwrap_or(u32::MAX));
                slot.insert(local);
                local_to_node.push(node);

                if let Some(kind) = classify_catalog_scope_kind(node.kind()) {
                    let local_scope = u32::try_from(scope_catalog.len()).unwrap_or(u32::MAX);
                    let root_syntax = SyntaxId::from_parts(file_id, local);
                    scope_catalog.push(ScopeCatalogEntry {
                        id: scope_ir_id(file_id, local_scope),
                        local_syntax: local,
                        root_syntax,
                        kind,
                    });
                }
            }
            node.for_each_child(&mut |child| {
                visit(file_id, child, node_to_local, local_to_node, scope_catalog);
                false
            });
        }

        let mut node_to_local = FxHashMap::default();
        let mut local_to_node = Vec::new();
        let mut scope_catalog = Vec::new();
        let source_file_addr = std::ptr::from_ref(&*sf) as usize;

        visit(
            file_id,
            sf.as_node(),
            &mut node_to_local,
            &mut local_to_node,
            &mut scope_catalog,
        );

        Self {
            file_id,
            rel_file,
            source_file_addr,
            node_to_local,
            local_to_node,
            scope_catalog,
        }
    }

    #[must_use]
    pub fn syntax_id_for_node(&self, node: P<Node>) -> Option<SyntaxId> {
        self.node_to_local
            .get(&node)
            .copied()
            .map(|local| SyntaxId::from_parts(self.file_id, local))
    }

    #[must_use]
    pub fn local_id_for_node(&self, node: P<Node>) -> Option<LocalSyntaxId> {
        self.node_to_local.get(&node).copied()
    }

    #[must_use]
    pub fn node_for_syntax_id(&self, syntax: SyntaxId) -> Option<P<Node>> {
        if syntax.file_id() != self.file_id {
            return None;
        }
        self.local_to_node
            .get(usize::try_from(syntax.local_id().0).ok()?)
            .copied()
    }
}

const fn classify_catalog_scope_kind(kind: Kind) -> Option<IrScopeKind> {
    match kind {
        Kind::FunctionDeclaration | Kind::FunctionExpression | Kind::ArrowFunction => {
            Some(IrScopeKind::Function)
        }
        Kind::MethodDeclaration => Some(IrScopeKind::Method),
        Kind::Constructor => Some(IrScopeKind::Constructor),
        Kind::GetAccessor => Some(IrScopeKind::Getter),
        Kind::SetAccessor => Some(IrScopeKind::Setter),
        Kind::ClassStaticBlockDeclaration => Some(IrScopeKind::ClassStaticInit),
        Kind::SourceFile => Some(IrScopeKind::ModuleTopLevel),
        _ => None,
    }
}

/// Snapshot-independent [`SyntaxCatalog`] implementation backed by interned AST handles.
#[derive(Default)]
pub struct SyntaxCatalogBridge {
    cwd: String,
    files: Vec<String>,
    file_to_id: FxHashMap<String, FileId>,
    nodes: FxHashMap<SyntaxId, P<Node>>,
    node_to_id: FxHashMap<P<Node>, SyntaxId>,
    next_local_ids: FxHashMap<FileId, u32>,
}

impl SyntaxCatalogBridge {
    #[must_use]
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Self::default()
        }
    }

    pub fn intern_node(&mut self, node: P<Node>) -> Option<SyntaxId> {
        if let Some(&id) = self.node_to_id.get(&node) {
            return Some(id);
        }
        let sf = get_source_file_of_node(node)?;
        let rel_file = relative_to_cwd(&self.cwd, sf.file_name());
        let file_id = if let Some(&id) = self.file_to_id.get(&rel_file) {
            id
        } else {
            let id = FileId(u32::try_from(self.files.len()).unwrap_or(u32::MAX));
            self.files.push(rel_file.clone());
            self.file_to_id.insert(rel_file, id);
            id
        };
        let next_local = self.next_local_ids.entry(file_id).or_insert(0);
        let local = LocalSyntaxId(*next_local);
        *next_local = next_local.saturating_add(1);
        let syntax_id = SyntaxId::from_parts(file_id, local);
        self.nodes.insert(syntax_id, node);
        self.node_to_id.insert(node, syntax_id);
        Some(syntax_id)
    }

    #[must_use]
    pub fn get_node(&self, syntax: SyntaxId) -> Option<P<Node>> {
        self.nodes.get(&syntax).copied()
    }
}

impl SyntaxCatalog for SyntaxCatalogBridge {
    fn syntax_location(&self, syntax: SyntaxId) -> Option<DiagnosticLocation> {
        let node = self.get_node(syntax)?;
        node_location_relative(&self.cwd, node)
    }

    fn syntax_text(&self, syntax: SyntaxId) -> Option<String> {
        let node = self.get_node(syntax)?;
        node_text(node)
    }

    fn syntax_kind(&self, syntax: SyntaxId) -> Option<String> {
        let node = self.get_node(syntax)?;
        Some(format!("{:?}", node.kind()))
    }

    fn file_path(&self, file: FileId) -> Option<&str> {
        self.files
            .get(usize::try_from(file.0).ok()?)
            .map(String::as_str)
    }
}

/// Normalizes slashes and dot segments in a POSIX or Windows-like path.
#[must_use]
pub fn normalize_path(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    let is_abs = replaced.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in replaced.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if let Some(&last) = parts.last()
                    && last != ".."
                {
                    parts.pop();
                } else if !is_abs {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    if is_abs {
        if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", parts.join("/"))
        }
    } else if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Resolves `candidate` against `cwd` and normalizes dot segments.
#[must_use]
pub fn resolve_against_cwd(cwd: &str, candidate: &str) -> String {
    let normalized = candidate.replace('\\', "/");
    if Path::new(&normalized).is_absolute() {
        normalize_path(&normalized)
    } else if cwd == "/" {
        normalize_path(&format!("/{normalized}"))
    } else {
        normalize_path(&format!("{cwd}/{normalized}"))
    }
}

/// Converts `file_path` to a path relative to `cwd` when inside `cwd`.
#[must_use]
pub fn relative_to_cwd(cwd: &str, file_path: &str) -> String {
    let file_norm = normalize_path(file_path);
    let cwd_norm = normalize_path(cwd);
    let prefix = if cwd_norm == "/" {
        "/".to_string()
    } else {
        format!("{cwd_norm}/")
    };
    if let Some(stripped) = file_norm.strip_prefix(&prefix) {
        return stripped.to_string();
    }
    if file_norm == cwd_norm {
        return ".".to_string();
    }
    file_norm
}

/// Returns `true` when `sf` belongs to the analyzed project rather than libs, external deps, or references.
#[must_use]
pub fn is_project_source_file(program: &Program, sf: P<SourceFile>) -> bool {
    let path = sf.path();
    !program.is_source_file_default_library(path)
        && !program.is_source_file_from_external_library(sf)
        && !program.is_source_from_project_reference(path)
}

/// Computes the 1-based UTF-16 line/column and byte span for `node` within `sf`.
#[must_use]
pub fn node_location(sf: P<SourceFile>, rel_file: &str, node: P<Node>) -> DiagnosticLocation {
    let start = get_token_pos_of_node(node, sf, false);
    let end = node.end().source_offset().unwrap_or(start).max(start);
    span_location(sf, rel_file, start, end)
}

/// Computes the 1-based UTF-16 line/column and byte span for an explicit byte range within `sf`.
#[must_use]
pub fn span_location(
    sf: P<SourceFile>,
    rel_file: &str,
    start: u32,
    end: u32,
) -> DiagnosticLocation {
    let (start_line0, start_col0) = get_ecma_line_and_utf16_character_of_position(sf.get(), start);
    let (end_line0, end_col0) = get_ecma_line_and_utf16_character_of_position(sf.get(), end);

    DiagnosticLocation {
        file: rel_file.to_string(),
        line: u32::try_from(start_line0).unwrap_or(u32::MAX - 1) + 1,
        column: start_col0 + 1,
        end_line: u32::try_from(end_line0).unwrap_or(u32::MAX - 1) + 1,
        end_column: end_col0 + 1,
        start,
        end,
    }
}

/// Computes the [`DiagnosticLocation`] of `node` relative to `cwd`.
#[must_use]
pub fn node_location_relative(cwd: &str, node: P<Node>) -> Option<DiagnosticLocation> {
    let sf = get_source_file_of_node(node)?;
    let rel_file = relative_to_cwd(cwd, sf.file_name());
    Some(node_location(sf, &rel_file, node))
}

/// Slices source text from `sf` for the UTF-8 byte range `[start, end)`.
#[must_use]
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

/// Returns the source text of `node` excluding leading trivia.
#[must_use]
pub fn node_text(node: P<Node>) -> Option<String> {
    let sf = get_source_file_of_node(node)?;
    let loc = node_location(sf, "", node);
    Some(slice_source_text(sf, loc.start, loc.end))
}

/// Strips parenthesized, `as`, type assertion, `satisfies`, and non-null wrapper expressions.
#[must_use]
pub fn unwrap_expr_wrappers(mut node: P<Node>) -> P<Node> {
    loop {
        match node.kind() {
            Kind::ParenthesizedExpression => {
                node = node.as_parenthesized_expression().expression();
            }
            Kind::AsExpression => {
                node = node.as_as_expression().expression();
            }
            Kind::TypeAssertionExpression => {
                node = node.as_type_assertion().expression();
            }
            Kind::SatisfiesExpression => {
                node = node.as_satisfies_expression().expression();
            }
            Kind::NonNullExpression => {
                node = node.as_non_null_expression().expression();
            }
            _ => return node,
        }
    }
}

/// Extracts static property key text for identifiers, string/number literals, and computed string keys.
#[must_use]
pub fn prop_key_text(node: P<Node>) -> Option<String> {
    if node.kind() == Kind::ComputedPropertyName {
        let expr = unwrap_expr_wrappers(node.expression()?);
        if matches!(
            expr.kind(),
            Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
        ) {
            return prop_key_text(expr);
        }
        return None;
    }
    let raw = node_text(node)?;
    if matches!(
        node.kind(),
        Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
    ) {
        let bytes = raw.as_bytes();
        if bytes.len() >= 2 {
            let q = bytes[0];
            if (q == b'"' || q == b'\'' || q == b'`') && bytes[bytes.len() - 1] == q {
                return Some(raw[1..raw.len() - 1].to_string());
            }
        }
    }
    Some(raw)
}

/// Safely extracts `node.expression()` only on AST kinds that define an expression slot.
#[must_use]
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

/// Safely extracts `node.initializer()` only on AST kinds that define an initializer slot.
#[must_use]
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

/// Safely extracts call/new argument nodes.
#[must_use]
pub fn safe_arguments(node: P<Node>) -> Option<&'static [P<Node>]> {
    matches!(node.kind(), Kind::CallExpression | Kind::NewExpression).then(|| node.arguments())
}

/// Safely extracts parameter nodes on function-like declarations.
#[must_use]
pub fn safe_parameters(node: P<Node>) -> Option<&'static [P<Node>]> {
    (is_function_like_declaration(Some(node)) || node.function_like_data().is_some())
        .then(|| node.parameters())
}

/// Maps a public `SyntaxKind` string to `tsrs_ast::Kind`.
#[must_use]
pub fn parse_syntax_kind(name: &str) -> Option<Kind> {
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
    filter: &SourceRangeSpec<'_>,
) -> Result<ResolvedByteRange, SyntaxViewError> {
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
        return Err(SyntaxViewError::new(
            "INVALID_PARAMS",
            format!("Invalid source range: start ({start}) exceeds end ({end})."),
        ));
    }

    let mode = match filter.match_mode {
        None | Some("within") => RangeMatchMode::Within,
        Some("intersects") => RangeMatchMode::Intersects,
        Some(other) => {
            return Err(SyntaxViewError::new(
                "INVALID_PARAMS",
                format!("Unsupported range match mode: {other}"),
            ));
        }
    };

    Ok(ResolvedByteRange { start, end, mode })
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
    sf: P<SourceFile>,
    rel_file: &str,
    node: P<Node>,
    target_kinds: &[(Kind, &str)],
    range: Option<ResolvedByteRange>,
    text_filter: &[&str],
    callee_filter: &[&str],
    out: &mut Vec<SyntaxMatch>,
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
                out.push(SyntaxMatch {
                    node,
                    kind: kind_label,
                    text: slice_source_text(sf, location.start, location.end),
                    location,
                    children: SyntaxChildren {
                        expression: safe_expression(node),
                        arguments: safe_arguments(node).map(<[P<Node>]>::to_vec),
                        name: node.name(),
                        initializer: safe_initializer(node),
                        body: node.body(),
                        parameters: safe_parameters(node).map(<[P<Node>]>::to_vec),
                    },
                });
            }
        }
    }

    node.for_each_child(&mut |child| {
        collect_matching_nodes(
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

#[derive(Debug, Clone, Copy)]
struct OpaqueSpan {
    start: u32,
    end: u32,
    is_code: bool,
}

#[derive(Debug, Clone, Copy)]
struct RawComment {
    start: u32,
    end: u32,
    is_line: bool,
    is_jsdoc: bool,
}

fn scan_source_file_comments(sf: P<SourceFile>, rel_file: &str) -> Vec<ScannedComment> {
    let text = sf.text();
    let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let line_starts = get_ecma_line_starts(sf.get());
    let num_lines = line_starts.len().max(1);

    let mut opaque_spans = Vec::new();
    let mut jsx_wrappers = Vec::new();
    collect_ast_spans(sf.as_node(), sf, len, &mut opaque_spans, &mut jsx_wrappers);
    opaque_spans.sort_by_key(|span| (span.start, span.end));
    jsx_wrappers.sort_by_key(|span| (span.0, span.1));

    let mut line_has_code = vec![false; num_lines];
    let mut line_has_comment = vec![false; num_lines];
    let comments = scan_tokens_and_comments(
        text,
        line_starts,
        &opaque_spans,
        &jsx_wrappers,
        &mut line_has_code,
        &mut line_has_comment,
    );

    let mut results = Vec::new();
    for comment in comments {
        if comment.is_jsdoc {
            continue;
        }
        let Some((eff_start, eff_end, is_jsx)) =
            standalone_span(text, line_starts, &comment, &jsx_wrappers)
        else {
            continue;
        };
        let Some(body) = extract_comment_body(text, &comment) else {
            continue;
        };

        let at = span_location(sf, rel_file, eff_start, eff_end);
        let end_pos = eff_end.saturating_sub(1).max(eff_start);
        let end_line = byte_to_line1(line_starts, end_pos);
        let next_code_line = find_next_code_line(end_line, &line_has_code, &line_has_comment);

        results.push(ScannedComment {
            kind: if comment.is_line {
                CommentKind::Line
            } else {
                CommentKind::Block
            },
            text: body,
            at,
            next_code_line,
            is_jsx,
        });
    }

    results
}

fn collect_ast_spans(
    node: P<Node>,
    sf: P<SourceFile>,
    text_len: u32,
    opaque_spans: &mut Vec<OpaqueSpan>,
    jsx_wrappers: &mut Vec<(u32, u32)>,
) {
    match node.kind() {
        Kind::StringLiteral
        | Kind::NoSubstitutionTemplateLiteral
        | Kind::TemplateHead
        | Kind::TemplateMiddle
        | Kind::TemplateTail
        | Kind::RegularExpressionLiteral => {
            let start = get_token_pos_of_node(node, sf, false).min(text_len);
            let end = node
                .end()
                .source_offset()
                .unwrap_or(start)
                .clamp(start, text_len);
            if end > start {
                opaque_spans.push(OpaqueSpan {
                    start,
                    end,
                    is_code: true,
                });
            }
            return;
        }
        Kind::JsxText | Kind::JsxTextAllWhiteSpaces => {
            let start = get_token_pos_of_node(node, sf, false).min(text_len);
            let end = node
                .end()
                .source_offset()
                .unwrap_or(start)
                .clamp(start, text_len);
            if end > start {
                let is_trivia_ws = node.kind() == Kind::JsxTextAllWhiteSpaces
                    || node.as_jsx_text().contains_only_trivia_white_spaces;
                opaque_spans.push(OpaqueSpan {
                    start,
                    end,
                    is_code: !is_trivia_ws,
                });
            }
            return;
        }
        Kind::JsxExpression if node.as_jsx_expression().expression.is_none() => {
            let start = get_token_pos_of_node(node, sf, false).min(text_len);
            let end = node
                .end()
                .source_offset()
                .unwrap_or(start)
                .clamp(start, text_len);
            if end >= start + 2 {
                let bytes = sf.text().as_bytes();
                if bytes.get(usize::try_from(start).unwrap_or(0)) == Some(&b'{')
                    && bytes.get(usize::try_from(end - 1).unwrap_or(0)) == Some(&b'}')
                {
                    jsx_wrappers.push((start, end));
                }
            }
        }
        _ => {}
    }

    node.for_each_child(&mut |child| {
        collect_ast_spans(child, sf, text_len, opaque_spans, jsx_wrappers);
        false
    });
}

fn scan_tokens_and_comments(
    text: &str,
    line_starts: &[u32],
    opaque_spans: &[OpaqueSpan],
    jsx_wrappers: &[(u32, u32)],
    line_has_code: &mut [bool],
    line_has_comment: &mut [bool],
) -> Vec<RawComment> {
    let bytes = text.as_bytes();
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    let mut pos: u32 = 0;
    let mut opaque_idx = 0usize;
    let mut jsx_idx = 0usize;
    let mut comments = Vec::new();

    if text.starts_with("#!") {
        while let Some(&b) = bytes.get(usize::try_from(pos).unwrap_or(usize::MAX)) {
            if b == b'\n' || b == b'\r' {
                break;
            }
            pos += 1;
        }
        if let Some(first) = line_has_comment.first_mut() {
            *first = true;
        }
    }

    while pos < len {
        while opaque_idx < opaque_spans.len() && opaque_spans[opaque_idx].end <= pos {
            opaque_idx += 1;
        }
        if let Some(span) = opaque_spans.get(opaque_idx)
            && pos >= span.start
            && pos < span.end
        {
            if span.is_code {
                mark_span_lines(line_starts, span.start, span.end, line_has_code);
            }
            pos = span.end;
            continue;
        }

        while jsx_idx < jsx_wrappers.len() && jsx_wrappers[jsx_idx].1 <= pos {
            jsx_idx += 1;
        }
        if let Some(&(j_start, j_end)) = jsx_wrappers.get(jsx_idx)
            && (pos == j_start || pos + 1 == j_end)
        {
            pos += 1;
            continue;
        }

        let pos_usize = usize::try_from(pos).unwrap_or(bytes.len());
        let Some(rest) = text.get(pos_usize..) else {
            break;
        };
        let Some(ch) = rest.chars().next() else {
            break;
        };

        if ch.is_whitespace() {
            pos += u32::try_from(ch.len_utf8()).unwrap_or(1);
            continue;
        }

        if rest.starts_with("//") {
            let start = pos;
            pos += 2;
            while let Some(slice) = text.get(usize::try_from(pos).unwrap_or(bytes.len())..) {
                let Some(next_ch) = slice.chars().next() else {
                    break;
                };
                if is_ecma_line_break(next_ch) {
                    break;
                }
                pos += u32::try_from(next_ch.len_utf8()).unwrap_or(1);
            }
            mark_span_lines(line_starts, start, pos.max(start + 1), line_has_comment);
            comments.push(RawComment {
                start,
                end: pos,
                is_line: true,
                is_jsdoc: false,
            });
            continue;
        }

        if rest.starts_with("/*") {
            let start = pos;
            let body_start = usize::try_from(pos + 2).unwrap_or(bytes.len());
            let end = bytes
                .get(body_start..)
                .and_then(|tail| tail.windows(2).position(|w| w == b"*/"))
                .map_or(len, |rel| pos + 2 + u32::try_from(rel).unwrap_or(0) + 2);
            let raw = text
                .get(usize::try_from(start).unwrap_or(0)..usize::try_from(end).unwrap_or(0))
                .unwrap_or("");
            let is_jsdoc = raw.starts_with("/**") && raw != "/**/";
            mark_span_lines(line_starts, start, end, line_has_comment);
            comments.push(RawComment {
                start,
                end,
                is_line: false,
                is_jsdoc,
            });
            pos = end;
            continue;
        }

        let char_len = u32::try_from(ch.len_utf8()).unwrap_or(1);
        mark_span_lines(line_starts, pos, pos + char_len, line_has_code);
        pos += char_len;
    }

    comments
}

fn mark_span_lines(line_starts: &[u32], start: u32, end: u32, flags: &mut [bool]) {
    if end <= start || flags.is_empty() {
        return;
    }
    let first_line = byte_to_line1(line_starts, start);
    let last_line = byte_to_line1(line_starts, end.saturating_sub(1).max(start));
    let first_idx = usize::try_from(first_line.saturating_sub(1)).unwrap_or(0);
    let last_idx = usize::try_from(last_line.saturating_sub(1))
        .unwrap_or(0)
        .min(flags.len().saturating_sub(1));
    for flag in &mut flags[first_idx..=last_idx] {
        *flag = true;
    }
}

fn standalone_span(
    text: &str,
    line_starts: &[u32],
    comment: &RawComment,
    jsx_wrappers: &[(u32, u32)],
) -> Option<(u32, u32, bool)> {
    let (eff_start, eff_end, is_jsx) = if let Some(&(j_start, j_end)) = jsx_wrappers
        .iter()
        .find(|&&(js, je)| comment.start > js && comment.end < je)
    {
        let before = text
            .get(
                usize::try_from(j_start + 1).unwrap_or(0)
                    ..usize::try_from(comment.start).unwrap_or(0),
            )
            .unwrap_or("");
        let after = text
            .get(usize::try_from(comment.end).unwrap_or(0)..usize::try_from(j_end - 1).unwrap_or(0))
            .unwrap_or("");
        if !before.chars().all(char::is_whitespace) || !after.chars().all(char::is_whitespace) {
            return None;
        }
        (j_start, j_end, true)
    } else {
        (comment.start, comment.end, false)
    };

    let start_line_idx = usize::try_from(byte_to_line1(line_starts, eff_start) - 1).unwrap_or(0);
    let line_start = line_starts.get(start_line_idx).copied().unwrap_or(0);
    let prefix = text
        .get(usize::try_from(line_start).unwrap_or(0)..usize::try_from(eff_start).unwrap_or(0))
        .unwrap_or("");
    if !prefix.chars().all(char::is_whitespace) {
        return None;
    }

    let end_pos = eff_end.saturating_sub(1).max(eff_start);
    let end_line_idx = usize::try_from(byte_to_line1(line_starts, end_pos) - 1).unwrap_or(0);
    let next_line_start = line_starts
        .get(end_line_idx + 1)
        .copied()
        .unwrap_or_else(|| u32::try_from(text.len()).unwrap_or(u32::MAX));
    let suffix = text
        .get(
            usize::try_from(eff_end).unwrap_or(0)
                ..usize::try_from(next_line_start).unwrap_or(text.len()),
        )
        .unwrap_or("");
    if !suffix.chars().all(char::is_whitespace) {
        return None;
    }

    Some((eff_start, eff_end, is_jsx))
}

fn extract_comment_body(text: &str, comment: &RawComment) -> Option<String> {
    let raw = text.get(
        usize::try_from(comment.start).unwrap_or(0)..usize::try_from(comment.end).unwrap_or(0),
    )?;
    if comment.is_line {
        return Some(raw.strip_prefix("//")?.trim().to_string());
    }

    let inner = raw.strip_prefix("/*")?;
    let inner = inner.strip_suffix("*/").unwrap_or(inner);
    let mut lines = Vec::new();
    for (idx, line) in split_ecma_lines(inner).enumerate() {
        let trimmed = if idx == 0 {
            line.trim()
        } else {
            let ws_trimmed = line.trim_start();
            ws_trimmed
                .strip_prefix('*')
                .map_or(ws_trimmed, str::trim)
                .trim()
        };
        if !trimmed.is_empty() {
            lines.push(trimmed);
        }
    }
    Some(lines.join(" "))
}

fn find_next_code_line(
    comment_end_line: u32,
    line_has_code: &[bool],
    line_has_comment: &[bool],
) -> Option<u32> {
    let start_idx = usize::try_from(comment_end_line).unwrap_or(usize::MAX);
    for idx in start_idx..line_has_code.len() {
        if line_has_code[idx] {
            return Some(u32::try_from(idx + 1).unwrap_or(u32::MAX));
        }
        if !line_has_comment[idx] {
            return None;
        }
    }
    None
}

fn byte_to_line1(line_starts: &[u32], byte_pos: u32) -> u32 {
    let idx = line_starts.partition_point(|&start| start <= byte_pos);
    u32::try_from(idx.max(1)).unwrap_or(u32::MAX)
}

const fn is_ecma_line_break(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn split_ecma_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '\u{2028}', '\u{2029}'])
        .flat_map(|segment| segment.split('\r'))
}
