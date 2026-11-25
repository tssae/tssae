#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // Public fallible reads uniformly return AdapterError.

//! Stable, compiler-independent interfaces for Verule native library adapters.
//!
//! Adapter authors implement [`Adapter`] over [`NodeRef`] values and compile a `cdylib`.
//! The only cross-library data is a versioned C ABI carrying opaque node IDs, UTF-8 byte
//! slices, and JSON fact values; no compiler AST or Rust allocation crosses the boundary.

use std::{
    collections::BTreeSet,
    hash::{Hash, Hasher},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod ffi;
#[cfg(feature = "loader")]
#[allow(unsafe_code)]
mod loader;

#[cfg(feature = "loader")]
pub use loader::NativeLibrary;

/// Current C ABI version accepted by the engine.
pub const ADAPTER_ABI_VERSION: u32 = 1;

/// Metadata reported by a native library before the engine calls its extraction entrypoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterManifest {
    pub abi_version: u32,
    pub id: String,
    pub version: String,
    pub fact_type_ids: Vec<String>,
}

impl AdapterManifest {
    /// Checks the manifest fields that are required for deterministic provider selection.
    pub fn validate(&self) -> Result<(), AdapterError> {
        if self.abi_version != ADAPTER_ABI_VERSION {
            return Err(AdapterError::UnsupportedAbi {
                found: self.abi_version,
                expected: ADAPTER_ABI_VERSION,
            });
        }
        if self.id.trim().is_empty() {
            return Err(AdapterError::InvalidManifest {
                message: "adapter id must not be empty".to_string(),
            });
        }
        if self.version.trim().is_empty() {
            return Err(AdapterError::InvalidManifest {
                message: "adapter version must not be empty".to_string(),
            });
        }
        let mut seen = BTreeSet::new();
        for fact_type_id in &self.fact_type_ids {
            if fact_type_id.trim().is_empty() {
                return Err(AdapterError::InvalidManifest {
                    message: "fact type ids must not be empty".to_string(),
                });
            }
            if !seen.insert(fact_type_id) {
                return Err(AdapterError::DuplicateFactType {
                    fact_type_id: fact_type_id.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Errors returned by the loader or an adapter invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdapterError {
    UnsupportedAbi {
        found: u32,
        expected: u32,
    },
    InvalidManifest {
        message: String,
    },
    DuplicateFactType {
        fact_type_id: String,
    },
    Load {
        package_name: String,
        path: String,
        message: String,
    },
    MissingSymbol {
        package_name: String,
        symbol: String,
    },
    UnsupportedFactType {
        adapter_id: String,
        fact_type_id: String,
    },
    Host {
        message: String,
    },
    Plugin {
        message: String,
    },
    InvalidOutput {
        message: String,
    },
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedAbi { found, expected } => write!(
                formatter,
                "adapter ABI version {found} is unsupported; this engine requires version {expected}"
            ),
            Self::InvalidManifest { message } => {
                write!(formatter, "invalid adapter manifest: {message}")
            }
            Self::DuplicateFactType { fact_type_id } => {
                write!(
                    formatter,
                    "adapter manifest repeats fact type \"{fact_type_id}\""
                )
            }
            Self::Load {
                package_name,
                path,
                message,
            } => write!(
                formatter,
                "failed to load adapter package \"{package_name}\" from \"{path}\": {message}"
            ),
            Self::MissingSymbol {
                package_name,
                symbol,
            } => write!(
                formatter,
                "adapter package \"{package_name}\" does not export required symbol \"{symbol}\""
            ),
            Self::UnsupportedFactType {
                adapter_id,
                fact_type_id,
            } => write!(
                formatter,
                "adapter \"{adapter_id}\" does not provide fact type \"{fact_type_id}\""
            ),
            Self::Host { message } => write!(formatter, "adapter host error: {message}"),
            Self::Plugin { message } => write!(formatter, "adapter error: {message}"),
            Self::InvalidOutput { message } => {
                write!(formatter, "invalid adapter output: {message}")
            }
        }
    }
}

impl std::error::Error for AdapterError {}

/// Stable syntax categories provided to adapters without exposing compiler AST types.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[repr(u32)]
pub enum NodeKind {
    Other = 0,
    ArrowFunction = 1,
    Block = 2,
    CallExpression = 3,
    ComputedPropertyName = 4,
    FunctionExpression = 5,
    Identifier = 6,
    ImportSpecifier = 7,
    NewExpression = 8,
    NoSubstitutionTemplateLiteral = 9,
    NumericLiteral = 10,
    ObjectLiteralExpression = 11,
    PropertyAccessExpression = 12,
    PropertyAssignment = 13,
    ReturnStatement = 14,
    ShorthandPropertyAssignment = 15,
    SpreadAssignment = 16,
    StringLiteral = 17,
    UndefinedKeyword = 18,
    VariableDeclaration = 19,
    ArrayLiteralExpression = 20,
    ElementAccessExpression = 21,
    TrueKeyword = 22,
    FalseKeyword = 23,
    NullKeyword = 24,
    PropertyDeclaration = 25,
    FunctionDeclaration = 26,
    Parameter = 27,
    BindingElement = 28,
    AsExpression = 29,
    NonNullExpression = 30,
    ParenthesizedExpression = 31,
    SatisfiesExpression = 32,
    BinaryExpression = 33,
    ConditionalExpression = 34,
    PrefixUnaryExpression = 35,
    TemplateExpression = 36,
    TemplateHead = 37,
    TemplateSpan = 38,
}

impl NodeKind {
    /// Returns the stable TypeScript syntax kind label used in adapter observations.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Other => "Other",
            Self::ArrowFunction => "ArrowFunction",
            Self::Block => "Block",
            Self::CallExpression => "CallExpression",
            Self::ComputedPropertyName => "ComputedPropertyName",
            Self::FunctionExpression => "FunctionExpression",
            Self::Identifier => "Identifier",
            Self::ImportSpecifier => "ImportSpecifier",
            Self::NewExpression => "NewExpression",
            Self::NoSubstitutionTemplateLiteral => "NoSubstitutionTemplateLiteral",
            Self::NumericLiteral => "NumericLiteral",
            Self::ObjectLiteralExpression => "ObjectLiteralExpression",
            Self::PropertyAccessExpression => "PropertyAccessExpression",
            Self::PropertyAssignment => "PropertyAssignment",
            Self::ReturnStatement => "ReturnStatement",
            Self::ShorthandPropertyAssignment => "ShorthandPropertyAssignment",
            Self::SpreadAssignment => "SpreadAssignment",
            Self::StringLiteral => "StringLiteral",
            Self::UndefinedKeyword => "UndefinedKeyword",
            Self::VariableDeclaration => "VariableDeclaration",
            Self::ArrayLiteralExpression => "ArrayLiteralExpression",
            Self::ElementAccessExpression => "ElementAccessExpression",
            Self::TrueKeyword => "TrueKeyword",
            Self::FalseKeyword => "FalseKeyword",
            Self::NullKeyword => "NullKeyword",
            Self::PropertyDeclaration => "PropertyDeclaration",
            Self::FunctionDeclaration => "FunctionDeclaration",
            Self::Parameter => "Parameter",
            Self::BindingElement => "BindingElement",
            Self::AsExpression => "AsExpression",
            Self::NonNullExpression => "NonNullExpression",
            Self::ParenthesizedExpression => "ParenthesizedExpression",
            Self::SatisfiesExpression => "SatisfiesExpression",
            Self::BinaryExpression => "BinaryExpression",
            Self::ConditionalExpression => "ConditionalExpression",
            Self::PrefixUnaryExpression => "PrefixUnaryExpression",
            Self::TemplateExpression => "TemplateExpression",
            Self::TemplateHead => "TemplateHead",
            Self::TemplateSpan => "TemplateSpan",
        }
    }
}

impl TryFrom<u32> for NodeKind {
    type Error = AdapterError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::ArrowFunction),
            2 => Ok(Self::Block),
            3 => Ok(Self::CallExpression),
            4 => Ok(Self::ComputedPropertyName),
            5 => Ok(Self::FunctionExpression),
            6 => Ok(Self::Identifier),
            7 => Ok(Self::ImportSpecifier),
            8 => Ok(Self::NewExpression),
            9 => Ok(Self::NoSubstitutionTemplateLiteral),
            10 => Ok(Self::NumericLiteral),
            11 => Ok(Self::ObjectLiteralExpression),
            12 => Ok(Self::PropertyAccessExpression),
            13 => Ok(Self::PropertyAssignment),
            14 => Ok(Self::ReturnStatement),
            15 => Ok(Self::ShorthandPropertyAssignment),
            16 => Ok(Self::SpreadAssignment),
            17 => Ok(Self::StringLiteral),
            18 => Ok(Self::UndefinedKeyword),
            19 => Ok(Self::VariableDeclaration),
            20 => Ok(Self::ArrayLiteralExpression),
            21 => Ok(Self::ElementAccessExpression),
            22 => Ok(Self::TrueKeyword),
            23 => Ok(Self::FalseKeyword),
            24 => Ok(Self::NullKeyword),
            25 => Ok(Self::PropertyDeclaration),
            26 => Ok(Self::FunctionDeclaration),
            27 => Ok(Self::Parameter),
            28 => Ok(Self::BindingElement),
            29 => Ok(Self::AsExpression),
            30 => Ok(Self::NonNullExpression),
            31 => Ok(Self::ParenthesizedExpression),
            32 => Ok(Self::SatisfiesExpression),
            33 => Ok(Self::BinaryExpression),
            34 => Ok(Self::ConditionalExpression),
            35 => Ok(Self::PrefixUnaryExpression),
            36 => Ok(Self::TemplateExpression),
            37 => Ok(Self::TemplateHead),
            38 => Ok(Self::TemplateSpan),
            _ => Ok(Self::Other),
        }
    }
}

