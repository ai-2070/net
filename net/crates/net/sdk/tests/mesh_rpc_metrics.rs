//! End-to-end SDK test for caller-side nRPC metrics.
//!
//! Mixed-outcome calls populate the snapshot; Prometheus text
//! output contains canonical metric names with the right label
//! values. Pinned: each error variant maps to the corresponding
//! `errors_*` counter, latency observations land in the bucket
//! histogram, and `in_flight` is balanced after a call resolves.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::CallOptions;

async fn two_meshes(psk: &[u8; 32]) -> (Mesh, Mesh, std::net::SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", psk).unwrap().build().await.unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", psk).unwrap().build().await.unwrap();
    let addr_b = b.inner().local_addr();
    (a, b, addr_b)
}

async fn handshake(a: &Mesh, b: &Mesh, addr_b: std::net::SocketAddr) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

/// Three successful calls + one server-error + one timeout →
/// snapshot shows 5 calls_total, 1 errors_server, 1 errors_timeout,
/// 0 in_flight, and the latency histogram has 5 observations.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metrics_snapshot_reflects_mixed_outcomes() {
    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{
        RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    };

    /// Echo with a "fail next" toggle — flips on every call so we
    /// can deterministically alternate Ok/Err shapes from a single
    /// handler.
    struct ToggleHandler {
        fail_after_invocation: usize,
        slow_after_invocation: usize,
        invocation_count: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl RpcHandler for ToggleHandler {
        async fn call(
            &self,
            ctx: RpcContext,
        ) -> Result<RpcResponsePayload, RpcHandlerError> {
            let n = self
                .invocation_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == self.slow_after_invocation {
                // Sleep past the caller's deadline.
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            if n == self.fail_after_invocation {
                return Err(RpcHandlerError::Internal("boom".into()));
            }
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: ctx.payload.body,
            })
        }
    }

    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc(
            "echo_metrics",
            Arc::new(ToggleHandler {
                fail_after_invocation: 3,
                slow_after_invocation: 4,
                invocation_count: std::sync::atomic::AtomicUsize::new(0),
            }),
        )
        .expect("serve_rpc");

    let target = server.inner().node_id();

    // 3 successes.
    for _ in 0..3 {
        caller
            .call(target, "echo_metrics", bytes::Bytes::from_static(b"hi"), CallOptions::default())
            .await
            .expect("ok");
    }
    // 1 server-error.
    let err = caller
        .call(target, "echo_metrics", bytes::Bytes::from_static(b""), CallOptions::default())
        .await
        .expect_err("must fail");
    match err {
        net_sdk::mesh_rpc::RpcError::ServerError { .. } => {}
        other => panic!("expected ServerError, got {other:?}"),
    }
    // 1 timeout.
    let timeout_err = caller
        .call(
            target,
            "echo_metrics",
            bytes::Bytes::from_static(b""),
            CallOptions {
                deadline: Some(Instant::now() + Duration::from_millis(200)),
                ..Default::default()
            },
        )
        .await
        .expect_err("must time out");
    assert!(matches!(
        timeout_err,
        net_sdk::mesh_rpc::RpcError::Timeout { .. }
    ));

    let snap = caller.rpc_metrics_snapshot();
    let echo = snap
        .services
        .iter()
        .find(|s| s.service == "echo_metrics")
        .expect("service must appear in snapshot");

    assert_eq!(echo.calls_total, 5, "all 5 calls resolved");
    assert_eq!(echo.errors_server, 1, "exactly one ServerError");
    assert_eq!(echo.errors_timeout, 1, "exactly one Timeout");
    assert_eq!(echo.errors_no_route, 0);
    assert_eq!(echo.errors_transport, 0);
    assert_eq!(echo.in_flight, 0, "all calls resolved → in_flight back to 0");
    assert_eq!(
        echo.latency_count, 5,
        "every resolved call records one latency observation",
    );
    // The +Inf bucket must equal the count.
    assert_eq!(
        *echo.latency_buckets.last().unwrap(),
        echo.latency_count,
        "Prometheus +Inf bucket convention",
    );
    assert!(echo.latency_sum_ns > 0, "latency_sum should accumulate");
}

/// Prometheus output contains canonical metric names + the
/// service label with our value. Snapshot format is
/// `text/plain; version=0.0.4` compatible.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metrics_prometheus_text_contains_canonical_names() {
    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{
        RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    };

    struct Always;
    #[async_trait]
    impl RpcHandler for Always {
        async fn call(
            &self,
            ctx: RpcContext,
        ) -> Result<RpcResponsePayload, RpcHandlerError> {
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: ctx.payload.body,
            })
        }
    }

    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc("prom_test", Arc::new(Always))
        .expect("serve_rpc");

    let target = server.inner().node_id();
    for _ in 0..2 {
        caller
            .call(target, "prom_test", bytes::Bytes::from_static(b""), CallOptions::default())
            .await
            .expect("ok");
    }

    let text = caller.rpc_metrics_snapshot().prometheus_text();
    // Canonical metric names.
    for name in &[
        "nrpc_calls_total",
        "nrpc_errors_total",
        "nrpc_in_flight_calls",
        "nrpc_call_latency_seconds_bucket",
        "nrpc_call_latency_seconds_sum",
        "nrpc_call_latency_seconds_count",
    ] {
        assert!(
            text.contains(name),
            "Prometheus text missing metric {name}\n----\n{text}",
        );
    }
    // Our service label appears.
    assert!(text.contains("service=\"prom_test\""), "service label missing");
    // The +Inf bucket terminates the histogram.
    assert!(text.contains("le=\"+Inf\""), "+Inf bucket missing");
    // calls_total reflects both calls.
    assert!(
        text.contains("nrpc_calls_total{service=\"prom_test\"} 2"),
        "calls_total must show 2",
    );
}
