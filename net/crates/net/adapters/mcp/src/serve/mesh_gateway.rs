//! The real [`CapabilityGateway`]: a thin client over a joined [`Mesh`]
//! (`MCP_BRIDGE_PLAN.md` Phase 2, demand side).
//!
//! Discovery and invocation both route over the mesh:
//! - **search** — find bridge providers by the [`BRIDGE_PROVIDER_TAG`] in the
//!   capability fold, then fetch each one's catalog via the describe service
//!   ([`bridge::DESCRIBE_SERVICE`](crate::bridge::DESCRIBE_SERVICE)) and filter
//!   by the query.
//! - **describe** — fetch one tool's full descriptor from its provider.
//! - **invoke** — `Mesh::call` the tool's nRPC service on its provider; decode
//!   the `CallToolResult`.
//!
//! A [`CapabilityId`]'s `provider` is the provider's node id (v0 is
//! node-namespaced; aliases are a Phase 4 display concern and never enter ids).
//!
//! **The reply-channel race.** A cross-node `Mesh::call` to a freshly-served
//! handler can lose its first reply if the handler answers before the caller's
//! per-caller reply subscription has propagated (it surfaces as a timeout /
//! no-route). Every call here is therefore bounded and retried a few times.
//! Owner-scope denials and other application errors are **not** retried — they
//! are deterministic answers, not transient failures.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::{self, StreamExt};
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::{CallOptions, RpcError};
use serde_json::Value;
use tokio::sync::Mutex;

use super::backend::{
    CapabilityDetail, CapabilityGateway, CapabilityId, CapabilitySummary, GatewayError,
    InvokeSafety,
};
use super::grouping::{
    descriptor_fingerprint, group_capabilities, is_collapsible, CapabilityGroup,
};
use crate::bridge::{
    BridgedToolInfo, DescribeRequest, DescribeResponse, BRIDGE_PROVIDER_TAG, DESCRIBE_SERVICE,
};
use crate::spec::CallToolResult;
use crate::wrap::invoke::{ERR_BAD_REQUEST, ERR_OWNER_SCOPE, ERR_TOOL, ERR_UPSTREAM};

/// How many times a bounded call is retried before giving up (covers the
/// reply-channel first-reply race).
const MAX_ATTEMPTS: usize = 4;
/// Per-attempt deadline for a describe (catalog fetch) — an idempotent read
/// that should return promptly; a lost reply fails fast so the retry lands.
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Default per-attempt deadline for an invoke. A bridged MCP tool can
/// legitimately run far longer than a describe (a web fetch, image generation,
/// a long shell command), so the invoke deadline is generous and overridable
/// ([`MeshGateway::with_invoke_timeout`]) — not the 5s that made any slower
/// tool time out on every attempt and never return success.
const DEFAULT_INVOKE_TIMEOUT: Duration = Duration::from_secs(120);
/// Backoff between attempts.
const RETRY_BACKOFF: Duration = Duration::from_millis(120);
/// Max provider catalogs fetched concurrently during `search` — bounds the
/// fan-out so a large mesh doesn't open one describe call per provider at once.
const MAX_CONCURRENT_FETCHES: usize = 8;

/// A [`CapabilityGateway`] backed by a joined mesh node.
pub struct MeshGateway {
    mesh: Arc<Mesh>,
    /// `(capability, provider node) -> descriptor fingerprint`, learned as
    /// providers are described (search + describe). Failover consults it so it
    /// only ever routes to a provider whose contract matches the *primary's*
    /// — the same equivalence `group_capabilities` proved at search time — even
    /// after the primary is down and can't be re-described.
    fingerprints: Mutex<HashMap<(String, u64), u64>>,
    /// Per-attempt deadline for an invoke (default [`DEFAULT_INVOKE_TIMEOUT`]).
    invoke_timeout: Duration,
    /// Whether to collapse equivalent providers into one logical capability and
    /// fail invoke/describe over between them (Phase 4). **Off by default**:
    /// equivalence is proven only from wire-declared attributes a peer controls,
    /// so on a multi-identity mesh a hostile co-tenant could forge a matching
    /// fingerprint and become a group's representative or a failover target —
    /// receiving the operator's arguments. The operator opts in only when the
    /// mesh's peers are trustworthy-equivalent (their own nodes).
    trust_equivalent_providers: bool,
}

