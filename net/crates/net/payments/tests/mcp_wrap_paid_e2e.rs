//! The MCP wrap path's paid invoke, end to end, across the real mesh
//! wire with the **real `EnginePaymentAdmission`** (M2 of
//! `docs/plans/PAYMENTS_TEST_MATRIX.md`).
//!
//! The MCP-adapter twin of `mesh_paid_capability_e2e` (M1): where M1
//! drives the SDK-native `serve_tool_paid` path against
//! `EngineToolPaymentGate`, this drives the MCP wrap publication
//! (`ServerPublisher::publish_tools` + `WrapInvokeHandler`) against
//! `EnginePaymentAdmission` — the other sanctioned serving path, over the
//! wire, with the real engine gate. The existing wrap coverage stops at
//! publish-time pricing guards and a *scripted* admission; nothing ran a
//! paid invoke through the wrap handler with the engine deciding.
//!
//! Both gates are thin wrappers over the one `redeem_via_engine` mapping,
//! so the schematic vocabulary is identical to M1's — this test proves
//! the MCP adapter carries the quote header to the engine, serves the
//! wrapped invoker on admission, and renders the same fail-closed
//! verdicts (`missing_quote`, `already_redeemed`, `wrong_tool_binding`)
//! on refusal, all across two real `MeshNode`s.
#![cfg(all(feature = "mesh", feature = "mcp-gate"))]

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use net::adapter::net::identity::EntityKeypair;
use net_mcp::spec::{CallToolResult, Implementation, Tool};
use net_mcp::wrap::{
    CredentialStatus, LoweringContext, McpError, ServerPublisher, Substitutability, ToolInvoker,
    WrapConfig,
};
use net_payments::billing::BillingLog;
use net_payments::core::canonical::{canonical_bytes, SignedEnvelope as _};
use net_payments::core::registry::default_mock_registry;
use net_payments::core::terms::PricingTerms;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::mcp_gate::EnginePaymentAdmission;
use net_payments::flow::mesh::{serve_payments, MeshPaymentChannel};
use net_payments::flow::{CallerDecision, CallerPaymentFlow, Clock, InProcessProvider};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptions, RpcError};
use net_sdk::tool_payment::{
    FailureSchematic, ERR_PAYMENT, HDR_FAILURE_SCHEMATIC, HDR_PAYMENT_BINDING, HDR_PAYMENT_QUOTE,
};
use serde_json::json;

/// The provider's wrapped tools: plain Rust closures behind the MCP
/// `ToolInvoker` seam. `add` sums two integers; `echo` returns its
/// message. A single counter proves the invoker runs exactly once —
/// only the one paid `add` call must ever reach it.
#[derive(Default)]
struct PaidTools {
    calls: AtomicUsize,
}

#[async_trait]
impl ToolInvoker for PaidTools {
    async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, McpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match name {
            "add" => {
                let a = arguments.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
                let b = arguments.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(CallToolResult::text_ok((a + b).to_string()))
            }
            "echo" => {
                let msg = arguments
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Ok(CallToolResult::text_ok(msg))
            }
            other => Ok(CallToolResult::text_error(format!("unknown tool: {other}"))),
        }
    }
}

struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000, Ordering::SeqCst)
    }
}

fn tool(name: &str, schema: serde_json::Value) -> Tool {
    Tool {
        name: name.to_string(),
        title: None,
        description: Some(format!("The {name} tool.")),
        input_schema: schema,
        output_schema: None,
    }
}

