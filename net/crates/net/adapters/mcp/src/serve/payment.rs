//! The caller-side payment seam of the gated invoke.
//!
//! The gate composition ([`super::gated::gated_invoke`]) knows exactly one
//! payment fact: a capability whose describe carries `pricing_terms` is
//! **paid**, and a paid capability must never reach the provider without a
//! cleared payment. *How* payment clears — spend policy, quote, x402
//! payload, settlement — is behind this trait, implemented by
//! `net-payments` (the dependency points payments → adapter surface,
//! never the reverse; the substrate and this adapter carry payment
//! objects opaquely).
//!
//! The trait's outcomes mirror the consent shape: policy either clears
//! silently, wants a human (`RequiresPaymentApproval` — same contract as
//! `requires_approval`, resolved through the SDK consent API), denies, or
//! reports the payment machinery unavailable. Fail-closed is structural:
//! with no flow configured, a paid capability is denied before the
//! provider ever sees the call.

use async_trait::async_trait;
use serde_json::Value;

use super::backend::CapabilityId;

/// Request header carrying the paid invocation's quote id — the binding
/// between the payment (settled out-of-band via the payment services)
/// and this invocation. Same convention as `net-delegation`. The quote
/// id is a bearer redemption token in P0: it is content-derived
/// (32-byte blake3 hex, unguessable without the quote) and the
/// provider's engine redeems it **at most once** — a signed invocation
/// binding is the P1 hardening.
pub const HDR_PAYMENT_QUOTE: &str = "net-payment-quote";

/// The caller-side proof that an invocation was paid: the quote id the
/// provider's engine can redeem. Attached to the invoke as
/// [`HDR_PAYMENT_QUOTE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentProof {
    pub quote_id: String,
}

/// The structured outcome of the caller-side paid lifecycle.
#[derive(Debug, Clone)]
pub enum PaymentFlowDecision {
    /// Payment cleared under policy (quote accepted, payload authored,
    /// settlement path run per the mode). `quote_id` binds the upcoming
    /// invocation to the settled payment; `proof` is the opaque payment
    /// context (settlement refs, the signed billing event) the gate
    /// passes through without reading.
    Paid { quote_id: String, proof: Value },
    /// Spend policy wants a human. Mirrors the consent shape; the
    /// decision resolves through the SDK consent API and the shared
    /// policy store — never through the model.
    RequiresPaymentApproval {
        quote_id: String,
        policy_reason: String,
        approve_hint: String,
    },
    /// Spend policy denies outright (e.g. a real network in P0) — no
    /// approval path.
    Denied { policy_reason: String },
    /// The payment machinery could not answer (facilitator failure,
    /// store I/O). Fail-closed; `retryable` is the flow's honest claim.
    Failed { message: String, retryable: bool },
}

/// The caller-side payment flow for paid capabilities.
#[async_trait]
pub trait PaymentFlow: Send + Sync {
    /// Run the paid-invocation lifecycle for `id` under the caller's
    /// spend policy. `pricing_terms` is the capability's announced
    /// `net.pricing.terms@1` canonical JSON; `tool_args` are the
    /// validated invocation arguments (for input-bound pricing).
    async fn pay(
        &self,
        id: &CapabilityId,
        pricing_terms: &str,
        tool_args: &Value,
    ) -> PaymentFlowDecision;
}

/// The provider-side payment admission for paid tools: redeem the
/// invocation's quote against the payment engine **before the handler
/// runs**. Implemented by `net-payments` over its lifecycle engine; the
/// adapter only knows the verdict vocabulary.
///
/// Contract the implementation must hold (the engine's tests pin it):
/// the quote must be settled and billed, not frozen (reorg), bound to
/// this `tool_id`, and never redeemed before — one payment, one serve.
/// At-most-once is deliberate: a paid invoke whose reply is lost is not
/// re-servable on the same quote (matching the at-most-once retry
/// safety of credentialed tools).
#[async_trait]
pub trait PaymentAdmission: Send + Sync {
    /// `Err(reason)` rejects the invocation; the reason travels to the
    /// caller as the payment-rejection application error.
    async fn redeem(&self, tool_id: &str, quote_id: &str) -> Result<(), String>;
}
