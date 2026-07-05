//! The persistent pin store — **graduated to `net-mesh-sdk`**
//! (`MCP_BRIDGE_SDK_PLAN.md` P0).
//!
//! The machine-shared store, its atomic persistence, and the cross-process
//! lock protocol live in [`net_sdk::pins`] — one lock implementation on one
//! file, ever (doctrine #1). This module re-exports the types so the shim and
//! existing `net_mcp::serve::pins` consumers keep working; the bridge never
//! opens the store file through any other path.
//!
//! The two design rules (a model request can only ever write a *pending*
//! record — approval is operator-only; every read-modify-write goes through
//! [`PinStore::mutate`] under the advisory lock) are documented with the
//! implementation in the SDK.

pub use net_sdk::pins::{PinState, PinStore, PinStoreError};
