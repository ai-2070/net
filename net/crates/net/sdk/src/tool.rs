//! AI tool-calling surface — SDK-side helpers built atop the
//! substrate's `cortex::tool` module.
//!
//! Gated by the `tool` Cargo feature. Three things land here:
//!
//! 1. Re-exports of every public wire / type primitive from
//!    [`net::adapter::net::cortex::tool`]. Downstream consumers
//!    write `use net_sdk::tool::ToolDescriptor` rather than reaching
//!    deep into the substrate-side module path.
//! 2. [`metadata_for`] — builds a [`ToolDescriptor`] from any pair
//!    of Rust request / response types implementing
//!    [`schemars::JsonSchema`]. The schemas land on the descriptor
//!    as JSON-encoded strings (matching `ToolCapability::input_schema`'s
//!    existing shape).
//! 3. [`ToolMetadataBuilder`] — a fluent builder for the fields
//!    that aren't derivable from the type signature: description,
//!    version, streaming flag, tags, stateless / latency hints.
//!    Callers chain `metadata_for::<Req, Resp>(name).description(...)`
//!    to build the descriptor in one expression.
//!
//! The actual `serve_tool` / `list_tools` / `watch_tools` /
//! `call_tool` SDK methods land in subsequent A-2..A-6 slices; this
//! one just establishes the type re-exports + the schema-derivation
//! helper every later slice composes against.
//!
//! Plan: see `docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`,
//! slice A-1.

#[cfg(feature = "cortex")]
pub use net::adapter::net::behavior::fold::capability_aggregation::{TagMatcher, TagMatcherError};
#[cfg(feature = "cortex")]
pub use net::adapter::net::cortex::tool::{
    description_metadata_key, pricing_terms_metadata_key, streaming_metadata_key,
    tags_metadata_key, ToolDescriptor, ToolEvent, ToolListChange, ToolListWatch,
    ToolMetadataRegistry, ToolMetadataRequest, ToolMetadataResponse, TOOL_METADATA_FETCH_SERVICE,
};

#[cfg(feature = "cortex")]
use std::sync::Arc;

#[cfg(feature = "cortex")]
use crate::mesh::Mesh;
#[cfg(feature = "cortex")]
use crate::mesh_rpc::{
    Codec, RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus, ServeError,
    ServeHandle, NRPC_TYPED_BAD_REQUEST, NRPC_TYPED_HANDLER_ERROR,
};
#[cfg(feature = "cortex")]
use serde::{de::DeserializeOwned, Serialize};

/// Builder for a [`ToolDescriptor`] that derives its JSON Schema
/// from Rust type parameters. Construct via [`metadata_for`], then
/// chain setters for the fields that aren't derivable from the
/// type signature (description, version, streaming, etc.).
///
/// Example:
///
/// ```ignore
/// use net_sdk::tool::metadata_for;
///
/// #[derive(schemars::JsonSchema, serde::Deserialize)]
/// struct WebSearchReq { query: String, max_results: u32 }
///
/// #[derive(schemars::JsonSchema, serde::Serialize)]
/// struct WebSearchResp { results: Vec<String> }
///
/// let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
///     .description("Search the web for relevant pages.")
///     .stateless(true)
///     .estimated_time_ms(500)
///     .tag("web")
///     .tag("research")
///     .build();
/// // hand the descriptor to `serve_tool` (lands in A-2).
/// ```
#[must_use = "ToolMetadataBuilder does nothing until `.build()` is called"]
pub struct ToolMetadataBuilder {
    descriptor: ToolDescriptor,
}

impl ToolMetadataBuilder {
    /// Replace the default human-readable description. Mandatory for
    /// any tool an LLM should reason about — the model reads this
    /// field to decide when to call.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.descriptor.description = Some(description.into());
        self
    }

    /// Override the version (defaults to `"1.0.0"` from
    /// `ToolCapability::new`). Two registrations of the same `name`
    /// at different versions surface as separate descriptors in
    /// `list_tools`.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.descriptor.version = version.into();
        self
    }

    /// Mark the tool as server-streaming (lowers into the future
    /// `serve_tool_streaming` rather than the unary `serve_tool`).
    /// Adapters use this flag to decide whether to render progress
    /// + delta envelopes vs. one terminal result.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.descriptor.streaming = streaming;
        self
    }

    /// Set the `stateless` flag. Pure-function tools (same input →
    /// same output, no session state) get cached, retried in
    /// parallel, etc. Stateful tools opt out.
    pub fn stateless(mut self, stateless: bool) -> Self {
        self.descriptor.stateless = stateless;
        self
    }

    /// Soft latency hint for the model scheduler / UI spinner.
    /// `0` means "no estimate" (the default).
    pub fn estimated_time_ms(mut self, ms: u32) -> Self {
        self.descriptor.estimated_time_ms = ms;
        self
    }

    /// Append one tag. Free-form; adapters surface tags as
    /// provider-specific metadata (e.g. Anthropic `cache_control`
    /// hints).
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.descriptor.tags.push(tag.into());
        self
    }

    /// Replace the tag list wholesale. Useful when the caller has
    /// the tags as a `Vec` already.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.descriptor.tags = tags;
        self
    }

    /// Append a required capability / dependency. Mirrors
    /// `ToolCapability::requires`. Adapters can use this to surface
    /// "tool needs X" dependencies (e.g. a `web_search` tool that
    /// depends on a configured API key).
    pub fn requires(mut self, dep: impl Into<String>) -> Self {
        self.descriptor.requires.push(dep.into());
        self
    }

    /// Attach pricing: a `net.pricing.terms@1` envelope as canonical
    /// JSON (author it with `net-payments`; the SDK carries the string
    /// opaquely — the substrate never parses payment objects). Announced
    /// under the tool's `pricing_terms` metadata key, so the price is
    /// visible at discovery time. A paid capability is metadata +
    /// invocation policy, not a different kind of tool — and displaying
    /// a price never implies authorization to spend it.
    ///
    /// **An announced price must be an enforced price.** The redeem gate
    /// (`PaymentAdmission` — quote redemption before the handler runs)
    /// lives in the MCP adapter's publication path, so a priced
    /// descriptor is refused by [`Mesh::serve_tool`] /
    /// [`Mesh::serve_tool_streaming`]
    /// (`ServeError::UnenforceablePricing`): those paths have no gate
    /// and would serve the "paid" tool free. Publish paid tools via
    /// `ServerPublisher::publish_tools` with a `payment_admission` gate.
    pub fn pricing_terms(mut self, terms_json: impl Into<String>) -> Self {
        self.descriptor.pricing_terms = Some(terms_json.into());
        self
    }

    /// Consume the builder and return the finished [`ToolDescriptor`].
    /// Pass this into the future `serve_tool` / `serve_tool_streaming`
    /// methods (A-2 / A-3).
    pub fn build(self) -> ToolDescriptor {
        self.descriptor
    }
}

