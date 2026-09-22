// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real UDP calls with deliberately incomplete replies. Session eviction and
//! credit exhaustion are injected at the owning table/window, not mocked RPCs.
use super::*;
use crate::adapter::net::cortex::{
    encode_rpc_route, rpc::large_response, EventMeta, RpcContext, RpcHandler, RpcHandlerError,
    RpcResponsePayload, RpcStatus, DISPATCH_RPC_RESPONSE,
};
use crate::adapter::net::mesh_rpc::{CallOptions, RpcError};
use crate::adapter::net::DEFAULT_STREAM_WINDOW_BYTES;
use std::sync::atomic::{AtomicUsize, Ordering};

async fn connected_pair(start: bool) -> (Arc<MeshNode>, Arc<MeshNode>) {
    async fn node() -> Arc<MeshNode> {
        Arc::new(
            MeshNode::new(
                EntityKeypair::generate(),
                MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x42; 32])
                    .with_heartbeat_interval(Duration::from_millis(200))
                    .with_session_timeout(Duration::from_secs(5)),
            )
            .await
            .unwrap(),
        )
    }
    let caller = node().await;
    let server = node().await;
    let accept = {
        let server = server.clone();
        let caller_id = caller.node_id();
        tokio::spawn(async move { server.accept(caller_id).await.unwrap() })
    };
    caller
        .connect(server.local_addr(), server.public_key(), server.node_id())
        .await
        .unwrap();
    accept.await.unwrap();
    if start {
        caller.start();
        server.start();
    }
    (caller, server)
}

struct PausedHandler {
    entered: tokio::sync::mpsc::UnboundedSender<u64>,
    release: Arc<tokio::sync::Notify>,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcHandler for PausedHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.send(ctx.call_id).unwrap();
        self.release.notified().await;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::new(),
        })
    }
}

/// Publish up to `limit` fragments of a 22 000-byte response for `call_id`,
/// in order, over the real reply path (stream 0xF123).
/// `send_first_fragment` is `limit == 1`; the delayed-duplicate correlation
/// sends the complete transfer.
async fn send_response_fragments(server: &MeshNode, caller: &MeshNode, call_id: u64, limit: usize) {
    let channel = ChannelName::new(&format!(
        "partial.replies.{:016x}",
        caller.identity.entity_id().origin_hash()
    ))
    .unwrap();
    let mut pieces = Vec::new();
    large_response::emit(
        RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(vec![b'x'; 22_000]),
        },
        true,
        |piece| pieces.push(piece),
    );
    for piece in pieces.into_iter().take(limit) {
        let mut frame = EventMeta::new(
            DISPATCH_RPC_RESPONSE,
            0,
            server.identity.entity_id().origin_hash(),
            call_id,
            0,
        )
        .to_bytes()
        .to_vec();
        encode_rpc_route(&mut frame, channel.hash());
        piece.encode_into(&mut frame);
        assert!(matches!(
            server
                .try_publish_to_peer_bound(
                    caller.node_id(),
                    channel.hash(),
                    0xF123,
                    true,
                    &[Bytes::from(frame)],
                    server.peer_session_id(caller.node_id()),
                )
                .await,
            PeerPublishOutcome::Sent
        ));
    }
}

async fn send_first_fragment(server: &MeshNode, caller: &MeshNode, call_id: u64) {
    send_response_fragments(server, caller, call_id, 1).await;
}

#[derive(Clone, Copy)]
enum End {
    Deadline,
    Drop,
    DropDuringPublish,
    Cancel,
    SessionEviction,
    SessionRetirement,
    Shutdown,
}

