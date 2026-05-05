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
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
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

    // Wide gap between primary and backup so the assertion has
    // generous slack across loaded CI / Windows scheduler noise:
    //   - primary sleeps 1500ms
    //   - backup is instant
    //   - hedge delay 50ms
    // Expected wall clock: ~50ms (hedge fire) + a few ms backup
    // round-trip, with up to several hundred ms scheduler /
    // network overhead. Asserting `< 1200ms` proves the backup
    // won (well under primary's 1500ms) without depending on
    // tight latency budgets that flake under load.
    const PRIMARY_SLEEP_MS: u64 = 1500;
    const HEDGE_DELAY_MS: u64 = 50;
    const HEDGE_MAX_WALL_CLOCK_MS: u64 = 1200;

    let _serve_primary = primary
        .serve_rpc(
            "lookup",
            Arc::new(DelayHandler {
                sleep_ms: PRIMARY_SLEEP_MS,
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
        delay: Duration::from_millis(HEDGE_DELAY_MS),
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
    // Body assertion above already proves the backup won. The
    // wall-clock assertion here just guards against the "primary
    // accidentally won" pathological case (e.g. a regression that
    // skipped the hedge delay or routed both calls to the same
    // target). 1200ms vs. primary's 1500ms gives 300ms of slack
    // — comfortable under loaded CI without trivializing the
    // assertion.
    assert!(
        elapsed < Duration::from_millis(HEDGE_MAX_WALL_CLOCK_MS),
        "wall-clock {elapsed:?} must be well under primary's {PRIMARY_SLEEP_MS}ms",
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
        async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
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

/// Hedge winner returns first → loser's future is dropped on the
/// caller side → loser's `UnaryCallGuard::Drop` fires CANCEL →
/// the slow primary's handler observes `ctx.cancellation`. Pins
/// the close of the documented "hedge cancellation gap" — losers
/// must actually free server-side resources, not just be silently
/// abandoned.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hedge_loser_handler_observes_cancellation() {
    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{
        RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Slow handler that explicitly observes cancellation.
    struct ObserveCancelHandler {
        cancelled: Arc<AtomicBool>,
    }
    #[async_trait]
    impl RpcHandler for ObserveCancelHandler {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            tokio::select! {
                _ = _ctx.cancellation.cancelled() => {
                    self.cancelled.store(true, Ordering::SeqCst);
                    Err(RpcHandlerError::Internal("cancelled by hedge".into()))
                }
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    Ok(RpcResponsePayload {
                        status: RpcStatus::Ok,
                        headers: vec![],
                        body: b"slow-finished".to_vec(),
                    })
                }
            }
        }
    }
    struct InstantHandler;
    #[async_trait]
    impl RpcHandler for InstantHandler {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: b"fast".to_vec(),
            })
        }
    }

    let psk = [0x42u8; 32];
    let caller = build_mesh(&psk).await;
    let primary = build_mesh(&psk).await;
    let backup = build_mesh(&psk).await;
    handshake(&caller, &primary).await;
    handshake(&caller, &backup).await;

    let observed = Arc::new(AtomicBool::new(false));
    let _serve_primary = primary
        .serve_rpc(
            "lookup",
            Arc::new(ObserveCancelHandler {
                cancelled: observed.clone(),
            }),
        )
        .expect("serve_rpc primary");
    let _serve_backup = backup
        .serve_rpc("lookup", Arc::new(InstantHandler))
        .expect("serve_rpc backup");

    let policy = HedgePolicy {
        delay: Duration::from_millis(50),
        hedges: 1,
    };

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
    assert_eq!(reply.body.as_ref(), b"fast", "backup must win");

    // Wait for the CANCEL to traverse and the primary's handler
    // to observe it. Generous because handshake-level RTTs vary
    // and the spawn-then-publish path adds a tokio scheduling hop.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !observed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        observed.load(Ordering::SeqCst),
        "primary's handler must observe ctx.cancellation after hedge winner returns",
    );
}

/// Regression for M19: when every hedge fails, the surfaced
/// error must be the PRIMARY's (`targets[0]`'s), not whichever
/// hedge happened to lose its race. The previous implementation
/// used `select_all`'s completion-order "last error wins" which
/// flipped depending on machine load — flake-prone diagnostics
/// in production logs.
///
/// We tag each server's error with a unique payload
/// (`primary-error` vs `backup-error`) and assert the surfaced
/// message contains the primary's tag across multiple runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hedge_all_failing_surfaces_primary_error_deterministically() {
    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload};
    /// Always returns `Internal(message)`. Used to make every
    /// hedge fail with a tagged diagnostic.
    struct AlwaysFail {
        message: &'static str,
        delay_ms: u64,
    }
    #[async_trait]
    impl RpcHandler for AlwaysFail {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            Err(RpcHandlerError::Internal(self.message.into()))
        }
    }

    let psk = [0x42u8; 32];
    let caller = build_mesh(&psk).await;
    let primary = build_mesh(&psk).await;
    let backup = build_mesh(&psk).await;
    handshake(&caller, &primary).await;
    handshake(&caller, &backup).await;

    // Primary is SLOWER than the backup so naive last-error-wins
    // would surface "backup-error" — proves the determinism fix
    // (we always prefer target_idx 0 when present).
    let _serve_primary = primary
        .serve_rpc(
            "lookup_fail",
            Arc::new(AlwaysFail {
                message: "primary-error",
                delay_ms: 100,
            }),
        )
        .expect("serve_rpc primary");
    let _serve_backup = backup
        .serve_rpc(
            "lookup_fail",
            Arc::new(AlwaysFail {
                message: "backup-error",
                delay_ms: 0,
            }),
        )
        .expect("serve_rpc backup");

    let policy = HedgePolicy {
        delay: Duration::from_millis(20),
        hedges: 1,
    };

    // Run 5 times to make a "completion order accidentally
    // matches primary order" coincidence fail-stop loud.
    for run in 0..5u32 {
        let err = caller
            .call_with_hedge_to(
                &[primary.inner().node_id(), backup.inner().node_id()],
                "lookup_fail",
                bytes::Bytes::from_static(b""),
                CallOptions::default(),
                &policy,
            )
            .await
            .expect_err("both servers always fail");
        match err {
            RpcError::ServerError { ref message, .. } => {
                assert!(
                    message.contains("primary-error"),
                    "run {run}: surfaced error must be primary's deterministically; \
                     got {message:?} (likely a regression to last-completer-wins)",
                );
            }
            other => panic!("run {run}: expected ServerError, got {other:?}"),
        }
    }
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