/// Read operations supported by the host for one compiler-independent syntax node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum NodeOperation {
    Kind = 1,
    KindLabel = 2,
    Name = 3,
    Expression = 4,
    Body = 5,
    Initializer = 6,
    SafeInitializer = 7,
    UnwrapExpression = 8,
    Text = 9,
    TypeText = 10,
    Location = 11,
    Declaration = 12,
    Arguments = 13,
    Properties = 14,
    Children = 15,
    ProgramReference = 16,
    DeclarationNode = 17,
}

impl TryFrom<u32> for NodeOperation {
    type Error = AdapterError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Kind),
            2 => Ok(Self::KindLabel),
            3 => Ok(Self::Name),
            4 => Ok(Self::Expression),
            5 => Ok(Self::Body),
            6 => Ok(Self::Initializer),
            7 => Ok(Self::SafeInitializer),
            8 => Ok(Self::UnwrapExpression),
            9 => Ok(Self::Text),
            10 => Ok(Self::TypeText),
            11 => Ok(Self::Location),
            12 => Ok(Self::Declaration),
            13 => Ok(Self::Arguments),
            14 => Ok(Self::Properties),
            15 => Ok(Self::Children),
            16 => Ok(Self::ProgramReference),
            17 => Ok(Self::DeclarationNode),
            _ => Err(AdapterError::Host {
                message: format!("unknown node operation {value}"),
            }),
        }
    }
}

