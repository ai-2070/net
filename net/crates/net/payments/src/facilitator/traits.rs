//! The facilitator client interface: x402 verify/settle.
//!
//! This is the interface `facilitator/client.rs` implements against real
//! facilitators in P1 and `facilitator/mock.rs` implements in-process for
//! P0 — the P1 acceptance test of the design is that pointing at a real
//! facilitator requires **zero interface changes**.
//!
//! Failure posture: verify/settle failures return structured, retryability-
//! tagged errors; policy chooses fail-closed (the default), a retry
//! window, or a fallback facilitator. Paid capabilities never silently
//! serve unverified.

use async_trait::async_trait;

use crate::core::verification::VerifierRef;
use crate::x402::payload::PaymentPayload;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::settlement::{SettlementResponse, VerifyResponse};
use crate::x402::X402Carry;

/// What went wrong at the facilitator boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacilitatorErrorKind {
    /// No answer inside the deadline.
    Timeout,
    /// Transport/availability failure.
    Unavailable,
    /// The facilitator answered outside the x402 protocol.
    Protocol,
    /// The facilitator answered and said no (terminal; the reason is in
    /// the response text).
    Rejected,
}

/// Structured facilitator failure. `retryable` is the facilitator
/// client's honest claim; whether to *use* the retry budget is policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("facilitator {kind:?} (retryable={retryable}): {message}")]
pub struct FacilitatorError {
    pub kind: FacilitatorErrorKind,
    pub retryable: bool,
    pub message: String,
}

impl FacilitatorError {
    pub fn timeout(message: impl Into<String>) -> Self {
        Self {
            kind: FacilitatorErrorKind::Timeout,
            retryable: true,
            message: message.into(),
        }
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: FacilitatorErrorKind::Unavailable,
            retryable: true,
            message: message.into(),
        }
    }
    pub fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: FacilitatorErrorKind::Protocol,
            retryable: false,
            message: message.into(),
        }
    }
    pub fn rejected(message: impl Into<String>) -> Self {
        Self {
            kind: FacilitatorErrorKind::Rejected,
            retryable: false,
            message: message.into(),
        }
    }
}

/// A verify result: the x402 response, byte-preserved.
///
/// **Carries no tier, by construction.** A facilitator receipt justifies
/// `observed` and nothing more — the x402 v2 spec gives facilitators no
/// way to report finality — so the engine mints
/// [`Observed`](crate::core::verification::VerificationTier::Observed)
/// for every facilitator answer rather
/// than reading a tier off one. `confirmed(n)` and `final` come only
/// from an independent on-chain check
/// ([`crate::checker::ChainChecker`]), which is what keeps a facilitator
/// out of the trust root for confidence.
///
/// This field used to exist, and only the shipped HTTP client clamped
/// itself to `observed`. `Facilitator` is a public trait, so any other
/// implementation could report `final` and satisfy a `final`-tier policy
/// with no chain check at all. Removing the field makes that
/// unrepresentable instead of discouraged.
#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    pub response: X402Carry<VerifyResponse>,
}

/// A settle result, likewise — and likewise tier-free.
#[derive(Debug, Clone)]
pub struct SettleOutcome {
    pub response: X402Carry<SettlementResponse>,
}

/// The verify/settle client interface (x402 `POST /verify`, `POST
/// /settle` semantics, transport-agnostic).
#[async_trait]
pub trait Facilitator: Send + Sync {
    /// Who this facilitator is, recorded in every verification result.
    fn reference(&self) -> VerifierRef;

    /// x402 verify: does `payload` satisfy `requirements`?
    async fn verify(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError>;

    /// x402 settle: execute the payment.
    async fn settle(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<SettleOutcome, FacilitatorError>;
}
