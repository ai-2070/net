//! P4 — paid vs. unpaid nRPC delta (feature `mesh`). Apples-to-apples: the
//! SAME application surface both sides — `serve_tool` (unpaid) vs
//! `serve_tool_paid` (paid), identical request/response types, identical
//! handler body, same JSON codec and transport. The ONLY difference is the
//! payment gate, so
//!
//!     delta = paid_p50 − unpaid_p50
//!
//! is the **ready-settled payment-gate overhead**: the caller presents an
//! already-settled quote id and the provider runs `redeem_for_invocation`
//! (lock → load → redeemed check-and-set → whole-file persist). This is NOT
//! the full proof-present acceptance boundary (`accept_payment` +
//! `redeem_for_invocation`) — P2 owns that. Measured on a controlled
//! localhost mesh at a fixed 450-record store.
//!
//! Quotes are at-most-once and there is no proof-reuse API, so N distinct
//! settled quotes are **pre-minted in-process** (issue + accept on the
//! provider engine) OUTSIDE the timed region, then attached as the
//! `net-payment-quote` header (bearer; binding off). Store cardinality is
//! held per concurrency level (each paid call consumes one fresh quote).
//! Concurrency 1 / 16 / 128 over a warm two-node loopback mesh.
//!
//! Run: cargo bench -p net-payments --features mesh --bench mesh_paid_invoke

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::terms::PricingTerms;
use net_payments::flow::mesh::EngineToolPaymentGate;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::CallOptions;
use net_sdk::tool::{metadata_for, ToolServeHandle};
use net_sdk::tool_payment::HDR_PAYMENT_QUOTE;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

#[path = "bench_common/mod.rs"]
mod bench_common;

use bench_common::{
    build_engine, mint_settled, mock_requirements, new_hist, runtime, BenchMetadata, EngineFixture,
    Throughput,
};

const CONCURRENCY: &[usize] = &[1, 16, 128];
const PAID_TOOL: &str = "fixture-tool";
const FREE_TOOL: &str = "free-tool";

