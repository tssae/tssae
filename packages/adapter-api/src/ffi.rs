#![allow(unsafe_code)]
#![doc(hidden)]

use std::{
    any::Any,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    slice,
    sync::OnceLock,
};

use serde_json::Value;

use crate::{
    ADAPTER_ABI_VERSION, Adapter, AdapterError, Context, DeclarationInfo, FactRequest,
    HostQueriedFact, HostValue, NativeAdapterInputAttribution, NativeAdapterOutput,
    NativeContribution, NodeKind, NodeOperation, SourceLocation,
};

pub const MANIFEST_SYMBOL: &[u8] = b"verule_adapter_manifest_v1\0";
pub const ENTRY_SYMBOL: &[u8] = b"verule_adapter_v1\0";

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiSlice {
    pub data: *const u8,
    pub len: usize,
}

impl FfiSlice {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            data: std::ptr::null(),
            len: 0,
        }
    }

    #[must_use]
    pub const fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            data: bytes.as_ptr(),
            len: bytes.len(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiNodeList {
    pub data: *const u64,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiLocation {
    pub file: FfiSlice,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub start: u32,
    pub end: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiReply {
    pub tag: u32,
    pub flags: u32,
    pub handle: u64,
    pub nodes: FfiNodeList,
    pub bytes: FfiSlice,
    pub location: FfiLocation,
}

#[repr(C)]
pub struct FfiFactQuery {
    pub fact_type_id: FfiSlice,
    pub provider_id: FfiSlice,
    pub has_provider_id: bool,
    pub condition: FfiSlice,
    pub targets: FfiNodeList,
    pub has_targets: bool,
}

pub type ReadNodeCallback = unsafe extern "C" fn(*mut c_void, u64, u32) -> FfiReply;
pub type ReadInputCallback = unsafe extern "C" fn(*mut c_void, FfiSlice) -> FfiReply;
pub type RegisterInputCallback = unsafe extern "C" fn(*mut c_void, FfiSlice) -> i32;
pub type FinishCallback = unsafe extern "C" fn(*mut c_void, FfiSlice);
pub type FailCallback = unsafe extern "C" fn(*mut c_void, FfiSlice);
pub type QueryFactsCallback = unsafe extern "C" fn(*mut c_void, *const FfiFactQuery) -> FfiReply;

#[repr(C)]
pub struct FfiHostApi {
    pub abi_version: u32,
    pub context: *mut c_void,
    pub read_node: Option<ReadNodeCallback>,
    pub read_additional_input: Option<ReadInputCallback>,
    pub register_additional_input: Option<RegisterInputCallback>,
    pub finish: Option<FinishCallback>,
    pub fail: Option<FailCallback>,
    pub query_facts: Option<QueryFactsCallback>,
}

#[repr(C)]
pub struct FfiRequest {
    pub fact_type_id: FfiSlice,
    pub provider_id: FfiSlice,
    pub project_id: FfiSlice,
    pub condition: FfiSlice,
    pub calls: FfiNodeList,
    pub variables: FfiNodeList,
    pub targets: FfiNodeList,
    pub has_targets: bool,
    pub explain: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiManifestSlice {
    pub bytes: FfiSlice,
}

pub(crate) const REPLY_NONE: u32 = 0;
pub(crate) const REPLY_NODE: u32 = 1;
pub(crate) const REPLY_NODES: u32 = 2;
pub(crate) const REPLY_TEXT: u32 = 3;
pub(crate) const REPLY_KIND: u32 = 4;
pub(crate) const REPLY_LOCATION: u32 = 5;
pub(crate) const REPLY_DECLARATION: u32 = 6;
pub(crate) const REPLY_ERROR: u32 = 7;
pub(crate) const REPLY_FACTS: u32 = 8;

/// Builds a stable manifest slice cached in storage emitted for a single plugin.
pub fn manifest_slice<A: Adapter>(storage: &'static OnceLock<Box<[u8]>>) -> FfiManifestSlice {
    let bytes = storage.get_or_init(|| {
        serde_json::to_vec(&A::default().manifest())
            .unwrap_or_default()
            .into_boxed_slice()
    });
    FfiManifestSlice {
        bytes: FfiSlice::from_bytes(bytes),
    }
}

/// Executes one adapter call and reports failures through the host's error callback.
/// Runs the adapter entrypoint over raw protocol pointers supplied by the host.
///
/// # Safety
///
/// `host` and `request` must point to valid ABI-v1 structures for the full call. Every
/// non-empty slice in `request` and every callback context in `host` must remain valid
/// until this function returns. Host callbacks must be synchronous and must not unwind
/// across the ABI boundary.
#[must_use]
pub unsafe fn invoke_adapter<A: Adapter>(
    host: *const FfiHostApi,
    request: *const FfiRequest,
) -> i32 {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // The engine constructs these pointers for this synchronous call and keeps all
        // pointed-to buffers alive until the adapter returns.
        unsafe { invoke_adapter_inner::<A>(host, request) }
    }));
    match outcome {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            unsafe { report_failure(host, &error.to_string()) };
            1
        }
        Err(panic) => {
            let message = panic_message(panic.as_ref());
            unsafe { report_failure(host, &message) };
            2
        }
    }
}

