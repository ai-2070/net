//! A provider must not announce a price its settlement backend cannot
//! honour.
//!
//! The registry check and this one answer different questions. The
//! registry says the *asset* is one this provider knows; `/supported`
//! says the *route* — the `(scheme, network)` pair — is one its
//! facilitator will actually settle. A provider that passes the first and
//! fails the second publishes terms, signs a quote under them, and finds
//! out at verify/settle time — after the caller has handed over a signed
//! authorization.

use std::sync::Arc;

use async_trait::async_trait;
use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::VerifierRef;
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::facilitator::{Facilitator, FacilitatorError, SettleOutcome, VerifyOutcome};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

/// A facilitator that answers `/supported` with a scripted pair list —
/// the discovery surface a real HTTP facilitator has and the mock does
/// not.
struct Announcing {
    pairs: Vec<(String, String)>,
}

#[async_trait]
impl Facilitator for Announcing {
    fn reference(&self) -> VerifierRef {
        VerifierRef {
            identity: None,
            endpoint: "announcing-fixture".into(),
        }
    }

    async fn verify(
        &self,
        _p: &X402Carry<PaymentPayload>,
        _r: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        unreachable!("these tests never reach a payment")
    }

    async fn settle(
        &self,
        _p: &X402Carry<PaymentPayload>,
        _r: &X402Carry<PaymentRequirements>,
    ) -> Result<SettleOutcome, FacilitatorError> {
        unreachable!("these tests never reach a payment")
    }

    async fn supported_pairs(&self) -> Result<Option<Vec<(String, String)>>, FacilitatorError> {
        Ok(Some(self.pairs.clone()))
    }
}

fn engine_with(facilitator: Arc<dyn Facilitator>) -> (PaymentEngine, tempfile::TempDir) {
    let provider = Arc::new(EntityKeypair::generate());
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = PaymentEngine::new(
        provider.clone(),
        facilitator,
        Arc::new(AdmitAll),
        default_mock_registry(provider.entity_id().clone()),
        dir.path().join("engine.json"),
    )
    .expect("engine");
    (engine, dir)
}

fn requirement(scheme: &str, network: &str) -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: scheme.into(),
        network: network.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author")
}

#[tokio::test]
async fn a_route_the_backend_settles_is_announceable() {
    let (engine, _dir) = engine_with(Arc::new(Announcing {
        pairs: vec![(MOCK_SCHEME.into(), MOCK_NETWORK.into())],
    }));
    engine
        .check_settlement_routes(&[requirement(MOCK_SCHEME, MOCK_NETWORK)])
        .await
        .expect("the backend offers this pair");
}

#[tokio::test]
async fn a_route_the_backend_does_not_settle_is_refused_before_it_is_announced() {
    // The facilitator settles Base, the provider is about to advertise
    // Base Sepolia. Both are real routes; neither is a registry problem.
    let (engine, _dir) = engine_with(Arc::new(Announcing {
        pairs: vec![("exact".into(), "eip155:8453".into())],
    }));
    let err = engine
        .check_settlement_routes(&[requirement("exact", "eip155:84532")])
        .await
        .expect_err("announcing an unsettleable route must fail");
    let message = err.to_string();
    assert!(
        message.contains("eip155:84532"),
        "the refusal must name the route it refused: {message}"
    );
    assert!(
        message.contains("eip155:8453"),
        "and what the backend does offer, or an operator cannot fix it: {message}"
    );
}

/// One bad entry among good ones still fails: a caller picking that entry
/// gets refused with nothing to fall back to, which is the whole failure
/// mode.
#[tokio::test]
async fn every_announced_route_has_to_clear_it_not_just_one() {
    let (engine, _dir) = engine_with(Arc::new(Announcing {
        pairs: vec![(MOCK_SCHEME.into(), MOCK_NETWORK.into())],
    }));
    engine
        .check_settlement_routes(&[
            requirement(MOCK_SCHEME, MOCK_NETWORK),
            requirement("exact", "eip155:8453"),
        ])
        .await
        .expect_err("the second entry is unsettleable");
}

/// A backend with no discovery surface passes.
///
/// `None` means "cannot say", never "supports nothing". `Facilitator` is
/// a public trait and the default returns `None`, so refusing on silence
/// would turn every implementation without a `/supported` endpoint —
/// starting with the mock the whole conformance suite runs on — into a
/// provider that cannot announce anything.
#[tokio::test]
async fn a_backend_that_cannot_say_what_it_settles_does_not_block_announcing() {
    let (engine, _dir) = engine_with(Arc::new(MockFacilitator::new()));
    engine
        .check_settlement_routes(&[
            requirement(MOCK_SCHEME, MOCK_NETWORK),
            requirement("exact", "eip155:8453"),
        ])
        .await
        .expect("silence is not a refusal");
}
