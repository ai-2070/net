//! Publish a node's **own** local tools as mesh capabilities — the Node twin of
//! the Python `publish.rs` (and the inverse of `net wrap`). A node announces an
//! explicit tool set (name + description + input JSON Schema) backed by a JS
//! **async handler**, and any consumer discovers / describes / invokes it
//! through the ordinary `CapabilityGateway` — no consume-side change.
//!
//! Doctrine #1 (no logic in bindings): the whole publish → announce → describe →
//! serve → merge machinery is single-sourced in
//! `net_mcp::wrap::ServerPublisher::publish_tools`; this file only marshals a JS
//! handler into a `ToolInvoker` (via the proven `blob.rs` TSFN→Promise bridge)
//! and projects the publication handle.
//!
//! **H8 (no key material).** Nothing crossing this boundary is a key — only tool
//! descriptors, JSON arguments, and JSON results. The invoke seam is a JS async
//! callback dispatched through a `ThreadsafeFunction`.
//!
//! This is the **free** (unpriced) publish path and the prerequisite for a Node
//! payment provider (`PAYMENTS_PY_TS_SDK_GAP_PLAN.md` B5): the paid path reuses
//! `mesh_over` + the invoker + [`LocalPublicationHandle`], adding a
//! `PaymentEngine` + `EnginePaymentAdmission` on top. Behind the `publish`
//! feature.

#![cfg(feature = "publish")]
// napi-derive registers these items via a generated `extern "C"` table the
// dead-code lint can't trace; `cargo clippy --tests` otherwise flags the
// invoker struct, the TSFN alias, and `mesh_over` (reached only through the
// generated `publishTools` glue) as unused under the test profile.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use parking_lot::Mutex;
use serde_json::Value;

use net::adapter::net::{ChannelConfigRegistry, MeshNode};
use net_mcp::spec::{CallToolResult, Implementation, Tool};
use net_mcp::wrap::{
    CredentialStatus, LocalPublicationHandle as InnerPublicationHandle, LoweringContext, McpError,
    OwnerScope, ServerPublisher, Substitutability, ToolInvoker, WrapConfig,
};
use net_sdk::mesh::Mesh as SdkMesh;

/// Total budget for one JS tool-handler call (JS returning the Promise + the
/// Promise resolving), against a single deadline — same discipline as the
/// `blob.rs` async adapter. A hung Node event loop must not strand the
/// substrate's serve task forever; a genuinely slow tool overrides via
/// `PublishOptions.handlerTimeoutMs`. The mesh caller has its own RPC deadline
/// beneath this — this is the provider-side safety net.
const DEFAULT_TOOL_HANDLER_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// JS-facing value types.
// ---------------------------------------------------------------------------

/// One tool to publish: `name` + optional `description` + its input JSON Schema
/// as a JSON object string. Consumers invoke it as `provider/<sanitizedName>`.
#[napi(object)]
pub struct PublishToolJs {
    /// The tool's name (its original name; the served id is a sanitized,
    /// channel-safe form).
    pub name: String,
    /// Human-readable description shown on describe. Optional.
    pub description: Option<String>,
    /// The tool's input JSON Schema, as a JSON object string (e.g.
    /// `'{"type":"object","properties":{...}}'`).
    pub input_schema: String,
}

/// The argument object handed to the JS tool handler.
#[napi(object)]
pub struct ToolInvokeArgs {
    /// The invoked tool's original name (the `name` you published, not the
    /// sanitized id).
    pub tool_name: String,
    /// The invocation arguments as a JSON object string (default `{}`).
    pub arguments_json: String,
}

/// The JS tool handler's return object: the tool's text output plus an optional
/// tool-level error flag.
#[napi(object)]
pub struct ToolCallResultJs {
    /// The tool's text output.
    pub text: String,
    /// `true` iff the tool ran but produced an error *result* — a tool-level
    /// failure, distinct from a transport failure (which is a thrown / rejected
    /// Promise). Default `false`.
    pub is_error: Option<bool>,
}

