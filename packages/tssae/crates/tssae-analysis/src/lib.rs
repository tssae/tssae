//! Semantic analysis engine crate.
//!
//! This crate operates solely on `tssae_common` opaque handles, pure IR structures,
//! and abstract boundary traits, with zero dependency on any `tsrs_*` crate.

pub mod branch_coverage;
pub mod call_resolution;
pub mod class_overrides;
pub mod ir;
pub mod summaries;
pub mod var_writes;

pub use ir::IrProvider;
pub use tssae_common::{
    BranchId, ConclusionStatus, DiagnosticLocation, EvidenceItem, FileId, FunctionIrId, GapItem,
    LocalBindingId, LocalSyntaxId, SemanticModel, SignatureId, SymbolId, SyntaxCatalog, SyntaxId,
    TriStateOutcome, TypeId, UnknownReason, ValueId, VertexId,
};

#[cfg(test)]
mod dependency_boundary_tests {
    #[test]
    fn tssae_analysis_manifest_has_zero_tsrs_dependencies() {
        let cargo_toml = include_str!("../Cargo.toml");
        assert!(
            !cargo_toml.contains("tsrs_"),
            "tssae-analysis must maintain zero tsrs_* dependencies"
        );
    }
}
