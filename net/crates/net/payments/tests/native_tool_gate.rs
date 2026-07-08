//! `EngineToolPaymentGate` — the engine-backed implementation of the
//! SDK's native tool payment gate (`net_sdk::tool_payment::ToolPaymentGate`,
//! consumed by `Mesh::serve_tool_paid`). Semantics must be identical to
//! the MCP wrap path's `EnginePaymentAdmission`: a paid quote redeems
//! exactly once, bound to its tool; everything else is a structured
//! denial, fail-closed.
#![cfg(feature = "mesh")]

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::VerificationTier;
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::mesh::EngineToolPaymentGate;
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_sdk::tool_payment::ToolPaymentGate as _;

const NOW: u64 = 1_000_000_000_000_000;
const CAPABILITY: &str = "fixture-provider/fixture-tool";

async fn paid_quote_id(engine: &Arc<PaymentEngine>, caller: &EntityKeypair) -> String {
    let requirements = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author");
    let quote = engine
        .issue_quote(
            caller.entity_id().clone(),
            CAPABILITY,
            requirements,
            NOW,
            60_000_000_000,
        )
        .expect("quote");
    let payload = X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": "payer-1" }),
        extensions: None,
    })
    .expect("payload");
    let decision = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .expect("accept");
    assert!(matches!(decision, PaymentDecision::Served { .. }));
    quote.quote_id
}

#[tokio::test]
async fn the_engine_gate_redeems_a_paid_quote_exactly_once() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            default_mock_registry(provider.entity_id().clone()),
            dir.path().join("engine.json"),
        )
        .expect("engine"),
    );
    let quote_id = paid_quote_id(&engine, &caller).await;
    let gate = EngineToolPaymentGate::new(engine.clone());

    // A paid, billed, unfrozen quote bound to this tool: admitted once.
    gate.redeem("fixture-tool", &quote_id, None)
        .await
        .expect("first redemption admits");

    // At-most-once: the same quote never serves twice. The denial
    // carries both renderings — the unchanged human message and the
    // schematic with the mapping table's `already_redeemed` row (a
    // consumed instrument whose money moved: requote, don't retry).
    let err = gate
        .redeem("fixture-tool", &quote_id, None)
        .await
        .expect_err("a second redemption must be denied");
    assert!(err.message.contains("redeem"), "{}", err.message);
    assert_eq!(err.schematic.reason, "already_redeemed");
    assert_eq!(err.schematic.funds_moved, "yes");
    assert_eq!(err.schematic.prior_payment, "consumed");
    assert!(err.schematic.recovery.safe_to_requote);
    assert!(!err.schematic.recovery.safe_to_retry);

    // Bound to the capability's tool: another tool never redeems it —
    // a security row: do not retry, do not just buy another quote.
    let fresh = paid_quote_id(&engine, &caller).await;
    let err = gate
        .redeem("some-other-tool", &fresh, None)
        .await
        .expect_err("a quote never redeems for a different tool");
    assert!(!err.message.is_empty());
    assert_eq!(err.schematic.reason, "wrong_tool_binding");
    assert_eq!(err.schematic.recovery.class, "security_violation");
    assert!(!err.schematic.recovery.safe_to_requote);

    // An unknown quote is a structured denial, not a panic.
    let err = gate
        .redeem("fixture-tool", "no-such-quote", None)
        .await
        .expect_err("unknown quote denied");
    assert!(err.message.contains("unknown quote"), "{}", err.message);
    assert_eq!(err.schematic.reason, "unknown_quote");
}