/// Build a [`ToolMetadataBuilder`] for the given `(Req, Resp)` pair.
/// Both types must implement [`schemars::JsonSchema`]; the helper
/// derives JSON Schema (draft 2020-12) for each and stores them on
/// the descriptor as JSON-encoded strings.
///
/// The `name` parameter is the nRPC service name; same string the
/// caller will pass to `serve_tool` and the agent will see in
/// `list_tools`.
///
/// Description defaults to an empty string; callers should chain
/// [`ToolMetadataBuilder::description`] to set it before `.build()`.
/// `version` defaults to `"1.0.0"`; `stateless` defaults to `true`
/// (matching `ToolCapability::new`'s defaults); `streaming`
/// defaults to `false`.
pub fn metadata_for<Req, Resp>(name: impl Into<String>) -> ToolMetadataBuilder
where
    Req: schemars::JsonSchema,
    Resp: schemars::JsonSchema,
{
    let name = name.into();
    let input_schema = schemars::schema_for!(Req);
    let output_schema = schemars::schema_for!(Resp);
    let input_schema_json =
        serde_json::to_string(&input_schema).expect("schemars output is always valid JSON");
    let output_schema_json =
        serde_json::to_string(&output_schema).expect("schemars output is always valid JSON");
    ToolMetadataBuilder {
        descriptor: ToolDescriptor {
            tool_id: name.clone(),
            name,
            version: "1.0.0".to_string(),
            description: None,
            input_schema: Some(input_schema_json),
            output_schema: Some(output_schema_json),
            requires: Vec::new(),
            estimated_time_ms: 0,
            stateless: true,
            streaming: false,
            tags: Vec::new(),
            pricing_terms: None,
            node_count: 0,
        },
    }
}

// ============================================================================
// ToolServeHandle — owns the typed-RPC `ServeHandle` and reverses the
// tool_registry insert when dropped.
// ============================================================================

/// Returned by [`Mesh::serve_tool`]. Holds the underlying typed-RPC
/// `ServeHandle` (which unregisters the handler on Drop) plus a
/// clone of the `MeshNode`'s `tool_registry` so Drop can paired-
/// remove the descriptor.
///
/// Lifecycle:
/// - Construct via `Mesh::serve_tool(...)` — atomically registers
///   the handler, inserts the descriptor, and (on the first
///   `serve_tool` call) auto-installs the `tool.metadata.fetch`
///   service handler.
/// - On Drop:
///     1. Remove the descriptor from `tool_registry` — the next
///        `announce_capabilities` no longer emits the
///        `ai-tool:<name>` tag.
///     2. The inner `ServeHandle` drops, unregistering the nRPC
///        handler.
///
/// The auto-installed `tool.metadata.fetch` service stays
/// registered for the lifetime of the `Mesh`; it's harmless when
/// the registry is empty (returns `NotFound` for every request).
#[cfg(feature = "cortex")]
pub struct ToolServeHandle {
    /// Inner handle from `serve_rpc_typed`. Dropping it
    /// unregisters the nRPC handler.
    #[allow(dead_code)] // Held for Drop side effect.
    inner: ServeHandle,
    /// Tool registry the descriptor was inserted into. Drop's
    /// remove path uses this — keeping the `Arc` clone ensures
    /// the registry outlives the handle (otherwise the registry
    /// could vanish if the `Mesh` was dropped between handle
    /// construction and handle Drop).
    registry: Arc<ToolMetadataRegistry>,
    /// Name to remove on Drop. Stored separately because `inner`
    /// keeps its own `service` field private to the substrate
    /// crate.
    tool_id: String,
}

#[cfg(feature = "cortex")]
impl Drop for ToolServeHandle {
    fn drop(&mut self) {
        self.registry.remove(&self.tool_id);
        // `inner` drops on its own and reverses the nRPC handler
        // registration; we don't need to do anything else here.
    }
}

#[cfg(feature = "cortex")]
impl Mesh {
    /// Atomically register `handler` as an AI tool:
    ///
    /// 1. The descriptor is inserted into the local
    ///    `tool_registry` — subsequent `announce_capabilities`
    ///    calls auto-emit the `ai-tool:<name>` tag, the typed
    ///    `ToolCapability`, and the description / streaming /
    ///    tags metadata keys (see A-2a).
    /// 2. The handler is registered as an nRPC service at
    ///    `descriptor.tool_id` via `serve_rpc_typed` — the
    ///    substrate also tracks the service in `rpc_local_services`
    ///    so subsequent announces include the `nrpc:<name>` tag.
    /// 3. The first `serve_tool` call on this `Mesh` lazily
    ///    installs the `tool.metadata.fetch` server handler so
    ///    agents can pull the full descriptor for tools whose
    ///    schemas were too large for the capability-fold payload
    ///    budget. The install handle lives for the lifetime of
    ///    the `Mesh`; subsequent `serve_tool` calls skip it.
    ///
    /// If step 2 fails, step 1 is rolled back — the registry
    /// insert is paired-removed before the error returns, and the
    /// auto-install (if it happened in this call) stays in place
    /// (low cost; cleaning it up would race with concurrent
    /// `serve_tool` calls).
    ///
    /// The returned [`ToolServeHandle`] reverses both registry
    /// insert (step 1) and handler registration (step 2) on Drop.
    ///
    /// JSON codec is used unconditionally for AI tools — every
    /// provider (OpenAI, Anthropic, Gemini, MCP) consumes JSON
    /// for tool input/output. Wire-format consistency lets the
    /// adapter packages in M-* lower descriptors and dispatched
    /// tool-calls without per-tool codec negotiation.
    pub fn serve_tool<Req, Resp, F, Fut>(
        &self,
        descriptor: ToolDescriptor,
        handler: F,
    ) -> std::result::Result<ToolServeHandle, ServeError>
    where
        Req: DeserializeOwned + Send + Sync + 'static,
        Resp: Serialize + Send + Sync + 'static,
        F: Fn(Req) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Resp, String>> + Send + 'static,
    {
        let tool_id = descriptor.tool_id.clone();
        // An announced price must always be an enforced price. This path
        // has no payment-admission gate, so a priced descriptor would be
        // discovered as paid while serving free to any direct caller —
        // refuse loudly instead (see `ToolDescriptorBuilder::pricing_terms`).
        if descriptor.pricing_terms.is_some() {
            return Err(ServeError::UnenforceablePricing(tool_id));
        }
        let registry = self.inner().tool_registry().clone();

        // Step 1: registry insert. Done before the handler so the
        // descriptor is observable to `tool.metadata.fetch` the
        // moment the handler responds to its first call.
        let prior = registry.insert(descriptor);
        if let Some(prior) = prior {
            // Reject duplicate registrations rather than silently
            // overwriting — the prior handler still lives in
            // `rpc_local_services` from its own `serve_rpc_typed`
            // call; overwriting would leak that handler's
            // `ServeHandle` Drop and surface confusing behavior
            // (registry says X, handler answers Y).
            registry.insert(prior);
            return Err(ServeError::AlreadyServing(tool_id));
        }

        // Step 2: handler register. If this fails, paired-remove
        // the descriptor we just inserted so the registry doesn't
        // hold a phantom entry.
        let inner = match self.serve_rpc_typed::<Req, Resp, _, _>(&tool_id, Codec::Json, handler) {
            Ok(h) => h,
            Err(e) => {
                registry.remove(&tool_id);
                return Err(e);
            }
        };

        // Step 3: lazy auto-install of `tool.metadata.fetch`. The
        // handler answers `{ name } -> ToolMetadataResponse` for
        // any caller that wants the full descriptor (for schemas
        // too large to fit in the capability-fold payload).
        self.ensure_tool_metadata_fetch_installed();

        Ok(ToolServeHandle {
            inner,
            registry,
            tool_id,
        })
    }

