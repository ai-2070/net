//! `RpcObserver` hook — observability seam on the typed-nRPC
//! dispatch path. Fires on every `call_typed` (caller side)
//! completion with the metadata an operator-facing tail (the
//! deck's NRPC view, a metrics exporter, a tracing bridge)
//! wants: caller, callee, method, latency, status, byte counts.
//!
//! The observer is installed at the `MeshNode` level via
//! [`super::super::MeshNode::set_rpc_observer`] and fires from
//! the substrate's call path so every caller's traffic flows
//! through it without per-call wiring at the SDK surface.
//!
//! See `DECK_DEMO_HARNESS_PLAN.md` Missing Item D for the design
//! rationale. v1 ships caller-side firing only; server-side
//! (inbound) firing is a follow-up — the dispatch path's
//! mpsc-driven handler invocation needs additional plumbing
//! before we can record the dispatch-to-respond span cleanly.

use std::sync::Arc;

/// Direction of the observed RPC boundary relative to the local
/// node — `Outbound` for calls this node initiated, `Inbound`
/// for handler invocations on this node. v1 emits only
/// `Outbound`; `Inbound` is reserved for a future server-side
/// hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcDirection {
    /// Local node initiated the call.
    Outbound,
    /// Local node received the call and ran the handler.
    Inbound,
}

/// Status of an observed RPC call. Maps from the dispatch
/// path's exit branches: `Ok` for a successful response,
/// `Error(msg)` for a server-returned typed error or a
/// transport-level failure, `Timeout` for a deadline expiry,
/// and `Canceled` for a future drop / cancel-token trip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcCallStatus {
    /// Successful response received from the callee.
    Ok,
    /// Server returned a typed error or a transport-level
    /// failure surfaced before the response could be parsed.
    /// The string carries an operator-readable diagnostic.
    Error(String),
    /// `opts.deadline` expired before the response arrived.
    Timeout,
    /// The call future was dropped before completion (e.g.
    /// `select!` cancelled, hedge loser, explicit cancel
    /// token). Reserved — not yet emitted by v1.
    Canceled,
}

/// Single observed RPC boundary. All fields are populated from
/// the substrate's call path at fire time; the observer must
/// not mutate them (the type is owned for cheap per-call
/// construction).
#[derive(Clone, Debug)]
pub struct RpcCallEvent {
    /// The 64-bit node id of the calling node. Equal to
    /// `local_node_id` on `Outbound` events.
    pub caller: u64,
    /// The 64-bit node id of the responding node.
    pub callee: u64,
    /// Service / method name as passed into `call_typed` or
    /// registered via `serve_rpc_typed`.
    pub method: String,
    /// Wall-clock-equivalent elapsed time between request send
    /// (caller side) or dispatch (server side) and the
    /// observation point. Truncated to ms — observers in this
    /// codebase don't need ns resolution and `u32` keeps the
    /// struct compact.
    pub latency_ms: u32,
    /// Outcome of the call at the observation point.
    pub status: RpcCallStatus,
    /// Wire payload size of the request body (excluding the
    /// 24-byte `EventMeta` prefix). 0 when not available
    /// (transport-error branches before the body was framed).
    pub request_bytes: u32,
    /// Wire payload size of the response body. 0 when not
    /// available (timeout, transport error, or cancellation
    /// before the response arrived).
    pub response_bytes: u32,
    /// Whether the observation came from the caller side
    /// (`Outbound`) or the server side (`Inbound`).
    pub direction: RpcDirection,
    /// Unix-ms timestamp captured at fire time. Best-effort
    /// (pre-1970 clocks read 0).
    pub ts_unix_ms: u64,
}

/// Observer trait. The substrate calls `on_call` synchronously
/// from the dispatch task on each completed RPC boundary;
/// implementations must be cheap (the firing thread is the
/// hot path). A push into a bounded mpsc or a lock-free ring
/// is the expected shape.
pub trait RpcObserver: Send + Sync + 'static {
    /// Fired once per observed RPC boundary. Must be cheap —
    /// the dispatch thread blocks until the call returns.
    fn on_call(&self, evt: RpcCallEvent);
}

/// Convenience type alias for the swappable observer cell on
/// `MeshNode`. `Arc<dyn RpcObserver>` lets multiple ArcSwap
/// loads share the same underlying observer without cloning
/// the trait object.
pub type RpcObserverHandle = Arc<dyn RpcObserver>;

/// Capture `Instant::now()` translated to a unix-millis
/// timestamp. Used by the call-path firing sites. Wall-clock
/// is best-effort; a pre-1970 clock reads 0 rather than
/// underflowing.
pub fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_now_ms_returns_recent_timestamp() {
        // The only meaningful contract for this helper: monotonic on
        // a sane clock (`SystemTime::now()` doesn't read pre-epoch)
        // and within a reasonable window of "now". We pin both —
        // the `unwrap_or(0)` fallback only fires on a pre-1970 clock.
        let t = unix_now_ms();
        // 2025-01-01 in unix ms — any sane test environment is past
        // this, so a zero return would mean the SystemTime call
        // failed in a way we want to surface.
        assert!(t > 1_735_689_600_000, "unix_now_ms returned suspicious {t}");
    }
}
