//! `net.billing.event@1` — the signed usage record. x402 has no
//! equivalent; this is Net's value-add.
//!
//! Definition, verbatim (repeated in spec and SDK docs, by doctrine): **a
//! billing event is a signed technical record linking invocation, quote,
//! settlement verification, and amount — input to accounting systems,
//! never an accounting artifact itself.** Never `net.invoice.*`,
//! `net.tax.*`, or `net.receipt.*`.
//!
//! Billing events are immutable: later invalidation/adjustment/refund/
//! dispute events *reference* them; nothing is rewritten. Event-sourced
//! all the way down.

use net::adapter::net::identity::EntityId;
use serde::{Deserialize, Serialize};

use super::canonical::{EnvelopeError, ExtraFields, SignatureHex, SignedEnvelope};
use super::units::AtomicAmount;
use super::versioning::{ensure_tag, TAG_BILLING_EVENT};

/// One immutable billing event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingEvent {
    /// Always [`TAG_BILLING_EVENT`].
    pub object: String,
    /// Content-derived event id (hex; see [`BillingEvent::derive_id`]).
    /// Same-idempotency-key retries produce the same id — one charge, one
    /// billing event.
    pub billing_event_id: String,
    /// The idempotency key (`{caller, provider, capability, quote}`
    /// scoped, see [`crate::core::idempotency`]).
    pub idempotency_key: String,
    /// Capability id in display form.
    pub capability: String,
    /// The quote this charge satisfies.
    pub quote_id: String,
    /// Settlement transaction id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    /// Chain hash of the verification event this billing event was
    /// emitted under (audit path into the verification chain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_ref: Option<String>,
    /// Paying identity.
    pub payer: EntityId,
    /// Paid identity (also the signer: billing events are the provider's
    /// signed statement of usage in P0; callers persist their own copy).
    pub payee: EntityId,
    /// CAIP-2 network the settlement rode.
    pub network: String,
    /// The x402 asset locator, as carried in the requirements.
    pub asset: String,
    /// Amount **delivered**, atomic units.
    pub amount: AtomicAmount,
    /// Signer clock, ns since epoch.
    pub occurred_at_ns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureHex>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

const BILLING_ID_DOMAIN: &[u8] = b"net.billing.event@1.id";

impl BillingEvent {
    /// Derive the event id from the idempotency key. Deliberately *not*
    /// salted with time: a same-key retry that reaches emission twice
    /// produces the same id, and the store treats it as the same event.
    pub fn derive_id(idempotency_key: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(BILLING_ID_DOMAIN);
        hasher.update(idempotency_key.as_bytes());
        hex::encode(hasher.finalize().as_bytes())
    }

    /// Decode + verify tag, id derivation, and signature.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        let ev: Self =
            serde_json::from_slice(bytes).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
        ensure_tag(TAG_BILLING_EVENT, &ev.object)?;
        if ev.billing_event_id != Self::derive_id(&ev.idempotency_key) {
            return Err(EnvelopeError::Field(
                "billing_event_id does not derive from the idempotency key".into(),
            ));
        }
        ev.verify_signature()?;
        Ok(ev)
    }
}

impl SignedEnvelope for BillingEvent {
    const OBJECT_TAG: &'static str = TAG_BILLING_EVENT;
    fn signer(&self) -> &EntityId {
        &self.payee
    }
    fn signature(&self) -> Option<&SignatureHex> {
        self.signature.as_ref()
    }
    fn set_signature(&mut self, sig: SignatureHex) {
        self.signature = Some(sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canonical::canonical_bytes;
    use net::adapter::net::identity::EntityKeypair;

    fn event(kp: &EntityKeypair, idem_key: &str) -> BillingEvent {
        let mut ev = BillingEvent {
            object: TAG_BILLING_EVENT.to_string(),
            billing_event_id: BillingEvent::derive_id(idem_key),
            idempotency_key: idem_key.to_string(),
            capability: "prov/fixture-tool".into(),
            quote_id: "q1".into(),
            transaction: Some("0xabc".into()),
            verification_ref: None,
            payer: EntityKeypair::generate().entity_id().clone(),
            payee: kp.entity_id().clone(),
            network: "mock:net".into(),
            asset: "musd".into(),
            amount: AtomicAmount::from_u128(2_500),
            occurred_at_ns: 42,
            signature: None,
            extra: ExtraFields::new(),
        };
        ev.sign_with(kp).unwrap();
        ev
    }

    #[test]
    fn same_idempotency_key_same_event_id() {
        let kp = EntityKeypair::generate();
        assert_eq!(event(&kp, "k1").billing_event_id, event(&kp, "k1").billing_event_id);
        assert_ne!(event(&kp, "k1").billing_event_id, event(&kp, "k2").billing_event_id);
    }

    #[test]
    fn round_trips_and_rejects_forged_ids() {
        let kp = EntityKeypair::generate();
        let ev = event(&kp, "k1");
        let bytes = canonical_bytes(&ev).unwrap();
        let back = BillingEvent::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.amount, ev.amount);

        let mut forged = ev.clone();
        forged.billing_event_id = BillingEvent::derive_id("other-key");
        forged.sign_with(&kp).unwrap();
        let bytes = canonical_bytes(&forged).unwrap();
        assert!(BillingEvent::from_json_bytes(&bytes).is_err());
    }
}
