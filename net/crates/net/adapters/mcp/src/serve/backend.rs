//! The [`CapabilityGateway`] seam and the DTOs it returns.
//!
//! The shim's protocol layer never touches the mesh directly — it calls a
//! `CapabilityGateway`. This keeps the whole MCP-server surface (initialize,
//! meta-tools, consent, validation) testable in-process against an in-memory
//! gateway, and lets the real daemon-attached implementation land separately
//! without reshaping the shim. It is also the doctrine boundary: the shim is
//! a thin daemon client, and *only* the gateway knows how the daemon is
//! reached (Phase 2, doctrine #4).
//!
//! DTOs here are deliberately plain — id, name, schema, credential status —
//! rather than raw `net_sdk` types, so the shim depends on this narrow shape
//! and the gateway impl maps the daemon's capability index / RPC surface into
//! it. Whether a capability *requires approval* is **not** carried here: that
//! is shim state (the [`super::consent`] policy), decided per response.

use async_trait::async_trait;

use crate::spec::CallToolResult;

// The capability-identity types graduated to the SDK (`net_sdk::consent`,
// `MCP_BRIDGE_SDK_PLAN.md` P0) — one implementation shared by the shim, the
// CLI, and every binding, so identity (and thus the consent / pin-store key)
// can never fork. Re-exported here so `net_mcp::serve::CapabilityId` and the
// existing `serve::backend` paths keep working. Routing
// ([`parse_node`](crate::serve::mesh_gateway)) accepts the same node-id
// spellings the SDK canonicalizes, so identity and routing cannot disagree.
pub use net_sdk::consent::{CapabilityId, CapabilityIdError};

/// A search-result row: enough to let the model decide whether to describe or
/// invoke a capability, without the full schema. `requires_approval` is added
/// by the shim from its consent policy, not carried here.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilitySummary {
    /// Canonical id — the *primary* provider's id when this row is a collapsed
    /// group of interchangeable providers (Phase 4). Invoke fails over across
    /// the other `providers`.
    pub id: CapabilityId,
    /// Human-facing name (the descriptor's `name`).
    pub name: String,
    /// Short description, if the provider gave one.
    pub description: Option<String>,
    /// Compat tier — `mcp_bridge` for wrapped tools, richer for native caps.
    pub compat_tier: String,
    /// Credential status wire form (`credentialed` / `external_api` /
    /// `unknown` / `none`); drives the consent gate.
    pub credential_status: String,
    /// Every provider node id backing this logical capability, sorted. Length 1
    /// for a provider-local capability; more when equivalent providers were
    /// collapsed into one group.
    pub providers: Vec<u64>,
}

/// The full describe result: schema + risk/credential status + provider info
/// (Phase 2 `net_describe_capability`).
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityDetail {
    /// Canonical id.
    pub id: CapabilityId,
    /// Human-facing name.
    pub name: String,
    /// Description, if any.
    pub description: Option<String>,
    /// The tool's input JSON Schema (an object; `{}` if none was advertised).
    pub input_schema: serde_json::Value,
    /// The tool's output JSON Schema, if advertised.
    pub output_schema: Option<serde_json::Value>,
    /// Compat tier.
    pub compat_tier: String,
    /// Credential status wire form.
    pub credential_status: String,
    /// Substitutability (`provider_local` / `provider_equivalent`).
    pub substitutability: String,
    /// Provider-declared version, if any.
    pub version: String,
    /// `net.pricing.terms@1` canonical JSON when the capability is paid.
    /// Opaque to the gateway (payment semantics live in `net-payments`);
    /// `None` = free. The gate fails a paid capability closed unless a
    /// payment flow is configured and clears it.
    pub pricing_terms: Option<String>,
}

/// Errors a gateway operation can return. The variants map to the plan's
/// exact failure strings at the shim boundary (see [`super`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    /// No capability matched the id (describe / invoke).
    #[error("no capability found for `{0}`")]
    NotFound(String),
    /// The remote wrapper rejected the caller — its owner-scope gate fired.
    /// The message is the wrapper's structured rejection reason.
    #[error("{0}")]
    Denied(String),
    /// No daemon is reachable. Surfaced to the host as [`super::MSG_NO_DAEMON`].
    #[error("no Net daemon is running")]
    NoDaemon,
    /// A transport / routing failure reaching the daemon or provider.
    #[error("transport error: {0}")]
    Transport(String),
    /// Anything else, carried verbatim.
    #[error("{0}")]
    Other(String),
}