impl MeshGateway {
    /// Build a gateway over an already-joined `mesh`.
    pub fn new(mesh: Arc<Mesh>) -> Self {
        Self {
            mesh,
            fingerprints: Mutex::new(HashMap::new()),
            invoke_timeout: DEFAULT_INVOKE_TIMEOUT,
            trust_equivalent_providers: false,
        }
    }

    /// Override the per-attempt invoke deadline (default 120s). Describe keeps a
    /// short fixed deadline — only tool invocation can legitimately run long, so
    /// a host wrapping slow tools can widen just this.
    pub fn with_invoke_timeout(mut self, timeout: Duration) -> Self {
        self.invoke_timeout = timeout;
        self
    }

    /// Opt in to cross-provider collapse + failover (Phase 4). Default off.
    ///
    /// Enable ONLY when every peer on the mesh is trusted to be a genuine
    /// equivalent of the operator's own providers — because equivalence is
    /// decided from forgeable wire attributes (`substitutability`,
    /// `credential_status`, schema), not a verified shared owner identity (that
    /// verification is a later refinement). With it off, each provider's
    /// capability is discovered, pinned, and invoked on its own node id, so a
    /// peer can never silently stand in for another.
    pub fn trust_equivalent_providers(mut self, trust: bool) -> Self {
        self.trust_equivalent_providers = trust;
        self
    }

    /// Record a provider's descriptor fingerprint for later failover matching.
    async fn remember_fingerprint(&self, capability: &str, node: u64, info: &BridgedToolInfo) {
        self.fingerprints
            .lock()
            .await
            .insert((capability.to_string(), node), descriptor_fingerprint(info));
    }

    /// The last-known fingerprint of `capability` on `node`, if we've described
    /// it before (via search or a successful describe).
    async fn known_fingerprint(&self, capability: &str, node: u64) -> Option<u64> {
        self.fingerprints
            .lock()
            .await
            .get(&(capability.to_string(), node))
            .copied()
    }

    /// One bounded `Mesh::call` with a per-attempt `timeout`. An outer timeout
    /// maps to [`RpcError::Timeout`] so the retry logic treats a hung/lost call
    /// uniformly.
    async fn call_once(
        &self,
        node: u64,
        service: &str,
        body: Bytes,
        timeout: Duration,
    ) -> Result<Bytes, RpcError> {
        match tokio::time::timeout(
            timeout,
            self.mesh.call(node, service, body, CallOptions::default()),
        )
        .await
        {
            Ok(Ok(reply)) => Ok(reply.body),
            Ok(Err(e)) => Err(e),
            Err(_elapsed) => Err(RpcError::Timeout {
                elapsed_ms: timeout.as_millis() as u64,
            }),
        }
    }

