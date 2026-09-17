//! A2A control calls are **bounded**: in size before they are sent, and
//! in time once they are.
//!
//! Both properties exist because of the same observed defect, and
//! neither is hypothetical.
//!
//! The A2A wire carries its payload inside a JSON array-of-bytes
//! envelope, which costs up to four bytes per payload byte, and one mesh
//! packet carries ~8 KiB. A brief whose *encoded* form passed ~2 KB
//! therefore quadrupled past the packet budget and was **never
//! delivered** — and because `CallOptions::deadline` defaults to `None`,
//! the caller waited for a reply that could not arrive. Measured on the
//! real wire before the fix: a 1800-byte prompt acked, a 2000-byte
//! prompt hung forever.
//!
//! Two distinct defects hid behind that one symptom, so each has its own
//! witnesses here:
//!
//! 1. **no size check** — an undeliverable request left the caller with
//!    silence indistinguishable from an absent peer. Now
//!    [`A2aFlowError::BriefTooLarge`], refused locally with no packet
//!    sent, and a service may not even *announce* a `max_prompt_bytes`
//!    the wire cannot honor.
//! 2. **no deadline** — any lost request parked the caller permanently.
//!    Now [`A2aFlowError::Timeout`], which says *unknown* rather than
//!    *failed*, and every A2A verb is idempotent so it is safe to retry.
//!
//! The size limit is a constant mirrored from core's private protocol
//! numbers, so `a_brief_at_the_wire_limit_round_trips` deliberately
//! sends a brief of *exactly* the limit over a real two-node wire: if the
//! packet budget ever shrinks, that witness fails instead of vanished
//! requests coming back.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use net_sdk::a2a::{
    A2aBounds, A2aOffer, CancelToken, TaskBrief, TaskExecutor, TaskRegistry, TaskState,
};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_a2a::{
    A2aFlowError, A2aServiceConfig, A2aServicePolicy, A2A_CALL_TIMEOUT, A2A_MAX_BRIEF_BYTES,
    A2A_STATUS_SERVICE,
};
use net_sdk::mesh_rpc::{RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, ServeError};

const PSK: [u8; 32] = [0x5Au8; 32];

async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .build()
        .await
        .expect("build")
}

/// Handshake every caller to `executor` while all nodes are still
/// unstarted, then start the dispatch loops — a started node's receive
/// loop auto-accepts and races the responder handshake.
async fn connect_all(executor: &Mesh, callers: &[&Mesh]) {
    let addr = executor.inner().local_addr();
    let pubkey = *executor.inner().public_key();
    let nid_exec = executor.inner().node_id();
    for caller in callers {
        let nid_caller = caller.inner().node_id();
        let (accepted, connected) = tokio::join!(executor.inner().accept(nid_caller), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            caller.inner().connect(addr, &pubkey, nid_exec).await
        });
        accepted.expect("accept");
        connected.expect("connect");
    }
    for caller in callers {
        caller.inner().start();
    }
    executor.inner().start();
}

/// Completes immediately, counting the briefs it was actually handed —
/// so "nothing was sent" is observed on the far side, not inferred.
struct Counting {
    runs: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl TaskExecutor for Counting {
    async fn run(&self, _brief: TaskBrief, _cancel: CancelToken) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok("artifact:done".to_string())
    }
}

/// A brief whose `encode()` is exactly `target` bytes.
///
/// Built by measuring the envelope rather than guessing at it: the
/// prompt is padded until the encoded length lands on `target`, so the
/// witness is about the wire limit and not about my arithmetic for the
/// JSON overhead of a `TaskBrief`.
fn brief_encoding_to(target: usize) -> TaskBrief {
    let probe = TaskBrief::new("").with_task_id("fixed-id");
    let overhead = probe.encode().len();
    assert!(
        target > overhead,
        "cannot build a {target}-byte brief: the empty brief already encodes to {overhead}"
    );
    let brief = TaskBrief::new("x".repeat(target - overhead)).with_task_id("fixed-id");
    assert_eq!(
        brief.encode().len(),
        target,
        "the padding arithmetic is wrong; fix the test, not the limit"
    );
    brief
}

