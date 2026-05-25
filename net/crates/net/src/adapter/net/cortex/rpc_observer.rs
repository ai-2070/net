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

use std::sync::atomic::{AtomicU64, Ordering};
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

// ============================================================================
// ObserverChannel — bounded-mpsc trampoline shared across bindings (N4).
//
// Every binding (napi, pyo3, rpc-ffi) was hand-rolling the same
// bounded-mpsc + drain-worker + drop-counter shape. Consolidating
// here lets each binding write only its language-specific dispatch
// closure (TSFN call / GIL-acquired Python invocation / C function
// pointer) instead of ~55 lines of channel + worker plumbing.
// ============================================================================

/// Bound on the per-binding observer event buffer. Big enough that
/// a momentarily-slow observer doesn't lose events under normal
/// load; small enough that an actually-broken observer surfaces
/// drops within seconds rather than minutes.
pub const OBSERVER_BUFFER_CAPACITY: usize = 1024;

/// Process-global count of observer events dropped because the
/// bounded buffer was full. Shared across every binding's
/// [`ObserverChannel`] instance. Surface via the binding's
/// `metrics_snapshot.observer_dropped_total` field.
///
/// Per-process (not per-mesh / per-binding-instance) because the
/// observer dispatch path is fundamentally per-process; consumers
/// reading the snapshot expect a monotonic process-lifetime count.
pub static OBSERVER_DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Bounded-mpsc observer trampoline. Constructed by each language
/// binding's `set_observer` implementation; installed on the mesh
/// via [`super::super::MeshNode::set_rpc_observer`].
///
/// The substrate dispatch path's [`RpcObserver::on_call`] pays only
/// `Arc::clone` + `try_send` + atomic counter on overflow — every
/// allocation / GIL-acquisition / TSFN call defers to the worker
/// drained off the dispatch thread.
pub struct ObserverChannel {
    sender: tokio::sync::mpsc::Sender<Arc<RpcCallEvent>>,
}

impl ObserverChannel {
    /// Build a bounded channel + spawn a drain worker on the given
    /// runtime handle. `dispatch` runs once per drained event on
    /// the worker task — bindings put their language-specific
    /// invocation here (TSFN, GIL acquisition + Python call, C
    /// function-pointer call).
    ///
    /// The worker exits cleanly when the channel closes (every
    /// `ObserverChannel` is dropped + no more senders exist).
    pub fn install<F>(runtime: &tokio::runtime::Handle, dispatch: F) -> Self
    where
        F: Fn(Arc<RpcCallEvent>) + Send + 'static,
    {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<Arc<RpcCallEvent>>(OBSERVER_BUFFER_CAPACITY);
        runtime.spawn(async move {
            while let Some(evt) = receiver.recv().await {
                dispatch(evt);
            }
            // Sender dropped → channel closed → worker exits.
        });
        Self { sender }
    }
}

impl RpcObserver for ObserverChannel {
    fn on_call(&self, evt: RpcCallEvent) {
        if self.sender.try_send(Arc::new(evt)).is_err() {
            OBSERVER_DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Current value of the process-global observer drop counter.
/// Bindings surface this via their snapshot's
/// `observer_dropped_total` field.
pub fn observer_dropped_total() -> u64 {
    OBSERVER_DROPPED_TOTAL.load(Ordering::Relaxed)
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

    /// `ObserverChannel::on_call` drops events when the bounded
    /// channel fills and increments `OBSERVER_DROPPED_TOTAL` by one
    /// per drop. The whole point of the v3 mpsc design — overflow
    /// MUST surface via the snapshot's `observer_dropped_total` so
    /// a slow consumer is observable from production telemetry.
    ///
    /// The worker gate is a `parking_lot::Mutex` held by the test
    /// for the duration of the burst; the worker tries to lock it
    /// once per event and blocks until the burst is done. Avoids
    /// `std::thread::sleep` which would also block tokio shutdown.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn observer_channel_drops_overflow_events_and_counts_them() {
        let handle = tokio::runtime::Handle::current();
        let gate = Arc::new(parking_lot::Mutex::new(()));
        let baseline = OBSERVER_DROPPED_TOTAL.load(Ordering::Relaxed);
        let burst_guard = gate.lock();
        let worker_gate = gate.clone();
        let channel = ObserverChannel::install(&handle, move |_evt| {
            let _wait = worker_gate.lock();
        });
        let make_event = || RpcCallEvent {
            caller: 1,
            callee: 2,
            method: "test.svc.echo".into(),
            latency_ms: 0,
            status: RpcCallStatus::Ok,
            request_bytes: 0,
            response_bytes: 0,
            direction: RpcDirection::Outbound,
            ts_unix_ms: 0,
        };
        const FIRED: u64 = 2000;
        for _ in 0..FIRED {
            channel.on_call(make_event());
        }
        let dropped = OBSERVER_DROPPED_TOTAL.load(Ordering::Relaxed) - baseline;
        // First event reaches the worker (which then blocks on the
        // gate); OBSERVER_BUFFER_CAPACITY-1 fit in the buffer; the
        // rest drop. Allow ±1 slack for the worker's recv-then-lock
        // race.
        let expected_min = FIRED - OBSERVER_BUFFER_CAPACITY as u64 - 1;
        assert!(
            dropped >= expected_min,
            "expected ≥ {expected_min} drops, got {dropped}",
        );
        drop(burst_guard);
    }

    #[test]
    fn observer_dropped_total_helper_matches_static() {
        let direct = OBSERVER_DROPPED_TOTAL.load(Ordering::Relaxed);
        let via_helper = observer_dropped_total();
        assert_eq!(direct, via_helper);
    }
}
