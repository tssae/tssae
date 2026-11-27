use std::{
    borrow::Cow,
    collections::BTreeSet,
    sync::{Arc, RwLock},
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use tsrs_api::{Session, SessionOptions, session::SnapshotData};
use tsrs_ast::{Node, SourceFile, Symbol, get_source_file_of_node};
use tsrs_checker::{Signature, Type};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::{
    P,
    context::{CheckerLifetime, Context, with_checker_lifetime},
};
use tsrs_project::ID as ProjectId;
use tsrs_vfs::{FS, bundled, osvfs};
use tssae_analysis::ir::FunctionIr;
use tssae_common::{
    DeclarationIdentity, DeclarationSummaryData, DiagnosticLocation, EvidenceItem, FileId, GapItem,
    ImplementationFact, LocalSyntaxId, MemberTargetHierarchy, SemanticModel, SignatureId,
    SummarySource, SymbolId, SyntaxCatalog, SyntaxId, SyntaxScopeInfo, TypeId, TypeSummaryData,
    WriteSyntaxInfo,
};

use crate::{
    hierarchy::ProgramHierarchyIndex,
    impl_resolution::{
        FacadeImplementationInput, FacadeImplementationOutcome, FirstPartyResolutionSource,
        ResolvedPackageInfo, SourceMapCache, collect_resolved_packages_for_program,
    },
    ir_lower::{
        lower_scope_to_ir_with_index, normalize_cfg_scope_node, normalize_function_node,
        resolve_enclosing_cfg_owner, resolve_enclosing_function,
    },
    semantic_facade::{
        FacadeAssignability, FacadeCallSelect, FacadeDeclarationSelect, FacadeSignatureSelect,
        FacadeTypeSelect, NotACallError, SemanticFacade, SemanticModelBridge, SymbolRegistry,
    },
    syntax_view::{
        FileIdRegistry, FileSyntaxIndex, FindSyntaxFilter, ScannedComment, SyntaxLookupOptions,
        SyntaxView, SyntaxViewError, is_project_source_file, node_location, normalize_path,
        relative_to_cwd,
    },
};

/// Opaque `Copy` handle wrapping a `tsrs` AST syntax node without exposing `tsrs_ast` outside the boundary.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundaryNode(pub(crate) P<Node>);

/// Opaque `Copy` handle wrapping a `tsrs` symbol.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundarySymbol(pub(crate) P<Symbol>);

/// Opaque `Copy` handle wrapping a `tsrs` call/construct signature.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundarySignature(pub(crate) P<Signature>);

/// Opaque `Copy` handle wrapping a `tsrs` semantic type.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundaryType(pub(crate) P<Type>);

/// Opaque `Copy` handle wrapping a `tsrs` source file.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundarySourceFile(pub(crate) P<SourceFile>);

/// Opaque `Copy` handle wrapping a `tsrs` compiler `Program`.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct BoundaryProgram(pub(crate) &'static Program);

/// Opaque handle wrapping a `tsrs` project identifier.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundaryProjectId(pub(crate) ProjectId);

/// Opaque handle wrapping a `tsrs` compiler session.
#[derive(Clone)]
pub struct BoundarySession(pub(crate) Arc<Session>);

/// Opaque handle wrapping an immutable `tsrs` snapshot.
#[derive(Clone)]
pub struct BoundarySnapshotData(pub(crate) Arc<SnapshotData>);

/// Opaque mutable borrow of a `tsrs` type checker scoped to a query batch.
pub struct BoundaryChecker<'a>(pub(crate) &'a mut CheckerHandle);

impl BoundaryNode {
    #[must_use]
    pub fn source_file(self) -> Option<BoundarySourceFile> {
        get_source_file_of_node(self.0).map(BoundarySourceFile)
    }

    #[must_use]
    pub fn normalize_cfg_scope(self) -> Option<Self> {
        normalize_cfg_scope_node(self.0).map(Self)
    }

    #[must_use]
    pub fn normalize_function(self) -> Option<Self> {
        normalize_function_node(self.0).map(Self)
    }

    #[must_use]
    pub fn resolve_enclosing_function(self) -> Option<Self> {
        resolve_enclosing_function(self.0).map(Self)
    }

    #[must_use]
    pub fn resolve_enclosing_cfg_owner(self) -> Option<Self> {
        resolve_enclosing_cfg_owner(self.0).map(Self)
    }

    #[must_use]
    pub fn location(self, sf: BoundarySourceFile, rel_file: &str) -> DiagnosticLocation {
        node_location(sf.0, rel_file, self.0)
    }
}

