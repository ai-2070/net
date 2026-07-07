//! `net.pricing.terms@1` — pricing visible at discovery time.
//!
//! Terms ride in the capability announcement (as tool metadata), so a
//! caller learns the price of a capability from discovery — no 402
//! round-trip on the mesh. The embedded `accepts[]` entries are x402
//! `PaymentRequirements` **templates**: discovery/UX metadata, non-binding
//! until instantiated in a `net.payment.quote@1`. Billing and settlement
//! bind to quote-instantiated requirements only.
//!
//! Terms are not independently signed: they travel inside the signed
//! capability announcement (native path) or the bridge describe catalog,
//! and nothing binds to them — displaying a price never implies
//! authorization to spend it, and quoting re-states the price under the
//! provider's signature.

use serde::{Deserialize, Serialize};

use super::canonical::{EnvelopeError, ExtraFields};
use super::registry::RegistryRef;
use super::versioning::{ensure_tag, TAG_PRICING_TERMS};
use crate::x402::requirements::PaymentRequirements;
use crate::x402::X402Carry;
use net::adapter::net::identity::EntityId;

/// The discovery-time pricing envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricingTerms {
    /// Always [`TAG_PRICING_TERMS`].
    pub object: String,
    /// The provider identity that will issue quotes for these terms.
    pub provider: EntityId,
    /// Capability id in display form (`provider/capability`).
    pub capability: String,
    /// x402 `PaymentRequirements` templates, byte-preserved. One entry per
    /// acceptable `(scheme, network, asset)`; the caller's policy picks.
    pub accepts: Vec<X402Carry<PaymentRequirements>>,
    /// The registry revision these templates were authored under.
    pub asset_registry: RegistryRef,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl PricingTerms {
    /// Author terms for a capability.
    pub fn new(
        provider: EntityId,
        capability: impl Into<String>,
        accepts: Vec<X402Carry<PaymentRequirements>>,
        asset_registry: RegistryRef,
    ) -> Self {
        Self {
            object: TAG_PRICING_TERMS.to_string(),
            provider,
            capability: capability.into(),
            accepts,
            asset_registry,
            extra: ExtraFields::new(),
        }
    }

    /// Decode from JSON bytes with tag + shape validation.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        let terms: Self =
            serde_json::from_slice(bytes).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
        ensure_tag(TAG_PRICING_TERMS, &terms.object)?;
        if terms.accepts.is_empty() {
            return Err(EnvelopeError::Field(
                "net.pricing.terms@1 with an empty accepts[] prices nothing".into(),
            ));
        }
        Ok(terms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canonical::canonical_bytes;
    use net::adapter::net::identity::EntityKeypair;

    fn template() -> X402Carry<PaymentRequirements> {
        X402Carry::author(&PaymentRequirements {
            scheme: "mock".into(),
            network: "mock:net".into(),
            amount: "2500".into(),
            asset: "musd".into(),
            pay_to: "mock-provider-settle-addr".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .unwrap()
    }

    fn terms() -> PricingTerms {
        PricingTerms::new(
            EntityKeypair::generate().entity_id().clone(),
            "prov/fixture-tool",
            vec![template()],
            RegistryRef {
                version: "net-default-0".into(),
                hash: "00".into(),
            },
        )
    }

    #[test]
    fn terms_round_trip_with_byte_preserved_templates() {
        let t = terms();
        let bytes = canonical_bytes(&t).unwrap();
        let back = PricingTerms::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.accepts[0].bytes(), t.accepts[0].bytes());
        assert_eq!(canonical_bytes(&back).unwrap(), bytes);
    }

    #[test]
    fn wrong_tag_and_empty_accepts_reject() {
        let mut t = terms();
        t.object = "net.payment.quote@1".into();
        let bytes = canonical_bytes(&t).unwrap();
        assert!(PricingTerms::from_json_bytes(&bytes).is_err());

        let mut t = terms();
        t.accepts.clear();
        let bytes = canonical_bytes(&t).unwrap();
        assert!(PricingTerms::from_json_bytes(&bytes).is_err());
    }

    #[test]
    fn future_version_is_structured_unsupported_version() {
        let mut t = terms();
        t.object = "net.pricing.terms@9".into();
        let bytes = canonical_bytes(&t).unwrap();
        let err = PricingTerms::from_json_bytes(&bytes).unwrap_err();
        assert!(format!("{err}").contains("unsupported_version"));
    }
}
