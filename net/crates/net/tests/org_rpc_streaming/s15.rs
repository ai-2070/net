//! Helpers for the slice 1.5 (bridge wiring + routing) witnesses.
//!
//! The 1.5 idiom drives the REAL serve bridge: `ServeHandle`'s fixtures
//! `inject_inbound_for_test` hands a frame to the bridge's own bounded mpsc
//! (the dispatcher's exact hand-off point), so the shared callee preflight,
//! the §3 admission transaction and the fold drive run in the production
//! order and synchronously in one bridge iteration. Every response (denial,
//! chunk, terminal) then leaves through the REAL transport, so the routing
//! assertions observe genuine wire delivery at real endpoint nodes — the
//! caller and bystander recorders of the NC2 probe idiom
//! (`nrpc_streaming_gate.rs:268-305`).

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use parking_lot::Mutex;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::cortex::rpc::{
    encode_rpc_route, RequestStream, RpcClientStreamingHandler, RpcContext, RpcDuplexHandler,
    RpcHandlerError, RpcInboundDispatcher, RpcInboundEvent, RpcRequestPayload, RpcResponsePayload,
    RpcResponseSink, RpcStatus, RpcStreamingHandler, FLAG_RPC_STREAMING_RESPONSE,
};
use net::adapter::net::cortex::{EventMeta, RpcStreamingContext};
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::MeshNode;

/// The captured endpoint frames at a recorder, in delivery order.
pub(crate) type CapturedEvents = Arc<Mutex<Vec<RpcInboundEvent>>>;

/// A recording `RpcInboundDispatcher` + its capture — the probe idiom: a
/// node registers it under the reply channel's hash and every frame the
/// provider routes to that channel lands here.
pub(crate) fn recorder() -> (RpcInboundDispatcher, CapturedEvents) {
    let seen: CapturedEvents = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let disp: RpcInboundDispatcher = Arc::new(move |ev| sink.lock().push(ev));
    (disp, seen)
}

/// Decode a captured RESPONSE frame's payload.
pub(crate) fn response_of(ev: &RpcInboundEvent) -> RpcResponsePayload {
    RpcResponsePayload::decode(
        ev.payload
            .slice(net::adapter::net::cortex::rpc::RPC_FRAME_BODY_OFFSET..),
    )
    .expect("a captured response frame decodes")
}

/// The wire shape of an `AdmissionDenied` (0x0009 + exactly one coarse
/// reason byte) — the assertion helper for "the caller is told".
pub(crate) fn is_denied_byte(ev: &RpcInboundEvent, coarse: u8) -> bool {
    let resp = response_of(ev);
    resp.status == RpcStatus::AdmissionDenied && resp.body.as_ref() == [coarse]
}

/// Build a wire frame (EventMeta + rpc route + encoded request) from a
/// finalized request payload.
pub(crate) fn frame_from(req: &RpcRequestPayload, call_id: u64, origin: u64) -> Bytes {
    let meta = EventMeta::new(
        net::adapter::net::cortex::DISPATCH_RPC_REQUEST,
        0,
        origin,
        call_id,
        0,
    );
    let mut buf = Vec::new();
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, 0);
    req.encode_into(&mut buf);
    Bytes::from(buf)
}

/// An SS-shaped opening REQUEST carrying NO admission header — the
/// "presents no org credential" forbidden opening (`MissingHeader`, coarse
/// `Denied`). `service` lets the same shape drive the CS/DX bridges too.
pub(crate) fn plain_ss_opening(service: &str, call_id: u64, origin: u64, body: &[u8]) -> Bytes {
    let req = RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags: FLAG_RPC_STREAMING_RESPONSE,
        headers: vec![],
        body: Bytes::copy_from_slice(body),
    };
    frame_from(&req, call_id, origin)
}

/// A server-streaming handler that counts ENTRIES and SINK SENDS — the
/// send-site counter is the plan's "output emission" channel (a handler
/// counter alone is not proof), and the wire recorders observe what the
/// sends would have put on the network.
pub(crate) struct CountingSS {
    pub(crate) entries: Arc<AtomicUsize>,
    pub(crate) sends: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for CountingSS {
    async fn call(&self, _ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        for chunk in [b"item-a".as_slice(), b"item-b"] {
            self.sends.fetch_add(1, Ordering::SeqCst);
            sink.send(Bytes::from_static(chunk));
        }
        Ok(())
    }
}

/// A client-streaming handler that must stay dark (input-delivery
/// observation).
pub(crate) struct CountingCS {
    pub(crate) entries: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for CountingCS {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        while requests.next().await.is_some() {}
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::new(),
        })
    }
}

/// A duplex handler that must stay dark (input-delivery observation).
pub(crate) struct CountingDX {
    pub(crate) entries: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcDuplexHandler for CountingDX {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        while let Some(r) = requests.next().await {
            responses.send(r.to_vec());
        }
        Ok(())
    }
}

/// Wire three nodes the way the NC2 probe does — direct sessions first,
/// ONE `start()` per node, then signed announcements so each side pins the
/// other's entity (the provider-side `resolve_direct_caller` needs the
/// server's pin of the caller). The bystander needs no pin: it only
/// subscribes and records.
pub(crate) async fn connect_three(
    caller: &Arc<MeshNode>,
    server: &Arc<MeshNode>,
    bystander: &Arc<MeshNode>,
) {
    super::fixture::connect_no_start(caller, server).await;
    super::fixture::connect_no_start(bystander, server).await;
    caller.start();
    bystander.start();
    server.start();
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("server announce");
    caller
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("caller announce");
    let caller_id = caller.node_id();
    let server_id = server.node_id();
    assert!(
        super::fixture::wait_until(std::time::Duration::from_secs(5), || {
            caller.peer_entity_id(server_id).is_some() && server.peer_entity_id(caller_id).is_some()
        })
        .await,
        "entity pins established in both directions",
    );
}

/// Bounded settle that FAILS the moment an unexpected frame lands: a
/// same-window version of the §T2 darkness discipline for a recorder that
/// must stay empty (an instant `is_empty()` read only proves "not yet").
pub(crate) async fn assert_stays_empty(
    seen: &CapturedEvents,
    window: std::time::Duration,
    what: &str,
) {
    let deadline = std::time::Instant::now() + window;
    loop {
        let observed = seen.lock().len();
        assert_eq!(
            observed, 0,
            "{what}: {observed} frame(s) reached the roster subscriber — \
             a protected response was fanned out",
        );
        if std::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Fresh caller keypair identity pair helper — the caller node's keypair
/// must be the proof's subject (step 5's TOFU member binding).
pub(crate) fn caller_keypair(seed: u8) -> EntityKeypair {
    EntityKeypair::from_bytes([seed; 32])
}