impl BoundarySourceFile {
    #[must_use]
    pub fn rel_path(self, cwd: &str) -> String {
        relative_to_cwd(cwd, self.0.file_name())
    }

    #[must_use]
    pub fn addr(self) -> usize {
        std::ptr::from_ref(&*self.0) as usize
    }

    #[must_use]
    pub fn build_syntax_index(self, file_id: FileId, rel_file: String) -> FileSyntaxIndex {
        FileSyntaxIndex::build(file_id, rel_file, self.0)
    }
}

impl BoundarySession {
    #[must_use]
    pub fn new(cwd: &str) -> Self {
        let fs: Arc<dyn FS> = Arc::new(bundled::wrap_fs(osvfs::fs()));
        let session = Session::new(SessionOptions::new(
            cwd.to_string(),
            bundled::default_lib_path(),
            fs,
            false,
        ));
        Self(session)
    }

    pub fn close(&self) {
        self.0.close();
    }
}

impl BoundarySnapshotData {
    /// Looks up the compiled `Program` for `project_id` in this snapshot.
    ///
    /// # Errors
    /// Returns an error string when the project is missing or fails to yield a program.
    pub fn get_program(&self, project_id: &BoundaryProjectId) -> Result<BoundaryProgram, String> {
        self.0
            .get_program(&project_id.0)
            .map(BoundaryProgram)
            .map_err(|err| err.to_string())
    }

    #[must_use]
    pub fn workspace_programs(&self) -> Vec<BoundaryProgram> {
        self.0
            .snapshot
            .project_collection
            .projects()
            .iter()
            .filter_map(|proj| proj.get_program().map(BoundaryProgram))
            .collect()
    }

    #[must_use]
    pub fn project_program_dirs(&self, cwd: &str) -> Vec<(BoundaryProgram, String)> {
        self.0
            .snapshot
            .project_collection
            .projects()
            .iter()
            .filter_map(|proj| {
                let prog = proj.get_program()?;
                let cfg_rel = relative_to_cwd(cwd, proj.config_file_name());
                let dir = std::path::Path::new(&cfg_rel)
                    .parent()
                    .and_then(|p| p.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(".")
                    .to_string();
                Some((BoundaryProgram(prog), dir))
            })
            .collect()
    }

    #[must_use]
    pub fn source_file_addrs(&self, cwd: &str) -> FxHashMap<String, usize> {
        let mut addrs = FxHashMap::default();
        for proj in self.0.snapshot.project_collection.projects() {
            if let Some(prog) = proj.get_program() {
                for &sf in prog.get_source_files() {
                    let rel = relative_to_cwd(cwd, sf.file_name());
                    let addr = std::ptr::from_ref(&*sf) as usize;
                    addrs.insert(rel, addr);
                }
            }
        }
        addrs
    }
}

impl BoundaryProgram {
    pub fn with_type_checker<R>(self, f: impl FnOnce(&mut BoundaryChecker<'_>) -> R) -> R {
        // Hold one CheckerLifetime::API instance across every step in the batch so
        // handles, signatures, instantiated types, and assignability share one checker.
        let ctx = with_checker_lifetime(&Context::background(), CheckerLifetime::API);
        let mut checker = self.0.get_type_checker(&ctx);
        let mut wrapper = BoundaryChecker(&mut checker);
        f(&mut wrapper)
    }

    #[must_use]
    pub fn collect_resolved_packages(self, cwd: &str) -> Vec<ResolvedPackageInfo> {
        collect_resolved_packages_for_program(cwd, self.0)
    }
}

impl FileSyntaxIndex {
    #[must_use]
    pub fn boundary_syntax_id_for_node(&self, node: BoundaryNode) -> Option<SyntaxId> {
        self.syntax_id_for_node(node.0)
    }

    #[must_use]
    pub fn boundary_node_for_syntax_id(&self, syntax: SyntaxId) -> Option<BoundaryNode> {
        self.node_for_syntax_id(syntax).map(BoundaryNode)
    }