/// Optional knobs for [`NetMesh::publish_tools`](crate::NetMesh).
#[napi(object)]
pub struct PublishOptions {
    /// Server version string used in the lowering. Default `"0"`.
    pub version: Option<String>,
    /// Restrict invocation to a single caller origin (an `originHash`, as
    /// BigInt). Omit to admit **only this node itself** — the fail-closed
    /// default, since the tools are backed by an arbitrary local callback.
    pub owner_origin: Option<BigInt>,
    /// Admit **every** mesh peer (overrides `ownerOrigin`). Default `false`.
    /// You must gate invocations yourself when you set this.
    pub allow_any_caller: Option<bool>,
    /// Per-call handler timeout in milliseconds. Default `120000`. The total
    /// budget across the handler returning its Promise and that Promise
    /// resolving.
    pub handler_timeout_ms: Option<u32>,
}

// ---------------------------------------------------------------------------
// The JS-handler → ToolInvoker bridge (TSFN → Promise, the blob.rs pattern).
// ---------------------------------------------------------------------------

/// The bridged JS tool handler:
/// `(args: ToolInvokeArgs) => Promise<ToolCallResultJs>`.
type InvokeTsfn = ThreadsafeFunction<
    ToolInvokeArgs,
    Promise<ToolCallResultJs>,
    ToolInvokeArgs,
    napi::Status,
    false,
>;

/// A [`ToolInvoker`] backed by a JS **async** handler. A mesh invoke calls the
/// handler with the tool's original name + JSON arguments; its resolved value
/// is the tool's result. A thrown / rejected Promise (or a timeout) becomes a
/// transport error the demand side surfaces in-band — never a silent success.
pub(crate) struct NodeToolInvoker {
    handler: InvokeTsfn,
    timeout: Duration,
}

/// Call the JS handler with `args`, await its Promise, return the result — both
/// stages under one deadline so the worst case is `timeout`, not `2×`.
async fn call_js_handler(
    handler: &InvokeTsfn,
    args: ToolInvokeArgs,
    timeout: Duration,
) -> std::result::Result<ToolCallResultJs, McpError> {
    let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<ToolCallResultJs>>>();
    let status = handler.call_with_return_value(
        args,
        ThreadsafeFunctionCallMode::NonBlocking,
        move |ret, _env| {
            let _ = tx.send(ret);
            Ok(())
        },
    );
    if status != napi::Status::Ok {
        return Err(McpError::Transport(format!(
            "local tool handler: TSFN enqueue status {status:?}"
        )));
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let promise = match tokio::time::timeout_at(deadline, rx).await {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => {
            return Err(McpError::Transport(format!(
                "local tool handler threw before returning a Promise: {e}"
            )))
        }
        Ok(Err(_)) => {
            return Err(McpError::Transport(
                "local tool handler callback channel disconnected".to_string(),
            ))
        }
        Err(_) => {
            return Err(McpError::Transport(format!(
                "local tool handler did not return a Promise within {} ms",
                timeout.as_millis()
            )))
        }
    };
    match tokio::time::timeout_at(deadline, promise).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(McpError::Transport(format!(
            "local tool handler Promise rejected: {e}"
        ))),
        Err(_) => Err(McpError::Transport(format!(
            "local tool handler Promise did not resolve within {} ms",
            timeout.as_millis()
        ))),
    }
}

#[async_trait]
impl ToolInvoker for NodeToolInvoker {
    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> std::result::Result<CallToolResult, McpError> {
        let args = ToolInvokeArgs {
            tool_name: name.to_string(),
            arguments_json: serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string()),
        };
        let result = call_js_handler(&self.handler, args, self.timeout).await?;
        let mut r = CallToolResult::text_ok(result.text);
        r.is_error = result.is_error.unwrap_or(false);
        Ok(r)
    }
}

