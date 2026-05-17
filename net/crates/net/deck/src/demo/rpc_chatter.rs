//! Phase 4 of `DECK_DEMO_PLAN.md` — real nRPC observation.
//!
//! Three pieces:
//! 1. **Observer bridge.** An [`RpcObserver`] impl that
//!    translates `RpcCallEvent`s into `NrpcCall` records and
//!    pushes them into the deck's `NrpcTail`. Installed on
//!    every node's `Mesh`.
//! 2. **Responders.** On 2 of the 5 nodes (indices 0 and 1)
//!    we call `mesh.serve_rpc_typed` to register an `echo`
//!    typed handler.
//! 3. **Requesters.** On the remaining 3 nodes (2, 3, 4) a
//!    tokio task fires periodic `call_typed` requests at a
//!    random responder. Real Noise-encrypted UDP, real
//!    substrate dispatch, real observer firings.
//!
//! The NRPC tab populates from observation, not from a
//! synthetic seeder.

use std::sync::Arc;
use std::time::Duration;

use net_sdk::mesh_rpc::{
    CallOptions, CallOptionsTyped, Codec, RpcCallEvent, RpcCallStatus, RpcError, RpcObserver,
    ServeHandle,
};
use net_sdk::testing::ClusterHarness;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::streams::{NrpcCall, NrpcStatus, NrpcTail};

/// Typed echo request — single u64 the responder bounces back.
/// The wire body is small (~12 bytes JSON) so the observer's
/// `request_bytes` reads as a recognizable non-trivial number
/// rather than appearing as a one-off transport-only payload.
#[derive(Debug, Serialize, Deserialize)]
struct EchoRequest {
    tick: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct EchoResponse {
    tick: u64,
    note: String,
}

/// Service name the responders register and the requesters
/// call. Stable across the demo session.
const ECHO_SERVICE: &str = "demo.echo";

/// Per-requester call cadence. Matches `DECK_DEMO_PLAN.md`'s
/// Phase 4 spec — 3 requesters at 250 ms each ≈ 12 calls/s,
/// dense enough that the NRPC tab updates visibly. Tunable
/// in one place.
const CALL_INTERVAL: Duration = Duration::from_millis(250);

/// Per-call deadline. Generous for loopback but bounded so a
/// hung responder doesn't accumulate in-flight call_id state
/// in the caller's pending map forever.
const CALL_DEADLINE: Duration = Duration::from_millis(2_000);

/// Observer bridge — converts substrate `RpcCallEvent`s into
/// deck `NrpcCall` records and pushes them into the shared
/// `NrpcTail`. Cheap on the hot path: one record allocation +
/// one Mutex push.
struct NrpcTailObserver {
    tail: NrpcTail,
}

impl RpcObserver for NrpcTailObserver {
    fn on_call(&self, evt: RpcCallEvent) {
        let status = match evt.status {
            RpcCallStatus::Ok => NrpcStatus::Ok,
            RpcCallStatus::Error(msg) => NrpcStatus::Error(msg),
            RpcCallStatus::Timeout => NrpcStatus::Timeout,
            RpcCallStatus::Canceled => NrpcStatus::Error("canceled".to_string()),
        };
        let call = NrpcCall {
            ts_ms: evt.ts_unix_ms,
            caller: evt.caller,
            callee: evt.callee,
            method: evt.method,
            latency_ms: evt.latency_ms,
            status,
            request_bytes: evt.request_bytes,
            response_bytes: evt.response_bytes,
        };
        self.tail.push(call);
    }
}

/// Wire the observer bridge on every node's Mesh. Idempotent —
/// re-calling replaces the previous observer.
pub fn install_observers(harness: &ClusterHarness, tail: NrpcTail) {
    for node in harness.nodes() {
        let obs: Arc<dyn RpcObserver> = Arc::new(NrpcTailObserver { tail: tail.clone() });
        node.mesh().set_rpc_observer(Some(obs));
    }
}

/// Register typed `echo` handlers on the first two nodes. The
/// returned handles must live for the demo session so the
/// service stays registered.
pub fn install_responders(
    harness: &ClusterHarness,
) -> Result<Vec<ServeHandle>, color_eyre::Report> {
    let mut handles = Vec::new();
    for idx in 0..2 {
        let node = harness.nth(idx);
        let h = node
            .mesh()
            .serve_rpc_typed(ECHO_SERVICE, Codec::Json, |req: EchoRequest| async move {
                Ok::<_, String>(EchoResponse {
                    tick: req.tick,
                    note: format!("echoed tick={}", req.tick),
                })
            })
            .map_err(|e| color_eyre::eyre::eyre!("serve_rpc_typed on node[{idx}]: {e:?}"))?;
        handles.push(h);
    }
    Ok(handles)
}

/// Spawn one requester task per node[2..N]. Each task fires a
/// `call_typed` at one of the two responder nodes every
/// `CALL_INTERVAL` ms, alternating between responders so
/// both nodes see traffic.
pub fn spawn_requester_loops(harness: &ClusterHarness) -> Vec<JoinHandle<()>> {
    let responder_ids: Vec<u64> = harness.nodes().iter().take(2).map(|n| n.node_id()).collect();
    if responder_ids.is_empty() {
        return Vec::new();
    }
    harness
        .nodes()
        .iter()
        .enumerate()
        .skip(2)
        .map(|(idx, node)| {
            let mesh = node.mesh().clone();
            let responders = responder_ids.clone();
            tokio::spawn(async move {
                run_requester_loop(idx, mesh, responders).await;
            })
        })
        .collect()
}

async fn run_requester_loop(
    requester_idx: usize,
    mesh: Arc<net_sdk::mesh::Mesh>,
    responders: Vec<u64>,
) {
    let mut tick: u64 = 0;
    loop {
        tokio::time::sleep(CALL_INTERVAL).await;
        let target = responders[(tick as usize + requester_idx) % responders.len()];
        let req = EchoRequest { tick };
        let opts = CallOptionsTyped {
            raw: CallOptions {
                deadline: Some(std::time::Instant::now() + CALL_DEADLINE),
                ..CallOptions::default()
            },
            ..CallOptionsTyped::default()
        };
        // We don't read the response — the observer captures
        // the call boundary regardless of caller-side success
        // handling, and the demo isn't using the response
        // body for anything.
        let _resp: Result<EchoResponse, RpcError> =
            mesh.call_typed(target, ECHO_SERVICE, &req, opts).await;
        tick = tick.wrapping_add(1);
    }
}
