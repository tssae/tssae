use serde::Serialize;

/// 1-based UTF-16 line/column and 0-based UTF-8 byte offsets in a normalized relative file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub start: u32,
    pub end: u32,
}

/// Legacy engine alias for [`DiagnosticLocation`].
pub type SourceLocation = DiagnosticLocation;

/// Canonical reason codes for static-analysis gaps and unknown IR elements.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnknownReason {
    UnresolvedType,
    UnresolvedDeclaration,
    UnresolvedSignature,
    NotACall,
    ParameterOutOfBounds,
    MissingSourceFile,
    WithStatement,
    DynamicEval,
    UnmodeledConstruct,
    UnmodeledLoop,
    UnmodeledTryCatch,
    UnmodeledJump,
    UnmodeledGenerator,
    UnmodeledAwait,
    MissingFunctionBody,
    InvalidCfgScope,
    TargetOutsideCfg,
    ForeignBranchReference,
    NoEnclosingFunction,
    MissingBehaviorSummary,
    OpenExternalHierarchy,
    DynamicCallee,
    MissingPackage,
}

impl UnknownReason {
    #[must_use]
    pub const fn as_gap_kind(self) -> &'static str {
        match self {
            Self::UnresolvedType => "unresolved-type",
            Self::UnresolvedDeclaration => "unresolved-declaration",
            Self::UnresolvedSignature => "unresolved-signature",
            Self::NotACall => "not-a-call",
            Self::ParameterOutOfBounds => "parameter-out-of-bounds",
            Self::MissingSourceFile => "missing-source-file",
            Self::WithStatement => "with-statement",
            Self::DynamicEval => "dynamic-eval",
            Self::UnmodeledConstruct => "unmodeled-construct",
            Self::UnmodeledLoop => "unmodeled-loop",
            Self::UnmodeledTryCatch => "unmodeled-try-catch",
            Self::UnmodeledJump => "unmodeled-jump",
            Self::UnmodeledGenerator => "unmodeled-generator",
            Self::UnmodeledAwait => "unmodeled-await",
            Self::MissingFunctionBody => "missing-function-body",
            Self::InvalidCfgScope => "invalid-cfg-scope",
            Self::TargetOutsideCfg => "target-outside-cfg",
            Self::ForeignBranchReference => "foreign-branch-reference",
            Self::NoEnclosingFunction => "no-enclosing-function",
            Self::MissingBehaviorSummary => "missing-behavior-summary",
            Self::OpenExternalHierarchy => "open-external-hierarchy",
            Self::DynamicCallee => "dynamic-callee",
            Self::MissingPackage => "missing-package",
        }
    }
}

/// Information gap attached when a query or analysis encounters an unknown or unresolved element.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GapItem {
    pub kind: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<DiagnosticLocation>,
}

impl GapItem {
    #[must_use]
    pub const fn new(kind: &'static str, message: String, at: Option<DiagnosticLocation>) -> Self {
        Self { kind, message, at }
    }

    #[must_use]
    pub const fn from_reason(
        reason: UnknownReason,
        message: String,
        at: Option<DiagnosticLocation>,
    ) -> Self {
        Self {
            kind: reason.as_gap_kind(),
            message,
            at,
        }
    }
}

/// Alias matching the public `Gap` domain term.
pub type Gap = GapItem;

/// Supporting evidence entry explaining how an engine conclusion was derived.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceItem {
    pub kind: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<DiagnosticLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration: Option<String>,
}

impl EvidenceItem {
    #[must_use]
    pub const fn simple(
        kind: &'static str,
        message: String,
        at: Option<DiagnosticLocation>,
    ) -> Self {
        Self {
            kind,
            message,
            at,
            model_id: None,
            declaration: None,
        }
    }

    #[must_use]
    pub const fn model(
        message: String,
        at: Option<DiagnosticLocation>,
        model_id: String,
        declaration: String,
    ) -> Self {
        Self {
            kind: "model",
            message,
            at,
            model_id: Some(model_id),
            declaration: Some(declaration),
        }
    }
}

/// Alias matching the public `Evidence` domain term.
pub type Evidence = EvidenceItem;

/// Tri-state status shared across semantic proof queries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConclusionStatus {
    Proved,
    Disproved,
    Inconclusive,
}

/// Generic tri-state outcome distinguishing proved, disproved with counterexample, and inconclusive with gaps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriStateOutcome<P, C> {
    Proved {
        value: P,
        evidence: Vec<EvidenceItem>,
    },
    Disproved {
        counterexample: C,
        evidence: Vec<EvidenceItem>,
    },
    Inconclusive {
        gaps: Vec<GapItem>,
        evidence: Vec<EvidenceItem>,
    },
}

impl<P, C> TriStateOutcome<P, C> {
    #[must_use]
    pub const fn status(&self) -> ConclusionStatus {
        match self {
            Self::Proved { .. } => ConclusionStatus::Proved,
            Self::Disproved { .. } => ConclusionStatus::Disproved,
            Self::Inconclusive { .. } => ConclusionStatus::Inconclusive,
        }
    }
}
