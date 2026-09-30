//! Cold-start latency — "nothing → a node that has seen its first peer".
//!
//! The complement of every other bench in this directory: they all
//! measure cost on an already-warm node (CPB warms every topology
//! explicitly; the datapath benches construct their structures outside
//! the timed region). Nothing measured the cost of getting *to* that
//! state. This bench does, in stages that compose into the headline
//! number:
//!
//! 1. `identity` — `EntityKeypair::generate()` (ed25519).
//! 2. `node_build` — `MeshNode::new` (UDP bind + session/router/
//!    failure-detector construction); keypair folded in, since that is
//!    what a caller pays for one node.
//! 3. `node_start` — `MeshNode::start()` alone (receive/heartbeat/
//!    capability-GC tasks), node built outside the timed region.
//! 4. `pair_first_peer` — the full path, decomposed per stage and then
//!    as a total: build A → build B → handshake → start both → A
//!    announces → B's fold exposes A. Change-driven, never a poll loop;
//!    the endpoint is an exact-state read (`find_nodes_by_filter`),
//!    matching C1 of the capability-propagation plan.
//!
//! **What this is NOT.** It does not measure process exec, dynamic
//! linking, or the CLI (`net mcp serve`) start-to-ready path — those are
//! layer-above costs that need a spawned binary, not a benchmark target.
//! It is in-process, warm-cache, mechanism-floor policy
//! ([`BenchConfig::wire_floor`]: no announce debounce, no rate limit) on
//! loopback, so it is the transport+handshake+decode+fold-insert floor,
//! not a policy-laden production boot.
//!
//! Rows 5–9 add the operational endpoints: cold start → first nRPC reply
//! (with the warm second call as a baseline), and a peer restart — drop →
//! reconnect → first nRPC — in both identity modes (same identity, the
//! daemon-restart case, and a fresh identity). nRPC needs only the
//! session, not the capability fold, so those rows skip the announce leg
//! by construction — that is what the primitive requires, not a shortcut.
//! `MeshNode` has no in-place reconnect, so "reconnect" is a rebuilt node
//! re-handshaking with the survivor.
//!
//! The pair rows run twice: `wire_floor` (the mechanism floor) and
//! `default_policy` (the shipped 100 ms announce debounce + 10 s announce
//! rate limit). The floor is not what an operator experiences.
//!
//! Run: `cargo bench --features "net cortex" --bench cold_start`
//!
//! Numbers are published in `BENCHMARKS.md`.

#[path = "bench_mesh_pair/mod.rs"]
mod bench_mesh_pair;

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;

use bench_mesh_pair::{
    await_capability_state, connect, node, permissive, runtime, wait_until, BenchConfig,
    LatencyReport, WORKER_THREADS,
};
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::mesh_rpc::CallOptions;
use net::adapter::net::{EntityKeypair, MeshNode};

/// Trivial echo handler — the server side of every nRPC row. Returns the
/// request body unchanged, so a reply proves the full round trip.
struct EchoHandler;

#[async_trait::async_trait]
impl RpcHandler for EchoHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

/// `EntityKeypair::generate` is pure CPU + one OS RNG read; a long run
/// costs nothing and resolves the tail.
const IDENTITY_SAMPLES: u64 = 2_000;
/// `MeshNode::new` binds a fresh UDP socket and runs no background tasks
/// (the receive loop is spawned by `start`), so a few hundred is cheap.
const BUILD_SAMPLES: u64 = 200;
/// `start()` and a full pair each allocate a socket and spawn tasks that
/// only retire on drop. A whole run is ~2 s at 100 samples per multi-node
/// row, so the count is set by noise, not cost: at 30 the pair total's
/// p50 moved ±9% between runs.
const START_SAMPLES: u64 = 100;
const PAIR_SAMPLES: u64 = 100;
/// Discarded leading samples. No topology is reused, so this only sheds
/// first-touch allocator/page-fault effects.
const WARMUP: u64 = 3;
/// Per-sample visibility deadline; an exceeded deadline is counted as a
/// timeout and dropped (never recorded as a latency).
const DEADLINE: Duration = Duration::from_secs(5);

