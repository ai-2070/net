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
//! which [`SeenNonces`] is for (keyed per caller, and capped). Replay costs the attacker nothing and
//! gains them nothing except a duplicate quote — quotes are free, and
//! paying one still requires the caller's settlement authorization — but
//! it can burn caller-scoped issuance or exposure limits, so the guard
//! is on by default.

use net::adapter::net::identity::EntityId;
use serde::{Deserialize, Serialize};

use super::canonical::{EnvelopeError, ExtraFields, SignatureHex, SignedEnvelope};
use super::versioning::ensure_tag;
// The tag lives in the versioning registry with every other envelope's, so
// the wire string has exactly one definition. Re-exported here because
// this is where readers of the envelope look for it.
pub use super::versioning::TAG_QUOTE_REQUEST;

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
    #[error("quote request nonce is longer than the {max}-byte limit")]
    NonceTooLong { max: usize },
    #[error(
        "the provider's quote-request replay guard is at capacity ({capacity}) — refusing rather \
         than forgetting a nonce that is still presentable"
    )]
    ReplayGuardSaturated { capacity: usize },
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

    /// Derive a nonce for this request.
    ///
    /// Deterministic rather than random, for the same reason quote ids
    /// are: the money path carries no rng.
    ///
    /// `sequence` is a per-channel counter and is what makes two requests
    /// distinct. Without it the inputs are caller + capability + template
    /// + timestamp, and [`crate::flow::Clock`] is a public seam with no
    /// monotonicity requirement — a fixed or coarse clock makes two
    /// legitimate back-to-back requests derive the same nonce, and the
    /// second is then refused as a replay. A counter cannot repeat within
    /// a process.
    ///
    /// This does not weaken retry idempotency, because a retry does not
    /// re-derive: the caller re-sends the *serialized request* it already
    /// built, nonce included. Deriving again is a new request, and a new
    /// request should have a new nonce.
    pub fn derive_nonce(
        caller: &EntityId,
        capability: &str,
        template_bytes: &[u8],
        issued_at_ns: u64,
        sequence: u64,
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"net.payments.quote_request.nonce@1");
        for part in [
            caller.as_bytes().as_slice(),
            capability.as_bytes(),
            template_bytes,
            &issued_at_ns.to_le_bytes(),
            &sequence.to_le_bytes(),
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
        // Cheap structural bound BEFORE the signature check. Verification
        // is the expensive step, so a caller who can make us do it on a
        // multi-megabyte nonce gets asymmetric work out of one request —
        // and the nonce cap exists precisely because a nonce is an
        // identifier, not a payload. Reject on shape first.
        if request.nonce.len() > MAX_NONCE_BYTES {
            return Err(QuoteRequestError::NonceTooLong {
                max: MAX_NONCE_BYTES,
            });
        }
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
/// `now > expiry + MAX_REQUEST_LIFETIME_NS`.
///
/// ## Keyed per caller, and capped
///
/// The key is `(caller, nonce)`, not the nonce alone. A global nonce
/// space would let one caller suppress another's quote by picking a
/// colliding nonce — a cross-identity denial of service costing the
/// attacker one signed request. Scoping to the caller means a collision
/// can only affect the identity that produced it.
///
/// The map is also hard-capped. Time-based expiry alone bounds nothing
/// useful against an attacker who can mint unique signed requests as
/// fast as it can send them: every one is a distinct nonce, and they all
/// live for a full window. At capacity the guard **refuses** rather than
/// evicting — forgetting a nonce that is still presentable would turn a
/// memory bound into a replay window, which is the thing it exists to
/// prevent. A saturated guard is a loud, retryable refusal.
///
/// **In-process, deliberately.** A quote is free and idempotent to
/// re-issue, so a replay that slips past a restart or a second node costs
/// a duplicate quote and nothing else — not worth the write amplification
/// of putting this in the locked store on the path of every quote. The
/// guard that has to be durable is the one on *payment*, and that one is.
#[derive(Debug)]
pub struct SeenNonces {
    /// `(caller, nonce)` → the last instant the request could still be
    /// accepted (its expiry plus the verifier's skew tolerance). Past
    /// that the nonce is unreplayable and the entry is dead weight.
    seen: std::collections::HashMap<(EntityId, String), u64>,
    capacity: usize,
    /// Map size at which the next expiry sweep runs. See [`Self::admit`].
    sweep_at: usize,
}

/// Nonces are identifiers, not payloads. Anything longer is a caller
/// trying to spend the provider's memory rather than identify a request.
pub const MAX_NONCE_BYTES: usize = 128;

/// How many in-window nonces one provider remembers before refusing.
///
/// Sized so the guard costs a few MiB at worst while comfortably
/// exceeding any honest request rate over a 60s window: 100k requests a
/// minute from legitimate callers is far past where other limits bite.
pub const DEFAULT_REPLAY_GUARD_CAPACITY: usize = 100_000;

impl Default for SeenNonces {
    fn default() -> Self {
        Self::new()
    }
}

impl SeenNonces {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_REPLAY_GUARD_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            seen: std::collections::HashMap::new(),
            capacity,
            sweep_at: (capacity / 8).max(1),
        }
    }

    /// Record this caller's `nonce` as used, or refuse it.
    ///
    /// The expired-entry sweep is **amortized**, not per-call. A `retain`
    /// on every admission is O(n) under the shared mutex, so once the
    /// window is full every subsequent quote pays a full-map scan — a
    /// caller could fill the guard and then make issuance quadratic for
    /// everyone. Sweeping only when the map has grown by a fraction of
    /// its capacity (or when it is actually full) keeps the amortized
    /// cost constant while bounding how much dead weight can accumulate.
    /// `accept_until_ns` is the last instant the request could still be
    /// accepted — its expiry plus whatever skew the verifier allows. The
    /// caller passes it rather than the raw expiry so the guard sweeps
    /// against the same boundary `verify` enforces.
    pub fn admit(
        &mut self,
        caller: &EntityId,
        nonce: &str,
        accept_until_ns: u64,
        now_ns: u64,
    ) -> Result<(), QuoteRequestError> {
        if nonce.len() > MAX_NONCE_BYTES {
            return Err(QuoteRequestError::NonceTooLong {
                max: MAX_NONCE_BYTES,
            });
        }
        if self.seen.len() >= self.sweep_at {
            self.sweep(now_ns);
        }
        let key = (caller.clone(), nonce.to_string());
        if self.seen.contains_key(&key) {
            return Err(QuoteRequestError::ReplayedNonce);
        }
        // Measured after any sweep, so capacity reflects what is actually
        // still presentable rather than historical volume.
        if self.seen.len() >= self.capacity {
            // One more sweep before refusing: better to pay the scan than
            // to reject a legitimate caller over entries that have all
            // expired since the last one.
            self.sweep(now_ns);
            if self.seen.len() >= self.capacity {
                return Err(QuoteRequestError::ReplayGuardSaturated {
                    capacity: self.capacity,
                });
            }
        }
        self.seen.insert(key, accept_until_ns);
        Ok(())
    }

    /// Drop entries that can no longer be presented, and schedule the
    /// next sweep a growth step away.
    fn sweep(&mut self, now_ns: u64) {
        // Against the stored deadline, which is the request's own expiry
        // plus the provider's skew tolerance — i.e. the last instant
        // `verify` would still accept it.
        //
        // This used to add `MAX_REQUEST_LIFETIME_NS` on top, holding every
        // nonce for a further full minute after it stopped being
        // presentable. That is dead weight counted against the capacity,
        // so under sustained traffic the guard could saturate and start
        // refusing new callers over entries that could not be replayed
        // anyway.
        self.seen.retain(|_, deadline| now_ns < *deadline);
        // Next sweep once the map has grown by ~1/8 of capacity, floored
        // so a tiny capacity still sweeps sometimes.
        let step = (self.capacity / 8).max(1);
        self.sweep_at = self.seen.len().saturating_add(step).min(self.capacity);
    }

    /// Give a nonce back, because the request it belonged to was refused
    /// before it produced anything.
    ///
    /// [`Self::admit`] is a check-and-set so concurrent duplicates cannot
    /// both pass — but a request that is then denied (provider admission,
    /// a policy refusal) produced no quote, so holding its nonce would
    /// both retain state for work never done and block the caller's
    /// legitimate retry once the denial is fixed. Only the caller that
    /// took the nonce can give it back.
    pub fn release(&mut self, caller: &EntityId, nonce: &str) {
        self.seen.remove(&(caller.clone(), nonce.to_string()));
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
        let nonce = QuoteRequest::derive_nonce(caller.entity_id(), CAPABILITY, TEMPLATE, NOW, 0);
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

    /// The guard is keyed per caller, so one caller cannot suppress
    /// another's quote by colliding a nonce.
    ///
    /// A global nonce space would make that a cross-identity denial of
    /// service costing the attacker one signed request.
    #[test]
    fn one_callers_nonce_does_not_block_another_callers() {
        let a = EntityKeypair::generate().entity_id().clone();
        let b = EntityKeypair::generate().entity_id().clone();
        let mut seen = SeenNonces::new();

        assert!(seen.admit(&a, "shared", NOW + 1_000, NOW).is_ok());
        // Same nonce string, different caller: unaffected.
        assert!(
            seen.admit(&b, "shared", NOW + 1_000, NOW).is_ok(),
            "a nonce collision must not cross identities"
        );
        // And each is still replay-protected within its own scope.
        assert_eq!(
            seen.admit(&a, "shared", NOW + 1_000, NOW),
            Err(QuoteRequestError::ReplayedNonce)
        );
        assert_eq!(
            seen.admit(&b, "shared", NOW + 1_000, NOW),
            Err(QuoteRequestError::ReplayedNonce)
        );
    }

    /// At capacity the guard refuses rather than evicting.
    ///
    /// Time-based expiry alone bounds nothing against an attacker minting
    /// unique signed requests as fast as it can send them — every one is
    /// a distinct nonce living a full window. Evicting to make room would
    /// turn the memory bound into a replay window, which is the thing the
    /// guard exists to prevent, so saturation is a loud refusal.
    #[test]
    fn a_saturated_guard_refuses_rather_than_forgetting() {
        let caller = EntityKeypair::generate().entity_id().clone();
        let mut seen = SeenNonces::with_capacity(2);

        assert!(seen.admit(&caller, "n1", NOW + 1_000, NOW).is_ok());
        assert!(seen.admit(&caller, "n2", NOW + 1_000, NOW).is_ok());
        assert_eq!(
            seen.admit(&caller, "n3", NOW + 1_000, NOW),
            Err(QuoteRequestError::ReplayGuardSaturated { capacity: 2 })
        );
        // The nonces already held are still held — nothing was evicted to
        // make room, so nothing became replayable.
        assert_eq!(
            seen.admit(&caller, "n1", NOW + 1_000, NOW),
            Err(QuoteRequestError::ReplayedNonce)
        );

        // Once the window passes, the sweep frees the space again.
        let later = NOW + 1_000 + MAX_REQUEST_LIFETIME_NS + 1;
        assert!(seen.admit(&caller, "n3", later + 1_000, later).is_ok());
    }

    /// A nonce is an identifier, not a payload: an over-long one is a
    /// caller spending the provider's memory rather than identifying a
    /// request.
    #[test]
    fn an_over_long_nonce_is_refused() {
        let caller = EntityKeypair::generate().entity_id().clone();
        let mut seen = SeenNonces::new();
        let huge = "n".repeat(MAX_NONCE_BYTES + 1);
        assert_eq!(
            seen.admit(&caller, &huge, NOW + 1_000, NOW),
            Err(QuoteRequestError::NonceTooLong {
                max: MAX_NONCE_BYTES
            })
        );
        assert!(seen.is_empty(), "a refused nonce must not be recorded");
        // Exactly at the limit is fine.
        let ok = "n".repeat(MAX_NONCE_BYTES);
        assert!(seen.admit(&caller, &ok, NOW + 1_000, NOW).is_ok());
    }

    /// A released nonce is free again — a request that produced nothing
    /// must not retain state or block the caller's retry.
    #[test]
    fn a_released_nonce_can_be_used_again() {
        let caller = EntityKeypair::generate().entity_id().clone();
        let mut seen = SeenNonces::new();
        assert!(seen.admit(&caller, "n", NOW + 1_000, NOW).is_ok());
        assert_eq!(seen.len(), 1);

        seen.release(&caller, "n");
        assert!(seen.is_empty());
        assert!(
            seen.admit(&caller, "n", NOW + 1_000, NOW).is_ok(),
            "a denied request's nonce must be reusable once the denial is fixed"
        );

        // Release is scoped to the caller that took it.
        let other = EntityKeypair::generate().entity_id().clone();
        seen.release(&other, "n");
        assert_eq!(
            seen.admit(&caller, "n", NOW + 1_000, NOW),
            Err(QuoteRequestError::ReplayedNonce),
            "another identity's release must not free this nonce"
        );
    }

    /// Replay protection holds, and expired entries do not accumulate.
    ///
    /// The sweep is amortized rather than per-admission: a `retain` on
    /// every call is O(n) under the shared mutex, so a full window would
    /// make every subsequent quote pay a whole-map scan. A small capacity
    /// here puts the sweep threshold within reach of a short test.
    #[test]
    fn nonces_replay_once_and_expired_entries_do_not_accumulate() {
        let caller = EntityKeypair::generate().entity_id().clone();
        let mut seen = SeenNonces::with_capacity(8);

        assert!(seen.admit(&caller, "a", NOW + 1_000, NOW).is_ok());
        assert_eq!(
            seen.admit(&caller, "a", NOW + 1_000, NOW),
            Err(QuoteRequestError::ReplayedNonce)
        );
        // A different nonce is fine.
        assert!(seen.admit(&caller, "b", NOW + 1_000, NOW).is_ok());
        assert_eq!(seen.len(), 2);

        // Well past the window, admissions sweep what can no longer be
        // presented rather than growing without bound.
        let later = NOW + 1_000 + MAX_REQUEST_LIFETIME_NS + 1;
        for i in 0..8 {
            let _ = seen.admit(&caller, &format!("late-{i}"), later + 1_000, later);
        }
        assert!(
            seen.len() <= 8,
            "the guard must not exceed its capacity, got {}",
            seen.len()
        );
        assert!(
            !seen.seen.contains_key(&(caller.clone(), "a".to_string())),
            "an entry past its window must be swept"
        );
    }
}