fn lowering_ctx() -> LoweringContext {
    LoweringContext {
        server_version: "payments-e2e-1.0".to_string(),
        credential_status: CredentialStatus::None,
        substitutability: Substitutability::ProviderLocal,
        pricing: Default::default(),
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
/// the schematic decoded off the reply header per the discipline rule.
#[derive(Debug)]
struct ToolRefusal {
    status: u16,
    message: String,
    schematic: Option<FailureSchematic>,
}

/// Invoke a wrapped tool with request headers, bounded + retried (the
/// first cross-node call can lose its reply before the per-caller reply
/// subscription propagates). A server refusal is a deterministic answer:
/// returned, never retried.
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
async fn a_wrapped_paid_tool_serves_once_and_only_once_across_the_mesh() {
    let psk = [0x57u8; 32];
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn Clock> = Arc::new(TestClock(AtomicU64::new(1_000_000_000_000_000)));

    // ── two real nodes ─────────────────────────────────────────────
    let provider_mesh = Arc::new(
        MeshBuilder::new("127.0.0.1:0", &psk)
            .expect("builder")
            .build()
            .await
            .expect("provider mesh"),
    );
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
    // The payment services (`serve_payments`) and the MCP wrap
    // admission gate (`EnginePaymentAdmission`) share one
    // `PaymentEngine`, so a quote paid over the payment wire is the one
    // the wrap handler redeems over the tool wire.
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

    // Publish two priced tools through the MCP wrap path, gated by the
    // real engine admission. `add` is the one the caller pays for;
    // `echo` is the wrong-tool target the reused quote aims at.
    let admission = Arc::new(EnginePaymentAdmission::new(engine.clone()));
    let mut config = WrapConfig::owner_only(
        Implementation {
            name: "payments-e2e".to_string(),
            version: "1.0".to_string(),
        },
        caller_mesh.origin_hash(),
    );
    let minimal_terms = r#"{"object":"net.pricing.terms@1"}"#.to_string();
    config
        .pricing
        .insert("add".to_string(), minimal_terms.clone());
    config.pricing.insert("echo".to_string(), minimal_terms);
    config.payment_admission = Some(admission);

    let invoker = Arc::new(PaidTools::default());
    let publisher = ServerPublisher::new(Arc::clone(&provider_mesh));
    let _publication = publisher
        .publish_tools(
            &[
                tool(
                    "add",
                    json!({
                        "type": "object",
                        "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } }
                    }),
                ),
                tool(
                    "echo",
                    json!({ "type": "object", "properties": { "message": { "type": "string" } } }),
                ),
            ],
            invoker.clone() as Arc<dyn ToolInvoker>,
            lowering_ctx(),
            config,
        )
        .await
        .expect("a priced wrap publication with the engine gate publishes");

    // ── caller: the payment flow over the wire ─────────────────────
    let capability = format!("{provider_id}/add");
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

    let caller_keys = Arc::new(EntityKeypair::generate());
    let spend_path = dir.path().join("spend-policy.json");
    let flow = CallerPaymentFlow::new(
        caller_keys,
        SpendPolicyEngine::new(&spend_path, SpendProfile::DevTest),
        registry,
        Arc::new(MeshPaymentChannel::new(caller_mesh.clone())),
        clock,
    );

    // The wrapped tool is invoked directly by (provider node, service):
    // `publish_tools().await` has already registered the handler, so RPC
    // routing needs no capability-fold discovery — the invoke-retry loop
    // covers only the reply-channel first-reply race.
    let add_body = serde_json::to_vec(&json!({ "a": 2, "b": 3 })).unwrap();

    // (1) UNPAID invoke → refused before the invoker, with the
    //     `missing_quote` admission schematic on the reply header.
    let refusal = invoke_tool(&caller_mesh, provider_id, "add", &add_body, vec![])
        .await
        .expect_err("an unpaid wrapped invoke must be refused");
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
    assert_eq!(
        invoker.calls.load(Ordering::SeqCst),
        0,
        "the invoker never runs on an unpaid call"
    );

    // (2) PAY over the wire, then invoke `add` WITH the quote +
    //     possession proof: the wrap handler redeems through the engine
    //     admission and the invoker serves exactly once.
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

    let paid_headers = vec![
        (HDR_PAYMENT_QUOTE.to_string(), quote_id.clone().into_bytes()),
        (HDR_PAYMENT_BINDING.to_string(), binding),
    ];
    let reply = invoke_tool(
        &caller_mesh,
        provider_id,
        "add",
        &add_body,
        paid_headers.clone(),
    )
    .await
    .expect("the paid wrapped invoke serves");
    let result: CallToolResult = serde_json::from_slice(&reply).expect("decode add result");
    assert_eq!(result.text(), "5");
    assert_eq!(
        invoker.calls.load(Ordering::SeqCst),
        1,
        "the paid invoke ran the invoker once"
    );

    // (3) REPLAY the same proof → refused `already_redeemed`.
    let replay = invoke_tool(&caller_mesh, provider_id, "add", &add_body, paid_headers)
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
    assert_eq!(schematic.prior_payment, "consumed");
    assert!(schematic.recovery.safe_to_requote);
    assert!(!schematic.recovery.safe_to_retry);

    // (4) REUSE the `add` quote on `echo` → refused `wrong_tool_binding`
    //     (bearer reuse: a present binding would fail the possession
    //     check first and mask the tool-binding verdict).
    let echo_body = serde_json::to_vec(&json!({ "message": "hi" })).unwrap();
    let bearer = vec![(HDR_PAYMENT_QUOTE.to_string(), quote_id.into_bytes())];
    let misdirected = invoke_tool(&caller_mesh, provider_id, "echo", &echo_body, bearer)
        .await
        .expect_err("a quote never redeems for a different wrapped tool");
    assert_eq!(misdirected.status, ERR_PAYMENT);
    let schematic = misdirected
        .schematic
        .expect("the wrong-tool refusal carries its schematic");
    assert_eq!(schematic.reason, "wrong_tool_binding");
    assert_eq!(schematic.recovery.class, "security_violation");
    assert!(!schematic.recovery.safe_to_requote);

    // (5) The invariant, stated as counts: the invoker ran once, billing
    //     recorded exactly one paid serve, verifying from a fresh
    //     verifier. Neither the replay nor the wrong-tool reuse touched
    //     either.
    assert_eq!(
        invoker.calls.load(Ordering::SeqCst),
        1,
        "exactly one invoker execution across the whole loop"
    );
    let recorded = provider_log.read_all().await.expect("read");
    assert_eq!(recorded.len(), 1, "exactly one billing event");
    assert_eq!(recorded[0].billing_event_id, billing.billing_event_id);
    recorded[0]
        .verify_signature()
        .expect("billing signature verifies");
}
