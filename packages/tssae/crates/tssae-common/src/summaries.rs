use serde::{Deserialize, Serialize};

/// Discovery source tier for a behavior summary pack, ordered highest to lowest priority.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SummarySource {
    Local,
    Package,
    Community,
    Builtin,
}

impl SummarySource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Package => "package",
            Self::Community => "community",
            Self::Builtin => "builtin",
        }
    }
}

/// Independent behavioral aspect described by a summary entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SummaryAspectKind {
    Calls,
    Values,
    Instances,
    Effects,
}

impl SummaryAspectKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Values => "values",
            Self::Instances => "instances",
            Self::Effects => "effects",
        }
    }
}

/// Completeness declaration for one behavioral aspect; omitted defaults to `Partial`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SummaryCompleteness {
    Complete,
    #[default]
    Partial,
}

/// Declarative identity matcher targeting an external package symbol, local module symbol, or global.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryTargetMatcher {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub member_path: Vec<String>,
}

/// Pure-data endpoint reference within a summarized call signature.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SummaryValueRef {
    Named(SummaryNamedValueRef),
    Tagged(SummaryTaggedValueRef),
}

/// Simple named endpoint reference in a summary value flow.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SummaryNamedValueRef {
    Receiver,
    ReceiverElement,
    Return,
    ReturnElement,
}

/// Structured parameterized endpoint reference in a summary value flow.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SummaryTaggedValueRef {
    Arg {
        index: usize,
    },
    ArgElement {
        index: usize,
    },
    #[serde(rename_all = "camelCase")]
    CallbackParam {
        callback_arg_index: usize,
        param_index: usize,
    },
    #[serde(rename_all = "camelCase")]
    CallbackReturn {
        callback_arg_index: usize,
    },
    #[serde(rename_all = "camelCase")]
    CallbackParamArg {
        callback_arg_index: usize,
        callback_param_index: usize,
        arg_index: usize,
    },
}

/// Execution timing of an invoked callback argument.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallbackTiming {
    Synchronous,
    Deferred,
    LifecycleEnd,
}

/// Expected invocation multiplicity of a callback argument.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallbackCardinality {
    AtMostOnce,
    ZeroOrMore,
    Once,
}

/// Receiver (`this`) binding passed when invoking a callback or forwarded function.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SummaryThisArg {
    Undefined(SummaryThisUndefined),
    FromArg {
        #[serde(rename = "fromArgIndex")]
        from_arg_index: usize,
    },
}

/// Explicit `undefined` `this` marker for serialized JSON matching `"undefined"`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SummaryThisUndefined {
    Undefined,
}

/// Callback invocation specification within a `calls` behavior summary aspect.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeCallbackSpec {
    pub callback_arg_index: usize,
    pub timing: CallbackTiming,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<CallbackCardinality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub this_arg: Option<SummaryThisArg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_name: Option<String>,
}

/// Target callable selected by `forwardsCall`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ForwardCallTarget {
    Receiver(ForwardCallReceiver),
    Arg {
        #[serde(rename = "argIndex")]
        arg_index: usize,
    },
}

/// Marker matching the `"receiver"` literal in `forwardsCall.target`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ForwardCallReceiver {
    Receiver,
}

/// Forwarding semantics for `Function.prototype.call`, `apply`, and `bind` equivalents.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ForwardCallMode {
    Call,
    Apply,
    Bind,
}

/// Direct or bound call-forwarding specification within a `calls` summary aspect.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardCallSpec {
    pub target: ForwardCallTarget,
    pub mode: ForwardCallMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub this_arg: Option<SummaryThisArg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_from_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_array_index: Option<usize>,
}

/// Call-graph and callback-invocation aspect of a behavior summary.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallAspectSummary {
    #[serde(default)]
    pub completeness: SummaryCompleteness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invokes_callbacks: Vec<InvokeCallbackSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwards_call: Option<ForwardCallSpec>,
}

/// Directed value propagation edge within a `values` summary aspect.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueFlowSpec {
    pub from: SummaryValueRef,
    pub to: SummaryValueRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_object_path: Option<bool>,
}

/// Value-propagation aspect of a behavior summary.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueAspectSummary {
    #[serde(default)]
    pub completeness: SummaryCompleteness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flows: Vec<ValueFlowSpec>,
}

/// Target reference (`"receiver"` or `{ argIndex }`) used in `instances` and `effects` aspects.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SummarySlotRef {
    Receiver(ForwardCallReceiver),
    Arg {
        #[serde(rename = "argIndex")]
        arg_index: usize,
    },
}

/// Instance-identity outcome declared by an `instances` summary aspect.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum InstanceOutcome {
    AllocatesFreshInstance,
    ForwardsInstance { from: SummarySlotRef },
    None,
}

/// Instance-identity aspect of a behavior summary.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceAspectSummary {
    #[serde(default)]
    pub completeness: SummaryCompleteness,
    pub outcome: InstanceOutcome,
}