async fn partial_call_cleanup(end: End) {
    let (caller, server) = connected_pair(true).await;
    let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc(
            "partial",
            Arc::new(PausedHandler {
                entered,
                release: release.clone(),
                calls: calls.clone(),
            }),
        )
        .unwrap();
    if matches!(end, End::DropDuringPublish) {
        // Drop the call future INSIDE its request-publish await — the gap
        // between `register_large` and the `UnaryCallGuard` — with fragment
        // state already attached. The same `(1, 22_007) -> (0, 0)`
        // leak-vs-release transition the other ends assert proves the
        // pre-publish cleanup; without the guard installed before the
        // publish, the entry and its 22 007 budget bytes strand forever.
        let request_stream = MeshNode::publish_stream_id(&ChannelId::new(
            ChannelName::new("partial.requests").unwrap(),
        ));
        let arrived = caller.arm_publish_park(request_stream);
        let call = {
            let caller = caller.clone();
            let server_id = server.node_id();
            tokio::spawn(async move {
                caller
                    .call(server_id, "partial", Bytes::new(), CallOptions::default())
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), arrived.notified())
            .await
            .expect("call must park inside its publish await");
        let pending = caller.rpc_client_pending_arc();
        assert_eq!(
            pending.retained_for_test(),
            (1, 0),
            "the pending entry is registered before the publish runs"
        );
        let ids = pending.unary_call_ids_for_test();
        assert_eq!(ids.len(), 1, "exactly the parked call is pending");
        let call_id = ids[0];
        send_first_fragment(&server, &caller, call_id).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while pending.retained_for_test().1 == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first fragment must reach real reply dispatcher");
        assert_eq!(
            pending.retained_for_test(),
            (1, 22_007),
            "fragment state hangs off the still-pending call"
        );
        assert!(!call.is_finished(), "the call is parked mid-publish");
        caller.disarm_publish_park();
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        assert_eq!(
            pending.retained_for_test(),
            (0, 0),
            "drop during publish must release the pending entry and all fragment storage"
        );
        release.notify_one();
        caller.shutdown().await.unwrap();
        server.shutdown().await.unwrap();
        assert_eq!(pending.retained_for_test(), (0, 0));
        return;
    }
    let token = caller.reserve_cancel_token();
    let call = {
        let caller = caller.clone();
        let server_id = server.node_id();
        tokio::spawn(async move {
            caller
                .call(
                    server_id,
                    "partial",
                    Bytes::new(),
                    CallOptions {
                        deadline: matches!(end, End::Deadline)
                            .then(|| Instant::now() + Duration::from_secs(2)),
                        cancel_token: matches!(end, End::Cancel).then_some(token),
                        ..Default::default()
                    },
                )
                .await
        })
    };
    let call_id = tokio::time::timeout(Duration::from_secs(1), entered_rx.recv())
        .await
        .unwrap()
        .unwrap();
    send_first_fragment(&server, &caller, call_id).await;
    let pending = caller.rpc_client_pending_arc();
    tokio::time::timeout(Duration::from_secs(1), async {
        while pending.retained_for_test().1 == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first fragment must reach real reply dispatcher");
    assert_eq!(pending.retained_for_test(), (1, 22_007));
    assert!(
        !call.is_finished(),
        "a partial reply must not complete a call"
    );
    match end {
        End::Deadline => {
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(3), call)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(RpcError::Timeout { .. })
            ));
        }
        End::Drop => {
            call.abort();
            assert!(call.await.unwrap_err().is_cancelled());
        }
        End::Cancel => {
            caller.cancel_registry().cancel(token);
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(1), call)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(RpcError::Cancelled)
            ));
        }
        End::DropDuringPublish => unreachable!("handled above"),
        End::SessionEviction | End::SessionRetirement | End::Shutdown => {
            // Deterministic session-table eviction: exercise the live call's
            // retirement watcher without relying on heartbeat timing.
            match end {
                End::SessionEviction => {
                    assert!(caller.peers.remove(&server.node_id()).is_some());
                }
                End::SessionRetirement => caller
                    .peers
                    .get(&server.node_id())
                    .unwrap()
                    .session
                    .retire_receive_lifetime(),
                End::Shutdown => caller.shutdown().await.unwrap(),
                _ => unreachable!(),
            }
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(1), call)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(RpcError::Transport(_))
            ));
        }
    }
    assert_eq!(
        pending.retained_for_test(),
        (0, 0),
        "all fragment storage must be released"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no handler retry");
    // A delayed duplicate cannot recreate a retired pending entry. The
    // duplicate has no state effect to wait on — that IS the property — so
    // its processing is correlated through a LIVE control call on the same
    // reply stream (review finding 17). The control call is registered
    // FIRST, so no later registration can sweep state the duplicate
    // creates; then the duplicate; then the control's completing fragment
    // transfer — all on stream 0xF123 in publish order. The control call
    // resolving with its reassembled 22 000-byte body is the real reply
    // dispatcher having processed everything ahead of it, duplicate
    // included, so the final (0, 0) is observed AFTER the duplicate was
    // processed rather than vacuously before it.
    if matches!(end, End::Deadline | End::Drop | End::Cancel) {
        let control = {
            let caller = caller.clone();
            let server_id = server.node_id();
            tokio::spawn(async move {
                caller
                    .call(server_id, "partial", Bytes::new(), CallOptions::default())
                    .await
            })
        };
        let control_id = tokio::time::timeout(Duration::from_secs(1), entered_rx.recv())
            .await
            .expect("control call must reach the handler")
            .expect("handler reports its call id");
        send_first_fragment(&server, &caller, call_id).await;
        send_response_fragments(&server, &caller, control_id, usize::MAX).await;
        let reply = tokio::time::timeout(Duration::from_secs(3), control)
            .await
            .expect("control response must be processed in-window")
            .unwrap()
            .expect("control call must complete");
        assert_eq!(
            reply.body.len(),
            22_000,
            "the control must ride the real fragment path"
        );
        assert_eq!(
            pending.retained_for_test(),
            (0, 0),
            "a delayed duplicate must not recreate a retired pending entry"
        );
    }
    release.notify_one();
    caller.shutdown().await.unwrap();
    server.shutdown().await.unwrap();
    assert_eq!(pending.retained_for_test(), (0, 0));
}