/// A normalized source range whose file path is relative to the analyzed project.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub start: u32,
    pub end: u32,
}

/// Declaration information resolved by the compiler host.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationInfo {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub location: Option<SourceLocation>,
    pub syntax: Option<u64>,
    pub is_external: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_name: Option<String>,
}

/// A typed piece of evidence attached to a fact contribution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<SourceLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration: Option<String>,
}

/// An explanation that limits what a fact extraction could conclude.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gap {
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<SourceLocation>,
}

/// Value returned to the engine by the host for a node read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostValue {
    None,
    Node(Option<u64>),
    Nodes(Vec<u64>),
    Text(Option<String>),
    Kind(NodeKind),
    Location(Option<SourceLocation>),
    Declaration(Option<DeclarationInfo>),
    Facts(Vec<HostQueriedFact>),
}

/// Host-level query parameters passed across [`HostAccess::query_facts`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFactQuery {
    pub fact_type_id: String,
    pub provider_id: Option<String>,
    pub condition: Option<Value>,
    pub targets: Option<Vec<u64>>,
}

/// Resolution status of a local unknown recorded during targeted explain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainGapResolutionStatus {
    Unresolved,
    ResolvedByOverride,
}

/// Override or downstream conclusion that resolved an earlier local unknown.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainGapResolution {
    pub provider_id: String,
    pub operation: String,
    pub conclusion: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<SourceLocation>,
}

/// Local unknown recorded during targeted explain (`request.explain == true`).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainLocalGap {
    pub status: ExplainGapResolutionStatus,
    pub fact_type_id: String,
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub gap: Gap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_location: Option<SourceLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<ExplainGapResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<String>,
}

/// Per-contribution explain trace populated only when `request.explain` is true.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FactExplainTrace {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_gaps: Vec<ExplainLocalGap>,
}

/// Host-level fact item returned by [`HostAccess::query_facts`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostQueriedFact {
    pub subject_node_id: u64,
    pub value: Value,
    pub evidence: Vec<Evidence>,
    pub gaps: Vec<Gap>,
    pub providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explain_trace: Option<FactExplainTrace>,
}