    /// The **gated** sibling of [`Self::serve_tool`]: serve a tool whose
    /// descriptor announces `pricing_terms`, with every invocation's
    /// quote redeemed through `gate` **before** the handler runs.
    ///
    /// This is the native completion of the invariant `serve_tool`
    /// enforces by refusal ("an announced price is an enforced price on
    /// every serving path"): `serve_tool` rejects priced descriptors
    /// (`ServeError::UnenforceablePricing`); this method is the
    /// sanctioned way to serve them without the MCP adapter.
    ///
    /// Wire contract (identical to the MCP wrap path, so demand-side
    /// gateways pay native and wrapped tools the same way):
    ///
    /// - the caller attaches the quote id as the
    ///   [`HDR_PAYMENT_QUOTE`](crate::tool_payment::HDR_PAYMENT_QUOTE)
    ///   request header, and optionally the signed invocation binding as
    ///   [`HDR_PAYMENT_BINDING`](crate::tool_payment::HDR_PAYMENT_BINDING);
    /// - a refusal (missing header, unpaid / frozen / already-redeemed
    ///   quote, gate failure) is the application error
    ///   [`ERR_PAYMENT`](crate::tool_payment::ERR_PAYMENT) — fail-closed,
    ///   the handler never sees an unpaid call;
    /// - ordering mirrors the wrap path: the request body is decoded
    ///   **before** the gate, so a structurally invalid call (one that
    ///   can never execute) is rejected without consuming the quote.
    ///
    /// A descriptor **without** `pricing_terms` is refused
    /// (`ServeError::MissingPricingTerms`): a gate on an unannounced
    /// price would refuse every caller with no way to know why — serve
    /// free tools via [`Self::serve_tool`].
    ///
    /// Registry/rollback/Drop semantics are identical to
    /// [`Self::serve_tool`]; the codec is JSON, unconditionally, like
    /// every AI-tool path.
    pub fn serve_tool_paid<Req, Resp, F, Fut>(
        &self,
        descriptor: ToolDescriptor,
        gate: std::sync::Arc<dyn crate::tool_payment::ToolPaymentGate>,
        handler: F,
    ) -> std::result::Result<ToolServeHandle, ServeError>
    where
        Req: DeserializeOwned + Send + Sync + 'static,
        Resp: Serialize + Send + Sync + 'static,
        F: Fn(Req) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Resp, String>> + Send + 'static,
    {
        let tool_id = descriptor.tool_id.clone();
        if descriptor.pricing_terms.is_none() {
            return Err(ServeError::MissingPricingTerms(tool_id));
        }
        let registry = self.inner().tool_registry().clone();

        // Step 1: registry insert (duplicate rejection + paired-remove
        // rollback, exactly as `serve_tool`).
        let prior = registry.insert(descriptor);
        if let Some(prior) = prior {
            registry.insert(prior);
            return Err(ServeError::AlreadyServing(tool_id));
        }

        // Step 2: the gated handler. Untyped registration — the payment
        // headers live on the RpcContext, which the typed path does not
        // expose; the wrapper reproduces the typed codec conventions
        // (bad body → NRPC_TYPED_BAD_REQUEST, handler error →
        // NRPC_TYPED_HANDLER_ERROR) around the gate.
        let paid = PaidToolHandler {
            tool_id: tool_id.clone(),
            gate,
            codec: Codec::Json,
            inner: std::sync::Arc::new(handler),
            _req: std::marker::PhantomData::<Req>,
            _resp: std::marker::PhantomData::<Resp>,
        };
        let inner = match self.serve_rpc(&tool_id, std::sync::Arc::new(paid)) {
            Ok(h) => h,
            Err(e) => {
                registry.remove(&tool_id);
                return Err(e);
            }
        };

        // Step 3: lazy `tool.metadata.fetch` install, as `serve_tool`.
        self.ensure_tool_metadata_fetch_installed();

        Ok(ToolServeHandle {
            inner,
            registry,
            tool_id,
        })
    }

    /// Streaming variant of [`Self::serve_tool`]. The handler
    /// returns a [`futures::Stream`] of [`ToolEvent`]s; the SDK
    /// serializes each item as one JSON-encoded chunk on the
    /// underlying `serve_rpc_streaming_typed` path.
    ///
    /// Contract for handlers:
    ///
    /// - Emit one terminal event ([`ToolEvent::Result`] or
    ///   [`ToolEvent::Error`]) to close the stream cleanly. The SDK
    ///   stops driving the user's stream the moment a terminal
    ///   event is emitted — any items the handler tries to yield
    ///   after a terminal are not transmitted.
    /// - If the stream ends without a terminal event, the SDK
    ///   synthesizes [`ToolEvent::Error`] with
    ///   `code = "missing_terminal"` so callers can rely on every
    ///   stream ending with a terminal envelope.
    ///
    /// `descriptor.streaming` is forced to `true` on registration —
    /// the `tool::<id>::streaming` metadata key emitted by the
    /// announce merge (A-2a) reflects the actual register path the
    /// host took, not the value the caller built into the
    /// descriptor.
    ///
    /// Atomicity, Drop-reverses, and lazy `tool.metadata.fetch`
    /// install all behave the same as [`Self::serve_tool`].
    pub fn serve_tool_streaming<Req, F, Fut, St>(
        &self,
        mut descriptor: ToolDescriptor,
        handler: F,
    ) -> std::result::Result<ToolServeHandle, ServeError>
    where
        Req: DeserializeOwned + Send + Sync + 'static,
        F: Fn(Req) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = St> + Send + 'static,
        St: futures::Stream<Item = ToolEvent> + Send + 'static,
    {
        // Force the streaming flag on so announces reflect reality
        // even if the caller forgot `.streaming(true)` on the builder.
        descriptor.streaming = true;
        let tool_id = descriptor.tool_id.clone();
        // Same refusal as `serve_tool`: no payment gate on this path, so
        // an announced price would be unenforceable.
        if descriptor.pricing_terms.is_some() {
            return Err(ServeError::UnenforceablePricing(tool_id));
        }
        let registry = self.inner().tool_registry().clone();

        // Step 1: registry insert (same paired-remove rollback on
        // failure as `serve_tool`).
        let prior = registry.insert(descriptor);
        if let Some(prior) = prior {
            registry.insert(prior);
            return Err(ServeError::AlreadyServing(tool_id));
        }

        // Step 2: typed-streaming handler register. We drive the
        // user's stream and emit each `ToolEvent` as one chunk via
        // the typed sink. Terminal events stop the loop; if the
        // stream ends without one, synthesize a `missing_terminal`
        // `Error`.
        let handler = Arc::new(handler);
        let inner = match self
            .serve_rpc_streaming_typed::<Req, ToolEvent, _, _>(&tool_id, Codec::Json, move |req, sink| {
                let handler = handler.clone();
                async move {
                    use futures::StreamExt;
                    let stream = handler(req).await;
                    futures::pin_mut!(stream);
                    let mut seen_terminal = false;
                    while let Some(event) = stream.next().await {
                        let terminal = event.is_terminal();
                        sink.send(&event)
                            .map_err(|e| format!("tool event send: {e}"))?;
                        if terminal {
                            seen_terminal = true;
                            break;
                        }
                    }
                    if !seen_terminal {
                        let synthesized = ToolEvent::Error {
                            code: "missing_terminal".to_string(),
                            message:
                                "tool handler ended its stream without emitting a terminal Result or Error event"
                                    .to_string(),
                            details: None,
                        };
                        sink.send(&synthesized)
                            .map_err(|e| format!("synthesized terminal send: {e}"))?;
                    }
                    Ok(())
                }
            }) {
            Ok(h) => h,
            Err(e) => {
                registry.remove(&tool_id);
                return Err(e);
            }
        };

        // Step 3: lazy auto-install of `tool.metadata.fetch`.
        self.ensure_tool_metadata_fetch_installed();

        Ok(ToolServeHandle {
            inner,
            registry,
            tool_id,
        })
    }

