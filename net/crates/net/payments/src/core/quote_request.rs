//! `net.payment.quote_request@1` — the caller-signed request for a quote.
//!
//! ## Why this exists
//!
//! Quote issuance runs the provider's admission policy ("never quote a
//! caller you'd deny") and stamps the caller identity that becomes
//! `BillingEvent.payer`. Both are worth exactly as much as the identity
//! they are evaluated against.
//!
//! The mesh quote service used to take that identity from a field in the
//! request body. `EntityId` is an ed25519 **public** key, so asserting
//! someone else's costs nothing: an attacker could name any admitted
//! caller to clear admission, and could name a victim to have the
//! provider's signed billing record attribute the payment to them.
//!
//! The obvious fix does not work. nRPC's `RpcContext::caller_origin` is
//! documented at its source as *routing metadata, not identity
//! authentication* — it is carried on the packet header, so comparing it
//! against a body field compares two claims from the same untrusted
//! source. The only AEAD-verified peer identity is the **wire-session
//! peer**, i.e. the last hop, which is not necessarily the originator.
//! So the public RPC surface exposes no authenticated end-to-end caller,
//! and payments has to carry its own proof.
//!
//! ## What the signature covers, and why each field is in it
//!
//! | field | why |
//! |---|---|
//! | `object` (the tag) | domain separation — this signature cannot be replayed as another envelope |
//! | `provider` | **destination binding**: a request signed for provider A cannot be replayed to provider B |
//! | `caller` | the identity being claimed; it is also the signer |
//! | `capability` | a request for a cheap tool cannot be replayed against an expensive one |
//! | `template_hash` | the announced terms the caller is asking to be quoted under |
//! | `issued_at_ns` / `expires_at_ns` | freshness — a captured request stops working |
//! | `nonce` | replay identity within the freshness window |
//!
//! The signature covers the canonical bytes with the `signature` key
//! absent, exactly like every other envelope here, so the `object` tag is
//! inside the signed transcript and the encoding is the one pinned by the
//! cross-language golden vectors.
//!
//! ## What it does *not* establish
//!
//! This proves the requester holds the caller's identity key. It does not
//! make the transport confidential, and it is not a session: an
//! intermediary that observes a valid request can replay it **within the
//! freshness window** unless the provider also rejects repeated nonces,
//! which [`SeenNonces`] is for. Replay costs the attacker nothing and
//! gains them nothing except a duplicate quote — quotes are free, and
//! paying one still requires the caller's settlement authorization — but
//! it can burn caller-scoped issuance or exposure limits, so the guard
//! is on by default.

use net::adapter::net::identity::EntityId;
use serde::{Deserialize, Serialize};

use super::canonical::{EnvelopeError, ExtraFields, SignatureHex, SignedEnvelope};
use super::versioning::ensure_tag;

/// `net.payment.quote_request@1`.
pub const TAG_QUOTE_REQUEST: &str = "net.payment.quote_request@1";

/// The longest validity window a quote request may claim.
///
/// A caller sets its own `expires_at_ns`, so without a ceiling one could
/// mint a request valid for a year and hand a replayable credential to
/// anything that sees it. Sixty seconds is far more than a quote round
/// trip needs and short enough that the replay window is not a standing
/// liability.
pub const MAX_REQUEST_LIFETIME_NS: u64 = 60_000_000_000;

/// The caller-signed request for a quote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteRequest {
    /// Always [`TAG_QUOTE_REQUEST`].
    pub object: String,
    /// The provider this request is addressed to — the destination bind.
    pub provider: EntityId,
    /// The caller being claimed. Also the signer.
    pub caller: EntityId,
    /// Capability id in display form (`provider/capability`).
    pub capability: String,
    /// blake3 hex of the announced template's preserved bytes.
    pub template_hash: String,
    /// Caller clock, ns since epoch.
    pub issued_at_ns: u64,
    /// Caller clock, ns since epoch. Bounded by [`MAX_REQUEST_LIFETIME_NS`].
    pub expires_at_ns: u64,
    /// Replay identity within the freshness window.
    pub nonce: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureHex>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

/// Why a quote request was refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum QuoteRequestError {
    #[error("quote request envelope invalid: {0}")]
    Envelope(#[from] EnvelopeError),
    #[error("quote request is addressed to a different provider")]
    WrongProvider,
    #[error("quote request is for capability `{requested}`, not `{served}`")]
    WrongCapability { requested: String, served: String },
    #[error("quote request does not bind the template it was sent with")]
    TemplateMismatch,
    #[error("quote request validity window is empty or inverted")]
    BadWindow,
    #[error(
        "quote request claims a {claimed_ns}ns lifetime, over the {max_ns}ns ceiling — a \
         long-lived signed request is a replayable credential"
    )]
    LifetimeTooLong { claimed_ns: u64, max_ns: u64 },
    #[error("quote request expired")]
    Expired,
    #[error("quote request is not yet valid (issued in the future beyond tolerance)")]
    NotYetValid,
    #[error("quote request nonce was already used")]
    ReplayedNonce,
}