    #[must_use]
    pub fn boundary_local_id_for_node(&self, node: BoundaryNode) -> Option<LocalSyntaxId> {
        self.local_id_for_node(node.0)
    }
}

/// Lowers `scope_node` into function IR using an optional prebuilt [`FileSyntaxIndex`].
///
/// # Errors
/// Returns a [`GapItem`] if `scope_node` cannot be lowered to IR.
pub fn lower_scope_to_ir_boundary(
    cwd: &str,
    program: BoundaryProgram,
    checker: &mut BoundaryChecker<'_>,
    scope_node: BoundaryNode,
    local_scope: u32,
    file_index: Option<&FileSyntaxIndex>,
) -> Result<Arc<FunctionIr>, GapItem> {
    lower_scope_to_ir_with_index(
        cwd,
        program.0,
        checker.0,
        scope_node.0,
        local_scope,
        file_index,
    )
    .map(|ls| ls.ir)
}

/// Snapshot-scoped file catalog mapping `FileId`s to opaque `BoundarySourceFile` handles.
pub struct BoundaryProjectFileCatalog {
    pub sf_by_file_id: FxHashMap<FileId, BoundarySourceFile>,
    pub rel_paths_by_file_id: FxHashMap<FileId, String>,
    pub project_files: Vec<FileId>,
}

/// Builds the file catalog for `program` and sibling `workspace_programs`.
#[must_use]
pub fn build_boundary_project_file_catalog(
    cwd: &str,
    program: BoundaryProgram,
    workspace_programs: &[BoundaryProgram],
    file_ids: &Arc<RwLock<FileIdRegistry>>,
) -> BoundaryProjectFileCatalog {
    let mut sf_by_file_id = FxHashMap::default();
    let mut rel_paths_by_file_id = FxHashMap::default();
    let mut project_files = Vec::new();
    let mut seen_rel = FxHashSet::default();

    let mut reg = file_ids.write().ok();
    let mut register_sf = |sf: P<SourceFile>, is_proj: bool| {
        let rel = normalize_path(&relative_to_cwd(cwd, sf.file_name()));
        let Some(ref mut reg) = reg else {
            return;
        };
        let fid = reg.intern(&rel);
        sf_by_file_id.entry(fid).or_insert(BoundarySourceFile(sf));
        rel_paths_by_file_id
            .entry(fid)
            .or_insert_with(|| rel.clone());
        if is_proj && seen_rel.insert(rel) {
            project_files.push(fid);
        }
    };

    for &sf in program.0.get_source_files() {
        if !program.0.is_source_file_default_library(sf.path()) {
            register_sf(sf, is_project_source_file(program.0, sf));
        }
    }
    for &wp in workspace_programs {
        for &sf in wp.0.get_source_files() {
            if !wp.0.is_source_file_default_library(sf.path()) {
                register_sf(sf, false);
            }
        }
    }
    drop(reg);
    project_files.sort_by(|a, b| {
        rel_paths_by_file_id
            .get(a)
            .cmp(&rel_paths_by_file_id.get(b))
    });

    BoundaryProjectFileCatalog {
        sf_by_file_id,
        rel_paths_by_file_id,
        project_files,
    }
}

/// Direct child boundary syntax nodes attached to a matched or inspected AST node.
#[derive(Clone)]
pub struct BoundarySyntaxChildren {
    pub expression: Option<BoundaryNode>,
    pub arguments: Option<Vec<BoundaryNode>>,
    pub name: Option<BoundaryNode>,
    pub initializer: Option<BoundaryNode>,
    pub body: Option<BoundaryNode>,
    pub parameters: Option<Vec<BoundaryNode>>,
}

/// One matched syntax node produced by [`SyntaxView::find_boundary_syntax`].
#[derive(Clone)]
pub struct BoundarySyntaxMatch {
    pub node: BoundaryNode,
    pub kind: String,
    pub text: String,
    pub location: DiagnosticLocation,
    pub children: BoundarySyntaxChildren,
}

/// Inspected properties of a single syntax node for `syntaxOf`.
#[derive(Clone)]
pub struct BoundarySyntaxLookupMatch {
    pub node: BoundaryNode,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub location: Option<DiagnosticLocation>,
    pub expression: Option<BoundaryNode>,
    pub arguments: Option<Vec<BoundaryNode>>,
    pub name: Option<BoundaryNode>,
    pub initializer: Option<BoundaryNode>,
    pub body: Option<BoundaryNode>,
    pub parameters: Option<Vec<BoundaryNode>>,
}

impl SyntaxView<'_> {
    pub fn scan_boundary_program_comments(
        &self,
        program: BoundaryProgram,
        filter_set: Option<&BTreeSet<String>>,
        seen_files: &mut BTreeSet<String>,
        comments: &mut Vec<ScannedComment>,
    ) {
        self.scan_program_comments(program.0, filter_set, seen_files, comments);
    }