    /// Capability-routed unary tool call. Encodes `request` as JSON,
    /// resolves a target node from `nrpc:<tool_id>` in the local
    /// capability fold (via [`net::adapter::net::MeshNode::call_service`]),
    /// awaits the typed `Resp`.
    ///
    /// Codec is JSON unconditionally — every AI provider (OpenAI,
    /// Anthropic, Gemini, MCP) consumes JSON for tool input/output,
    /// so the substrate enforces one codec for the whole tool surface.
    /// Adapters can lower descriptors and dispatched calls without
    /// per-tool codec negotiation.
    ///
    /// Returns `RpcError::NoRoute` if no host currently serves the
    /// tool. Bubbles handler errors as `RpcError::ServerError` with
    /// status `NRPC_TYPED_HANDLER_ERROR` carrying the handler's
    /// error message.
    pub async fn call_tool<Req, Resp>(
        &self,
        tool_id: &str,
        request: &Req,
    ) -> std::result::Result<Resp, crate::mesh_rpc::RpcError>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        self.call_service_typed::<Req, Resp>(
            tool_id,
            request,
            crate::mesh_rpc::CallOptionsTyped {
                raw: Default::default(),
                codec: Codec::Json,
            },
        )
        .await
    }

    /// Capability-routed streaming tool call. Encodes `request` as
    /// JSON, opens a streaming call against `nrpc:<tool_id>` via
    /// the substrate's `call_service_streaming` (S-1), returns an
    /// [`crate::mesh_rpc::RpcStreamTyped<ToolEvent>`] that decodes
    /// each chunk as a [`ToolEvent`].
    ///
    /// Stream lifecycle:
    /// - Server emits zero or more `Start` / `Progress` / `Delta`
    ///   envelopes, then exactly one terminal `Result` or `Error`.
    ///   The SDK does NOT enforce this contract on the caller side
    ///   — it surfaces the wire events verbatim. Adapters
    ///   (`formats/anthropic`, `formats/openai`, etc.) own the
    ///   contract enforcement.
    /// - If the handler ends without a terminal event, the server-
    ///   side wrapper synthesizes
    ///   `ToolEvent::Error { code: "missing_terminal", ... }` — see
    ///   [`Self::serve_tool_streaming`].
    /// - Dropping the returned stream emits CANCEL to the server
    ///   (substrate cancel-token contract).
    pub async fn call_tool_streaming<Req>(
        &self,
        tool_id: &str,
        request: &Req,
    ) -> std::result::Result<crate::mesh_rpc::RpcStreamTyped<ToolEvent>, crate::mesh_rpc::RpcError>
    where
        Req: serde::Serialize,
    {
        self.call_service_streaming_typed::<Req, ToolEvent>(
            tool_id,
            request,
            crate::mesh_rpc::CallOptionsTyped {
                raw: Default::default(),
                codec: Codec::Json,
            },
        )
        .await
    }

    /// Walk the capability fold for every published AI tool and
    /// return one [`ToolDescriptor`] per (tool_id, version) with
    /// `node_count` filled in. One in-memory pass; no network.
    ///
    /// `matcher` is the standard substrate [`TagMatcher`] — an entry
    /// is included if ANY of its tags match. Common shapes:
    ///
    /// - `None` — every tool the local fold has seen.
    /// - `Some(TagMatcher::Prefix { value: "ai-tool:".into() })` —
    ///   "every node advertising AT LEAST ONE AI tool" (filters out
    ///   peers that don't publish any tool but otherwise pass the
    ///   fold).
    /// - `Some(TagMatcher::Prefix { value: "region.eu".into() })` —
    ///   tools served by EU-region hosts.
    ///
    /// Delegates to
    /// [`net::adapter::net::MeshNode::list_tools`](net::adapter::net::MeshNode::list_tools).
    pub fn list_tools(&self, matcher: Option<&TagMatcher>) -> Vec<ToolDescriptor> {
        self.inner().list_tools(matcher)
    }

    /// Subscribe to a stream of [`ToolListChange`] events for every
    /// dynamic addition / removal / publisher-count change in the
    /// local capability fold's tool view, filtered by `matcher`.
    ///
    /// Event-driven: a change is delivered the moment the capability
    /// fold mutates (latency is bounded by fold-apply, not a timer),
    /// and an idle fold does zero periodic work.
    ///
    /// `interval` is a *debounce ceiling*, not a poll cadence:
    /// - `None` — pure event-driven; the watch only wakes on a real
    ///   mutation.
    /// - `Some(d)` — additionally guarantees a re-diff at least every
    ///   `d` as a safety net, independent of the change signal.
    ///
    /// The returned [`ToolListWatch`] implements
    /// `futures::Stream<Item = ToolListChange>`. Dropping it — or
    /// calling [`ToolListWatch::cancel`] — ends the stream and stops
    /// the underlying substrate task.
    ///
    /// First event fires AFTER the initial baseline snapshot — call
    /// [`Self::list_tools`] first if you need the starting shape.
    ///
    /// Delegates to
    /// [`net::adapter::net::MeshNode::watch_tools`](net::adapter::net::MeshNode::watch_tools).
    pub fn watch_tools(
        &self,
        matcher: Option<TagMatcher>,
        interval: Option<std::time::Duration>,
    ) -> ToolListWatch {
        self.node_arc().watch_tools(matcher, interval)
    }

    /// Idempotent — installs the `tool.metadata.fetch` nRPC
    /// service handler if not yet present. Holds a `parking_lot`
    /// mutex; the first caller through wins, the rest see
    /// `Some(_)` and return immediately.
    fn ensure_tool_metadata_fetch_installed(&self) {
        let mut slot = self.tool_metadata_fetch.lock();
        if slot.is_some() {
            return;
        }
        let registry = self.node().tool_registry().clone();
        let handler = move |req: ToolMetadataRequest| {
            let registry = registry.clone();
            async move {
                Ok(match registry.get(&req.name) {
                    Some(descriptor) => ToolMetadataResponse::Found { descriptor },
                    None => ToolMetadataResponse::NotFound { name: req.name },
                })
            }
        };
        // If install fails (e.g. the service name's already taken
        // by some manual `serve_rpc_typed` call — unlikely but
        // possible), leave `slot` as `None`; subsequent
        // `serve_tool` calls retry. The failure is silent here
        // because (a) it's recoverable on retry, (b) it's surfaceable
        // via `tool.metadata.fetch` returning NotFound (or transport
        // errors) at the agent side, and (c) `ensure_*` is called
        // from inside an infallible-returning `serve_tool` path —
        // surfacing the error would require a fallible signature
        // and complicate the happy path. The conflict is
        // operator-misconfiguration, not transient failure.
        if let Ok(handle) = self.serve_rpc_typed::<ToolMetadataRequest, ToolMetadataResponse, _, _>(
            TOOL_METADATA_FETCH_SERVICE,
            Codec::Json,
            handler,
        ) {
            *slot = Some(handle);
        }
    }
}

