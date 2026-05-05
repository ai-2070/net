//! End-to-end SDK test for the hedge helper.
//!
//! Builds a 3-node mesh (caller + 2 servers), serves the same
//! RPC on both servers with asymmetric latency (primary slow,
//! backup fast), and pins:
//!
//! - **Backup wins under slow primary** — `call_with_hedge_to`
//!   fires the primary, waits `delay`, fires the backup,
//!   returns the backup's body BEFORE the primary's wall-clock
//!   completion.
//! - **No-hedge degeneracy** — `hedges = 0` falls back to a
//!   single straight call to the first target.
//! - **Empty targets** — `RpcError::NoRoute`.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptions, RpcError};
use net_sdk::mesh_rpc_resilience::HedgePolicy;

async fn build_mesh(psk: &[u8; 32]) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn handshake(a: &Mesh, b: &Mesh) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let addr_b = b.inner().local_addr();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

/// Primary sleeps 800ms before responding. Backup is instant. With
/// 50ms hedge delay, the wrapper should fire the backup and the
/// backup's body should win — well before the primary's 800ms
/// completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hedge_backup_wins_when_primary_is_slow() {
    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{
        RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    };

    struct DelayHandler {
        sleep_ms: u64,
        body: &'static [u8],
    }
    #[async_trait]
    impl RpcHandler for DelayHandler {
        async fn call(
            &self,
            _ctx: RpcContext,
        ) -> Result<RpcResponsePayload, RpcHandlerError> {
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: self.body.to_vec(),
            })
        }
    }

    let psk = [0x42u8; 32];
    let caller = build_mesh(&psk).await;
    let primary = build_mesh(&psk).await;
    let backup = build_mesh(&psk).await;
    handshake(&caller, &primary).await;
    handshake(&caller, &backup).await;

    let _serve_primary = primary
        .serve_rpc(
            "lookup",
            Arc::new(DelayHandler {
                sleep_ms: 800,
                body: b"slow-primary",
            }),
        )
        .expect("serve_rpc primary");
    let _serve_backup = backup
        .serve_rpc(
            "lookup",
            Arc::new(DelayHandler {
                sleep_ms: 0,
                body: b"fast-backup",
            }),
        )
        .expect("serve_rpc backup");

    let policy = HedgePolicy {
        delay: Duration::from_millis(50),
        hedges: 1,
    };

    let started = Instant::now();
    let reply = caller
        .call_with_hedge_to(
            &[primary.inner().node_id(), backup.inner().node_id()],
            "lookup",
            bytes::Bytes::from_static(b""),
            CallOptions::default(),
            &policy,
        )
        .await
        .expect("hedge must succeed");
    let elapsed = started.elapsed();

    assert_eq!(
        reply.body.as_ref(),
        b"fast-backup",
        "backup must win the race (got {:?})",
        std::str::from_utf8(reply.body.as_ref()).unwrap_or("<non-utf8>"),
    );
    // Wall-clock should be ~hedge delay + backup latency, well
    // under the primary's 800ms.
    assert!(
        elapsed < Duration::from_millis(600),
        "wall-clock {elapsed:?} must be much less than primary's 800ms",
    );
}

/// `hedges = 0` is a degenerate config — the wrapper degrades to
/// a single straight call against `targets[0]`. Pin: no panic, no
/// timeout, primary's reply round-trips.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hedge_zero_degrades_to_single_call() {
    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{
        RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    };
    struct EchoHandler;
    #[async_trait]
    impl RpcHandler for EchoHandler {
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
    let caller = build_mesh(&psk).await;
    let server = build_mesh(&psk).await;
    handshake(&caller, &server).await;

    let _serve = server
        .serve_rpc("echo", Arc::new(EchoHandler))
        .expect("serve_rpc");

    let policy = HedgePolicy {
        delay: Duration::from_millis(50),
        hedges: 0,
    };
    let reply = caller
        .call_with_hedge_to(
            &[server.inner().node_id(), 0xdead_beef_dead_beef], // second is unreachable
            "echo",
            bytes::Bytes::from_static(b"hello-no-hedge"),
            CallOptions::default(),
            &policy,
        )
        .await
        .expect("hedge=0 must succeed via primary");
    assert_eq!(reply.body.as_ref(), b"hello-no-hedge");
}

/// Empty targets → immediate `RpcError::NoRoute`. Pin: no panic,
/// no hang, error surfaces with a diagnostic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hedge_empty_targets_returns_no_route() {
    let psk = [0x42u8; 32];
    let caller = build_mesh(&psk).await;
    let policy = HedgePolicy::default();
    let err = caller
        .call_with_hedge_to(
            &[],
            "anything",
            bytes::Bytes::from_static(b""),
            CallOptions::default(),
            &policy,
        )
        .await
        .expect_err("empty targets must error");
    match err {
        RpcError::NoRoute { reason, .. } => {
            assert!(
                reason.contains("empty"),
                "diagnostic must mention empty, got {reason:?}",
            );
        }
        other => panic!("expected NoRoute, got {other:?}"),
    }
}
