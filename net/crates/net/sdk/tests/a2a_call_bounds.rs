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
    a2a_announceable_prompt_bytes, A2aFlowError, A2aServiceConfig, A2aServicePolicy,
    A2A_CALL_TIMEOUT, A2A_MAX_BRIEF_BYTES, A2A_STATUS_SERVICE,
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
        caller.start();
    }
    executor.start();
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

/// A free `summarize` offer announcing `max_prompt_bytes`. One shape, so
/// every bounds row below differs only in the number under test.
fn offer_with(max_prompt_bytes: u64) -> A2aOffer {
    A2aOffer {
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
    }
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
///
/// **Retargeted, round 2.** This row used to place the boundary at
/// `A2A_MAX_BRIEF_BYTES` itself — which is exactly the incoherence the
/// reviewer found: a *raw* prompt at that ceiling cannot fit, because the
/// task id, the service and revision names and the JSON structure come
/// out of the same encoded budget. So the control was announcing a bound
/// no caller could ever use, and the test attested to it. The property it
/// pins is unchanged — an unusable announcement refuses, a usable one
/// serves — but the boundary is now the real one the provider publishes,
/// [`a2a_announceable_prompt_bytes`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_service_announcing_an_undeliverable_prompt_bound_refuses_to_serve() {
    let host = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let executor: Arc<dyn TaskExecutor> = Arc::new(Counting {
        runs: Arc::clone(&runs),
    });

    let config_for = |max_prompt_bytes: u64| {
        A2aServiceConfig::new(BTreeMap::from([(
            "summarize".to_string(),
            A2aServicePolicy::Free(offer_with(max_prompt_bytes)),
        )]))
    };

    let usable = a2a_announceable_prompt_bytes("summarize", "r1");
    assert!(
        usable > 0 && usable < A2A_MAX_BRIEF_BYTES as u64,
        "the usable prompt ceiling must sit strictly below the encoded-brief limit: \
         {usable} vs {A2A_MAX_BRIEF_BYTES}"
    );

    let refusal = host.serve_a2a_configured(
        TaskRegistry::new(),
        Arc::clone(&executor),
        config_for(usable + 1),
    );
    let err = match refusal {
        Ok(_) => panic!("an unusable prompt bound must refuse to serve"),
        Err(e) => e,
    };
    match err {
        ServeError::A2aUndeliverableBounds(msg) => {
            assert!(
                msg.contains("summarize"),
                "the refusal must name the offending service: {msg}"
            );
            assert!(
                msg.contains("max_prompt_bytes") && msg.contains(&usable.to_string()),
                "the refusal must name the field and the ceiling to announce instead: {msg}"
            );
        }
        other => panic!("expected A2aUndeliverableBounds, got {other:?}"),
    }

    // Control: the same catalog with the largest USABLE bound serves.
    // Without it, a serve path broken for any reason would satisfy the
    // assertion above.
    let serving = host
        .serve_a2a_configured(TaskRegistry::new(), executor, config_for(usable))
        .expect("the largest usable bound serves");
    assert_eq!(
        serving.handles.len(),
        5,
        "the configured catalog serves five services"
    );
}

/// The advertised ceiling is not merely *announceable* — it is
/// **usable**: a prompt of exactly `max_prompt_bytes` reaches the
/// provider over a real wire and is admitted.
///
/// This is the other half of the coherence the reviewer asked for. The
/// row above proves an unusable announcement is refused; this one proves
/// the largest accepted announcement can actually be spent, end to end,
/// rather than being a number that merely survives validation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_announced_prompt_ceiling_is_usable_over_the_wire() {
    let host = mesh().await;
    let caller = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let usable = a2a_announceable_prompt_bytes("summarize", "r1");
    let _serving = host
        .serve_a2a_configured(
            TaskRegistry::new(),
            Arc::new(Counting {
                runs: Arc::clone(&runs),
            }),
            A2aServiceConfig::new(BTreeMap::from([(
                "summarize".to_string(),
                A2aServicePolicy::Free(offer_with(usable)),
            )])),
        )
        .expect("serve at the usable ceiling");
    connect_all(&host, &[&caller]).await;
    let target = host.inner().node_id();

    // A prompt of exactly the advertised size, with the task id the
    // deliverability contract reserves room for.
    let brief = TaskBrief::new("x".repeat(usable as usize))
        .with_task_id("t".repeat(128))
        .with_service("summarize", "r1");
    let reply = caller
        .prepare_a2a(target, &brief)
        .await
        .expect("a prompt at the advertised ceiling must reach the provider");
    assert!(
        matches!(reply, net_sdk::a2a::PrepareReply::Reservation(_)),
        "a prompt at the advertised ceiling must be admitted, got {reply:?}"
    );
}