/// The positive control for the size limit: a brief of **exactly**
/// `A2A_MAX_BRIEF_BYTES` crosses the real wire and runs.
///
/// This is the witness that keeps the mirrored packet-budget constant
/// honest. A limit set above what the transport can carry passes every
/// "too large is refused" test and still loses requests in production;
/// only sending the largest allowed brief for real can catch that.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_brief_at_the_wire_limit_round_trips() {
    let host = mesh().await;
    let caller = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let _h = host
        .serve_a2a(
            TaskRegistry::new(),
            Arc::new(Counting {
                runs: Arc::clone(&runs),
            }),
        )
        .expect("serve");
    connect_all(&host, &[&caller]).await;
    let target = host.inner().node_id();

    let brief = brief_encoding_to(A2A_MAX_BRIEF_BYTES);
    let ack = caller
        .submit_task(target, &brief)
        .await
        .expect("a brief at exactly the limit must be deliverable");
    assert!(ack.accepted, "ack: {ack:?}");

    // Delivered AND executed — an ack alone would not prove the body
    // survived the trip intact.
    for _ in 0..200 {
        if runs.load(Ordering::SeqCst) == 1 {
            let rec = caller
                .task_status(target, &ack.task_id)
                .await
                .expect("status")
                .expect("the submitter reads her own task");
            assert_eq!(
                rec.brief.prompt.len(),
                brief.prompt.len(),
                "the far side reassembled a different prompt than was sent"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("a limit-sized brief was acked but never executed");
}

/// One byte over the limit is refused **locally**: no packet, no
/// executor, and the error names both sizes so a caller can act on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_brief_over_the_wire_limit_is_refused_locally_and_never_sent() {
    let host = mesh().await;
    let caller = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let _h = host
        .serve_a2a(
            TaskRegistry::new(),
            Arc::new(Counting {
                runs: Arc::clone(&runs),
            }),
        )
        .expect("serve");
    connect_all(&host, &[&caller]).await;
    let target = host.inner().node_id();

    let brief = brief_encoding_to(A2A_MAX_BRIEF_BYTES + 1);
    let started = Instant::now();
    let err = caller
        .submit_task(target, &brief)
        .await
        .expect_err("a brief over the limit must be refused, not sent");
    match err {
        A2aFlowError::BriefTooLarge { encoded, limit } => {
            assert_eq!(encoded, A2A_MAX_BRIEF_BYTES + 1);
            assert_eq!(limit, A2A_MAX_BRIEF_BYTES);
        }
        other => panic!("expected BriefTooLarge, got {other:?}"),
    }
    // Refused before the wire: far faster than any round trip, and
    // nothing ran on the far side.
    //
    // Note what this row does and does not prove. At `limit + 1` the
    // brief is still *physically* deliverable — the limit keeps a
    // framing reserve, so the true packet cliff sits above it. So this
    // is a witness that the **contract** is enforced, not that this
    // particular brief would have vanished.
    // `a_brief_past_the_packet_cliff_is_refused_instead_of_vanishing`
    // is the row that covers the original defect.
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the refusal waited on the network instead of refusing locally"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "an over-large brief reached the executor"
    );

    // Control: the same caller and the same host accept a brief that
    // fits, so the refusal is about size and not a broken fixture.
    let ok = caller
        .submit_task(target, &brief_encoding_to(A2A_MAX_BRIEF_BYTES))
        .await
        .expect("an in-limit brief still works");
    assert!(ok.accepted);
}