// ---------------------------------------------------------------------------
// Marshaling helpers — pure, so the projection is unit-testable without the
// napi link (the cargo-test linking limit is doctrine, `node/src/mesh_rpc.rs`).
// ---------------------------------------------------------------------------

/// Wrap a raw node in an SDK `Mesh` sharing the live node. A fresh channel
/// registry — nRPC dispatch lives on the node; the registry is auxiliary
/// bookkeeping the served handle keeps alive. Mirrors the Python `mesh_over`.
/// Reused by the future paid publish path (B5).
pub(crate) fn mesh_over(node: Arc<MeshNode>) -> SdkMesh {
    SdkMesh::from_node_arc(node, Arc::new(ChannelConfigRegistry::new()), None)
}

/// Parse the published tool set into SDK `Tool`s. A bad input schema is a
/// caller error surfaced up front (before the publish round-trip). Reused by
/// the paid publish path ([`crate::payment_provider`]).
pub(crate) fn build_sdk_tools(tools: &[PublishToolJs]) -> Result<Vec<Tool>> {
    let mut sdk_tools = Vec::with_capacity(tools.len());
    for t in tools {
        let input_schema: Value = serde_json::from_str(&t.input_schema).map_err(|e| {
            Error::from_reason(format!(
                "publish: tool `{}`: inputSchema is not valid JSON: {e}",
                t.name
            ))
        })?;
        sdk_tools.push(Tool {
            name: t.name.clone(),
            title: None,
            description: t.description.clone(),
            input_schema,
            output_schema: None,
        });
    }
    Ok(sdk_tools)
}