/// Whether re-executing an invoke is harmless — the retry policy for
/// [`CapabilityGateway::invoke`]. A typed flag rather than a bare `bool`, so a
/// call site or implementation can't silently invert, hard-code, or cargo-cult
/// the meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvokeSafety {
    /// Duplicate execution is harmless — an uncredentialed, stateless tool. A
    /// timed-out call MAY be retried on the same provider, covering the
    /// reply-channel first-reply race for ultra-fast handlers.
    DuplicateSafe,
    /// The invoke is **at-most-once** — a credentialed / stateful tool. A
    /// timeout (which does not prove the call didn't execute) is surfaced
    /// rather than retried, so a side effect — a created issue, a charge — is
    /// never silently duplicated.
    AtMostOnce,
}

impl InvokeSafety {
    /// Derive the retry safety from a capability's wire-declared credential
    /// status: only an uncredentialed (`none`) tool is duplicate-safe. This is a
    /// resilience hint from the provider, NOT an authorization decision — the
    /// consent gate never trusts a wire status, and mislabelling only risks a
    /// provider duplicating a call to its own tool.
    pub fn from_credential_status(status: &str) -> Self {
        if status == "none" {
            Self::DuplicateSafe
        } else {
            Self::AtMostOnce
        }
    }

    /// Whether a timed-out call may be retried on the same provider.
    pub fn allows_timeout_retry(self) -> bool {
        matches!(self, Self::DuplicateSafe)
    }
}

/// The single mesh-facing seam the shim depends on. Search + describe are
/// reads against the daemon's capability index; invoke routes an nRPC
/// `tools/call` to the provider and returns the [`CallToolResult`].
///
/// Implementations are the doctrine boundary — a real one attaches to the
/// running daemon (thin client, no embedded node); the tests use an in-memory
/// one. The shim treats every impl identically.
#[async_trait]
pub trait CapabilityGateway: Send + Sync {
    /// Find capabilities matching `query` (v0: substring over id / name /
    /// description). An empty result is `Ok(vec![])`, not an error — the shim
    /// turns that into the "no capabilities" guidance.
    async fn search(&self, query: &str) -> Result<Vec<CapabilitySummary>, GatewayError>;

    /// Full detail for one capability, including its input schema.
    async fn describe(&self, id: &CapabilityId) -> Result<CapabilityDetail, GatewayError>;

    /// Invoke the capability with pre-validated, consent-cleared `arguments`
    /// and return the tool result. A remote owner-scope rejection is
    /// [`GatewayError::Denied`]; a tool-level failure is an `Ok`
    /// [`CallToolResult`] with `is_error = true`.
    ///
    /// `safety` selects the retry policy on a timeout — see [`InvokeSafety`].
    async fn invoke(
        &self,
        id: &CapabilityId,
        arguments: serde_json::Value,
        safety: InvokeSafety,
    ) -> Result<CallToolResult, GatewayError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // The `CapabilityId` parse / canonicalization tests moved to
    // `net_sdk::consent` with the type (MCP_BRIDGE_SDK_PLAN.md P0).

    #[test]
    fn invoke_safety_derives_from_credential_status() {
        // Only an uncredentialed tool is duplicate-safe (may retry a timeout).
        assert_eq!(
            InvokeSafety::from_credential_status("none"),
            InvokeSafety::DuplicateSafe,
        );
        assert!(InvokeSafety::from_credential_status("none").allows_timeout_retry());
        // Everything else — including a garbled / unknown status — is
        // at-most-once, so a lost reply never duplicates a side effect.
        for status in ["credentialed", "external_api", "unknown", "", "bogus"] {
            assert_eq!(
                InvokeSafety::from_credential_status(status),
                InvokeSafety::AtMostOnce,
                "{status:?} must be at-most-once",
            );
            assert!(!InvokeSafety::from_credential_status(status).allows_timeout_retry());
        }
    }
}