/// The original defect, directly: a brief big enough to overflow a
/// packet is **refused**, where it used to vanish.
///
/// Measured on this wire before the fix: a brief encoding to ~1870 bytes
/// was acked, one encoding to ~2070 was never delivered and the caller —
/// with no deadline — waited for a reply that could not come. This row
/// sends a brief comfortably past that cliff and requires a *local*
/// refusal, so the pass cannot be produced by the request arriving.
///
/// Remove the size check and this row does not merely change its error
/// type: the submission disappears and the call burns the full deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_brief_past_the_packet_cliff_is_refused_instead_of_vanishing() {
    let host = mesh().await;
    let caller = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let _h = host
        .serve_a2a(
            TaskRegistry::new(),
            Arc::new(Counting {
                runs: Arc::clone(&runs),
            }),
        )
        .expect("serve");
    connect_all(&host, &[&caller]).await;
    let target = host.inner().node_id();

    // Well past the measured cliff: the array-of-bytes envelope
    // quadruples this to ~12 KB against an ~8 KB packet.
    let brief = brief_encoding_to(3000);
    let started = Instant::now();
    let err = caller
        .submit_task(target, &brief)
        .await
        .expect_err("an undeliverable brief must be refused");
    assert!(
        matches!(err, A2aFlowError::BriefTooLarge { .. }),
        "expected a local BriefTooLarge; a Timeout here means the request was sent and \
         vanished, which is the defect: {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the caller spent {:?} on an undeliverable request instead of refusing it",
        started.elapsed()
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0, "it reached the executor");

    // Control: the link is live and does deliver — so the refusal above
    // is about the size, not a dead peer.
    let ok = caller
        .submit_task(target, &TaskBrief::new("small"))
        .await
        .expect("the link delivers a small brief");
    assert!(ok.accepted);
}

/// `prepare_a2a` carries a brief too, so it gets the same local refusal
/// — a paid caller must learn the work is undeliverable *before* it
/// reserves anything or buys a quote against it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepare_refuses_an_over_limit_brief_before_reserving() {
    let caller = mesh().await;
    let brief = brief_encoding_to(A2A_MAX_BRIEF_BYTES + 1);
    // No host is needed: the refusal must happen before any packet, and
    // that is exactly what this asserts — a call that reached the wire
    // would time out instead.
    let started = Instant::now();
    let err = caller
        .prepare_a2a(7, &brief)
        .await
        .expect_err("prepare must refuse an undeliverable brief");
    assert!(
        matches!(err, A2aFlowError::BriefTooLarge { .. }),
        "expected BriefTooLarge, got {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "prepare consulted the network before checking the brief"
    );
}

/// A service may not **announce** a bound the wire cannot honor.
///
/// An announced `max_prompt_bytes` is a promise a caller sizes its work
/// against, and the failure mode for exceeding it is a vanished request
/// rather than a refusal — so the lie is refused at serve time, the same
/// discipline as an unenforceable price.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_service_announcing_an_undeliverable_prompt_bound_refuses_to_serve() {
    let host = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let executor: Arc<dyn TaskExecutor> = Arc::new(Counting {
        runs: Arc::clone(&runs),
    });

    let offer_with = |max_prompt_bytes: u64| A2aOffer {
        service_id: "summarize".to_string(),
        revision: "r1".to_string(),
        description: None,
        pricing_terms: None,
        bounds: A2aBounds {
            max_prompt_bytes,
            max_context_refs: 4,
            max_tags: 4,
            max_tag_bytes: 32,
            max_in_flight: 2,
        },
        reservation_ttl_secs: 600,
        reservation_retention_secs: 604_800,
        retention_secs: 3600,
    };

    let config_for = |max_prompt_bytes: u64| {
        A2aServiceConfig::new(BTreeMap::from([(
            "summarize".to_string(),
            A2aServicePolicy::Free(offer_with(max_prompt_bytes)),
        )]))
    };

    let refusal = host.serve_a2a_configured(
        TaskRegistry::new(),
        Arc::clone(&executor),
        config_for(A2A_MAX_BRIEF_BYTES as u64 + 1),
    );
    let err = match refusal {
        Ok(_) => panic!("an undeliverable prompt bound must refuse to serve"),
        Err(e) => e,
    };
    match err {
        ServeError::A2aUndeliverableBounds(msg) => {
            assert!(
                msg.contains("summarize"),
                "the refusal must name the offending service: {msg}"
            );
        }
        other => panic!("expected A2aUndeliverableBounds, got {other:?}"),
    }

    // Control: the same catalog with a deliverable bound serves. Without
    // it, a serve path broken for any reason would satisfy the assertion
    // above.
    let serving = host
        .serve_a2a_configured(
            TaskRegistry::new(),
            executor,
            config_for(A2A_MAX_BRIEF_BYTES as u64),
        )
        .expect("a deliverable bound serves");
    assert_eq!(
        serving.handles.len(),
        5,
        "the configured catalog serves five services"
    );
}