fn samples() -> usize {
    std::env::var("NET_PAY_BENCH_SAMPLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(200)
}

#[derive(JsonSchema, Deserialize, Serialize)]
struct EchoReq {
    message: String,
}

#[derive(JsonSchema, Deserialize, Serialize)]
struct EchoResp {
    echoed: String,
}

/// Build two loopback nodes, handshake them (concurrent accept + connect),
/// and start both — the same dance the SDK's nrpc bench harness uses.
async fn handshaken_pair() -> (Mesh, Mesh) {
    let psk = [0x42u8; 32];
    let server = MeshBuilder::new("127.0.0.1:0", &psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let caller = MeshBuilder::new("127.0.0.1:0", &psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let server_addr = server.local_addr().to_string();
    let server_pub = *server.public_key();
    let server_id = server.node_id();
    let caller_id = caller.node_id();
    let (accept, connect) = tokio::join!(server.accept(caller_id), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller.connect(&server_addr, &server_pub, server_id).await
    });
    accept.expect("accept");
    connect.expect("connect");
    server.start();
    caller.start();
    (server, caller)
}

/// Serve the SAME echo handler twice on `provider`: unpaid via `serve_tool`
/// and paid via `serve_tool_paid` (gated by the fixture's engine). Returns the
/// serve handles (kept alive for the run).
fn serve_both(provider: &Mesh, fx: &EngineFixture) -> (ToolServeHandle, ToolServeHandle) {
    // Unpaid: no pricing terms (serve_tool refuses a priced descriptor).
    let free = metadata_for::<EchoReq, EchoResp>(FREE_TOOL)
        .description("Echo, free.")
        .build();
    let free_h = provider
        .serve_tool::<EchoReq, EchoResp, _, _>(free, |req: EchoReq| async move {
            Ok::<_, String>(EchoResp {
                echoed: req.message,
            })
        })
        .expect("serve free tool");

    // Paid: identical handler, plus a priced descriptor + the engine gate.
    let template = mock_requirements("2500");
    let terms = PricingTerms::new(
        fx.provider.entity_id().clone(),
        format!("{}/{PAID_TOOL}", provider.node_id()),
        vec![template],
        fx.registry.reference().expect("registry reference"),
    );
    let announced =
        String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8");
    let gate = Arc::new(EngineToolPaymentGate::new(fx.engine.clone()));
    let paid = metadata_for::<EchoReq, EchoResp>(PAID_TOOL)
        .description("Echo, for money.")
        .pricing_terms(&announced)
        .build();
    let paid_h = provider
        .serve_tool_paid::<EchoReq, EchoResp, _, _>(paid, gate, |req: EchoReq| async move {
            Ok::<_, String>(EchoResp {
                echoed: req.message,
            })
        })
        .expect("serve paid tool");
    (free_h, paid_h)
}

/// Pre-mint `n` distinct settled quotes on the provider engine, returning
/// their ids. Each is single-use; `idx` advances so ids never collide.
async fn mint_batch(fx: &EngineFixture, idx: &mut u64, n: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        ids.push(mint_settled(fx, *idx).await.quote_id);
        *idx += 1;
    }
    ids
}

/// Time `count` calls to `service` at concurrency `conc`. When `quote_ids` is
/// Some, each call attaches its own quote as the payment header (paid path);
/// None is the unpaid path (no headers). Returns the histogram + attempts/s.
async fn run_calls(
    caller: Arc<Mesh>,
    provider_id: u64,
    service: &'static str,
    quote_ids: Option<Arc<Vec<String>>>,
    conc: usize,
    count: usize,
) -> (hdrhistogram::Histogram<u64>, f64) {
    let sem = Arc::new(Semaphore::new(conc));
    let mut handles = Vec::with_capacity(count);
    let start = Instant::now();
    for i in 0..count {
        let permit = Arc::clone(&sem).acquire_owned().await.unwrap();
        let caller = Arc::clone(&caller);
        let headers = match &quote_ids {
            Some(q) => vec![(HDR_PAYMENT_QUOTE.to_string(), q[i].clone().into_bytes())],
            None => vec![],
        };
        handles.push(tokio::spawn(async move {
            let body = serde_json::to_vec(&EchoReq {
                message: "hi".into(),
            })
            .unwrap();
            let opts = CallOptions {
                request_headers: headers,
                ..CallOptions::default()
            };
            let t = Instant::now();
            let reply = caller
                .call(provider_id, service, Bytes::from(body), opts)
                .await
                .expect("call");
            let elapsed = t.elapsed().as_nanos() as u64;
            drop(permit);
            std::hint::black_box(reply);
            elapsed
        }));
    }
    let mut hist = new_hist();
    for h in handles {
        hist.record(h.await.expect("join")).expect("record");
    }
    (hist, count as f64 / start.elapsed().as_secs_f64())
}

fn main() {
    let rt = runtime();
    let n = samples();
    let fx = build_engine();

    // Build the mesh AND register the tools inside the runtime (serve_* spawns
    // bridge tasks, so it needs a reactor in context).
    let (provider, caller, _handles) = rt.block_on(async {
        let (provider, caller) = handshaken_pair().await;
        let handles = serve_both(&provider, &fx);
        (provider, caller, handles)
    });
    let provider_id = provider.node_id();
    let caller = Arc::new(caller);
    let mut idx = 1_000u64;

    // Warm-up: install the lazy metadata handlers + prime the per-caller reply
    // subscription so the first measured call isn't disproportionately slow.
    rt.block_on(async {
        let warm_ids = mint_batch(&fx, &mut idx, 8).await;
        for id in &warm_ids {
            let _ = run_calls(
                Arc::clone(&caller),
                provider_id,
                PAID_TOOL,
                Some(Arc::new(vec![id.clone()])),
                1,
                1,
            )
            .await;
        }
        let _ = run_calls(Arc::clone(&caller), provider_id, FREE_TOOL, None, 1, 8).await;
    });

    // Fixed cardinality: pre-mint EVERY quote up front (redeem adds no
    // record), so the engine store stays constant across all paid rows —
    // store size is not a free variable of concurrency (see P2 for its
    // size-scaling). Each level consumes its own disjoint slice.
    let total = n * CONCURRENCY.len();
    let all_ids = rt.block_on(mint_batch(&fx, &mut idx, total));
    let base = BenchMetadata::base(&fx, Duration::ZERO); // records_before == total (held)

    println!("mesh_paid_invoke — samples={n}, controlled localhost mesh, binding=off (bearer)");
    println!(
        "fixed store cardinality={total} records; delta = ready-settled payment-gate overhead"
    );

    for (level, &conc) in CONCURRENCY.iter().enumerate() {
        // Unpaid — the shared baseline (touches no engine).
        let (uh, ut) = rt.block_on(run_calls(
            Arc::clone(&caller),
            provider_id,
            FREE_TOOL,
            None,
            conc,
            n,
        ));
        base.for_row(format!("unpaid c{conc}"), n, conc, false, &fx)
            .report(&uh, &Throughput::uniform(ut));

        // Paid — this level's disjoint slice of pre-minted quotes.
        let ids = Arc::new(all_ids[level * n..(level + 1) * n].to_vec());
        let (ph, pt) = rt.block_on(run_calls(
            Arc::clone(&caller),
            provider_id,
            PAID_TOOL,
            Some(ids),
            conc,
            n,
        ));
        base.for_row(format!("paid c{conc}"), n, conc, false, &fx)
            .report(&ph, &Throughput::uniform(pt));

        let delta_us =
            (ph.value_at_quantile(0.50) as f64 - uh.value_at_quantile(0.50) as f64) / 1000.0;
        println!("      >> DELTA c{conc}: paid_p50 - unpaid_p50 = {delta_us:.2}us (ready-settled redemption tax)");
    }
}