    /// Searches `program` for syntax nodes matching `filter`, returning opaque [`BoundaryNode`] handles.
    ///
    /// # Errors
    /// Returns [`SyntaxViewError`] when the filter specifies invalid kinds or ranges.
    pub fn find_boundary_syntax(
        &self,
        program: BoundaryProgram,
        filter: &FindSyntaxFilter<'_>,
    ) -> Result<Vec<BoundarySyntaxMatch>, SyntaxViewError> {
        let matches = self.find_syntax(program.0, filter)?;
        Ok(matches
            .into_iter()
            .map(|m| BoundarySyntaxMatch {
                node: BoundaryNode(m.node),
                kind: m.kind,
                text: m.text,
                location: m.location,
                children: BoundarySyntaxChildren {
                    expression: m.children.expression.map(BoundaryNode),
                    arguments: m
                        .children
                        .arguments
                        .map(|args| args.into_iter().map(BoundaryNode).collect()),
                    name: m.children.name.map(BoundaryNode),
                    initializer: m.children.initializer.map(BoundaryNode),
                    body: m.children.body.map(BoundaryNode),
                    parameters: m
                        .children
                        .parameters
                        .map(|params| params.into_iter().map(BoundaryNode).collect()),
                },
            })
            .collect())
    }

    #[must_use]
    pub fn inspect_boundary_node(
        &self,
        node: BoundaryNode,
        opts: &SyntaxLookupOptions,
    ) -> BoundarySyntaxLookupMatch {
        let inspected = self.inspect_node(node.0, opts);
        BoundarySyntaxLookupMatch {
            node: BoundaryNode(inspected.node),
            kind: inspected.kind,
            text: inspected.text,
            location: inspected.location,
            expression: inspected.expression.map(BoundaryNode),
            arguments: inspected
                .arguments
                .map(|args| args.into_iter().map(BoundaryNode).collect()),
            name: inspected.name.map(BoundaryNode),
            initializer: inspected.initializer.map(BoundaryNode),
            body: inspected.body.map(BoundaryNode),
            parameters: inspected
                .parameters
                .map(|params| params.into_iter().map(BoundaryNode).collect()),
        }
    }
}

/// Summarized TypeScript type with opaque boundary handles.
#[derive(Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Mirrors the four boolean type-classification flags exposed on public TypeAtResult."
)]
pub struct BoundaryTypeSummary {
    pub ty: BoundaryType,
    pub enclosing: Option<BoundaryNode>,
    pub type_text: String,
    pub is_union: bool,
    pub is_intersection: bool,
    pub is_literal: bool,
    pub literal_value: Option<Value>,
    pub union_members: Option<Vec<String>>,
    pub is_unresolved: bool,
    pub gap_at: Option<DiagnosticLocation>,
}

/// Target input variants accepted by [`BoundarySemanticFacade::resolve_declaration`].
#[derive(Clone, Copy)]
pub enum BoundaryDeclarationTarget {
    Node(BoundaryNode),
    Signature(BoundarySignature, Option<BoundaryNode>),
    Declaration(BoundaryNode, Option<BoundarySymbol>),
}

/// Result of resolving a declaration target through [`BoundarySemanticFacade`].
pub struct BoundaryDeclarationResolution {
    pub primary_decl: Option<BoundaryNode>,
    pub all_decls: Vec<BoundaryNode>,
    pub symbol: Option<BoundarySymbol>,
    pub target_loc: Option<DiagnosticLocation>,
    pub name: Option<String>,
    pub container_name: Option<String>,
    pub kind: Option<String>,
    pub location: Option<DiagnosticLocation>,
    pub is_external: Option<bool>,
    pub package_name: Option<String>,
    pub export_name: Option<String>,
}

/// Instantiated parameter metadata resolved for a call or signature.
pub struct BoundaryInstantiatedParameter {
    pub index: usize,
    pub parameter_name: Option<String>,
    pub is_rest: bool,
    pub type_summary: BoundaryTypeSummary,
}

/// Projected semantic fields for a call or new expression.
pub struct BoundaryCallProjection {
    pub gaps: Vec<GapItem>,
    pub location: Option<DiagnosticLocation>,
    pub callee: Option<BoundaryNode>,
    pub callee_declaration: Option<(BoundaryNode, Option<BoundarySymbol>)>,
    pub signature: Option<BoundarySignature>,
    pub arguments: Option<Vec<BoundaryNode>>,
    pub instantiated_parameter_types: Option<Vec<BoundaryInstantiatedParameter>>,
    pub return_type: Option<BoundaryTypeSummary>,
}

/// Projected signature parameter item.
pub struct BoundarySignatureParameter {
    pub name: String,
    pub declaration_node: Option<BoundaryNode>,
    pub symbol: BoundarySymbol,
    pub optional: bool,
    pub rest: bool,
}

