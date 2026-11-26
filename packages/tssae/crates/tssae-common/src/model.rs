use serde_json::Value;

use crate::diagnostics::DiagnosticLocation;
use crate::handles::{FileId, SignatureId, SymbolId, SyntaxId, TypeId};

/// Pure syntactic summary of an AST node exposed across the boundary layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxNodeSummary {
    pub id: SyntaxId,
    pub kind: String,
    pub location: DiagnosticLocation,
    pub text: String,
}

/// Pure semantic summary of a resolved TypeScript type.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Mirrors the four boolean summary flags exposed on TypeAtResult and TypeProjectionRow."
)]
pub struct TypeSummaryData {
    pub type_id: TypeId,
    pub type_text: String,
    pub is_union: bool,
    pub is_intersection: bool,
    pub is_literal: bool,
    pub literal_value: Option<Value>,
    pub union_members: Option<Vec<String>>,
    pub is_unresolved: bool,
}

/// Pure summary of a resolved declaration target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationSummaryData {
    pub symbol: Option<SymbolId>,
    pub primary_syntax: Option<SyntaxId>,
    pub name: Option<String>,
    pub container_name: Option<String>,
    pub kind: Option<String>,
    pub location: Option<DiagnosticLocation>,
    pub is_external: bool,
}

/// Pure summary of a resolved call or construct signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureSummaryData {
    pub signature_id: SignatureId,
    pub declaration_syntax: Option<SyntaxId>,
    pub signature_text: String,
    pub min_argument_count: usize,
    pub has_rest_parameter: bool,
    pub is_instantiated: bool,
}

/// Read-only syntax catalog implemented by the `tsrs` boundary layer for analysis crates.
pub trait SyntaxCatalog {
    fn syntax_location(&self, syntax: SyntaxId) -> Option<DiagnosticLocation>;
    fn syntax_text(&self, syntax: SyntaxId) -> Option<String>;
    fn syntax_kind(&self, syntax: SyntaxId) -> Option<String>;
    fn file_path(&self, file: FileId) -> Option<&str>;
}

/// Read-only semantic facade trait implemented by the `tsrs` boundary layer for analysis crates.
pub trait SemanticModel {
    fn symbol_at_syntax(&self, syntax: SyntaxId) -> Option<SymbolId>;
    fn type_at_syntax(&self, syntax: SyntaxId) -> Option<TypeSummaryData>;
    fn resolve_declaration(&self, syntax: SyntaxId) -> Option<DeclarationSummaryData>;
    fn is_assignable_to(&self, source: TypeId, target: TypeId) -> bool;
    fn base_types(&self, ty: TypeId) -> Vec<TypeId>;
    fn is_external_syntax(&self, syntax: SyntaxId) -> bool;
}
