#![deny(unsafe_code)]
// Node identity uses only the immutable context pointer and opaque ID; Context's RefCell
// tracks supplemental inputs and does not participate in NodeRef equality or hashing.
#![allow(clippy::mutable_key_type)]

use std::{cmp::Ordering, collections::BTreeMap};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use serde_json::Value;
use tssae_adapter_api::{
    ADAPTER_ABI_VERSION, Adapter, AdapterError, AdapterManifest, Context, Evidence,
    FactContribution, FactRequest, Gap, NodeKind, NodeRef as ApiNodeRef, SourceLocation,
};

mod algorithm;
mod drizzle_zod;

type Kind = NodeKind;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct NodeRef<'a>(ApiNodeRef<'a>);

struct CheckerHandle;

#[derive(Clone, Serialize)]
pub struct GapItem {
    kind: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    at: Option<SourceLocation>,
}

#[derive(Clone, Serialize)]
pub struct EvidenceItem {
    kind: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    at: Option<SourceLocation>,
}

impl EvidenceItem {
    fn simple(kind: &'static str, message: String, at: Option<SourceLocation>) -> Self {
        Self { kind, message, at }
    }
}

impl NodeRef<'_> {
    fn kind(self) -> Kind {
        self.0.kind().expect("the host could not read a node kind")
    }

    fn name(self) -> Option<Self> {
        self.0
            .name()
            .expect("the host could not read a node name")
            .map(Self)
    }

    fn expression(self) -> Option<Self> {
        self.0
            .expression()
            .expect("the host could not read a node expression")
            .map(Self)
    }

    fn initializer(self) -> Option<Self> {
        self.0
            .initializer()
            .expect("the host could not read a node initializer")
            .map(Self)
    }

    fn body(self) -> Option<Self> {
        self.0
            .body()
            .expect("the host could not read a node body")
            .map(Self)
    }

    fn arguments(self) -> Vec<Self> {
        self.0
            .arguments()
            .expect("the host could not read node arguments")
            .into_iter()
            .map(Self)
            .collect()
    }

    fn for_each_child(self, callback: &mut impl FnMut(Self) -> bool) -> bool {
        self.0
            .children()
            .expect("the host could not read node children")
            .into_iter()
            .map(Self)
            .any(callback)
    }
}

fn properties<'a>(node: NodeRef<'a>) -> Vec<NodeRef<'a>> {
    node.0
        .properties()
        .expect("the host could not read object properties")
        .into_iter()
        .map(NodeRef)
        .collect()
}

fn node_text(node: NodeRef<'_>) -> Option<String> {
    node.0.text().expect("the host could not read node text")
}

fn kind_label(node: NodeRef<'_>) -> String {
    node.0
        .kind_label()
        .expect("the host could not read a node kind label")
        .unwrap_or_else(|| node.kind().as_str().to_owned())
}

fn safe_initializer<'a>(node: NodeRef<'a>) -> Option<NodeRef<'a>> {
    node.0
        .safe_initializer()
        .expect("the host could not resolve a safe initializer")
        .map(NodeRef)
}

fn unwrap_expr_wrappers<'a>(node: NodeRef<'a>) -> NodeRef<'a> {
    node.0
        .unwrap_expression()
        .expect("the host could not unwrap an expression")
        .map(NodeRef)
        .unwrap_or(node)
}

fn read_type_text(_checker: &mut CheckerHandle, node: NodeRef<'_>) -> String {
    node.0
        .type_text()
        .expect("the host could not read a node type")
        .unwrap_or_default()
}

fn resolve_decl_for_ref<'a>(
    _cwd: &str,
    _checker: &mut CheckerHandle,
    node: NodeRef<'a>,
) -> Option<(Option<String>, NodeRef<'a>)> {
    let name = node.0.declaration().ok().flatten()?.name;
    let declaration = node.0.declaration_node().ok().flatten()?;
    Some((name, NodeRef(declaration)))
}

fn node_location_rel(_cwd: &str, node: NodeRef<'_>) -> Option<SourceLocation> {
    node.0
        .location()
        .expect("the host could not read a node location")
}

crate::drizzle_algorithm!();

struct DrizzleAdapter;

impl Default for DrizzleAdapter {
    fn default() -> Self {
        Self
    }
}