// ============================================================================
// Format translators
// ============================================================================
//
// Lower a `ToolDescriptor` into a provider-native tool definition
// shape (OpenAI, Anthropic, MCP). Pure functions, no transitive deps
// beyond serde_json. Rust agent code that hits a provider's HTTP API
// uses these to populate the `tools` array in its request payload.
//
// The plan's M-1/M-2/M-3 ship the same functionality in Python and
// TypeScript packages; the Rust version here is the canonical
// reference implementation. Cross-language tests (T-1) pin the JSON
// shape across all three.

#[cfg(feature = "cortex")]
pub mod formats {
    //! Provider-native tool-definition translators.
    //!
    //! Each submodule exports two directions of conversion:
    //!
    //! 1. `to_<provider>_tool(&ToolDescriptor) -> Value` —
    //!    descriptor → provider's tool-definition shape, used to
    //!    populate the `tools` array on a request to the provider's
    //!    HTTP API.
    //! 2. `lower_<provider>_tool_call(&Value) -> Result<ToolCallSpec, _>`
    //!    — parse the provider's tool-call reply (OpenAI's
    //!    `tool_calls[]`, Anthropic's `tool_use` content block, etc.)
    //!    into a [`ToolCallSpec`] the agent can hand to
    //!    `Mesh::call_tool(spec.name, &spec.arguments)`.
    //!
    //! All translators short-circuit on a missing `input_schema` by
    //! emitting an empty-object schema (`{"type": "object",
    //! "properties": {}}`). Providers reject a `null` parameter
    //! schema in their strict-mode validators, but they all accept
    //! the empty-properties object as "no arguments."
    //!
    //! The plan (M-1..M-4) defines parallel Python and TypeScript
    //! packages with the same lowering; this Rust module is the
    //! canonical reference. Cross-language tests (T-1) pin byte
    //! equality.

    use super::ToolDescriptor;
    use serde_json::{json, Value};