/// The service name every nRPC row serves and calls.
const RPC_SERVICE: &str = "cold_start_echo";
/// A distinct service for liveness probes. The probe must NOT use
/// [`RPC_SERVICE`]: a first call to a `(target, service)` pair creates
/// client-side state (the lazy reply subscription and the route cache),
/// so probing on the measured service would warm exactly the path the
/// measurement claims to be cold.
const PROBE_SERVICE: &str = "cold_start_probe";

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== Net cold start (nothing → usable node) ===\n");
        identity_generate();
        node_build().await;
        node_start().await;
        pair_cold_start(
            &BenchConfig::wire_floor(),
            "pair cold start (wire floor)",
            PAIR_SAMPLES,
            WARMUP,
            DEADLINE,
        )
        .await;
        // The shipped policy: 100 ms announce debounce + 10 s announce
        // rate limit. The mechanism floor above is not what an operator
        // experiences — and this pass answers whether the policy delays a
        // fresh node's FIRST announce (measured: it does not; it governs
        // subsequent ones).
        pair_cold_start(
            &BenchConfig::default_policy(),
            "pair cold start (default policy)",
            PAIR_SAMPLES,
            WARMUP,
            DEADLINE,
        )
        .await;
        rpc_after_cold_start().await;
        reconnect_after_drop().await;
    });
}

/// One self-describing row. Cold-start stages are not the
/// capability-propagation shape (no manifest, no version delta), so they
/// print their own line instead of [`LatencyReport::print_row`].
fn print_stage(
    label: &str,
    start_event: &str,
    endpoint: &str,
    report: &LatencyReport,
    warmup: u64,
    timeouts: u64,
) {
    println!("── {label} ──");
    println!("   start={start_event}  endpoint={endpoint}");
    println!(
        "   samples={} warmup={} workers={} timeouts={}",
        report.samples(),
        warmup,
        WORKER_THREADS,
        timeouts,
    );
    println!(
        "   p50={:.2}us p95={:.2}us p99={:.2}us max={:.2}us mean={:.2}us",
        report.quantile_us(0.50),
        report.quantile_us(0.95),
        report.quantile_us(0.99),
        report.max_us(),
        report.mean_us(),
    );
    println!();
}

/// Stage 1 — ed25519 keypair generation, the first thing every node pays.
fn identity_generate() {
    let mut report = LatencyReport::new();
    for i in 0..IDENTITY_SAMPLES {
        let t = Instant::now();
        let kp = EntityKeypair::generate();
        let d = t.elapsed();
        black_box(&kp);
        if i >= WARMUP {
            report.record(d.as_nanos() as u64);
        }
    }
    print_stage(
        "identity · EntityKeypair::generate",
        "EntityKeypair::generate()",
        "returned keypair",
        &report,
        WARMUP,
        0,
    );
}

/// Stage 2 — one node from nothing: keypair + `MeshNode::new`. No
/// background tasks are spawned until `start`, so the sample count can be
/// high without accumulating live loops.
async fn node_build() {
    let cfg = BenchConfig::wire_floor();
    let mut report = LatencyReport::new();
    for i in 0..BUILD_SAMPLES {
        let t = Instant::now();
        let n = MeshNode::new(EntityKeypair::generate(), cfg.mesh_config())
            .await
            .expect("MeshNode::new");
        let d = t.elapsed();
        if i >= WARMUP {
            report.record(d.as_nanos() as u64);
        }
        drop(n);
    }
    print_stage(
        "node build · keypair + MeshNode::new",
        "EntityKeypair::generate()",
        "constructed node (socket bound, no tasks)",
        &report,
        WARMUP,
        0,
    );
}

/// Stage 3 — `start()` in isolation: the node is built outside the timed
/// region, so this is only the task-spawn cost.
async fn node_start() {
    let cfg = BenchConfig::wire_floor();
    let mut report = LatencyReport::new();
    for i in 0..START_SAMPLES {
        let n = node(&cfg).await;
        let t = Instant::now();
        n.start();
        let d = t.elapsed();
        if i >= WARMUP {
            report.record(d.as_nanos() as u64);
        }
        drop(n);
    }
    print_stage(
        "node start · MeshNode::start",
        "MeshNode::start()",
        "receive/heartbeat/GC tasks spawned",
        &report,
        WARMUP,
        0,
    );
}

