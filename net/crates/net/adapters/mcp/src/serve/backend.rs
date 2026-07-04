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

/// A capability's canonical identity: the provider node plus the capability
/// name. Structured, never a bare string (Phase 4: "canonical identity is
/// structured"). The display / wire form is `provider/capability` — `/`
/// qualifies the node, matching the plan's display convention
/// (`homelab/github.create_issue`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityId {
    /// The provider node qualifier (v0: node-namespaced). Never a mutable
    /// display alias — those never enter identifiers.
    pub provider: String,
    /// The capability / tool name (may itself contain `.` or `/`).
    pub capability: String,
}

/// Why a `provider/capability` string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityIdError {
    /// No `/` separating the provider from the capability.
    #[error("capability id `{0}` must be `provider/capability` (missing `/`)")]
    MissingProvider(String),
    /// The provider or capability half was empty.
    #[error("capability id `{0}` has an empty provider or capability")]
    Empty(String),
}

impl CapabilityId {
    /// Build from parts.
    pub fn new(provider: impl Into<String>, capability: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            capability: capability.into(),
        }
    }

    /// Parse the `provider/capability` display form. Splits on the **first**
    /// `/` — the provider (a node qualifier) never contains `/`, so the
    /// remainder is the capability even when the capability name itself has a
    /// `/` (e.g. `homelab/svc/sub` → provider `homelab`, capability `svc/sub`).
    pub fn parse(s: &str) -> Result<Self, CapabilityIdError> {
        let (provider, capability) = s
            .split_once('/')
            .ok_or_else(|| CapabilityIdError::MissingProvider(s.to_string()))?;
        if provider.is_empty() || capability.is_empty() {
            return Err(CapabilityIdError::Empty(s.to_string()));
        }
        Ok(Self::new(provider, capability))
    }

    /// The `provider/capability` display / wire form.
    pub fn display(&self) -> String {
        format!("{}/{}", self.provider, self.capability)
    }
}

impl std::fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.capability)
    }
}

/// A search-result row: enough to let the model decide whether to describe or
/// invoke a capability, without the full schema. `requires_approval` is added
/// by the shim from its consent policy, not carried here.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilitySummary {
    /// Canonical id.
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
    async fn invoke(
        &self,
        id: &CapabilityId,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, GatewayError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_and_capability_on_first_slash() {
        let id = CapabilityId::parse("homelab/github.create_issue").unwrap();
        assert_eq!(id.provider, "homelab");
        assert_eq!(id.capability, "github.create_issue");
        assert_eq!(id.display(), "homelab/github.create_issue");

        // Capability names may themselves contain `/` — only the first split
        // is the provider boundary.
        let nested = CapabilityId::parse("homelab/svc/sub").unwrap();
        assert_eq!(nested.provider, "homelab");
        assert_eq!(nested.capability, "svc/sub");
    }

    #[test]
    fn rejects_missing_or_empty_halves() {
        assert_eq!(
            CapabilityId::parse("bareword"),
            Err(CapabilityIdError::MissingProvider("bareword".to_string())),
        );
        assert_eq!(
            CapabilityId::parse("/cap"),
            Err(CapabilityIdError::Empty("/cap".to_string())),
        );
        assert_eq!(
            CapabilityId::parse("prov/"),
            Err(CapabilityIdError::Empty("prov/".to_string())),
        );
    }

    #[test]
    fn display_round_trips_through_parse() {
        let id = CapabilityId::new("node-b", "time.now");
        assert_eq!(CapabilityId::parse(&id.display()).unwrap(), id);
    }
}