/// A store failure (here: a corrupted state file) fails closed with a
/// GENERIC caller-facing message. The raw `EngineError` wraps `StoreError`,
/// whose `Corrupt { path, .. }` / `io` variants carry the on-disk path and
/// serde detail — none of which may travel to an SDK/MCP caller.
#[tokio::test]
async fn a_store_failure_fails_closed_without_leaking_internal_detail() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("engine.json");
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            default_mock_registry(provider.entity_id().clone()),
            state_path.clone(),
        )
        .expect("engine"),
    );
    let quote_id = paid_quote_id(&engine, &caller).await;
    let gate = EngineToolPaymentGate::new(engine.clone());

    // Corrupt the on-disk store: the next redeem's `mutate_json` load fails
    // with a `StoreError::Corrupt { path, .. }` instead of reaching a
    // decision — the exact path that used to interpolate the raw error.
    std::fs::write(&state_path, b"{ not-valid-json").expect("corrupt the store");

    let err = gate
        .redeem("fixture-tool", &quote_id, None)
        .await
        .expect_err("a store failure must fail closed");

    // Fail-closed AND scrubbed: exactly the generic verdict, with no file
    // path, tempdir, or serde/StoreError internals leaked to the caller.
    assert_eq!(err.message, "payment engine unavailable (fail-closed)");
    assert!(
        !err.message.contains("engine.json"),
        "leaked store path: {}",
        err.message
    );
    assert!(
        !err.message.contains(dir.path().to_string_lossy().as_ref()),
        "leaked tempdir path: {}",
        err.message
    );
    assert!(
        !err.message.to_lowercase().contains("json")
            && !err.message.to_lowercase().contains("corrupt"),
        "leaked parser detail: {}",
        err.message
    );

    // The schematic obeys the same scrub by construction — it is
    // rendered from NOTHING but the generic verdict, and its serialized
    // bytes carry no store path, tempdir, or parser detail either.
    assert_eq!(err.schematic.reason, "engine_unavailable");
    let json = String::from_utf8(
        err.schematic
            .to_header_bytes()
            .expect("the generic schematic always fits"),
    )
    .expect("schematic is UTF-8");
    assert!(!json.contains("engine.json"), "leaked store path: {json}");
    assert!(
        !json.contains(dir.path().to_string_lossy().as_ref()),
        "leaked tempdir path: {json}"
    );
    assert!(
        !json.to_lowercase().contains("corrupt") && !json.contains("not-valid"),
        "leaked parser detail: {json}"
    );
}

/// A `tracing::Layer` that records every event's fields as (name, value)
/// pairs — precise enough to assert the emit site's *structured* fields
/// by key + value, not a formatted-string substring.
struct FieldCapture {
    fields: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FieldCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Collector<'a>(&'a mut Vec<(String, String)>);
        impl tracing::field::Visit for Collector<'_> {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.push((field.name().to_string(), value.to_string()));
            }
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                // `%x` (Display) records here via a wrapper whose Debug is
                // the display string — no quotes to strip.
                self.0
                    .push((field.name().to_string(), format!("{value:?}")));
            }
        }
        let mut buf = self.fields.lock().unwrap();
        event.record(&mut Collector(&mut buf));
    }
}

/// The redeem-denial emit site carries typed fields, not prose: operators
/// grep `reason` / `stage` / `recovery_class`, so those are a captured
/// contract (the Tier-4 "logs" projection cell of `PAYMENTS_TEST_MATRIX.md`).
/// `redeem_via_engine` renders every gate denial through one
/// `tracing::info!` — an unknown quote is the simplest trigger. Sync +
/// current-thread runtime so the emit fires on the same thread the
/// capturing subscriber is the default for.
#[test]
fn a_redeem_denial_emits_the_typed_tracing_fields() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let fields = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(FieldCapture {
        fields: fields.clone(),
    });

    tracing::subscriber::with_default(subscriber, || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let provider = Arc::new(EntityKeypair::generate());
            let dir = tempfile::tempdir().expect("tempdir");
            let engine = Arc::new(
                PaymentEngine::new(
                    provider.clone(),
                    Arc::new(MockFacilitator::new()),
                    Arc::new(AdmitAll),
                    default_mock_registry(provider.entity_id().clone()),
                    dir.path().join("engine.json"),
                )
                .expect("engine"),
            );
            let gate = EngineToolPaymentGate::new(engine);
            let err = gate
                .redeem("fixture-tool", "no-such-quote", None)
                .await
                .expect_err("an unknown quote is denied");
            assert_eq!(err.schematic.reason, "unknown_quote");
        });
    });

    let captured = fields.lock().unwrap();
    let has = |k: &str, v: &str| captured.iter().any(|(name, value)| name == k && value == v);
    assert!(
        has("reason", "unknown_quote"),
        "the denial's typed reason rides the trace: {captured:?}"
    );
    assert!(
        has("stage", "redeem"),
        "the lifecycle stage rides the trace"
    );
    assert!(
        has("recovery_class", "new_quote_required"),
        "the recovery class rides the trace"
    );
    assert!(
        has("tool_id", "fixture-tool"),
        "the tool id rides the trace"
    );
    // The message stays prose; the verdict is the structured fields.
    assert!(
        captured
            .iter()
            .any(|(name, value)| name == "message" && value == "payment redemption denied"),
        "the human message is a field, the verdict is the typed fields"
    );
}
