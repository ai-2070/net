//! Net ↔ MCP edge adapter — `net wrap` (supply side) and `net mcp serve`
//! (demand side).
//!
//! **Doctrine (see `docs/plans/MCP_BRIDGE_PLAN.md`).** The Net core and
//! protocol crates have zero MCP awareness; all MCP code lives here. This
//! adapter rides on the public `net-mesh-sdk` surface only — never on core
//! crates directly (the same rule the Redis / JetStream adapters follow).
//! If MCP churns, this adapter churns; the mesh does not.
//!
//! This crate is being built bottom-up:
//!
//! - [`spec`] — the MCP 2026-07-28 (stateless) JSON-RPC wire types. **All**
//!   spec-version-specific shapes live here and nowhere else, so a spec bump
//!   is a single-module change (doctrine #6). No mesh dependency.
//! - [`wrap`] — the supply side: [`wrap::StdioMcpClient`] speaks JSON-RPC to
//!   a spawned MCP server; [`wrap::classify`] scores credential exposure; and
//!   [`wrap::lower_tool`] lowers a `tools/list` entry to
//!   `net_sdk::tool::ToolDescriptor` + MCP-bridge metadata.
//!
//! The single mesh-facing dependency is `net-mesh-sdk` (doctrine #1); the
//! `ToolDescriptor` lowering target comes from its `tool` feature. Still to
//! land: the nRPC → `tools/call` bridge, the announce/serve wiring, and the
//! `serve` shim (Phase 2).

pub mod spec;
pub mod wrap;