/// Resource cleanup / container lifecycle operation in an `effects` summary aspect.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ResourceOpSpec {
    DisposesTarget {
        target: SummarySlotRef,
    },
    ReplacesPreviousValue {
        target: SummarySlotRef,
    },
    RegistersIntoContainer {
        item: SummarySlotRef,
        container: SummarySlotRef,
    },
    DetachesFromContainer {
        item: SummarySlotRef,
        container: SummarySlotRef,
    },
}

/// Side-effect and resource-cleanup aspect of a behavior summary.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectAspectSummary {
    #[serde(default)]
    pub completeness: SummaryCompleteness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_ops: Vec<ResourceOpSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutates_receiver: Option<bool>,
}

/// Single entry inside a [`BehaviorSummaryPack`].
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorSummaryEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "match")]
    pub matcher: SummaryTargetMatcher,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<CallAspectSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<ValueAspectSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instances: Option<InstanceAspectSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<EffectAspectSummary>,
}

/// Pure-data behavior summary pack produced by `defineSummaries(...)` or JSON files.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorSummaryPack {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub entries: Vec<BehaviorSummaryEntry>,
}

/// Summary pack tagged with its discovery source tier and optional origin file path.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaggedSummaryPack {
    pub source: SummarySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_file: Option<String>,
    pub pack: BehaviorSummaryPack,
}

const fn default_true() -> bool {
    true
}

/// Snapshot-level behavior summary configuration sent from Node client to engine.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryConfigPayload {
    #[serde(default)]
    pub packs: Vec<TaggedSummaryPack>,
    #[serde(default = "default_true")]
    pub enable_builtin: bool,
}

impl Default for SummaryConfigPayload {
    fn default() -> Self {
        Self {
            packs: Vec::new(),
            enable_builtin: true,
        }
    }
}

/// Canonical declaration identity extracted at the `tsrs` boundary for summary matching.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DeclarationIdentity {
    pub package_name: Option<String>,
    pub package_version: Option<String>,
    pub export_path: Option<String>,
    pub module_path: Option<String>,
    pub is_default_library: bool,
    pub member_paths: Vec<Vec<String>>,
    pub is_instance_member: bool,
}

/// Per-RPC counters tracking behavior summary lookup hits and misses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SummaryLookupStats {
    pub lookups: usize,
    pub hits: usize,
    pub misses: usize,
    pub local_hits: usize,
    pub package_hits: usize,
    pub community_hits: usize,
    pub builtin_hits: usize,
}

impl SummaryLookupStats {
    pub const fn record(&mut self, source: Option<SummarySource>) {
        self.lookups += 1;
        match source {
            Some(SummarySource::Local) => {
                self.hits += 1;
                self.local_hits += 1;
            }
            Some(SummarySource::Package) => {
                self.hits += 1;
                self.package_hits += 1;
            }
            Some(SummarySource::Community) => {
                self.hits += 1;
                self.community_hits += 1;
            }
            Some(SummarySource::Builtin) => {
                self.hits += 1;
                self.builtin_hits += 1;
            }
            None => {
                self.misses += 1;
            }
        }
    }

    pub const fn merge_from(&mut self, other: &Self) {
        self.lookups += other.lookups;
        self.hits += other.hits;
        self.misses += other.misses;
        self.local_hits += other.local_hits;
        self.package_hits += other.package_hits;
        self.community_hits += other.community_hits;
        self.builtin_hits += other.builtin_hits;
    }
}

/// One conflicting summary party reported in a [`SummaryConflictError`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryConflictParty {
    pub source: SummarySource,
    pub pack_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_file: Option<String>,
    pub matcher: SummaryTargetMatcher,
    pub aspect: SummaryAspectKind,
    pub completeness: SummaryCompleteness,
    pub summary: serde_json::Value,
}

/// Structured error details emitted when two complete summaries in the same tier disagree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryConflictError {
    pub source: SummarySource,
    pub aspect: SummaryAspectKind,
    pub target_description: String,
    pub reason: String,
    pub parties: Box<[SummaryConflictParty; 2]>,
}

impl SummaryConflictError {
    #[must_use]
    pub fn format_message(&self) -> String {
        let [left, right] = &*self.parties;
        let left_origin = left.origin_file.as_deref().unwrap_or("<in-memory>");
        let right_origin = right.origin_file.as_deref().unwrap_or("<in-memory>");
        format!(
            "Behavior summary conflict in \"{}\" tier for \"{}\" on aspect \"{}\": {}. Party 1: pack \"{}\" ({left_origin}, matcher {:?}) declared {}. Party 2: pack \"{}\" ({right_origin}, matcher {:?}) declared {}.",
            self.source.as_str(),
            self.target_description,
            self.aspect.as_str(),
            self.reason,
            left.pack_id,
            left.matcher,
            left.summary,
            right.pack_id,
            right.matcher,
            right.summary,
        )
    }
}