    /// One tool invocation parsed out of a provider's reply. The
    /// canonical hand-off shape between an LLM-provider adapter and
    /// `Mesh::call_tool` / `Mesh::call_tool_streaming`.
    ///
    /// `provider_call_id` round-trips the provider's own identifier
    /// (OpenAI's `tool_calls[].id`, Anthropic's `tool_use.id`, MCP's
    /// optional `id`). Adapters use it to correlate the tool-call
    /// result back into the provider's expected reply shape (e.g.
    /// `{"role": "tool", "tool_call_id": "<id>", "content": "..."}`).
    /// `None` when the provider didn't supply one.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ToolCallSpec {
        /// nRPC tool_id to invoke. Matches `ToolDescriptor::tool_id` /
        /// the `name` field every provider uses for its tool slot.
        pub name: String,
        /// JSON-encoded arguments to hand to `Mesh::call_tool`.
        /// Stored as a string so the caller can either feed it
        /// straight to a raw byte API or parse it with `serde_json`
        /// — the parse vs. forward decision is provider-agnostic.
        pub arguments_json: String,
        /// Provider-supplied call id, when present. Adapters carry
        /// this back into the tool-result reply so the LLM can
        /// correlate the response.
        pub provider_call_id: Option<String>,
    }

    /// Error returned when a provider's tool-call reply doesn't
    /// match the expected shape (missing `name`, malformed
    /// arguments, etc.). Each variant carries the field that was
    /// missing or malformed so adapters can produce a tight
    /// diagnostic.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ToolCallParseError {
        /// The provider's reply was missing a required field.
        MissingField(&'static str),
        /// A field's type didn't match what the provider's spec
        /// promises (e.g. `name` was not a string).
        WrongType {
            /// Field name in the provider's reply shape.
            field: &'static str,
            /// What the spec requires.
            expected: &'static str,
        },
        /// The provider sent a JSON-encoded arguments string that
        /// failed to parse. Carried verbatim so the caller can
        /// log the offender. (OpenAI's `function.arguments` is a
        /// string of JSON, not a parsed object — adapters
        /// double-encode/decode the boundary.)
        InvalidArgumentsJson(String),
    }

    impl std::fmt::Display for ToolCallParseError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::MissingField(name) => write!(f, "tool-call reply missing field `{name}`"),
                Self::WrongType { field, expected } => write!(
                    f,
                    "tool-call reply field `{field}` had wrong type (expected {expected})"
                ),
                Self::InvalidArgumentsJson(detail) => {
                    write!(f, "tool-call arguments were not valid JSON: {detail}")
                }
            }
        }
    }

    impl std::error::Error for ToolCallParseError {}

    /// Parse the descriptor's stored input schema (a JSON-encoded
    /// string). Falls back to an empty-object schema if missing or
    /// malformed — provider strict-mode validators require a
    /// non-null `parameters` / `input_schema` field.
    fn input_schema_value(desc: &ToolDescriptor) -> Value {
        desc.input_schema
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
    }

    /// Translators for the OpenAI Chat Completions / Responses API
    /// `tools` array shape.
    pub mod openai {
        use super::*;

        /// Lower a [`ToolDescriptor`] into an OpenAI tool definition.
        ///
        /// Wire shape:
        /// ```json
        /// {
        ///   "type": "function",
        ///   "function": {
        ///     "name": "<tool_id>",
        ///     "description": "<description>",
        ///     "parameters": <input_schema>,
        ///     "strict": <bool>
        ///   }
        /// }
        /// ```
        ///
        /// `strict` is set to `true` when `descriptor.input_schema` was
        /// publishable on the fold (i.e. not dropped due to size).
        /// OpenAI's strict-mode tool calling requires the schema to be
        /// present and conform to a subset of JSON Schema; we surface
        /// it as a hint, not a guarantee — callers that explicitly
        /// need non-strict can post-process the returned `Value`.
        pub fn to_openai_tool(desc: &ToolDescriptor) -> Value {
            let parameters = input_schema_value(desc);
            let strict = desc.input_schema.is_some();
            json!({
                "type": "function",
                "function": {
                    "name": desc.tool_id,
                    "description": desc.description.clone().unwrap_or_default(),
                    "parameters": parameters,
                    "strict": strict,
                }
            })
        }

        /// Parse one OpenAI `tool_calls[]` entry into a [`ToolCallSpec`].
        /// OpenAI's reply shape is:
        /// ```json
        /// {
        ///   "id": "<call_id>",
        ///   "type": "function",
        ///   "function": {
        ///     "name": "<tool_id>",
        ///     "arguments": "<JSON-encoded string>"
        ///   }
        /// }
        /// ```
        ///
        /// `function.arguments` is a STRING containing JSON — the
        /// OpenAI API doesn't parse it. The spec carries the string
        /// verbatim so the caller can either forward to a raw byte
        /// API or `serde_json::from_str` it. This sidesteps double
        /// re-serialization on the happy path.
        pub fn lower_openai_tool_call(call: &Value) -> Result<ToolCallSpec, ToolCallParseError> {
            let function = call
                .get("function")
                .ok_or(ToolCallParseError::MissingField("function"))?;
            let name = function
                .get("name")
                .ok_or(ToolCallParseError::MissingField("function.name"))?
                .as_str()
                .ok_or(ToolCallParseError::WrongType {
                    field: "function.name",
                    expected: "string",
                })?
                .to_string();
            let arguments_json = function
                .get("arguments")
                .ok_or(ToolCallParseError::MissingField("function.arguments"))?
                .as_str()
                .ok_or(ToolCallParseError::WrongType {
                    field: "function.arguments",
                    expected: "string (JSON-encoded)",
                })?
                .to_string();
            // Validate it parses; fail fast with a tight diagnostic
            // rather than letting the malformed string ride through
            // `call_tool` and surface as a server-side decode error.
            if let Err(e) = serde_json::from_str::<Value>(&arguments_json) {
                return Err(ToolCallParseError::InvalidArgumentsJson(format!("{e}")));
            }
            let provider_call_id = call.get("id").and_then(|v| v.as_str()).map(String::from);
            Ok(ToolCallSpec {
                name,
                arguments_json,
                provider_call_id,
            })
        }
    }

    /// Translators for the Anthropic Messages API `tools` array shape.
    pub mod anthropic {
        use super::*;

        /// Lower a [`ToolDescriptor`] into an Anthropic tool
        /// definition.
        ///
        /// Wire shape:
        /// ```json
        /// {
        ///   "name": "<tool_id>",
        ///   "description": "<description>",
        ///   "input_schema": <input_schema>
        /// }
        /// ```
        ///
        /// Anthropic does not have a strict-mode flag at the tool
        /// level (it relies on schema-validated tool inputs as the
        /// default). `description` defaults to an empty string when
        /// the descriptor omits one — Anthropic accepts it but a
        /// real description materially affects the model's
        /// tool-selection behavior, so callers should always set one.
        pub fn to_anthropic_tool(desc: &ToolDescriptor) -> Value {
            json!({
                "name": desc.tool_id,
                "description": desc.description.clone().unwrap_or_default(),
                "input_schema": input_schema_value(desc),
            })
        }

        /// Parse one Anthropic `tool_use` content block into a
        /// [`ToolCallSpec`]. Block shape:
        /// ```json
        /// {
        ///   "type": "tool_use",
        ///   "id": "toolu_<id>",
        ///   "name": "<tool_id>",
        ///   "input": { … }
        /// }
        /// ```
        ///
        /// Anthropic's `input` is already a parsed object (unlike
        /// OpenAI's string-encoded arguments), so the spec
        /// re-serializes it once to preserve the
        /// `arguments_json: String` contract on `ToolCallSpec`.
        pub fn lower_anthropic_tool_use(block: &Value) -> Result<ToolCallSpec, ToolCallParseError> {
            let name = block
                .get("name")
                .ok_or(ToolCallParseError::MissingField("name"))?
                .as_str()
                .ok_or(ToolCallParseError::WrongType {
                    field: "name",
                    expected: "string",
                })?
                .to_string();
            let input = block
                .get("input")
                .ok_or(ToolCallParseError::MissingField("input"))?;
            let arguments_json = serde_json::to_string(input)
                .map_err(|e| ToolCallParseError::InvalidArgumentsJson(format!("{e}")))?;
            let provider_call_id = block.get("id").and_then(|v| v.as_str()).map(String::from);
            Ok(ToolCallSpec {
                name,
                arguments_json,
                provider_call_id,
            })
        }
    }

    /// Translators for the Model Context Protocol (MCP) `tools/list`
    /// response shape.
    pub mod mcp {
        use super::*;

        /// Lower a [`ToolDescriptor`] into an MCP tool definition.
        ///
        /// Wire shape:
        /// ```json
        /// {
        ///   "name": "<tool_id>",
        ///   "description": "<description>",
        ///   "inputSchema": <input_schema>
        /// }
        /// ```
        ///
        /// MCP's tool shape is the closest to our native
        /// `ToolDescriptor` — same `name` field, same
        /// JSON-Schema-shaped input descriptor, just camelCase
        /// `inputSchema` (vs Anthropic's `input_schema`).
        pub fn to_mcp_tool(desc: &ToolDescriptor) -> Value {
            json!({
                "name": desc.tool_id,
                "description": desc.description.clone().unwrap_or_default(),
                "inputSchema": input_schema_value(desc),
            })
        }

        /// Parse an MCP `tools/call` request into a [`ToolCallSpec`].
        /// Request params shape:
        /// ```json
        /// { "name": "<tool_id>", "arguments": { … } }
        /// ```
        ///
        /// MCP requests don't carry a call_id at this layer (the
        /// JSON-RPC envelope's `id` lives one level up). The spec
        /// leaves `provider_call_id` as `None` — the caller is
        /// expected to thread the JSON-RPC `id` separately.
        pub fn lower_mcp_tools_call(params: &Value) -> Result<ToolCallSpec, ToolCallParseError> {
            let name = params
                .get("name")
                .ok_or(ToolCallParseError::MissingField("name"))?
                .as_str()
                .ok_or(ToolCallParseError::WrongType {
                    field: "name",
                    expected: "string",
                })?
                .to_string();
            let arguments = params
                .get("arguments")
                .ok_or(ToolCallParseError::MissingField("arguments"))?;
            let arguments_json = serde_json::to_string(arguments)
                .map_err(|e| ToolCallParseError::InvalidArgumentsJson(format!("{e}")))?;
            Ok(ToolCallSpec {
                name,
                arguments_json,
                provider_call_id: None,
            })
        }
    }

    /// Translators for the Google Gemini `generateContent` API
    /// function-calling shape.
    pub mod gemini {
        use super::*;

        /// Lower a [`ToolDescriptor`] into a Gemini
        /// `FunctionDeclaration`.
        ///
        /// Wire shape (one entry in a
        /// `tools[0].function_declarations[]` array):
        /// ```json
        /// {
        ///   "name": "<tool_id>",
        ///   "description": "<description>",
        ///   "parameters": <input_schema>
        /// }
        /// ```
        ///
        /// Gemini wraps function declarations under
        /// `tools: [{ function_declarations: [ … ] }]`; this helper
        /// returns ONE declaration. The caller is responsible for
        /// the outer wrapping — keeps the API symmetric with the
        /// other provider translators.
        pub fn to_gemini_function_declaration(desc: &ToolDescriptor) -> Value {
            json!({
                "name": desc.tool_id,
                "description": desc.description.clone().unwrap_or_default(),
                "parameters": input_schema_value(desc),
            })
        }

        /// Parse one Gemini `functionCall` part into a
        /// [`ToolCallSpec`]. Part shape:
        /// ```json
        /// { "name": "<tool_id>", "args": { … } }
        /// ```
        ///
        /// Gemini doesn't supply a call id — the spec's
        /// `provider_call_id` is `None`. Multi-call sequences are
        /// identified positionally by their index in the model's
        /// reply.
        pub fn lower_gemini_function_call(
            call: &Value,
        ) -> Result<ToolCallSpec, ToolCallParseError> {
            let name = call
                .get("name")
                .ok_or(ToolCallParseError::MissingField("name"))?
                .as_str()
                .ok_or(ToolCallParseError::WrongType {
                    field: "name",
                    expected: "string",
                })?
                .to_string();
            let args = call
                .get("args")
                .ok_or(ToolCallParseError::MissingField("args"))?;
            let arguments_json = serde_json::to_string(args)
                .map_err(|e| ToolCallParseError::InvalidArgumentsJson(format!("{e}")))?;
            Ok(ToolCallSpec {
                name,
                arguments_json,
                provider_call_id: None,
            })
        }
    }
}

