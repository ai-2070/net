//! `net.payment.quote@1` — the binding, provider-identity-signed offer.
//!
//! A quote is the moment pricing becomes a commercial fact: the provider
//! instantiates one of its announced templates into concrete x402
//! `PaymentRequirements` (byte-preserved from here on), binds it to a
//! capability (and optionally an input hash), pins the registry revision,
//! stamps an authoritative expiry (the x402 `maxTimeoutSeconds` is
//! advisory; this envelope's expiry governs), and signs.
//!
//! **Provider policy runs at quote issuance — never quote a caller you'd
//! deny.** Accepting a denied caller's payment creates refund obligations
//! the protocol doesn't want; authorize before accepting value. (The
//! post-verification provider check is a re-check.)
//!
//! One round: request → binding quote → accept or walk. No counter-offer
//! object exists, and that absence is the rule.

use net::adapter::net::identity::EntityId;
use serde::{Deserialize, Serialize};

use super::canonical::{EnvelopeError, ExtraFields, SignatureHex, SignedEnvelope};
use super::registry::RegistryRef;
use super::versioning::{ensure_tag, TAG_PAYMENT_QUOTE};
use crate::x402::requirements::PaymentRequirements;
use crate::x402::X402Carry;

/// The signed quote envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentQuote {
    /// Always [`TAG_PAYMENT_QUOTE`].
    pub object: String,
    /// Content-derived quote id (hex; see [`PaymentQuote::derive_id`]).
    pub quote_id: String,
    /// The provider identity issuing (and signing) this quote.
    pub provider: EntityId,
    /// The caller this quote was issued to. Quotes are per-caller —
    /// issuing one asserts the provider's policy admitted this caller.
    pub caller: EntityId,
    /// Capability id in display form (`provider/capability`).
    pub capability: String,
    /// Optional blake3 hex of the invocation input the price covers
    /// (RFQ/dynamic pricing binds this; P0 static pricing leaves it None —
    /// quote-small-invoke-big fails verification when present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<String>,
    /// The instantiated x402 requirements, byte-preserved. This — not the
    /// discovery template — is what settlement and billing bind to.
    pub requirements: X402Carry<PaymentRequirements>,
    /// The registry revision this quote was issued under. Verification
    /// uses this revision, never "latest".
    pub asset_registry: RegistryRef,
    /// Issuance time (signer clock, ns since epoch).
    pub issued_at_ns: u64,
    /// Authoritative expiry (signer clock, ns since epoch). Bounded-
    /// tolerance comparison is policy's job; there is no global clock.
    pub expires_at_ns: u64,
    /// Binding hash over `{version tag, capability, input, requirements
    /// bytes, registry ref}` — covers the version tag, so no cross-version
    /// replay.
    pub terms_hash: String,
    /// Provider identity signature over the canonical bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureHex>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

const TERMS_HASH_DOMAIN: &[u8] = b"net.payment.quote@1.terms_hash";
const QUOTE_ID_DOMAIN: &[u8] = b"net.payment.quote@1.quote_id";

fn transcript_hash(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize().as_bytes())
}

impl PaymentQuote {
    /// Build an unsigned quote; `sign_with` completes it.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: EntityId,
        caller: EntityId,
        capability: impl Into<String>,
        input_hash: Option<String>,
        requirements: X402Carry<PaymentRequirements>,
        asset_registry: RegistryRef,
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> Self {
        let capability = capability.into();
        let terms_hash = Self::derive_terms_hash(
            &capability,
            input_hash.as_deref(),
            requirements.bytes(),
            &asset_registry,
        );
        let quote_id = Self::derive_id(&provider, &caller, &terms_hash, issued_at_ns);
        Self {
            object: TAG_PAYMENT_QUOTE.to_string(),
            quote_id,
            provider,
            caller,
            capability,
            input_hash,
            requirements,
            asset_registry,
            issued_at_ns,
            expires_at_ns,
            terms_hash,
            signature: None,
            extra: ExtraFields::new(),
        }
    }

    /// The terms hash: what the quote *prices*. Covers the version tag
    /// (cross-version replay fails), the capability, the input hash when
    /// pricing bound one, the preserved requirements bytes, and the
    /// registry revision.
    pub fn derive_terms_hash(
        capability: &str,
        input_hash: Option<&str>,
        requirements_bytes: &[u8],
        registry: &RegistryRef,
    ) -> String {
        transcript_hash(
            TERMS_HASH_DOMAIN,
            &[
                TAG_PAYMENT_QUOTE.as_bytes(),
                capability.as_bytes(),
                input_hash.unwrap_or("").as_bytes(),
                requirements_bytes,
                registry.version.as_bytes(),
                registry.hash.as_bytes(),
            ],
        )
    }

    /// The quote id: content-derived (no rng in the money path), unique
    /// per `{provider, caller, terms, issuance instant}`.
    pub fn derive_id(
        provider: &EntityId,
        caller: &EntityId,
        terms_hash: &str,
        issued_at_ns: u64,
    ) -> String {
        transcript_hash(
            QUOTE_ID_DOMAIN,
            &[
                provider.as_bytes(),
                caller.as_bytes(),
                terms_hash.as_bytes(),
                &issued_at_ns.to_le_bytes(),
            ],
        )
    }

