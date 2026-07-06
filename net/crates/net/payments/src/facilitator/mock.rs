//! The in-process mock facilitator — P0's settlement backbone, not a toy.
//!
//! Implements the real [`Facilitator`] interface against a `mock` scheme
//! on the `mock:net` CAIP-2 network. Every behavior a real network can
//! exhibit is injectable per quote and deterministic, which makes this
//! the conformance simulator every real network passes before real money
//! exists (doctrine: same lifecycle on every network).
//!
//! Modes are keyed by the content hash of the requirements carry — the
//! same bytes the quote binds — so a test can arm a behavior for exactly
//! one quote's lifecycle.

use std::collections::HashMap;

use async_trait::async_trait;
use parking_lot::Mutex;

use super::traits::{Facilitator, FacilitatorError, SettleOutcome, VerifyOutcome};
use crate::core::verification::{VerificationTier, VerifierRef};
use crate::x402::payload::PaymentPayload;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::settlement::{SettlementResponse, VerifyResponse};
use crate::x402::{X402Carry, X402Error};

/// The CAIP-2 network the mock facilitator settles on.
pub const MOCK_NETWORK: &str = "mock:net";
/// The x402 scheme the mock facilitator implements.
pub const MOCK_SCHEME: &str = "mock";

/// Injectable facilitator behaviors — one per failure class the P0
/// lifecycle must prove it survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MockMode {
    /// Verify passes, settle succeeds, tier `observed`.
    #[default]
    Success,
    /// Settle "succeeds" but delivers less than the requirements demanded
    /// (fee-shaped shortfall); verification of the delivered amount fails.
    WrongAmount,
    /// Settle succeeds at `observed`; re-verifies only reach
    /// `confirmed(1)` from the third call on.
    LateFinality,
    /// Settle succeeds and the first verify passes (receipt issued); every
    /// later verify reports the tx reorged out → the engine must emit
    /// `invalidated {reason: reorg}` and freeze the quote.
    ReorgInvalidate,
    /// The payload was already consumed against another quote.
    Replay,
    /// The requirements' validity window has passed.
    ExpiredRequirements,
    /// Verify never answers inside the deadline (structured retryable
    /// timeout — policy decides fail-closed / retry / fallback).
    VerificationTimeout,
}

#[derive(Default)]
struct QuoteState {
    verify_calls: u32,
}

/// The mock facilitator. Cheap to construct per test; `Send + Sync`.
pub struct MockFacilitator {
    default_mode: MockMode,
    modes: Mutex<HashMap<String, MockMode>>,
    state: Mutex<HashMap<String, QuoteState>>,
    /// Settled payments, keyed by **payload** content hash — the payment
    /// is the payload, not the requirements: two quotes for the same
    /// static-priced tool legitimately share requirements bytes.
    settled: Mutex<std::collections::HashSet<String>>,
}

impl MockFacilitator {
    pub fn new() -> Self {
        Self {
            default_mode: MockMode::Success,
            modes: Mutex::new(HashMap::new()),
            state: Mutex::new(HashMap::new()),
            settled: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Arm a behavior for the quote whose requirements carry has this
    /// content hash (see [`X402Carry::content_hash`]).
    pub fn arm(&self, requirements_hash: impl Into<String>, mode: MockMode) {
        self.modes.lock().insert(requirements_hash.into(), mode);
    }

    /// Set the default behavior for un-armed quotes.
    pub fn with_default_mode(mut self, mode: MockMode) -> Self {
        self.default_mode = mode;
        self
    }

    fn mode_for(&self, requirements_hash: &str) -> MockMode {
        self.modes
            .lock()
            .get(requirements_hash)
            .copied()
            .unwrap_or(self.default_mode)
    }

    /// Deterministic mock tx id: content-derived from the payload bytes.
    fn tx_for(payload: &X402Carry<PaymentPayload>) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"net.payments.mock.tx@1");
        hasher.update(payload.bytes());
        format!("mock:{}", hex::encode(&hasher.finalize().as_bytes()[..16]))
    }

    /// The mock speaks exactly one scheme on exactly one network; anything
    /// else is a protocol error, mirroring a real facilitator's posture.
    fn check_domain(requirements: &X402Carry<PaymentRequirements>) -> Result<(), FacilitatorError> {
        let view = requirements.view();
        if view.scheme != MOCK_SCHEME || view.network != MOCK_NETWORK {
            return Err(FacilitatorError::protocol(format!(
                "mock facilitator speaks scheme `{MOCK_SCHEME}` on `{MOCK_NETWORK}`, got `{}` on `{}`",
                view.scheme, view.network
            )));
        }
        Ok(())
    }

