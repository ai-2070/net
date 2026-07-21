//! OA2-E0.2 P0 witnesses — captured-service equality on the serve
//! bridge.
//!
//! The route discriminator (E0.2) selects a dispatcher by *canonical
//! channel hash*, but the initial `RpcRequestPayload` also carries
//! its own self-declared `service` string, and the server folds
//! historically routed to their handler without re-checking it. A
//! frame physically delivered to `admin.requests` (route = admin's
//! canonical hash) whose payload names `echo` would run the admin
//! handler under an `echo` request — a cross-service confused
//! deputy.
//!
//! These witnesses drive REAL `serve_rpc` handlers over the REAL
//! network path (node A publishes onto node B's request channel):
//!
//! - a mismatched initial REQUEST is dropped BEFORE the handler runs
//!   (admin handler invocation count stays 0), in BOTH directions
//!   (admin-route/echo-payload and echo-route/admin-payload);
//! - the drop is specifically the service mismatch: a correctly
//!   named REQUEST on the same channel DOES reach the handler
//!   (positive control), so a count of 0 above is the guard firing,
//!   not a broken harness. Under the pre-fix fold the mismatched
//!   frame would increment the count to 1 (Kyra's counterexample).

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::cortex::{
    encode_rpc_route, EventMeta, RpcContext, RpcHandler, RpcHandlerError, RpcRequestPayload,
    RpcResponsePayload, RpcStatus, DISPATCH_RPC_REQUEST, EVENT_META_SIZE, RPC_ROUTE_V1_SIZE,
};
use net::adapter::net::{
    ChannelName, ChannelPublisher, EntityKeypair, MeshNode, MeshNodeConfig, PublishConfig,
    SocketBufferConfig,
};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b.node_id())
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
}

/// Records how many times its handler body ran, so a test can assert
/// the fold did (or did not) dispatch to it.
struct CountingHandler {
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcHandler for CountingHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.count.fetch_add(1, Ordering::Relaxed);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

/// Build a real initial-REQUEST frame `EventMeta ‖ RpcRouteV1(route)
/// ‖ RpcRequestPayload{service, ..}` — the exact shape a sender
/// emits, but with `route` and `service` chosen independently so a
/// test can name a service that differs from the routed channel.
///
/// `origin` MUST be the publishing node's real origin hash: the Gate-3
/// binding in `bridge_origin_check` drops any frame whose payload
/// (`EventMeta`) origin differs from its authenticated packet origin,
/// BEFORE the service-equality gate this test is exercising. A synthetic
/// value here would make every assertion below pass vacuously — including
/// the positive controls, which is exactly how this fixture broke.
fn request_frame(origin: u64, route: u64, service: &str, body: &[u8]) -> Bytes {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, origin, 1, 0);
    let req = RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: vec![],
        body: Bytes::copy_from_slice(body),
    };
    let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + req.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, route);
    req.encode_into(&mut buf);
    Bytes::from(buf)
}

/// Publish `frame` from `a` onto `channel` (which `b` is subscribed
/// to), then give `b` a window to (not) dispatch it.
async fn deliver(a: &Arc<MeshNode>, channel: &ChannelName, frame: Bytes) {
    let publisher = ChannelPublisher::new(channel.clone(), PublishConfig::default());
    a.publish(&publisher, frame).await.expect("publish");
    tokio::time::sleep(Duration::from_millis(150)).await;
}

/// Two nodes; B serves `admin` and `echo` with counting handlers and
/// is subscribed (from A) to both request channels so a raw publish
/// from A reaches the registered dispatcher.
struct Fixture {
    a: Arc<MeshNode>,
    /// Held to keep B (which owns the registered handlers) alive.
    #[allow(dead_code)]
    b: Arc<MeshNode>,
    /// Held to keep the registrations live for the test's duration.
    #[allow(dead_code)]
    admin_handle: net::adapter::net::mesh_rpc::ServeHandle,
    #[allow(dead_code)]
    echo_handle: net::adapter::net::mesh_rpc::ServeHandle,
    admin_requests: ChannelName,
    echo_requests: ChannelName,
    admin_hits: Arc<AtomicUsize>,
    echo_hits: Arc<AtomicUsize>,
}

