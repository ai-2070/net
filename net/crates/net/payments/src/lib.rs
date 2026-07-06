//! # net-payments — x402-native payments for the Net mesh
//!
//! x402 v2 (Linux Foundation; CAIP-2/CAIP-19 identifiers; scheme-per-chain;
//! facilitator verify/settle) is the payment wire format, carried **verbatim
//! and byte-preserved** inside Net-signed envelopes. Net adds only what x402
//! lacks: provider identity signatures (know *who* you're paying, not just
//! which domain), pricing announced at discovery time, tiered verification,
//! immutable billing events, and the policy/budget layer. x402 moves the
//! money; Net signs the commercial facts around it.
//!
//! Category line (verbatim, per the plan): Net standardizes the commercial
//! facts around capability invocation; it does not intermediate the money.
//! It does not custody funds, process payments, issue invoices, determine
//! taxes, or clear transactions.
//!
//! ## The one rule that kills the envelope-drift bug class
//!
//! x402 structures are parsed and validated but **never re-serialized
//! through these types for signing**. Net signs around the original bytes
//! ([`x402::X402Carry`]). A payments PR that re-serializes x402 through Net
//! types is a rejected PR (review invariant).
//!
//! ## Module map
//!
//! - [`x402`] — verbatim v2 structures, byte-preserving carry, CAIP parsing.
//!   All x402 spec churn is quarantined here.
//! - [`core`] — the Net envelopes (`net.pricing.terms@1`,
//!   `net.payment.quote@1`, `net.settlement.ref@1`,
//!   `net.payment.verification@1`, `net.billing.event@1`), units, the asset
//!   registry, idempotency, canonicalization, versioning.
//! - [`facilitator`] — the verify/settle client interface, the mock
//!   facilitator (P0), and the real-facilitator client (P1).
//! - [`engine`] — the provider-side lifecycle engine: quote issuance
//!   under provider policy, verify/settle orchestration, the consumed-
//!   payload replay index, idempotent completion, signed verification
//!   chains with reorg freeze, billing emission.
//! - [`policy`] — the locked payment state store (pins pattern) + spend
//!   policy engine.
//!
//! Pinned x402 revision for P0 fixtures: `specs/x402-specification-v2.md`
//! at x402-foundation/x402 commit `087922a5eecc06ea773636b75df205814ba295b5`
//! (2026-05-29). Fixtures are version-pinned per supported spec revision
//! (`tests/cross_lang_payments/fixtures/x402/v2.0/...`), never "latest".

pub mod billing;
pub mod core;
pub mod engine;
pub mod facilitator;
pub mod policy;
pub mod x402;

pub use crate::core::billing_event::BillingEvent;
pub use crate::engine::{PaymentDecision, PaymentEngine};
pub use crate::core::quote::PaymentQuote;
pub use crate::core::terms::PricingTerms;
pub use crate::core::units::AtomicAmount;
pub use crate::core::verification::{VerificationEvent, VerificationTier};
pub use crate::x402::caip::{AssetId, ChainId};
pub use crate::x402::X402Carry;
