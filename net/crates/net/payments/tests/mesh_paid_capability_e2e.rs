//! The canonical Net Payments loop, end to end, across the real mesh
//! wire — **the one made impossible to regress** (M1 of
//! `docs/plans/PAYMENTS_TEST_MATRIX.md`).
//!
//! Every other paid-serve test proves one half: the payment flow crosses
//! the wire (`mesh_payments_e2e`) but never invokes a handler; the tool
//! gate serves (`tool_serve_paid`, `native_tool_gate`) but either with a
//! *scripted* gate over the wire or the real gate only in-process. This
//! test is the composition neither covers: two real `MeshNode`s, the
//! **real `EngineToolPaymentGate`** over one shared `PaymentEngine`, and
//! the whole company loop —
//!
//! 1. provider publishes a priced tool (`serve_tool_paid`);
//! 2. caller discovers the announced `pricing_terms`;
//! 3. an **unpaid** invoke is refused (`ERR_PAYMENT` + `missing_quote`
//!    schematic) before the handler runs;
//! 4. the caller **pays** through `CallerPaymentFlow` (quote → spend
//!    policy → payload → settle, all over the wire);
//! 5. the **paid** invoke redeems the quote through the gate and serves;
//! 6. **replaying** the same proof is refused `already_redeemed`
//!    (`prior_payment=consumed`);
//! 7. **reusing** the quote on a different tool is refused
//!    `wrong_tool_binding` (`security_violation`);
//! 8. the handler ran **exactly once** and billing recorded **exactly
//!    one** paid serve, its signature verifying caller-side.
//!
//! The short invariant, proven in one flow: every priced path is
//! gated-or-denied; the one valid payment serves once; every invalid
//! reuse fails before the handler; the one success bills once; every
//! failure carries its machine-actionable recovery verdict.
#![cfg(feature = "mesh")]

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::canonical::{canonical_bytes, SignedEnvelope as _};
use net_payments::core::registry::default_mock_registry;
use net_payments::core::terms::PricingTerms;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::mesh::{serve_payments, EngineToolPaymentGate, MeshPaymentChannel};
use net_payments::flow::{CallerDecision, CallerPaymentFlow, Clock, InProcessProvider};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptions, RpcError};
use net_sdk::tool::metadata_for;
use net_sdk::tool_payment::{
    FailureSchematic, ERR_PAYMENT, HDR_FAILURE_SCHEMATIC, HDR_PAYMENT_BINDING, HDR_PAYMENT_QUOTE,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(JsonSchema, Deserialize, Serialize, Debug, PartialEq, Eq)]
struct EchoReq {
    message: String,
}

#[derive(JsonSchema, Deserialize, Serialize, Debug, PartialEq, Eq)]
struct EchoResp {
    echoed: String,
}

struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000, Ordering::SeqCst)
    }
}

async fn handshake(server: &Mesh, caller: &Mesh) {
    let server_addr = server.inner().local_addr();
    let server_pub = *server.inner().public_key();
    let server_id = server.inner().node_id();
    let caller_id = caller.inner().node_id();
    let (accept, connect) = tokio::join!(server.inner().accept(caller_id), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller
            .inner()
            .connect(server_addr, &server_pub, server_id)
            .await
    });
    accept.expect("accept");
    connect.expect("connect");
    server.inner().start();
    caller.inner().start();
}

/// A tool refusal, both renderings: the wire status + human message, and
/// the schematic decoded off the reply header per the discipline rule
/// (exactly one valid header, else `None` and the human path).
#[derive(Debug)]
struct ToolRefusal {
    status: u16,
    message: String,
    schematic: Option<FailureSchematic>,
}