/// Synchronous access that the engine implements for the current project snapshot.
pub trait HostAccess {
    /// Reads a syntax property or compiler-derived value from an opaque node handle.
    fn read_node(&mut self, node_id: u64, operation: NodeOperation) -> Result<HostValue, String>;

    /// Reads a UTF-8 file and registers it as an input of the active adapter invocation.
    fn read_additional_input(&mut self, path: &str) -> Result<String, String>;

    /// Registers an input path without reading it.
    fn register_additional_input(&mut self, path: &str) -> Result<(), String>;

    /// Queries facts from other enabled native providers on the active snapshot.
    fn query_facts(&mut self, query: HostFactQuery) -> Result<Vec<HostQueriedFact>, String>;
}

/// A request to one native provider for one project snapshot.
#[derive(Clone, Debug)]
pub struct NativeFactRequest<'a> {
    pub fact_type_id: &'a str,
    pub provider_id: &'a str,
    pub project_id: &'a str,
    pub condition: Option<&'a Value>,
    pub calls: &'a [u64],
    pub variables: &'a [u64],
    pub targets: Option<&'a [u64]>,
    pub explain: bool,
}

/// Raw fact contribution returned by an adapter through the public Rust API.
#[derive(Clone)]
pub struct FactContribution<'a> {
    pub subject: NodeRef<'a>,
    pub value: Value,
    pub evidence: Vec<Evidence>,
    pub gaps: Vec<Gap>,
    pub explain_trace: Option<FactExplainTrace>,
}

impl<'a> FactContribution<'a> {
    /// Creates a contribution for a node in the active request.
    #[must_use]
    pub const fn new(subject: NodeRef<'a>, value: Value) -> Self {
        Self {
            subject,
            value,
            evidence: Vec::new(),
            gaps: Vec::new(),
            explain_trace: None,
        }
    }
}

/// Metadata-free, JSON result returned by the engine's native extraction path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeContribution {
    pub subject: String,
    pub subject_location: Option<SourceLocation>,
    pub value: Value,
    pub evidence: Vec<Evidence>,
    pub gaps: Vec<Gap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explain_trace: Option<FactExplainTrace>,
}

/// Attribution pairing a read/registered additional input path with the adapter that used it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeAdapterInputAttribution {
    pub adapter_id: String,
    pub path: String,
}

/// Result returned by a native adapter, including files used by the extraction.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeAdapterOutput {
    pub contributions: Vec<NativeContribution>,
    pub additional_inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adapter_inputs: Vec<NativeAdapterInputAttribution>,
}

/// Query parameters for requesting facts from other enabled providers on the active snapshot.
#[derive(Clone, Debug)]
pub struct FactQuery<'a> {
    pub fact_type_id: &'a str,
    pub provider_id: Option<&'a str>,
    pub condition: Option<Value>,
    pub targets: Option<&'a [NodeRef<'a>]>,
}

/// One fact item returned by [`Context::query_facts`], bound to the active [`Context`] lifetime.
#[derive(Clone, Debug)]
pub struct QueriedFact<'a> {
    pub subject: NodeRef<'a>,
    pub value: Value,
    pub evidence: Vec<Evidence>,
    pub gaps: Vec<Gap>,
    pub providers: Vec<String>,
    pub explain_trace: Option<FactExplainTrace>,
}

/// Request data available to an adapter implementation.
pub struct FactRequest<'a> {
    pub fact_type_id: &'a str,
    pub provider_id: &'a str,
    pub project_id: &'a str,
    pub condition: Option<Value>,
    pub calls: Vec<NodeRef<'a>>,
    pub variables: Vec<NodeRef<'a>>,
    pub targets: Option<Vec<NodeRef<'a>>>,
    pub explain: bool,
}

/// Read-only access to the active project snapshot and its source files.
pub struct Context<'a> {
    pub(crate) host: &'a ffi::FfiHostApi,
    additional_inputs: std::cell::RefCell<BTreeSet<String>>,
}

impl<'a> Context<'a> {
    pub(crate) const fn new(host: &'a ffi::FfiHostApi) -> Self {
        Self {
            host,
            additional_inputs: std::cell::RefCell::new(BTreeSet::new()),
        }
    }