/// Projected semantic fields for a call/construct signature.
pub struct BoundarySignatureProjection {
    pub gaps: Vec<GapItem>,
    pub signature_text: Option<String>,
    pub declaration: Option<(BoundaryNode, Option<BoundarySymbol>)>,
    pub location: Option<DiagnosticLocation>,
    pub syntax_node: Option<BoundaryNode>,
    pub parameters: Option<Vec<BoundarySignatureParameter>>,
    pub min_argument_count: Option<usize>,
    pub has_rest_parameter: Option<bool>,
    pub instantiated_parameter_types: Option<Vec<BoundaryInstantiatedParameter>>,
    pub return_type: Option<BoundaryTypeSummary>,
    pub is_instantiated: Option<bool>,
}

/// Projected property of a type.
pub struct BoundaryTypeProperty {
    pub name: String,
    pub optional: bool,
    pub declaration_node: Option<BoundaryNode>,
    pub symbol: BoundarySymbol,
}

/// Projected semantic fields for a TypeScript type.
pub struct BoundaryTypeProjection {
    pub summary: BoundaryTypeSummary,
    pub union_member_types: Option<Vec<BoundaryType>>,
    pub properties: Option<Vec<BoundaryTypeProperty>>,
    pub type_arguments: Option<Vec<BoundaryType>>,
}

/// Target input accepted by [`BoundarySemanticFacade::resolve_implementation`].
#[derive(Clone, Copy)]
pub enum BoundaryImplementationInput {
    Node(BoundaryNode),
    Declaration(BoundaryNode, Option<BoundarySymbol>),
}

/// Concrete first-party implementation target located in workspace source.
#[derive(Clone)]
pub struct BoundaryImplementationTarget {
    pub file: String,
    pub location: DiagnosticLocation,
    pub declaration_node: BoundaryNode,
    pub declaration_symbol: Option<BoundarySymbol>,
    pub export_name: Option<String>,
    pub package_name: Option<String>,
    pub resolution_source: FirstPartyResolutionSource,
    pub declaration_file: Option<String>,
}

/// Outcome of resolving an implementation target through [`BoundarySemanticFacade::resolve_implementation`].
#[derive(Clone)]
pub enum BoundaryImplementationOutcome {
    Resolved(BoundaryImplementationTarget),
    External {
        package_name: Option<String>,
        declaration_file: Option<String>,
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        declaration_file: Option<String>,
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

fn wrap_type_summary(summary: crate::semantic_facade::FacadeTypeSummary) -> BoundaryTypeSummary {
    BoundaryTypeSummary {
        ty: BoundaryType(summary.ty),
        enclosing: summary.enclosing.map(BoundaryNode),
        type_text: summary.type_text,
        is_union: summary.is_union,
        is_intersection: summary.is_intersection,
        is_literal: summary.is_literal,
        literal_value: summary.literal_value,
        union_members: summary.union_members,
        is_unresolved: summary.is_unresolved,
        gap_at: summary.gap_at,
    }
}

fn wrap_instantiated_params(
    params: Vec<crate::semantic_facade::FacadeInstantiatedParameter>,
) -> Vec<BoundaryInstantiatedParameter> {
    params
        .into_iter()
        .map(|p| BoundaryInstantiatedParameter {
            index: p.index,
            parameter_name: p.parameter_name,
            is_rest: p.is_rest,
            type_summary: wrap_type_summary(p.type_summary),
        })
        .collect()
}

fn wrap_call_projection(
    proj: crate::semantic_facade::FacadeCallProjection,
) -> BoundaryCallProjection {
    BoundaryCallProjection {
        gaps: proj.gaps,
        location: proj.location,
        callee: proj.callee.map(BoundaryNode),
        callee_declaration: proj
            .callee_declaration
            .map(|(n, s)| (BoundaryNode(n), s.map(BoundarySymbol))),
        signature: proj.signature.map(BoundarySignature),
        arguments: proj
            .arguments
            .map(|args| args.into_iter().map(BoundaryNode).collect()),
        instantiated_parameter_types: proj
            .instantiated_parameter_types
            .map(wrap_instantiated_params),
        return_type: proj.return_type.map(wrap_type_summary),
    }
}

/// Boundary facade wrapping [`SemanticFacade`] with opaque handles.
pub struct BoundarySemanticFacade<'a> {
    inner: SemanticFacade<'a>,
}

impl<'a> BoundarySemanticFacade<'a> {
    #[must_use]
    pub const fn new(
        cwd: &'a str,
        program: BoundaryProgram,
        checker: &'a mut BoundaryChecker<'_>,
    ) -> Self {
        Self {
            inner: SemanticFacade::new(cwd, program.0, checker.0),
        }
    }

