//! End-to-end SDK test for the circuit breaker.
//!
//! Builds a real mesh + handshake and pins:
//!
//! - **Trip after threshold consecutive failures** — Closed → Open
//!   transition.
//! - **Open short-circuits without invoking the handler** — calls
//!   during cooldown return `BreakerError::Open` and the server's
//!   handler doesn't run.
//! - **Half-open probe → Closed on success** — after `reset_after`,
//!   the next call probes; on success the breaker closes and
//!   subsequent calls flow normally.
//! - **Half-open probe → Open on failure** — bad probe re-opens
//!   with a fresh cooldown.
//! - **Application errors don't trip** — the default predicate
//!   only counts transient infrastructure failures; a typed
//!   handler `Err(String)` (= `Application(0x4001)`) leaves the
//!   counters at zero.
//! - **`reset()` operator override** clears state.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{Codec, RpcError};
use net_sdk::mesh_rpc_resilience::{
    BreakerError, BreakerState, CircuitBreaker, CircuitBreakerConfig,
};
use serde::{Deserialize, Serialize};

async fn two_meshes(psk: &[u8; 32]) -> (Mesh, Mesh, std::net::SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap();
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

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
struct Ping(u64);

/// A handler whose behavior is steered by an `AtomicBool` flag.
/// When `fail` is true, returns `Internal`; when false, echoes
/// the request body. Lets us simulate "downstream goes bad, then
/// recovers" inside one test.
fn make_flaky_server(
    server: &Mesh,
    flag: Arc<AtomicBool>,
    invocations: Arc<AtomicUsize>,
) -> net_sdk::mesh_rpc::ServeHandle {
    use async_trait::async_trait;
    use net_sdk::mesh_rpc::{
        RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
    };
    struct Flaky {
        fail: Arc<AtomicBool>,
        count: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl RpcHandler for Flaky {
        async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                Err(RpcHandlerError::Internal(
                    "simulated downstream failure".into(),
                ))
            } else {
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: ctx.payload.body,
                })
            }
        }
    }
    server
        .serve_rpc(
            "flaky",
            Arc::new(Flaky {
                fail: flag,
                count: invocations,
            }),
        )
        .expect("serve_rpc")
}

/// Threshold + open + half-open probe + close — full happy
/// state-machine cycle in one test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaker_full_state_machine_cycle() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let fail_flag = Arc::new(AtomicBool::new(true));
    let invocations = Arc::new(AtomicUsize::new(0));
    let _serve = make_flaky_server(&server, fail_flag.clone(), invocations.clone());

    let breaker = CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 3,
        success_threshold: 1,
        reset_after: Duration::from_millis(200),
        ..Default::default()
    });

    let target = server.inner().node_id();
    let do_call = || async {
        caller
            .call(
                target,
                "flaky",
                bytes::Bytes::from_static(b"ping"),
                Default::default(),
            )
            .await
    };

    // 1) Three consecutive Internal failures trip Closed → Open.
    for _ in 0..3 {
        let err = breaker
            .call(do_call)
            .await
            .expect_err("must surface Internal");
        assert!(matches!(
            err,
            BreakerError::Inner(RpcError::ServerError { .. })
        ));
    }
    assert_eq!(
        breaker.state(),
        BreakerState::Open,
        "must trip after threshold"
    );

    // 2) During cooldown, calls short-circuit with Open and the
    // handler is NOT invoked.
    let invocations_before = invocations.load(Ordering::SeqCst);
    for _ in 0..5 {
        let err = breaker.call(do_call).await.expect_err("must short-circuit");
        assert!(matches!(err, BreakerError::Open));
    }
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        invocations_before,
        "Open must not invoke handler",
    );

    // 3) Recover: flip the server to Ok, wait out cooldown, next
    // call is the half-open probe; on success → Closed.
    fail_flag.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(220)).await;

    let reply = breaker
        .call(do_call)
        .await
        .expect("half-open probe must succeed");
    assert!(matches!(reply, net_sdk::mesh_rpc::RpcReply { .. }));
    assert_eq!(reply.body.as_ref(), b"ping");
    assert_eq!(
        breaker.state(),
        BreakerState::Closed,
        "probe success closes breaker"
    );

    // 4) Subsequent calls flow normally (counters reset).
    for _ in 0..3 {
        let _ = breaker.call(do_call).await.expect("must succeed");
    }
    assert_eq!(breaker.consecutive_failures(), 0);
}

/// Half-open probe FAILS → breaker re-opens with a fresh cooldown
/// (not transition to Closed). Tests the unhappy half-open path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaker_failed_half_open_probe_reopens() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let fail_flag = Arc::new(AtomicBool::new(true));
    let invocations = Arc::new(AtomicUsize::new(0));
    let _serve = make_flaky_server(&server, fail_flag.clone(), invocations.clone());

    let breaker = CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 1,
        reset_after: Duration::from_millis(150),
        ..Default::default()
    });
    let target = server.inner().node_id();
    let do_call = || async {
        caller
            .call(
                target,
                "flaky",
                bytes::Bytes::from_static(b""),
                Default::default(),
            )
            .await
    };

    // Trip.
    for _ in 0..2 {
        let _ = breaker.call(do_call).await;
    }
    assert_eq!(breaker.state(), BreakerState::Open);

    // Wait out cooldown — flag is still failing → half-open
    // probe will fail and re-open.
    tokio::time::sleep(Duration::from_millis(180)).await;
    let err = breaker
        .call(do_call)
        .await
        .expect_err("probe still failing");
    assert!(matches!(err, BreakerError::Inner(_)));
    assert_eq!(
        breaker.state(),
        BreakerState::Open,
        "failed probe must reopen, not close",
    );

    // Confirm the cooldown was reset (no immediate re-probe).
    let err = breaker.call(do_call).await.expect_err("still cooling");
    assert!(matches!(err, BreakerError::Open));
}

