//! Supply side — publish local MCP servers as mesh capabilities.
//!
//! The public entry point is [`ServerPublisher::publish_server`]
//! (`MCP_BRIDGE_SDK_PLAN.md` P0) — spawn, discover, lower, announce, serve —
//! returning a [`PublicationHandle`] that scopes the publication's lifetime.
//! The pieces it orchestrates:
//!
//! - [`stdio`] — [`StdioMcpClient`]: spawn a stdio MCP server and speak
//!   JSON-RPC 2.0 to it (no mesh dependency).
//! - [`credentials`] — [`classify`]: score a wrapped server's credential
//!   exposure, conservatively (unknown is gated like credentialed). A **pure
//!   helper** native integrations may call to *display* risk before
//!   publishing through the general SDK.
//! - [`descriptor`] — [`lower_tool`]: lower a `tools/list` entry to
//!   `net_sdk::tool::ToolDescriptor` plus the MCP-bridge metadata carried as
//!   `CapabilitySet` tags. The other pure helper — lowering stays
//!   single-sourced here for every consumer.
//! - [`invoke`] — [`WrapInvokeHandler`]: bridge an incoming nRPC call to the
//!   wrapped `tools/call`, gated by the caller's owner scope.
//! - [`catalog`] — the describe service internals (one service per node,
//!   merged + per-publication-scoped when a node carries several
//!   publications).
//! - [`session`] — [`ServerPublisher`] / [`PublicationHandle`]: the
//!   orchestration + the per-node merge behind them.

pub mod catalog;
pub mod credentials;
pub mod delegation;
pub mod descriptor;
pub mod invoke;
pub mod session;
pub mod stdio;

pub use credentials::{classify, ClassifyError, CredentialOverride, CredentialStatus, WrapEnv};
pub use delegation::{
    build_challenge, build_envelope, AuditSink, DelegationAudit, DelegationGate, DelegationReject,
    DelegationSigner, HDR_DELEGATION, HDR_DELEGATION_SIG,
};
pub use descriptor::{lower_tool, LoweredTool, LoweringContext, Substitutability};
pub use invoke::{OwnerScope, ToolInvoker, WrapInvokeHandler, ERR_DELEGATION};
pub use session::{
    build_capability_set, LocalPublicationHandle, PublicationHandle, RefreshDelta, ServerPublisher,
    WrapConfig, WrapError,
};
pub use stdio::StdioMcpClient;

use crate::spec::JsonRpcError;

/// Errors from talking to a wrapped stdio MCP server.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// The MCP server process failed to spawn.
    #[error("failed to spawn MCP server: {0}")]
    Spawn(#[source] std::io::Error),

    /// An I/O error writing to / reading from the server's stdio.
    #[error("MCP stdio I/O error: {0}")]
    Io(#[source] std::io::Error),

    /// The transport ended before a response arrived (server exited, stdout
    /// closed, or a pipe was missing).
    #[error("MCP transport closed: {0}")]
    Transport(String),

    /// The server returned a JSON-RPC **error** response. Distinct from a
    /// tool-level error (which is a successful `tools/call` result with
    /// `is_error = true`) — this is a protocol failure.
    #[error("MCP protocol error {}: {}", .0.code, .0.message)]
    Protocol(JsonRpcError),

    /// A message or result could not be (de)serialized against the wire
    /// types in [`crate::spec`].
    #[error("MCP message decode error: {0}")]
    Decode(#[source] serde_json::Error),
}