/// Stage 4 — the headline: build both nodes, handshake, start, let A
/// announce, and stop when B's fold exposes A. Per-stage rows decompose
/// the total so a regression points at a layer.
async fn pair_cold_start(
    cfg: &BenchConfig,
    label: &str,
    samples: u64,
    warmup: u64,
    deadline: Duration,
) {
    let mut build_a = LatencyReport::new();
    let mut build_b = LatencyReport::new();
    let mut handshake = LatencyReport::new();
    let mut start = LatencyReport::new();
    let mut announce_visible = LatencyReport::new();
    let mut total = LatencyReport::new();
    let mut timeouts = 0u64;

    for i in 0..samples {
        let t_total = Instant::now();

        let t = Instant::now();
        let a = node(cfg).await;
        let d_build_a = t.elapsed();

        let t = Instant::now();
        let b = node(cfg).await;
        let d_build_b = t.elapsed();

        let t = Instant::now();
        connect(&a, &b).await;
        let d_handshake = t.elapsed();

        let t = Instant::now();
        a.start();
        b.start();
        let d_start = t.elapsed();

        let a_id = a.node_id();
        let tag = format!("cs:{i}");
        let mut rx = b.capability_fold().subscribe_changes();
        let t = Instant::now();
        a.announce_capabilities(CapabilitySet::new().add_tag(tag))
            .await
            .expect("announce");

        // Endpoint: B's fold exposing A, awaited on the fold watch (the
        // wake mechanism), confirmed by the exact-state read — never the
        // bare wake (the fold signals under its write locks).
        let outcome = tokio::time::timeout(
            deadline,
            await_capability_state(&mut rx, || {
                b.find_nodes_by_filter(&permissive()).contains(&a_id)
            }),
        )
        .await;
        let d_announce = t.elapsed();

        if outcome.is_ok() && i >= warmup {
            let d_total = t_total.elapsed();
            build_a.record(d_build_a.as_nanos() as u64);
            build_b.record(d_build_b.as_nanos() as u64);
            handshake.record(d_handshake.as_nanos() as u64);
            start.record(d_start.as_nanos() as u64);
            announce_visible.record(d_announce.as_nanos() as u64);
            total.record(d_total.as_nanos() as u64);
        } else if outcome.is_err() {
            timeouts += 1;
        }

        drop((a, b));
    }

    let n = total.samples();
    print_stage(
        &format!("{label} · build A"),
        "Instant::now()",
        "node A constructed",
        &build_a,
        warmup,
        timeouts,
    );
    print_stage(
        &format!("{label} · build B"),
        "Instant::now()",
        "node B constructed",
        &build_b,
        warmup,
        timeouts,
    );
    print_stage(
        &format!("{label} · handshake (connect + accept)"),
        "Instant::now()",
        "session pinned both sides",
        &handshake,
        warmup,
        timeouts,
    );
    print_stage(
        &format!("{label} · start() both"),
        "Instant::now()",
        "receive loops live",
        &start,
        warmup,
        timeouts,
    );
    print_stage(
        &format!("{label} · announce → first peer visible"),
        "announce_capabilities call",
        "exact-state read (find_nodes_by_filter, change-driven wait)",
        &announce_visible,
        warmup,
        timeouts,
    );
    println!("── {label} · TOTAL ──");
    println!("   start=node(&cfg) for A  endpoint=B's fold exposes A  samples={n}");
    println!(
        "   p50={:.2}us p95={:.2}us p99={:.2}us max={:.2}us mean={:.2}us  timeouts={}",
        total.quantile_us(0.50),
        total.quantile_us(0.95),
        total.quantile_us(0.99),
        total.max_us(),
        total.mean_us(),
        timeouts,
    );
    println!();
}

