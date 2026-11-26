use std::cell::RefCell;

use rustc_hash::FxHashMap;
use serde_json::Value;
use tsrs_ast::{Kind, Node, SourceFile, Symbol, SymbolFlags, get_source_file_of_node};
use tsrs_checker::{LiteralValue, Signature, Type, TypeDataTag, TypeFlags, TypeFormatFlags};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::{P, resolve_project_reference_path};
use tssae_common::{
    DeclarationSummaryData, DiagnosticLocation, GapItem, SemanticModel, SymbolId, SyntaxId, TypeId,
    TypeSummaryData, UnknownReason,
};

use crate::syntax_view::{
    SyntaxCatalogBridge, is_project_source_file, node_location, node_location_relative,
    relative_to_cwd, resolve_against_cwd, slice_source_text,
};

/// Parameter index selector forwarded from projection queries.
#[derive(Clone, Debug)]
pub enum FacadeParameterIndexSelection {
    All(bool),
    Indices(Vec<usize>),
}

impl FacadeParameterIndexSelection {
    #[must_use]
    pub fn requested_indices(&self, param_count: usize) -> Option<Vec<usize>> {
        match self {
            Self::All(false) => None,
            Self::All(true) => Some((0..param_count).collect()),
            Self::Indices(indices) => Some(indices.clone()),
        }
    }
}

/// Summarized TypeScript type produced by [`SemanticFacade`].
#[derive(Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Mirrors the four boolean type-classification flags exposed on public TypeAtResult."
)]
pub struct FacadeTypeSummary {
    pub ty: P<Type>,
    pub enclosing: Option<P<Node>>,
    pub type_text: String,
    pub is_union: bool,
    pub is_intersection: bool,
    pub is_literal: bool,
    pub literal_value: Option<Value>,
    pub union_members: Option<Vec<String>>,
    pub is_unresolved: bool,
    pub gap_at: Option<DiagnosticLocation>,
}

/// Target input variants accepted by [`SemanticFacade::resolve_declaration`].
#[derive(Clone, Copy)]
pub enum FacadeDeclarationTarget {
    Node(P<Node>),
    Signature(P<Signature>, Option<P<Node>>),
    Declaration(P<Node>, Option<P<Symbol>>),
}

/// Selection flags for [`SemanticFacade::resolve_declaration`].
#[derive(Clone, Copy, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Maps 1:1 to resolveDeclarations projection selection keys."
)]
pub struct FacadeDeclarationSelect {
    pub name: bool,
    pub container_name: bool,
    pub kind: bool,
    pub location: bool,
    pub is_external: bool,
    pub declarations: bool,
}

/// Result of resolving a declaration target through [`SemanticFacade`].
pub struct FacadeDeclarationResolution {
    pub primary_decl: Option<P<Node>>,
    pub all_decls: Vec<P<Node>>,
    pub symbol: Option<P<Symbol>>,
    pub target_loc: Option<DiagnosticLocation>,
    pub name: Option<String>,
    pub container_name: Option<String>,
    pub kind: Option<String>,
    pub location: Option<DiagnosticLocation>,
    pub is_external: Option<bool>,
}

/// Selection options for [`SemanticFacade::resolve_call`] and [`SemanticFacade::project_call`].
#[derive(Clone, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Maps 1:1 to resolveCalls/projectCalls projection selection keys."
)]
pub struct FacadeCallSelect {
    pub location: bool,
    pub callee: bool,
    pub callee_declaration: bool,
    pub signature: bool,
    pub arguments: bool,
    pub instantiated_parameter_types: Option<FacadeParameterIndexSelection>,
    pub return_type: bool,
}

/// Instantiated parameter metadata resolved for a call or signature.
pub struct FacadeInstantiatedParameter {
    pub index: usize,
    pub parameter_name: Option<String>,
    pub is_rest: bool,
    pub type_summary: FacadeTypeSummary,
}