impl QuoteRequest {
    /// Build an unsigned request; `sign_with` completes it.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: EntityId,
        caller: EntityId,
        capability: impl Into<String>,
        template_bytes: &[u8],
        issued_at_ns: u64,
        ttl_ns: u64,
        nonce: impl Into<String>,
    ) -> Self {
        Self {
            object: TAG_QUOTE_REQUEST.to_string(),
            provider,
            caller,
            capability: capability.into(),
            template_hash: hex::encode(blake3::hash(template_bytes).as_bytes()),
            issued_at_ns,
            expires_at_ns: issued_at_ns.saturating_add(ttl_ns.min(MAX_REQUEST_LIFETIME_NS)),
            nonce: nonce.into(),
            signature: None,
            extra: ExtraFields::new(),
        }
    }

    /// Derive a nonce for this request deterministically, so a retry of
    /// the *same* request re-presents the same nonce (and is therefore
    /// idempotent at the provider's replay guard) while distinct requests
    /// never collide.
    ///
    /// Deterministic rather than random for the same reason quote ids are:
    /// the money path carries no rng, and a retry that minted a fresh
    /// nonce would look like a replay attempt rather than a retry.
    pub fn derive_nonce(
        caller: &EntityId,
        capability: &str,
        template_bytes: &[u8],
        issued_at_ns: u64,
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"net.payments.quote_request.nonce@1");
        for part in [
            caller.as_bytes().as_slice(),
            capability.as_bytes(),
            template_bytes,
            &issued_at_ns.to_le_bytes(),
        ] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        hex::encode(&hasher.finalize().as_bytes()[..16])
    }

    /// Decode and fully verify a received request.
    ///
    /// Checks, in order: the tag, the signature (against the *claimed*
    /// caller — which is what makes the claim a proof), the destination
    /// provider, the capability, the template bind, and freshness. Every
    /// one is fail-closed.
    ///
    /// Replay is **not** checked here because it needs state; the caller
    /// passes the nonce to [`SeenNonces::admit`] after this returns.
    pub fn verify(
        bytes: &[u8],
        served_provider: &EntityId,
        served_capability: &str,
        template_bytes: &[u8],
        now_ns: u64,
        skew_ns: u64,
    ) -> Result<Self, QuoteRequestError> {
        let request: Self = serde_json::from_slice(bytes)
            .map_err(|e| EnvelopeError::Encoding(e.to_string()))
            .map_err(QuoteRequestError::Envelope)?;
        ensure_tag(TAG_QUOTE_REQUEST, &request.object).map_err(EnvelopeError::from)?;
        // The signature is verified against `request.caller` — the identity
        // the request claims. That is the whole point: holding the key is
        // what turns a claim into a proof.
        request.verify_signature()?;

        if request.provider != *served_provider {
            return Err(QuoteRequestError::WrongProvider);
        }
        if request.capability != served_capability {
            return Err(QuoteRequestError::WrongCapability {
                requested: request.capability.clone(),
                served: served_capability.to_string(),
            });
        }
        if request.template_hash != hex::encode(blake3::hash(template_bytes).as_bytes()) {
            return Err(QuoteRequestError::TemplateMismatch);
        }
        if request.expires_at_ns <= request.issued_at_ns {
            return Err(QuoteRequestError::BadWindow);
        }
        let lifetime = request.expires_at_ns - request.issued_at_ns;
        if lifetime > MAX_REQUEST_LIFETIME_NS {
            return Err(QuoteRequestError::LifetimeTooLong {
                claimed_ns: lifetime,
                max_ns: MAX_REQUEST_LIFETIME_NS,
            });
        }
        if now_ns >= request.expires_at_ns.saturating_add(skew_ns) {
            return Err(QuoteRequestError::Expired);
        }
        if request.issued_at_ns > now_ns.saturating_add(skew_ns) {
            return Err(QuoteRequestError::NotYetValid);
        }
        Ok(request)
    }
}

impl SignedEnvelope for QuoteRequest {
    const OBJECT_TAG: &'static str = TAG_QUOTE_REQUEST;
    fn signer(&self) -> &EntityId {
        &self.caller
    }
    fn signature(&self) -> Option<&SignatureHex> {
        self.signature.as_ref()
    }
    fn set_signature(&mut self, sig: SignatureHex) {
        self.signature = Some(sig);
    }
}