/// Every announced field bound can be satisfied and the brief still be
/// undeliverable, because they share **one** encoded budget.
///
/// The refusal is local and names both numbers, and the control shows the
/// documented remedy works: move the bulk into a context artifact ref.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_brief_inside_every_field_bound_can_still_exceed_the_joint_budget() {
    let caller = mesh().await;
    let usable = a2a_announceable_prompt_bytes("summarize", "r1");

    // Inside `max_prompt_bytes`, inside `max_context_refs`, inside
    // `max_tags`, inside `max_tag_bytes` — and over the joint budget.
    let greedy = TaskBrief::new("x".repeat(usable as usize))
        .with_task_id("job-1")
        .with_service("summarize", "r1")
        .with_context_refs(vec!["blob://".to_string() + &"c".repeat(120); 4])
        .with_tags(vec!["t".repeat(32); 4]);
    let err = caller
        .prepare_a2a(7, &greedy)
        .await
        .expect_err("the joint budget must refuse it");
    match err {
        A2aFlowError::BriefTooLarge { encoded, limit } => {
            assert!(
                encoded > limit && limit == A2A_MAX_BRIEF_BYTES,
                "the refusal must name the encoded size and the limit: {encoded} / {limit}"
            );
        }
        other => panic!("expected BriefTooLarge, got {other:?}"),
    }

    // Control: the same work with the bulk in a ref encodes small, so
    // the refusal above is about the budget and not about refs or tags
    // being present at all.
    let lean = TaskBrief::new("summarize the attached document")
        .with_task_id("job-1")
        .with_service("summarize", "r1")
        .with_context_refs(vec!["blob://".to_string() + &"c".repeat(120); 4])
        .with_tags(vec!["t".repeat(32); 4]);
    assert!(
        lean.encode().len() <= A2A_MAX_BRIEF_BYTES,
        "the documented remedy must produce a deliverable brief"
    );
}

/// An offer whose own data cannot be **discovered** refuses to serve.
///
/// The reviewer's R8: a free service with a 4 KiB description started
/// successfully and then `describe_a2a` timed out, because only the
/// request brief was bounded. Descriptions now fit (the describe wire
/// stopped paying the array-of-bytes envelope), so the refusal boundary
/// sits at a genuinely undeliverable page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_offer_too_large_to_discover_refuses_to_serve() {
    let host = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let described = |description_len: usize| {
        A2aServiceConfig::new(BTreeMap::from([(
            "summarize".to_string(),
            A2aServicePolicy::Free(A2aOffer {
                description: Some("d".repeat(description_len)),
                ..offer_with(512)
            }),
        )]))
    };

    let refused = host.serve_a2a_configured(
        TaskRegistry::new(),
        Arc::new(Counting {
            runs: Arc::clone(&runs),
        }),
        described(16 * 1024),
    );
    match refused {
        Ok(_) => panic!("an undiscoverable offer must refuse to serve"),
        Err(ServeError::A2aUndeliverableBounds(msg)) => assert!(
            msg.contains("summarize") && msg.contains("discovery page"),
            "the refusal must name the service and the page it cannot fit: {msg}"
        ),
        Err(other) => panic!("expected A2aUndeliverableBounds, got {other:?}"),
    }

    // Control: the reviewer's own 4 KiB description now serves AND is
    // discoverable — the repair moved the ceiling rather than widening a
    // timeout.
    let _serving = host
        .serve_a2a_configured(
            TaskRegistry::new(),
            Arc::new(Counting { runs }),
            described(4096),
        )
        .expect("a 4 KiB description serves");
}