/// Error returned when [`SemanticFacade::resolve_call`] is given a non-call syntax node.
pub struct NotACallError {
    pub kind_label: String,
    pub location: Option<DiagnosticLocation>,
}

/// Projected semantic fields for a call or new expression.
pub struct FacadeCallProjection {
    pub gaps: Vec<GapItem>,
    pub location: Option<DiagnosticLocation>,
    pub callee: Option<P<Node>>,
    pub callee_declaration: Option<(P<Node>, Option<P<Symbol>>)>,
    pub signature: Option<P<Signature>>,
    pub arguments: Option<Vec<P<Node>>>,
    pub instantiated_parameter_types: Option<Vec<FacadeInstantiatedParameter>>,
    pub return_type: Option<FacadeTypeSummary>,
}

/// Selection options for [`SemanticFacade::project_signature`].
#[derive(Clone, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Maps 1:1 to projectSignatures selection keys."
)]
pub struct FacadeSignatureSelect {
    pub signature_text: bool,
    pub declaration: bool,
    pub location: bool,
    pub syntax: bool,
    pub parameters: bool,
    pub min_argument_count: bool,
    pub has_rest_parameter: bool,
    pub instantiated_parameter_types: Option<FacadeParameterIndexSelection>,
    pub return_type: bool,
    pub is_instantiated: bool,
}

/// Projected signature parameter item.
pub struct FacadeSignatureParameter {
    pub name: String,
    pub declaration_node: Option<P<Node>>,
    pub symbol: P<Symbol>,
    pub optional: bool,
    pub rest: bool,
}

/// Projected semantic fields for a call/construct signature.
pub struct FacadeSignatureProjection {
    pub gaps: Vec<GapItem>,
    pub signature_text: Option<String>,
    pub declaration: Option<(P<Node>, Option<P<Symbol>>)>,
    pub location: Option<DiagnosticLocation>,
    pub syntax_node: Option<P<Node>>,
    pub parameters: Option<Vec<FacadeSignatureParameter>>,
    pub min_argument_count: Option<usize>,
    pub has_rest_parameter: Option<bool>,
    pub instantiated_parameter_types: Option<Vec<FacadeInstantiatedParameter>>,
    pub return_type: Option<FacadeTypeSummary>,
    pub is_instantiated: Option<bool>,
}

/// Selection options for [`SemanticFacade::project_type`].
#[derive(Clone, Copy, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Maps 1:1 to projectTypes selection keys."
)]
pub struct FacadeTypeSelect {
    pub type_text: bool,
    pub is_union: bool,
    pub is_intersection: bool,
    pub is_literal: bool,
    pub literal_value: bool,
    pub union_members: bool,
    pub union_member_types: bool,
    pub properties: bool,
    pub type_arguments: bool,
}

/// Projected property of a type.
pub struct FacadeTypeProperty {
    pub name: String,
    pub optional: bool,
    pub declaration_node: Option<P<Node>>,
    pub symbol: P<Symbol>,
}

/// Projected semantic fields for a TypeScript type.
pub struct FacadeTypeProjection {
    pub summary: FacadeTypeSummary,
    pub union_member_types: Option<Vec<P<Type>>>,
    pub properties: Option<Vec<FacadeTypeProperty>>,
    pub type_arguments: Option<Vec<P<Type>>>,
}

/// Result of checking type assignability via [`SemanticFacade::is_assignable_to`].
pub struct FacadeAssignability {
    pub assignable: bool,
    pub has_unresolved_type: bool,
    pub gap_at: Option<DiagnosticLocation>,
}

/// Resolved module information exposed by [`SemanticFacade`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedModuleSummary {
    pub resolved_file_name: String,
    pub is_external_library_import: bool,
    pub package_name: Option<String>,
}

/// Semantic facade encapsulating `Program` and `CheckerHandle` interactions.
pub struct SemanticFacade<'a> {
    cwd: &'a str,
    program: &'static Program,
    checker: &'a mut CheckerHandle,
}

