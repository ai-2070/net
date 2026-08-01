//! M3: a quote id is a **credential**, not just an identifier.
//!
//! With bearer redemption (no invocation binding presented), possession
//! of the quote id is sufficient to consume the paid invocation — see
//! `PaymentEngine::redeem_for_invocation`. So the caller-side flow must
//! not put one in a log line: every sink that scrapes the process would
//! hold a spendable credential, and the sites that log are *failure*
//! paths, where the quote is most likely still unredeemed — exactly when
//! leaking it costs something.
//!
//! Operators reading these lines need to correlate events, not to
//! reconstruct the id, so they get a short non-authorizing `quote_ref`.

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::billing_event::BillingEvent;
use net_payments::core::canonical::SignedEnvelope as _;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::terms::PricingTerms;
use net_payments::core::units::AtomicAmount;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::flow::{
    CallerDecision, CallerPaymentFlow, Clock, InProcessProvider, ProviderChannel,
};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

const CAPABILITY: &str = "fixture-provider/fixture-tool";

struct TestClock(std::sync::atomic::AtomicU64);
impl Clock for TestClock {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000, std::sync::atomic::Ordering::SeqCst)
    }
}

/// A `tracing::Layer` recording every event's fields as (name, value)
/// pairs — the `engine_retention.rs` / `native_tool_gate.rs` idiom,
/// precise enough to assert structured fields by key and value rather
/// than by substring on a formatted line.
struct FieldCapture {
    fields: Arc<parking_lot::Mutex<Vec<(String, String)>>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FieldCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // Every value type, not just str/debug. The central assertion
        // below is "no field carries the quote id" — a visitor with holes
        // would let a regression that logs it through another type slip
        // past silently, which is the one failure mode this test exists
        // to prevent. `record_debug` is the catch-all `tracing` falls
        // back to, but the typed methods are called in preference to it,
        // so each has to be captured explicitly.
        struct Collector<'a>(&'a mut Vec<(String, String)>);
        impl Collector<'_> {
            fn put(&mut self, field: &tracing::field::Field, value: impl std::fmt::Display) {
                self.0.push((field.name().to_string(), value.to_string()));
            }
        }
        impl tracing::field::Visit for Collector<'_> {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.put(field, value);
            }
            fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
                self.put(field, value);
            }
            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                self.put(field, value);
            }
            fn record_i128(&mut self, field: &tracing::field::Field, value: i128) {
                self.put(field, value);
            }
            fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
                self.put(field, value);
            }
            fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
                self.put(field, value);
            }
            fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
                self.put(field, value);
            }
            fn record_bytes(&mut self, field: &tracing::field::Field, value: &[u8]) {
                self.0
                    .push((field.name().to_string(), format!("{value:?}")));
            }
            fn record_error(
                &mut self,
                field: &tracing::field::Field,
                value: &(dyn std::error::Error + 'static),
            ) {
                self.put(field, value);
            }
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0
                    .push((field.name().to_string(), format!("{value:?}")));
            }
        }
        let mut buf = self.fields.lock();
        event.record(&mut Collector(&mut buf));
    }
}

/// Pays honestly, then swaps the provider's billing event for one that is
/// well-formed and correctly signed but bound to a *different* quote —
/// driving the flow's "does not bind this quote/caller/provider" branch,
/// one of the sites that used to log the full quote id.
struct ForgedBillingProvider {
    inner: Arc<dyn ProviderChannel>,
    provider_keys: Arc<EntityKeypair>,
    /// The real paying identity, so the forged event differs from a valid
    /// one in exactly one field.
    caller_id: net::adapter::net::identity::EntityId,
}

#[async_trait::async_trait]
impl ProviderChannel for ForgedBillingProvider {
    async fn quote(
        &self,
        caller: &net::adapter::net::identity::EntityId,
        provider: &net::adapter::net::identity::EntityId,
        capability: &str,
        template: &X402Carry<PaymentRequirements>,
    ) -> Result<Vec<u8>, net_payments::flow::ChannelError> {
        self.inner
            .quote(caller, provider, capability, template)
            .await
    }

