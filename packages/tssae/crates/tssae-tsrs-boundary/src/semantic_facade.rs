use std::{
    cell::{OnceCell, RefCell},
    sync::{Arc, RwLock},
};

use rustc_hash::FxHashMap;
use serde_json::Value;
use tsrs_ast::{
    Kind, Node, NodeFlags, SourceFile, Symbol, SymbolFlags, get_source_file_of_node,
    is_function_like_declaration,
};
use tsrs_checker::{
    LiteralValue, Signature, Type, TypeDataTag, TypeFlags, TypeFormatFlags, TypePredicateKind,
};
use tsrs_compiler::{CheckerHandle, Program};
use tsrs_core::{P, resolve_project_reference_path};
use tssae_analysis::ir::{ImplicitAccessorKind, TypeGuardSemantics};
use tssae_common::{
    DeclarationSummaryData, DiagnosticLocation, FileId, GapItem, ImplementationFact,
    ImplementationSourceKind, MemberTargetHierarchy, ResolvedImplementationFact, SemanticModel,
    SignatureId, SymbolId, SyntaxCatalog, SyntaxId, SyntaxScopeInfo, TypeId, TypeSummaryData,
    UnknownReason, WriteSyntaxInfo,
};

use crate::{
    hierarchy::{HierarchyInterner, HierarchyQueryEnv, ProgramHierarchyIndex},
    impl_resolution::{
        FacadeImplementationInput, FacadeImplementationOutcome, FirstPartyResolutionSource,
        SourceMapCache,
    },
    syntax_view::{
        FileIdRegistry, FileSyntaxIndex, SyntaxCatalogBridge, is_project_source_file,
        node_location, node_location_relative, node_text, relative_to_cwd, resolve_against_cwd,
        slice_source_text, unwrap_expr_wrappers,
    },
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
    /// Accepts explicit `CallExpression`, `NewExpression`, `TaggedTemplateExpression`,
    /// as well as implicit getter `PropertyAccessExpression`, setter `BinaryExpression`,
    /// and `using` `VariableDeclaration` sites.
    ///
    /// # Errors
    /// Returns [`NotACallError`] if `node` is not a supported call/construct/implicit-call site.
    pub fn resolve_call(
        &mut self,
        node: P<Node>,
        select: &FacadeCallSelect,
    ) -> Result<FacadeCallProjection, NotACallError> {
        let implicit_flavor = self.classify_implicit_call(node);
        if !matches!(
            node.kind(),
            Kind::CallExpression | Kind::NewExpression | Kind::TaggedTemplateExpression
        ) && implicit_flavor.is_none()
        {
            return Err(NotACallError {
                kind_label: format!("{:?}", node.kind()),
                location: node_location_relative(self.cwd, node),
            });
        }
        Ok(self.project_call_with_flavor(node, select, implicit_flavor))
    }

    pub fn project_call(
        &mut self,
        node: P<Node>,
        select: &FacadeCallSelect,
    ) -> FacadeCallProjection {
        let implicit_flavor = self.classify_implicit_call(node);
        self.project_call_with_flavor(node, select, implicit_flavor)
    }

    fn classify_implicit_call(&mut self, node: P<Node>) -> Option<ImplicitAccessorKind> {
        match node.kind() {
            Kind::PropertyAccessExpression => {
                if let Some(parent) = node.parent()
                    && parent.kind() == Kind::BinaryExpression
                    && parent.as_binary_expression().left() == node
                {
                    return None;
                }
                let sym = self
                    .checker
                    .get_symbol_at_location_exported(node)
                    .map(|s| unwrap_aliased_symbol(self.checker, s))
                    .filter(|&s| s != self.checker.get_unknown_symbol())?;
                sym.flags()
                    .intersects(SymbolFlags::GetAccessor)
                    .then_some(ImplicitAccessorKind::Getter)
            }
            Kind::BinaryExpression => {
                let bin = node.as_binary_expression();
                let op = bin.operator_token().kind();
                if !matches!(
                    op,
                    Kind::EqualsToken
                        | Kind::PlusEqualsToken
                        | Kind::MinusEqualsToken
                        | Kind::AsteriskEqualsToken
                        | Kind::SlashEqualsToken
                        | Kind::PercentEqualsToken
                        | Kind::BarBarEqualsToken
                        | Kind::AmpersandAmpersandEqualsToken
                        | Kind::QuestionQuestionEqualsToken
                ) {
                    return None;
                }
                let lhs = unwrap_expr_wrappers(bin.left());
                if lhs.kind() != Kind::PropertyAccessExpression {
                    return None;
                }
                let sym = self
                    .checker
                    .get_symbol_at_location_exported(lhs)
                    .map(|s| unwrap_aliased_symbol(self.checker, s))
                    .filter(|&s| s != self.checker.get_unknown_symbol())?;
                sym.flags()
                    .intersects(SymbolFlags::SetAccessor)
                    .then_some(ImplicitAccessorKind::Setter)
            }
            Kind::VariableDeclaration => {
                let list = node.parent()?;
                if list.kind() == Kind::VariableDeclarationList
                    && list
                        .flags()
                        .intersects(NodeFlags::Using | NodeFlags::AwaitUsing)
                {
                    // Reuse `Getter` sentinel in this private enum helper only to mark `using` as callable.
                    return Some(ImplicitAccessorKind::Getter);
                }
                None
            }
            _ => None,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Projects both explicit call/new/tagged-template expressions and implicit accessor/`using` call sites."
    )]
    fn project_call_with_flavor(
        &mut self,
        node: P<Node>,
        select: &FacadeCallSelect,
        implicit_flavor: Option<ImplicitAccessorKind>,
    ) -> FacadeCallProjection {
        let call_loc = node_location_relative(self.cwd, node);
        let mut gaps = Vec::new();

        let location = if select.location {
            call_loc.clone()
        } else {
            None
        };

        let is_explicit = matches!(
            node.kind(),
            Kind::CallExpression | Kind::NewExpression | Kind::TaggedTemplateExpression
        );

        let callee = if select.callee {
            if node.kind() == Kind::TaggedTemplateExpression {
                Some(node.as_tagged_template_expression().tag())
            } else if node.kind() == Kind::BinaryExpression {
                Some(unwrap_expr_wrappers(node.as_binary_expression().left()))
            } else if node.kind() == Kind::PropertyAccessExpression
                || node.kind() == Kind::VariableDeclaration
            {
                Some(node)
            } else {
                node.expression()
            }
        } else {
            None
        };

        let callee_declaration = if select.callee_declaration {
            let resolved = resolve_implicit_or_call_decl(self.checker, node, implicit_flavor);
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

        let needs_sig = is_explicit
            && (select.signature
                || select.instantiated_parameter_types.is_some()
                || select.return_type);
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
        let arguments = select.arguments.then(|| {
            if node.kind() == Kind::TaggedTemplateExpression {
                vec![node.as_tagged_template_expression().template()]
            } else if node.kind() == Kind::BinaryExpression {
                vec![node.as_binary_expression().right()]
            } else if matches!(
                node.kind(),
                Kind::PropertyAccessExpression | Kind::VariableDeclaration
            ) {
                Vec::new()
            } else {
                node.arguments().to_vec()
            }
        });

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

/// Snapshot-scoped registry assigning stable [`SymbolId`]s across multiple analysis queries.
#[derive(Default)]
pub struct SymbolRegistry {
    symbols: FxHashMap<SymbolId, P<Symbol>>,
    symbol_ids: FxHashMap<P<Symbol>, SymbolId>,
}

impl SymbolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, sym: P<Symbol>) -> SymbolId {
        if let Some(&id) = self.symbol_ids.get(&sym) {
            return id;
        }
        let next = u64::try_from(self.symbols.len()).unwrap_or(u64::MAX);
        let id = SymbolId(next);
        self.symbols.insert(id, sym);
        self.symbol_ids.insert(sym, id);
        id
    }

    #[must_use]
    pub fn get(&self, id: SymbolId) -> Option<P<Symbol>> {
        self.symbols.get(&id).copied()
    }
}

/// Snapshot-scoped bridge implementing [`SemanticModel`] and [`SyntaxCatalog`] over opaque handles.
///
/// Uses interior mutability for the underlying `CheckerHandle` and interning tables so
/// analysis passes can query read-only semantic facts via `&dyn SemanticModel`.
pub struct SemanticModelBridge<'a> {
    cwd: &'a str,
    program: &'static Program,
    workspace_programs: &'a [&'static Program],
    checker: RefCell<&'a mut CheckerHandle>,
    syntax: RefCell<SyntaxCatalogBridge>,
    file_index: Option<(&'a FileSyntaxIndex, FileId)>,
    file_ids: Option<Arc<RwLock<FileIdRegistry>>>,
    file_indices: Option<RefCell<&'a mut FxHashMap<FileId, Arc<FileSyntaxIndex>>>>,
    sourcemap_cache: Option<&'a SourceMapCache>,
    hierarchy_index: OnceCell<ProgramHierarchyIndex>,
    shared_symbols: Option<RefCell<&'a mut SymbolRegistry>>,
    local_symbols: RefCell<SymbolRegistry>,
    types: RefCell<InternedTypeMap>,
    type_ids: RefCell<FxHashMap<P<Type>, TypeId>>,
    signatures: RefCell<FxHashMap<SignatureId, P<Signature>>>,
    signature_ids: RefCell<FxHashMap<P<Signature>, SignatureId>>,
}

