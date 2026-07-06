//! `net.payment.verification@1` — tiered, chained, immutable.
//!
//! Verification confidence is a tier, not a boolean, and the tier
//! vocabulary is a **fixed protocol enum**: `observed | confirmed(n) |
//! final`, canonical across all networks. Adapters map their chain
//! semantics *into* it (Solana commitment levels, EVM confirmations, XRPL
//! validation) rather than exporting chain-specific states into policy.
//! Facilitator receipt → `observed`/`confirmed(n)`; an independent
//! on-chain check of the tx hash → `final` — the facilitator never has to
//! be in anyone's trust root.
//!
//! Events chain per settlement ref and are never rewritten:
//! `invalidated {reason: reorg}` is a first-class outcome that freezes
//! further serving against the quote; overpayment is a verification
//! *exception* for provider policy, never auto-satisfied.

use net::adapter::net::identity::EntityId;
use serde::{Deserialize, Serialize};

use super::canonical::{canonical_bytes, EnvelopeError, ExtraFields, SignatureHex, SignedEnvelope};
use super::versioning::{ensure_tag, TAG_PAYMENT_VERIFICATION};

/// The fixed confidence vocabulary. Ordering: `Observed < Confirmed(n) <
/// Confirmed(n+1) < Final`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationTier {
    /// A facilitator (or adapter) saw the transaction; no depth claim.
    Observed,
    /// N confirmations / equivalent chain-native depth.
    Confirmed(u32),
    /// Independently checked on-chain finality.
    Final,
}

impl VerificationTier {
    fn rank(&self) -> (u8, u32) {
        match self {
            Self::Observed => (0, 0),
            Self::Confirmed(n) => (1, *n),
            Self::Final => (2, 0),
        }
    }

    /// Does this tier satisfy a policy minimum?
    pub fn satisfies(&self, minimum: &VerificationTier) -> bool {
        self.rank() >= minimum.rank()
    }
}

impl PartialOrd for VerificationTier {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.rank().cmp(&other.rank()))
    }
}

/// Why a previously-issued verification no longer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidationReason {
    /// The settlement's chain history was reorganized out.
    Reorg,
    /// The requirements/quote expired before the payment landed.
    Expired,
    /// The payload was already consumed against another quote.
    Replay,
}

/// Verification exceptions: outcomes that are neither pass nor fail and
/// go to provider policy for manual handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExceptionKind {
    /// More than the quoted amount was delivered. The verifier never
    /// auto-satisfies on overpayment; no automatic refunds exist in v1.
    Overpayment,
}

/// The outcome carried by one verification event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// The payment satisfies the quote at the event's tier.
    Verified,
    /// A prior verification is withdrawn. Serving against the quote
    /// freezes; billing events are never rewritten — adjustments reference
    /// them.
    Invalidated { reason: InvalidationReason },
    /// Manual-handling outcome for provider policy.
    Exception { kind: ExceptionKind },
}

/// Who performed the verify, recorded in every result — facilitator trust
/// is a named dependency, never ambient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierRef {
    /// Facilitator/checker identity when it has one on the mesh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<EntityId>,
    /// Endpoint or well-known name (`mock`, an HTTPS facilitator URL, or
    /// `independent-chain-check`).
    pub endpoint: String,
}

/// One immutable, signed verification event in a per-quote chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationEvent {
    /// Always [`TAG_PAYMENT_VERIFICATION`].
    pub object: String,
    /// The quote this verification speaks to.
    pub quote_id: String,
    /// Settlement transaction id, when one exists yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    /// Confidence tier at the time of this event.
    pub tier: VerificationTier,
    /// The outcome.
    pub status: VerificationStatus,
    /// Who verified.
    pub verifier: VerifierRef,
    /// Hex blake3 of the previous event's canonical bytes — the chain
    /// link. `None` only for the first event of a quote's chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    /// Signer clock, ns since epoch.
    pub checked_at_ns: u64,
    /// The identity signing this event (provider-side engine in P0).
    pub signer: EntityId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureHex>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl VerificationEvent {
    /// The chain-link hash of this event (over canonical bytes including
    /// the signature — a link commits to the signed fact, not just its
    /// content).
    pub fn chain_hash(&self) -> Result<String, EnvelopeError> {
        let bytes = canonical_bytes(self)?;
        Ok(hex::encode(blake3::hash(&bytes).as_bytes()))
    }

    /// Decode + verify tag and signature.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        let ev: Self =
            serde_json::from_slice(bytes).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
        ensure_tag(TAG_PAYMENT_VERIFICATION, &ev.object)?;
        ev.verify_signature()?;
        Ok(ev)
    }
}

