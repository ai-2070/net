//! PyO3 surface for publishing a node's **own** local tools as mesh
//! capabilities (`HERMES_INTEGRATION_PLAN_V2.md` Phase 2, Slice B).
//!
//! The inverse of `net wrap`: a Python node announces an explicit tool set
//! (name + description + input JSON Schema) backed by a Python **async
//! callback**, and any consumer discovers / describes / invokes it through the
//! *existing* `AsyncCapabilityGateway` — no consume-side change. The whole
//! publish → announce → describe → serve → merge machinery is single-sourced in
//! `net_mcp::wrap::ServerPublisher::publish_tools` (bridge doctrine H2); this
//! file only marshals.
//!
//! **H8 (no key material).** Nothing crossing this boundary is a key — only
//! tool descriptors, JSON arguments, and JSON results. The invoke seam is a
//! Python coroutine dispatched through [`crate::async_bridge`].

use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use tokio::runtime::Runtime;

use net::adapter::net::channel::ChannelConfigRegistry;
use net::adapter::net::MeshNode;
use net_mcp::spec::{CallToolResult, Implementation, Tool};
use net_mcp::wrap::{
    CredentialStatus, LocalPublicationHandle, LoweringContext, McpError, OwnerScope,
    ServerPublisher, Substitutability, ToolInvoker, WrapConfig,
};
use net_sdk::mesh::Mesh;
use serde_json::Value;

/// A [`ToolInvoker`] backed by a Python **async** callback
/// `async (tool_name: str, args_json: str) -> str | tuple[str, bool]`.
///
/// A mesh invoke of tool `id` calls the callback with the tool's original name
/// and its JSON-encoded arguments; the returned string is the tool's text
/// output (a `(text, is_error)` tuple flags a tool-level failure). The
/// coroutine runs on the binding's dispatcher loop (see
/// [`crate::async_bridge::dispatch_handler_coro`]); a raised Python exception
/// becomes a transport error the demand side surfaces in-band.
pub(crate) struct PyToolInvoker {
    pub(crate) callback: Py<PyAny>,
}

#[async_trait::async_trait]
impl ToolInvoker for PyToolInvoker {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult, McpError> {
        let name = name.to_string();
        let args_json = serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string());
        // GIL only to build + submit the coroutine; await it off the GIL so the
        // mesh runtime worker isn't blocked holding the lock.
        let fut = Python::attach(|py| -> PyResult<_> {
            let coro = self
                .callback
                .bind(py)
                .call1((name.as_str(), args_json.as_str()))?;
            crate::async_bridge::dispatch_handler_coro(py, coro)
        })
        .map_err(|e| {
            McpError::Transport(format!(
                "local tool `{name}`: invoking the handler failed: {e}"
            ))
        })?;
        let result = fut.await.map_err(|e| {
            McpError::Transport(format!("local tool `{name}`: handler raised: {e}"))
        })?;
        Python::attach(|py| py_to_result(result.bind(py)))
    }
}

/// Convert a handler's Python return value to a [`CallToolResult`]: a
/// `(text, is_error)` tuple flags a tool-level error; a plain `str` is a
/// success (`text_ok`); anything else is a contract error.
fn py_to_result(obj: &Bound<'_, PyAny>) -> Result<CallToolResult, McpError> {
    if let Ok((text, is_error)) = obj.extract::<(String, bool)>() {
        let mut r = CallToolResult::text_ok(text);
        r.is_error = is_error;
        return Ok(r);
    }
    // Int-as-bool leniency: `(text, 1)` / `(text, 0)` is idiomatic Python and
    // the intent is unambiguous. A bool-only extract used to reject it as a
    // *transport* error — silently dropping both the tool's text and its
    // is_error flag (misclassified as a bridge failure, not a tool result).
    if let Ok((text, is_error)) = obj.extract::<(String, i64)>() {
        let mut r = CallToolResult::text_ok(text);
        r.is_error = is_error != 0;
        return Ok(r);
    }
    if let Ok(text) = obj.extract::<String>() {
        return Ok(CallToolResult::text_ok(text));
    }
    Err(McpError::Transport(
        "local tool handler must return a str or a (str, bool) tuple".to_string(),
    ))
}