impl Adapter for DrizzleAdapter {
    fn manifest(&self) -> AdapterManifest {
        AdapterManifest {
            abi_version: ADAPTER_ABI_VERSION,
            id: "drizzle".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            fact_type_ids: vec![
                "verule.query.row-projection".to_string(),
                "verule.drizzle.table-columns".to_string(),
                drizzle_zod::fact_type_id().to_string(),
            ],
        }
    }

    fn extract<'a>(
        &self,
        context: &'a Context<'_>,
        request: &FactRequest<'a>,
    ) -> Result<Vec<FactContribution<'a>>, AdapterError> {
        match request.fact_type_id {
            "verule.query.row-projection" => extract_row_projections(request),
            "verule.drizzle.table-columns" => extract_table_columns(request),
            "verule.validator.schema-fields" => {
                drizzle_zod::extract_schema_fields(context, request)
            }
            _ => Err(AdapterError::UnsupportedFactType {
                adapter_id: "drizzle".to_string(),
                fact_type_id: request.fact_type_id.to_string(),
            }),
        }
    }
}

fn collect_drizzle_nodes<'a>(request: &FactRequest<'a>) -> CollectedDrizzleNodes<'a> {
    let calls = request
        .calls
        .iter()
        .copied()
        .map(NodeRef)
        .collect::<Vec<_>>();
    let variables = request
        .variables
        .iter()
        .copied()
        .map(NodeRef)
        .collect::<Vec<_>>();
    let inner_receiver_locs = classify_receiver_locations("", &calls);
    CollectedDrizzleNodes {
        all_calls: calls,
        all_var_decls: variables,
        inner_receiver_locs,
    }
}

fn convert_contribution<'a, T: Serialize>(
    subject: NodeRef<'a>,
    value: T,
    evidence: Vec<EvidenceItem>,
    gaps: Vec<GapItem>,
    context_label: &str,
) -> Result<FactContribution<'a>, AdapterError> {
    let value = serde_json::to_value(value).map_err(|error| AdapterError::InvalidOutput {
        message: format!("failed to serialize Drizzle {context_label}: {error}"),
    })?;
    let mut contribution = FactContribution::new(subject.0, value);
    contribution.evidence = evidence
        .into_iter()
        .map(|item| Evidence {
            kind: item.kind.to_string(),
            message: item.message,
            at: item.at,
            model_id: None,
            declaration: None,
        })
        .collect();
    contribution.gaps = gaps
        .into_iter()
        .map(|item| Gap {
            kind: item.kind.to_string(),
            message: item.message,
            at: item.at,
        })
        .collect();
    Ok(contribution)
}

fn extract_row_projections<'a>(
    request: &FactRequest<'a>,
) -> Result<Vec<FactContribution<'a>>, AdapterError> {
    if request
        .condition
        .as_ref()
        .and_then(|condition| condition.get("library"))
        .and_then(Value::as_str)
        .is_some_and(|library| library != "drizzle")
    {
        return Ok(Vec::new());
    }

    let nodes = collect_drizzle_nodes(request);
    let requested_nodes = request.targets.as_ref().map(|targets| {
        targets
            .iter()
            .copied()
            .map(NodeRef)
            .collect::<FxHashSet<_>>()
    });
    let mut checker = CheckerHandle;
    let extracted = extract_all_drizzle_items("", &mut checker, &nodes, requested_nodes.as_ref());

    extracted
        .into_iter()
        .map(|item| {
            convert_contribution(
                item.call,
                item.value,
                item.evidence,
                item.gaps,
                "projection",
            )
        })
        .collect()
}

fn extract_table_columns<'a>(
    request: &FactRequest<'a>,
) -> Result<Vec<FactContribution<'a>>, AdapterError> {
    let requested_table_name = request
        .condition
        .as_ref()
        .and_then(|condition| condition.get("tableName"))
        .and_then(Value::as_str);

    let nodes = collect_drizzle_nodes(request);
    let requested_targets = request
        .targets
        .as_ref()
        .map(|targets| targets.iter().copied().map(NodeRef).collect::<Vec<_>>());
    let mut checker = CheckerHandle;
    let extracted = extract_all_table_columns_items(
        "",
        &mut checker,
        &nodes,
        requested_targets.as_deref(),
        requested_table_name,
    );

    extracted
        .into_iter()
        .map(|item| {
            convert_contribution(
                item.subject,
                item.value,
                item.evidence,
                item.gaps,
                "table columns",
            )
        })
        .collect()
}

tssae_adapter_api::export_adapter!(DrizzleAdapter);
