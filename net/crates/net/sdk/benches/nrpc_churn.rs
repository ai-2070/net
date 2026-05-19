//! Item 5 — connection / handshake churn. Custom main +
//! hdrhistogram for the same reason as `nrpc_tail.rs`:
//! Criterion's sampling model throws away the per-iteration tail
//! we want to characterize.
//!
//! For each of `PAIRS` iterations:
//! 1. Build two `Mesh` instances on `127.0.0.1:0`.
//! 2. Run accept + connect concurrently (handshake).
//! 3. Issue one direct `call_typed` (first-RPC latency).
//! 4. Drop both meshes — sockets release immediately.
//!
//! Phase timings are recorded in three histograms:
//!   - build_pair: pair-creation latency
//!   - handshake:  accept/connect duration
//!   - first_rpc:  latency of the first call after start
//!
//! `secure_channels` toggle: the SDK does not expose a runtime
//! insecure-channel mode (PSK + framed transport is always on),
//! so this bench reports a single security profile rather than
//! the secure/insecure pair. If a debug-only insecure transport
//! is later added, this is where the second axis lands.
//!
//! 1000 pairs = 2000 Mesh instances + ~2000 UDP sockets. UDP has
//! no TIME_WAIT — closed sockets free immediately — so this
//! stays well under any reasonable ephemeral-port range.
//!
//! Run with:
//!   cargo bench --bench nrpc_churn --features net,cortex -p net-mesh-sdk

use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use net_sdk::capabilities::CapabilitySet;
use net_sdk::mesh::MeshBuilder;
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec};

#[path = "nrpc_common/mod.rs"]
mod nrpc_common;

use nrpc_common::{payload, runtime, EchoReq, EchoResp};

const PAIRS: usize = 1_000;
const SVC: &str = "churn_echo";

fn main() {
    let rt = runtime();

    let mut h_build = new_hist();
    let mut h_handshake = new_hist();
    let mut h_first = new_hist();

    let req = EchoReq { body: payload(32) };
    let psk = [0x42u8; 32];

    rt.block_on(async {
        for i in 0..PAIRS {
            // ---- Phase 1: build ----
            let t0 = Instant::now();
            let server = MeshBuilder::new("127.0.0.1:0", &psk)
                .expect("builder")
                .build()
                .await
                .expect("server build");
            let caller = MeshBuilder::new("127.0.0.1:0", &psk)
                .expect("builder")
                .build()
                .await
                .expect("caller build");
            h_build
                .record(t0.elapsed().as_nanos() as u64)
                .expect("record build");

            // ---- Phase 2: handshake ----
            let server_addr = server.local_addr().to_string();
            let server_pub = *server.public_key();
            let server_id = server.node_id();
            let caller_id = caller.node_id();
            let t1 = Instant::now();
            let (accept_res, connect_res) = tokio::join!(server.accept(caller_id), async {
                // Same 50 ms pre-connect breather as nrpc_echo.rs:81 —
                // ensures the accept is parked on the receive
                // loop before the connect arrives.
                tokio::time::sleep(Duration::from_millis(50)).await;
                caller.connect(&server_addr, &server_pub, server_id).await
            });
            accept_res.expect("accept");
            connect_res.expect("connect");
            server.start();
            caller.start();
            // Subtract the 50ms breather so the recorded
            // handshake reflects actual handshake work, not the
            // artificial gap.
            let handshake_ns = t1
                .elapsed()
                .saturating_sub(Duration::from_millis(50))
                .as_nanos() as u64;
            h_handshake
                .record(handshake_ns.max(1))
                .expect("record handshake");

            // Need a service to call; register inline (handle
            // lives until end of loop iter).
            let _handle = server
                .serve_rpc_typed(SVC, Codec::Json, |req: EchoReq| async move {
                    Ok::<_, String>(EchoResp { body: req.body })
                })
                .expect("serve");
            // Discovery isn't needed: we're using direct routing.
            server
                .inner()
                .announce_capabilities(CapabilitySet::new())
                .await
                .expect("announce");

            // ---- Phase 3: first RPC ----
            let t2 = Instant::now();
            let resp: EchoResp = caller
                .call_typed(server_id, SVC, &req, CallOptionsTyped::default())
                .await
                .expect("first rpc");
            h_first
                .record(t2.elapsed().as_nanos() as u64)
                .expect("record first");
            std::hint::black_box(resp);

            if (i + 1) % 100 == 0 {
                eprintln!("  ... {} / {} pairs", i + 1, PAIRS);
            }
            // server + caller drop here; sockets release.
        }
    });

    println!("nrpc_churn — pairs={PAIRS}, codec=json, routing=direct");
    println!(
        "  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}",
        "phase", "p50_us", "p95_us", "p99_us", "p99.9_us", "max_us"
    );
    print_row("build_pair", &h_build);
    print_row("handshake", &h_handshake);
    print_row("first_rpc", &h_first);
}

fn new_hist() -> Histogram<u64> {
    Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).expect("hdrhistogram alloc")
}

fn print_row(label: &str, hist: &Histogram<u64>) {
    let to_us = |v: u64| v as f64 / 1_000.0;
    println!(
        "  {:>10}  {:>10.2}  {:>10.2}  {:>10.2}  {:>10.2}  {:>10.2}  (mean {:.2} us)",
        label,
        to_us(hist.value_at_quantile(0.50)),
        to_us(hist.value_at_quantile(0.95)),
        to_us(hist.value_at_quantile(0.99)),
        to_us(hist.value_at_quantile(0.999)),
        to_us(hist.max()),
        hist.mean() / 1_000.0,
    );
}