/// A catalog larger than one packet is discovered **completely**, across
/// pages.
///
/// Every offer here fits a page on its own; together they do not. Before
/// pagination the reply was handed to the transport, dropped, and the
/// caller burned its deadline — so this row fails as a `Timeout` (or as a
/// short catalog) if paging regresses, not merely with a different error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catalog_larger_than_one_packet_is_discovered_across_pages() {
    let host = mesh().await;
    let caller = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));

    // 12 offers × ~1.2 KiB of description ≈ 15 KiB, against a ~7 KiB
    // page.
    let mut services = BTreeMap::new();
    for i in 0..12u32 {
        let id = format!("svc-{i:02}");
        services.insert(
            id.clone(),
            A2aServicePolicy::Free(A2aOffer {
                service_id: id,
                description: Some(format!("{i:02}").repeat(600)),
                ..offer_with(512)
            }),
        );
    }
    let _serving = host
        .serve_a2a_configured(
            TaskRegistry::new(),
            Arc::new(Counting { runs }),
            A2aServiceConfig::new(services),
        )
        .expect("serve a multi-page catalog");
    connect_all(&host, &[&caller]).await;
    let target = host.inner().node_id();

    let offers = caller
        .describe_a2a(target)
        .await
        .expect("a multi-page catalog must be discoverable");
    let ids: Vec<&str> = offers.iter().map(|o| o.service_id.as_str()).collect();
    let expected: Vec<String> = (0..12u32).map(|i| format!("svc-{i:02}")).collect();
    assert_eq!(
        ids,
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "the walk must return every service exactly once, in catalog order"
    );
    for offer in &offers {
        assert_eq!(
            offer.description.as_ref().expect("description").len(),
            1200,
            "each page's offers must arrive intact"
        );
    }
}

