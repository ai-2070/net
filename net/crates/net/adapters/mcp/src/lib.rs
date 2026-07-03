//! Net ↔ MCP edge adapter — `net wrap` (supply side) and `net mcp serve`
//! (demand side).
//!
//! **Doctrine (see `docs/plans/MCP_BRIDGE_PLAN.md`).** The Net core and
//! protocol crates have zero MCP awareness; all MCP code lives here. This
//! adapter rides on the public `net-mesh-sdk` surface only — never on core
//! crates directly (the same rule the Redis / JetStream adapters follow).
//! If MCP churns, this adapter churns; the mesh does not.
//!
//! This crate is being built bottom-up. What lands first is the piece with
//! no mesh dependency at all:
//!
//! - [`spec`] — the MCP 2026-07-28 (stateless) JSON-RPC wire types. **All**
//!   spec-version-specific shapes live here and nowhere else, so a spec bump
//!   is a single-module change (doctrine #6).
//! - [`wrap`] — the supply side. [`wrap::StdioMcpClient`] spawns a stdio MCP
//!   server and speaks JSON-RPC to it (`initialize` / `tools/list` /
//!   `tools/call`); descriptor lowering and the nRPC bridge (which pull in
//!   the SDK) land on top of it in later slices.
//!
//! The mesh-facing halves (`wrap::descriptor`, `wrap::invoke`, the
//! `serve` shim) and the `net-mesh-sdk` dependency they need arrive with
//! Phase 1 / Phase 2; keeping them out of this slice keeps the protocol
//! layer independently testable against the conformance fixture.

pub mod spec;
pub mod wrap;
