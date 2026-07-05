//! The stdio MCP **server** loop (`MCP_BRIDGE_PLAN.md` Phase 2, `serve/shim.rs`).
//!
//! [`Shim`] reads newline-delimited JSON-RPC from the host on one side and
//! answers `initialize` / `tools/list` / `tools/call` on the other. Its
//! `tools/list` is the meta-tool surface ([`super::meta_tools`]); its
//! `tools/call` dispatches each `net_*` meta-tool through the
//! [`CapabilityGateway`], applying pre-flight [`validation`] and the
//! [`consent`](super::consent) gate on the invoke path.
//!
//! **Serial by design (v0).** The shim processes one request at a time — a
//! slow invoke blocks the next request. MCP hosts send one request and await,
//! so this is correct and simple; request pipelining is a later refinement.
//!
//! The shim holds no mesh identity or socket — everything mesh-facing is the
//! gateway's job (doctrine #4). It is transport-generic over any
//! `AsyncBufRead` / `AsyncWrite`, so tests drive it over an in-memory pipe and
//! the CLI drives it over real stdio.

use std::path::PathBuf;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use super::backend::{CapabilityGateway, CapabilityId, GatewayError};
use super::consent::ConsentPolicy;
use super::gated::{gated_invoke, GatedOutcome};
use super::pins::PinStore;
use super::{meta_tools, requires_approval_message, MSG_DENIED_BY_WRAPPER, MSG_NO_CAPABILITIES};
use crate::spec::{
    method, CallToolParams, CallToolResult, Implementation, IncomingKind, IncomingMessage,
    JsonRpcErrorResponse, JsonRpcNotification, JsonRpcSuccess, RequestId, INVALID_PARAMS,
    INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, PROTOCOL_VERSION, SERVER_VERSION,
};

/// How often the serve loop polls the pin store to detect an out-of-band
/// approval and emit `tools/list_changed`. Interactive-paced; overridable in
/// tests via [`Shim::with_pin_poll_interval`].
const DEFAULT_PIN_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Max approved pins described concurrently when building the promoted-tool
/// list, so a `tools/list` bounds its latency to the slowest single describe
/// rather than the sum (mirrors the gateway's own search fan-out).
const MAX_CONCURRENT_PIN_DESCRIBES: usize = 8;

/// The demand-side shim: a stdio MCP server exposing the mesh's capabilities
/// as meta-tools, backed by a [`CapabilityGateway`].
pub struct Shim<G> {
    gateway: G,
    consent: ConsentPolicy,
    server_info: Implementation,
    /// Path to the persistent, machine-shared pin store. When set, an approved
    /// pin there satisfies consent, `net_request_pin` records a pending request
    /// there, and it is reloaded per call so an out-of-band `net mcp pin
    /// approve` is seen immediately. `None` in tests keeps consent in-memory.
    pin_store_path: Option<PathBuf>,
    /// How often to poll the pin store for changes (emitting list_changed).
    pin_poll_interval: Duration,
    /// Set once the host sends `notifications/initialized`. Tracked for
    /// completeness; the stateless shim answers regardless.
    initialized: bool,
}

impl<G: CapabilityGateway> Shim<G> {
    /// Build a shim over `gateway` with an empty consent policy.
    pub fn new(gateway: G) -> Self {
        Self {
            gateway,
            consent: ConsentPolicy::new(),
            server_info: Implementation {
                name: "net".to_string(),
                version: SERVER_VERSION.to_string(),
            },
            pin_store_path: None,
            pin_poll_interval: DEFAULT_PIN_POLL_INTERVAL,
            initialized: false,
        }
    }

    /// Install a consent policy (config allowlist + any pins).
    pub fn with_consent(mut self, consent: ConsentPolicy) -> Self {
        self.consent = consent;
        self
    }

    /// Back consent with the persistent pin store at `path` (Phase 3): approved
    /// pins there admit an otherwise-gated capability, and `net_request_pin`
    /// records pending requests there.
    pub fn with_pin_store(mut self, path: PathBuf) -> Self {
        self.pin_store_path = Some(path);
        self
    }

    /// Override the pin-store poll interval (how quickly an out-of-band
    /// approval triggers a `tools/list_changed`).
    pub fn with_pin_poll_interval(mut self, interval: Duration) -> Self {
        self.pin_poll_interval = interval;
        self
    }

    /// A stable signature of the currently-approved pin set (sorted display
    /// ids). Changing it is what triggers a `tools/list_changed` — so only an
    /// approval/removal that changes the *promoted* tools notifies the host,
    /// not a mere pending request.
    async fn approved_signature(&self) -> Vec<String> {
        match self.load_pins().await {
            Some(store) => {
                let mut ids: Vec<String> = store.approved().iter().map(|id| id.display()).collect();
                ids.sort();
                ids
            }
            None => Vec::new(),
        }
    }

    /// Load the pin store, if one is configured. A read/parse error yields
    /// `None` — a broken store must never *grant* consent (fail closed).
    async fn load_pins(&self) -> Option<PinStore> {
        match &self.pin_store_path {
            Some(path) => PinStore::load(path).await.ok(),
            None => None,
        }
    }

    /// Does invoking `id` (with `status`) still require approval, given the
    /// static consent policy and an already-loaded pin store snapshot? An
    /// approved store pin admits it; otherwise the consent rule stands.
    fn gated_after_pins(&self, id: &CapabilityId, status: &str, pins: &Option<PinStore>) -> bool {
        self.consent.requires_approval(id, status)
            && !pins.as_ref().map(|p| p.is_approved(id)).unwrap_or(false)
    }

