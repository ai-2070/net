//! The payment policy layer: one engine, one implementation.
//!
//! Caller-side spend policy runs before anything leaves; provider-side
//! policy runs at quote issuance and re-checks before the handler fires.
//! The model never decides payment policy — it requests invocation; this
//! engine enforces; approvals render in agent UX and the decision lives
//! in shared policy state.
//!
//! - [`store`] — the locked per-user policy store (pins-pattern:
//!   sidecar-lock + atomic temp+rename + lock-held RMW).
//! - [`spend`] — the spend-policy vocabulary and decision engine.

pub mod spend;
pub mod store;