/// Application errors (typed `Err(String)` = `Application(0x4001)`)
/// do NOT trip the breaker per the default predicate. Pin: counter
/// stays at zero, breaker stays Closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaker_application_errors_do_not_trip() {
    let psk = [0x42u8; 32];
    let (caller, server, addr_server) = two_meshes(&psk).await;
    handshake(&caller, &server, addr_server).await;

    let _serve = server
        .serve_rpc_typed("validate", Codec::Json, |req: Ping| async move {
            Err::<Ping, _>(format!("rejected {}", req.0))
        })
        .expect("serve_rpc_typed");

    let breaker = CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 2,
        ..Default::default()
    });
    let target = server.inner().node_id();

    // 5 application errors in a row.
    for i in 0..5u64 {
        let err = breaker
            .call(|| async {
                let body = serde_json::to_vec(&Ping(i)).unwrap();
                caller
                    .call(
                        target,
                        "validate",
                        bytes::Bytes::from(body),
                        Default::default(),
                    )
                    .await
            })
            .await
            .expect_err("validation failure");
        assert!(matches!(
            err,
            BreakerError::Inner(RpcError::ServerError { .. })
        ));
    }
    // Breaker stays Closed; counter stays at 0.
    assert_eq!(breaker.state(), BreakerState::Closed);
    assert_eq!(breaker.consecutive_failures(), 0);
}

/// `reset()` is the operator override — flips state back to Closed
/// and zeroes counters regardless of where the breaker was.
#[test]
fn breaker_reset_clears_state() {
    let breaker = CircuitBreaker::new(CircuitBreakerConfig::default());

    // Drive the breaker into a synthetic Open state by simulating
    // failures via direct calls to the `call` method with a
    // closure that returns a transient error.
    let body = async move {
        for _ in 0..10 {
            let _ = breaker
                .call(|| async { Err::<(), _>(RpcError::Timeout { elapsed_ms: 10 }) })
                .await;
        }
        assert_eq!(breaker.state(), BreakerState::Open);
        breaker.reset();
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert_eq!(breaker.consecutive_failures(), 0);
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(body);
}

/// Regression: a panic inside a HalfOpen probe future must NOT
/// leak `probe_in_flight=true` (which would wedge the breaker
/// HalfOpen forever — every subsequent call short-circuits with
/// `Open`, recoverable only via `reset()`). The RAII probe guard
/// catches this and re-opens the breaker with a fresh cooldown,
/// the same semantic as a probe that returned a counted error.
#[test]
fn breaker_half_open_panic_reopens_instead_of_wedging() {
    let body = async move {
        // Tight cooldown so we can drive Closed → Open → HalfOpen
        // quickly. failure_threshold=1 trips on the first failure.
        let cfg = CircuitBreakerConfig {
            failure_threshold: 1,
            reset_after: Duration::from_millis(10),
            success_threshold: 1,
            failure_predicate: Arc::new(net_sdk::mesh_rpc_resilience::default_breaker_failure),
        };
        let breaker = Arc::new(CircuitBreaker::new(cfg));

        // Trip into Open.
        let _ = breaker
            .call(|| async { Err::<(), _>(RpcError::Timeout { elapsed_ms: 5 }) })
            .await;
        assert_eq!(breaker.state(), BreakerState::Open);

        // Wait out cooldown so the next call probes.
        tokio::time::sleep(Duration::from_millis(25)).await;

        // Probe panics. Catch the unwind so the test binary doesn't
        // abort; the breaker's RAII guard must clear `probe_in_flight`
        // and re-open the breaker.
        let breaker_for_probe = breaker.clone();
        let probe_result = tokio::spawn(async move {
            breaker_for_probe
                .call(|| async {
                    panic!("simulated handler panic during HalfOpen probe");
                    #[allow(unreachable_code)]
                    Ok::<(), _>(())
                })
                .await
        })
        .await;
        assert!(
            probe_result.is_err(),
            "panic must propagate out of breaker.call (so the caller observes the failure)"
        );

        // Wait for the (presumed-still-Open) cooldown to elapse so
        // the next call probes again. If the breaker was wedged with
        // `probe_in_flight=true`, this call would be rejected with
        // `BreakerError::Open` immediately — the RAII guard prevents
        // that.
        tokio::time::sleep(Duration::from_millis(25)).await;

        // Next call must successfully reach the inner closure (i.e.
        // be admitted as a probe).
        let invoked = Arc::new(AtomicBool::new(false));
        let invoked_inner = invoked.clone();
        let result = breaker
            .call(|| async {
                invoked_inner.store(true, Ordering::SeqCst);
                Ok::<u32, _>(42)
            })
            .await;
        assert!(
            invoked.load(Ordering::SeqCst),
            "after a panic during a probe, the breaker must accept a fresh probe — got {result:?}",
        );
        assert!(matches!(result, Ok(42)));
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(body);
}

/// `BreakerError::into_rpc_error` flattens the breaker distinction
/// for callers who don't care about it.
#[test]
fn breaker_error_flatten() {
    let open: RpcError = BreakerError::Open.into_rpc_error();
    assert!(matches!(open, RpcError::NoRoute { .. }));
    let inner: RpcError =
        BreakerError::Inner(RpcError::Timeout { elapsed_ms: 99 }).into_rpc_error();
    assert!(matches!(inner, RpcError::Timeout { elapsed_ms: 99 }));
}
