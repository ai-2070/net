//! Scheme-specific payload authoring — the caller side of "scheme-per-
//! chain". Pure document builders: no keys, no signatures, no I/O.
//! Signing happens behind [`crate::flow::signer::SchemeSigner`], and the
//! only crypto that can ever run inside Net is the dev signer's,
//! feature-gated.

pub mod exact_evm;
pub mod exact_svm;