#[tokio::test]
async fn partial_response_deadline_releases_real_call_storage() {
    partial_call_cleanup(End::Deadline).await;
}
#[tokio::test]
async fn partial_response_dropped_future_releases_real_call_storage() {
    partial_call_cleanup(End::Drop).await;
}
#[tokio::test]
async fn partial_response_drop_during_publish_releases_real_call_storage() {
    partial_call_cleanup(End::DropDuringPublish).await;
}
#[tokio::test]
async fn partial_response_cancel_token_releases_real_call_storage() {
    partial_call_cleanup(End::Cancel).await;
}
#[tokio::test]
async fn partial_response_session_eviction_releases_call_without_deadline() {
    partial_call_cleanup(End::SessionEviction).await;
}

#[tokio::test]
async fn partial_response_retired_session_releases_call_without_deadline() {
    partial_call_cleanup(End::SessionRetirement).await;
}

#[tokio::test]
async fn partial_response_node_shutdown_releases_call_without_deadline() {
    partial_call_cleanup(End::Shutdown).await;
}

#[tokio::test]
async fn fragment_credit_stall_is_bounded_and_credit_can_be_reused() {
    // No receive loops: peer credit cannot be replenished by the network.
    let (sender, receiver) = connected_pair(false).await;
    let session = sender
        .peers
        .get(&receiver.node_id())
        .unwrap()
        .session
        .clone();
    const STREAM: u64 = 0xF124;
    session.open_stream_with(STREAM, true, 1);
    let reservation = match session.try_acquire_tx_credit_guard(STREAM, DEFAULT_STREAM_WINDOW_BYTES)
    {
        TxAdmit::Acquired { guard, .. } => guard,
        _ => panic!("fresh stream must have its full window"),
    };
    let event = Bytes::from_static(b"bounded fragment send");
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        sender.try_publish_to_peer_bound(
            receiver.node_id(),
            1,
            STREAM,
            true,
            std::slice::from_ref(&event),
            Some(session.session_id()),
        ),
    )
    .await
    .expect("credit stall must be bounded");
    assert!(matches!(result, PeerPublishOutcome::SendFailed(_)));
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "must wait for credit, not fail immediately"
    );
    drop(reservation);
    assert!(matches!(
        sender
            .try_publish_to_peer_bound(
                receiver.node_id(),
                1,
                STREAM,
                true,
                &[event],
                Some(session.session_id()),
            )
            .await,
        PeerPublishOutcome::Sent
    ));
}

