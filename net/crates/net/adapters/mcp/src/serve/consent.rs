//! Shim-side consent gate — **graduated to `net-mesh-sdk`**
//! (`MCP_BRIDGE_SDK_PLAN.md` P0).
//!
//! Consent isn't MCP-specific: every surface that exposes mesh capabilities
//! to a model-driven caller gates invocation on the same local decision. The
//! one implementation lives in [`net_sdk::consent`]; this module re-exports it
//! so the shim and existing `net_mcp::serve::consent` consumers keep working
//! — the bridge holds **no** consent logic of its own (doctrine #1).
//!
//! The doctrine lives with the implementation — see the SDK module for the
//! trust-boundary notes (every wire status is gated, including `"none"`;
//! display never implies invocation; approvals stay out of band). What stays
//! bridge-side is only the *wiring*: [`super::Shim`] combines this policy with
//! the persistent [`super::pins`] store per request.

pub use net_sdk::consent::{ConsentDecision, ConsentPolicy};