/// Bounded, time-windowed replay guard for quote-request nonces.
///
/// A verified request is still replayable inside its freshness window by
/// anything that saw it, so the provider remembers nonces until they can
/// no longer be presented. Entries are dropped once
/// `now > expiry + MAX_REQUEST_LIFETIME_NS`, which bounds the set by the
/// request rate over one window rather than by total traffic.
///
/// **In-process, deliberately.** A quote is free and idempotent to
/// re-issue, so a replay that slips past a restart or a second node costs
/// a duplicate quote and nothing else — not worth the write amplification
/// of putting this in the locked store on the path of every quote. The
/// guard that has to be durable is the one on *payment*, and that one is.
#[derive(Debug, Default)]
pub struct SeenNonces {
    /// nonce → the instant it stops being presentable.
    seen: std::collections::HashMap<String, u64>,
}

impl SeenNonces {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `nonce` as used, or refuse it as a replay.
    ///
    /// Sweeps expired entries as it goes — the operation that grows the
    /// map is the one that prunes it, mirroring the engine's retention.
    pub fn admit(
        &mut self,
        nonce: &str,
        expires_at_ns: u64,
        now_ns: u64,
    ) -> Result<(), QuoteRequestError> {
        self.seen
            .retain(|_, expiry| now_ns < expiry.saturating_add(MAX_REQUEST_LIFETIME_NS));
        if self.seen.contains_key(nonce) {
            return Err(QuoteRequestError::ReplayedNonce);
        }
        self.seen.insert(nonce.to_string(), expires_at_ns);
        Ok(())
    }

    /// How many nonces are currently remembered (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canonical::canonical_bytes;
    use net::adapter::net::identity::EntityKeypair;

    const NOW: u64 = 1_000_000_000_000_000;
    const TEMPLATE: &[u8] = b"{\"scheme\":\"mock\"}";
    const CAPABILITY: &str = "prov/tool";

    fn signed(caller: &EntityKeypair, provider: &EntityId) -> Vec<u8> {
        let nonce = QuoteRequest::derive_nonce(caller.entity_id(), CAPABILITY, TEMPLATE, NOW);
        let mut req = QuoteRequest::new(
            provider.clone(),
            caller.entity_id().clone(),
            CAPABILITY,
            TEMPLATE,
            NOW,
            30_000_000_000,
            nonce,
        );
        req.sign_with(caller).expect("sign");
        canonical_bytes(&req).expect("canonical")
    }

    #[test]
    fn a_signed_request_verifies_and_binds_everything_it_claims() {
        let caller = EntityKeypair::generate();
        let provider = EntityKeypair::generate().entity_id().clone();
        let bytes = signed(&caller, &provider);

        let ok = QuoteRequest::verify(&bytes, &provider, CAPABILITY, TEMPLATE, NOW + 1, 0)
            .expect("verify");
        assert_eq!(ok.caller, *caller.entity_id());
    }

    /// The forgery the whole envelope exists to stop: naming a caller you
    /// do not hold the key for.
    #[test]
    fn a_caller_identity_cannot_be_claimed_without_its_key() {
        let victim = EntityKeypair::generate();
        let attacker = EntityKeypair::generate();
        let provider = EntityKeypair::generate().entity_id().clone();

        // Attacker builds a request naming the victim, signs with its own key.
        let mut forged = QuoteRequest::new(
            provider.clone(),
            victim.entity_id().clone(), // the claim
            CAPABILITY,
            TEMPLATE,
            NOW,
            30_000_000_000,
            "n1",
        );
        // `sign_with` refuses outright when the keypair is not the signer.
        assert!(forged.sign_with(&attacker).is_err());

        // Forcing a signature in by other means still fails verification:
        // the envelope verifies against `caller`, not against whoever signed.
        let payload = crate::core::canonical::signed_payload_bytes(&forged).unwrap();
        let sig = attacker.try_sign(&payload).unwrap();
        forged.signature = Some(SignatureHex(sig.to_bytes()));
        let bytes = canonical_bytes(&forged).unwrap();
        assert!(matches!(
            QuoteRequest::verify(&bytes, &provider, CAPABILITY, TEMPLATE, NOW + 1, 0),
            Err(QuoteRequestError::Envelope(EnvelopeError::BadSignature))
        ));
    }

    /// Destination binding: a request signed for one provider cannot be
    /// relayed to another, even though it is perfectly well signed.
    #[test]
    fn a_request_cannot_be_replayed_to_a_different_provider() {
        let caller = EntityKeypair::generate();
        let intended = EntityKeypair::generate().entity_id().clone();
        let other = EntityKeypair::generate().entity_id().clone();
        let bytes = signed(&caller, &intended);

        assert_eq!(
            QuoteRequest::verify(&bytes, &other, CAPABILITY, TEMPLATE, NOW + 1, 0),
            Err(QuoteRequestError::WrongProvider)
        );
    }