async fn interrupted_credit_wait(retire: bool) {
    let (sender, receiver) = connected_pair(false).await;
    let session = sender
        .peers
        .get(&receiver.node_id())
        .unwrap()
        .session
        .clone();
    const STREAM: u64 = 0xF125;
    session.open_stream_with(STREAM, true, 1);
    let reservation = match session.try_acquire_tx_credit_guard(STREAM, DEFAULT_STREAM_WINDOW_BYTES)
    {
        TxAdmit::Acquired { guard, .. } => guard,
        _ => panic!("fresh window"),
    };
    let events = [Bytes::from_static(b"waiting fragment")];
    let send = sender.try_publish_to_peer_bound(
        receiver.node_id(),
        1,
        STREAM,
        true,
        &events,
        Some(session.session_id()),
    );
    tokio::pin!(send);
    tokio::select! {
        _ = &mut send => panic!("send must be blocked before credit returns"),
        _ = tokio::time::sleep(Duration::from_millis(20)) => {}
    }
    if retire {
        session.retire_receive_lifetime();
    }
    // Refund after retirement too: available credit must not revive a retired
    // incarnation or allow a blocked fragment to escape the liveness fence.
    drop(reservation);
    let result = tokio::time::timeout(Duration::from_millis(300), send)
        .await
        .unwrap();
    if retire {
        assert!(matches!(result, PeerPublishOutcome::SendFailed(_)));
        assert_eq!(
            session.try_stream(STREAM).unwrap().tx_credit_remaining(),
            DEFAULT_STREAM_WINDOW_BYTES
        );
    } else {
        assert!(matches!(result, PeerPublishOutcome::Sent));
        assert!(
            session.try_stream(STREAM).unwrap().tx_credit_remaining() < DEFAULT_STREAM_WINDOW_BYTES
        );
    }
}

#[tokio::test]
async fn fragment_wait_resumes_after_credit_refund() {
    interrupted_credit_wait(false).await;
}

#[tokio::test]
async fn fragment_wait_cannot_send_after_session_retirement() {
    interrupted_credit_wait(true).await;
}

#[tokio::test]
async fn inactive_but_unretired_session_still_accepts_fragment_send() {
    let (sender, receiver) = connected_pair(false).await;
    let session = sender
        .peers
        .get(&receiver.node_id())
        .unwrap()
        .session
        .clone();
    // Advisory inactivity is not retirement; fencing it would break legitimate
    // late traffic (wire::Session's NR3 contract).
    session.deactivate();
    assert!(matches!(
        sender
            .try_publish_to_peer_bound(
                receiver.node_id(),
                1,
                0xF126,
                true,
                &[Bytes::from_static(b"still live")],
                Some(session.session_id())
            )
            .await,
        PeerPublishOutcome::Sent
    ));
}

struct SizedHandler(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl RpcHandler for SizedHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(vec![
                b'x';
                if ctx.payload.body.is_empty() {
                    22_000
                } else {
                    5
                }
            ]),
        })
    }
}