impl<'a> SemanticModelBridge<'a> {
    #[must_use]
    pub fn new(cwd: &'a str, program: &'static Program, checker: &'a mut CheckerHandle) -> Self {
        Self {
            cwd,
            program,
            workspace_programs: &[],
            checker: RefCell::new(checker),
            syntax: RefCell::new(SyntaxCatalogBridge::new(cwd)),
            file_index: None,
            file_ids: None,
            file_indices: None,
            sourcemap_cache: None,
            hierarchy_index: OnceCell::new(),
            shared_symbols: None,
            local_symbols: RefCell::new(SymbolRegistry::new()),
            types: RefCell::new(FxHashMap::default()),
            type_ids: RefCell::new(FxHashMap::default()),
            signatures: RefCell::new(FxHashMap::default()),
            signature_ids: RefCell::new(FxHashMap::default()),
        }
    }

    #[must_use]
    pub fn with_file_index(
        cwd: &'a str,
        program: &'static Program,
        checker: &'a mut CheckerHandle,
        file_index: &'a FileSyntaxIndex,
    ) -> Self {
        let mut bridge = Self::new(cwd, program, checker);
        bridge.file_index = Some((file_index, file_index.file_id));
        bridge
    }

    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "Wires snapshot multi-file syntax indices, symbol registry, source-map cache, and workspace programs."
    )]
    pub fn with_snapshot_store(
        cwd: &'a str,
        program: &'static Program,
        workspace_programs: &'a [&'static Program],
        checker: &'a mut CheckerHandle,
        file_ids: Arc<RwLock<FileIdRegistry>>,
        file_indices: &'a mut FxHashMap<FileId, Arc<FileSyntaxIndex>>,
        shared_symbols: &'a mut SymbolRegistry,
        sourcemap_cache: &'a SourceMapCache,
    ) -> Self {
        Self {
            cwd,
            program,
            workspace_programs,
            checker: RefCell::new(checker),
            syntax: RefCell::new(SyntaxCatalogBridge::new(cwd)),
            file_index: None,
            file_ids: Some(file_ids),
            file_indices: Some(RefCell::new(file_indices)),
            sourcemap_cache: Some(sourcemap_cache),
            hierarchy_index: OnceCell::new(),
            shared_symbols: Some(RefCell::new(shared_symbols)),
            local_symbols: RefCell::new(SymbolRegistry::new()),
            types: RefCell::new(FxHashMap::default()),
            type_ids: RefCell::new(FxHashMap::default()),
            signatures: RefCell::new(FxHashMap::default()),
            signature_ids: RefCell::new(FxHashMap::default()),
        }
    }

    #[must_use]
    pub const fn cwd(&self) -> &'a str {
        self.cwd
    }

    pub fn ensure_source_file_index(
        &self,
        sf: P<SourceFile>,
    ) -> Option<(FileId, Arc<FileSyntaxIndex>)> {
        let fid = self.ensure_file_index_for_sf(sf)?;
        let idx = self.file_indices.as_ref()?.borrow().get(&fid).cloned()?;
        Some((fid, idx))
    }

    #[must_use]
    pub fn get_file_syntax_index(&self, file_id: FileId) -> Option<Arc<FileSyntaxIndex>> {
        if let Some((_, fid)) = self.file_index
            && fid == file_id
        {
            return None;
        }
        self.file_indices.as_ref()?.borrow().get(&file_id).cloned()
    }

    /// Lowers `scope_node` using this bridge's checker and cached file syntax index.
    ///
    /// # Errors
    /// Returns a [`GapItem`] if `scope_node` has no source file or fails IR lowering.
    pub fn lower_scope_to_ir(
        &self,
        scope_node: P<Node>,
        local_scope: u32,
    ) -> Result<Arc<tssae_analysis::ir::FunctionIr>, GapItem> {
        let sf = get_source_file_of_node(scope_node).ok_or_else(|| {
            GapItem::from_reason(
                UnknownReason::MissingSourceFile,
                "Scope node has no source file.".to_string(),
                None,
            )
        })?;
        let file_index = self.ensure_source_file_index(sf).map(|(_, idx)| idx);
        let mut checker = self.checker.borrow_mut();
        if matches!(
            scope_node.kind(),
            Kind::ClassDeclaration | Kind::ClassExpression
        ) {
            return crate::ir_lower::lower_class_default_constructor_to_ir(
                self.cwd,
                self.program,
                &mut checker,
                scope_node,
                local_scope,
                file_index.as_deref(),
            )
            .map(|ls| ls.ir);
        }
        crate::ir_lower::lower_scope_to_ir_with_index(
            self.cwd,
            self.program,
            &mut checker,
            scope_node,
            local_scope,
            file_index.as_deref(),
        )
        .map(|ls| ls.ir)
    }

    fn ensure_file_index_for_sf(&self, sf: P<SourceFile>) -> Option<FileId> {
        let file_ids = self.file_ids.as_ref()?;
        let rel_file = relative_to_cwd(self.cwd, sf.file_name());
        let file_id = file_ids.write().ok()?.intern(&rel_file);
        let sf_addr = std::ptr::from_ref(&*sf) as usize;
        if let Some(indices_cell) = &self.file_indices {
            let mut indices = indices_cell.borrow_mut();
            let needs_rebuild = indices
                .get(&file_id)
                .is_none_or(|existing| existing.source_file_addr != sf_addr);
            if needs_rebuild {
                let built = Arc::new(FileSyntaxIndex::build(file_id, rel_file, sf));
                indices.insert(file_id, built);
            }
        }
        Some(file_id)
    }

    pub fn intern_syntax(&self, node: P<Node>) -> Option<SyntaxId> {
        if let Some((index, _)) = self.file_index
            && let Some(sid) = index.syntax_id_for_node(node)
        {
            return Some(sid);
        }
        if let (Some(_), Some(indices_cell)) = (&self.file_ids, &self.file_indices)
            && let Some(sf) = get_source_file_of_node(node)
            && let Some(fid) = self.ensure_file_index_for_sf(sf)
            && let Some(sid) = indices_cell
                .borrow()
                .get(&fid)
                .and_then(|idx| idx.syntax_id_for_node(node))
        {
            return Some(sid);
        }
        self.syntax.borrow_mut().intern_node(node)
    }

    #[must_use]
    pub fn node_for_syntax(&self, syntax: SyntaxId) -> Option<P<Node>> {
        if let Some((index, fid)) = self.file_index
            && syntax.file_id() == fid
            && let Some(node) = index.node_for_syntax_id(syntax)
        {
            return Some(node);
        }
        if let Some(indices_cell) = &self.file_indices
            && let Some(node) = indices_cell
                .borrow()
                .get(&syntax.file_id())
                .and_then(|idx| idx.node_for_syntax_id(syntax))
        {
            return Some(node);
        }
        self.syntax.borrow().get_node(syntax)
    }

    #[must_use]
    pub fn symbol_for_id(&self, symbol: SymbolId) -> Option<P<Symbol>> {
        if let Some(shared) = &self.shared_symbols {
            return shared.borrow().get(symbol);
        }
        self.local_symbols.borrow().get(symbol)
    }

    fn hierarchy_index(&self) -> &ProgramHierarchyIndex {
        self.hierarchy_index.get_or_init(|| {
            let mut checker = self.checker.borrow_mut();
            ProgramHierarchyIndex::build(
                self.cwd,
                self.program,
                self.workspace_programs,
                &mut checker,
            )
        })
    }

    fn summarize_decl(
        &self,
        decl_node: Option<P<Node>>,
        sym_opt: Option<P<Symbol>>,
    ) -> DeclarationSummaryData {
        let primary_syntax = decl_node.and_then(|decl| self.intern_syntax(decl));
        let symbol = sym_opt.map(|sym| self.intern_symbol(sym));
        let raw_name = decl_node
            .and_then(|decl| {
                decl.name().and_then(node_text).or_else(|| {
                    (decl.kind() == Kind::Constructor).then(|| "constructor".to_string())
                })
            })
            .or_else(|| {
                sym_opt
                    .map(|s| s.name().to_string())
                    .filter(|n| !matches!(n.as_str(), "__function" | "__class" | "unknown"))
            });
        let container_name = decl_node.and_then(resolve_declaration_container_name);
        let kind = decl_node.map(|decl| format!("{:?}", decl.kind()));
        let location = decl_node.and_then(|decl| node_location_relative(self.cwd, decl));
        let decl_sf = decl_node.and_then(get_source_file_of_node);
        let is_default_library =
            decl_sf.is_some_and(|sf| self.program.is_source_file_default_library(sf.path()));
        let is_external = decl_sf.is_some_and(|sf| {
            !is_project_source_file(self.program, sf)
                && !crate::impl_resolution::is_first_party_workspace_path(self.cwd, sf.file_name())
        });
        let is_signature_only = decl_node.is_some_and(|decl| {
            matches!(
                decl.kind(),
                Kind::MethodSignature
                    | Kind::CallSignature
                    | Kind::ConstructSignature
                    | Kind::FunctionType
                    | Kind::ConstructorType
                    | Kind::PropertySignature
                    | Kind::Parameter
            )
        });
        let callable_node = decl_node.and_then(extract_callable_from_decl_node);
        let has_body = decl_node.is_some_and(|decl| {
            if is_function_like_declaration(Some(decl)) {
                return decl.body().is_some();
            }
            if matches!(decl.kind(), Kind::ClassDeclaration | Kind::ClassExpression) {
                return decl_sf.is_some_and(|sf| !sf.is_declaration_file());
            }
            callable_node.is_some_and(|f| f.body().is_some())
        });
        let initializer_syntax = decl_node
            .and_then(|decl| {
                matches!(
                    decl.kind(),
                    Kind::VariableDeclaration
                        | Kind::PropertyDeclaration
                        | Kind::PropertyAssignment
                )
                .then(|| decl.initializer().map(unwrap_expr_wrappers))
                .flatten()
            })
            .and_then(|init| self.intern_syntax(init));
        let callable_syntax = callable_node.and_then(|c| self.intern_syntax(c));

        DeclarationSummaryData {
            symbol,
            primary_syntax,
            name: raw_name,
            container_name,
            kind,
            location,
            is_external,
            is_default_library,
            has_body,
            is_signature_only,
            initializer_syntax,
            callable_syntax,
        }
    }

    #[must_use]
    pub fn symbol_for_node_raw(&self, node: P<Node>) -> Option<P<Symbol>> {
        let mut checker = self.checker.borrow_mut();
        resolve_symbol_for_node(&mut checker, node).symbol
    }

    #[must_use]
    pub fn is_global_undefined(&self, node: P<Node>) -> bool {
        if node.kind() != Kind::Identifier || node_text(node).as_deref() != Some("undefined") {
            return false;
        }
        let mut checker = self.checker.borrow_mut();
        let sym_opt = checker
            .get_symbol_at_location_exported(node)
            .map(|s| unwrap_aliased_symbol(&mut checker, s))
            .filter(|&s| s != checker.get_unknown_symbol());
        let ty_is_undefined = checker
            .get_type_at_location(node)
            .flags()
            .intersects(TypeFlags::Undefined);
        drop(checker);
        let Some(sym) = sym_opt else {
            return ty_is_undefined;
        };
        let decls = unwrap_sym_declarations(sym);
        if decls.is_empty() {
            return ty_is_undefined;
        }
        decls.iter().any(|&decl| {
            get_source_file_of_node(decl)
                .is_some_and(|sf| self.program.is_source_file_default_library(sf.path()))
        })
    }

    #[must_use]
    pub fn property_accessor_flags(&self, node: P<Node>) -> Option<ImplicitAccessorKind> {
        let mut checker = self.checker.borrow_mut();
        let sym = checker
            .get_symbol_at_location_exported(node)
            .or_else(|| {
                node.name()
                    .and_then(|name_node| checker.get_symbol_at_location_exported(name_node))
            })
            .map(|s| unwrap_aliased_symbol(&mut checker, s))
            .filter(|&s| s != checker.get_unknown_symbol())?;
        drop(checker);
        let flags = sym.flags();
        if flags.intersects(SymbolFlags::GetAccessor) {
            Some(ImplicitAccessorKind::Getter)
        } else if flags.intersects(SymbolFlags::SetAccessor) {
            Some(ImplicitAccessorKind::Setter)
        } else {
            None
        }
    }

    #[must_use]
    pub fn has_get_accessor(&self, node: P<Node>) -> bool {
        let mut checker = self.checker.borrow_mut();
        checker
            .get_symbol_at_location_exported(node)
            .or_else(|| {
                node.name()
                    .and_then(|name_node| checker.get_symbol_at_location_exported(name_node))
            })
            .map(|s| unwrap_aliased_symbol(&mut checker, s))
            .filter(|&s| s != checker.get_unknown_symbol())
            .is_some_and(|s| s.flags().intersects(SymbolFlags::GetAccessor))
    }

    #[must_use]
    pub fn has_set_accessor(&self, node: P<Node>) -> bool {
        let mut checker = self.checker.borrow_mut();
        checker
            .get_symbol_at_location_exported(node)
            .or_else(|| {
                node.name()
                    .and_then(|name_node| checker.get_symbol_at_location_exported(name_node))
            })
            .map(|s| unwrap_aliased_symbol(&mut checker, s))
            .filter(|&s| s != checker.get_unknown_symbol())
            .is_some_and(|s| s.flags().intersects(SymbolFlags::SetAccessor))
    }

    #[must_use]
    pub fn type_guard_for_function(&self, fn_node: P<Node>) -> Option<TypeGuardSemantics> {
        let mut checker = self.checker.borrow_mut();
        let sig = checker.get_signature_from_declaration_exported(fn_node);
        if is_unresolved_signature(&mut checker, sig) {
            return None;
        }
        let pred = checker.get_type_predicate_of_signature_exported(sig)?;
        let narrowed_type_text = pred.type_().map(|ty| {
            checker.type_to_string_ex_exported(ty, Some(fn_node), TypeFormatFlags::None, None)
        });
        drop(checker);
        match pred.kind() {
            TypePredicateKind::This => Some(TypeGuardSemantics::ThisPredicate {
                is_asserts: false,
                narrowed_type_text,
            }),
            TypePredicateKind::AssertsThis => Some(TypeGuardSemantics::ThisPredicate {
                is_asserts: true,
                narrowed_type_text,
            }),
            TypePredicateKind::Identifier => Some(TypeGuardSemantics::ParamPredicate {
                param_index: pred.parameter_index().unwrap_or(0),
                param_name: pred.parameter_name().to_string(),
                is_asserts: false,
                narrowed_type_text,
            }),
            TypePredicateKind::AssertsIdentifier => Some(TypeGuardSemantics::ParamPredicate {
                param_index: pred.parameter_index().unwrap_or(0),
                param_name: pred.parameter_name().to_string(),
                is_asserts: true,
                narrowed_type_text,
            }),
        }
    }

    pub fn intern_symbol(&self, sym: P<Symbol>) -> SymbolId {
        if let Some(shared) = &self.shared_symbols {
            return shared.borrow_mut().intern(sym);
        }
        self.local_symbols.borrow_mut().intern(sym)
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

    pub fn intern_signature(&self, sig: P<Signature>) -> SignatureId {
        if let Some(&id) = self.signature_ids.borrow().get(&sig) {
            return id;
        }
        let next = u32::try_from(self.signatures.borrow().len()).unwrap_or(u32::MAX);
        let id = SignatureId(next);
        self.signatures.borrow_mut().insert(id, sig);
        self.signature_ids.borrow_mut().insert(sig, id);
        id
    }

    pub fn symbol_id_for_node(&self, node: P<Node>) -> Option<SymbolId> {
        let sym = {
            let mut checker = self.checker.borrow_mut();
            let resolved = resolve_symbol_for_node(&mut checker, node).symbol;
            drop(checker);
            resolved?
        };
        Some(self.intern_symbol(sym))
    }

    pub fn type_id_for_node(&self, node: P<Node>) -> Option<TypeId> {
        let enclosing = node.parent().or(Some(node));
        let ty = {
            let mut checker = self.checker.borrow_mut();
            let ty = checker.get_type_at_location(node);
            drop(checker);
            ty
        };
        Some(self.intern_type(ty, enclosing))
    }

    pub fn signature_id_for_node(&self, node: P<Node>) -> Option<SignatureId> {
        if !matches!(
            node.kind(),
            Kind::CallExpression | Kind::NewExpression | Kind::TaggedTemplateExpression
        ) {
            return None;
        }
        let sig = self
            .checker
            .borrow_mut()
            .get_resolved_signature_exported(node);
        let unresolved = is_unresolved_signature(&mut self.checker.borrow_mut(), sig);
        if unresolved {
            None
        } else {
            Some(self.intern_signature(sig))
        }
    }
}

