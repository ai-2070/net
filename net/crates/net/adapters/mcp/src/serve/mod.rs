//! Demand side — expose the mesh's capabilities to a local MCP host as a
//! stdio MCP **server** (`MCP_BRIDGE_PLAN.md` Phase 2, `net mcp serve`).
//!
//! This is the mirror of [`crate::wrap`]: where `wrap` is an MCP *client*
//! (spawns a server, speaks JSON-RPC to it), the shim here is an MCP
//! *server* — the host (Claude Code, Cursor, …) spawns `net mcp serve` and
//! speaks JSON-RPC to it over stdio. The shim answers with a small set of
//! **meta-tools** ([`meta_tools`]) that let the model search, describe, and
//! invoke capabilities discovered across the mesh.
//!
//! **Doctrine.** The shim is a *thin client* to the running `net` daemon —
//! never an embedded mesh node (doctrine #4, Phase 2). All mesh access goes
//! through a [`CapabilityGateway`], the single seam between the protocol
//! layer here and however the daemon is reached; the shim itself holds no
//! mesh identity and no socket. Authority is mediated at three layers that
//! stack: the shim's own [`consent`] gate (credentialed / external / unknown
//! capabilities need local approval), the remote wrapper's owner-scope check,
//! and the daemon's local identity. A single host approval of the meta-tool
//! surface is therefore not a skeleton key.
//!
//! Built bottom-up like the wrap side:
//! - [`backend`] — the [`CapabilityGateway`] trait + the DTOs it returns.
//! - [`validation`] — pre-flight argument validation against a tool's schema.
//! - [`consent`] / [`pins`] — the allowlist / pin consent gate and the
//!   persistent pin store. Both **graduated to `net-mesh-sdk`**
//!   (`MCP_BRIDGE_SDK_PLAN.md` P0 — consent isn't MCP-specific); these
//!   modules re-export `net_sdk::consent` / `net_sdk::pins`, and the shim
//!   only *wires* them per request.
//! - [`meta_tools`] — the `net_*` meta-tool surface the host sees.
//! - [`shim`] — the JSON-RPC server loop that wires it all together.

pub mod backend;
pub mod consent;
pub mod grouping;
pub mod mesh_gateway;
pub mod meta_tools;
pub mod pins;
pub mod shim;
pub mod validation;

pub use backend::{
    CapabilityDetail, CapabilityGateway, CapabilityId, CapabilityIdError, CapabilitySummary,
    GatewayError, InvokeSafety,
};
pub use consent::{ConsentDecision, ConsentPolicy};
pub use mesh_gateway::MeshGateway;
pub use pins::{PinState, PinStore, PinStoreError};
pub use shim::Shim;
pub use validation::{validate_args, ValidationError};

// ---------------------------------------------------------------------------
// Failure strings (MCP_BRIDGE_PLAN.md Phase 2 "Failure strings are product").
// These exact wordings are part of the product contract and are asserted by
// tests — change them only alongside the plan.
// ---------------------------------------------------------------------------

/// No daemon reachable. Emitted by the CLI wiring before the shim starts.
pub const MSG_NO_DAEMON: &str = "No Net daemon is running. Start one with: net up";
/// A capability search returned nothing — the demand side has an empty index.
pub const MSG_NO_CAPABILITIES: &str =
    "No remote capabilities found. Run 'net wrap ...' on another machine.";
/// A capability needs local approval before it can be invoked. `{id}` is the
/// capability's display id — see [`consent`].
pub const MSG_REQUIRES_APPROVAL_PREFIX: &str = "Capability requires local approval. Approve with:";
/// The remote wrapper rejected the caller's identity — a confused-deputy
/// defense firing on the supply side (see `wrap::invoke::ERR_OWNER_SCOPE`).
pub const MSG_DENIED_BY_WRAPPER: &str =
    "Denied by remote wrapper: caller root identity does not match owner scope.";

/// Build the "requires local approval" message for a specific capability id.
pub fn requires_approval_message(cap_id: &str) -> String {
    format!("{MSG_REQUIRES_APPROVAL_PREFIX} net mcp pin approve {cap_id}")
}