/// Wrap a raw node in an SDK `Mesh` sharing the live node (a fresh channel
/// registry — nRPC dispatch lives on the node; the registry is auxiliary
/// bookkeeping the served handles keep alive). Mirrors `enrollment::mesh_over`.
pub(crate) fn mesh_over(node: Arc<MeshNode>) -> Mesh {
    Mesh::from_node_arc(node, Arc::new(ChannelConfigRegistry::new()), None)
}

/// The shared publish scaffolding both the free (`mesh_publish_tools`) and paid
/// ([`crate::payment_provider`]) paths run: parse the tool descriptors, build
/// the lowering context + owner-scoped `WrapConfig`, then announce + serve
/// through `ServerPublisher::publish_tools` (releasing the GIL for the async
/// work). The two paths differ ONLY in `pricing` + `payment_admission` (empty /
/// `None` = free) — kept in one place so schema validation, version handling,
/// scope defaults, and callback wiring can't drift between them.
///
/// `owner_origin` scopes who may invoke: `Some(origin)` admits only that caller
/// (an `origin_hash`); `None` admits only **this node itself** (fail-closed —
/// the tools are backed by an arbitrary local callback). Admitting every mesh
/// peer requires the explicit `allow_any_caller` opt-in, which overrides
/// `owner_origin`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mesh_publish_tools_configured(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<Runtime>,
    tools: Vec<(String, Option<String>, String)>,
    callback: Py<PyAny>,
    version: String,
    owner_origin: Option<u64>,
    allow_any_caller: bool,
    pricing: std::collections::BTreeMap<String, String>,
    payment_admission: Option<Arc<dyn net_mcp::serve::payment::PaymentAdmission>>,
) -> PyResult<PyLocalPublicationHandle> {
    let mut sdk_tools = Vec::with_capacity(tools.len());
    for (name, description, schema_json) in &tools {
        let input_schema: Value = serde_json::from_str(schema_json).map_err(|e| {
            PyValueError::new_err(format!(
                "tool `{name}`: input_schema is not valid JSON: {e}"
            ))
        })?;
        sdk_tools.push(Tool {
            name: name.clone(),
            title: None,
            description: description.clone(),
            input_schema,
            output_schema: None,
        });
    }

    let ctx = LoweringContext {
        server_version: if version.is_empty() {
            "0".to_string()
        } else {
            version
        },
        // Local, operator-owned tools; the in-root federation model (Slice C/D)
        // governs consent, not per-tool credential labels.
        credential_status: CredentialStatus::None,
        substitutability: Substitutability::ProviderLocal,
        // The context stays unpriced on both paths — pricing rides
        // `config.pricing` below, which `publish_tools` folds into the lowering.
        pricing: Default::default(),
    };
    let client_info = Implementation {
        name: "net-publish".to_string(),
        version: "0".to_string(),
    };
    let mesh = Arc::new(mesh_over(node));
    // Fail closed by default: no owner_origin → only this node's own origin
    // may invoke. `OwnerScope::any()` is reachable only through the explicit
    // `allow_any_caller` opt-in.
    let owner = owner_origin.unwrap_or_else(|| mesh.origin_hash());
    let mut config = WrapConfig::owner_only(client_info, owner);
    if allow_any_caller {
        config.scope = OwnerScope::any();
    }
    config.pricing = pricing;
    config.payment_admission = payment_admission;

    let publisher = ServerPublisher::new(mesh);
    let invoker: Arc<dyn ToolInvoker> = Arc::new(PyToolInvoker { callback });

    let rt = Arc::clone(&runtime);
    let handle = py
        .detach(move || rt.block_on(publisher.publish_tools(&sdk_tools, invoker, ctx, config)))
        .map_err(|e| PyRuntimeError::new_err(format!("publish_tools failed: {e}")))?;

    Ok(PyLocalPublicationHandle::wrap(handle, runtime))
}