    /// Decode + integrity-check a received quote: tag, expiry ordering,
    /// terms-hash recomputation, quote-id recomputation, and signature.
    /// This is the caller-side gate before any payload is authored.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        let quote: Self =
            serde_json::from_slice(bytes).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
        ensure_tag(TAG_PAYMENT_QUOTE, &quote.object)?;
        quote.check_integrity()?;
        quote.verify_signature()?;
        Ok(quote)
    }

    /// Structural checks independent of the signature.
    pub fn check_integrity(&self) -> Result<(), EnvelopeError> {
        if self.expires_at_ns <= self.issued_at_ns {
            return Err(EnvelopeError::Field(
                "quote expires before (or at) issuance".into(),
            ));
        }
        let expected_terms = Self::derive_terms_hash(
            &self.capability,
            self.input_hash.as_deref(),
            self.requirements.bytes(),
            &self.asset_registry,
        );
        if expected_terms != self.terms_hash {
            return Err(EnvelopeError::Field(format!(
                "terms_hash mismatch: envelope says {}, content derives {}",
                self.terms_hash, expected_terms
            )));
        }
        let expected_id =
            Self::derive_id(&self.provider, &self.caller, &self.terms_hash, self.issued_at_ns);
        if expected_id != self.quote_id {
            return Err(EnvelopeError::Field(format!(
                "quote_id mismatch: envelope says {}, content derives {}",
                self.quote_id, expected_id
            )));
        }
        Ok(())
    }

    /// Whether the quote is expired at `now_ns` (signer-clock comparison;
    /// callers apply their policy tolerance to `now_ns` before asking).
    pub fn is_expired_at(&self, now_ns: u64) -> bool {
        now_ns >= self.expires_at_ns
    }
}

impl SignedEnvelope for PaymentQuote {
    const OBJECT_TAG: &'static str = TAG_PAYMENT_QUOTE;
    fn signer(&self) -> &EntityId {
        &self.provider
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

    fn requirements() -> X402Carry<PaymentRequirements> {
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

    fn signed_quote(provider: &EntityKeypair, caller: EntityId) -> PaymentQuote {
        let mut q = PaymentQuote::new(
            provider.entity_id().clone(),
            caller,
            "prov/fixture-tool",
            None,
            requirements(),
            RegistryRef { version: "net-default-0".into(), hash: "aa".into() },
            1_000,
            2_000,
        );
        q.sign_with(provider).unwrap();
        q
    }

    #[test]
    fn quote_round_trips_and_verifies() {
        let provider = EntityKeypair::generate();
        let caller = EntityKeypair::generate().entity_id().clone();
        let q = signed_quote(&provider, caller);
        let bytes = canonical_bytes(&q).unwrap();
        let back = PaymentQuote::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.quote_id, q.quote_id);
        assert_eq!(back.requirements.bytes(), q.requirements.bytes());
    }

    #[test]
    fn tampering_with_price_breaks_terms_hash_or_signature() {
        let provider = EntityKeypair::generate();
        let caller = EntityKeypair::generate().entity_id().clone();
        let q = signed_quote(&provider, caller);

        // Swap in cheaper requirements: terms_hash recomputation catches it.
        let mut tampered = q.clone();
        tampered.requirements = X402Carry::author(&PaymentRequirements {
            scheme: "mock".into(),
            network: "mock:net".into(),
            amount: "1".into(),
            asset: "musd".into(),
            pay_to: "attacker".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .unwrap();
        assert!(tampered.check_integrity().is_err());

        // Extend the expiry: id/terms still match, signature catches it.
        let mut extended = q.clone();
        extended.expires_at_ns = u64::MAX;
        assert!(extended.check_integrity().is_ok());
        assert_eq!(extended.verify_signature(), Err(EnvelopeError::BadSignature));
    }

    #[test]
    fn unsigned_quotes_are_rejected_fail_closed() {
        let provider = EntityKeypair::generate();
        let caller = EntityKeypair::generate().entity_id().clone();
        let mut q = signed_quote(&provider, caller);
        q.signature = None;
        let bytes = canonical_bytes(&q).unwrap();
        assert!(PaymentQuote::from_json_bytes(&bytes).is_err());
    }

    #[test]
    fn expiry_is_authoritative_and_ordered() {
        let provider = EntityKeypair::generate();
        let caller = EntityKeypair::generate().entity_id().clone();
        let q = signed_quote(&provider, caller.clone());
        assert!(!q.is_expired_at(1_999));
        assert!(q.is_expired_at(2_000));

        let mut inverted = PaymentQuote::new(
            provider.entity_id().clone(),
            caller,
            "prov/fixture-tool",
            None,
            requirements(),
            RegistryRef { version: "net-default-0".into(), hash: "aa".into() },
            2_000,
            1_000,
        );
        inverted.sign_with(&provider).unwrap();
        assert!(inverted.check_integrity().is_err());
    }

    #[test]
    fn unknown_fields_survive_and_stay_signed() {
        let provider = EntityKeypair::generate();
        let caller = EntityKeypair::generate().entity_id().clone();
        let mut q = signed_quote(&provider, caller);
        q.extra.insert("future_field".into(), serde_json::json!("v1.1 data"));
        q.sign_with(&provider).unwrap();

        let bytes = canonical_bytes(&q).unwrap();
        let back = PaymentQuote::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.extra.get("future_field"), Some(&serde_json::json!("v1.1 data")));

        // Stripping the unknown field breaks the signature — unknown
        // fields are covered, not decorative.
        let mut stripped = back.clone();
        stripped.extra.clear();
        assert_eq!(stripped.verify_signature(), Err(EnvelopeError::BadSignature));
    }
}