    /// Call with a per-attempt `timeout`, retrying while `retriable` says the
    /// error is worth another attempt. Application errors
    /// ([`RpcError::ServerError`]) always return immediately — they are the
    /// answer. `retriable` differs by call idempotency: a describe (idempotent)
    /// retries any transient error; a non-idempotent invoke only retries errors
    /// that prove the call never executed (see [`retriable_send_safe`]).
    async fn call_retry(
        &self,
        node: u64,
        service: &str,
        body: Bytes,
        timeout: Duration,
        retriable: fn(&RpcError) -> bool,
    ) -> Result<Bytes, RpcError> {
        let mut last: Option<RpcError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            match self.call_once(node, service, body.clone(), timeout).await {
                Ok(bytes) => return Ok(bytes),
                Err(e) if retriable(&e) => {
                    last = Some(e);
                    if attempt + 1 < MAX_ATTEMPTS {
                        tokio::time::sleep(RETRY_BACKOFF).await;
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or(RpcError::Timeout { elapsed_ms: 0 }))
    }

    /// Fetch a provider's catalog (optionally filtered to one tool).
    async fn fetch_catalog(
        &self,
        node: u64,
        req: &DescribeRequest,
    ) -> Result<DescribeResponse, GatewayError> {
        let body = Bytes::from(
            serde_json::to_vec(req)
                .map_err(|e| GatewayError::Other(format!("encode describe request: {e}")))?,
        );
        match self
            .call_retry(node, DESCRIBE_SERVICE, body, DESCRIBE_TIMEOUT, is_retriable)
            .await
        {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| GatewayError::Other(format!("decode describe response: {e}"))),
            Err(e) => Err(map_describe_error(e, node)),
        }
    }

    /// Describe `id`'s capability on one specific provider node.
    async fn describe_on(
        &self,
        node: u64,
        id: &CapabilityId,
    ) -> Result<CapabilityDetail, GatewayError> {
        let catalog = self
            .fetch_catalog(
                node,
                &DescribeRequest {
                    tool_id: Some(id.capability.clone()),
                },
            )
            .await?;
        let info = catalog
            .tools
            .into_iter()
            .find(|t| t.tool_id == id.capability)
            .ok_or_else(|| GatewayError::NotFound(id.display()))?;
        // Learn this provider's contract for later failover matching.
        self.remember_fingerprint(&id.capability, node, &info).await;
        Ok(detail_from(id, info))
    }

    /// Reachable providers PROVABLY interchangeable with `id`'s primary,
    /// excluding `primary` — the failover candidates. A candidate qualifies only
    /// if it is collapsible (the operator's `provider_equivalent` assertion) AND
    /// its contract fingerprint matches the primary's — the same equivalence
    /// `group_capabilities` requires at search time, so failover never routes to
    /// a same-name / different-contract provider. Sorted node order, so
    /// `describe` and `invoke` fail over to the same one.
    ///
    /// The primary itself needs no separate collapsibility check: the
    /// fingerprint folds in `substitutability` and `credential_status` (see
    /// [`descriptor_fingerprint`]), so a credentialed or provider-local primary
    /// can never share a fingerprint with a collapsible candidate — the match on
    /// the next line rejects it by construction.
    ///
    /// **Fail safe:** without the primary's known fingerprint (never searched or
    /// described while it was up) no candidate can be proven equivalent, so we
    /// do not fail over — better a transport error than sending
    /// primary-validated arguments to an unverified provider.
    async fn equivalent_providers(
        &self,
        id: &CapabilityId,
        primary: u64,
    ) -> Vec<(u64, BridgedToolInfo)> {
        // Failover across providers is the same opt-in as collapse: without it,
        // an invoke stays on its declared provider and never routes the caller's
        // arguments to a peer that merely advertised a matching wire contract.
        if !self.trust_equivalent_providers {
            return Vec::new();
        }
        let Some(target_fp) = self.known_fingerprint(&id.capability, primary).await else {
            return Vec::new();
        };
        let mut nodes: Vec<u64> = self
            .mesh
            .find_nodes(&CapabilityFilter::new().require_tag(&id.capability))
            .into_iter()
            .filter(|&n| n != primary)
            .collect();
        nodes.sort_unstable();
        let mut out = Vec::new();
        for node in nodes {
            if let Ok(catalog) = self
                .fetch_catalog(
                    node,
                    &DescribeRequest {
                        tool_id: Some(id.capability.clone()),
                    },
                )
                .await
            {
                if let Some(info) = catalog
                    .tools
                    .into_iter()
                    .find(|t| t.tool_id == id.capability)
                {
                    if is_collapsible(&info) && descriptor_fingerprint(&info) == target_fp {
                        out.push((node, info));
                    }
                }
            }
        }
        out
    }

    /// Invoke `capability` on one provider node, mapping the nRPC result:
    /// success / a tool-level error / a malformed-request or upstream error are
    /// all `Ok(CallToolResult)` (the provider answered); an owner-scope
    /// rejection is `Err(Denied)`; an unreachable provider is `Err(Transport)`
    /// — the only error the caller fails over on.
    async fn invoke_on(
        &self,
        node: u64,
        capability: &str,
        body: Bytes,
        retriable: fn(&RpcError) -> bool,
    ) -> Result<CallToolResult, GatewayError> {
        match self
            .call_retry(node, capability, body, self.invoke_timeout, retriable)
            .await
        {
            // Success: the wrap handler encoded the CallToolResult as the body.
            Ok(bytes) => serde_json::from_slice::<CallToolResult>(&bytes)
                .map_err(|e| GatewayError::Other(format!("decode tool result: {e}"))),
            // Owner-scope rejection at the provider — the confused-deputy defense.
            Err(RpcError::ServerError { status, message }) if status == ERR_OWNER_SCOPE => {
                Err(GatewayError::Denied(message))
            }
            // The tool ran and reported a tool-level error: the wrap handler put
            // the full CallToolResult JSON in the message. Recover it so the
            // model sees the structured error rather than a transport failure.
            Err(RpcError::ServerError { status, message }) if status == ERR_TOOL => {
                Ok(serde_json::from_str::<CallToolResult>(&message)
                    .unwrap_or_else(|_| CallToolResult::text_error(message)))
            }
            // The bridge couldn't reach the tool, or the request was malformed
            // (should not happen — we pre-validate). Surface in-band.
            Err(RpcError::ServerError { status, message })
                if status == ERR_UPSTREAM || status == ERR_BAD_REQUEST =>
            {
                Ok(CallToolResult::text_error(message))
            }
            Err(RpcError::ServerError { status, message }) => Ok(CallToolResult::text_error(
                format!("provider error {status:#06x}: {message}"),
            )),
            Err(e) => Err(GatewayError::Transport(e.to_string())),
        }
    }
}

#[async_trait]
impl CapabilityGateway for MeshGateway {
    async fn search(&self, query: &str) -> Result<Vec<CapabilitySummary>, GatewayError> {
        let providers = self
            .mesh
            .find_nodes(&CapabilityFilter::new().require_tag(BRIDGE_PROVIDER_TAG));
        let q = query.to_lowercase();

        // Fetch provider catalogs concurrently with bounded fan-out, so search
        // completes in ~the slowest provider's time rather than the sum — a
        // single slow/unreachable provider (which can burn the full retry
        // budget) no longer blocks the others. `buffered` (not
        // `buffer_unordered`) preserves the deterministic `find_nodes` order, so
        // results are stable across runs regardless of which provider answers
        // first.
        let catalogs: Vec<(u64, Result<DescribeResponse, GatewayError>)> = stream::iter(providers)
            .map(|node| async move {
                (
                    node,
                    self.fetch_catalog(node, &DescribeRequest::default()).await,
                )
            })
            .buffered(MAX_CONCURRENT_FETCHES)
            .collect()
            .await;

        // Flatten every reachable provider's catalog into (provider, tool)
        // pairs. Any per-provider failure makes that provider invisible — never
        // fail the whole search. Concretely: `Denied` (out of owner scope),
        // `Transport` (unreachable, or serving no describe service — a `NoRoute`
        // maps here), or `Other` (a catalog we couldn't decode). One bad or
        // hostile provider must not abort global discovery.
        let mut discovered: Vec<(u64, BridgedToolInfo)> = Vec::new();
        for (node, result) in catalogs {
            let Ok(catalog) = result else { continue };
            for t in catalog.tools {
                discovered.push((node, t));
            }
        }

        // Remember each provider's contract fingerprint so a later invoke can
        // fail over only within the same proven-equivalent group.
        for (node, info) in &discovered {
            self.remember_fingerprint(&info.tool_id, *node, info).await;
        }

        // Collapse interchangeable providers into one logical capability each
        // (Phase 4, opt-in — see `trust_equivalent_providers`), then filter by
        // the query — matched against every provider's text in the group, not
        // just the primary's.
        let out = group_capabilities(discovered, self.trust_equivalent_providers)
            .into_iter()
            .filter(|g| q.is_empty() || g.matches_query(&q))
            .map(group_summary)
            .collect();
        Ok(out)
    }

