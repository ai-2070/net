//! Cold-start and reconnect **smoke ceilings**: the regression gate for
//! the numbers `benches/cold_start.rs` publishes in `BENCHMARKS.md`.
//!
//! The bench is `harness = false` and never runs in CI, so its numbers can
//! rot silently: the doc would still say ~1.3 ms long after the path took
//! tens of milliseconds. These tests replay the bench's two operational
//! paths once each and assert a **generous ceiling**, orders of magnitude
//! above the measured cost. They catch an order-of-magnitude regression (a
//! handshake that starts waiting on a timer, a first call that falls back
//! to a retry) without the flakiness a tight threshold buys on a shared CI
//! runner.
//!
//! Each measurement is the **fastest of three attempts**: one slow
//! scheduling slice on a busy runner cannot fail it, while a real
//! regression slows every attempt. The ceilings are sized for an
//! unoptimized (`opt-level = 0`) test build, which is what CI runs; see
//! [`CEILING`].
//!
//! Not a benchmark: nothing here is recorded, and a pass says only "not
//! pathologically slow". The numbers live in `BENCHMARKS.md`.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::mesh_rpc::CallOptions;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

const PSK: [u8; 32] = [0x5Au8; 32];
const SERVICE: &str = "cold_start_smoke_echo";
/// A probe service kept apart from [`SERVICE`], as in the bench: a first
/// call to a `(target, service)` pair creates client-side state, so
/// probing on the measured service would warm the path under test.
const PROBE: &str = "cold_start_smoke_probe";
const ATTEMPTS: usize = 3;
/// How long any single step may wait before the attempt is abandoned.
const STEP_DEADLINE: Duration = Duration::from_secs(10);

/// The ceiling for each path.
///
/// Measured 2026-10-01 on an i9-14900K (Windows 11), in this unoptimized
/// test build, as the fastest of three: ~21 ms cold start → first reply,
/// ~33 ms restart → first reply, stable to within a millisecond across
/// runs. That is 20–30x the release-build numbers in `BENCHMARKS.md`
/// (~1.3 ms and ~1 ms on an M1 Max).
///
/// 500 ms is 15–25x above those, which leaves room for a slower CI runner
/// while still failing on a genuine order-of-magnitude regression.
const CEILING: Duration = Duration::from_millis(500);

struct Echo;

#[async_trait::async_trait]
impl RpcHandler for Echo {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

/// The bench's `wire_floor` configuration: no announce debounce, no rate
/// limit, loopback.
fn config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    MeshNodeConfig::new(addr, PSK)
        .with_announce_debounce(Duration::ZERO)
        .with_min_announce_interval(Duration::ZERO)
}

async fn build(keypair: EntityKeypair) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(keypair, config())
            .await
            .expect("MeshNode::new"),
    )
}

/// `a` dials `b`; `b` answers with `accept`, which is only legal before
/// `b.start()`.
async fn connect(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let (a_id, b_pub, b_addr, b_id) = (a.node_id(), *b.public_key(), b.local_addr(), b.node_id());
    let responder = Arc::clone(b);
    let accept = tokio::spawn(async move { responder.accept(a_id).await });
    tokio::time::timeout(STEP_DEADLINE, a.connect(b_addr, &b_pub, b_id))
        .await
        .expect("connect finished")
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
}

async fn call(from: &Arc<MeshNode>, to: u64, service: &str) {
    tokio::time::timeout(
        STEP_DEADLINE,
        from.call(
            to,
            service,
            Bytes::from_static(b"ping"),
            CallOptions::default(),
        ),
    )
    .await
    .expect("the call finished")
    .expect("the call succeeded");
}

async fn wait_for_sessions(a: &MeshNode, b: &MeshNode) {
    let deadline = Instant::now() + STEP_DEADLINE;
    while !(a.peer_count() >= 1 && b.peer_count() >= 1) {
        assert!(Instant::now() < deadline, "sessions never came up");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Nothing → two nodes → a session → the first nRPC reply: the bench's
/// "cold start: TOTAL" row.
async fn cold_start_once() -> Duration {
    let started = Instant::now();
    let server = build(EntityKeypair::generate()).await;
    let _serve = server
        .serve_rpc(SERVICE, Arc::new(Echo))
        .expect("serve_rpc");
    let caller = build(EntityKeypair::generate()).await;
    connect(&caller, &server).await;
    caller.start();
    server.start();
    wait_for_sessions(&caller, &server).await;
    call(&caller, server.node_id(), SERVICE).await;
    started.elapsed()
}

/// A working pair, then the server drops and comes back under the same
/// identity; timed from the drop to the first nRPC reply on the new
/// session. The bench's "peer restart (same identity)" rows, summed.
///
/// The survivor dials the returning node, as in the bench: a node that has
/// started cannot `accept` a new direct handshake.
async fn restart_once() -> Duration {
    let keypair = EntityKeypair::generate();
    let server = build(keypair.clone()).await;
    let serve = server
        .serve_rpc(SERVICE, Arc::new(Echo))
        .expect("serve_rpc");
    let probe = server.serve_rpc(PROBE, Arc::new(Echo)).expect("serve_rpc");
    let caller = build(EntityKeypair::generate()).await;
    connect(&caller, &server).await;
    caller.start();
    server.start();
    wait_for_sessions(&caller, &server).await;
    call(&caller, server.node_id(), PROBE).await;

    let started = Instant::now();
    // A `ServeHandle` holds its node, so every handle goes for the socket
    // to close.
    drop((serve, probe, server));
    let returned = build(keypair).await;
    let _serve = returned
        .serve_rpc(SERVICE, Arc::new(Echo))
        .expect("serve_rpc");
    connect(&caller, &returned).await;
    returned.start();
    call(&caller, returned.node_id(), SERVICE).await;
    started.elapsed()
}

fn fastest(samples: &[Duration]) -> Duration {
    *samples.iter().min().expect("at least one attempt")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cold_start_reaches_its_first_nrpc_reply_under_the_ceiling() {
    let mut samples = Vec::with_capacity(ATTEMPTS);
    for _ in 0..ATTEMPTS {
        samples.push(cold_start_once().await);
    }
    let best = fastest(&samples);
    assert!(
        best < CEILING,
        "cold start → first nRPC reply took {best:?} at best of {samples:?}, over the \
         {CEILING:?} ceiling (~21 ms measured in a test build, ~1.3 ms in a release \
         build): an order-of-magnitude regression, not noise. Re-run `cargo bench \
         --features \"net cortex\" --bench cold_start` to see which stage moved."
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_peer_serves_its_first_nrpc_under_the_ceiling() {
    let mut samples = Vec::with_capacity(ATTEMPTS);
    for _ in 0..ATTEMPTS {
        samples.push(restart_once().await);
    }
    let best = fastest(&samples);
    assert!(
        best < CEILING,
        "peer restart → first nRPC reply took {best:?} at best of {samples:?}, over the \
         {CEILING:?} ceiling (~33 ms measured in a test build, ~1 ms in a release \
         build): an order-of-magnitude regression, not noise. Re-run `cargo bench \
         --features \"net cortex\" --bench cold_start` to see which stage moved."
    );
}
