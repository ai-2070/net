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

async fn send_first_fragment(server: &MeshNode, caller: &MeshNode, call_id: u64) {
    let channel = ChannelName::new(&format!(
        "partial.replies.{:016x}",
        caller.identity.entity_id().origin_hash()
    ))
    .unwrap();
    let mut first = None;
    large_response::emit(
        RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(vec![b'x'; 22_000]),
        },
        true,
        |piece| {
            if first.is_none() {
                first = Some(piece);
            }
        },
    );
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
    first.unwrap().encode_into(&mut frame);
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

#[derive(Clone, Copy)]
enum End {
    Deadline,
    Drop,
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
    // A delayed duplicate cannot recreate a retired pending entry.
    if matches!(end, End::Deadline | End::Drop | End::Cancel) {
        send_first_fragment(&server, &caller, call_id).await;
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