    async fn describe(&self, id: &CapabilityId) -> Result<CapabilityDetail, GatewayError> {
        let primary = parse_node(&id.provider)?;
        // Try the primary provider; if it's unreachable, describe an equivalent
        // provider instead (same contract) — so a pinned/known capability keeps
        // describing after its primary goes down.
        match self.describe_on(primary, id).await {
            Err(GatewayError::Transport(reason)) => {
                match self
                    .equivalent_providers(id, primary)
                    .await
                    .into_iter()
                    .next()
                {
                    Some((_, info)) => Ok(detail_from(id, info)),
                    None => Err(GatewayError::Transport(format!(
                        "no reachable provider for `{}` ({reason})",
                        id.display()
                    ))),
                }
            }
            other => other,
        }
    }

    /// Invoke, with transparent failover to an equivalent provider when the
    /// primary is unreachable.
    ///
    /// **Duplicate-execution safety.** A timeout does not prove the primary
    /// didn't execute the call, so re-running it — via the same-node retry in
    /// `call_retry` *or* failover to another provider — can execute a tool more
    /// than once. Both are gated on [`InvokeSafety`]:
    ///
    /// - **Same-node retry** uses [`retriable_send_safe`] for
    ///   [`InvokeSafety::AtMostOnce`], so a credentialed / stateful tool is
    ///   never re-sent on a mere timeout; a [`InvokeSafety::DuplicateSafe`] tool
    ///   retries transient timeouts, recovering from the reply-channel
    ///   first-reply race.
    /// - **Failover** happens ONLY between providers `group_capabilities`
    ///   collapsed, which requires the operator's `provider_equivalent` opt-in
    ///   AND `credential_status == none` — the same uncredentialed class that is
    ///   duplicate-safe. A credentialed tool never collapses, so it has no
    ///   failover candidates and stays on its single provider.
    ///
    /// Net: for a credentialed tool the whole path is at-most-once; for an
    /// uncredentialed one the operator has asserted duplicate execution is
    /// harmless (running on any provider yields the same result).
    async fn invoke(
        &self,
        id: &CapabilityId,
        arguments: Value,
        safety: InvokeSafety,
    ) -> Result<CallToolResult, GatewayError> {
        let primary = parse_node(&id.provider)?;
        let body = Bytes::from(
            serde_json::to_vec(&arguments)
                .map_err(|e| GatewayError::Other(format!("encode arguments: {e}")))?,
        );
        // A duplicate-safe call retries any transient error; an at-most-once
        // invoke only retries errors that prove non-execution.
        let retriable: fn(&RpcError) -> bool = if safety.allows_timeout_retry() {
            is_retriable
        } else {
            retriable_send_safe
        };
        match self
            .invoke_on(primary, &id.capability, body.clone(), retriable)
            .await
        {
            // Primary unreachable — fail over to an equivalent provider (see the
            // duplicate-execution note above; only uncredentialed, collapsible
            // tools ever have candidates here). Only a transport failure fails
            // over: a `Denied` (authorization) or a tool-level error is a real
            // answer another provider wouldn't change.
            Err(GatewayError::Transport(primary_reason)) => {
                for (node, _) in self.equivalent_providers(id, primary).await {
                    match self
                        .invoke_on(node, &id.capability, body.clone(), retriable)
                        .await
                    {
                        Err(GatewayError::Transport(_)) => continue, // this one is down too
                        answer => return answer,
                    }
                }
                Err(GatewayError::Transport(format!(
                    "all providers for `{}` are unreachable (primary: {primary_reason})",
                    id.display()
                )))
            }
            answer => answer,
        }
    }
}

/// True for errors worth retrying an **idempotent** call (a describe, or an
/// uncredentialed invoke whose duplicate execution is harmless): the
/// reply-channel first-reply race and transient routing all surface as one of
/// these, and re-running the call has no ill effect.
fn is_retriable(e: &RpcError) -> bool {
    matches!(
        e,
        RpcError::NoRoute { .. } | RpcError::Timeout { .. } | RpcError::Transport(_)
    )
}

/// True only for errors that prove a **non-idempotent** call (an invoke of a
/// credentialed / stateful tool) never reached a handler, so retrying cannot
/// execute the tool twice. A `NoRoute` means the router found no handler — the
/// request was never delivered. A `Timeout` or generic `Transport` failure is
/// ambiguous (the handler may have run and only the reply was lost to the
/// first-reply race), so it is NOT retried: better an at-most-once error the
/// caller can act on than a duplicated side effect (a second issue / charge).
fn retriable_send_safe(e: &RpcError) -> bool {
    matches!(e, RpcError::NoRoute { .. })
}

/// Map a describe-call error to a gateway error.
fn map_describe_error(e: RpcError, node: u64) -> GatewayError {
    match e {
        RpcError::ServerError { status, message } if status == ERR_OWNER_SCOPE => {
            GatewayError::Denied(message)
        }
        RpcError::NoRoute { .. } => GatewayError::Transport(format!(
            "node {node} does not serve the describe service (not a bridge provider, or withdrawn)"
        )),
        RpcError::Timeout { elapsed_ms } => GatewayError::Transport(format!(
            "describe on node {node} timed out after {elapsed_ms}ms"
        )),
        other => GatewayError::Transport(other.to_string()),
    }
}

/// Build a `CapabilityDetail` from a provider's descriptor, keeping the
/// caller's cap_id (the primary handle) even when a failover provider answered
/// — the providers are interchangeable, so the schema/status are equivalent.
fn detail_from(id: &CapabilityId, info: BridgedToolInfo) -> CapabilityDetail {
    CapabilityDetail {
        id: id.clone(),
        name: info.name,
        description: info.description,
        input_schema: info.input_schema,
        output_schema: info.output_schema,
        compat_tier: info.compat_tier,
        credential_status: info.credential_status,
        substitutability: info.substitutability,
        version: info.version,
    }
}

/// Build a search summary for a grouped logical capability. The id is the
/// primary provider's; `providers` lists all of them so invoke can fail over.
fn group_summary(group: CapabilityGroup) -> CapabilitySummary {
    CapabilitySummary {
        id: CapabilityId::new(group.primary().to_string(), group.capability.clone()),
        name: group.info.name,
        description: group.info.description,
        compat_tier: group.info.compat_tier,
        credential_status: group.info.credential_status,
        providers: group.providers,
    }
}

/// Parse a provider node-id string (decimal or `0x`-hex) back to a `u64`.
fn parse_node(provider: &str) -> Result<u64, GatewayError> {
    let s = provider.trim();
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse::<u64>(),
    };
    parsed.map_err(|_| GatewayError::NotFound(format!("provider `{provider}` is not a node id")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoke_deadline_is_more_generous_than_describe() {
        // F5: a bridged tool can legitimately run far longer than a catalog
        // fetch, so the invoke deadline must not be pinned to the short describe
        // one — the old single 5s CALL_TIMEOUT made any slower tool time out on
        // every attempt and never return success.
        assert!(
            DEFAULT_INVOKE_TIMEOUT > DESCRIBE_TIMEOUT,
            "invoke must allow longer than describe",
        );
        assert!(
            DEFAULT_INVOKE_TIMEOUT >= Duration::from_secs(60),
            "the invoke deadline should comfortably exceed slow-tool runtimes",
        );
    }

    #[test]
    fn parse_node_accepts_decimal_and_hex() {
        assert_eq!(parse_node("12345").unwrap(), 12345);
        assert_eq!(parse_node("0x2a").unwrap(), 42);
        assert_eq!(parse_node(" 7 ").unwrap(), 7);
        assert!(parse_node("not-a-node").is_err());
    }

    #[test]
    fn retriable_covers_transient_not_application_errors() {
        assert!(is_retriable(&RpcError::Timeout { elapsed_ms: 10 }));
        assert!(is_retriable(&RpcError::NoRoute {
            target: 1,
            reason: "x".into(),
        }));
        assert!(!is_retriable(&RpcError::ServerError {
            status: ERR_OWNER_SCOPE,
            message: "denied".into(),
        }));
    }

    #[test]
    fn send_safe_retriable_only_covers_proven_non_execution() {
        // F1: a non-idempotent invoke may retry ONLY on an error proving the
        // call never reached a handler — a NoRoute — so a credentialed side
        // effect is never duplicated. A Timeout or Transport failure is
        // ambiguous (the tool may have run, only the reply lost), so it is not
        // retried; the describe/idempotent predicate still retries a Timeout.
        assert!(retriable_send_safe(&RpcError::NoRoute {
            target: 1,
            reason: "x".into(),
        }));
        assert!(!retriable_send_safe(&RpcError::Timeout { elapsed_ms: 10 }));
        assert!(!retriable_send_safe(&RpcError::ServerError {
            status: ERR_OWNER_SCOPE,
            message: "denied".into(),
        }));
        assert!(
            is_retriable(&RpcError::Timeout { elapsed_ms: 10 }),
            "the idempotent predicate keeps retrying a timeout",
        );
    }

    #[test]
    fn describe_error_maps_owner_scope_to_denied() {
        let mapped = map_describe_error(
            RpcError::ServerError {
                status: ERR_OWNER_SCOPE,
                message: "caller root identity does not match owner scope".into(),
            },
            9,
        );
        assert!(matches!(mapped, GatewayError::Denied(_)));
        // A no-route is a transport-level "not a provider", not a denial.
        assert!(matches!(
            map_describe_error(
                RpcError::NoRoute {
                    target: 9,
                    reason: "unknown".into()
                },
                9
            ),
            GatewayError::Transport(_)
        ));
    }

    #[test]
    fn group_summary_uses_primary_node_and_lists_providers() {
        let info = BridgedToolInfo {
            tool_id: "echo".into(),
            name: "Echo".into(),
            description: None,
            input_schema: serde_json::json!({}),
            output_schema: None,
            version: "1".into(),
            compat_tier: "mcp_bridge".into(),
            credential_status: "none".into(),
            substitutability: "provider_equivalent".into(),
            visibility: "owner_only".into(),
            invocation_scope: "same_root_identity".into(),
        };
        // Build the group through the real path so the primary/provider list
        // come from grouping, not a hand-rolled struct. Collapse enabled: this
        // exercises the two-provider representative selection.
        let group = group_capabilities(vec![(99, info.clone()), (42, info)], true)
            .into_iter()
            .next()
            .expect("one collapsed group");
        let s = group_summary(group);
        assert_eq!(
            s.id.display(),
            "42/echo",
            "id uses the primary (lowest) node"
        );
        assert_eq!(s.providers, vec![42, 99]);
    }
}
