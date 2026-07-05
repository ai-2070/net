//! Net ↔ MCP edge adapter library — the single Rust implementation of MCP
//! interoperability for the mesh (`MCP_BRIDGE_SDK_PLAN.md`). The CLI's
//! `net wrap` / `net mcp serve` are thin frontends over the public APIs here,
//! and the language bindings marshal into them — one core, many faces, zero
//! reimplementation.
//!
//! **Doctrine (see `docs/plans/MCP_BRIDGE_PLAN.md`).** The Net core and
//! protocol crates have zero MCP awareness; all MCP code lives here. This
//! adapter rides on the public `net-mesh-sdk` surface only — never on core
//! crates directly (the same rule the Redis / JetStream adapters follow).
//! If MCP churns, this adapter churns; the mesh does not. And adapters
//! *attach* while nodes *participate*: agent runtimes are first-class Net
//! nodes through the general SDK — they call this crate's pure helpers
//! ([`wrap::lower_tool`], [`wrap::classify`]) when publishing MCP-backed
//! tools, never to route their identity.
//!
//! The public surface (P0 carve-out):
//!
//! - [`spec`] — the MCP 2026-07-28 (stateless) JSON-RPC wire types. **All**
//!   spec-version-specific shapes live here and nowhere else, so a spec bump
//!   is a single-module change (doctrine #6). No mesh dependency.
//! - [`wrap`] — the supply side: [`wrap::ServerPublisher::publish_server`]
//!   publishes a spawned stdio MCP server's tools as mesh capabilities,
//!   handle-scoped ([`wrap::PublicationHandle`]); [`wrap::classify`] scores
//!   credential exposure and [`wrap::lower_tool`] lowers a `tools/list` entry
//!   to `net_sdk::tool::ToolDescriptor` + MCP-bridge metadata (the two pure
//!   helpers).
//!
//! The single mesh-facing dependency is `net-mesh-sdk` (doctrine #1); the
//! `ToolDescriptor` lowering target comes from its `tool` feature.
//! - [`serve`] — the demand side: the [`serve::Shim`] stdio MCP **server**
//!   that exposes the mesh's capabilities to a local MCP host as meta-tools,
//!   with pre-flight validation and a consent gate. Consent + the pin store
//!   graduated to `net_sdk::consent` / `net_sdk::pins` (P0 — consent isn't
//!   MCP-specific); the shim wires them, re-exported at the old paths.
//! - [`forward`] — spec-only foundation for opt-in, deny-by-default credential
//!   & header forwarding (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 0): the
//!   `net.invoke.forwarded_context@1` object, its canonical binding, the
//!   policy schema, the secret wrapper type, and the never-for-stdio doctrine.
//!   It forwards nothing — it exists so future phases can't smuggle forwarding
//!   past these hostile-by-default types. **Never bound**: bindings see secret
//!   *refs* and policy surfaces only.
//! - [`bridge`] — the small inter-node describe protocol shared by the supply
//!   and demand sides: the [`bridge::BRIDGE_PROVIDER_TAG`] a demand-side
//!   gateway finds bridge providers by, and the [`bridge::DESCRIBE_SERVICE`]
//!   nRPC service name it fetches each provider's scoped catalog through.

pub mod bridge;
pub mod forward;
pub mod serve;
pub mod spec;
pub mod wrap;
