//! The Net payment envelopes and their supporting machinery.
//!
//! Object model (envelopes around x402):
//!
//! | tag | role |
//! |---|---|
//! | `net.pricing.terms@1` | accepts[] templates at discovery; non-binding |
//! | `net.payment.quote@1` | provider-signed, instantiated requirements; binding |
//! | `net.settlement.ref@1` | wraps the x402 settle response + tx hash |
//! | `net.payment.verification@1` | tiered, chained, immutable |
//! | `net.billing.event@1` | the signed usage record |
//! | `net.payment.dispute@1` | reserved (P5); the tag exists, nothing else |
//!
//! There is no intent object — the client-signed x402 `PaymentPayload`
//! travels in the invocation envelope.

pub mod billing_event;
pub mod canonical;
pub mod idempotency;
pub mod quote;
pub mod registry;
pub mod settlement_ref;
pub mod terms;
pub mod units;
pub mod verification;
pub mod versioning;