impl Fixture {
    async fn new() -> Self {
        let a = build_node().await;
        let b = build_node().await;
        handshake(&a, &b).await;

        let admin_hits = Arc::new(AtomicUsize::new(0));
        let echo_hits = Arc::new(AtomicUsize::new(0));
        let admin_handle = b
            .serve_rpc(
                "admin",
                Arc::new(CountingHandler {
                    count: admin_hits.clone(),
                }),
            )
            .expect("serve admin");
        let echo_handle = b
            .serve_rpc(
                "echo",
                Arc::new(CountingHandler {
                    count: echo_hits.clone(),
                }),
            )
            .expect("serve echo");

        let admin_requests = ChannelName::new("admin.requests").unwrap();
        let echo_requests = ChannelName::new("echo.requests").unwrap();
        // Subscribe B (from A) to both request channels so A's raw
        // publish is roster-delivered to B's serve dispatchers.
        b.subscribe_channel(a.node_id(), admin_requests.clone())
            .await
            .expect("sub admin.requests");
        b.subscribe_channel(a.node_id(), echo_requests.clone())
            .await
            .expect("sub echo.requests");

        Self {
            a,
            b,
            admin_handle,
            echo_handle,
            admin_requests,
            echo_requests,
            admin_hits,
            echo_hits,
        }
    }
}

/// A REQUEST routed to `admin.requests` (route = admin's canonical
/// hash) whose payload names `echo` must NOT reach the admin
/// handler. A correctly-named admin REQUEST on the same channel then
/// DOES reach it — proving the drop was the service mismatch, not a
/// dead harness.
#[tokio::test]
async fn admin_route_with_echo_payload_never_runs_admin_handler() {
    let f = Fixture::new().await;
    let admin_route = f.admin_requests.hash();

    // Confused deputy: correct route, wrong self-declared service.
    deliver(
        &f.a,
        &f.admin_requests,
        request_frame(
            f.a.origin_hash(),
            admin_route,
            "echo",
            b"payload claims echo",
        ),
    )
    .await;
    assert_eq!(
        f.admin_hits.load(Ordering::Relaxed),
        0,
        "admin handler must not run for an echo-named payload routed to admin",
    );
    assert_eq!(
        f.echo_hits.load(Ordering::Relaxed),
        0,
        "echo handler must not run either — the frame was routed to admin's dispatcher",
    );

    // Positive control: same channel, matching service → runs.
    deliver(
        &f.a,
        &f.admin_requests,
        request_frame(
            f.a.origin_hash(),
            admin_route,
            "admin",
            b"payload claims admin",
        ),
    )
    .await;
    assert_eq!(
        f.admin_hits.load(Ordering::Relaxed),
        1,
        "a correctly-named admin REQUEST on the same channel must reach the handler",
    );
}

/// The mirror direction: a REQUEST routed to `echo.requests` whose
/// payload names `admin` must NOT reach the echo handler; a
/// correctly-named echo REQUEST then does.
#[tokio::test]
async fn echo_route_with_admin_payload_never_runs_echo_handler() {
    let f = Fixture::new().await;
    let echo_route = f.echo_requests.hash();

    deliver(
        &f.a,
        &f.echo_requests,
        request_frame(
            f.a.origin_hash(),
            echo_route,
            "admin",
            b"payload claims admin",
        ),
    )
    .await;
    assert_eq!(
        f.echo_hits.load(Ordering::Relaxed),
        0,
        "echo handler must not run for an admin-named payload routed to echo",
    );
    assert_eq!(
        f.admin_hits.load(Ordering::Relaxed),
        0,
        "admin handler must not run either — the frame was routed to echo's dispatcher",
    );

    deliver(
        &f.a,
        &f.echo_requests,
        request_frame(
            f.a.origin_hash(),
            echo_route,
            "echo",
            b"payload claims echo",
        ),
    )
    .await;
    assert_eq!(
        f.echo_hits.load(Ordering::Relaxed),
        1,
        "a correctly-named echo REQUEST on the same channel must reach the handler",
    );
}
