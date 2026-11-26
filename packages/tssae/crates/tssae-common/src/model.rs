use serde_json::Value;

use crate::diagnostics::{DiagnosticLocation, EvidenceItem, GapItem};
use crate::handles::{FileId, SignatureId, SymbolId, SyntaxId, TypeId};
use crate::summaries::{DeclarationIdentity, SummarySource};

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
#[expect(
    clippy::struct_excessive_bools,
    reason = "Captures independent declaration classification flags across the tsrs boundary."
)]
pub struct DeclarationSummaryData {
    pub symbol: Option<SymbolId>,
    pub primary_syntax: Option<SyntaxId>,
    pub name: Option<String>,
    pub container_name: Option<String>,
    pub kind: Option<String>,
    pub location: Option<DiagnosticLocation>,
    pub is_external: bool,
    pub is_default_library: bool,
    pub has_body: bool,
    pub is_signature_only: bool,
    pub initializer_syntax: Option<SyntaxId>,
    pub callable_syntax: Option<SyntaxId>,
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

/// Classification of a receiver type in a class/interface hierarchy query.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ClassOrInterfaceKind {
    Class,
    Interface,
    OtherType,
}

/// Pure member metadata attached to a class or interface node in a type hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Represents independent TypeScript/ECMAScript member modifiers across the boundary."
)]
pub struct ClassMemberFact {
    pub name: String,
    pub member_syntax: SyntaxId,
    pub symbol: Option<SymbolId>,
    pub kind: String,
    pub location: Option<DiagnosticLocation>,
    pub has_body: bool,
    pub is_abstract: bool,
    pub is_static: bool,
    pub is_private_ecma: bool,
    pub is_override: bool,
    pub initializer_syntax: Option<SyntaxId>,
    pub callable_syntax: Option<SyntaxId>,
}

/// Pure class or interface descriptor in the snapshot inheritance graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeHierarchyNode {
    pub syntax: SyntaxId,
    pub symbol: SymbolId,
    pub name: Option<String>,
    pub kind: ClassOrInterfaceKind,
    pub is_exported: bool,
    pub is_abstract: bool,
    pub is_external: bool,
    pub base_symbols: Vec<SymbolId>,
    pub members: Vec<ClassMemberFact>,
    pub constructor_syntax: Option<SyntaxId>,
}

/// Inheritance and export facts collected at the `tsrs` boundary for a member-access receiver.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Carries independent boundary receiver classification flags."
)]
pub struct MemberTargetHierarchy {
    pub receiver_kind: ClassOrInterfaceKind,
    pub receiver_symbols: Vec<SymbolId>,
    pub receiver_name: Option<String>,
    pub receiver_is_exported: bool,
    pub receiver_is_abstract: bool,
    pub receiver_is_external: bool,
    pub is_union_or_intersection: bool,
    pub has_unresolved_type: bool,
    pub candidate_classes: Vec<TypeHierarchyNode>,
}

/// First-party implementation resolution source matching `FirstPartyResolutionSource`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImplementationSourceKind {
    CurrentProject,
    ProjectReference,
    CustomConditions,
    DeclarationMap,
}

/// Concrete first-party implementation target summary exposed across the boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedImplementationFact {
    pub file: String,
    pub location: DiagnosticLocation,
    pub declaration_syntax: SyntaxId,
    pub declaration_symbol: Option<SymbolId>,
    pub fn_syntax: Option<SyntaxId>,
    pub export_name: Option<String>,
    pub package_name: Option<String>,
    pub resolution_source: ImplementationSourceKind,
    pub declaration_file: Option<String>,
}

/// Outcome of resolving whether a target has a first-party workspace implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImplementationFact {
    Resolved(ResolvedImplementationFact),
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
    fn signature_at_syntax(&self, syntax: SyntaxId) -> Option<SignatureId>;
    fn resolve_declaration(&self, syntax: SyntaxId) -> Option<DeclarationSummaryData>;
    fn resolve_declaration_by_symbol(&self, symbol: SymbolId) -> Option<DeclarationSummaryData>;
    fn is_assignable_to(&self, source: TypeId, target: TypeId) -> bool;
    fn base_types(&self, ty: TypeId) -> Vec<TypeId>;
    fn is_external_syntax(&self, syntax: SyntaxId) -> bool;
    fn member_target_hierarchy(
        &self,
        receiver_syntax: SyntaxId,
        member_name: Option<&str>,
        static_receiver: bool,
    ) -> Option<MemberTargetHierarchy>;
    fn class_hierarchy_for_symbol(&self, class_symbol: SymbolId) -> Option<MemberTargetHierarchy>;
    fn resolve_class_constructor(&self, class_syntax: SyntaxId) -> Option<DeclarationSummaryData>;
    fn resolve_super_target(&self, call_syntax: SyntaxId) -> Option<DeclarationSummaryData>;
    fn resolve_implementation_for_syntax(
        &self,
        decl_or_node_syntax: SyntaxId,
        call_site_syntax: Option<SyntaxId>,
    ) -> ImplementationFact;
    fn check_call_missing_package_gap(&self, call_syntax: SyntaxId) -> Option<GapItem>;
    fn declaration_identity(&self, _syntax: SyntaxId) -> Option<DeclarationIdentity> {
        None
    }
    fn record_summary_lookup(&self, _source: Option<SummarySource>) {}
}
