//! RT-6 contract test — the `tool.watch` server-streamed remote
//! watch (`REALTIME_ROUTING_AND_DISCOVERY_PLAN.md` §4.4 Track C).
//!
//! Three pins against two real `Mesh`es:
//!
//! 1. **Change frames** — a tool served + announced on the serving
//!    node after the subscription opened arrives at the remote
//!    subscriber as a `ToolWatchFrame::Change(Added)`.
//! 2. **Overflow → Resync** — a non-polling, flow-controlled
//!    subscriber that falls further behind than the entire buffered
//!    path can absorb gets its queued deltas dropped and receives an
//!    explicit `ToolWatchFrame::Resync` (never a silent delta loss),
//!    after which a local `list_tools` re-baseline is consistent
//!    with the client's own fold.
//! 3. **Callee-side auth** — a caller excluded by the server's
//!    `nrpc:tool.watch` allow-list gets a terminal
//!    `CapabilityDenied` frame and the handler never runs; a
//!    higher-version permissive announcement re-opens the gate
//!    (revocation-is-an-announcement, both directions).

#![cfg(all(feature = "tool", feature = "cortex"))]

use std::net::SocketAddr;
use std::time::Duration;

use futures::StreamExt;

use net::adapter::net::behavior::{CapabilityAnnouncement, CapabilitySet, ToolCapability};
use net::adapter::net::cortex::rpc::STREAMING_PUMP_CAPACITY;
use net_sdk::capabilities::CapabilitySet as SdkCapabilitySet;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptions, CallOptionsTyped, Codec, RpcError, RpcStreamTyped};
use net_sdk::tool::{
    metadata_for, ToolListChange, ToolWatchFrame, WatchToolsRequest, TOOL_WATCH_SERVICE,
    TOOL_WATCH_SUBSCRIBER_BUFFER,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const PSK: [u8; 32] = [0x42u8; 32];

/// Synthetic origin for fold-injected flood announcements — never a
/// real peer, so nothing routes to it.
const FAKE_NODE_ID: u64 = 0xFACE_0000_0000_0001;

#[derive(JsonSchema, Deserialize, Serialize, Debug, PartialEq, Eq)]
struct WebSearchReq {
    query: String,
}

#[derive(JsonSchema, Deserialize, Serialize, Debug, PartialEq, Eq)]
struct WebSearchResp {
    results: Vec<String>,
}

async fn build_pair() -> (Mesh, Mesh, SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let addr_b = b.inner().local_addr();
    (a, b, addr_b)
}

async fn handshake(a: &Mesh, b: &Mesh, addr_b: SocketAddr) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(
        b.inner().accept(nid_a),
        a.inner().connect(addr_b, &pub_b, nid_b),
    );
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

/// Open a `tool.watch` subscription (no matcher, event-driven) from
/// `client` against `server`, optionally flow-controlled.
async fn open_watch(
    client: &Mesh,
    server: &Mesh,
    window: Option<u32>,
) -> RpcStreamTyped<ToolWatchFrame> {
    client
        .call_streaming_typed::<WatchToolsRequest, ToolWatchFrame>(
            server.inner().node_id(),
            TOOL_WATCH_SERVICE,
            &WatchToolsRequest {
                matcher: None,
                interval_ms: None,
            },
            CallOptionsTyped {
                raw: CallOptions {
                    stream_window_initial: window,
                    ..Default::default()
                },
                codec: Codec::Json,
            },
        )
        .await
        .expect("call_streaming_typed(tool.watch)")
}

/// Number of `tool.watch` handler invocations the server has seen.
fn watch_invocations(server: &Mesh) -> u64 {
    server
        .inner()
        .rpc_metrics_snapshot()
        .services
        .iter()
        .find(|s| s.service == TOOL_WATCH_SERVICE)
        .map(|s| s.handler_invocations_total)
        .unwrap_or(0)
}

/// Wait until the server-side `tool.watch` handler has started at
/// least `min` times (proves the subscription — and therefore its
/// baseline snapshot — is live before the test mutates the fold).
async fn wait_subscribed(server: &Mesh, min: u64) -> bool {
    wait_until(|| watch_invocations(server) >= min, Duration::from_secs(5)).await
}

/// Build the server's own restrictive/permissive `nrpc:tool.watch`
/// announcement for the auth-gate scenario. Empty `allowed_nodes`
/// is the permissive default; a non-empty list excludes everyone
/// not on it. Versions must supersede the auto-self-index /
/// explicit-announce version space (small counters), hence 10^6+.
fn watch_gate_announcement(
    server: &Mesh,
    version: u64,
    allowed_nodes: Vec<u64>,
) -> CapabilityAnnouncement {
    let caps = CapabilitySet::new().add_tag(format!("nrpc:{TOOL_WATCH_SERVICE}"));
    let mut ann = CapabilityAnnouncement::new(
        server.inner().node_id(),
        server.inner().entity_id().clone(),
        version,
        caps,
    );
    ann.allowed_nodes = allowed_nodes;
    ann
}

// ---------------------------------------------------------------------------
// (a) change frames reach a remote subscriber
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_watch_streams_change_frames_to_remote_subscriber() {
    let (client, server, server_addr) = build_pair().await;
    handshake(&client, &server, server_addr).await;

    // Serve the watch WITHOUT serving any tool — pins the public
    // `serve_tool_watch` install path.
    server.serve_tool_watch();

    let mut stream = open_watch(&client, &server, None).await;
    assert!(
        wait_subscribed(&server, 1).await,
        "tool.watch handler must start",
    );
    // The baseline snapshot is taken synchronously a few lines into
    // the handler after the invocation counter bumps; give it a
    // beat so the tool served below is a post-baseline change.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
        .description("Search the web.")
        .build();
    let _handle = server
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(descriptor, |req| async move {
            Ok(WebSearchResp {
                results: vec![req.query],
            })
        })
        .expect("serve_tool");
    server
        .announce_capabilities(SdkCapabilitySet::new())
        .await
        .expect("announce");

    // The server's own fold self-indexes on announce → its local
    // watch diffs → the subscriber receives the Added frame.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.next().await {
                Some(Ok(ToolWatchFrame::Change(ToolListChange::Added(d))))
                    if d.tool_id == "web_search" =>
                {
                    break;
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => panic!("stream error before Added(web_search): {e:?}"),
                None => panic!("stream closed before Added(web_search)"),
            }
        }
    })
    .await
    .expect("Added(web_search) frame within 10s");
}