impl<'a> SemanticFacade<'a> {
    #[must_use]
    pub const fn new(
        cwd: &'a str,
        program: &'static Program,
        checker: &'a mut CheckerHandle,
    ) -> Self {
        Self {
            cwd,
            program,
            checker,
        }
    }

    #[must_use]
    pub const fn cwd(&self) -> &'a str {
        self.cwd
    }

    #[must_use]
    pub const fn program(&self) -> &'static Program {
        self.program
    }

    pub(crate) const fn checker_mut(&mut self) -> &mut CheckerHandle {
        self.checker
    }

    /// Returns `true` when `sf` belongs to the analyzed project rather than libs, external deps, or references.
    #[must_use]
    pub fn is_project_source_file(&self, sf: P<SourceFile>) -> bool {
        is_project_source_file(self.program, sf)
    }

    #[must_use]
    pub fn is_external_library_file(&self, sf: P<SourceFile>) -> bool {
        self.program.is_source_file_from_external_library(sf)
    }

    #[must_use]
    pub fn is_default_library_file(&self, sf: P<SourceFile>) -> bool {
        self.program.is_source_file_default_library(sf.path())
    }

    #[must_use]
    pub fn is_from_project_reference(&self, sf: P<SourceFile>) -> bool {
        self.program.is_source_from_project_reference(sf.path())
    }

    #[must_use]
    pub fn parse_file_redirect(&self, file_name: &str) -> String {
        self.program.get_parse_file_redirect(file_name)
    }

    #[must_use]
    pub fn project_reference_configs(&self) -> Vec<String> {
        let mut refs = Vec::new();
        for reference in self.program.command_line().project_references() {
            let raw_child = resolve_project_reference_path(reference);
            let child_abs = resolve_against_cwd(self.cwd, &raw_child);
            let child_rel = relative_to_cwd(self.cwd, &child_abs);
            if !refs.contains(&child_rel) {
                refs.push(child_rel);
            }
        }
        refs
    }

    #[must_use]
    pub fn resolved_package_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .program
            .resolved_package_names()
            .keys()
            .iter()
            .cloned()
            .collect();
        names.sort();
        names
    }

    #[must_use]
    pub fn resolve_module_specifier(
        &self,
        sf: P<SourceFile>,
        specifier: P<Node>,
    ) -> Option<ResolvedModuleSummary> {
        let resolved = self
            .program
            .get_resolved_module_from_module_specifier(sf, specifier)?;
        let package_name = if resolved.package_id.name.is_empty() {
            None
        } else {
            Some(resolved.package_id.name.to_string())
        };
        Some(ResolvedModuleSummary {
            resolved_file_name: resolved.resolved_file_name.to_string(),
            is_external_library_import: resolved.is_external_library_import,
            package_name,
        })
    }

    #[must_use]
    pub fn signature_declaration_node(&mut self, sig: P<Signature>) -> Option<P<Node>> {
        if is_unresolved_signature(self.checker, sig) {
            None
        } else {
            sig.declaration()
                .or_else(|| sig.target().and_then(|t| t.declaration()))
        }
    }

    #[must_use]
    pub fn raw_signature_declaration_node(sig: P<Signature>) -> Option<P<Node>> {
        sig.declaration()
            .or_else(|| sig.target().and_then(|t| t.declaration()))
    }

    pub fn type_at_location(&mut self, node: P<Node>) -> FacadeTypeSummary {
        let ty = self.checker.get_type_at_location(node);
        let enclosing = node.parent().or(Some(node));
        let gap_at = node_location_relative(self.cwd, node);
        summarize_type(self.checker, ty, enclosing, gap_at)
    }

    pub fn summarize_type(&mut self, ty: P<Type>, enclosing: Option<P<Node>>) -> FacadeTypeSummary {
        let gap_at = enclosing.and_then(|n| node_location_relative(self.cwd, n));
        summarize_type(self.checker, ty, enclosing, gap_at)
    }

    pub fn resolve_declaration(
        &mut self,
        target: FacadeDeclarationTarget,
        select: &FacadeDeclarationSelect,
    ) -> FacadeDeclarationResolution {
        let (resolved, target_loc) = resolve_target_input(self.cwd, self.checker, target);

        let name = select
            .name
            .then(|| {
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
            })
            .flatten();

        let container_name = select
            .container_name
            .then(|| {
                resolved
                    .primary_decl
                    .and_then(resolve_declaration_container_name)
            })
            .flatten();

        let kind = select
            .kind
            .then(|| {
                resolved
                    .primary_decl
                    .map(|decl| format!("{:?}", decl.kind()))
            })
            .flatten();

        let location = select
            .location
            .then(|| {
                resolved
                    .primary_decl
                    .and_then(|decl| node_location_relative(self.cwd, decl))
            })
            .flatten();

        let is_external = select.is_external.then(|| {
            resolved.primary_decl.is_some_and(|decl| {
                get_source_file_of_node(decl).is_some_and(|sf| !self.is_project_source_file(sf))
            })
        });

        let all_decls = if select.declarations {
            resolved.all_decls
        } else {
            Vec::new()
        };

        FacadeDeclarationResolution {
            primary_decl: resolved.primary_decl,
            all_decls,
            symbol: resolved.symbol,
            target_loc,
            name,
            container_name,
            kind,
            location,
            is_external,
        }
    }

    /// Inspects a syntax target for `resolveCalls`.
    ///
    /// # Errors
    /// Returns [`NotACallError`] if `node` is not a `CallExpression` or `NewExpression`.
    pub fn resolve_call(
        &mut self,
        node: P<Node>,
        select: &FacadeCallSelect,
    ) -> Result<FacadeCallProjection, NotACallError> {
        if !matches!(node.kind(), Kind::CallExpression | Kind::NewExpression) {
            return Err(NotACallError {
                kind_label: format!("{:?}", node.kind()),
                location: node_location_relative(self.cwd, node),
            });
        }
        Ok(self.project_call(node, select))
    }

    pub fn project_call(
        &mut self,
        node: P<Node>,
        select: &FacadeCallSelect,
    ) -> FacadeCallProjection {
        let call_loc = node_location_relative(self.cwd, node);
        let mut gaps = Vec::new();

        let location = if select.location {
            call_loc.clone()
        } else {
            None
        };

        let callee = if select.callee {
            node.expression()
        } else {
            None
        };

        let callee_declaration = if select.callee_declaration {
            let resolved = resolve_symbol_for_node(self.checker, node);
            if let Some(decl) = resolved.primary_decl {
                Some((decl, resolved.symbol))
            } else {
                gaps.push(GapItem::from_reason(
                    UnknownReason::UnresolvedDeclaration,
                    "Could not resolve callee declaration for call.".to_string(),
                    call_loc.clone(),
                ));
                None
            }
        } else {
            None
        };

        let needs_sig =
            select.signature || select.instantiated_parameter_types.is_some() || select.return_type;
        let resolved_sig = if needs_sig {
            let sig = self.checker.get_resolved_signature_exported(node);
            if is_unresolved_signature(self.checker, sig) {
                gaps.push(GapItem::from_reason(
                    UnknownReason::UnresolvedSignature,
                    "Call signature could not be resolved.".to_string(),
                    call_loc.clone(),
                ));
                None
            } else {
                Some(sig)
            }
        } else {
            None
        };

        let signature = if select.signature { resolved_sig } else { None };
        let arguments = select.arguments.then(|| node.arguments().to_vec());

        let instantiated_parameter_types =
            select
                .instantiated_parameter_types
                .as_ref()
                .map(|param_sel| {
                    let Some(sig) = resolved_sig else {
                        return Vec::new();
                    };
                    collect_instantiated_param_types(
                        self.checker,
                        sig,
                        Some(node),
                        param_sel,
                        call_loc.as_ref(),
                        &mut gaps,
                    )
                });

        let return_type = if select.return_type {
            resolved_sig.map(|sig| {
                let ret_ty = self.checker.get_return_type_of_signature_exported(sig);
                summarize_type(self.checker, ret_ty, Some(node), call_loc.clone())
            })
        } else {
            None
        };

        FacadeCallProjection {
            gaps,
            location,
            callee,
            callee_declaration,
            signature,
            arguments,
            instantiated_parameter_types,
            return_type,
        }
    }

    pub fn project_signature(
        &mut self,
        sig: P<Signature>,
        call_node: Option<P<Node>>,
        select: &FacadeSignatureSelect,
    ) -> FacadeSignatureProjection {
        let mut gaps = Vec::new();
        let decl_node = Self::raw_signature_declaration_node(sig);
        let enclosing = call_node.or(decl_node);
        let gap_at = enclosing.and_then(|n| node_location_relative(self.cwd, n));

        let signature_text = select.signature_text.then(|| {
            self.checker
                .signature_to_string_ex(sig, enclosing, TypeFormatFlags::None, None)
        });

        let declaration = if select.declaration {
            decl_node.map(|decl| {
                let sym = decl.symbol().or_else(|| {
                    decl.name()
                        .and_then(|n| self.checker.get_symbol_at_location_exported(n))
                });
                (decl, sym)
            })
        } else {
            None
        };

        let location = if select.location {
            decl_node.and_then(|decl| node_location_relative(self.cwd, decl))
        } else {
            None
        };

        let syntax_node = if select.syntax { decl_node } else { None };
        let parameters = select.parameters.then(|| build_signature_parameters(sig));
        let min_argument_count = select.min_argument_count.then(|| sig.min_argument_count());
        let has_rest_parameter = select.has_rest_parameter.then(|| sig.has_rest_parameter());

        let instantiated_parameter_types =
            select.instantiated_parameter_types.as_ref().map(|sel| {
                collect_instantiated_param_types(
                    self.checker,
                    sig,
                    enclosing,
                    sel,
                    gap_at.as_ref(),
                    &mut gaps,
                )
            });

        let return_type = select.return_type.then(|| {
            let ret_ty = self.checker.get_return_type_of_signature_exported(sig);
            summarize_type(self.checker, ret_ty, enclosing, gap_at)
        });

        let is_instantiated = select.is_instantiated.then(|| sig.target().is_some());

        FacadeSignatureProjection {
            gaps,
            signature_text,
            declaration,
            location,
            syntax_node,
            parameters,
            min_argument_count,
            has_rest_parameter,
            instantiated_parameter_types,
            return_type,
            is_instantiated,
        }
    }

    pub fn project_type(
        &mut self,
        ty: P<Type>,
        enclosing: Option<P<Node>>,
        select: &FacadeTypeSelect,
    ) -> FacadeTypeProjection {
        let gap_at = enclosing.and_then(|n| node_location_relative(self.cwd, n));
        let summary = summarize_type(self.checker, ty, enclosing, gap_at);

        let union_member_types = select.union_member_types.then(|| {
            if ty.is_union() {
                ty.types().to_vec()
            } else {
                Vec::new()
            }
        });

        let properties = select.properties.then(|| {
            self.checker
                .get_properties_of_type_exported(ty)
                .iter()
                .map(|&prop_sym| {
                    let declaration_node = prop_sym
                        .value_declaration()
                        .or_else(|| prop_sym.declarations().first().copied());
                    FacadeTypeProperty {
                        name: prop_sym.name().to_string(),
                        optional: prop_sym.flags().intersects(SymbolFlags::Optional),
                        declaration_node,
                        symbol: prop_sym,
                    }
                })
                .collect()
        });

        let type_arguments = select.type_arguments.then(|| {
            if ty.try_as_type_reference().is_some() {
                self.checker.get_type_arguments_exported(ty).to_vec()
            } else {
                Vec::new()
            }
        });

        FacadeTypeProjection {
            summary,
            union_member_types,
            properties,
            type_arguments,
        }
    }

    pub fn is_assignable_to(
        &mut self,
        source_ty: P<Type>,
        target_ty: P<Type>,
        enclosing: Option<P<Node>>,
    ) -> FacadeAssignability {
        let has_unresolved_type =
            is_error_or_unresolved_type(source_ty) || is_error_or_unresolved_type(target_ty);
        let gap_at = if has_unresolved_type {
            enclosing.and_then(|n| node_location_relative(self.cwd, n))
        } else {
            None
        };
        let assignable = self
            .checker
            .is_type_assignable_to_exported(source_ty, target_ty);
        FacadeAssignability {
            assignable,
            has_unresolved_type,
            gap_at,
        }
    }
}