/// Stage 5 — cold start → first nRPC reply. Builds a two-node cluster
/// from nothing (server + caller, handshake, start, serve), waits for the
/// session, then issues the first `call` and stops at the reply. Records
/// the session-ready leg and the call leg separately so a regression
/// points at transport vs. the RPC path. No announce leg: nRPC addresses
/// the peer directly and needs only the session, which is exactly what
/// `call` requires.
async fn rpc_after_cold_start() {
    let cfg = BenchConfig::wire_floor();
    let mut ready = LatencyReport::new();
    let mut call_leg = LatencyReport::new();
    let mut warm_call = LatencyReport::new();
    let mut total = LatencyReport::new();
    let mut timeouts = 0u64;

    for i in 0..PAIR_SAMPLES {
        let t_total = Instant::now();

        let server = node(&cfg).await;
        let serve = server
            .serve_rpc(RPC_SERVICE, Arc::new(EchoHandler))
            .expect("serve_rpc");

        let caller = node(&cfg).await;
        connect(&caller, &server).await;
        caller.start();
        server.start();

        // Session readiness on both sides — the transport-level sentinel
        // (no capability announce required for a direct call).
        let up = wait_until(Duration::from_secs(5), || {
            caller.peer_count() >= 1 && server.peer_count() >= 1
        })
        .await;
        let d_ready = t_total.elapsed();
        if !up {
            timeouts += 1;
            drop((caller, server, serve));
            continue;
        }

        let t = Instant::now();
        let reply = tokio::time::timeout(
            DEADLINE,
            caller.call(
                server.node_id(),
                RPC_SERVICE,
                Bytes::from_static(b"hello"),
                CallOptions::default(),
            ),
        )
        .await;
        let d_call = t.elapsed();

        // Warm baseline: the SECOND call on the same pair. The first call
        // paid the lazy reply subscription; this one is steady state, so
        // the first-call leg minus this is the subscription setup cost.
        let mut d_warm = None;
        if reply.is_ok() {
            let t = Instant::now();
            let warm = tokio::time::timeout(
                DEADLINE,
                caller.call(
                    server.node_id(),
                    RPC_SERVICE,
                    Bytes::from_static(b"again"),
                    CallOptions::default(),
                ),
            )
            .await;
            if warm.is_ok() {
                d_warm = Some(t.elapsed());
            }
        }

        match reply {
            Ok(Ok(_)) if i >= WARMUP => {
                ready.record(d_ready.as_nanos() as u64);
                call_leg.record(d_call.as_nanos() as u64);
                total.record(t_total.elapsed().as_nanos() as u64);
                if let Some(d) = d_warm {
                    warm_call.record(d.as_nanos() as u64);
                }
            }
            Ok(Ok(_)) => {}
            _ => timeouts += 1,
        }
        drop((caller, server, serve));
    }

    print_stage(
        "rpc cold start · build server + caller → session ready",
        "node(&cfg) for server",
        "peer_count >= 1 both sides",
        &ready,
        WARMUP,
        timeouts,
    );
    print_stage(
        "rpc cold start · first call → reply",
        "call(...) invocation",
        "RpcReply returned (echo body)",
        &call_leg,
        WARMUP,
        timeouts,
    );
    print_stage(
        "rpc cold start · second call → reply (warm, same pair)",
        "call(...) invocation",
        "RpcReply returned (echo body)",
        &warm_call,
        WARMUP,
        timeouts,
    );
    println!("── rpc cold start · TOTAL (nothing → first nRPC reply) ──");
    println!(
        "   start=node(&cfg) for server  endpoint=first RpcReply  samples={}",
        total.samples()
    );
    println!(
        "   p50={:.2}us p95={:.2}us p99={:.2}us max={:.2}us mean={:.2}us  timeouts={}",
        total.quantile_us(0.50),
        total.quantile_us(0.95),
        total.quantile_us(0.99),
        total.max_us(),
        total.mean_us(),
        timeouts,
    );
    println!();
}

/// Stage 6/7 — a node restarts: the RPC server drops out of the mesh, a
/// replacement cold-starts, the survivor re-handshakes with it, and the
/// first nRPC lands. Two measurements each: (a) drop → session
/// re-established, and (b) re-established → first nRPC reply. Run twice:
/// the returning node keeping its **same identity** (what a daemon
/// restart looks like — the survivor's stale session is *replaced*) and
/// with a **fresh identity** (the survivor ends with two peer entries).
///
/// **Why the survivor initiates the handshake.** A node answers an
/// inbound handshake only through `accept()`, and `accept()` after
/// `start()` is a documented error (CR-7: the dispatch loop would race
/// the responder for `msg1`). A peer that is already running therefore
/// cannot admit a brand-new inbound handshake — so the returning node
/// arms `accept()` pre-`start()` and the survivor, already started,
/// dials it (the post-`start()` *initiator* path is supported).
async fn reconnect_after_drop() {
    // `connect()` passes no prior-session expectation, and
    // `PriorSession::from_option(None)` is `PriorSession::Any` — "replace
    // whatever is installed, if anything". So a same-identity reconnect
    // takes the install transition over the stale session rather than
    // being refused.
    reconnect_variant(true, "peer restart · fresh identity").await;
    reconnect_variant(false, "peer restart · same identity").await;
}