// ---------------------------------------------------------------------------
// (b) overflow drops queued deltas and emits Resync — never silent loss
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_watch_overflow_drops_deltas_and_emits_resync() {
    let (client, server, server_addr) = build_pair().await;
    handshake(&client, &server, server_addr).await;

    // A real tool announced over the wire BEFORE the watch opens:
    // it replicates into the client's fold (and the subscription
    // baseline) and anchors the post-Resync re-baseline assertion.
    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
        .description("Search the web.")
        .build();
    let _handle = server
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(descriptor, |req| async move {
            Ok(WebSearchResp {
                results: vec![req.query],
            })
        })
        .expect("serve_tool");
    server
        .announce_capabilities(SdkCapabilitySet::new())
        .await
        .expect("announce");
    assert!(
        wait_until(
            || client
                .list_tools(None)
                .iter()
                .any(|d| d.tool_id == "web_search"),
            Duration::from_secs(5),
        )
        .await,
        "client fold must replicate the announced tool",
    );

    // Flow-controlled subscription with the smallest window: after
    // ONE un-granted chunk the server's publish pump stalls, so the
    // wire path can buffer at most STREAMING_PUMP_CAPACITY + 1
    // frames and everything beyond backs up into the bounded
    // per-subscriber queue — which must overflow, not drop
    // silently.
    let mut stream = open_watch(&client, &server, Some(1)).await;
    assert!(wait_subscribed(&server, 1).await, "handler must start");
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The flood: ONE injected announcement from a synthetic node
    // carrying more tools than the whole buffered path (pump queue
    // + per-subscriber queue + in-flight slack) can hold. A single
    // fold apply → a single diff pass → one Added per tool, burst-
    // emitted while the subscriber is NOT polling.
    let flood = STREAMING_PUMP_CAPACITY + TOOL_WATCH_SUBSCRIBER_BUFFER + 512;
    let tools: Vec<ToolCapability> = (0..flood)
        .map(|i| ToolCapability::new(format!("flood_{i:05}"), format!("flood_{i:05}")))
        .collect();
    let ann = CapabilityAnnouncement::new(
        FAKE_NODE_ID,
        server.inner().entity_id().clone(),
        1,
        CapabilitySet::new().add_tools(tools),
    );
    server.inner().test_inject_capability_announcement(ann);

    // Let the server's diff run and the pipeline saturate while the
    // client does not poll.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Resume polling: a prefix of buffered Change frames, then the
    // Resync the overflow contract promises.
    let mut changes_before_resync = 0usize;
    let saw_resync = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match stream.next().await {
                Some(Ok(ToolWatchFrame::Resync)) => break true,
                Some(Ok(ToolWatchFrame::Change(_))) => changes_before_resync += 1,
                Some(Err(e)) => panic!("stream error before Resync: {e:?}"),
                None => break false,
            }
        }
    })
    .await
    .expect("Resync frame within 60s");
    assert!(saw_resync, "stream must deliver Resync, not close");
    assert!(
        changes_before_resync < flood,
        "overflow must have dropped deltas ({changes_before_resync} of {flood} delivered) — \
         if every delta arrived, the per-subscriber buffer never overflowed",
    );

    // Re-baseline per the frame contract: the client rebuilds from
    // its OWN local fold via list_tools (the fold is mesh-
    // replicated; there is no remote list service). The flood was
    // injected only into the server's fold and never announced on
    // the wire, so a consistent client baseline has the announced
    // tool and none of the flood entries.
    let baseline = client.list_tools(None);
    assert!(
        baseline.iter().any(|d| d.tool_id == "web_search"),
        "re-baseline must retain the wire-announced tool",
    );
    assert!(
        baseline.iter().all(|d| !d.tool_id.starts_with("flood_")),
        "re-baseline reflects the client's own fold, which never saw the injected flood",
    );
}