type InternedTypeMap = FxHashMap<TypeId, (P<Type>, Option<P<Node>>)>;

/// Snapshot-scoped bridge implementing [`SemanticModel`] over opaque handles.
///
/// Uses interior mutability for the underlying `CheckerHandle` and interning tables so
/// analysis passes can query read-only semantic facts via `&dyn SemanticModel`.
pub struct SemanticModelBridge<'a> {
    cwd: &'a str,
    program: &'static Program,
    checker: RefCell<&'a mut CheckerHandle>,
    syntax: RefCell<SyntaxCatalogBridge>,
    symbols: RefCell<FxHashMap<SymbolId, P<Symbol>>>,
    symbol_ids: RefCell<FxHashMap<P<Symbol>, SymbolId>>,
    types: RefCell<InternedTypeMap>,
    type_ids: RefCell<FxHashMap<P<Type>, TypeId>>,
}

impl<'a> SemanticModelBridge<'a> {
    #[must_use]
    pub fn new(cwd: &'a str, program: &'static Program, checker: &'a mut CheckerHandle) -> Self {
        Self {
            cwd,
            program,
            checker: RefCell::new(checker),
            syntax: RefCell::new(SyntaxCatalogBridge::new(cwd)),
            symbols: RefCell::new(FxHashMap::default()),
            symbol_ids: RefCell::new(FxHashMap::default()),
            types: RefCell::new(FxHashMap::default()),
            type_ids: RefCell::new(FxHashMap::default()),
        }
    }