impl SyntaxCatalog for SemanticModelBridge<'_> {
    fn syntax_location(&self, syntax: SyntaxId) -> Option<DiagnosticLocation> {
        let node = self.node_for_syntax(syntax)?;
        node_location_relative(self.cwd, node)
    }

    fn syntax_text(&self, syntax: SyntaxId) -> Option<String> {
        let node = self.node_for_syntax(syntax)?;
        node_text(node)
    }

    fn syntax_kind(&self, syntax: SyntaxId) -> Option<String> {
        let node = self.node_for_syntax(syntax)?;
        Some(format!("{:?}", node.kind()))
    }

    fn file_path(&self, file: FileId) -> Option<&str> {
        if let Some((index, fid)) = self.file_index
            && fid == file
        {
            return Some(Box::leak(index.rel_file.clone().into_boxed_str()));
        }
        None
    }
}

impl SemanticModel for SemanticModelBridge<'_> {
    fn symbol_at_syntax(&self, syntax: SyntaxId) -> Option<SymbolId> {
        let node = self.node_for_syntax(syntax)?;
        let sym = {
            let mut checker = self.checker.borrow_mut();
            let resolved = resolve_symbol_for_node(&mut checker, node).symbol;
            drop(checker);
            resolved?
        };
        Some(self.intern_symbol(sym))
    }

    fn type_at_syntax(&self, syntax: SyntaxId) -> Option<TypeSummaryData> {
        let node = self.node_for_syntax(syntax)?;
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

    fn signature_at_syntax(&self, syntax: SyntaxId) -> Option<SignatureId> {
        let node = self.node_for_syntax(syntax)?;
        self.signature_id_for_node(node)
    }

    fn resolve_declaration(&self, syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        let node = self.node_for_syntax(syntax)?;
        let resolved = {
            let mut checker = self.checker.borrow_mut();
            let resolved = resolve_symbol_for_node(&mut checker, node);
            drop(checker);
            resolved
        };
        resolved.primary_decl.as_ref()?;
        Some(self.summarize_decl(resolved.primary_decl, resolved.symbol))
    }

    fn resolve_declaration_by_symbol(&self, symbol: SymbolId) -> Option<DeclarationSummaryData> {
        let sym = self.symbol_for_id(symbol)?;
        let primary = pick_preferred_symbol_decl(sym);
        primary.as_ref()?;
        Some(self.summarize_decl(primary, Some(sym)))
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
        let Some(node) = self.node_for_syntax(syntax) else {
            return false;
        };
        get_source_file_of_node(node).is_some_and(|sf| !is_project_source_file(self.program, sf))
    }

    fn member_target_hierarchy(
        &self,
        receiver_syntax: SyntaxId,
        _member_name: Option<&str>,
        static_receiver: bool,
    ) -> Option<MemberTargetHierarchy> {
        struct BridgeInterner<'a, 'b>(&'a SemanticModelBridge<'b>);
        impl HierarchyInterner for BridgeInterner<'_, '_> {
            fn intern_syntax(&mut self, node: P<Node>) -> Option<SyntaxId> {
                self.0.intern_syntax(node)
            }
            fn intern_symbol(&mut self, sym: P<Symbol>) -> SymbolId {
                self.0.intern_symbol(sym)
            }
        }
        let recv_node = self.node_for_syntax(receiver_syntax)?;
        let index = self.hierarchy_index();
        let mut checker = self.checker.borrow_mut();
        let mut env = HierarchyQueryEnv {
            cwd: self.cwd,
            program: self.program,
            checker: &mut checker,
        };
        let mut interner = BridgeInterner(self);
        index.build_hierarchy_for_receiver(&mut env, recv_node, static_receiver, &mut interner)
    }

    fn class_hierarchy_for_symbol(&self, class_symbol: SymbolId) -> Option<MemberTargetHierarchy> {
        struct BridgeInterner<'a, 'b>(&'a SemanticModelBridge<'b>);
        impl HierarchyInterner for BridgeInterner<'_, '_> {
            fn intern_syntax(&mut self, node: P<Node>) -> Option<SyntaxId> {
                self.0.intern_syntax(node)
            }
            fn intern_symbol(&mut self, sym: P<Symbol>) -> SymbolId {
                self.0.intern_symbol(sym)
            }
        }
        let sym = self.symbol_for_id(class_symbol)?;
        let index = self.hierarchy_index();
        let mut checker = self.checker.borrow_mut();
        let mut env = HierarchyQueryEnv {
            cwd: self.cwd,
            program: self.program,
            checker: &mut checker,
        };
        let mut interner = BridgeInterner(self);
        Some(index.hierarchy_from_symbols(&mut env, &[sym], false, false, &mut interner))
    }

    fn resolve_class_constructor(&self, class_syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        let node = self.node_for_syntax(class_syntax)?;
        if node.kind() == Kind::Constructor {
            let sym = self.symbol_for_node_raw(node);
            return Some(self.summarize_decl(Some(node), sym));
        }
        if matches!(node.kind(), Kind::ClassDeclaration | Kind::ClassExpression) {
            let members = if node.kind() == Kind::ClassDeclaration {
                node.as_class_declaration().members().nodes()
            } else {
                node.as_class_expression().members().nodes()
            };
            let ctor = members
                .iter()
                .copied()
                .find(|m| m.kind() == Kind::Constructor && m.body().is_some())
                .or_else(|| {
                    members
                        .iter()
                        .copied()
                        .find(|m| m.kind() == Kind::Constructor)
                });
            let target_node = ctor.unwrap_or(node);
            let sym = self.symbol_for_node_raw(target_node);
            return Some(self.summarize_decl(Some(target_node), sym));
        }
        None
    }

    fn resolve_super_target(&self, call_syntax: SyntaxId) -> Option<DeclarationSummaryData> {
        let call_node = self.node_for_syntax(call_syntax)?;
        let mut cur = call_node.parent();
        let class_node = loop {
            let n = cur?;
            if matches!(n.kind(), Kind::ClassDeclaration | Kind::ClassExpression) {
                break n;
            }
            cur = n.parent();
        };
        let clauses = if class_node.kind() == Kind::ClassDeclaration {
            class_node.as_class_declaration().heritage_clauses()?
        } else {
            class_node.as_class_expression().heritage_clauses()?
        };
        let mut base_expr: Option<P<Node>> = None;
        for &clause in clauses.nodes() {
            if clause.kind() == Kind::HeritageClause
                && clause.as_heritage_clause().token() == Kind::ExtendsKeyword
                && let Some(&first_ty) = clause.as_heritage_clause().types().nodes().first()
                && first_ty.kind() == Kind::ExpressionWithTypeArguments
            {
                base_expr = Some(first_ty.as_expression_with_type_arguments().expression());
                break;
            }
        }
        let base_expr = base_expr?;
        let base_sym = {
            let mut checker = self.checker.borrow_mut();
            checker
                .get_symbol_at_location_exported(base_expr)
                .map(|s| unwrap_aliased_symbol(&mut checker, s))
                .filter(|&s| s != checker.get_unknown_symbol())?
        };
        let base_class = pick_preferred_symbol_decl(base_sym)?;
        let base_class_syntax = self.intern_syntax(base_class)?;
        self.resolve_class_constructor(base_class_syntax)
    }

    fn resolve_implementation_for_syntax(
        &self,
        decl_or_node_syntax: SyntaxId,
        call_site_syntax: Option<SyntaxId>,
    ) -> ImplementationFact {
        let fallback_node = self.node_for_syntax(decl_or_node_syntax);
        let call_node = call_site_syntax.and_then(|s| self.node_for_syntax(s));
        let Some(target_node) = fallback_node else {
            return ImplementationFact::External {
                package_name: None,
                declaration_file: None,
                evidence: Vec::new(),
            };
        };

        let sym = {
            let mut checker = self.checker.borrow_mut();
            resolve_symbol_for_node(&mut checker, target_node).symbol
        };
        let input = if let Some(cn) = call_node
            && matches!(cn.kind(), Kind::CallExpression | Kind::NewExpression)
            && target_node == cn
        {
            FacadeImplementationInput::Node(cn)
        } else {
            FacadeImplementationInput::Declaration(target_node, sym)
        };

        let mut owned_cache = None;
        let sm_cache = self
            .sourcemap_cache
            .unwrap_or_else(|| owned_cache.get_or_insert_with(SourceMapCache::default));

        let outcome = {
            let mut checker = self.checker.borrow_mut();
            let mut facade = SemanticFacade::new(self.cwd, self.program, &mut checker);
            facade.resolve_implementation(input, self.workspace_programs, sm_cache)
        };

        match outcome {
            FacadeImplementationOutcome::Resolved(target) => {
                let Some(declaration_syntax) = self.intern_syntax(target.declaration_node) else {
                    return ImplementationFact::External {
                        package_name: target.package_name,
                        declaration_file: target.declaration_file,
                        evidence: Vec::new(),
                    };
                };
                let declaration_symbol = target.declaration_symbol.map(|s| self.intern_symbol(s));
                let fn_syntax = target.fn_node.and_then(|n| self.intern_syntax(n));
                let resolution_source = match target.resolution_source {
                    FirstPartyResolutionSource::CurrentProject => {
                        ImplementationSourceKind::CurrentProject
                    }
                    FirstPartyResolutionSource::ProjectReference => {
                        ImplementationSourceKind::ProjectReference
                    }
                    FirstPartyResolutionSource::CustomConditions => {
                        ImplementationSourceKind::CustomConditions
                    }
                    FirstPartyResolutionSource::DeclarationMap => {
                        ImplementationSourceKind::DeclarationMap
                    }
                };
                ImplementationFact::Resolved(ResolvedImplementationFact {
                    file: target.file,
                    location: target.location,
                    declaration_syntax,
                    declaration_symbol,
                    fn_syntax,
                    export_name: target.export_name,
                    package_name: target.package_name,
                    resolution_source,
                    declaration_file: target.declaration_file,
                })
            }
            FacadeImplementationOutcome::External {
                package_name,
                declaration_file,
                evidence,
            } => ImplementationFact::External {
                package_name,
                declaration_file,
                evidence,
            },
            FacadeImplementationOutcome::Inconclusive {
                declaration_file,
                gaps,
                evidence,
            } => ImplementationFact::Inconclusive {
                declaration_file,
                gaps,
                evidence,
            },
        }
    }

    fn check_call_missing_package_gap(&self, call_syntax: SyntaxId) -> Option<GapItem> {
        let node = self.node_for_syntax(call_syntax)?;
        let mut checker = self.checker.borrow_mut();
        let mut facade = SemanticFacade::new(self.cwd, self.program, &mut checker);
        facade.check_call_missing_package_gap(node)
    }

    fn declaration_identity(&self, syntax: SyntaxId) -> Option<tssae_common::DeclarationIdentity> {
        let node = self.node_for_syntax(syntax)?;
        let mut checker = self.checker.borrow_mut();
        let mut facade = SemanticFacade::new(self.cwd, self.program, &mut checker);
        facade.extract_declaration_identity(node)
    }

    fn syntax_scope_info(&self, syntax: SyntaxId) -> Option<SyntaxScopeInfo> {
        let node = self.node_for_syntax(syntax)?;
        let enclosing_fn = crate::ir_lower::resolve_enclosing_function(node)?;
        let enclosing_function = self.intern_syntax(enclosing_fn);

        let mut enclosing_block = None;
        let mut cur = node;
        while let Some(parent) = cur.parent() {
            if parent.kind() == Kind::Block {
                enclosing_block = self.intern_syntax(parent);
                break;
            }
            if parent == enclosing_fn {
                break;
            }
            cur = parent;
        }

        let mut cfg_scope_node = enclosing_fn;
        let mut highest_clean_block = None;
        let mut walk = node;
        while let Some(parent) = walk.parent() {
            if matches!(
                parent.kind(),
                Kind::ForStatement
                    | Kind::ForInStatement
                    | Kind::ForOfStatement
                    | Kind::WhileStatement
                    | Kind::DoStatement
                    | Kind::TryStatement
                    | Kind::LabeledStatement
            ) {
                cfg_scope_node = highest_clean_block.unwrap_or(enclosing_fn);
                break;
            }
            if parent == enclosing_fn {
                cfg_scope_node = enclosing_fn;
                break;
            }
            if parent.kind() == Kind::Block {
                highest_clean_block = Some(parent);
            }
            walk = parent;
        }

        Some(SyntaxScopeInfo {
            enclosing_function,
            enclosing_block,
            cfg_scope: self.intern_syntax(cfg_scope_node),
        })
    }

    fn branch_statement_syntax(&self, condition_syntax: SyntaxId) -> Option<SyntaxId> {
        let node = self.node_for_syntax(condition_syntax)?;
        let mut cur = node;
        while let Some(parent) = cur.parent() {
            if parent.kind() == Kind::IfStatement && parent.as_if_statement().expression() == cur {
                return self.intern_syntax(parent);
            }
            if matches!(
                parent.kind(),
                Kind::ParenthesizedExpression
                    | Kind::AsExpression
                    | Kind::TypeAssertionExpression
                    | Kind::SatisfiesExpression
                    | Kind::NonNullExpression
            ) {
                cur = parent;
                continue;
            }
            break;
        }
        None
    }

    fn write_syntax_info(&self, write_syntax: SyntaxId) -> Option<WriteSyntaxInfo> {
        let node = self.node_for_syntax(write_syntax)?;
        match node.kind() {
            Kind::BinaryExpression => {
                let bin = node.as_binary_expression();
                let target_node = unwrap_expr_wrappers(bin.left());
                let target_syntax = self.intern_syntax(target_node)?;
                let logical_assign_op = match bin.operator_token().kind() {
                    Kind::AmpersandAmpersandEqualsToken => Some("&&="),
                    Kind::BarBarEqualsToken => Some("||="),
                    Kind::QuestionQuestionEqualsToken => Some("??="),
                    _ => None,
                };
                Some(WriteSyntaxInfo {
                    target_syntax,
                    logical_assign_op,
                })
            }
            Kind::PrefixUnaryExpression => {
                let p = node.as_prefix_unary_expression();
                let target_node = unwrap_expr_wrappers(p.operand());
                Some(WriteSyntaxInfo {
                    target_syntax: self.intern_syntax(target_node)?,
                    logical_assign_op: None,
                })
            }
            Kind::PostfixUnaryExpression => {
                let p = node.as_postfix_unary_expression();
                let target_node = unwrap_expr_wrappers(p.operand());
                Some(WriteSyntaxInfo {
                    target_syntax: self.intern_syntax(target_node)?,
                    logical_assign_op: None,
                })
            }
            Kind::PropertyDeclaration => Some(WriteSyntaxInfo {
                target_syntax: write_syntax,
                logical_assign_op: None,
            }),
            _ => None,
        }
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

pub(crate) fn unwrap_sym_declarations(sym: P<Symbol>) -> Vec<P<Node>> {
    let mut decls = sym.declarations().to_vec();
    if let Some(vd) = sym.value_declaration()
        && !decls.contains(&vd)
    {
        decls.push(vd);
    }
    decls
}

fn extract_callable_from_decl_node(decl: P<Node>) -> Option<P<Node>> {
    if is_function_like_declaration(Some(decl)) {
        return Some(decl);
    }
    match decl.kind() {
        Kind::VariableDeclaration | Kind::PropertyDeclaration | Kind::PropertyAssignment => {
            decl.initializer().and_then(|init| {
                let inner = unwrap_expr_wrappers(init);
                is_function_like_declaration(Some(inner)).then_some(inner)
            })
        }
        Kind::ExportAssignment => decl.expression().and_then(|expr| {
            let inner = unwrap_expr_wrappers(expr);
            is_function_like_declaration(Some(inner)).then_some(inner)
        }),
        _ => None,
    }
}

fn pick_preferred_symbol_decl(sym: P<Symbol>) -> Option<P<Node>> {
    let decls = unwrap_sym_declarations(sym);
    for &d in &decls {
        if let Some(callable) = extract_callable_from_decl_node(d)
            && callable.body().is_some()
        {
            return Some(d);
        }
    }
    sym.value_declaration().or_else(|| decls.into_iter().next())
}

fn resolve_implicit_or_call_decl(
    checker: &mut CheckerHandle,
    node: P<Node>,
    implicit_flavor: Option<ImplicitAccessorKind>,
) -> ResolvedDeclarationNodes {
    if let Some(flavor) = implicit_flavor {
        let target_node = match node.kind() {
            Kind::BinaryExpression => unwrap_expr_wrappers(node.as_binary_expression().left()),
            _ => node,
        };
        if node.kind() == Kind::VariableDeclaration {
            let ty = checker.get_type_at_location(node);
            let sym = checker
                .get_property_of_type_exported(ty, "[Symbol.asyncDispose]")
                .or_else(|| checker.get_property_of_type_exported(ty, "[Symbol.dispose]"))
                .map(|s| unwrap_aliased_symbol(checker, s));
            if let Some(s) = sym {
                return ResolvedDeclarationNodes {
                    primary_decl: pick_preferred_symbol_decl(s),
                    all_decls: unwrap_sym_declarations(s),
                    symbol: Some(s),
                };
            }
        }
        if let Some(sym) = checker
            .get_symbol_at_location_exported(target_node)
            .map(|s| unwrap_aliased_symbol(checker, s))
            .filter(|&s| s != checker.get_unknown_symbol())
        {
            let expected_kind = match flavor {
                ImplicitAccessorKind::Getter => Kind::GetAccessor,
                ImplicitAccessorKind::Setter => Kind::SetAccessor,
            };
            let decls = unwrap_sym_declarations(sym);
            let primary = decls
                .iter()
                .copied()
                .find(|&d| d.kind() == expected_kind)
                .or_else(|| pick_preferred_symbol_decl(sym));
            return ResolvedDeclarationNodes {
                primary_decl: primary,
                all_decls: decls,
                symbol: Some(sym),
            };
        }
    }
    resolve_symbol_for_node(checker, node)
}

fn resolve_setter_assignment_symbol(
    checker: &mut CheckerHandle,
    node: P<Node>,
) -> Option<ResolvedDeclarationNodes> {
    let bin = node.as_binary_expression();
    if !matches!(
        bin.operator_token().kind(),
        Kind::EqualsToken
            | Kind::PlusEqualsToken
            | Kind::MinusEqualsToken
            | Kind::AsteriskEqualsToken
            | Kind::SlashEqualsToken
            | Kind::PercentEqualsToken
            | Kind::BarBarEqualsToken
            | Kind::AmpersandAmpersandEqualsToken
            | Kind::QuestionQuestionEqualsToken
    ) {
        return None;
    }
    let lhs = unwrap_expr_wrappers(bin.left());
    if lhs.kind() != Kind::PropertyAccessExpression {
        return None;
    }
    let sym = checker
        .get_symbol_at_location_exported(lhs)
        .map(|s| unwrap_aliased_symbol(checker, s))
        .filter(|&s| s != checker.get_unknown_symbol())
        .filter(|s| s.flags().intersects(SymbolFlags::SetAccessor))?;
    let all_decls = unwrap_sym_declarations(sym);
    let primary_decl = all_decls
        .iter()
        .copied()
        .find(|&d| d.kind() == Kind::SetAccessor)
        .or_else(|| pick_preferred_symbol_decl(sym));
    Some(ResolvedDeclarationNodes {
        primary_decl,
        all_decls,
        symbol: Some(sym),
    })
}

pub(crate) fn resolve_symbol_for_node(
    checker: &mut CheckerHandle,
    node: P<Node>,
) -> ResolvedDeclarationNodes {
    if matches!(
        node.kind(),
        Kind::CallExpression | Kind::NewExpression | Kind::TaggedTemplateExpression
    ) {
        let sig = checker.get_resolved_signature_exported(node);
        let sig_decl = if is_unresolved_signature(checker, sig) {
            None
        } else {
            sig.declaration()
                .or_else(|| sig.target().and_then(|t| t.declaration()))
        };

        let callee_expr = if node.kind() == Kind::TaggedTemplateExpression {
            Some(node.as_tagged_template_expression().tag())
        } else {
            node.expression()
        };

        let callee_sym = callee_expr
            .and_then(|expr| checker.get_symbol_at_location_exported(expr))
            .map(|s| unwrap_aliased_symbol(checker, s))
            .filter(|&s| s != checker.get_unknown_symbol());

        let all_decls = callee_sym.map_or_default(|s| s.declarations().to_vec());
        let primary_decl = sig_decl
            .filter(|d| extract_callable_from_decl_node(*d).is_some_and(|f| f.body().is_some()))
            .or_else(|| callee_sym.and_then(pick_preferred_symbol_decl))
            .or(sig_decl);

        return ResolvedDeclarationNodes {
            primary_decl,
            all_decls,
            symbol: callee_sym,
        };
    }

    if is_declaration_node(node) && matches!(node.kind(), Kind::GetAccessor | Kind::SetAccessor) {
        let sym = checker
            .get_symbol_at_location_exported(node)
            .or_else(|| node.symbol())
            .map(|s| unwrap_aliased_symbol(checker, s))
            .filter(|&s| s != checker.get_unknown_symbol());
        let all_decls = sym.map_or_else(|| vec![node], unwrap_sym_declarations);
        return ResolvedDeclarationNodes {
            primary_decl: Some(node),
            all_decls,
            symbol: sym,
        };
    }

    if node.kind() == Kind::BinaryExpression
        && let Some(resolved) = resolve_setter_assignment_symbol(checker, node)
    {
        return resolved;
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
        let all_decls = unwrap_sym_declarations(resolved_sym);
        let primary_decl = pick_preferred_symbol_decl(resolved_sym);
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
            | Kind::FunctionExpression
            | Kind::ArrowFunction
            | Kind::VariableDeclaration
            | Kind::Parameter
            | Kind::MethodDeclaration
            | Kind::MethodSignature
            | Kind::ClassDeclaration
            | Kind::ClassExpression
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
            | Kind::ExportAssignment
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