/// A terminal outcome the status reply cannot carry is an **actionable
/// bounded failure**, not a timeout.
///
/// The response side of the same defect: the provider computed a record,
/// handed an over-large reply to the transport, and the caller waited out
/// `A2A_CALL_TIMEOUT` for an answer that already existed. Now it learns
/// the sizes immediately, and the provider's own record is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_undeliverable_status_record_is_refused_with_its_sizes() {
    /// Returns a result far past what a status reply can carry.
    struct Huge;
    #[async_trait::async_trait]
    impl TaskExecutor for Huge {
        async fn run(&self, brief: TaskBrief, _c: CancelToken) -> Result<String, String> {
            if brief.prompt == "small" {
                return Ok("artifact:ok".to_string());
            }
            Ok(format!("blob://{}", "r".repeat(4000)))
        }
    }

    let host = mesh().await;
    let caller = mesh().await;
    let _h = host
        .serve_a2a(TaskRegistry::new(), Arc::new(Huge))
        .expect("serve");
    connect_all(&host, &[&caller]).await;
    let target = host.inner().node_id();

    let ack = caller
        .submit_task(target, &TaskBrief::new("produce a giant result"))
        .await
        .expect("submit");
    assert!(ack.accepted);

    let started = Instant::now();
    let mut refusal = None;
    for _ in 0..200 {
        match caller.task_status(target, &ack.task_id).await {
            Err(e) => {
                refusal = Some(e);
                break;
            }
            Ok(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    match refusal.expect("the over-large record must be refused, not dropped") {
        A2aFlowError::ReplyTooLarge(message) => {
            assert!(
                message.contains("status record") && message.contains("per-packet"),
                "the refusal must name what overflowed and the limit: {message}"
            );
        }
        other => panic!(
            "expected ReplyTooLarge; a Timeout here means the reply was computed and \
             dropped, which is the defect: {other:?}"
        ),
    }
    assert!(
        started.elapsed() < A2A_CALL_TIMEOUT,
        "the caller burned its full deadline ({:?}) instead of being told",
        started.elapsed()
    );

    // Control: the same host, the same status verb, a record that fits —
    // so the refusal above is about the size and not a broken status
    // path.
    let small = caller
        .submit_task(target, &TaskBrief::new("small"))
        .await
        .expect("submit small");
    for _ in 0..200 {
        if let Ok(Some(rec)) = caller.task_status(target, &small.task_id).await {
            if rec.state.is_terminal() {
                assert_eq!(
                    rec.state,
                    TaskState::Completed {
                        result_ref: "artifact:ok".to_string()
                    }
                );
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the control record never became readable");
}

/// A payment proof whose header cannot ride the request is refused
/// **locally, before the call is registered** — on every profile.
///
/// The header encoder asserts its 4096-byte bound only in debug and
/// narrows an over-long value into a `u16` in a shipped profile, so an
/// unvalidated proof from a binding is a debug panic and a release
/// corrupted frame. A 4097-byte binding therefore has to be refused
/// before it reaches the encoder, which is what this measures: the call
/// returns immediately, with no target reachable at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proof_over_the_request_header_bound_is_refused_before_the_call() {
    use net_sdk::a2a::{AdmissionReservation, PreparedTask};
    use net_sdk::a2a_payment::TaskPaymentProof;

    let caller = mesh().await;
    let prepared = PreparedTask {
        provider_node: 7,
        brief: TaskBrief::new("work").with_task_id("job-1"),
        offer_hash: "offer".to_string(),
        reservation: AdmissionReservation {
            task_id: "job-1".to_string(),
            admission_id: "adm-1".to_string(),
            commitment: "commit".to_string(),
            purchase_hash: "purchase".to_string(),
            capability: "7/net.a2a.task/summarize".to_string(),
            pricing_terms: None,
            expires_at: u64::MAX,
        },
    };
    let proof = |binding: Vec<u8>| TaskPaymentProof {
        quote_id: "quote-1".to_string(),
        binding_sig: binding,
    };

    let started = Instant::now();
    let err = caller
        .submit_task_paid(&prepared, &proof(vec![0x5a; 4097]))
        .await
        .expect_err("a 4097-byte binding must be refused locally");
    match err {
        A2aFlowError::ProofUndeliverable(detail) => assert!(
            detail.contains("4097") && detail.contains("4096"),
            "the refusal must name the size and the limit: {detail}"
        ),
        other => panic!("expected ProofUndeliverable, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the refusal reached the network instead of refusing locally"
    );

    // Control: a well-shaped proof of the real signature size passes
    // validation and fails on TRANSPORT instead — node 7 does not exist
    // — so the refusals above are about the proof and not about
    // `submit_task_paid` refusing everything.
    let control = caller
        .submit_task_paid(&prepared, &proof(vec![0x5a; 64]))
        .await
        .expect_err("node 7 is unreachable");
    assert!(
        matches!(control, A2aFlowError::Transport(_) | A2aFlowError::Timeout),
        "a 64-byte binding must pass proof validation and fail on transport, got {control:?}"
    );
}

/// An application refusal whose prose outgrows the packet is **bounded
/// and delivered**, not dropped.
///
/// The verdict is the load-bearing part of a refusal; the tail of an
/// application's essay is not. So provider-authored prose is truncated at
/// one place with the cut marked, and the caller still learns it was
/// rejected and why — where an unbounded reason produced a reply the
/// transport discarded and a caller that waited out its deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_over_long_refusal_reason_is_bounded_and_still_delivered() {
    struct Verbose;
    #[async_trait::async_trait]
    impl net_sdk::mesh_a2a::TaskPreflight for Verbose {
        async fn preflight(
            &self,
            _owner: net_sdk::a2a::TaskOwner,
            _offer: &A2aOffer,
            _brief: &TaskBrief,
        ) -> Result<(), String> {
            Err(format!("QUOTA EXHAUSTED: {}", "why ".repeat(5000)))
        }
    }

    let host = mesh().await;
    let caller = mesh().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let _serving = host
        .serve_a2a_configured(
            TaskRegistry::new(),
            Arc::new(Counting { runs }),
            A2aServiceConfig::new(BTreeMap::from([(
                "summarize".to_string(),
                A2aServicePolicy::Free(offer_with(512)),
            )]))
            .with_preflight(Arc::new(Verbose)),
        )
        .expect("serve");
    connect_all(&host, &[&caller]).await;
    let target = host.inner().node_id();

    let brief = TaskBrief::new("work")
        .with_task_id("job-1")
        .with_service("summarize", "r1");
    let started = Instant::now();
    let reply = caller
        .prepare_a2a(target, &brief)
        .await
        .expect("an over-long refusal must still be delivered");
    match reply {
        net_sdk::a2a::PrepareReply::Rejected { reason } => {
            assert!(
                reason.starts_with("QUOTA EXHAUSTED:"),
                "the verdict must survive the truncation: {reason}"
            );
            assert!(
                reason.contains("more bytes dropped"),
                "the cut must be marked so a reader is not left guessing: {reason}"
            );
            assert!(
                reason.len() < 600,
                "the reason was not bounded: {} bytes",
                reason.len()
            );
        }
        other => panic!("expected a Rejected reply, got {other:?}"),
    }
    assert!(
        started.elapsed() < A2A_CALL_TIMEOUT,
        "the caller burned its deadline on an undeliverable refusal"
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