    #[must_use]
    pub fn raw_signature_declaration_node(sig: BoundarySignature) -> Option<BoundaryNode> {
        SemanticFacade::raw_signature_declaration_node(sig.0).map(BoundaryNode)
    }

    pub fn type_at_location(&mut self, node: BoundaryNode) -> BoundaryTypeSummary {
        wrap_type_summary(self.inner.type_at_location(node.0))
    }

    pub fn resolve_declaration(
        &mut self,
        target: BoundaryDeclarationTarget,
        select: &FacadeDeclarationSelect,
    ) -> BoundaryDeclarationResolution {
        let facade_target = match target {
            BoundaryDeclarationTarget::Node(n) => {
                crate::semantic_facade::FacadeDeclarationTarget::Node(n.0)
            }
            BoundaryDeclarationTarget::Signature(s, c) => {
                crate::semantic_facade::FacadeDeclarationTarget::Signature(s.0, c.map(|n| n.0))
            }
            BoundaryDeclarationTarget::Declaration(n, s) => {
                crate::semantic_facade::FacadeDeclarationTarget::Declaration(
                    n.0,
                    s.map(|sym| sym.0),
                )
            }
        };
        let res = self.inner.resolve_declaration(facade_target, select);
        BoundaryDeclarationResolution {
            primary_decl: res.primary_decl.map(BoundaryNode),
            all_decls: res.all_decls.into_iter().map(BoundaryNode).collect(),
            symbol: res.symbol.map(BoundarySymbol),
            target_loc: res.target_loc,
            name: res.name,
            container_name: res.container_name,
            kind: res.kind,
            location: res.location,
            is_external: res.is_external,
            package_name: res.package_name,
            export_name: res.export_name,
        }
    }

    /// Resolves a call/new expression and projects the requested fields.
    ///
    /// # Errors
    /// Returns [`NotACallError`] when `node` is not a call or new expression.
    pub fn resolve_call(
        &mut self,
        node: BoundaryNode,
        select: &FacadeCallSelect,
    ) -> Result<BoundaryCallProjection, NotACallError> {
        self.inner
            .resolve_call(node.0, select)
            .map(wrap_call_projection)
    }

    pub fn project_call(
        &mut self,
        node: BoundaryNode,
        select: &FacadeCallSelect,
    ) -> BoundaryCallProjection {
        wrap_call_projection(self.inner.project_call(node.0, select))
    }

    pub fn project_signature(
        &mut self,
        sig: BoundarySignature,
        call_node: Option<BoundaryNode>,
        select: &FacadeSignatureSelect,
    ) -> BoundarySignatureProjection {
        let proj = self
            .inner
            .project_signature(sig.0, call_node.map(|n| n.0), select);
        BoundarySignatureProjection {
            gaps: proj.gaps,
            signature_text: proj.signature_text,
            declaration: proj
                .declaration
                .map(|(n, s)| (BoundaryNode(n), s.map(BoundarySymbol))),
            location: proj.location,
            syntax_node: proj.syntax_node.map(BoundaryNode),
            parameters: proj.parameters.map(|params| {
                params
                    .into_iter()
                    .map(|p| BoundarySignatureParameter {
                        name: p.name,
                        declaration_node: p.declaration_node.map(BoundaryNode),
                        symbol: BoundarySymbol(p.symbol),
                        optional: p.optional,
                        rest: p.rest,
                    })
                    .collect()
            }),
            min_argument_count: proj.min_argument_count,
            has_rest_parameter: proj.has_rest_parameter,
            instantiated_parameter_types: proj
                .instantiated_parameter_types
                .map(wrap_instantiated_params),
            return_type: proj.return_type.map(wrap_type_summary),
            is_instantiated: proj.is_instantiated,
        }
    }

    pub fn project_type(
        &mut self,
        ty: BoundaryType,
        enclosing: Option<BoundaryNode>,
        select: &FacadeTypeSelect,
    ) -> BoundaryTypeProjection {
        let proj = self
            .inner
            .project_type(ty.0, enclosing.map(|n| n.0), select);
        BoundaryTypeProjection {
            summary: wrap_type_summary(proj.summary),
            union_member_types: proj
                .union_member_types
                .map(|members| members.into_iter().map(BoundaryType).collect()),
            properties: proj.properties.map(|props| {
                props
                    .into_iter()
                    .map(|p| BoundaryTypeProperty {
                        name: p.name,
                        optional: p.optional,
                        declaration_node: p.declaration_node.map(BoundaryNode),
                        symbol: BoundarySymbol(p.symbol),
                    })
                    .collect()
            }),
            type_arguments: proj
                .type_arguments
                .map(|args| args.into_iter().map(BoundaryType).collect()),
        }
    }

