//! The independent verification checker — `confirmed(n)` and `final`.
//!
//! A facilitator receipt can only ever justify tier `observed` (the v2
//! spec gives facilitators no way to report finality). Everything above
//! comes from here: an independent check of the settlement transaction
//! against a chain RPC endpoint the *participant* configures — the
//! facilitator is never in the trust root for confidence.
//!
//! Doctrine: adapters map their chain semantics **into** the fixed tier
//! vocabulary (`observed | confirmed(n) | final`); chain-specific states
//! never leak upward. The checker also cross-checks the amount
//! **delivered** (never sent) where the chain exposes it — the exact-
//! amount policy's independent leg.

use async_trait::async_trait;

use crate::core::verification::{VerificationTier, VerifierRef};

#[cfg(feature = "http-facilitator")]
pub mod eip155;
#[cfg(feature = "http-facilitator")]
pub mod svm;
#[cfg(feature = "http-facilitator")]
mod transport;
#[cfg(feature = "http-facilitator")]
pub mod xrpl;

/// Checker failure (RPC unreachable, malformed answer). Retryability is
/// the checker's honest claim; policy decides whether to use it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("chain checker: {message} (retryable={retryable})")]
pub struct CheckerError {
    pub message: String,
    pub retryable: bool,
}

impl CheckerError {
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
    pub fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

/// What to cross-check delivery against: the token contract, the quoted
/// recipient, and — critically — the authorized payer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferQuery {
    /// The asset locator as the requirements carried it (token contract
    /// on eip155).
    pub token: String,
    /// The quoted `payTo` recipient (the indexed `to` topic).
    pub to: String,
    /// The authorized payer — the EIP-3009 `from` (the indexed `from`
    /// topic). When set, a `Transfer` only counts toward delivery if its
    /// `from` equals this payer. This binds delivery to *this quote's*
    /// authorization rather than to any qualifying transfer to the same
    /// merchant, so a facilitator cannot satisfy a quote by pointing at a
    /// different customer's payment. `None` leaves delivery bound only to
    /// (token, recipient) — for schemes/paths with no on-chain payer.
    pub from: Option<String>,
    /// The scheme's per-quote settlement reference, when the scheme has
    /// one (exact-XRPL's `invoiceId`, carried on-ledger as
    /// `MemoData`/`InvoiceID`). When set, an adapter that understands it
    /// counts delivery only from a transaction bound to it — the
    /// strongest per-quote bind available; adapters for schemes without
    /// a reference ignore it.
    pub reference: Option<String>,
    /// The recipient sub-account tag, when the chain has one (XRPL
    /// `DestinationTag` for shared-address merchants). When set, the
    /// matched transaction must carry exactly this tag; when `None`, a
    /// tag-aware adapter requires the transaction to carry *no* tag — a
    /// quote that authorized no sub-account must not be satisfied by a
    /// payment routed to one (M3). Adapters for chains without tags ignore
    /// it.
    pub to_tag: Option<u32>,
}

/// The chain's answer, in protocol vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainVerdict {
    /// Not (yet) included — no confidence claim either way. Re-check
    /// later; never an invalidation by itself.
    Pending,
    /// Included and successful, at the tier the depth justifies
    /// (`Confirmed(n)` or `Final` per the adapter's mapping), with the
    /// amount delivered to the queried recipient when observable.
    Included {
        tier: VerificationTier,
        delivered: Option<String>,
    },
    /// Included and **reverted** — the settlement did not happen. A
    /// first-class invalidation, same family as a reorg.
    Reverted,
}

/// The independent checker interface.
#[async_trait]
pub trait ChainChecker: Send + Sync {
    /// Who checked — recorded in every verification event this produces
    /// (`independent-chain-check:<endpoint>` by convention).
    fn reference(&self) -> VerifierRef;

    /// Check `transaction` on `network`, optionally cross-checking the
    /// delivered amount for `query`.
    async fn check(
        &self,
        network: &str,
        transaction: &str,
        query: Option<&TransferQuery>,
    ) -> Result<ChainVerdict, CheckerError>;
}