    /// Run the server loop until the host closes stdin (EOF). Each non-empty
    /// input line is handled; requests produce exactly one response line,
    /// notifications produce none. Concurrently — via one `select!`, so no
    /// extra task or shared writer — the loop polls the pin store and emits
    /// `tools/list_changed` when an out-of-band `net mcp pin approve` changes
    /// the promoted tool set, so the host refreshes without re-listing on a
    /// timer of its own.
    pub async fn serve<R, W>(mut self, reader: R, mut writer: W) -> std::io::Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut lines = reader.lines();
        // Only watch when a pin store is configured; otherwise the tick branch
        // is a never-ready future and the loop is a pure read loop.
        let mut watch = self
            .pin_store_path
            .as_ref()
            .map(|_| tokio::time::interval(self.pin_poll_interval));
        let mut last_sig = self.approved_signature().await;

        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { break }; // EOF
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Some(response) = self.handle_line(&line).await {
                        write_line(&mut writer, &response).await?;
                    }
                }
                _ = tick(&mut watch) => {
                    let sig = self.approved_signature().await;
                    if sig != last_sig {
                        last_sig = sig;
                        let note = JsonRpcNotification::new(method::TOOLS_LIST_CHANGED, None);
                        write_line(&mut writer, &line_of(&note)).await?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Handle one raw line. Returns the response line to write, or `None` for
    /// notifications / stray responses that need no reply.
    async fn handle_line(&mut self, line: &str) -> Option<String> {
        let msg: IncomingMessage = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => {
                return Some(error_line(
                    None,
                    PARSE_ERROR,
                    format!("parse error: invalid JSON ({e})"),
                ));
            }
        };

        match msg.kind() {
            IncomingKind::Notification => {
                self.handle_notification(&msg);
                None
            }
            // The compat-tier shim never sends server→client requests, so any
            // response the host sends is unexpected — ignore it silently.
            IncomingKind::Response => None,
            IncomingKind::Malformed => Some(error_line(
                msg.id.clone(),
                INVALID_REQUEST,
                "invalid request: missing `method`",
            )),
            IncomingKind::Request => match (msg.method.clone(), msg.id.clone()) {
                (Some(m), Some(id)) => Some(self.handle_request(id, &m, msg.params).await),
                // Unreachable given `kind() == Request`, but no panic.
                _ => None,
            },
        }
    }

    fn handle_notification(&mut self, msg: &IncomingMessage) {
        if msg.method.as_deref() == Some(method::INITIALIZED) {
            self.initialized = true;
        }
        // `notifications/cancelled`, `tools/list_changed`, … — nothing to do.
    }

    async fn handle_request(
        &mut self,
        id: RequestId,
        method: &str,
        params: Option<Value>,
    ) -> String {
        match method {
            method::INITIALIZE => {
                // The client sends `notifications/initialized` next; be lenient
                // and answer regardless of handshake ordering.
                let result = json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    // We serve tools/list and emit list_changed when the pin set
                    // changes (a pinned capability is promoted to a first-class
                    // tool), so advertise the capability.
                    "capabilities": { "tools": { "listChanged": true } },
                    "serverInfo": to_value_or_null(&self.server_info),
                    "instructions": "Net mesh capability bridge. Use net_search_capabilities \
                                     to find tools on the mesh, net_describe_capability for a \
                                     tool's schema, then net_invoke_capability to run it. \
                                     Approved-pinned capabilities also appear as first-class \
                                     tools by their own name.",
                });
                success_line(id, result)
            }
            method::TOOLS_LIST => {
                // Meta-tools first, then every approved-pinned capability as a
                // first-class typed tool (its own name + real schema).
                let mut tools = meta_tools::meta_tools();
                tools.extend(self.promoted_pinned_tools().await);
                success_line(id, json!({ "tools": to_value_or_null(&tools) }))
            }
            method::TOOLS_CALL => self.handle_tools_call(id, params).await,
            other => error_line(
                Some(id),
                METHOD_NOT_FOUND,
                format!("method not found: {other}"),
            ),
        }
    }

    /// `tools/call` always resolves to a JSON-RPC **success** carrying a
    /// [`CallToolResult`] — a bad tool name or a failed call is reported
    /// in-band via `is_error`, so the model sees the message. Only malformed
    /// `params` (not a valid `tools/call` shape) is a protocol-level error.
    async fn handle_tools_call(&self, id: RequestId, params: Option<Value>) -> String {
        let params = match params {
            Some(p) => p,
            None => return error_line(Some(id), INVALID_PARAMS, "tools/call requires params"),
        };
        let call: CallToolParams = match serde_json::from_value(params) {
            Ok(c) => c,
            Err(e) => {
                return error_line(
                    Some(id),
                    INVALID_PARAMS,
                    format!("invalid tools/call params: {e}"),
                )
            }
        };

        let result = self.dispatch_tool(&call.name, &call.arguments).await;
        success_line(id, to_value_or_null(&result))
    }

    /// Route a `tools/call` to its handler: a meta-tool, an approved-pinned
    /// capability invoked directly by its first-class name, or an in-band
    /// unknown-tool error.
    async fn dispatch_tool(&self, name: &str, args: &Value) -> CallToolResult {
        match name {
            n if n == meta_tools::name::SEARCH => self.tool_search(args).await,
            n if n == meta_tools::name::DESCRIBE => self.tool_describe(args).await,
            n if n == meta_tools::name::INVOKE => self.tool_invoke(args).await,
            n if n == meta_tools::name::LIST_PINNED => self.tool_list_pinned().await,
            n if n == meta_tools::name::REQUEST_PIN => self.tool_request_pin(args).await,
            other => match self.resolve_pinned_tool(other).await {
                // A promoted pinned tool — the arguments are the capability's
                // own arguments (no meta-tool wrapper). Consent is already
                // satisfied by the approved pin.
                Some(id) => self.invoke_capability(&id, args.clone()).await,
                None => CallToolResult::text_error(format!(
                    "unknown tool `{other}`. This server exposes the net_* meta-tools \
                     ({}, {}, {}, {}, {}) plus any approved-pinned capabilities by name. \
                     Use net_search_capabilities to find mesh tools.",
                    meta_tools::name::SEARCH,
                    meta_tools::name::DESCRIBE,
                    meta_tools::name::INVOKE,
                    meta_tools::name::LIST_PINNED,
                    meta_tools::name::REQUEST_PIN,
                )),
            },
        }
    }

    /// The approved-pinned capabilities, each as a first-class typed tool named
    /// by its host-safe name with its real input schema. A pin whose provider
    /// is currently unreachable is skipped rather than listed without a schema.
    ///
    /// The describes run with **bounded concurrency**, not serially: a pin whose
    /// provider is down can burn the gateway's full retry budget, and doing them
    /// one at a time would block the single-threaded serve loop (including the
    /// pin-store poll) for the *sum* of those timeouts on every `tools/list`.
    /// `buffered` (not `buffer_unordered`) preserves the deterministic
    /// [`assign_pinned_tool_names`] order.
    async fn promoted_pinned_tools(&self) -> Vec<crate::spec::Tool> {
        let Some(store) = self.load_pins().await else {
            return Vec::new();
        };
        stream::iter(assign_pinned_tool_names(&store.approved()))
            .map(|(id, name)| async move {
                let detail = self.gateway.describe(&id).await.ok()?;
                Some(crate::spec::Tool {
                    name,
                    title: Some(id.display()),
                    description: detail
                        .description
                        .or_else(|| Some(format!("Pinned capability {}", id.display()))),
                    input_schema: detail.input_schema,
                    output_schema: detail.output_schema,
                })
            })
            .buffered(MAX_CONCURRENT_PIN_DESCRIBES)
            .filter_map(|tool| async move { tool })
            .collect()
            .await
    }

    /// Resolve a called first-class tool name to the approved-pinned capability
    /// it stands for, if any. Uses the same deterministic assignment as
    /// `promoted_pinned_tools` (recomputed from the store each call), so a
    /// disambiguated name resolves to exactly the capability it was listed for.
    async fn resolve_pinned_tool(&self, name: &str) -> Option<CapabilityId> {
        let store = self.load_pins().await?;
        assign_pinned_tool_names(&store.approved())
            .into_iter()
            .find(|(_, n)| n == name)
            .map(|(id, _)| id)
    }

    async fn tool_search(&self, args: &Value) -> CallToolResult {
        let query = match str_arg(args, "query") {
            Ok(q) => q,
            Err(e) => return CallToolResult::text_error(e),
        };
        let summaries = match self.gateway.search(&query).await {
            Ok(s) => s,
            Err(e) => return CallToolResult::text_error(gateway_error_text(&e)),
        };
        if summaries.is_empty() {
            return CallToolResult::text_ok(MSG_NO_CAPABILITIES);
        }
        let pins = self.load_pins().await;
        let rows: Vec<Value> = summaries
            .iter()
            .map(|s| {
                json!({
                    "cap_id": s.id.display(),
                    "name": s.name,
                    "description": s.description,
                    "compat_tier": s.compat_tier,
                    "credential_status": s.credential_status,
                    // Provider node ids backing this capability; >1 when
                    // equivalent providers were collapsed (invoke fails over).
                    "providers": s.providers,
                    "requires_approval": self.gated_after_pins(&s.id, &s.credential_status, &pins),
                })
            })
            .collect();
        json_result(json!({ "capabilities": rows }))
    }

    async fn tool_describe(&self, args: &Value) -> CallToolResult {
        let id = match parse_cap_id_arg(args) {
            Ok(id) => id,
            Err(e) => return CallToolResult::text_error(e),
        };
        let detail = match self.gateway.describe(&id).await {
            Ok(d) => d,
            Err(e) => return CallToolResult::text_error(gateway_error_text(&e)),
        };
        let pins = self.load_pins().await;
        json_result(json!({
            "cap_id": detail.id.display(),
            "name": detail.name,
            "description": detail.description,
            "input_schema": detail.input_schema,
            "output_schema": detail.output_schema,
            "compat_tier": detail.compat_tier,
            "credential_status": detail.credential_status,
            "substitutability": detail.substitutability,
            "version": detail.version,
            "requires_approval": self.gated_after_pins(
                &detail.id,
                &detail.credential_status,
                &pins,
            ),
        }))
    }

    async fn tool_invoke(&self, args: &Value) -> CallToolResult {
        let id = match parse_cap_id_arg(args) {
            Ok(id) => id,
            Err(e) => return CallToolResult::text_error(e),
        };
        // The nested tool arguments; absent means "no arguments".
        let tool_args = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
        self.invoke_capability(&id, tool_args).await
    }

    /// Invoke `id` with `tool_args` (the capability's own arguments): describe
    /// → pre-flight validate → consent gate → route to the provider. Shared by
    /// the `net_invoke_capability` meta-tool and the first-class pinned-tool
    /// dispatch.
    ///
    /// The gate itself lives in [`gated_invoke`] (one implementation, shared
    /// with the native SDK gateway); this method only reloads the pin store per
    /// call — so an out-of-band `net mcp pin approve` takes effect immediately —
    /// and flattens the structured [`GatedOutcome`] to the shim's product
    /// failure strings.
    async fn invoke_capability(&self, id: &CapabilityId, tool_args: Value) -> CallToolResult {
        let pins = self.load_pins().await;
        match gated_invoke(&self.gateway, &self.consent, pins.as_ref(), id, tool_args).await {
            GatedOutcome::Invoked(result) => result,
            GatedOutcome::ValidationFailed(reason) => CallToolResult::text_error(format!(
                "argument validation failed: {reason}. See net_describe_capability for the schema.",
            )),
            GatedOutcome::RequiresApproval => {
                CallToolResult::text_error(requires_approval_message(&id.display()))
            }
            GatedOutcome::Failed(GatewayError::Denied(reason)) => {
                CallToolResult::text_error(denied_message(&reason))
            }
            GatedOutcome::Failed(e) => CallToolResult::text_error(gateway_error_text(&e)),
        }
    }

    async fn tool_list_pinned(&self) -> CallToolResult {
        // Approved pins come from the persistent store (shared across shims)
        // plus any in-memory pins (tests / static allowlist-as-pin).
        let mut pinned: std::collections::BTreeSet<String> =
            self.consent.pinned().map(|id| id.display()).collect();
        if let Some(store) = self.load_pins().await {
            for id in store.approved() {
                pinned.insert(id.display());
            }
        }
        json_result(json!({ "pinned": pinned.into_iter().collect::<Vec<_>>() }))
    }

    async fn tool_request_pin(&self, args: &Value) -> CallToolResult {
        let id = match parse_cap_id_arg(args) {
            Ok(id) => id,
            Err(e) => return CallToolResult::text_error(e),
        };
        // Record a *pending* request in the shared store — never an approval.
        // Moving pending → approved happens only through `net mcp pin approve`,
        // outside the model loop (the plan's rule: the model must not approve
        // its own future access). Done under the store lock so it can't clobber
        // a concurrent operator approve/reject.
        if let Some(path) = &self.pin_store_path {
            if let Err(e) = PinStore::mutate(path.clone(), |store| store.request(&id)).await {
                return CallToolResult::text_error(format!(
                    "could not record the pin request: {e}"
                ));
            }
        }
        CallToolResult::text_ok(format!(
            "Pin requested for `{}`. A human must approve it out of band before it \
             becomes usable: run `net mcp pin approve {}`. Requesting a pin grants \
             no access by itself.",
            id.display(),
            id.display(),
        ))
    }
}