unsafe fn invoke_adapter_inner<A: Adapter>(
    host_ptr: *const FfiHostApi,
    request_ptr: *const FfiRequest,
) -> Result<(), AdapterError> {
    let host = unsafe { host_ptr.as_ref() }.ok_or_else(|| AdapterError::InvalidOutput {
        message: "host API pointer was null".to_string(),
    })?;
    let wire = unsafe { request_ptr.as_ref() }.ok_or_else(|| AdapterError::InvalidOutput {
        message: "request pointer was null".to_string(),
    })?;
    if host.abi_version != ADAPTER_ABI_VERSION {
        return Err(AdapterError::UnsupportedAbi {
            found: host.abi_version,
            expected: ADAPTER_ABI_VERSION,
        });
    }

    let fact_type_id = unsafe { read_string(wire.fact_type_id) }?;
    let provider_id = unsafe { read_string(wire.provider_id) }?;
    let project_id = unsafe { read_string(wire.project_id) }?;
    let condition_text = unsafe { read_string(wire.condition) }?;
    let condition = if condition_text.is_empty() {
        None
    } else {
        Some(
            serde_json::from_str::<Value>(&condition_text).map_err(|error| {
                AdapterError::InvalidOutput {
                    message: format!("request condition is not JSON: {error}"),
                }
            })?,
        )
    };

    let manifest = A::default().manifest();
    manifest.validate()?;
    if !manifest.fact_type_ids.iter().any(|id| id == &fact_type_id) {
        return Err(AdapterError::UnsupportedFactType {
            adapter_id: manifest.id,
            fact_type_id,
        });
    }

    let context = Context::new(host);
    let calls = unsafe { read_node_list(wire.calls) }?
        .into_iter()
        .map(|id| context.node(id))
        .collect();
    let variables = unsafe { read_node_list(wire.variables) }?
        .into_iter()
        .map(|id| context.node(id))
        .collect();
    let targets = if wire.has_targets {
        Some(
            unsafe { read_node_list(wire.targets) }?
                .into_iter()
                .map(|id| context.node(id))
                .collect(),
        )
    } else {
        None
    };
    let request = FactRequest {
        fact_type_id: &fact_type_id,
        provider_id: &provider_id,
        project_id: &project_id,
        condition,
        calls,
        variables,
        targets,
        explain: wire.explain,
    };
    let contributions = A::default().extract(&context, &request)?;
    finish_adapter_output(host, &manifest.id, &context, contributions)
}

fn finish_adapter_output(
    host: &FfiHostApi,
    adapter_id: &str,
    context: &Context<'_>,
    contributions: Vec<crate::FactContribution<'_>>,
) -> Result<(), AdapterError> {
    let mut output_contributions = Vec::with_capacity(contributions.len());
    for contribution in contributions {
        output_contributions.push(NativeContribution {
            subject: contribution.subject.program_ref()?,
            subject_location: contribution.subject.location()?,
            value: contribution.value,
            evidence: contribution.evidence,
            gaps: contribution.gaps,
            explain_trace: contribution.explain_trace,
        });
    }
    let additional_inputs: Vec<String> =
        context.additional_inputs.borrow().iter().cloned().collect();
    let adapter_inputs = additional_inputs
        .iter()
        .map(|path| NativeAdapterInputAttribution {
            adapter_id: adapter_id.to_string(),
            path: path.clone(),
        })
        .collect();
    let output = NativeAdapterOutput {
        contributions: output_contributions,
        additional_inputs,
        adapter_inputs,
    };
    let bytes = serde_json::to_vec(&output).map_err(|error| AdapterError::InvalidOutput {
        message: error.to_string(),
    })?;
    let Some(finish) = host.finish else {
        return Err(AdapterError::Host {
            message: "host API is missing the result callback".to_string(),
        });
    };
    unsafe { finish(host.context, FfiSlice::from_bytes(&bytes)) };
    Ok(())
}