/// Resolve a JS `owner_origin` BigInt to a `u64` origin hash. A negative or
/// out-of-range value is a caller error (origins are unsigned 64-bit). Reused by
/// the paid publish path ([`crate::payment_provider`]).
pub(crate) fn parse_owner_origin(owner_origin: Option<BigInt>) -> Result<Option<u64>> {
    match owner_origin {
        Some(bi) => {
            let (signed, value, lossless) = bi.get_u64();
            if signed || !lossless {
                return Err(Error::from_reason(
                    "publish: ownerOrigin must be a non-negative 64-bit origin hash",
                ));
            }
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

/// Build the lowering context for locally-published tools: local,
/// operator-owned tools; the in-root federation model governs consent, not
/// per-tool credential labels. `ctx.pricing` is always empty here — the free
/// path prices nothing, and the paid path
/// ([`crate::payment_provider`]) carries pricing on `WrapConfig.pricing`
/// instead (both funnel through `publish_tools`, which folds one into the
/// other). Reused by both call sites so the lowering can't drift.
pub(crate) fn local_lowering_context(version: Option<String>) -> LoweringContext {
    LoweringContext {
        server_version: match version {
            Some(v) if !v.is_empty() => v,
            _ => "0".to_string(),
        },
        credential_status: CredentialStatus::None,
        substitutability: Substitutability::ProviderLocal,
        pricing: Default::default(),
    }
}

/// Build the JS-handler-backed [`ToolInvoker`] — build the `ThreadsafeFunction`
/// on the JS thread (napi requires it), wrap it in [`NodeToolInvoker`] with the
/// per-call timeout. Called synchronously (the `Function` is `!Send`); the
/// resulting `Arc` is `Send` and crosses into the async publish. Reused by the
/// paid publish path.
pub(crate) fn build_tool_invoker(
    handler: Function<'_, ToolInvokeArgs, Promise<ToolCallResultJs>>,
    handler_timeout_ms: Option<u32>,
) -> Result<Arc<dyn ToolInvoker>> {
    let timeout = handler_timeout_ms
        .map(|ms| Duration::from_millis(ms as u64))
        .unwrap_or(DEFAULT_TOOL_HANDLER_TIMEOUT);
    let tsfn: InvokeTsfn = handler.build_threadsafe_function().build()?;
    Ok(Arc::new(NodeToolInvoker {
        handler: tsfn,
        timeout,
    }))
}

// ---------------------------------------------------------------------------
// The publish entry point — sync setup (build the TSFN off the JS thread would
// be UB), then hand all-Send state to `env.spawn_future`. The idiomatic napi
// "sync setup, async continuation" shape (see `compute.rs::spawn`).
// ---------------------------------------------------------------------------

/// Build the invoker + config synchronously and spawn the announce/serve work.
/// The `Function` is `!Send`, so the TSFN is built here (on the JS thread) and
/// only the `Send` TSFN crosses into the future.
pub(crate) fn spawn_publish_tools<'env>(
    env: &'env Env,
    node: Arc<MeshNode>,
    tools: Vec<PublishToolJs>,
    handler: Function<'_, ToolInvokeArgs, Promise<ToolCallResultJs>>,
    options: Option<PublishOptions>,
) -> Result<PromiseRaw<'env, LocalPublicationHandle>> {
    // Validate + marshal up front so a bad schema / origin rejects immediately
    // rather than inside the Promise.
    let sdk_tools = build_sdk_tools(&tools)?;
    let opts = options.unwrap_or(PublishOptions {
        version: None,
        owner_origin: None,
        allow_any_caller: None,
        handler_timeout_ms: None,
    });
    let owner_origin = parse_owner_origin(opts.owner_origin)?;
    let allow_any_caller = opts.allow_any_caller.unwrap_or(false);
    let ctx = local_lowering_context(opts.version);
    let client_info = Implementation {
        name: "net-publish".to_string(),
        version: "0".to_string(),
    };

    // Build the invoker on the JS thread (the TSFN build napi requires); only
    // the `Send` `Arc` crosses into the future. Key material is unrepresentable
    // — only the typed args + result cross.
    let invoker = build_tool_invoker(handler, opts.handler_timeout_ms)?;

    env.spawn_future(async move {
        let mesh = Arc::new(mesh_over(node));
        // Fail closed: no ownerOrigin → only this node's own origin may invoke.
        // `OwnerScope::any()` is reachable only through `allowAnyCaller`.
        let owner = owner_origin.unwrap_or_else(|| mesh.origin_hash());
        let mut config = WrapConfig::owner_only(client_info, owner);
        if allow_any_caller {
            config.scope = OwnerScope::any();
        }
        let publisher = ServerPublisher::new(mesh);
        let handle = publisher
            .publish_tools(&sdk_tools, invoker, ctx, config)
            .await
            .map_err(|e| Error::from_reason(format!("publish: publishTools failed: {e}")))?;
        Ok(LocalPublicationHandle::wrap(handle))
    })
}

// ---------------------------------------------------------------------------
// The publication handle.
// ---------------------------------------------------------------------------

/// A live publication of a node's own local tools (from
/// `NetMesh.publishTools`). Hold it to keep the tools announced + served;
/// [`withdraw`](Self::withdraw) reverses it (re-announcing the remainder), and
/// dropping it (or [`stop`](Self::stop)) unregisters the services.
#[napi]
pub struct LocalPublicationHandle {
    /// `None` once withdrawn / stopped. A `parking_lot::Mutex` because the
    /// handle is shared to JS and `withdraw` consumes the inner across an
    /// await — take it out under the lock, then drive the round-trip unlocked.
    inner: Mutex<Option<InnerPublicationHandle>>,
}

impl LocalPublicationHandle {
    /// Wrap a fresh `LocalPublicationHandle`. Reused by the future paid publish
    /// path (B5), which produces the same inner handle type.
    pub(crate) fn wrap(inner: InnerPublicationHandle) -> Self {
        Self {
            inner: Mutex::new(Some(inner)),
        }
    }
}

#[napi]
impl LocalPublicationHandle {
    /// The served tool ids (channel-safe; a sanitized id differs from the
    /// original name). Empty once withdrawn / stopped.
    #[napi(getter)]
    pub fn tools(&self) -> Vec<String> {
        self.inner
            .lock()
            .as_ref()
            .map(|h| h.tools().to_vec())
            .unwrap_or_default()
    }

    /// Tool names skipped because they had no usable id (an empty name).
    #[napi(getter)]
    pub fn skipped_tools(&self) -> Vec<String> {
        self.inner
            .lock()
            .as_ref()
            .map(|h| h.skipped_tools().to_vec())
            .unwrap_or_default()
    }

    /// Whether the publication is still live.
    #[napi(getter)]
    pub fn serving(&self) -> bool {
        self.inner.lock().is_some()
    }

    /// Withdraw the publication immediately: re-announce the remaining
    /// publications' set so peers stop advertising these tools, then stop the
    /// services. Idempotent — a second call resolves to a no-op.
    #[napi]
    pub async fn withdraw(&self) -> Result<()> {
        // Take the inner out under the lock, then await the round-trip unlocked
        // (no guard held across the await).
        let handle = self.inner.lock().take();
        if let Some(handle) = handle {
            handle
                .withdraw()
                .await
                .map_err(|e| Error::from_reason(format!("publish: withdraw failed: {e}")))?;
        }
        Ok(())
    }

    /// Stop serving (unregister the services on drop; unlike
    /// [`withdraw`](Self::withdraw) this does not re-announce — the announcement
    /// reconciles at the next registry change). Idempotent.
    #[napi]
    pub fn stop(&self) {
        let _ = self.inner.lock().take();
    }
}

// ---------------------------------------------------------------------------
// Pure marshaling tests — the napi class can't link under `cargo test`, so this
// pins only the schema/origin parsing (format strings + plain structs).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_sdk_tools_parses_schema_and_rejects_bad_json() {
        let tools = vec![
            PublishToolJs {
                name: "echo".to_string(),
                description: Some("echoes".to_string()),
                input_schema: r#"{"type":"object"}"#.to_string(),
            },
            PublishToolJs {
                name: "noop".to_string(),
                description: None,
                input_schema: "{}".to_string(),
            },
        ];
        let sdk = build_sdk_tools(&tools).expect("valid schemas parse");
        assert_eq!(sdk.len(), 2);
        assert_eq!(sdk[0].name, "echo");
        assert_eq!(sdk[0].description.as_deref(), Some("echoes"));
        assert_eq!(sdk[0].input_schema["type"], "object");
        assert!(sdk[1].description.is_none());

        let bad = vec![PublishToolJs {
            name: "broken".to_string(),
            description: None,
            input_schema: "not json".to_string(),
        }];
        let err = build_sdk_tools(&bad).unwrap_err();
        assert!(err.reason.contains("broken"));
        assert!(err.reason.contains("inputSchema"));
    }

    #[test]
    fn parse_owner_origin_round_trips_and_fails_closed() {
        assert_eq!(parse_owner_origin(None).unwrap(), None);
        assert_eq!(
            parse_owner_origin(Some(BigInt::from(42u64))).unwrap(),
            Some(42)
        );
        // A negative BigInt (signed) is rejected — origins are unsigned.
        assert!(parse_owner_origin(Some(BigInt::from(-1i64))).is_err());
    }

    #[test]
    fn local_lowering_context_defaults_version_and_prices_nothing() {
        let ctx = local_lowering_context(None);
        assert_eq!(ctx.server_version, "0");
        assert!(ctx.pricing.is_empty());
        let ctx = local_lowering_context(Some(String::new()));
        assert_eq!(ctx.server_version, "0");
        let ctx = local_lowering_context(Some("2.1".to_string()));
        assert_eq!(ctx.server_version, "2.1");
        assert!(matches!(ctx.credential_status, CredentialStatus::None));
        assert!(matches!(
            ctx.substitutability,
            Substitutability::ProviderLocal
        ));
    }
}
