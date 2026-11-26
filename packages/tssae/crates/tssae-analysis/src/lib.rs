//! Semantic analysis engine crate.
//!
//! This crate operates solely on `tssae_common` opaque handles, pure IR structures,
//! and abstract boundary traits, with zero dependency on any `tsrs_*` crate.

pub mod ir;

pub use tssae_common::{
    ConclusionStatus, DiagnosticLocation, EvidenceItem, FileId, FunctionIrId, GapItem,
    LocalBindingId, LocalSyntaxId, SemanticModel, SignatureId, SymbolId, SyntaxCatalog, SyntaxId,
    TriStateOutcome, TypeId, UnknownReason, ValueId, VertexId,
};