// ---------------------------------------------------------------------------
// (c) callee-side capability gate on the streaming bridge
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_watch_callee_gate_denies_unauthorized_caller() {
    let (client, server, server_addr) = build_pair().await;
    handshake(&client, &server, server_addr).await;

    server.serve_tool_watch();

    // Restrictive announcement in the server's OWN fold: the
    // `nrpc:tool.watch` tag with an allow-list that excludes the
    // caller (same fixture as the unary conformance scenario 6 —
    // `may_execute` is allow-by-default on empty lists, so a
    // non-empty list is the deny fixture).
    server
        .inner()
        .test_inject_capability_announcement(watch_gate_announcement(
            &server,
            1_000_000,
            vec![0xDEAD_BEEF_BAAD_F00D],
        ));

    let mut stream = open_watch(&client, &server, None).await;
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("terminal deny frame within 5s")
        .expect("stream yields exactly one terminal item");
    match first {
        Err(RpcError::ServerError {
            status, message, ..
        }) => {
            // RpcStatus::CapabilityDenied = 0x0008 on the wire.
            assert_eq!(
                status, 0x0008,
                "expected CapabilityDenied wire status, got {status:#06x}",
            );
            assert!(
                message.contains("nrpc:tool.watch"),
                "diagnostic must name the denied tag: {message}",
            );
        }
        other => panic!("expected terminal ServerError(CapabilityDenied), got {other:?}"),
    }
    assert!(
        stream.next().await.is_none(),
        "deny must terminate the stream",
    );
    assert_eq!(
        watch_invocations(&server),
        0,
        "the gate must deny BEFORE the handler runs",
    );

    // Higher-version permissive announcement re-opens the gate
    // (revocation IS a new announcement — and so is re-granting);
    // a fresh subscription now reaches the handler.
    server
        .inner()
        .test_inject_capability_announcement(watch_gate_announcement(&server, 1_000_001, vec![]));
    let _stream = open_watch(&client, &server, None).await;
    assert!(
        wait_subscribed(&server, 1).await,
        "permissive view must admit the subscriber end-to-end",
    );
}