// --- free helpers ----------------------------------------------------------

/// Await the next pin-store poll tick, or never (a pure read loop) when no
/// store is configured.
async fn tick(watch: &mut Option<tokio::time::Interval>) {
    match watch {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Write one line (+ newline) and flush.
async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

/// Serialize to a JSON line, falling back to a canned internal-error line if
/// serialization somehow fails (it does not for these types) — never panics.
fn line_of<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal serialization error"}}"#
            .to_string()
    })
}

/// A JSON-RPC success response line.
fn success_line(id: RequestId, result: Value) -> String {
    line_of(&JsonRpcSuccess::new(id, result))
}

/// A JSON-RPC error response line.
fn error_line(id: Option<RequestId>, code: i64, message: impl Into<String>) -> String {
    line_of(&JsonRpcErrorResponse::new(id, code, message))
}

/// Serialize a value to JSON, falling back to `null` (never panics). Used for
/// the `result` payloads, which are structurally guaranteed to serialize.
fn to_value_or_null<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Wrap a JSON data object as a successful `CallToolResult`: a pretty-printed
/// text block (so hosts without structured support still see everything) plus
/// the machine-readable `structured_content`.
fn json_result(data: Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string());
    CallToolResult {
        content: vec![json!({ "type": "text", "text": text })],
        is_error: false,
        structured_content: Some(data),
    }
}

/// Extract a required string argument from a meta-tool's arguments object.
fn str_arg(args: &Value, field: &str) -> Result<String, String> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing or non-string argument `{field}`"))
}

/// Extract and parse the `cap_id` argument into a [`CapabilityId`].
fn parse_cap_id_arg(args: &Value) -> Result<CapabilityId, String> {
    let raw = str_arg(args, "cap_id")?;
    CapabilityId::parse(&raw).map_err(|e| e.to_string())
}

/// The meta-tool names a promoted pinned tool must never take — otherwise it
/// would be unreachable, since `dispatch_tool` routes a meta-tool name to the
/// meta-tool branch before it ever consults the pinned tools.
const RESERVED_TOOL_NAMES: [&str; 5] = [
    meta_tools::name::SEARCH,
    meta_tools::name::DESCRIBE,
    meta_tools::name::INVOKE,
    meta_tools::name::LIST_PINNED,
    meta_tools::name::REQUEST_PIN,
];