impl SignedEnvelope for VerificationEvent {
    const OBJECT_TAG: &'static str = TAG_PAYMENT_VERIFICATION;
    fn signer(&self) -> &EntityId {
        &self.signer
    }
    fn signature(&self) -> Option<&SignatureHex> {
        self.signature.as_ref()
    }
    fn set_signature(&mut self, sig: SignatureHex) {
        self.signature = Some(sig);
    }
}

/// Chain-integrity check over a quote's ordered verification events:
/// every `prev` must equal the chain hash of its predecessor, and events
/// after an invalidation are a protocol violation (the engine freezes the
/// quote instead of verifying past it).
pub fn check_chain(events: &[VerificationEvent]) -> Result<(), EnvelopeError> {
    let mut prev_hash: Option<String> = None;
    let mut invalidated = false;
    for (i, ev) in events.iter().enumerate() {
        if invalidated {
            return Err(EnvelopeError::Field(format!(
                "event {i} follows an invalidation — serving against this quote is frozen"
            )));
        }
        if ev.prev != prev_hash {
            return Err(EnvelopeError::Field(format!(
                "event {i} chain link mismatch: prev={:?}, expected {:?}",
                ev.prev, prev_hash
            )));
        }
        prev_hash = Some(ev.chain_hash()?);
        if matches!(ev.status, VerificationStatus::Invalidated { .. }) {
            invalidated = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::adapter::net::identity::EntityKeypair;

    #[test]
    fn tier_ordering_is_the_protocol_order() {
        use VerificationTier::*;
        assert!(Observed < Confirmed(1));
        assert!(Confirmed(1) < Confirmed(6));
        assert!(Confirmed(1_000_000) < Final);
        assert!(Final.satisfies(&Confirmed(6)));
        assert!(!Observed.satisfies(&Confirmed(1)));
    }

    #[test]
    fn tier_wire_form_is_stable() {
        assert_eq!(
            serde_json::to_string(&VerificationTier::Observed).unwrap(),
            "\"observed\""
        );
        assert_eq!(
            serde_json::to_string(&VerificationTier::Confirmed(6)).unwrap(),
            "{\"confirmed\":6}"
        );
        assert_eq!(serde_json::to_string(&VerificationTier::Final).unwrap(), "\"final\"");
    }

    fn event(
        kp: &EntityKeypair,
        prev: Option<String>,
        status: VerificationStatus,
    ) -> VerificationEvent {
        let mut ev = VerificationEvent {
            object: TAG_PAYMENT_VERIFICATION.to_string(),
            quote_id: "q1".into(),
            transaction: Some("0xabc".into()),
            tier: VerificationTier::Observed,
            status,
            verifier: VerifierRef { identity: None, endpoint: "mock".into() },
            prev,
            checked_at_ns: 1,
            signer: kp.entity_id().clone(),
            signature: None,
            extra: ExtraFields::new(),
        };
        ev.sign_with(kp).unwrap();
        ev
    }

    #[test]
    fn chains_link_and_freeze_after_invalidation() {
        let kp = EntityKeypair::generate();
        let e1 = event(&kp, None, VerificationStatus::Verified);
        let e2 = event(&kp, Some(e1.chain_hash().unwrap()), VerificationStatus::Verified);
        check_chain(&[e1.clone(), e2.clone()]).unwrap();

        // Broken link.
        let orphan = event(&kp, Some("00".into()), VerificationStatus::Verified);
        assert!(check_chain(&[e1.clone(), orphan]).is_err());

        // Reorg invalidation is terminal for the chain.
        let invalidated = event(
            &kp,
            Some(e2.chain_hash().unwrap()),
            VerificationStatus::Invalidated { reason: InvalidationReason::Reorg },
        );
        let after = event(
            &kp,
            Some(invalidated.chain_hash().unwrap()),
            VerificationStatus::Verified,
        );
        check_chain(&[e1.clone(), e2.clone(), invalidated.clone()]).unwrap();
        assert!(check_chain(&[e1, e2, invalidated, after]).is_err());
    }

    #[test]
    fn events_round_trip_signed() {
        let kp = EntityKeypair::generate();
        let ev = event(&kp, None, VerificationStatus::Exception {
            kind: ExceptionKind::Overpayment,
        });
        let bytes = canonical_bytes(&ev).unwrap();
        let back = VerificationEvent::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.status, ev.status);
    }
}
