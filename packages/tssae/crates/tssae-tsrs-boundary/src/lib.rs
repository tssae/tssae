//! The sole boundary crate permitted to depend on `tsrs_*` compiler internals.
//!
//! Exposes [`syntax_view::SyntaxView`] for AST inspection, diagnostic locations, and
//! comment scanning, and [`semantic_facade::SemanticFacade`] for symbols, types,
//! signatures, assignability, and module/project-reference mapping.

pub mod impl_resolution;
#[deny(clippy::wildcard_enum_match_arm)]
pub mod ir_lower;
pub mod semantic_facade;
pub mod syntax_view;

pub use impl_resolution::{
    DeclarationProvenance, FacadeImplementationInput, FacadeImplementationOutcome,
    FacadeImplementationTarget, FirstPartyResolutionSource, SourceMapCache,
    is_first_party_source_file, is_first_party_workspace_path,
};
pub use ir_lower::{
    LoweredScope, lower_class_default_constructor_to_ir, lower_class_static_init_to_ir,
    lower_class_to_ir, lower_scope_to_ir, lower_scope_to_ir_with_index, normalize_cfg_scope_node,
    normalize_function_node, resolve_enclosing_function,
};

pub use semantic_facade::{
    FacadeAssignability, FacadeCallProjection, FacadeCallSelect, FacadeDeclarationResolution,
    FacadeDeclarationSelect, FacadeDeclarationTarget, FacadeInstantiatedParameter,
    FacadeParameterIndexSelection, FacadeSignatureParameter, FacadeSignatureProjection,
    FacadeSignatureSelect, FacadeTypeProjection, FacadeTypeProperty, FacadeTypeSelect,
    FacadeTypeSummary, NotACallError, ResolvedModuleSummary, SemanticFacade, SemanticModelBridge,
};
pub use syntax_view::{
    CommentKind, FileIdRegistry, FileSyntaxIndex, FindSyntaxFilter, RangeMatchMode, ScannedComment,
    SourceRangeSpec, SyntaxCatalogBridge, SyntaxChildren, SyntaxLookupMatch, SyntaxLookupOptions,
    SyntaxMatch, SyntaxView, SyntaxViewError, is_project_source_file, node_location,
    node_location_relative, node_text, normalize_path, parse_syntax_kind, prop_key_text,
    relative_to_cwd, resolve_against_cwd, safe_arguments, safe_expression, safe_initializer,
    safe_parameters, slice_source_text, unwrap_expr_wrappers,
};
