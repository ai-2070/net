//! `net.settlement.ref@1` — the Net envelope around an x402 settlement
//! response.
//!
//! Wraps the facilitator's settle response (byte-preserved) together with
//! the quote binding and the verifier/facilitator that produced it. This
//! is the object verification chains hang off and billing events point
//! at.

use net::adapter::net::identity::EntityId;
use serde::{Deserialize, Serialize};

use super::canonical::{EnvelopeError, ExtraFields, SignatureHex, SignedEnvelope};
use super::verification::VerifierRef;
use super::versioning::{ensure_tag, TAG_SETTLEMENT_REF};
use crate::x402::settlement::SettlementResponse;
use crate::x402::X402Carry;

/// The signed settlement-reference envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettlementRef {
    /// Always [`TAG_SETTLEMENT_REF`].
    pub object: String,
    /// The quote this settlement satisfies.
    pub quote_id: String,
    /// The x402 settlement response, byte-preserved.
    pub settlement: X402Carry<SettlementResponse>,
    /// Convenience mirror of `settlement.transaction` for indexing;
    /// integrity-checked against the carry on decode.
    pub transaction: String,
    /// CAIP-2 network, mirrored from the response likewise.
    pub network: String,
    /// The facilitator that executed settle — a named dependency,
    /// recorded per result.
    pub facilitator: VerifierRef,
    /// Signer clock, ns since epoch.
    pub settled_at_ns: u64,
    /// The identity that ran settlement (provider side in P0) and signs
    /// this record.
    pub signer: EntityId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureHex>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl SettlementRef {
    /// Build an unsigned ref from a settle response, mirroring the
    /// indexed fields from the carry so they cannot disagree.
    pub fn new(
        quote_id: impl Into<String>,
        settlement: X402Carry<SettlementResponse>,
        facilitator: VerifierRef,
        settled_at_ns: u64,
        signer: EntityId,
    ) -> Self {
        let transaction = settlement.view().transaction.clone();
        let network = settlement.view().network.clone();
        Self {
            object: TAG_SETTLEMENT_REF.to_string(),
            quote_id: quote_id.into(),
            settlement,
            transaction,
            network,
            facilitator,
            settled_at_ns,
            signer,
            signature: None,
            extra: ExtraFields::new(),
        }
    }

    /// Decode + verify tag, mirror integrity, and signature.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        let sref: Self =
            serde_json::from_slice(bytes).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
        ensure_tag(TAG_SETTLEMENT_REF, &sref.object)?;
        if sref.transaction != sref.settlement.view().transaction
            || sref.network != sref.settlement.view().network
        {
            return Err(EnvelopeError::Field(
                "settlement ref mirrors disagree with the carried x402 response".into(),
            ));
        }
        sref.verify_signature()?;
        Ok(sref)
    }
}

impl SignedEnvelope for SettlementRef {
    const OBJECT_TAG: &'static str = TAG_SETTLEMENT_REF;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canonical::canonical_bytes;
    use net::adapter::net::identity::EntityKeypair;

    fn settle_carry() -> X402Carry<SettlementResponse> {
        let json =
            r#"{"success":true,"transaction":"0xfeed","network":"mock:net","amount":"2500"}"#;
        X402Carry::from_bytes(json.as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn round_trips_and_pins_mirrors_to_the_carry() {
        let kp = EntityKeypair::generate();
        let mut sref = SettlementRef::new(
            "q1",
            settle_carry(),
            VerifierRef {
                identity: None,
                endpoint: "mock".into(),
            },
            7,
            kp.entity_id().clone(),
        );
        sref.sign_with(&kp).unwrap();
        let bytes = canonical_bytes(&sref).unwrap();
        let back = SettlementRef::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.transaction, "0xfeed");

        // A tampered mirror (indexing a different tx than the signed
        // response) is rejected before the signature is even consulted.
        let mut tampered = back.clone();
        tampered.transaction = "0xother".into();
        tampered.sign_with(&kp).unwrap();
        let bytes = canonical_bytes(&tampered).unwrap();
        assert!(SettlementRef::from_json_bytes(&bytes).is_err());
    }
}