/// Assign a unique, host-safe first-class tool name to each approved-pinned
/// capability — as a **pure function of the capability id**, independent of
/// which *other* pins happen to be approved.
///
/// Making the name id-local (rather than giving the first claimant the bare
/// base name and suffixing later collisions) is what keeps `tools/list` and a
/// later `tools/call` in agreement across an out-of-band
/// `net mcp pin approve/reject`: a name the host cached from an earlier list can
/// never be *reassigned* to a different capability when the approved set changes
/// (F9). A stale cached name resolves either to the same capability or to
/// nothing (a safe "unknown tool"), never to another one with the caller's
/// arguments. `approved` is still deterministically ordered
/// ([`PinStore::approved`](super::pins::PinStore::approved), sorted by id), but
/// the assignment no longer depends on that order.
fn assign_pinned_tool_names(approved: &[CapabilityId]) -> Vec<(CapabilityId, String)> {
    approved
        .iter()
        .map(|id| (id.clone(), stable_pinned_tool_name(id)))
        .collect()
}

/// The host-safe first-class tool name for one pinned capability — a pure
/// function of its id (see [`assign_pinned_tool_names`]).
///
/// The lossy [`safe_tool_name`] base (character replacement + truncation) is
/// *always* suffixed with a deterministic hash of the full id, so the name
/// depends only on `id` and is independent of the approved set. The suffix is
/// `_<6 hex>` ([`with_hash_suffix`]) — a shape no [`RESERVED_TOOL_NAMES`] entry
/// has (they are plain meta-tool identifiers) — so a promoted pin can never
/// shadow a meta-tool, and no reserved-name avoidance loop is needed. The
/// `debug_assert` pins that invariant against a future change to either the
/// suffix format or the reserved set.
///
/// A residual hash collision between two distinct ids sharing a base is
/// astronomically unlikely; if it ever happened they would share a name — no
/// worse than the pre-existing lossy-base collision, and still not a
/// cross-capability *remap*.
fn stable_pinned_tool_name(id: &CapabilityId) -> String {
    let base = safe_tool_name(id);
    let name = with_hash_suffix(&base, id, 0);
    debug_assert!(
        !RESERVED_TOOL_NAMES.contains(&name.as_str()),
        "a hash-suffixed pinned name must never equal a reserved meta-tool name",
    );
    name
}

/// `base` (trimmed to fit) plus `_<6 hex>`, a deterministic hash of the id and
/// `salt`, within the 64-char host-name limit.
fn with_hash_suffix(base: &str, id: &CapabilityId, salt: u32) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.display().hash(&mut hasher);
    salt.hash(&mut hasher);
    let suffix = format!("{:06x}", hasher.finish() & 0x00ff_ffff);
    let max_base = 64usize.saturating_sub(suffix.len() + 1);
    let mut trimmed = base.to_string();
    trimmed.truncate(max_base);
    format!("{trimmed}_{suffix}")
}

/// A host-charset-safe base name for a pinned capability: `[a-zA-Z0-9_-]`,
/// ≤ 64 chars (the Phase 4 model-facing naming rule), derived deterministically
/// from the id's display form. Lossy — see [`assign_pinned_tool_names`], which
/// resolves the resulting collisions.
fn safe_tool_name(id: &CapabilityId) -> String {
    let mut name: String = id
        .display()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    name.truncate(64);
    name
}

/// Map a gateway error to the model-facing message, using the plan's exact
/// strings where they apply.
fn gateway_error_text(e: &GatewayError) -> String {
    match e {
        GatewayError::NoDaemon => super::MSG_NO_DAEMON.to_string(),
        GatewayError::Denied(reason) => denied_message(reason),
        GatewayError::NotFound(id) => format!("no capability found for `{id}`"),
        GatewayError::Transport(m) => format!("transport error reaching the mesh: {m}"),
        GatewayError::Other(m) => m.clone(),
    }
}