/// Publish `tools` (each `(name, description, input_schema_json)`) on the live
/// `node`, backed by the Python `callback` — the **free** (unpriced) path. Thin
/// wrapper over [`mesh_publish_tools_configured`] with no pricing + no payment
/// gate (paid tools ride the `net-payments` publish path).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mesh_publish_tools(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<Runtime>,
    tools: Vec<(String, Option<String>, String)>,
    callback: Py<PyAny>,
    version: String,
    owner_origin: Option<u64>,
    allow_any_caller: bool,
) -> PyResult<PyLocalPublicationHandle> {
    mesh_publish_tools_configured(
        py,
        node,
        runtime,
        tools,
        callback,
        version,
        owner_origin,
        allow_any_caller,
        std::collections::BTreeMap::new(),
        None,
    )
}

/// A live publication of a node's own local tools (from
/// `NetMesh.publish_tools`). Hold it to keep the tools announced + served;
/// [`withdraw`](Self::withdraw) reverses it (re-announcing the remainder), and
/// dropping it (or [`stop`](Self::stop)) unregisters the services.
#[pyclass(
    name = "LocalPublicationHandle",
    module = "net._net",
    skip_from_py_object
)]
pub struct PyLocalPublicationHandle {
    inner: Option<LocalPublicationHandle>,
    runtime: Arc<Runtime>,
}

impl PyLocalPublicationHandle {
    /// Wrap a fresh `LocalPublicationHandle` — for the paid publish path
    /// ([`crate::payment_provider`]), which reuses the same handle type.
    pub(crate) fn wrap(inner: LocalPublicationHandle, runtime: Arc<Runtime>) -> Self {
        Self {
            inner: Some(inner),
            runtime,
        }
    }
}

#[pymethods]
impl PyLocalPublicationHandle {
    /// The served tool ids (channel-safe; a sanitized id differs from the
    /// original name).
    #[getter]
    fn tools(&self) -> Vec<String> {
        self.inner
            .as_ref()
            .map(|h| h.tools().to_vec())
            .unwrap_or_default()
    }

    /// Tool names skipped because they had no usable id (an empty name).
    #[getter]
    fn skipped_tools(&self) -> Vec<String> {
        self.inner
            .as_ref()
            .map(|h| h.skipped_tools().to_vec())
            .unwrap_or_default()
    }

    /// Whether the publication is still live.
    #[getter]
    fn serving(&self) -> bool {
        self.inner.is_some()
    }

    /// Withdraw the publication immediately: re-announce the remaining
    /// publications' set so peers stop advertising these tools, then stop the
    /// services. Idempotent — a second call is a no-op. Releases the GIL for the
    /// re-announce round-trip.
    fn withdraw(&mut self, py: Python<'_>) -> PyResult<()> {
        if let Some(handle) = self.inner.take() {
            let rt = Arc::clone(&self.runtime);
            py.detach(move || rt.block_on(handle.withdraw()))
                .map_err(|e| PyRuntimeError::new_err(format!("withdraw failed: {e}")))?;
        }
        Ok(())
    }

    /// Stop serving (unregister the services on drop; unlike
    /// [`withdraw`](Self::withdraw) this does not re-announce — the announcement
    /// reconciles at the next registry change). Idempotent.
    fn stop(&mut self) {
        self.inner = None;
    }

    fn __repr__(&self) -> String {
        format!(
            "LocalPublicationHandle(serving={}, tools={})",
            self.inner.is_some(),
            self.inner.as_ref().map(|h| h.tools().len()).unwrap_or(0),
        )
    }
}
