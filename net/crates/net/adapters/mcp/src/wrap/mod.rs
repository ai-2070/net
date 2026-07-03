//! Supply side — wrap a local MCP server as a mesh capability.
//!
//! - [`stdio`] — [`StdioMcpClient`]: spawn a stdio MCP server and speak
//!   JSON-RPC 2.0 to it (no mesh dependency).
//! - [`credentials`] — classify a wrapped server's credential exposure,
//!   conservatively (unknown is gated like credentialed).
//! - [`descriptor`] — lower a `tools/list` entry to `net_sdk::tool::ToolDescriptor`
//!   plus the MCP-bridge metadata carried as `CapabilitySet` tags.
//! - [`invoke`] — [`WrapInvokeHandler`]: bridge an incoming nRPC call to the
//!   wrapped `tools/call`, gated by the caller's owner scope.
//!
//! Still to land (Phase 1): the announce/serve wiring that discovers the
//! wrapped tools, lowers them, and serves each [`WrapInvokeHandler`] on the
//! mesh — plus the `net wrap` CLI command that drives it.

pub mod credentials;
pub mod descriptor;
pub mod invoke;
pub mod stdio;

pub use credentials::{classify, CredentialOverride, CredentialStatus, WrapEnv};
pub use descriptor::{lower_tool, LoweredTool, LoweringContext, Substitutability};
pub use invoke::{OwnerScope, WrapInvokeHandler};
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