/// The handler wrapper behind [`Mesh::serve_tool_paid`]: decode →
/// redeem → run → encode. Decode comes first so a structurally invalid
/// call never consumes the quote (the MCP wrap path's ordering); the
/// gate runs before the user's handler so it never sees an unpaid call.
#[cfg(feature = "cortex")]
struct PaidToolHandler<Req, Resp, F> {
    tool_id: String,
    gate: std::sync::Arc<dyn crate::tool_payment::ToolPaymentGate>,
    codec: Codec,
    inner: std::sync::Arc<F>,
    _req: std::marker::PhantomData<Req>,
    _resp: std::marker::PhantomData<Resp>,
}

#[cfg(feature = "cortex")]
fn paid_header<'a>(headers: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_slice())
}

#[cfg(feature = "cortex")]
#[async_trait::async_trait]
impl<Req, Resp, F, Fut> RpcHandler for PaidToolHandler<Req, Resp, F>
where
    Req: DeserializeOwned + Send + Sync + 'static,
    Resp: Serialize + Send + Sync + 'static,
    F: Fn(Req) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<Resp, String>> + Send + 'static,
{
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        // [1] Decode BEFORE the gate: a call that can never execute must
        //     never consume the quote. Same code as the typed path.
        let req: Req = match self.codec.decode(&ctx.payload.body) {
            Ok(r) => r,
            Err(e) => {
                return Err(RpcHandlerError::Application {
                    code: NRPC_TYPED_BAD_REQUEST,
                    message: format!("typed handler: bad request body: {e}"),
                })
            }
        };

        // [2] The payment gate — the quote is redeemed (settled, billed,
        //     unfrozen, bound to this tool, never redeemed before) or the
        //     call is refused. Fail-closed: the handler never runs unpaid.
        let quote_id = paid_header(&ctx.payload.headers, crate::tool_payment::HDR_PAYMENT_QUOTE)
            .and_then(|raw| std::str::from_utf8(raw).ok())
            .ok_or_else(|| RpcHandlerError::Application {
                code: crate::tool_payment::ERR_PAYMENT,
                message: "paid tool invoked without a payment quote header".to_string(),
            })?;
        let binding = paid_header(
            &ctx.payload.headers,
            crate::tool_payment::HDR_PAYMENT_BINDING,
        );
        self.gate
            .redeem(&self.tool_id, quote_id, binding)
            .await
            .map_err(|reason| RpcHandlerError::Application {
                code: crate::tool_payment::ERR_PAYMENT,
                message: reason,
            })?;

        // [3] Run + encode, mirroring the typed-handler conventions.
        let resp = (self.inner)(req)
            .await
            .map_err(|message| RpcHandlerError::Application {
                code: NRPC_TYPED_HANDLER_ERROR,
                message,
            })?;
        let body = self
            .codec
            .encode(&resp)
            .map_err(|e| RpcHandlerError::Internal(format!("typed handler encode: {e}")))?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: body.into(),
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(JsonSchema, Deserialize, Serialize)]
    #[allow(dead_code)]
    struct WebSearchReq {
        /// The query string.
        query: String,
        /// Maximum results to return.
        max_results: u32,
    }

    #[derive(JsonSchema, Deserialize, Serialize)]
    #[allow(dead_code)]
    struct WebSearchResp {
        results: Vec<String>,
    }

    #[test]
    fn metadata_for_derives_schemas_and_sets_defaults() {
        let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search").build();
        assert_eq!(descriptor.tool_id, "web_search");
        assert_eq!(descriptor.name, "web_search");
        assert_eq!(descriptor.version, "1.0.0");
        assert!(descriptor.description.is_none());
        assert!(descriptor.stateless);
        assert!(!descriptor.streaming);
        assert_eq!(descriptor.estimated_time_ms, 0);
        assert_eq!(descriptor.node_count, 0);
        assert!(descriptor.tags.is_empty());
        assert!(descriptor.requires.is_empty());

        // Schemas must be present + parse as valid JSON.
        let input = descriptor
            .input_schema
            .as_ref()
            .expect("input schema present");
        let parsed: serde_json::Value =
            serde_json::from_str(input).expect("input schema must be valid JSON");
        // Object with `query` + `max_results` properties.
        let props = parsed
            .get("properties")
            .expect("object schema has properties");
        assert!(props.get("query").is_some());
        assert!(props.get("max_results").is_some());

        let output = descriptor
            .output_schema
            .as_ref()
            .expect("output schema present");
        let _: serde_json::Value =
            serde_json::from_str(output).expect("output schema must be valid JSON");
    }

    #[test]
    fn builder_setters_apply_in_chain() {
        let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
            .description("Search the web.")
            .version("2.1.0")
            .streaming(true)
            .stateless(false)
            .estimated_time_ms(500)
            .tag("web")
            .tag("research")
            .requires("api_key:tavily")
            .build();
        assert_eq!(descriptor.description.as_deref(), Some("Search the web."));
        assert_eq!(descriptor.version, "2.1.0");
        assert!(descriptor.streaming);
        assert!(!descriptor.stateless);
        assert_eq!(descriptor.estimated_time_ms, 500);
        assert_eq!(descriptor.tags, vec!["web", "research"]);
        assert_eq!(descriptor.requires, vec!["api_key:tavily"]);
    }

    #[test]
    fn builder_tags_replaces_wholesale() {
        let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
            .tag("first")
            .tags(vec!["replaced".into(), "second".into()])
            .build();
        // `tags(...)` wholesale replaces — `first` is gone.
        assert_eq!(descriptor.tags, vec!["replaced", "second"]);
    }

    fn sample_descriptor() -> ToolDescriptor {
        metadata_for::<WebSearchReq, WebSearchResp>("web_search")
            .description("Search the web.")
            .build()
    }

    #[test]
    fn openai_tool_has_function_type_and_strict_when_schema_present() {
        let desc = sample_descriptor();
        let tool = formats::openai::to_openai_tool(&desc);
        assert_eq!(tool["type"], "function");
        let function = &tool["function"];
        assert_eq!(function["name"], "web_search");
        assert_eq!(function["description"], "Search the web.");
        assert_eq!(function["strict"], true);
        // Parameters carry the schema's `properties` block.
        let params = &function["parameters"];
        assert!(
            params["properties"]["query"].is_object(),
            "input_schema's `query` property must surface in parameters",
        );
    }

    #[test]
    fn anthropic_tool_carries_input_schema_directly() {
        let desc = sample_descriptor();
        let tool = formats::anthropic::to_anthropic_tool(&desc);
        assert_eq!(tool["name"], "web_search");
        assert_eq!(tool["description"], "Search the web.");
        // Anthropic uses `input_schema` (snake_case).
        let schema = &tool["input_schema"];
        assert!(schema["properties"]["query"].is_object());
        assert!(
            tool.get("strict").is_none(),
            "Anthropic has no tool-level strict flag"
        );
    }

    #[test]
    fn mcp_tool_uses_input_schema_camelcase() {
        let desc = sample_descriptor();
        let tool = formats::mcp::to_mcp_tool(&desc);
        assert_eq!(tool["name"], "web_search");
        assert_eq!(tool["description"], "Search the web.");
        // MCP uses `inputSchema` (camelCase) — pinned by the spec.
        let schema = &tool["inputSchema"];
        assert!(schema["properties"]["query"].is_object());
    }

    #[test]
    fn gemini_function_declaration_uses_parameters_field() {
        let desc = sample_descriptor();
        let decl = formats::gemini::to_gemini_function_declaration(&desc);
        assert_eq!(decl["name"], "web_search");
        assert_eq!(decl["description"], "Search the web.");
        // Gemini uses `parameters` (same key as OpenAI, no wrapping
        // `function` envelope). The schema rides directly underneath.
        let params = &decl["parameters"];
        assert!(params["properties"]["query"].is_object());
    }

    #[test]
    fn openai_lower_tool_call_extracts_name_and_arguments() {
        use formats::openai::lower_openai_tool_call;
        let call = serde_json::json!({
            "id": "call_abc123",
            "type": "function",
            "function": {
                "name": "web_search",
                "arguments": "{\"query\":\"mesh\"}"
            }
        });
        let spec = lower_openai_tool_call(&call).expect("valid call parses");
        assert_eq!(spec.name, "web_search");
        assert_eq!(spec.arguments_json, "{\"query\":\"mesh\"}");
        assert_eq!(spec.provider_call_id.as_deref(), Some("call_abc123"));
    }

    #[test]
    fn openai_lower_tool_call_rejects_invalid_arguments_json() {
        use formats::openai::lower_openai_tool_call;
        use formats::ToolCallParseError;
        let call = serde_json::json!({
            "function": {
                "name": "x",
                "arguments": "not valid json {"
            }
        });
        match lower_openai_tool_call(&call) {
            Err(ToolCallParseError::InvalidArgumentsJson(_)) => {}
            other => panic!("expected InvalidArgumentsJson, got {other:?}"),
        }
    }

    #[test]
    fn anthropic_lower_tool_use_serializes_input_object() {
        use formats::anthropic::lower_anthropic_tool_use;
        let block = serde_json::json!({
            "type": "tool_use",
            "id": "toolu_xyz",
            "name": "web_search",
            "input": { "query": "mesh", "max_results": 5 }
        });
        let spec = lower_anthropic_tool_use(&block).expect("valid block parses");
        assert_eq!(spec.name, "web_search");
        // Re-parse to verify shape — the exact key ordering in
        // serde_json output isn't guaranteed, so don't byte-compare.
        let parsed: serde_json::Value =
            serde_json::from_str(&spec.arguments_json).expect("arguments round-trip JSON");
        assert_eq!(parsed["query"], "mesh");
        assert_eq!(parsed["max_results"], 5);
        assert_eq!(spec.provider_call_id.as_deref(), Some("toolu_xyz"));
    }

    #[test]
    fn mcp_lower_tools_call_threads_arguments_through() {
        use formats::mcp::lower_mcp_tools_call;
        let params = serde_json::json!({
            "name": "web_search",
            "arguments": { "query": "mesh" }
        });
        let spec = lower_mcp_tools_call(&params).expect("valid params parse");
        assert_eq!(spec.name, "web_search");
        let parsed: serde_json::Value =
            serde_json::from_str(&spec.arguments_json).expect("arguments round-trip JSON");
        assert_eq!(parsed["query"], "mesh");
        // MCP request params don't carry a call_id at this layer.
        assert!(spec.provider_call_id.is_none());
    }

    #[test]
    fn gemini_lower_function_call_handles_args_field() {
        use formats::gemini::lower_gemini_function_call;
        let call = serde_json::json!({
            "name": "web_search",
            "args": { "query": "mesh" }
        });
        let spec = lower_gemini_function_call(&call).expect("valid call parses");
        assert_eq!(spec.name, "web_search");
        let parsed: serde_json::Value =
            serde_json::from_str(&spec.arguments_json).expect("arguments round-trip JSON");
        assert_eq!(parsed["query"], "mesh");
        assert!(spec.provider_call_id.is_none(), "Gemini has no call_id");
    }

    #[test]
    fn formats_handle_missing_input_schema_with_empty_object() {
        // Build a descriptor with a None input schema (manual
        // construction since `metadata_for` always derives one).
        let desc = ToolDescriptor {
            tool_id: "no_schema_tool".into(),
            name: "no_schema_tool".into(),
            version: "1.0.0".into(),
            description: Some("Bare tool.".into()),
            input_schema: None,
            output_schema: None,
            requires: Vec::new(),
            estimated_time_ms: 0,
            stateless: true,
            streaming: false,
            tags: Vec::new(),
            pricing_terms: None,
            node_count: 0,
        };
        // Empty-object fallback prevents provider validators from
        // rejecting a null schema.
        let openai = formats::openai::to_openai_tool(&desc);
        assert_eq!(openai["function"]["parameters"]["type"], "object");
        assert_eq!(openai["function"]["strict"], false);
        let anthropic = formats::anthropic::to_anthropic_tool(&desc);
        assert_eq!(anthropic["input_schema"]["type"], "object");
        let mcp = formats::mcp::to_mcp_tool(&desc);
        assert_eq!(mcp["inputSchema"]["type"], "object");
        let gemini = formats::gemini::to_gemini_function_declaration(&desc);
        assert_eq!(gemini["parameters"]["type"], "object");
    }
}