/// A request that is **delivered and never answered** ends in a bounded
/// [`A2aFlowError::Timeout`], not a permanent park.
///
/// Staging matters here, and the first attempt at it was wrong: a peer
/// that does not serve the A2A services at all answers *fast*, with a
/// `NoRoute` transport error, because the reply channel is unknown. That
/// is not the hang. The hang needs a request that genuinely lands on a
/// live, subscribed service whose handler never replies — a wedged
/// executor, a handler blocked on a lock or a dead dependency. So this
/// witness registers a handler on the real A2A status service that parks
/// forever, which is the only honest way to reach the pre-fix
/// wait-for-ever behaviour.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delivered_request_that_is_never_answered_times_out() {
    struct Parks;
    #[async_trait::async_trait]
    impl RpcHandler for Parks {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            // Never answers, never errors: the caller's deadline is the
            // only thing that can end this call.
            std::future::pending::<()>().await;
            unreachable!("pending() never resolves")
        }
    }

    let host = mesh().await;
    let caller = mesh().await;
    let _stuck = host
        .serve_rpc(A2A_STATUS_SERVICE, Arc::new(Parks))
        .expect("serve a parking status handler");
    connect_all(&host, &[&caller]).await;
    let target = host.inner().node_id();

    // The outer bound is the assertion that the call ends on its OWN
    // deadline: if the verb had no deadline, this outer timeout would
    // fire and `expect` would fail with "hung", which is exactly the
    // pre-fix behaviour.
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        A2A_CALL_TIMEOUT + Duration::from_secs(10),
        caller.task_status(target, "delivered-but-unanswered"),
    )
    .await
    .expect("the call hung past its own deadline");
    match outcome {
        Err(A2aFlowError::Timeout) => {}
        other => panic!("expected Timeout from an unanswered request, got {other:?}"),
    }
    // And it waited for the deadline rather than failing early for some
    // unrelated reason — otherwise a broken link would satisfy the arm
    // above.
    assert!(
        started.elapsed() >= A2A_CALL_TIMEOUT,
        "the call returned Timeout after only {:?}, so it did not wait on the deadline",
        started.elapsed()
    );
}

/// The control for the witness above: on a healthy peer the same verbs
/// answer promptly, so `Timeout` is a statement about the stuck handler
/// and not something the deadline does to every call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_healthy_peer_answers_well_inside_the_deadline() {
    let host = mesh().await;
    let caller = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let _h = host
        .serve_a2a(
            TaskRegistry::new(),
            Arc::new(Counting {
                runs: Arc::clone(&runs),
            }),
        )
        .expect("serve");
    connect_all(&host, &[&caller]).await;
    let target = host.inner().node_id();

    let started = Instant::now();
    let ack = caller
        .submit_task(target, &TaskBrief::new("small"))
        .await
        .expect("a healthy peer acks");
    assert!(ack.accepted);
    for _ in 0..200 {
        if let Some(rec) = caller
            .task_status(target, &ack.task_id)
            .await
            .expect("status")
        {
            if rec.state.is_terminal() {
                assert_eq!(
                    rec.state,
                    TaskState::Completed {
                        result_ref: "artifact:done".into()
                    }
                );
                assert!(
                    started.elapsed() < A2A_CALL_TIMEOUT,
                    "a healthy round trip must not approach the deadline"
                );
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the control submission never completed");
}