    pub fn is_assignable_to(
        &mut self,
        source: BoundaryType,
        target: BoundaryType,
        enclosing: Option<BoundaryNode>,
    ) -> FacadeAssignability {
        self.inner
            .is_assignable_to(source.0, target.0, enclosing.map(|n| n.0))
    }

    pub fn resolve_implementation(
        &mut self,
        input: BoundaryImplementationInput,
        workspace_programs: &[BoundaryProgram],
        sourcemap_cache: &SourceMapCache,
    ) -> BoundaryImplementationOutcome {
        let facade_input = match input {
            BoundaryImplementationInput::Node(n) => FacadeImplementationInput::Node(n.0),
            BoundaryImplementationInput::Declaration(n, s) => {
                FacadeImplementationInput::Declaration(n.0, s.map(|sym| sym.0))
            }
        };
        let raw_programs: Vec<&'static Program> = workspace_programs.iter().map(|p| p.0).collect();
        match self
            .inner
            .resolve_implementation(facade_input, &raw_programs, sourcemap_cache)
        {
            FacadeImplementationOutcome::Resolved(target) => {
                BoundaryImplementationOutcome::Resolved(BoundaryImplementationTarget {
                    file: target.file,
                    location: target.location,
                    declaration_node: BoundaryNode(target.declaration_node),
                    declaration_symbol: target.declaration_symbol.map(BoundarySymbol),
                    export_name: target.export_name,
                    package_name: target.package_name,
                    resolution_source: target.resolution_source,
                    declaration_file: target.declaration_file,
                })
            }
            FacadeImplementationOutcome::External {
                package_name,
                declaration_file,
                evidence,
            } => BoundaryImplementationOutcome::External {
                package_name,
                declaration_file,
                evidence,
            },
            FacadeImplementationOutcome::Inconclusive {
                declaration_file,
                gaps,
                evidence,
            } => BoundaryImplementationOutcome::Inconclusive {
                declaration_file,
                gaps,
                evidence,
            },
        }
    }
}

/// Snapshot-scoped bridge exposing [`SemanticModel`] and [`SyntaxCatalog`] using opaque handles.
pub struct BoundarySemanticBridge<'a> {
    inner: SemanticModelBridge<'a>,
}

impl<'a> BoundarySemanticBridge<'a> {
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "Wires snapshot multi-file syntax indices, symbol registry, source-map cache, and workspace programs."
    )]
    pub fn with_snapshot_store(
        cwd: &'a str,
        program: BoundaryProgram,
        workspace_programs: &[BoundaryProgram],
        checker: &'a mut BoundaryChecker<'_>,
        file_ids: Arc<RwLock<FileIdRegistry>>,
        file_indices: &'a mut FxHashMap<FileId, Arc<FileSyntaxIndex>>,
        shared_symbols: &'a mut SymbolRegistry,
        sourcemap_cache: &'a SourceMapCache,
    ) -> Self {
        let raw_programs: Vec<&'static Program> = workspace_programs.iter().map(|p| p.0).collect();
        Self {
            inner: SemanticModelBridge::with_snapshot_store_cow(
                cwd,
                program.0,
                Cow::Owned(raw_programs),
                checker.0,
                file_ids,
                file_indices,
                shared_symbols,
                sourcemap_cache,
            ),
        }
    }

    pub fn set_cached_hierarchy(&self, index: Arc<ProgramHierarchyIndex>) {
        self.inner.set_cached_hierarchy(index);
    }

    #[must_use]
    pub fn cached_hierarchy(&self) -> Option<Arc<ProgramHierarchyIndex>> {
        self.inner.cached_hierarchy()
    }

    #[must_use]
    pub fn ensure_source_file_index(
        &self,
        sf: BoundarySourceFile,
    ) -> Option<(FileId, Arc<FileSyntaxIndex>)> {
        self.inner.ensure_source_file_index(sf.0)
    }

    /// Lowers `scope_node` using this bridge's checker and cached file syntax index.
    ///
    /// # Errors
    /// Returns a [`GapItem`] if `scope_node` has no source file or fails IR lowering.
    pub fn lower_scope_to_ir(
        &self,
        scope_node: BoundaryNode,
        local_scope: u32,
    ) -> Result<Arc<FunctionIr>, GapItem> {
        self.inner.lower_scope_to_ir(scope_node.0, local_scope)
    }

    #[must_use]
    pub fn intern_syntax(&self, node: BoundaryNode) -> Option<SyntaxId> {
        self.inner.intern_syntax(node.0)
    }

    #[must_use]
    pub fn intern_symbol(&self, symbol: BoundarySymbol) -> SymbolId {
        self.inner.intern_symbol(symbol.0)
    }

    #[must_use]
    pub fn node_for_syntax(&self, syntax: SyntaxId) -> Option<BoundaryNode> {
        self.inner.node_for_syntax(syntax).map(BoundaryNode)
    }

    #[must_use]
    pub fn symbol_for_id(&self, symbol: SymbolId) -> Option<BoundarySymbol> {
        self.inner.symbol_for_id(symbol).map(BoundarySymbol)
    }
}