/// The wrapper-denied message. When the reason is the canonical owner-scope
/// rejection this reproduces [`MSG_DENIED_BY_WRAPPER`] verbatim; otherwise it
/// carries the wrapper's specific reason.
fn denied_message(reason: &str) -> String {
    let reason = reason.trim().trim_end_matches('.');
    if reason == crate::wrap::invoke::OWNER_SCOPE_REJECTION {
        return MSG_DENIED_BY_WRAPPER.to_string();
    }
    format!("Denied by remote wrapper: {reason}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::backend::{CapabilityDetail, CapabilitySummary, InvokeSafety};
    use crate::serve::consent::ConsentPolicy;
    use crate::serve::pins::PinStore;
    use async_trait::async_trait;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncWriteExt, BufReader};

    // --- in-memory gateway ------------------------------------------------

    #[derive(Clone)]
    struct InMemoryGateway {
        caps: Vec<CapabilityDetail>,
        deny: HashSet<String>,
        invoke_calls: Arc<AtomicUsize>,
    }

    impl InMemoryGateway {
        fn new(caps: Vec<CapabilityDetail>) -> Self {
            Self {
                caps,
                deny: HashSet::new(),
                invoke_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
        fn deny(mut self, id: &str) -> Self {
            self.deny.insert(id.to_string());
            self
        }
        fn find(&self, id: &CapabilityId) -> Option<&CapabilityDetail> {
            self.caps.iter().find(|c| &c.id == id)
        }
    }

    #[async_trait]
    impl CapabilityGateway for InMemoryGateway {
        async fn search(&self, query: &str) -> Result<Vec<CapabilitySummary>, GatewayError> {
            let q = query.to_lowercase();
            Ok(self
                .caps
                .iter()
                .filter(|c| {
                    c.id.display().to_lowercase().contains(&q)
                        || c.name.to_lowercase().contains(&q)
                        || c.description
                            .as_deref()
                            .map(|d| d.to_lowercase().contains(&q))
                            .unwrap_or(false)
                })
                .map(|c| CapabilitySummary {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                    compat_tier: c.compat_tier.clone(),
                    credential_status: c.credential_status.clone(),
                    // The in-memory mock doesn't model provider grouping; the
                    // real MeshGateway populates this and the live test checks it.
                    providers: Vec::new(),
                })
                .collect())
        }

        async fn describe(&self, id: &CapabilityId) -> Result<CapabilityDetail, GatewayError> {
            self.find(id)
                .cloned()
                .ok_or_else(|| GatewayError::NotFound(id.display()))
        }

        async fn invoke(
            &self,
            id: &CapabilityId,
            arguments: Value,
            _safety: InvokeSafety,
        ) -> Result<CallToolResult, GatewayError> {
            self.invoke_calls.fetch_add(1, Ordering::SeqCst);
            if self.find(id).is_none() {
                return Err(GatewayError::NotFound(id.display()));
            }
            if self.deny.contains(&id.display()) {
                return Err(GatewayError::Denied(
                    crate::wrap::invoke::OWNER_SCOPE_REJECTION.to_string(),
                ));
            }
            Ok(CallToolResult::text_ok(format!(
                "invoked {} with {}",
                id.display(),
                arguments
            )))
        }
    }

    fn detail(id: &str, cred: &str, schema: Value) -> CapabilityDetail {
        let cap = CapabilityId::parse(id).unwrap();
        CapabilityDetail {
            id: cap,
            name: format!("{} tool", id),
            description: Some(format!("does {id}")),
            input_schema: schema,
            output_schema: None,
            compat_tier: "mcp_bridge".to_string(),
            credential_status: cred.to_string(),
            substitutability: "provider_local".to_string(),
            version: "1.0.0".to_string(),
        }
    }

    fn echo_schema() -> Value {
        json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"]
        })
    }

    // --- harness ----------------------------------------------------------

    /// Drive the shim over an in-memory duplex: feed `input` lines, collect
    /// every response line as parsed JSON.
    async fn run(gateway: InMemoryGateway, consent: ConsentPolicy, input: &[Value]) -> Vec<Value> {
        let lines: Vec<String> = input.iter().map(|v| v.to_string()).collect();
        run_raw(gateway, consent, &lines).await
    }

    /// Like [`run`] but backing the shim with a persistent pin store at `path`.
    async fn run_pinned(
        gateway: InMemoryGateway,
        consent: ConsentPolicy,
        path: PathBuf,
        input: &[Value],
    ) -> Vec<Value> {
        let lines: Vec<String> = input.iter().map(|v| v.to_string()).collect();
        run_inner(gateway, consent, Some(path), &lines).await
    }

    /// Like [`run`] but with raw (possibly non-JSON) input lines.
    async fn run_raw(
        gateway: InMemoryGateway,
        consent: ConsentPolicy,
        lines: &[String],
    ) -> Vec<Value> {
        run_inner(gateway, consent, None, lines).await
    }

    async fn run_inner(
        gateway: InMemoryGateway,
        consent: ConsentPolicy,
        pin_path: Option<PathBuf>,
        lines: &[String],
    ) -> Vec<Value> {
        let (client, server) = tokio::io::duplex(256 * 1024);
        let (server_rd, server_wr) = tokio::io::split(server);
        let mut shim = Shim::new(gateway).with_consent(consent);
        if let Some(path) = pin_path {
            shim = shim.with_pin_store(path);
        }
        let handle =
            tokio::spawn(async move { shim.serve(BufReader::new(server_rd), server_wr).await });

        let (client_rd, mut client_wr) = tokio::io::split(client);
        for line in lines {
            client_wr.write_all(line.as_bytes()).await.unwrap();
            client_wr.write_all(b"\n").await.unwrap();
        }
        // `shutdown` (not a bare drop) closes the write direction so the shim's
        // read loop sees EOF — a dropped split half leaves the duplex open.
        client_wr.shutdown().await.unwrap();
        drop(client_wr);

        let mut out = Vec::new();
        let mut resp = BufReader::new(client_rd).lines();
        while let Some(line) = resp.next_line().await.unwrap() {
            out.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        handle.await.unwrap().unwrap();
        out
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn call(id: i64, tool: &str, arguments: Value) -> Value {
        req(
            id,
            method::TOOLS_CALL,
            json!({ "name": tool, "arguments": arguments }),
        )
    }

    /// The `CallToolResult` inside a `tools/call` success response.
    fn tool_result(resp: &Value) -> CallToolResult {
        serde_json::from_value(resp["result"].clone()).unwrap()
    }

    fn gateway_with_echo_and_secret() -> InMemoryGateway {
        InMemoryGateway::new(vec![
            detail("nodeb/echo", "none", echo_schema()),
            detail("nodeb/secret", "credentialed", echo_schema()),
        ])
    }

    // --- tests ------------------------------------------------------------

    #[tokio::test]
    async fn initialize_advertises_protocol_and_lists_meta_tools() {
        let out = run(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &[
                req(1, method::INITIALIZE, json!({})),
                json!({ "jsonrpc": "2.0", "method": method::INITIALIZED }),
                req(2, method::TOOLS_LIST, json!({})),
            ],
        )
        .await;

        // The notification produces no response, so two responses for ids 1, 2.
        assert_eq!(out.len(), 2, "got {out:?}");
        assert_eq!(out[0]["id"], 1);
        assert_eq!(out[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(out[0]["result"]["serverInfo"]["name"], "net");

        assert_eq!(out[1]["id"], 2);
        let tools = out[1]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 5);
        assert!(tools.iter().any(|t| t["name"] == meta_tools::name::SEARCH));
    }

    #[tokio::test]
    async fn string_request_id_is_echoed_back() {
        let out = run_raw(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &[json!({ "jsonrpc": "2.0", "id": "abc", "method": method::TOOLS_LIST }).to_string()],
        )
        .await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], "abc", "string ids reflect back verbatim");
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let out = run(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &[req(9, "does/not/exist", json!({}))],
        )
        .await;
        assert_eq!(out[0]["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn malformed_json_is_parse_error_with_null_id() {
        let out = run_raw(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &["{ this is not json".to_string()],
        )
        .await;
        assert_eq!(out[0]["error"]["code"], PARSE_ERROR);
        assert_eq!(out[0]["id"], Value::Null);
    }

    #[tokio::test]
    async fn notification_produces_no_response() {
        let out = run(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &[json!({ "jsonrpc": "2.0", "method": method::INITIALIZED })],
        )
        .await;
        assert!(out.is_empty(), "notifications get no reply: {out:?}");
    }

    #[tokio::test]
    async fn search_returns_rows_with_requires_approval_flags() {
        // Every discovered capability is gated by default (a wire-declared
        // credential status is not trusted); an allowlisted one clears the flag.
        let mut consent = ConsentPolicy::new();
        consent.allow(CapabilityId::parse("nodeb/echo").unwrap());
        let out = run(
            gateway_with_echo_and_secret(),
            consent,
            &[call(1, meta_tools::name::SEARCH, json!({ "query": "" }))],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(!result.is_error);
        let caps = result.structured_content.unwrap()["capabilities"].clone();
        let caps = caps.as_array().unwrap();
        assert_eq!(caps.len(), 2);
        let echo = caps.iter().find(|c| c["cap_id"] == "nodeb/echo").unwrap();
        assert_eq!(
            echo["requires_approval"], false,
            "allowlisted ⇒ no approval"
        );
        let secret = caps.iter().find(|c| c["cap_id"] == "nodeb/secret").unwrap();
        assert_eq!(
            secret["requires_approval"], true,
            "unapproved ⇒ requires approval",
        );
    }

    #[tokio::test]
    async fn search_empty_returns_the_no_capabilities_message() {
        let out = run(
            InMemoryGateway::new(vec![]),
            ConsentPolicy::new(),
            &[call(
                1,
                meta_tools::name::SEARCH,
                json!({ "query": "anything" }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(!result.is_error);
        assert_eq!(result.text(), MSG_NO_CAPABILITIES);
    }

    #[tokio::test]
    async fn describe_returns_schema_and_status() {
        let out = run(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &[call(
                1,
                meta_tools::name::DESCRIBE,
                json!({ "cap_id": "nodeb/echo" }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        let data = result.structured_content.unwrap();
        assert_eq!(data["cap_id"], "nodeb/echo");
        assert_eq!(data["credential_status"], "none");
        assert_eq!(data["input_schema"]["type"], "object");
    }

    #[tokio::test]
    async fn invoke_allowlisted_capability_runs_the_tool() {
        // A discovered capability is gated regardless of its wire-declared
        // status; allowlisting it lets the invoke through to the provider.
        let mut consent = ConsentPolicy::new();
        consent.allow(CapabilityId::parse("nodeb/echo").unwrap());
        let out = run(
            gateway_with_echo_and_secret(),
            consent,
            &[call(
                1,
                meta_tools::name::INVOKE,
                json!({ "cap_id": "nodeb/echo", "arguments": { "message": "hi" } }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(!result.is_error, "{result:?}");
        assert!(result.text().contains("invoked nodeb/echo"));
    }

    #[tokio::test]
    async fn invoke_of_a_discovered_capability_is_gated_without_approval() {
        // The trust-boundary property at the shim level: a discovered
        // capability whose provider declares `none` is still gated — a wire
        // status can't grant free invocation.
        let gw = gateway_with_echo_and_secret();
        let calls = gw.invoke_calls.clone();
        let out = run(
            gw,
            ConsentPolicy::new(),
            &[call(
                1,
                meta_tools::name::INVOKE,
                json!({ "cap_id": "nodeb/echo", "arguments": { "message": "hi" } }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(
            result.is_error,
            "a self-declared `none` must not bypass consent"
        );
        assert!(result.text().contains("net mcp pin approve nodeb/echo"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "blocked before the provider"
        );
    }

    #[tokio::test]
    async fn invoke_credentialed_is_blocked_pending_approval() {
        let gw = gateway_with_echo_and_secret();
        let calls = gw.invoke_calls.clone();
        let out = run(
            gw,
            ConsentPolicy::new(),
            &[call(
                1,
                meta_tools::name::INVOKE,
                json!({ "cap_id": "nodeb/secret", "arguments": { "message": "x" } }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(result.is_error);
        assert!(
            result.text().contains("net mcp pin approve nodeb/secret"),
            "{}",
            result.text(),
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a blocked call must never reach the provider",
        );
    }

    #[tokio::test]
    async fn invoke_after_pin_reaches_the_provider() {
        let mut consent = ConsentPolicy::new();
        consent.pin(CapabilityId::parse("nodeb/secret").unwrap());
        let out = run(
            gateway_with_echo_and_secret(),
            consent,
            &[call(
                1,
                meta_tools::name::INVOKE,
                json!({ "cap_id": "nodeb/secret", "arguments": { "message": "x" } }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(!result.is_error, "pinned ⇒ invocable: {result:?}");
        assert!(result.text().contains("invoked nodeb/secret"));
    }

    #[tokio::test]
    async fn invoke_with_bad_args_fails_validation_before_provider() {
        let gw = gateway_with_echo_and_secret();
        let calls = gw.invoke_calls.clone();
        let out = run(
            gw,
            ConsentPolicy::new(),
            // `message` is required + must be a string; send a number.
            &[call(
                1,
                meta_tools::name::INVOKE,
                json!({ "cap_id": "nodeb/echo", "arguments": { "message": 42 } }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(result.is_error);
        assert!(
            result.text().contains("validation failed") && result.text().contains("message"),
            "{}",
            result.text(),
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "validation failure must not reach the provider",
        );
    }

    #[tokio::test]
    async fn invoke_denied_by_wrapper_surfaces_the_exact_message() {
        // An allowlisted cap (passes local consent) whose provider denies on
        // owner scope — the demand side reports the wrapper's rejection.
        let gw = InMemoryGateway::new(vec![detail("nodeb/echo", "none", echo_schema())])
            .deny("nodeb/echo");
        let mut consent = ConsentPolicy::new();
        consent.allow(CapabilityId::parse("nodeb/echo").unwrap());
        let out = run(
            gw,
            consent,
            &[call(
                1,
                meta_tools::name::INVOKE,
                json!({ "cap_id": "nodeb/echo", "arguments": { "message": "x" } }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(result.is_error);
        assert_eq!(result.text(), MSG_DENIED_BY_WRAPPER);
    }

    #[tokio::test]
    async fn request_pin_returns_out_of_band_instructions() {
        let out = run(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &[call(
                1,
                meta_tools::name::REQUEST_PIN,
                json!({ "cap_id": "nodeb/secret" }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(!result.is_error);
        assert!(result.text().contains("net mcp pin approve nodeb/secret"));
        assert!(result.text().contains("grants no access"));
    }

    #[tokio::test]
    async fn list_pinned_reflects_the_consent_policy() {
        let mut consent = ConsentPolicy::new();
        consent.pin(CapabilityId::parse("nodeb/secret").unwrap());
        let out = run(
            gateway_with_echo_and_secret(),
            consent,
            &[call(1, meta_tools::name::LIST_PINNED, json!({}))],
        )
        .await;
        let result = tool_result(&out[0]);
        let pinned = result.structured_content.unwrap()["pinned"].clone();
        assert_eq!(pinned, json!(["nodeb/secret"]));
    }

    #[tokio::test]
    async fn unknown_tool_name_is_an_in_band_error() {
        let out = run(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &[call(1, "net_not_a_tool", json!({}))],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(result.is_error);
        assert!(result.text().contains("unknown tool") && result.text().contains("meta-tools"));
    }

    #[tokio::test]
    async fn invoke_of_unknown_capability_reports_not_found() {
        let out = run(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &[call(
                1,
                meta_tools::name::INVOKE,
                json!({ "cap_id": "ghost/missing", "arguments": {} }),
            )],
        )
        .await;
        let result = tool_result(&out[0]);
        assert!(result.is_error);
        assert!(result.text().contains("no capability found"));
    }

    #[tokio::test]
    async fn several_requests_stream_in_order() {
        let out = run(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            &[
                req(1, method::INITIALIZE, json!({})),
                req(2, method::TOOLS_LIST, json!({})),
                call(3, meta_tools::name::SEARCH, json!({ "query": "echo" })),
            ],
        )
        .await;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["id"], 1);
        assert_eq!(out[1]["id"], 2);
        assert_eq!(out[2]["id"], 3);
    }

    // --- Phase 3: persistent pin store ------------------------------------

    fn pin_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pins.json");
        (dir, path)
    }

    #[tokio::test]
    async fn request_pin_records_a_pending_request_but_does_not_admit() {
        let (_dir, path) = pin_path();
        let gw = gateway_with_echo_and_secret();
        let calls = gw.invoke_calls.clone();
        let out = run_pinned(
            gw,
            ConsentPolicy::new(),
            path.clone(),
            &[
                // The model requests a pin...
                call(
                    1,
                    meta_tools::name::REQUEST_PIN,
                    json!({ "cap_id": "nodeb/secret" }),
                ),
                // ...then immediately tries to invoke — must still be blocked.
                call(
                    2,
                    meta_tools::name::INVOKE,
                    json!({ "cap_id": "nodeb/secret", "arguments": { "message": "x" } }),
                ),
            ],
        )
        .await;

        let requested = tool_result(&out[0]);
        assert!(!requested.is_error);
        assert!(requested
            .text()
            .contains("net mcp pin approve nodeb/secret"));

        let invoked = tool_result(&out[1]);
        assert!(invoked.is_error, "a mere request must not grant access");
        assert!(invoked.text().contains("net mcp pin approve nodeb/secret"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "blocked call never reached provider"
        );

        // The store holds a pending (not approved) record — the model cannot
        // approve its own access.
        let store = PinStore::load(&path).await.unwrap();
        assert_eq!(
            store.state(&CapabilityId::parse("nodeb/secret").unwrap()),
            Some(crate::serve::pins::PinState::Pending),
        );
        assert!(!store.is_approved(&CapabilityId::parse("nodeb/secret").unwrap()));
    }

    #[tokio::test]
    async fn an_out_of_band_approval_admits_the_capability() {
        let (_dir, path) = pin_path();
        // Simulate `net mcp pin approve nodeb/secret` writing the shared store.
        {
            let mut store = PinStore::load(&path).await.unwrap();
            store.approve(&CapabilityId::parse("nodeb/secret").unwrap());
            store.save().await.unwrap();
        }

        let out = run_pinned(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            path,
            &[
                // Search reflects the approval: no longer requires_approval.
                call(1, meta_tools::name::SEARCH, json!({ "query": "secret" })),
                // And invoke now reaches the provider.
                call(
                    2,
                    meta_tools::name::INVOKE,
                    json!({ "cap_id": "nodeb/secret", "arguments": { "message": "x" } }),
                ),
                // list_pinned reflects the store.
                call(3, meta_tools::name::LIST_PINNED, json!({})),
            ],
        )
        .await;

        let caps = tool_result(&out[0]).structured_content.unwrap()["capabilities"].clone();
        let secret = caps
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["cap_id"] == "nodeb/secret")
            .unwrap()
            .clone();
        assert_eq!(
            secret["requires_approval"], false,
            "an approved pin clears the approval flag",
        );

        let invoked = tool_result(&out[1]);
        assert!(!invoked.is_error, "approved pin ⇒ invocable: {invoked:?}");
        assert!(invoked.text().contains("invoked nodeb/secret"));

        let pinned = tool_result(&out[2]).structured_content.unwrap()["pinned"].clone();
        assert_eq!(pinned, json!(["nodeb/secret"]));
    }

    #[tokio::test]
    async fn an_approved_pin_is_promoted_to_a_first_class_tool() {
        let (_dir, path) = pin_path();
        {
            let mut store = PinStore::load(&path).await.unwrap();
            store.approve(&CapabilityId::parse("nodeb/secret").unwrap());
            store.save().await.unwrap();
        }
        // The first-class name is a pure function of the id (F9), so compute it
        // the same way the shim does rather than hard-coding a spelling.
        let secret_name = stable_pinned_tool_name(&CapabilityId::parse("nodeb/secret").unwrap());
        let out = run_pinned(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            path,
            &[
                req(1, method::TOOLS_LIST, json!({})),
                // Invoke it by its first-class name — arguments are the tool's
                // own, no meta-tool wrapper, and consent is satisfied by the pin.
                call(2, &secret_name, json!({ "message": "direct" })),
            ],
        )
        .await;

        let tools = out[0]["result"]["tools"].as_array().unwrap();
        // 5 meta-tools + the promoted pin.
        assert_eq!(tools.len(), 6, "{tools:?}");
        let promoted = tools
            .iter()
            .find(|t| t["name"].as_str() == Some(secret_name.as_str()))
            .expect("the pinned capability is a first-class tool");
        assert_eq!(promoted["inputSchema"]["type"], "object");
        assert_eq!(promoted["title"], "nodeb/secret");

        let invoked = tool_result(&out[1]);
        assert!(
            !invoked.is_error,
            "pinned first-class tool runs: {invoked:?}"
        );
        assert!(invoked.text().contains("invoked nodeb/secret"));
    }

    #[tokio::test]
    async fn multiple_pins_are_all_promoted() {
        // F4: promoted_pinned_tools describes pins with bounded concurrency;
        // every approved pin must still appear (none dropped by the fan-out).
        let (_dir, path) = pin_path();
        {
            let mut store = PinStore::load(&path).await.unwrap();
            store.approve(&CapabilityId::parse("nodeb/echo").unwrap());
            store.approve(&CapabilityId::parse("nodeb/secret").unwrap());
            store.save().await.unwrap();
        }
        let out = run_pinned(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            path,
            &[req(1, method::TOOLS_LIST, json!({}))],
        )
        .await;
        let tools = out[0]["result"]["tools"].as_array().unwrap();
        // 5 meta-tools + the 2 promoted pins.
        assert_eq!(tools.len(), 7, "{tools:?}");
        let titles: Vec<&str> = tools.iter().filter_map(|t| t["title"].as_str()).collect();
        assert!(titles.contains(&"nodeb/echo"), "echo promoted: {titles:?}");
        assert!(
            titles.contains(&"nodeb/secret"),
            "secret promoted: {titles:?}"
        );
    }

    #[tokio::test]
    async fn a_no_arg_pinned_tool_invoked_without_arguments_succeeds() {
        // Regression (F6): the host may omit `arguments` on a promoted pinned
        // tool, which deserializes to JSON null. That must behave like the same
        // capability invoked via net_invoke_capability (which defaults a missing
        // `arguments` to `{}`) — not fail pre-flight validation against the
        // tool's object schema.
        let (_dir, path) = pin_path();
        {
            let mut store = PinStore::load(&path).await.unwrap();
            store.approve(&CapabilityId::parse("nodeb/ping").unwrap());
            store.save().await.unwrap();
        }
        let gw = InMemoryGateway::new(vec![detail(
            "nodeb/ping",
            "none",
            json!({ "type": "object" }),
        )]);
        let ping_name = stable_pinned_tool_name(&CapabilityId::parse("nodeb/ping").unwrap());
        let out = run_pinned(
            gw,
            ConsentPolicy::new(),
            path,
            // A promoted pinned tool called with NO `arguments` field.
            &[req(1, method::TOOLS_CALL, json!({ "name": ping_name }))],
        )
        .await;
        let invoked = tool_result(&out[0]);
        assert!(!invoked.is_error, "no-arg pinned tool runs: {invoked:?}");
        assert!(invoked.text().contains("invoked nodeb/ping with {}"));
    }

    #[tokio::test]
    async fn an_unpinned_capability_has_no_first_class_tool() {
        let (_dir, path) = pin_path();
        // Empty store — nothing approved.
        let out = run_pinned(
            gateway_with_echo_and_secret(),
            ConsentPolicy::new(),
            path,
            &[
                req(1, method::TOOLS_LIST, json!({})),
                call(2, "nodeb_secret", json!({ "message": "x" })),
            ],
        )
        .await;
        let tools = out[0]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 5, "only meta-tools without an approved pin");
        let result = tool_result(&out[1]);
        assert!(result.is_error);
        assert!(result.text().contains("unknown tool"));
    }

    #[test]
    fn safe_tool_name_sanitizes_and_bounds() {
        let id = CapabilityId::parse("homelab/github.create_issue").unwrap();
        assert_eq!(safe_tool_name(&id), "homelab_github_create_issue");
        // Bounded to 64 chars.
        let long = CapabilityId::new("n", "a".repeat(200));
        assert!(safe_tool_name(&long).len() <= 64);
        // Only host-safe characters survive.
        assert!(safe_tool_name(&long)
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }

    #[test]
    fn assigned_pinned_names_are_unique_and_avoid_meta_tools() {
        // `a.b` and `a_b` both sanitize to the same base (a collision), and
        // `net/search_capabilities` sanitizes to a meta-tool name.
        let a = CapabilityId::new("nodeb", "a.b");
        let b = CapabilityId::new("nodeb", "a_b");
        assert_eq!(
            safe_tool_name(&a),
            safe_tool_name(&b),
            "precondition: same base"
        );
        let shadow = CapabilityId::new("net", "search_capabilities");
        assert!(
            meta_tools::is_meta_tool(&safe_tool_name(&shadow)),
            "precondition: sanitizes to a meta-tool name",
        );

        let approved = vec![a.clone(), b.clone(), shadow.clone()];
        let assigned = assign_pinned_tool_names(&approved);
        let names: Vec<String> = assigned.iter().map(|(_, n)| n.clone()).collect();

        // Every assigned name is unique...
        let unique: HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "names must be unique: {names:?}");
        // ...none shadows a meta-tool...
        for n in &names {
            assert!(!meta_tools::is_meta_tool(n), "{n} shadows a meta-tool");
            assert!(n.len() <= 64);
        }
        // ...and the assignment is deterministic (list ⇄ dispatch agree).
        let again: Vec<String> = assign_pinned_tool_names(&approved)
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        assert_eq!(names, again);
        // Each name resolves back to a distinct original id.
        let ids: HashSet<String> = assigned.iter().map(|(id, _)| id.display()).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn pinned_name_is_independent_of_the_approved_set() {
        // F9: a capability's first-class name is a pure function of its id, so
        // approving/rejecting OTHER pins out of band never remaps a name the
        // host cached from an earlier tools/list onto a different capability.
        let assigned_name = |set: &[CapabilityId], want: &CapabilityId| -> String {
            assign_pinned_tool_names(set)
                .into_iter()
                .find(|(id, _)| id == want)
                .map(|(_, n)| n)
                .expect("assigned")
        };

        let x = CapabilityId::parse("nodeb/deploy").unwrap();
        let y = CapabilityId::parse("nodeb/delete").unwrap();
        assert_eq!(
            assigned_name(&[x.clone()], &x),
            assigned_name(&[x.clone(), y.clone()], &x),
            "x's name must not depend on whether y is approved",
        );

        // Even two ids that share a lossy base each keep a stable name whether
        // or not the other is approved — and they never collide.
        let a = CapabilityId::new("nodeb", "a.b");
        let b = CapabilityId::new("nodeb", "a_b");
        assert_eq!(
            safe_tool_name(&a),
            safe_tool_name(&b),
            "precondition: same base"
        );
        assert_eq!(
            assigned_name(&[a.clone()], &a),
            assigned_name(&[a.clone(), b.clone()], &a),
            "a's name is stable regardless of b",
        );
        assert_ne!(
            assigned_name(&[a.clone(), b.clone()], &a),
            assigned_name(&[a.clone(), b.clone()], &b),
            "distinct ids still get distinct names",
        );
    }

    #[tokio::test]
    async fn an_out_of_band_approval_emits_tools_list_changed() {
        let (_dir, path) = pin_path();
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (server_rd, server_wr) = tokio::io::split(server);
        let shim = Shim::new(gateway_with_echo_and_secret())
            .with_pin_store(path.clone())
            .with_pin_poll_interval(Duration::from_millis(25));
        let handle =
            tokio::spawn(async move { shim.serve(BufReader::new(server_rd), server_wr).await });

        let (client_rd, mut client_wr) = tokio::io::split(client);
        let mut reader = BufReader::new(client_rd).lines();

        // Start the loop with an initialize, and consume its response.
        let init = req(1, method::INITIALIZE, json!({}));
        client_wr
            .write_all(format!("{init}\n").as_bytes())
            .await
            .unwrap();
        let resp: Value =
            serde_json::from_str(&reader.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(resp["id"], 1);

        // Approve a pin out of band (as `net mcp pin approve` would).
        {
            let mut store = PinStore::load(&path).await.unwrap();
            store.approve(&CapabilityId::parse("nodeb/secret").unwrap());
            store.save().await.unwrap();
        }

        // The shim should push a tools/list_changed notification. Bound the
        // wait so a regression fails instead of hanging.
        let note = tokio::time::timeout(Duration::from_secs(3), reader.next_line())
            .await
            .expect("list_changed arrives within the timeout")
            .unwrap()
            .unwrap();
        let nv: Value = serde_json::from_str(&note).unwrap();
        assert_eq!(nv["method"], method::TOOLS_LIST_CHANGED);
        assert!(nv.get("id").is_none(), "a notification carries no id");

        client_wr.shutdown().await.unwrap();
        drop(client_wr);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn no_list_changed_without_an_approval_change() {
        // A pending request (which the store records) must NOT emit
        // list_changed — only a change to the *approved* set does.
        let (_dir, path) = pin_path();
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (server_rd, server_wr) = tokio::io::split(server);
        let shim = Shim::new(gateway_with_echo_and_secret())
            .with_pin_store(path.clone())
            .with_pin_poll_interval(Duration::from_millis(20));
        let handle =
            tokio::spawn(async move { shim.serve(BufReader::new(server_rd), server_wr).await });

        let (client_rd, mut client_wr) = tokio::io::split(client);
        let mut reader = BufReader::new(client_rd).lines();

        // request_pin records a pending record (writes the file) but the
        // approved set is unchanged.
        let c = call(
            1,
            meta_tools::name::REQUEST_PIN,
            json!({ "cap_id": "nodeb/secret" }),
        );
        client_wr
            .write_all(format!("{c}\n").as_bytes())
            .await
            .unwrap();
        let resp: Value =
            serde_json::from_str(&reader.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(resp["id"], 1, "got the request_pin response");

        // Give the watcher several poll cycles; it must stay silent.
        let quiet = tokio::time::timeout(Duration::from_millis(150), reader.next_line()).await;
        assert!(
            quiet.is_err(),
            "a pending request must not emit list_changed: {quiet:?}",
        );

        client_wr.shutdown().await.unwrap();
        drop(client_wr);
        handle.await.unwrap().unwrap();
    }

    #[test]
    fn denied_message_canonicalizes_the_shared_owner_scope_rejection() {
        use crate::wrap::invoke::OWNER_SCOPE_REJECTION;
        // The canonical wrapper rejection maps to the exact product string.
        assert_eq!(denied_message(OWNER_SCOPE_REJECTION), MSG_DENIED_BY_WRAPPER);
        // Drift guard: the product string is literally the prefix + the shared
        // reason, so rewording OWNER_SCOPE_REJECTION without updating
        // MSG_DENIED_BY_WRAPPER fails here.
        assert!(MSG_DENIED_BY_WRAPPER.contains(OWNER_SCOPE_REJECTION));
        // A trailing period on the reason is normalised, not doubled.
        assert_eq!(
            denied_message(&format!("{OWNER_SCOPE_REJECTION}.")),
            MSG_DENIED_BY_WRAPPER,
        );
        // Any other reason is wrapped generically.
        assert_eq!(denied_message("nope"), "Denied by remote wrapper: nope.");
    }
}