    pub fn intern_syntax(&self, node: P<Node>) -> Option<SyntaxId> {
        self.syntax.borrow_mut().intern_node(node)
    }

    pub fn intern_symbol(&self, sym: P<Symbol>) -> SymbolId {
        if let Some(&id) = self.symbol_ids.borrow().get(&sym) {
            return id;
        }
        let next = u64::try_from(self.symbols.borrow().len()).unwrap_or(u64::MAX);
        let id = SymbolId(next);
        self.symbols.borrow_mut().insert(id, sym);
        self.symbol_ids.borrow_mut().insert(sym, id);
        id
    }

    pub fn intern_type(&self, ty: P<Type>, enclosing: Option<P<Node>>) -> TypeId {
        if let Some(&id) = self.type_ids.borrow().get(&ty) {
            return id;
        }
        let next = u32::try_from(self.types.borrow().len()).unwrap_or(u32::MAX);
        let id = TypeId(next);
        self.types.borrow_mut().insert(id, (ty, enclosing));
        self.type_ids.borrow_mut().insert(ty, id);
        id
    }
}

impl SemanticModel for SemanticModelBridge<'_> {
    fn symbol_at_syntax(&self, syntax: SyntaxId) -> Option<SymbolId> {
        let node = self.syntax.borrow().get_node(syntax)?;
        let sym = {
            let mut checker = self.checker.borrow_mut();
            let resolved = resolve_symbol_for_node(&mut checker, node).symbol;
            drop(checker);
            resolved?
        };
        Some(self.intern_symbol(sym))
    }

    fn type_at_syntax(&self, syntax: SyntaxId) -> Option<TypeSummaryData> {
        let node = self.syntax.borrow().get_node(syntax)?;
        let enclosing = node.parent().or(Some(node));
        let summary = {
            let mut checker = self.checker.borrow_mut();
            let ty = checker.get_type_at_location(node);
            let summary = summarize_type(&mut checker, ty, enclosing, None);
            drop(checker);
            summary
        };
        let type_id = self.intern_type(summary.ty, enclosing);
        Some(TypeSummaryData {
            type_id,
            type_text: summary.type_text,
            is_union: summary.is_union,
            is_intersection: summary.is_intersection,
            is_literal: summary.is_literal,
            literal_value: summary.literal_value,
            union_members: summary.union_members,
            is_unresolved: summary.is_unresolved,
        })
    }

    fn resolve_declaration(&self, syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        let node = self.syntax.borrow().get_node(syntax)?;
        let resolved = {
            let mut checker = self.checker.borrow_mut();
            let resolved = resolve_symbol_for_node(&mut checker, node);
            drop(checker);
            resolved
        };
        let primary_syntax = resolved
            .primary_decl
            .and_then(|decl| self.intern_syntax(decl));
        let symbol = resolved.symbol.map(|sym| self.intern_symbol(sym));
        let name = resolved.symbol.map(|s| s.name().to_string());
        let container_name = resolved
            .primary_decl
            .and_then(resolve_declaration_container_name);
        let kind = resolved
            .primary_decl
            .map(|decl| format!("{:?}", decl.kind()));
        let location = resolved
            .primary_decl
            .and_then(|decl| node_location_relative(self.cwd, decl));
        let is_external = resolved.primary_decl.is_some_and(|decl| {
            get_source_file_of_node(decl)
                .is_some_and(|sf| !is_project_source_file(self.program, sf))
        });
        Some(DeclarationSummaryData {
            symbol,
            primary_syntax,
            name,
            container_name,
            kind,
            location,
            is_external,
        })
    }

    fn is_assignable_to(&self, source: TypeId, target: TypeId) -> bool {
        let Some((source_ty, _)) = self.types.borrow().get(&source).copied() else {
            return false;
        };
        let Some((target_ty, _)) = self.types.borrow().get(&target).copied() else {
            return false;
        };
        self.checker
            .borrow_mut()
            .is_type_assignable_to_exported(source_ty, target_ty)
    }

    fn base_types(&self, ty: TypeId) -> Vec<TypeId> {
        let Some((raw_ty, enclosing)) = self.types.borrow().get(&ty).copied() else {
            return Vec::new();
        };
        let bases: Vec<P<Type>> = self
            .checker
            .borrow_mut()
            .get_base_types_exported(raw_ty)
            .to_vec();
        bases
            .into_iter()
            .map(|base| self.intern_type(base, enclosing))
            .collect()
    }

    fn is_external_syntax(&self, syntax: SyntaxId) -> bool {
        let Some(node) = self.syntax.borrow().get_node(syntax) else {
            return false;
        };
        get_source_file_of_node(node).is_some_and(|sf| !is_project_source_file(self.program, sf))
    }
}