    async fn pay(
        &self,
        quote_bytes: &[u8],
        payload: &X402Carry<net_payments::x402::payload::PaymentPayload>,
    ) -> Result<net_payments::flow::PayResponse, net_payments::flow::ChannelError> {
        let real = self.inner.pay(quote_bytes, payload).await?;
        let net_payments::flow::PayResponse::Served { transaction, .. } = real else {
            return Ok(real);
        };
        // A self-consistent billing event for some OTHER quote: correct
        // tag, correct id derivation, correctly signed — and correct on
        // every other bind too, so the ONLY thing wrong with it is the
        // quote id.
        //
        // The payer must therefore be the real caller, not the provider.
        // Setting it to the provider would also trip the `payer == caller`
        // check, and the test would pass without ever exercising the
        // quote bind it names.
        let scope = net_payments::core::idempotency::IdempotencyScope {
            caller: self.caller_id.clone(),
            provider: self.provider_keys.entity_id().clone(),
            capability: CAPABILITY.to_string(),
            quote_id: "some-other-quote".to_string(),
        };
        let idem = scope.key();
        let mut forged = BillingEvent {
            object: net_payments::core::versioning::TAG_BILLING_EVENT.to_string(),
            billing_event_id: BillingEvent::derive_id(&idem),
            idempotency_key: idem,
            capability: CAPABILITY.to_string(),
            invocation_id: None,
            quote_id: "some-other-quote".to_string(),
            transaction: transaction.clone(),
            verification_ref: None,
            payer: self.caller_id.clone(),
            payee: self.provider_keys.entity_id().clone(),
            network: MOCK_NETWORK.to_string(),
            asset: "musd".to_string(),
            amount: AtomicAmount::from_u128(2500),
            occurred_at_ns: 1,
            signature: None,
            extra: Default::default(),
        };
        forged.sign_with(&self.provider_keys).expect("sign");
        Ok(net_payments::flow::PayResponse::Served {
            billing_event: String::from_utf8(
                net_payments::core::canonical::canonical_bytes(&forged).expect("canonical"),
            )
            .expect("utf8"),
            transaction,
        })
    }
}

/// The flow's warning paths carry a short, non-authorizing `quote_ref`,
/// and no field anywhere carries the quote id itself.
///
/// Sync + current-thread runtime so the emit lands on the thread the
/// capturing subscriber is default for.
#[test]
fn flow_warnings_carry_a_short_quote_ref_never_the_quote_id() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let fields = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(FieldCapture {
        fields: fields.clone(),
    });

    let paid_quote_id = tracing::subscriber::with_default(subscriber, || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let clock: Arc<dyn Clock> = Arc::new(TestClock(std::sync::atomic::AtomicU64::new(
                1_000_000_000_000_000,
            )));
            let provider_keys = Arc::new(EntityKeypair::generate());
            let registry = default_mock_registry(provider_keys.entity_id().clone());
            let engine = Arc::new(
                PaymentEngine::new(
                    provider_keys.clone(),
                    Arc::new(MockFacilitator::new()),
                    Arc::new(AdmitAll),
                    registry.clone(),
                    dir.path().join("engine.json"),
                )
                .expect("engine"),
            );
            let honest = Arc::new(InProcessProvider::new(engine, clock.clone()));
            let caller = Arc::new(EntityKeypair::generate());
            let forging = Arc::new(ForgedBillingProvider {
                inner: honest,
                provider_keys: provider_keys.clone(),
                caller_id: caller.entity_id().clone(),
            });

            let template = X402Carry::author(&PaymentRequirements {
                scheme: MOCK_SCHEME.into(),
                network: MOCK_NETWORK.into(),
                amount: "2500".into(),
                asset: "musd".into(),
                pay_to: "mock-provider-settle-addr".into(),
                max_timeout_seconds: 60,
                extra: None,
            })
            .expect("template");
            let terms = PricingTerms::new(
                provider_keys.entity_id().clone(),
                CAPABILITY,
                vec![template],
                registry.reference().expect("ref"),
            );
            let terms_json = String::from_utf8(
                net_payments::core::canonical::canonical_bytes(&terms).expect("canonical"),
            )
            .expect("utf8");

            let flow = CallerPaymentFlow::new(
                caller,
                SpendPolicyEngine::new(dir.path().join("spend.json"), SpendProfile::DevTest),
                registry,
                forging,
                clock,
            );
            let decision = flow.run(CAPABILITY, &terms_json).await;
            // The payment still succeeded — a bad evidence blob is not a
            // fund loss — but the proof drops the unverifiable event.
            let CallerDecision::Paid { quote_id, .. } = decision else {
                panic!("expected Paid, got {decision:?}");
            };
            quote_id
        })
    });

    let captured = fields.lock().clone();
    let refs: Vec<&(String, String)> = captured.iter().filter(|(k, _)| k == "quote_ref").collect();
    assert!(
        !refs.is_empty(),
        "the dropped-billing-event warning must emit a quote_ref: {captured:?}"
    );
    for (_, value) in &refs {
        assert_eq!(
            value.len(),
            16,
            "quote_ref is 8 bytes of hex, got `{value}`"
        );
        assert!(
            value.chars().all(|c| c.is_ascii_hexdigit()),
            "quote_ref must be hex, got `{value}`"
        );
        assert_ne!(
            value.as_str(),
            paid_quote_id.as_str(),
            "quote_ref must not be the quote id itself"
        );
    }
    assert!(
        !captured.iter().any(|(_, v)| v.contains(&paid_quote_id)),
        "no logged field may carry the full quote id — it is a bearer \
         credential while binding-free redemption is permitted: {captured:?}"
    );
}
