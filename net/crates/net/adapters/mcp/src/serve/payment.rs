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

/// The structured outcome of the caller-side paid lifecycle.
#[derive(Debug, Clone)]
pub enum PaymentFlowDecision {
    /// Payment cleared under policy (quote accepted, payload authored,
    /// settlement path run per the mode). `proof` is an opaque payment
    /// context the flow may want attached to the invocation (quote id +
    /// settlement refs); the gate passes it through without reading it.
    Paid { proof: Value },
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