    /// Returns a node wrapper whose lifetime cannot outlive this extraction request.
    pub(crate) const fn node(&self, id: u64) -> NodeRef<'_> {
        NodeRef { context: self, id }
    }

    /// Reads and registers an additional input relative to the analyzed project root.
    pub fn read_additional_input(&self, path: &str) -> Result<String, AdapterError> {
        let content = ffi::read_additional_input(self.host, path)?;
        self.additional_inputs.borrow_mut().insert(path.to_string());
        Ok(content)
    }

    /// Registers an additional input path without reading its contents.
    pub fn register_additional_input(&self, path: &str) -> Result<(), AdapterError> {
        ffi::register_additional_input(self.host, path)?;
        self.additional_inputs.borrow_mut().insert(path.to_string());
        Ok(())
    }

    /// Queries facts from other enabled native providers on the active project snapshot.
    pub fn query_facts(
        &'a self,
        query: &FactQuery<'_>,
    ) -> Result<Vec<QueriedFact<'a>>, AdapterError> {
        if query.fact_type_id.trim().is_empty() {
            return Err(AdapterError::Host {
                message: "fact_type_id must not be empty".to_string(),
            });
        }
        if let Some(provider_id) = query.provider_id
            && provider_id.trim().is_empty()
        {
            return Err(AdapterError::Host {
                message: "provider_id must not be empty when specified".to_string(),
            });
        }
        if let Some(targets) = query.targets {
            for target in targets {
                if !std::ptr::eq(target.context, self) {
                    return Err(AdapterError::Host {
                        message: "query_facts target NodeRef belongs to a different context"
                            .to_string(),
                    });
                }
            }
        }
        let target_ids: Option<Vec<u64>> = query
            .targets
            .map(|targets| targets.iter().map(|node| node.id).collect());
        let condition_json = query
            .condition
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| AdapterError::Host {
                message: format!("failed to serialize fact query condition: {error}"),
            })?;
        let items = ffi::query_facts(
            self.host,
            query.fact_type_id,
            query.provider_id,
            condition_json.as_deref(),
            target_ids.as_deref(),
        )?;
        Ok(items
            .into_iter()
            .map(|item| QueriedFact {
                subject: self.node(item.subject_node_id),
                value: item.value,
                evidence: item.evidence,
                gaps: item.gaps,
                providers: item.providers,
                explain_trace: item.explain_trace,
            })
            .collect())
    }
}

/// A syntax reference scoped to one native extraction call.
#[derive(Clone, Copy)]
pub struct NodeRef<'a> {
    context: &'a Context<'a>,
    id: u64,
}

impl std::fmt::Debug for NodeRef<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeRef")
            .field("id", &self.id)
            .finish()
    }
}

impl PartialEq for NodeRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && std::ptr::eq(self.context, other.context)
    }
}

impl Eq for NodeRef<'_> {}

impl Hash for NodeRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::from_ref(self.context).hash(state);
        self.id.hash(state);
    }
}