#[tokio::test]
async fn large_sender_capacity_refuses_whole_response_without_blocking_small_replies() {
    let (caller, server) = connected_pair(true).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let _a = server
        .serve_rpc("capacity_a", Arc::new(SizedHandler(calls.clone())))
        .unwrap();
    let _b = server
        .serve_rpc("capacity_b", Arc::new(SizedHandler(calls.clone())))
        .unwrap();
    let held = server
        .rpc_large_response_slots
        .clone()
        .try_acquire_many_owned(MeshNode::LARGE_RESPONSE_PUMP_SLOTS as u32)
        .unwrap();
    for service in ["capacity_a", "capacity_b"] {
        let result = caller
            .call(
                server.node_id(),
                service,
                Bytes::new(),
                CallOptions {
                    deadline: Some(Instant::now() + Duration::from_secs(2)),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        let RpcError::ServerError {
            status, message, ..
        } = result
        else {
            panic!("pump exhaustion must surface as ServerError");
        };
        assert_eq!(
            status,
            RpcStatus::Backpressure.to_wire(),
            "pump exhaustion must surface as Backpressure so the caller can \
             tell a capacity refusal from a handler failure (Internal)"
        );
        assert!(message.contains("sender capacity exhausted"));
        assert_eq!(caller.rpc_client_pending_arc().retained_for_test(), (0, 0));
    }
    let small = caller
        .call(
            server.node_id(),
            "capacity_a",
            Bytes::from_static(b"small"),
            CallOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(small.body.len(), 5);
    drop(held);
    let large = caller
        .call(
            server.node_id(),
            "capacity_a",
            Bytes::new(),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_secs(2)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(large.body.len(), 22_000);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(server.rpc_large_response_slots.available_permits(), 8);
    caller.shutdown().await.unwrap();
    server.shutdown().await.unwrap();
}

enum SenderEnd {
    Cancel,
    Deadline,
    Shutdown,
}

async fn stopped_sender_returns_slot(end: SenderEnd) {
    let deadline = matches!(end, SenderEnd::Deadline);
    let (caller, server) = connected_pair(true).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let _serve = server
        .serve_rpc("blocked_delivery", Arc::new(SizedHandler(calls.clone())))
        .unwrap();
    let channel = ChannelName::new(&format!(
        "blocked_delivery.replies.{:016x}",
        caller.identity.entity_id().origin_hash()
    ))
    .unwrap();
    let stream = MeshNode::publish_stream_id(&ChannelId::new(channel));
    let session = server.peers.get(&caller.node_id()).unwrap().session.clone();
    session.open_stream_with(stream, true, 1);
    assert!(session
        .try_stream(stream)
        .unwrap()
        .try_acquire_tx_credit(DEFAULT_STREAM_WINDOW_BYTES));
    let token = caller.reserve_cancel_token();
    let call = {
        let caller = caller.clone();
        let server_id = server.node_id();
        tokio::spawn(async move {
            caller
                .call(
                    server_id,
                    "blocked_delivery",
                    Bytes::new(),
                    CallOptions {
                        deadline: deadline.then(|| Instant::now() + Duration::from_millis(500)),
                        cancel_token: Some(token),
                        ..Default::default()
                    },
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_millis(400), async {
        while server.rpc_large_response_slots.available_permits() == 8 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler returned and pump is blocked on credit");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(server.rpc_large_response_slots.available_permits(), 7);
    if matches!(end, SenderEnd::Shutdown) {
        server.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_millis(300), async {
            while server.rpc_large_response_slots.available_permits() != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("server shutdown stops the pump without a caller CANCEL");
        caller.cancel(token);
    } else if deadline {
        // Suppress the caller's automatic CANCEL on timeout: the sender must
        // stop on the request deadline itself, not pass because CANCEL arrived.
        caller
            .partition_filter
            .insert(PeerAddr::Udp(server.local_addr()));
    } else {
        caller.cancel(token);
    }
    let result = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .unwrap()
        .unwrap();
    if deadline {
        assert!(matches!(result, Err(RpcError::Timeout { .. })));
    } else {
        assert!(matches!(result, Err(RpcError::Cancelled)));
    }
    tokio::time::timeout(Duration::from_millis(300), async {
        while server.rpc_large_response_slots.available_permits() != 8 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancel/deadline releases server ownership before per-packet stall timeout");
    assert_eq!(caller.rpc_client_pending_arc().retained_for_test(), (0, 0));
    session
        .try_stream(stream)
        .unwrap()
        .refund_tx_credit(DEFAULT_STREAM_WINDOW_BYTES);
    // No queued fragment may consume newly available credit after completion.
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        session.try_stream(stream).unwrap().tx_credit_remaining(),
        DEFAULT_STREAM_WINDOW_BYTES
    );
    caller.shutdown().await.unwrap();
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancel_after_handler_return_stops_large_sender_and_releases_slot() {
    stopped_sender_returns_slot(SenderEnd::Cancel).await;
}

#[tokio::test]
async fn shared_request_deadline_stops_large_sender_before_credit_stall_timeout() {
    stopped_sender_returns_slot(SenderEnd::Deadline).await;
}

#[tokio::test]
async fn server_shutdown_stops_large_sender_and_releases_slot() {
    stopped_sender_returns_slot(SenderEnd::Shutdown).await;
}