unsafe fn report_failure(host_ptr: *const FfiHostApi, message: &str) {
    let Some(host) = (unsafe { host_ptr.as_ref() }) else {
        return;
    };
    if let Some(fail) = host.fail {
        unsafe { fail(host.context, FfiSlice::from_bytes(message.as_bytes())) };
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "adapter panicked with a non-string payload".to_string()
}

pub(crate) fn read_node(
    host: &FfiHostApi,
    node_id: u64,
    operation: NodeOperation,
) -> Result<HostValue, AdapterError> {
    let Some(callback) = host.read_node else {
        return Err(AdapterError::Host {
            message: "host API is missing the node reader".to_string(),
        });
    };
    let reply = unsafe { callback(host.context, node_id, operation as u32) };
    unsafe { decode_reply(reply) }
}

pub(crate) fn read_additional_input(host: &FfiHostApi, path: &str) -> Result<String, AdapterError> {
    let Some(callback) = host.read_additional_input else {
        return Err(AdapterError::Host {
            message: "host API is missing the additional-input reader".to_string(),
        });
    };
    let reply = unsafe { callback(host.context, FfiSlice::from_bytes(path.as_bytes())) };
    match unsafe { decode_reply(reply) }? {
        HostValue::Text(Some(content)) => Ok(content),
        HostValue::Text(None) => Err(AdapterError::Host {
            message: format!("additional input \"{path}\" has no content"),
        }),
        _ => Err(AdapterError::Host {
            message: "host returned the wrong value for an additional input read".to_string(),
        }),
    }
}

pub(crate) fn register_additional_input(host: &FfiHostApi, path: &str) -> Result<(), AdapterError> {
    let Some(callback) = host.register_additional_input else {
        return Err(AdapterError::Host {
            message: "host API is missing the additional-input registrar".to_string(),
        });
    };
    let result = unsafe { callback(host.context, FfiSlice::from_bytes(path.as_bytes())) };
    if result == 0 {
        Ok(())
    } else {
        Err(AdapterError::Host {
            message: format!("failed to register additional input \"{path}\""),
        })
    }
}

pub(crate) fn query_facts(
    host: &FfiHostApi,
    fact_type_id: &str,
    provider_id: Option<&str>,
    condition_json: Option<&str>,
    target_ids: Option<&[u64]>,
) -> Result<Vec<HostQueriedFact>, AdapterError> {
    let Some(callback) = host.query_facts else {
        return Err(AdapterError::Host {
            message: "host API is missing the fact query callback".to_string(),
        });
    };
    let wire_query = FfiFactQuery {
        fact_type_id: FfiSlice::from_bytes(fact_type_id.as_bytes()),
        provider_id: provider_id
            .map_or_else(FfiSlice::empty, |id| FfiSlice::from_bytes(id.as_bytes())),
        has_provider_id: provider_id.is_some(),
        condition: condition_json.map_or_else(FfiSlice::empty, |json| {
            FfiSlice::from_bytes(json.as_bytes())
        }),
        targets: FfiNodeList {
            data: target_ids.map_or(std::ptr::null(), <[u64]>::as_ptr),
            len: target_ids.map_or(0, <[u64]>::len),
        },
        has_targets: target_ids.is_some(),
    };
    let reply = unsafe { callback(host.context, &raw const wire_query) };
    match unsafe { decode_reply(reply) }? {
        HostValue::Facts(facts) => Ok(facts),
        _ => Err(AdapterError::Host {
            message: "host returned the wrong value for a fact query".to_string(),
        }),
    }
}

unsafe fn decode_reply(reply: FfiReply) -> Result<HostValue, AdapterError> {
    let decode_string = |slice: FfiSlice| unsafe { read_string(slice) };
    match reply.tag {
        REPLY_NONE => Ok(HostValue::None),
        REPLY_NODE => Ok(HostValue::Node((reply.flags != 0).then_some(reply.handle))),
        REPLY_NODES => {
            let values = unsafe { read_node_list(reply.nodes) }?;
            Ok(HostValue::Nodes(values))
        }
        REPLY_TEXT => Ok(HostValue::Text(if reply.flags == 0 {
            None
        } else {
            Some(decode_string(reply.bytes)?)
        })),
        REPLY_KIND => {
            let raw_kind = u32::try_from(reply.handle).map_err(|_| AdapterError::Host {
                message: "host returned an out-of-range syntax kind".to_string(),
            })?;
            let kind = NodeKind::try_from(raw_kind)?;
            Ok(HostValue::Kind(kind))
        }
        REPLY_LOCATION => Ok(HostValue::Location(if reply.flags == 0 {
            None
        } else {
            Some(SourceLocation {
                file: decode_string(reply.location.file)?,
                line: reply.location.line,
                column: reply.location.column,
                end_line: reply.location.end_line,
                end_column: reply.location.end_column,
                start: reply.location.start,
                end: reply.location.end,
            })
        })),
        REPLY_DECLARATION => Ok(HostValue::Declaration(if reply.flags == 0 {
            None
        } else {
            Some(
                serde_json::from_slice::<DeclarationInfo>(unsafe { read_bytes(reply.bytes) }?)
                    .map_err(|error| AdapterError::Host {
                        message: format!("host returned invalid declaration data: {error}"),
                    })?,
            )
        })),
        REPLY_ERROR => Err(AdapterError::Host {
            message: decode_string(reply.bytes)?,
        }),
        REPLY_FACTS => {
            let bytes = unsafe { read_bytes(reply.bytes) }?;
            let facts = serde_json::from_slice::<Vec<HostQueriedFact>>(bytes).map_err(|error| {
                AdapterError::Host {
                    message: format!("host returned invalid queried fact data: {error}"),
                }
            })?;
            Ok(HostValue::Facts(facts))
        }
        _ => Err(AdapterError::Host {
            message: format!("host returned unknown reply tag {}", reply.tag),
        }),
    }
}

unsafe fn read_string(slice: FfiSlice) -> Result<String, AdapterError> {
    let bytes = unsafe { read_bytes(slice) }?;
    String::from_utf8(bytes.to_vec()).map_err(|error| AdapterError::Host {
        message: format!("host returned invalid UTF-8: {error}"),
    })
}

unsafe fn read_bytes<'a>(slice: FfiSlice) -> Result<&'a [u8], AdapterError> {
    if slice.len == 0 {
        return Ok(&[]);
    }
    if slice.data.is_null() {
        return Err(AdapterError::Host {
            message: "host returned a null byte pointer with a nonzero length".to_string(),
        });
    }
    Ok(unsafe { slice::from_raw_parts(slice.data, slice.len) })
}

unsafe fn read_node_list(list: FfiNodeList) -> Result<Vec<u64>, AdapterError> {
    if list.len == 0 {
        return Ok(Vec::new());
    }
    if list.data.is_null() {
        return Err(AdapterError::Host {
            message: "host returned a null node-list pointer with a nonzero length".to_string(),
        });
    }
    Ok(unsafe { slice::from_raw_parts(list.data, list.len) }.to_vec())
}

/// Export a manifest and entrypoint for a single native adapter library.
#[macro_export]
macro_rules! export_adapter {
    ($adapter:ty) => {
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        pub extern "C" fn verule_adapter_manifest_v1() -> $crate::ffi::FfiManifestSlice {
            static MANIFEST: std::sync::OnceLock<Box<[u8]>> = std::sync::OnceLock::new();
            $crate::ffi::manifest_slice::<$adapter>(&MANIFEST)
        }

        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        /// # Safety
        /// The engine passes ABI-v1 structures and keeps their buffers and callback context
        /// alive until this synchronous entrypoint returns.
        pub unsafe extern "C" fn verule_adapter_v1(
            host: *const $crate::ffi::FfiHostApi,
            request: *const $crate::ffi::FfiRequest,
        ) -> i32 {
            unsafe { $crate::ffi::invoke_adapter::<$adapter>(host, request) }
        }
    };
}