impl NodeRef<'_> {
    /// Stable TypeScript syntax kind for this node.
    pub fn kind(self) -> Result<NodeKind, AdapterError> {
        match self.read(NodeOperation::Kind)? {
            HostValue::Kind(kind) => Ok(kind),
            _ => Err(AdapterError::Host {
                message: "host returned the wrong value for a node kind read".to_string(),
            }),
        }
    }

    /// Compiler's syntax kind label, including kinds not yet named in [`NodeKind`].
    pub fn kind_label(self) -> Result<Option<String>, AdapterError> {
        self.text_operation(NodeOperation::KindLabel)
    }

    /// Name child of a property, declaration, or access expression.
    pub fn name(self) -> Result<Option<Self>, AdapterError> {
        self.node_operation(NodeOperation::Name)
    }

    /// Receiver or callee child of an expression.
    pub fn expression(self) -> Result<Option<Self>, AdapterError> {
        self.node_operation(NodeOperation::Expression)
    }

    /// Function body child.
    pub fn body(self) -> Result<Option<Self>, AdapterError> {
        self.node_operation(NodeOperation::Body)
    }

    /// Initializer child of a declaration or property assignment.
    pub fn initializer(self) -> Result<Option<Self>, AdapterError> {
        self.node_operation(NodeOperation::Initializer)
    }

    /// Finds a usable initializer while following safe compiler wrappers.
    pub fn safe_initializer(self) -> Result<Option<Self>, AdapterError> {
        self.node_operation(NodeOperation::SafeInitializer)
    }

    /// Removes transparent parentheses, assertions, and non-null wrappers.
    pub fn unwrap_expression(self) -> Result<Option<Self>, AdapterError> {
        self.node_operation(NodeOperation::UnwrapExpression)
    }

    /// Source text occupied by this syntax node.
    pub fn text(self) -> Result<Option<String>, AdapterError> {
        self.text_operation(NodeOperation::Text)
    }

    /// Compiler-rendered type text at this node, when type information is available.
    pub fn type_text(self) -> Result<Option<String>, AdapterError> {
        self.text_operation(NodeOperation::TypeText)
    }

    /// Source location of this node within the active snapshot.
    pub fn location(self) -> Result<Option<SourceLocation>, AdapterError> {
        match self.read(NodeOperation::Location)? {
            HostValue::Location(location) => Ok(location),
            _ => Err(AdapterError::Host {
                message: "host returned the wrong value for a location read".to_string(),
            }),
        }
    }

    /// Resolved declaration metadata for this node, if the checker found one.
    pub fn declaration(self) -> Result<Option<DeclarationInfo>, AdapterError> {
        match self.read(NodeOperation::Declaration)? {
            HostValue::Declaration(declaration) => Ok(declaration),
            _ => Err(AdapterError::Host {
                message: "host returned the wrong value for a declaration read".to_string(),
            }),
        }
    }

    /// Follows a local identifier or call to its workspace source declaration.
    ///
    /// The declaration may be in another workspace package outside the analyzed project.
    /// Unresolved or external declarations return `None`; adapters never receive
    /// compiler-specific declaration types.
    pub fn declaration_node(self) -> Result<Option<Self>, AdapterError> {
        self.node_operation(NodeOperation::DeclarationNode)
    }

    /// Direct call arguments in source order.
    pub fn arguments(self) -> Result<Vec<Self>, AdapterError> {
        self.nodes_operation(NodeOperation::Arguments)
    }

    /// Direct object literal properties in source order.
    pub fn properties(self) -> Result<Vec<Self>, AdapterError> {
        self.nodes_operation(NodeOperation::Properties)
    }

    /// Direct syntax children in source order.
    pub fn children(self) -> Result<Vec<Self>, AdapterError> {
        self.nodes_operation(NodeOperation::Children)
    }

    /// Returns a snapshot-bound program handle suitable for a fact contribution.
    ///
    /// When multiple references target the same AST node, the host supplies one primary
    /// requested handle here and distributes each contribution across the other requested
    /// references to that node.
    pub fn program_ref(self) -> Result<String, AdapterError> {
        match self.read(NodeOperation::ProgramReference)? {
            HostValue::Text(Some(handle)) => Ok(handle),
            HostValue::Text(None) => Err(AdapterError::Host {
                message: "host could not allocate a program reference".to_string(),
            }),
            _ => Err(AdapterError::Host {
                message: "host returned the wrong value for a program reference read".to_string(),
            }),
        }
    }

    fn read(self, operation: NodeOperation) -> Result<HostValue, AdapterError> {
        // Context is valid for the entire synchronous extraction call. The C ABI wrapper
        // copies every host-owned result before another callback can replace its buffer.
        ffi::read_node(self.context.host, self.id, operation)
    }

    fn text_operation(self, operation: NodeOperation) -> Result<Option<String>, AdapterError> {
        match self.read(operation)? {
            HostValue::Text(text) => Ok(text),
            _ => Err(AdapterError::Host {
                message: "host returned the wrong value for a text read".to_string(),
            }),
        }
    }

    fn node_operation(self, operation: NodeOperation) -> Result<Option<Self>, AdapterError> {
        match self.read(operation)? {
            HostValue::Node(node) => Ok(node.map(|id| self.context.node(id))),
            _ => Err(AdapterError::Host {
                message: "host returned the wrong value for a node read".to_string(),
            }),
        }
    }

    fn nodes_operation(self, operation: NodeOperation) -> Result<Vec<Self>, AdapterError> {
        match self.read(operation)? {
            HostValue::Nodes(nodes) => {
                Ok(nodes.into_iter().map(|id| self.context.node(id)).collect())
            }
            _ => Err(AdapterError::Host {
                message: "host returned the wrong value for a node list read".to_string(),
            }),
        }
    }
}

/// A library adapter that extracts facts from a compiler-independent snapshot view.
pub trait Adapter: Default + Send + Sync + 'static {
    /// Identifies the provider and the fact types it can answer.
    fn manifest(&self) -> AdapterManifest;

    /// Extracts raw contributions from the requested project snapshot.
    fn extract<'a>(
        &self,
        context: &'a Context<'_>,
        request: &FactRequest<'a>,
    ) -> Result<Vec<FactContribution<'a>>, AdapterError>;
}