pub(crate) struct ResolvedDeclarationNodes {
    pub(crate) primary_decl: Option<P<Node>>,
    pub(crate) all_decls: Vec<P<Node>>,
    pub(crate) symbol: Option<P<Symbol>>,
}

fn is_error_or_unresolved_type(ty: P<Type>) -> bool {
    if ty.data_tag() == TypeDataTag::Intrinsic {
        let name = ty.as_intrinsic_type().intrinsic_name();
        return matches!(name, "error" | "unresolved");
    }
    ty.flags().intersects(TypeFlags::Any) && ty.alias().is_some()
}

fn summarize_type(
    checker: &mut CheckerHandle,
    ty: P<Type>,
    enclosing: Option<P<Node>>,
    gap_at: Option<DiagnosticLocation>,
) -> FacadeTypeSummary {
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

    FacadeTypeSummary {
        ty,
        enclosing,
        is_unresolved: is_error_or_unresolved_type(ty),
        type_text,
        is_union,
        is_intersection,
        is_literal,
        literal_value,
        union_members,
        gap_at,
    }
}

pub(crate) fn unwrap_aliased_symbol(checker: &mut CheckerHandle, mut sym: P<Symbol>) -> P<Symbol> {
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

pub(crate) fn is_unresolved_signature(checker: &mut CheckerHandle, sig: P<Signature>) -> bool {
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

pub(crate) fn resolve_symbol_for_node(
    checker: &mut CheckerHandle,
    node: P<Node>,
) -> ResolvedDeclarationNodes {
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

        return ResolvedDeclarationNodes {
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
        return ResolvedDeclarationNodes {
            primary_decl,
            all_decls,
            symbol: Some(resolved_sym),
        };
    }

    if is_declaration_node(node) {
        return ResolvedDeclarationNodes {
            primary_decl: Some(node),
            all_decls: vec![node],
            symbol: node.symbol(),
        };
    }

    ResolvedDeclarationNodes {
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

fn resolve_target_input(
    cwd: &str,
    checker: &mut CheckerHandle,
    target_input: FacadeDeclarationTarget,
) -> (ResolvedDeclarationNodes, Option<DiagnosticLocation>) {
    match target_input {
        FacadeDeclarationTarget::Node(node) => {
            let loc = node_location_relative(cwd, node);
            (resolve_symbol_for_node(checker, node), loc)
        }
        FacadeDeclarationTarget::Signature(sig, call_node) => {
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
                ResolvedDeclarationNodes {
                    primary_decl: decl,
                    all_decls,
                    symbol: sym,
                },
                loc,
            )
        }
        FacadeDeclarationTarget::Declaration(node, symbol) => {
            let loc = node_location_relative(cwd, node);
            let all_decls = symbol
                .map(|s| s.declarations().to_vec())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| vec![node]);
            (
                ResolvedDeclarationNodes {
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

fn collect_instantiated_param_types(
    checker: &mut CheckerHandle,
    sig: P<Signature>,
    enclosing: Option<P<Node>>,
    param_sel: &FacadeParameterIndexSelection,
    gap_at: Option<&DiagnosticLocation>,
    gaps: &mut Vec<GapItem>,
) -> Vec<FacadeInstantiatedParameter> {
    let params = sig.parameters();
    let Some(indices) = param_sel.requested_indices(params.len()) else {
        return Vec::new();
    };
    let has_rest = sig.has_rest_parameter();
    let mut out = Vec::with_capacity(indices.len());

    for idx in indices {
        if idx >= params.len() && !has_rest {
            gaps.push(GapItem::from_reason(
                UnknownReason::ParameterOutOfBounds,
                format!(
                    "Requested parameter index {idx}, but signature has {} parameter(s).",
                    params.len()
                ),
                gap_at.cloned(),
            ));
            continue;
        }
        let param_symbol = params.get(idx).copied().or_else(|| {
            if has_rest {
                params.last().copied()
            } else {
                None
            }
        });
        let parameter_name = param_symbol.map(|s| s.name().to_string());
        let is_rest = has_rest && idx + 1 >= params.len();
        let param_ty = checker.get_type_at_position_exported(sig, idx);
        let type_summary = summarize_type(checker, param_ty, enclosing, gap_at.cloned());
        if type_summary.is_unresolved {
            gaps.push(GapItem::from_reason(
                UnknownReason::UnresolvedType,
                format!(
                    "Type resolved to an error or unresolved type ({}).",
                    type_summary.type_text
                ),
                type_summary.gap_at.clone(),
            ));
        }
        out.push(FacadeInstantiatedParameter {
            index: idx,
            parameter_name,
            is_rest,
            type_summary,
        });
    }

    out
}

fn build_signature_parameters(sig: P<Signature>) -> Vec<FacadeSignatureParameter> {
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
        param_items.push(FacadeSignatureParameter {
            name: param_sym.name().to_string(),
            declaration_node: param_decl,
            symbol: param_sym,
            optional: is_optional,
            rest: is_rest,
        });
    }
    param_items
}