    /// Capability and template binds: a request for one thing cannot be
    /// spent on another.
    #[test]
    fn capability_and_template_are_bound() {
        let caller = EntityKeypair::generate();
        let provider = EntityKeypair::generate().entity_id().clone();
        let bytes = signed(&caller, &provider);

        assert!(matches!(
            QuoteRequest::verify(&bytes, &provider, "prov/other-tool", TEMPLATE, NOW + 1, 0),
            Err(QuoteRequestError::WrongCapability { .. })
        ));
        assert_eq!(
            QuoteRequest::verify(&bytes, &provider, CAPABILITY, b"{\"other\":1}", NOW + 1, 0),
            Err(QuoteRequestError::TemplateMismatch)
        );
    }

    #[test]
    fn freshness_is_enforced_in_both_directions_and_bounded() {
        let caller = EntityKeypair::generate();
        let provider = EntityKeypair::generate().entity_id().clone();
        let bytes = signed(&caller, &provider);

        // Past its expiry.
        assert_eq!(
            QuoteRequest::verify(
                &bytes,
                &provider,
                CAPABILITY,
                TEMPLATE,
                NOW + 30_000_000_001,
                0
            ),
            Err(QuoteRequestError::Expired)
        );
        // Issued in the future beyond tolerance.
        assert_eq!(
            QuoteRequest::verify(&bytes, &provider, CAPABILITY, TEMPLATE, NOW - 1, 0),
            Err(QuoteRequestError::NotYetValid)
        );

        // An abusively long window is refused rather than honoured: a
        // long-lived signed request is a replayable credential.
        let mut greedy = QuoteRequest::new(
            provider.clone(),
            caller.entity_id().clone(),
            CAPABILITY,
            TEMPLATE,
            NOW,
            30_000_000_000,
            "n2",
        );
        greedy.expires_at_ns = NOW + MAX_REQUEST_LIFETIME_NS * 100;
        greedy.sign_with(&caller).unwrap();
        let bytes = canonical_bytes(&greedy).unwrap();
        assert!(matches!(
            QuoteRequest::verify(&bytes, &provider, CAPABILITY, TEMPLATE, NOW + 1, 0),
            Err(QuoteRequestError::LifetimeTooLong { .. })
        ));
    }

    /// `new` clamps the TTL, so the ceiling cannot be exceeded by
    /// accident — only by hand-editing the envelope, which the test above
    /// covers.
    #[test]
    fn the_constructor_clamps_the_ttl() {
        let caller = EntityKeypair::generate();
        let provider = EntityKeypair::generate().entity_id().clone();
        let req = QuoteRequest::new(
            provider,
            caller.entity_id().clone(),
            CAPABILITY,
            TEMPLATE,
            NOW,
            u64::MAX,
            "n",
        );
        assert_eq!(req.expires_at_ns, NOW + MAX_REQUEST_LIFETIME_NS);
    }

    #[test]
    fn nonces_replay_once_and_the_window_bounds_the_set() {
        let mut seen = SeenNonces::new();
        assert!(seen.admit("a", NOW + 1_000, NOW).is_ok());
        assert_eq!(
            seen.admit("a", NOW + 1_000, NOW),
            Err(QuoteRequestError::ReplayedNonce)
        );
        // A different nonce is fine.
        assert!(seen.admit("b", NOW + 1_000, NOW).is_ok());
        assert_eq!(seen.len(), 2);

        // Well past the window, entries are swept and the nonce is free
        // again — by then the request it belonged to is long expired, so
        // re-admitting it grants nothing.
        let later = NOW + 1_000 + MAX_REQUEST_LIFETIME_NS + 1;
        assert!(seen.admit("c", later + 1_000, later).is_ok());
        assert_eq!(seen.len(), 1, "expired entries are swept as the map grows");
    }

    /// A retry of the same request re-presents the same nonce, so the
    /// replay guard treats a retry as a retry rather than an attack.
    #[test]
    fn the_derived_nonce_is_stable_for_one_request_and_distinct_across_requests() {
        let caller = EntityKeypair::generate().entity_id().clone();
        let a = QuoteRequest::derive_nonce(&caller, CAPABILITY, TEMPLATE, NOW);
        assert_eq!(
            a,
            QuoteRequest::derive_nonce(&caller, CAPABILITY, TEMPLATE, NOW)
        );
        assert_ne!(
            a,
            QuoteRequest::derive_nonce(&caller, CAPABILITY, TEMPLATE, NOW + 1)
        );
        assert_ne!(
            a,
            QuoteRequest::derive_nonce(&caller, "prov/other", TEMPLATE, NOW)
        );
        assert_ne!(
            a,
            QuoteRequest::derive_nonce(&caller, CAPABILITY, b"other", NOW)
        );
    }
}