    fn author_verify(view: &VerifyResponse) -> Result<X402Carry<VerifyResponse>, FacilitatorError> {
        X402Carry::author(view).map_err(|e: X402Error| FacilitatorError::protocol(e.to_string()))
    }

    fn author_settle(
        view: &SettlementResponse,
    ) -> Result<X402Carry<SettlementResponse>, FacilitatorError> {
        X402Carry::author(view).map_err(|e: X402Error| FacilitatorError::protocol(e.to_string()))
    }

    fn invalid(reason: &str) -> VerifyResponse {
        VerifyResponse {
            is_valid: false,
            invalid_reason: Some(reason.to_string()),
            payer: None,
            extra: None,
        }
    }
}

impl Default for MockFacilitator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Facilitator for MockFacilitator {
    fn reference(&self) -> VerifierRef {
        VerifierRef { identity: None, endpoint: "mock".to_string() }
    }

    async fn verify(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        Self::check_domain(requirements)?;
        let key = requirements.content_hash();
        let mode = self.mode_for(&key);

        let calls = {
            let mut state = self.state.lock();
            let entry = state.entry(key.clone()).or_default();
            entry.verify_calls += 1;
            entry.verify_calls
        };

        // Scheme-level binding check every mode shares: the payload must
        // accept exactly these requirements bytes' terms.
        let bound = payload.view().accepted == *requirements.view();

        let (view, tier) = match mode {
            _ if !bound => (Self::invalid("payload_requirements_mismatch"), VerificationTier::Observed),
            MockMode::VerificationTimeout => {
                return Err(FacilitatorError::timeout(
                    "mock facilitator armed with verification_timeout",
                ))
            }
            MockMode::ExpiredRequirements => {
                (Self::invalid("expired_requirements"), VerificationTier::Observed)
            }
            MockMode::Replay => (Self::invalid("payload_replayed"), VerificationTier::Observed),
            MockMode::WrongAmount => (Self::invalid("wrong_amount"), VerificationTier::Observed),
            MockMode::ReorgInvalidate if calls > 1 => {
                (Self::invalid("reorged_out"), VerificationTier::Observed)
            }
            MockMode::LateFinality => {
                let tier = if calls >= 3 {
                    VerificationTier::Confirmed(1)
                } else {
                    VerificationTier::Observed
                };
                (
                    VerifyResponse { is_valid: true, invalid_reason: None, payer: None, extra: None },
                    tier,
                )
            }
            MockMode::Success | MockMode::ReorgInvalidate => (
                VerifyResponse { is_valid: true, invalid_reason: None, payer: None, extra: None },
                VerificationTier::Observed,
            ),
        };
        Ok(VerifyOutcome { response: Self::author_verify(&view)?, tier })
    }