impl SyntaxCatalog for BoundarySemanticBridge<'_> {
    fn syntax_location(&self, syntax: SyntaxId) -> Option<DiagnosticLocation> {
        self.inner.syntax_location(syntax)
    }

    fn syntax_text(&self, syntax: SyntaxId) -> Option<String> {
        self.inner.syntax_text(syntax)
    }

    fn syntax_kind(&self, syntax: SyntaxId) -> Option<String> {
        self.inner.syntax_kind(syntax)
    }

    fn file_path(&self, file: FileId) -> Option<&str> {
        self.inner.file_path(file)
    }
}

impl SemanticModel for BoundarySemanticBridge<'_> {
    fn symbol_at_syntax(&self, syntax: SyntaxId) -> Option<SymbolId> {
        self.inner.symbol_at_syntax(syntax)
    }

    fn type_at_syntax(&self, syntax: SyntaxId) -> Option<TypeSummaryData> {
        self.inner.type_at_syntax(syntax)
    }

    fn signature_at_syntax(&self, syntax: SyntaxId) -> Option<SignatureId> {
        self.inner.signature_at_syntax(syntax)
    }

    fn resolve_declaration(&self, syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        self.inner.resolve_declaration(syntax)
    }

    fn resolve_declaration_by_symbol(&self, symbol: SymbolId) -> Option<DeclarationSummaryData> {
        self.inner.resolve_declaration_by_symbol(symbol)
    }

    fn is_assignable_to(&self, source: TypeId, target: TypeId) -> bool {
        self.inner.is_assignable_to(source, target)
    }

    fn base_types(&self, ty: TypeId) -> Vec<TypeId> {
        self.inner.base_types(ty)
    }

    fn is_external_syntax(&self, syntax: SyntaxId) -> bool {
        self.inner.is_external_syntax(syntax)
    }

    fn member_target_hierarchy(
        &self,
        receiver_syntax: SyntaxId,
        member_name: Option<&str>,
        static_receiver: bool,
    ) -> Option<MemberTargetHierarchy> {
        self.inner
            .member_target_hierarchy(receiver_syntax, member_name, static_receiver)
    }

    fn class_hierarchy_for_symbol(&self, class_symbol: SymbolId) -> Option<MemberTargetHierarchy> {
        self.inner.class_hierarchy_for_symbol(class_symbol)
    }

    fn resolve_class_constructor(&self, class_syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        self.inner.resolve_class_constructor(class_syntax)
    }

    fn resolve_super_target(&self, call_syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        self.inner.resolve_super_target(call_syntax)
    }

    fn resolve_implementation_for_syntax(
        &self,
        decl_or_node_syntax: SyntaxId,
        call_site_syntax: Option<SyntaxId>,
    ) -> ImplementationFact {
        self.inner
            .resolve_implementation_for_syntax(decl_or_node_syntax, call_site_syntax)
    }

    fn check_call_missing_package_gap(&self, call_syntax: SyntaxId) -> Option<GapItem> {
        self.inner.check_call_missing_package_gap(call_syntax)
    }

    fn declaration_identity(&self, syntax: SyntaxId) -> Option<DeclarationIdentity> {
        self.inner.declaration_identity(syntax)
    }

    fn syntax_scope_info(&self, syntax: SyntaxId) -> Option<SyntaxScopeInfo> {
        self.inner.syntax_scope_info(syntax)
    }

    fn branch_statement_syntax(&self, condition_syntax: SyntaxId) -> Option<SyntaxId> {
        self.inner.branch_statement_syntax(condition_syntax)
    }

    fn write_syntax_info(&self, write_syntax: SyntaxId) -> Option<WriteSyntaxInfo> {
        self.inner.write_syntax_info(write_syntax)
    }

    fn record_summary_lookup(&self, source: Option<SummarySource>) {
        self.inner.record_summary_lookup(source);
    }
}