async fn reconnect_variant(fresh_identity: bool, label: &str) {
    let cfg = BenchConfig::wire_floor();
    let mut reconnect = LatencyReport::new();
    let mut call_leg = LatencyReport::new();
    let mut timeouts = 0u64;

    for i in 0..PAIR_SAMPLES {
        // A working pair: caller (the survivor) ↔ server1. Built
        // explicitly so the returning node can reuse the identity.
        let server_keypair = EntityKeypair::generate();
        let server1 = Arc::new(
            MeshNode::new(server_keypair.clone(), cfg.mesh_config())
                .await
                .expect("MeshNode::new"),
        );
        let serve1_call = server1
            .serve_rpc(RPC_SERVICE, Arc::new(EchoHandler))
            .expect("serve_rpc");
        let serve1_probe = server1
            .serve_rpc(PROBE_SERVICE, Arc::new(EchoHandler))
            .expect("serve_rpc");
        let caller = node(&cfg).await;
        connect(&caller, &server1).await;
        caller.start();
        server1.start();

        let up = wait_until(Duration::from_secs(5), || {
            caller.peer_count() >= 1 && server1.peer_count() >= 1
        })
        .await;
        // Probe on its own service: proves the pair is live without
        // creating the measured service's client state.
        let healthy = up
            && caller
                .call(
                    server1.node_id(),
                    PROBE_SERVICE,
                    Bytes::from_static(b"probe"),
                    CallOptions::default(),
                )
                .await
                .is_ok();
        if !healthy {
            timeouts += 1;
            drop((caller, server1, serve1_call, serve1_probe));
            continue;
        }

        // The server node drops. `ServeHandle` holds a strong `Arc` back
        // to its node, so every handle must go for the socket to actually
        // close. t0 is the drop; everything the returning side pays —
        // keypair, socket, handshake, start, re-registration — is inside.
        let t_reconnect = Instant::now();
        drop(serve1_call);
        drop(serve1_probe);
        drop(server1);

        // The returning node: same identity (the survivor's session is
        // replaced) or a fresh one (a second peer entry), per variant.
        let return_keypair = if fresh_identity {
            EntityKeypair::generate()
        } else {
            server_keypair
        };
        let server2 = Arc::new(
            MeshNode::new(return_keypair, cfg.mesh_config())
                .await
                .expect("MeshNode::new"),
        );
        let serve2_call = server2
            .serve_rpc(RPC_SERVICE, Arc::new(EchoHandler))
            .expect("serve_rpc");
        let serve2_probe = server2
            .serve_rpc(PROBE_SERVICE, Arc::new(EchoHandler))
            .expect("serve_rpc");
        // server2 is not started → its responder `accept` is legal;
        // caller is started → its post-start initiator path is legal.
        connect(&caller, &server2).await;
        server2.start();

        // Re-established when `connect` returns: the initiator installs
        // the session inside `connect` before returning, so there is
        // nothing to poll for.
        let d_reconnect = t_reconnect.elapsed();

        let t = Instant::now();
        let reply = tokio::time::timeout(
            DEADLINE,
            caller.call(
                server2.node_id(),
                RPC_SERVICE,
                Bytes::from_static(b"after reconnect"),
                CallOptions::default(),
            ),
        )
        .await;
        let d_call = t.elapsed();

        match reply {
            Ok(Ok(_)) if i >= WARMUP => {
                reconnect.record(d_reconnect.as_nanos() as u64);
                call_leg.record(d_call.as_nanos() as u64);
            }
            Ok(Ok(_)) => {}
            _ => timeouts += 1,
        }
        drop((caller, server2, serve2_call, serve2_probe));
    }

    print_stage(
        &format!("{label} · drop → session re-established"),
        "drop(server) [+ ServeHandle]",
        "connect() returned (session installed)",
        &reconnect,
        WARMUP,
        timeouts,
    );
    print_stage(
        &format!("{label} · first nRPC after reconnect"),
        "call(...) invocation",
        "RpcReply returned (echo body)",
        &call_leg,
        WARMUP,
        timeouts,
    );
}