    async fn settle(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<SettleOutcome, FacilitatorError> {
        Self::check_domain(requirements)?;
        let key = requirements.content_hash();
        let mode = self.mode_for(&key);

        {
            let mut settled = self.settled.lock();
            if !settled.insert(payload.content_hash()) && mode != MockMode::Replay {
                // A facilitator-side second settle of the same payment is
                // the replay class regardless of the armed mode.
                return Err(FacilitatorError::rejected("payment already settled"));
            }
        }

        let amount = &requirements.view().amount;
        let response = match mode {
            MockMode::Replay => SettlementResponse {
                success: false,
                error_reason: Some("payload_replayed".to_string()),
                payer: None,
                transaction: String::new(),
                network: MOCK_NETWORK.to_string(),
                amount: None,
                extensions: None,
            },
            MockMode::ExpiredRequirements => SettlementResponse {
                success: false,
                error_reason: Some("expired_requirements".to_string()),
                payer: None,
                transaction: String::new(),
                network: MOCK_NETWORK.to_string(),
                amount: None,
                extensions: None,
            },
            MockMode::WrongAmount => SettlementResponse {
                success: true,
                error_reason: None,
                payer: None,
                transaction: Self::tx_for(payload),
                network: MOCK_NETWORK.to_string(),
                // Deliver one unit short — deterministic shortfall the
                // delivered-amount check must catch.
                amount: Some(short_by_one(amount)),
                extensions: None,
            },
            MockMode::Success
            | MockMode::LateFinality
            | MockMode::ReorgInvalidate
            | MockMode::VerificationTimeout => SettlementResponse {
                success: true,
                error_reason: None,
                payer: None,
                transaction: Self::tx_for(payload),
                network: MOCK_NETWORK.to_string(),
                amount: Some(amount.clone()),
                extensions: None,
            },
        };
        Ok(SettleOutcome {
            response: Self::author_settle(&response)?,
            tier: VerificationTier::Observed,
        })
    }
}

/// `amount - 1` in string space, saturating at zero — no floats, no parse
/// beyond the canonical grammar.
fn short_by_one(amount: &str) -> String {
    match crate::core::units::AtomicAmount::parse(amount) {
        Ok(a) => a
            .checked_sub(&crate::core::units::AtomicAmount::from_u128(1))
            .unwrap_or_else(|_| crate::core::units::AtomicAmount::from_u128(0))
            .to_canonical_string(),
        Err(_) => "0".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requirements() -> X402Carry<PaymentRequirements> {
        X402Carry::author(&PaymentRequirements {
            scheme: MOCK_SCHEME.into(),
            network: MOCK_NETWORK.into(),
            amount: "2500".into(),
            asset: "musd".into(),
            pay_to: "mock-payee".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .unwrap()
    }

    fn payload_for(reqs: &X402Carry<PaymentRequirements>) -> X402Carry<PaymentPayload> {
        X402Carry::author(&PaymentPayload {
            x402_version: 2,
            resource: None,
            accepted: reqs.view().clone(),
            payload: serde_json::json!({"mock_authorization": "payer-1"}),
            extensions: None,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn success_mode_settles_and_verifies() {
        let f = MockFacilitator::new();
        let reqs = requirements();
        let pay = payload_for(&reqs);
        let v = f.verify(&pay, &reqs).await.unwrap();
        assert!(v.response.view().is_valid);
        let s = f.settle(&pay, &reqs).await.unwrap();
        assert!(s.response.view().success);
        assert_eq!(s.response.view().amount.as_deref(), Some("2500"));
        assert!(s.response.view().transaction.starts_with("mock:"));
    }

    #[tokio::test]
    async fn double_settle_is_rejected() {
        let f = MockFacilitator::new();
        let reqs = requirements();
        let pay = payload_for(&reqs);
        f.settle(&pay, &reqs).await.unwrap();
        let err = f.settle(&pay, &reqs).await.unwrap_err();
        assert_eq!(err.kind, super::super::traits::FacilitatorErrorKind::Rejected);
        assert!(!err.retryable);
    }

    #[tokio::test]
    async fn armed_modes_fire_per_quote_and_deterministically() {
        let f = MockFacilitator::new();
        let reqs = requirements();
        let pay = payload_for(&reqs);

        f.arm(reqs.content_hash(), MockMode::ReorgInvalidate);
        let first = f.verify(&pay, &reqs).await.unwrap();
        assert!(first.response.view().is_valid, "receipt issued first");
        let second = f.verify(&pay, &reqs).await.unwrap();
        assert_eq!(second.response.view().invalid_reason.as_deref(), Some("reorged_out"));

        // A different quote (different requirements bytes) is unaffected.
        let other = X402Carry::author(&PaymentRequirements {
            amount: "9999".into(),
            ..reqs.view().clone()
        })
        .unwrap();
        let other_pay = payload_for(&other);
        assert!(f.verify(&other_pay, &other).await.unwrap().response.view().is_valid);
    }

    #[tokio::test]
    async fn wrong_amount_delivers_short() {
        let f = MockFacilitator::new().with_default_mode(MockMode::WrongAmount);
        let reqs = requirements();
        let pay = payload_for(&reqs);
        let s = f.settle(&pay, &reqs).await.unwrap();
        assert_eq!(s.response.view().amount.as_deref(), Some("2499"));
    }

    #[tokio::test]
    async fn timeout_mode_is_structured_and_retryable() {
        let f = MockFacilitator::new().with_default_mode(MockMode::VerificationTimeout);
        let reqs = requirements();
        let pay = payload_for(&reqs);
        let err = f.verify(&pay, &reqs).await.unwrap_err();
        assert_eq!(err.kind, super::super::traits::FacilitatorErrorKind::Timeout);
        assert!(err.retryable);
    }

    #[tokio::test]
    async fn non_mock_domains_are_protocol_errors() {
        let f = MockFacilitator::new();
        let reqs = X402Carry::author(&PaymentRequirements {
            scheme: "exact".into(),
            network: "eip155:8453".into(),
            amount: "1".into(),
            asset: "0xusdc".into(),
            pay_to: "0xpayee".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .unwrap();
        let pay = payload_for(&reqs);
        assert!(f.verify(&pay, &reqs).await.is_err());
    }

    #[tokio::test]
    async fn mismatched_payload_fails_binding() {
        let f = MockFacilitator::new();
        let reqs = requirements();
        let other = X402Carry::author(&PaymentRequirements {
            amount: "1".into(),
            ..reqs.view().clone()
        })
        .unwrap();
        let pay = payload_for(&other);
        let v = f.verify(&pay, &reqs).await.unwrap();
        assert_eq!(
            v.response.view().invalid_reason.as_deref(),
            Some("payload_requirements_mismatch")
        );
    }
}