/// Invoke a served tool with request headers, bounded + retried a few
/// times (the first cross-node call can lose its reply before the
/// per-caller reply subscription propagates — the round-trip idiom). A
/// server refusal is a deterministic answer: it is returned, never
/// retried.
async fn invoke_tool(
    caller: &Mesh,
    provider_id: u64,
    service: &str,
    body: &[u8],
    headers: Vec<(String, Vec<u8>)>,
) -> Result<Vec<u8>, ToolRefusal> {
    let mut last = String::new();
    for _ in 0..5 {
        let opts = CallOptions {
            request_headers: headers.clone(),
            ..CallOptions::default()
        };
        match tokio::time::timeout(
            Duration::from_secs(5),
            caller.call(
                provider_id,
                service,
                bytes::Bytes::copy_from_slice(body),
                opts,
            ),
        )
        .await
        {
            Ok(Ok(reply)) => return Ok(reply.body.to_vec()),
            Ok(Err(RpcError::ServerError {
                status,
                message,
                headers,
            })) => {
                let entries: Vec<&Vec<u8>> = headers
                    .iter()
                    .filter(|(name, _)| name == HDR_FAILURE_SCHEMATIC)
                    .map(|(_, value)| value)
                    .collect();
                let schematic = match entries.as_slice() {
                    [bytes] => FailureSchematic::from_header_bytes(bytes),
                    _ => None,
                };
                return Err(ToolRefusal {
                    status,
                    message,
                    schematic,
                });
            }
            Ok(Err(e)) => last = format!("rpc error: {e:?}"),
            Err(_) => last = "call timed out".to_string(),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("tool call never reached the provider: {last}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paid_capability_serves_once_and_only_once_across_the_mesh() {
    let psk = [0x42u8; 32];
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock(AtomicU64::new(1_000_000_000_000_000)));

    // ── two real nodes, real UDP loopback, real handshake ──────────
    let provider_mesh = MeshBuilder::new("127.0.0.1:0", &psk)
        .expect("builder")
        .build()
        .await
        .expect("provider mesh");
    let caller_mesh = Arc::new(
        MeshBuilder::new("127.0.0.1:0", &psk)
            .expect("builder")
            .build()
            .await
            .expect("caller mesh"),
    );
    handshake(&provider_mesh, &caller_mesh).await;
    let provider_id = provider_mesh.inner().node_id();

    // ── provider: ONE engine behind BOTH seams ─────────────────────
    // The payment services (`serve_payments`) and the tool gate
    // (`EngineToolPaymentGate`) share the same `PaymentEngine`, so a
    // quote paid over the payment wire is the very quote the gate
    // redeems over the tool wire — the composition the sibling tests
    // never join.
    let provider_keys = Arc::new(EntityKeypair::generate());
    let registry = default_mock_registry(provider_keys.entity_id().clone());
    let provider_log = Arc::new(BillingLog::new(dir.path().join("provider-billing.jsonl")));
    let engine = Arc::new(
        PaymentEngine::new(
            provider_keys.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            registry.clone(),
            dir.path().join("engine.json"),
        )
        .expect("engine")
        .with_billing_log(provider_log.clone()),
    );

    let in_process = Arc::new(InProcessProvider::new(engine.clone(), clock.clone()));
    let _payments = serve_payments(&provider_mesh, in_process).expect("serve payments");

    // The priced tool, published through the native paid-serve path and
    // gated by the real engine. `fixture-tool` is the tool segment of
    // the capability the caller pays.
    let gate = Arc::new(EngineToolPaymentGate::new(engine.clone()));
    let handler_runs = Arc::new(AtomicUsize::new(0));
    let counted = handler_runs.clone();
    let paid_tool = metadata_for::<EchoReq, EchoResp>("fixture-tool")
        .description("Echo, for money.")
        .pricing_terms(r#"{"object":"net.pricing.terms@1"}"#)
        .build();
    let _served_paid = provider_mesh
        .serve_tool_paid::<EchoReq, EchoResp, _, _>(paid_tool, gate.clone(), move |req: EchoReq| {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(EchoResp {
                    echoed: req.message,
                })
            }
        })
        .expect("a priced descriptor serves through the engine gate");

    // A second priced tool on the same gate — the target the wrong-tool
    // reuse aims at. Its handler must never run.
    let other_tool = metadata_for::<EchoReq, EchoResp>("other-tool")
        .description("A different priced tool.")
        .pricing_terms(r#"{"object":"net.pricing.terms@1"}"#)
        .build();
    let _served_other = provider_mesh
        .serve_tool_paid::<EchoReq, EchoResp, _, _>(
            other_tool,
            gate.clone(),
            |req: EchoReq| async move {
                Ok(EchoResp {
                    echoed: req.message,
                })
            },
        )
        .expect("second priced tool serves");

    // The capability id the mesh routes by: `<provider node>/<tool>`.
    let capability = format!("{provider_id}/fixture-tool");

    // (2) Discover the announced pricing — the `net.pricing.terms@1`
    //     envelope a describe would carry (constructed locally from the
    //     provider's known template, the established flow-input idiom).
    //     Built before the registry moves into the flow.
    let template = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author");
    let terms = PricingTerms::new(
        provider_keys.entity_id().clone(),
        capability.clone(),
        vec![template],
        registry.reference().expect("registry reference"),
    );
    let terms_json =
        String::from_utf8(canonical_bytes(&terms).expect("canonicalize")).expect("utf8");

    // ── caller: the payment flow over the wire ─────────────────────
    let caller_keys = Arc::new(EntityKeypair::generate());
    let spend_path = dir.path().join("spend-policy.json");
    let flow = CallerPaymentFlow::new(
        caller_keys,
        SpendPolicyEngine::new(&spend_path, SpendProfile::DevTest),
        registry,
        Arc::new(MeshPaymentChannel::new(caller_mesh.clone())),
        clock,
    );

    let echo_body = serde_json::to_vec(&EchoReq {
        message: "hi".into(),
    })
    .unwrap();

    // (3) UNPAID invoke → refused before the handler, with the
    //     `missing_quote` admission schematic on the reply header.
    let refusal = invoke_tool(
        &caller_mesh,
        provider_id,
        "fixture-tool",
        &echo_body,
        vec![],
    )
    .await
    .expect_err("an unpaid invoke must be refused");
    assert_eq!(refusal.status, ERR_PAYMENT);
    assert!(
        refusal.message.contains("payment quote"),
        "the human body names the missing quote: {}",
        refusal.message
    );
    let schematic = refusal
        .schematic
        .expect("the refusal carries its schematic");
    assert_eq!(schematic.reason, "missing_quote");
    assert_eq!(schematic.stage, "admission");
    assert!(!schematic.handler_executed);
    assert_eq!(
        handler_runs.load(Ordering::SeqCst),
        0,
        "the handler never runs on an unpaid call"
    );

    // (4) PAY over the wire: quote → DevTest auto-allow → payload →
    //     settle. The proof carries the provider-signed billing event.
    let CallerDecision::Paid {
        quote_id,
        binding_sig,
        proof,
    } = flow.run(&capability, &terms_json).await
    else {
        panic!("expected Paid over the wire");
    };
    let billing_json = proof["billing_event"].as_str().expect("billing event");
    let billing =
        net_payments::core::billing_event::BillingEvent::from_json_bytes(billing_json.as_bytes())
            .expect("caller-side verification of the provider-signed event");
    assert_eq!(billing.capability, capability);
    assert_eq!(
        provider_log.read_all().await.expect("read").len(),
        1,
        "one payment, one billing event"
    );
    let binding = binding_sig.expect("the paying identity signs the invocation binding");

    // (5) PAID invoke: quote id + possession proof ride the headers; the
    //     gate redeems the quote and the handler serves exactly once.
    let paid_headers = vec![
        (HDR_PAYMENT_QUOTE.to_string(), quote_id.clone().into_bytes()),
        (HDR_PAYMENT_BINDING.to_string(), binding.clone()),
    ];
    let reply = invoke_tool(
        &caller_mesh,
        provider_id,
        "fixture-tool",
        &echo_body,
        paid_headers.clone(),
    )
    .await
    .expect("the paid invoke serves");
    let resp: EchoResp = serde_json::from_slice(&reply).expect("decode");
    assert_eq!(resp.echoed, "hi");
    assert_eq!(
        handler_runs.load(Ordering::SeqCst),
        1,
        "the paid invoke ran the handler once"
    );

    // (6) REPLAY the same proof → refused `already_redeemed` (one
    //     payment, one serve): funds moved, the instrument is consumed,
    //     requoting is the sanctioned path forward.
    let replay = invoke_tool(
        &caller_mesh,
        provider_id,
        "fixture-tool",
        &echo_body,
        paid_headers,
    )
    .await
    .expect_err("replaying a consumed proof must be refused");
    assert_eq!(replay.status, ERR_PAYMENT);
    assert!(
        replay.message.contains("already redeemed"),
        "the human body is unchanged beside the schematic: {}",
        replay.message
    );
    let schematic = replay
        .schematic
        .expect("the replay refusal carries its schematic");
    assert_eq!(schematic.reason, "already_redeemed");
    assert_eq!(schematic.funds_moved, "yes");
    assert_eq!(schematic.prior_payment, "consumed");
    assert!(schematic.recovery.safe_to_requote);
    assert!(!schematic.recovery.safe_to_retry);

    // (7) REUSE the quote on a DIFFERENT tool → refused
    //     `wrong_tool_binding`, a security verdict (do not retry, do not
    //     just buy another quote). Bearer reuse (quote id only): a
    //     present binding would fail the possession check *first* and
    //     mask the tool-binding verdict this step exists to prove.
    let bearer = vec![(HDR_PAYMENT_QUOTE.to_string(), quote_id.into_bytes())];
    let misdirected = invoke_tool(&caller_mesh, provider_id, "other-tool", &echo_body, bearer)
        .await
        .expect_err("a quote never redeems for a different tool");
    assert_eq!(misdirected.status, ERR_PAYMENT);
    assert!(
        misdirected.message.contains("bound to capability"),
        "the human body names the tool-binding mismatch: {}",
        misdirected.message
    );
    let schematic = misdirected
        .schematic
        .expect("the wrong-tool refusal carries its schematic");
    assert_eq!(schematic.reason, "wrong_tool_binding");
    assert_eq!(schematic.recovery.class, "security_violation");
    assert!(!schematic.recovery.safe_to_requote);

    // (8) The invariant, stated as counts: the handler ran once, billing
    //     recorded exactly one paid serve, and it verifies from a fresh
    //     verifier. Neither the replay nor the wrong-tool reuse touched
    //     either.
    assert_eq!(
        handler_runs.load(Ordering::SeqCst),
        1,
        "exactly one handler execution across the whole loop"
    );
    let recorded = provider_log.read_all().await.expect("read");
    assert_eq!(recorded.len(), 1, "exactly one billing event");
    assert_eq!(recorded[0].billing_event_id, billing.billing_event_id);
    recorded[0]
        .verify_signature()
        .expect("billing signature verifies");
}
