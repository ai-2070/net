//! OA-2 §2.1–§2.2 of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md` —
//! the organization grant family:
//!
//! - [`CapabilityAuthorityId`] — the deterministic 32-byte
//!   authorization-scope name of a capability
//!   (`blake3::derive_key("net-org-capability-v1", tag)`).
//!   Documented ENUMERABLE; never a locator, never a secrecy
//!   mechanism.
//! - [`OrgDispatcherGrant`] — A → S: "entity `dispatcher` may act
//!   FOR org A" over an exact capability or any. Fixed one-hop,
//!   org-root-signed, days–weeks TTL (Locked #4 — there are no
//!   delegation chains in v1).
//! - [`OrgCapabilityGrant`] — B → A: "org A may DISCOVER and/or
//!   INVOKE capability C on B's target scope". The SIGNED grant
//!   carries only a [`GrantedDiscoveryBinding`] — an audience
//!   handle plus a key COMMITMENT
//!   (`blake3::derive_key("net-org-audience-commit-v1", key)`).
//!   The raw discovery key lives only in the local
//!   [`OrgAudienceSecret`] file, delivered out of band; it never
//!   transits RPC headers, tracing/debug paths, denial logs, or
//!   provider surfaces.
//!
//! # Structural rule (v1, enforced at issue AND decode)
//!
//! ```text
//! rights ⊇ DISCOVER  ⇔  discovery binding present
//! one DISCOVER grant ⇔ one unique handle ⇔ one unique key
//! ```
//!
//! Issuance ALWAYS mints fresh audience material for a DISCOVER
//! grant — there is deliberately no key-reuse surface (a shared key
//! would let an INVOKE-only grantee decrypt, and an expired grant's
//! holder would retain a still-live key). Shared "disclosure
//! groups" are a future explicit feature, not an accident of API
//! shape.
//!
//! Holding any grant is never invocation authority by itself:
//! admission (§2.4) verifies the full proof chain per call, and
//! `may_execute` never sees any of these types.

use ed25519_dalek::Signature;

use super::org::{current_timestamp, OrgError, OrgId, OrgKeypair};
use crate::adapter::net::identity::{EntityId, MAX_TOKEN_CLOCK_SKEW_SECS};

/// blake3 `derive_key` context for [`CapabilityAuthorityId`]
/// (plan §2.1). A context string, not a signing domain: the id is
/// a deterministic public name, enumerable by anyone who knows the
/// tag.
pub const CAPABILITY_AUTHORITY_CONTEXT: &str = "net-org-capability-v1";

/// blake3 `derive_key` context binding a discovery key to its
/// in-grant commitment (plan §2.2).
pub const AUDIENCE_COMMIT_CONTEXT: &str = "net-org-audience-commit-v1";

/// Signature domain for [`OrgDispatcherGrant`] — prefixed to the
/// signed payload so grant bytes can never be confused with a
/// membership cert or floor bundle signed by the same org root.
pub const ORG_DISPATCHER_GRANT_SIG_DOMAIN: &[u8] = b"net-org-dispatcher-grant-v1";

/// Signature domain for [`OrgCapabilityGrant`].
pub const ORG_CAPABILITY_GRANT_SIG_DOMAIN: &[u8] = b"net-org-capability-grant-v1";

/// Maximum grant validity window (issue AND verify — same
/// dual-enforcement discipline as `MAX_ORG_CERT_TTL_SECS`). The
/// plan pins grant lifetimes at "days–weeks" with renewal =
/// revocation in v1; 30 days is the ceiling that keeps "weeks"
/// honest while leaving room for operational slack. Flagged for
/// OA-2 review.
pub const MAX_ORG_GRANT_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// The deterministic authorization-scope name of a capability:
/// `blake3::derive_key("net-org-capability-v1", canonical tag
/// bytes)` (plan §2.1).
///
/// Authorization scope ONLY — never a locator and never a secret:
/// anyone who knows a capability tag can compute its id, and the
/// id appears in grants precisely so authority can name the
/// capability without carrying the (possibly private) descriptor.
/// Derived (non-constant-time) `PartialEq` is deliberate for the
/// same reason as `OrgId`'s.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CapabilityAuthorityId(pub [u8; 32]);

impl CapabilityAuthorityId {
    /// Derive the id for a canonical capability tag (the exact
    /// wire form, e.g. `nrpc:billing-reconcile`).
    pub fn for_tag(tag: &str) -> Self {
        Self(blake3::derive_key(
            CAPABILITY_AUTHORITY_CONTEXT,
            tag.as_bytes(),
        ))
    }

    /// Construct from raw bytes (wire decode).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for CapabilityAuthorityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CapabilityAuthorityId({})", hex_short(&self.0))
    }
}

impl std::fmt::Display for CapabilityAuthorityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex_short(&self.0))
    }
}

/// First 8 bytes as hex — log-friendly identity prefix (module-
/// local copy of the identity module's private helper, same as
/// `org.rs`).
fn hex_short(bytes: &[u8; 32]) -> String {
    hex::encode(&bytes[..8])
}

/// Grant rights bitset: `DISCOVER` and `INVOKE` are independent
/// (plan §2.2). Unknown bits are refused at issue and decode —
/// wire evolution is honest, so an old verifier never silently
/// masks away a right it does not understand.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GrantRights(u32);

impl GrantRights {
    /// May receive this capability's scoped announcements.
    pub const DISCOVER: Self = Self(1);
    /// May invoke this capability (subject to full admission).
    pub const INVOKE: Self = Self(1 << 1);
    /// Every bit this build understands.
    const KNOWN_MASK: u32 = 0b11;

    /// The union of two rights sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `true` iff every bit of `other` is present in `self`
    /// (`self ⊇ other`).
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The raw bits (wire form).
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Strict wire decode: empty and unknown bits are typed
    /// errors, never masked.
    pub fn try_from_bits(bits: u32) -> Result<Self, OrgError> {
        if bits == 0 {
            return Err(OrgError::EmptyRights);
        }
        if bits & !Self::KNOWN_MASK != 0 {
            return Err(OrgError::UnknownRights);
        }
        Ok(Self(bits))
    }
}

impl std::fmt::Debug for GrantRights {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.contains(Self::DISCOVER) {
            parts.push("DISCOVER");
        }
        if self.contains(Self::INVOKE) {
            parts.push("INVOKE");
        }
        if self.0 & !Self::KNOWN_MASK != 0 {
            parts.push("UNKNOWN");
        }
        write!(f, "GrantRights({})", parts.join("|"))
    }
}

/// What a dispatcher grant lets the dispatcher act on: one exact
/// capability, or any capability of the issuing org.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DispatcherScope {
    /// Exactly this capability.
    Exact(CapabilityAuthorityId),
    /// Any capability (the org trusts this dispatcher broadly —
    /// e.g. a scheduler).
    Any,
}

/// Whose nodes a capability grant covers. The call ALWAYS names an
/// exact provider P (plan §2.2) — this scope only bounds which P a
/// verifier may accept.
///
/// `ExactNode` carries the provider's `EntityId` — the
/// TOFU-authenticated cryptographic identity — deliberately NOT
/// the derived 64-bit `node_id`: an org-signed authority object
/// must not be satisfiable by a ~2³²-work grinding collision on
/// the short id. (Narrower than the plan's original
/// `ExactNode(NodeId)` sketch; reconciled to `EntityId` at the OA-2
/// exit gate — OA2-F.)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GrantTargetScope {
    /// Exactly this provider entity.
    ExactNode(EntityId),
    /// Any node owned by this org — reusable across discovered
    /// B-owned providers.
    AnyNodeOwnedBy(OrgId),
}

impl GrantTargetScope {
    /// Does this scope cover provider `entity`, whose PROVEN owner
    /// org (from its own installed authority scaffold — never fold
    /// state) is `owner`?
    ///
    /// `AnyNodeOwnedBy` with an unowned provider (`owner == None`)
    /// is `false`: an unadopted node is nobody's "node owned by".
    pub fn covers(&self, entity: &EntityId, owner: Option<&OrgId>) -> bool {
        match self {
            Self::ExactNode(exact) => exact == entity,
            Self::AnyNodeOwnedBy(org) => owner == Some(org),
        }
    }
}

/// The discovery half of a DISCOVER grant, INSIDE the signed
/// bytes: the audience routing handle plus the key COMMITMENT.
/// The raw key is never here (plan §2.2 — commitments in, keys
/// out).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GrantedDiscoveryBinding {
    /// Random per-grant audience routing handle. Public-ish;
    /// reveals nothing but linkage.
    pub audience_handle: [u8; 32],
    /// `blake3::derive_key("net-org-audience-commit-v1",
    /// discovery_key)` — lets a holder of the out-of-band key
    /// validate it against the signed grant without the key ever
    /// riding the wire.
    pub key_commitment: [u8; 32],
}

impl std::fmt::Debug for GrantedDiscoveryBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrantedDiscoveryBinding")
            .field("audience_handle", &hex_short(&self.audience_handle))
            .field("key_commitment", &hex_short(&self.key_commitment))
            .finish()
    }
}

/// The commitment for a raw discovery key.
pub fn audience_key_commitment(discovery_key: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key(AUDIENCE_COMMIT_CONTEXT, discovery_key)
}

/// The LOCAL, out-of-band half of a DISCOVER grant: the raw
/// audience decryption key, bound to its grant. Plain file 0600
/// under the config dir (Q2 — matches the org root key and the
/// OA-1 `owner-audience.key`), delivered out of band to B's
/// publishing nodes and A's consuming nodes.
///
/// NEVER on the wire, never in a proof, never in `Debug` output —
/// and structurally non-serializable: the compile-time assertion
/// below refuses a build in which this type gains a serde
/// `Serialize` impl, so it can never become a member of any wire
/// object (plan v1.3 carry-forward; witnessed in §2.6's gate).
pub struct OrgAudienceSecret {
    /// The grant this key belongs to.
    pub grant_id: [u8; 32],
    /// The audience routing handle (matches the signed binding).
    pub audience_handle: [u8; 32],
    /// The audience decryption key. SECRET.
    discovery_key: [u8; 32],
}

/// §2.6 type-level assertion: `OrgAudienceSecret` must never
/// implement `serde::Serialize`. If it ever does, the blanket impl
/// below becomes ambiguous with the `()` impl and this constant
/// fails to compile (the `static_assertions::assert_not_impl_any`
/// mechanism, inlined to avoid a dependency).
const _: fn() = || {
    trait AmbiguousIfSerialize<A> {
        fn guard() {}
    }
    impl<T: ?Sized> AmbiguousIfSerialize<()> for T {}
    #[allow(dead_code)]
    struct IsSerialize;
    impl<T: ?Sized + serde::Serialize> AmbiguousIfSerialize<IsSerialize> for T {}
    let _ = <OrgAudienceSecret as AmbiguousIfSerialize<_>>::guard;
};

/// Config-file codec version for [`OrgAudienceSecret`].
pub const ORG_AUDIENCE_SECRET_VERSION: u8 = 1;

impl OrgAudienceSecret {
    /// Encoded size of the explicit config codec (NOT a wire
    /// format): version ‖ grant_id ‖ handle ‖ key.
    pub const ENCODED_SIZE: usize = 1 + 32 + 32 + 32;

    /// Mint fresh audience material for `grant_id`: a random
    /// handle, a random key, and the signed-side binding
    /// committing to them. `getrandom` failure aborts — a
    /// predictable audience key would let anyone decrypt scoped
    /// announcements (same rationale as
    /// `OwnerAudienceCredential::generate`).
    pub(crate) fn mint(grant_id: [u8; 32]) -> (Self, GrantedDiscoveryBinding) {
        let mut bytes = [0u8; 64];
        if let Err(e) = getrandom::fill(&mut bytes) {
            eprintln!(
                "FATAL: OrgAudienceSecret getrandom failure ({e:?}); aborting to avoid predictable audience key"
            );
            std::process::abort();
        }
        let mut audience_handle = [0u8; 32];
        let mut discovery_key = [0u8; 32];
        audience_handle.copy_from_slice(&bytes[..32]);
        discovery_key.copy_from_slice(&bytes[32..]);
        // Zeroize the staging buffer — volatile writes prevent
        // optimizer elision.
        for byte in bytes.iter_mut() {
            // SAFETY: `byte` is a valid mutable reference into
            // `bytes` for this iteration, which is all
            // `ptr::write_volatile` requires.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        let binding = GrantedDiscoveryBinding {
            audience_handle,
            key_commitment: audience_key_commitment(&discovery_key),
        };
        (
            Self {
                grant_id,
                audience_handle,
                discovery_key,
            },
            binding,
        )
    }

    /// The audience decryption key. Deliberately a borrowing
    /// accessor rather than a public field so every use site is
    /// greppable.
    pub fn discovery_key(&self) -> &[u8; 32] {
        &self.discovery_key
    }

    /// Validate this secret against a grant's SIGNED binding:
    /// handle equal AND `key_commitment` equal to this key's
    /// commitment. A mismatch means the out-of-band material does
    /// not belong to the grant — reject locally, before any use
    /// (§2.6 witness).
    pub fn matches_binding(&self, binding: &GrantedDiscoveryBinding) -> bool {
        self.audience_handle == binding.audience_handle
            && audience_key_commitment(&self.discovery_key) == binding.key_commitment
    }

    /// Whole-object match against a capability GRANT (Kyra OA2-F): the secret
    /// is the out-of-band key for THIS grant iff its `grant_id` matches AND the
    /// grant carries a discovery binding this secret satisfies. Prefer this over
    /// [`Self::matches_binding`] at call sites — a bare binding cannot express
    /// the `grant_id`, so matching a binding alone leaves grant-id validation to
    /// the caller (and a same-`grant_id`/wrong-`key_commitment` mismatch must
    /// still be rejected on the commitment, not merely on a differing handle).
    pub fn matches_grant(&self, grant: &OrgCapabilityGrant) -> bool {
        self.grant_id == grant.grant_id
            && grant
                .discovery
                .as_ref()
                .is_some_and(|binding| self.matches_binding(binding))
    }

    /// Explicit config-file codec:
    /// `version ‖ grant_id ‖ handle ‖ key`, exactly
    /// [`Self::ENCODED_SIZE`] bytes.
    pub fn encode_config(&self) -> [u8; Self::ENCODED_SIZE] {
        let mut buf = [0u8; Self::ENCODED_SIZE];
        buf[0] = ORG_AUDIENCE_SECRET_VERSION;
        buf[1..33].copy_from_slice(&self.grant_id);
        buf[33..65].copy_from_slice(&self.audience_handle);
        buf[65..97].copy_from_slice(&self.discovery_key);
        buf
    }

    /// Strict inverse of [`Self::encode_config`]: exact length and
    /// known version byte, or a loud typed error.
    #[expect(
        clippy::unwrap_used,
        reason = "length checked to be exactly ENCODED_SIZE above; fixed slices convert infallibly"
    )]
    pub fn decode_config(bytes: &[u8]) -> Result<Self, OrgError> {
        if bytes.len() != Self::ENCODED_SIZE || bytes[0] != ORG_AUDIENCE_SECRET_VERSION {
            return Err(OrgError::InvalidFormat);
        }
        Ok(Self {
            grant_id: bytes[1..33].try_into().unwrap(),
            audience_handle: bytes[33..65].try_into().unwrap(),
            discovery_key: bytes[65..97].try_into().unwrap(),
        })
    }
}

impl std::fmt::Debug for OrgAudienceSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgAudienceSecret")
            .field("grant_id", &hex::encode(&self.grant_id[..8]))
            .field("audience_handle", &hex::encode(&self.audience_handle[..8]))
            .field("discovery_key", &"[REDACTED]")
            .finish()
    }
}

impl Drop for OrgAudienceSecret {
    fn drop(&mut self) {
        // Zeroize the key on drop — volatile writes prevent
        // optimizer elision.
        for byte in self.discovery_key.iter_mut() {
            // SAFETY: `byte` is a valid mutable reference into the
            // owned array for this iteration, which is all
            // `ptr::write_volatile` requires.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

// ---------------------------------------------------------------------------
// OrgDispatcherGrant
// ---------------------------------------------------------------------------

/// A → S: "entity `dispatcher` may act FOR org `org_id`" over
/// `capability_scope`. Fixed one-hop and org-root-signed (Locked
/// #4): there are no delegation chains in v1, so verification is
/// one signature against the org root, never a chain walk.
///
/// Wire format (185 bytes):
/// ```text
/// org_id:       32 (OrgId — issuing org root, the verifying key)
/// dispatcher:   32 (EntityId empowered to act for the org)
/// scope_tag:     1 (0x01 = Exact, 0x02 = Any)
/// capability:   32 (CapabilityAuthorityId; ZERO-filled for Any)
/// not_before:    8 (u64 unix seconds)
/// not_after:     8 (u64 unix seconds, exclusive)
/// nonce:         8 (u64; re-issues byte-distinct)
/// --- signed above (ORG_DISPATCHER_GRANT_SIG_DOMAIN prefixed) ---
/// signature:    64 (ed25519 by org_id)
/// ```
///
/// Holding one is never invocation authority: admission verifies
/// the full per-call proof, and the provider's own policy is
/// always final.
#[derive(Clone, PartialEq, Eq)]
pub struct OrgDispatcherGrant {
    /// The org the dispatcher acts for (also the verifying key).
    pub org_id: OrgId,
    /// The entity empowered to dispatch.
    pub dispatcher: EntityId,
    /// Which capabilities the dispatcher may act on.
    pub capability_scope: DispatcherScope,
    /// Valid from (unix seconds).
    pub not_before: u64,
    /// Valid until (unix seconds, exclusive).
    pub not_after: u64,
    /// Random per-issue nonce.
    pub nonce: u64,
    /// ed25519 signature over the domain-prefixed payload.
    pub signature: [u8; 64],
}

const DISPATCHER_SCOPE_TAG_EXACT: u8 = 0x01;
const DISPATCHER_SCOPE_TAG_ANY: u8 = 0x02;

impl OrgDispatcherGrant {
    /// Size of the signed payload (everything before the
    /// signature).
    const SIGNED_PAYLOAD_SIZE: usize = 32 + 32 + 1 + 32 + 8 + 8 + 8; // 121

    /// Size of the domain-prefixed signing input.
    const SIGNING_INPUT_SIZE: usize =
        ORG_DISPATCHER_GRANT_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Total serialized size.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64; // 185

    /// Issue a dispatcher grant valid from now for
    /// `duration_secs`. Rejects zero and over-ceiling TTLs with
    /// typed errors (same discipline as the membership cert).
    pub fn try_issue(
        org: &OrgKeypair,
        dispatcher: EntityId,
        capability_scope: DispatcherScope,
        duration_secs: u64,
    ) -> Result<Self, OrgError> {
        if duration_secs == 0 {
            return Err(OrgError::ZeroTtl);
        }
        if duration_secs > MAX_ORG_GRANT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        let nonce = fresh_nonce("OrgDispatcherGrant");
        let now = current_timestamp();
        Ok(Self::issue_at(
            org,
            dispatcher,
            capability_scope,
            now,
            now.saturating_add(duration_secs),
            nonce,
        ))
    }

    /// Build and sign with fully explicit fields. `pub(crate)` —
    /// the public issuing surface is [`Self::try_issue`]; golden
    /// vectors and in-crate tooling pin deterministic bytes here.
    pub(crate) fn issue_at(
        org: &OrgKeypair,
        dispatcher: EntityId,
        capability_scope: DispatcherScope,
        not_before: u64,
        not_after: u64,
        nonce: u64,
    ) -> Self {
        let mut grant = Self {
            org_id: org.org_id(),
            dispatcher,
            capability_scope,
            not_before,
            not_after,
            nonce,
            signature: [0u8; 64],
        };
        grant.signature = org.sign(&grant.signing_input()).to_bytes();
        grant
    }

    /// Canonical signed payload — fixed offsets, little-endian.
    /// The scope tag byte keeps the encoding injective even though
    /// `Any` zero-fills the capability field.
    pub(crate) fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
        let mut buf = [0u8; Self::SIGNED_PAYLOAD_SIZE];
        let mut off = 0;
        buf[off..off + 32].copy_from_slice(self.org_id.as_bytes());
        off += 32;
        buf[off..off + 32].copy_from_slice(self.dispatcher.as_bytes());
        off += 32;
        match &self.capability_scope {
            DispatcherScope::Exact(cap) => {
                buf[off] = DISPATCHER_SCOPE_TAG_EXACT;
                buf[off + 1..off + 33].copy_from_slice(cap.as_bytes());
            }
            DispatcherScope::Any => {
                buf[off] = DISPATCHER_SCOPE_TAG_ANY;
                // capability bytes stay zero
            }
        }
        off += 33;
        buf[off..off + 8].copy_from_slice(&self.not_before.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.not_after.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.nonce.to_le_bytes());
        buf
    }

    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..ORG_DISPATCHER_GRANT_SIG_DOMAIN.len()]
            .copy_from_slice(ORG_DISPATCHER_GRANT_SIG_DOMAIN);
        buf[ORG_DISPATCHER_GRANT_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
        buf
    }

    /// Verify structural validity and the signature: window shape,
    /// TTL ceiling (issue AND verify), then `verify_strict`
    /// against `org_id`. No wall-clock or floor checks — those are
    /// contextual ([`Self::is_valid_with_skew`]; floors apply to
    /// the membership cert, not grants, in v1).
    pub fn verify(&self) -> Result<(), OrgError> {
        if self.not_after <= self.not_before {
            return Err(OrgError::InvalidValidityWindow);
        }
        if self.not_after - self.not_before > MAX_ORG_GRANT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        let sig = Signature::from_bytes(&self.signature);
        self.org_id.verify(&self.signing_input(), &sig)
    }

    /// Signature + wall-clock validity with `skew_secs` tolerance
    /// on both bounds (ceiling-enforced, same as the cert).
    pub fn is_valid_with_skew(&self, skew_secs: u64) -> Result<(), OrgError> {
        self.is_valid_at_with_skew(current_timestamp(), skew_secs)
    }

    /// Explicit-time variant (Kyra E1 audit): validate against a
    /// caller-supplied `now_secs` instead of re-reading the wall
    /// clock, so one admission uses a single clock sample.
    pub fn is_valid_at_with_skew(&self, now_secs: u64, skew_secs: u64) -> Result<(), OrgError> {
        if skew_secs > MAX_TOKEN_CLOCK_SKEW_SECS {
            return Err(OrgError::ClockSkewTooLarge);
        }
        self.verify()?;
        check_time_bounds_at(self.not_before, self.not_after, now_secs, skew_secs)
    }

    /// Does this grant's scope cover `capability`?
    pub fn covers_capability(&self, capability: &CapabilityAuthorityId) -> bool {
        match &self.capability_scope {
            DispatcherScope::Exact(exact) => exact == capability,
            DispatcherScope::Any => true,
        }
    }

    /// Serialize to canonical wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::WIRE_SIZE);
        buf.extend_from_slice(&self.signed_payload());
        buf.extend_from_slice(&self.signature);
        buf
    }

    /// Strict wire decode: exact length, known scope tag, and the
    /// canonical zero-fill for `Any` (a nonzero capability under an
    /// `Any` tag would make two byte forms decode to one value).
    /// Decoding does NOT verify the signature.
    #[expect(
        clippy::unwrap_used,
        reason = "data.len() == WIRE_SIZE checked above; fixed-offset slices convert infallibly"
    )]
    pub fn from_bytes(data: &[u8]) -> Result<Self, OrgError> {
        if data.len() != Self::WIRE_SIZE {
            return Err(OrgError::InvalidFormat);
        }
        let org_id = OrgId::from_bytes(data[0..32].try_into().unwrap());
        let dispatcher = EntityId::from_bytes(data[32..64].try_into().unwrap());
        let capability_bytes: [u8; 32] = data[65..97].try_into().unwrap();
        let capability_scope = match data[64] {
            DISPATCHER_SCOPE_TAG_EXACT => {
                DispatcherScope::Exact(CapabilityAuthorityId::from_bytes(capability_bytes))
            }
            DISPATCHER_SCOPE_TAG_ANY => {
                if capability_bytes != [0u8; 32] {
                    return Err(OrgError::InvalidFormat);
                }
                DispatcherScope::Any
            }
            _ => return Err(OrgError::InvalidFormat),
        };
        let not_before = u64::from_le_bytes(data[97..105].try_into().unwrap());
        let not_after = u64::from_le_bytes(data[105..113].try_into().unwrap());
        let nonce = u64::from_le_bytes(data[113..121].try_into().unwrap());
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&data[121..185]);
        Ok(Self {
            org_id,
            dispatcher,
            capability_scope,
            not_before,
            not_after,
            nonce,
            signature,
        })
    }
}

impl std::fmt::Debug for OrgDispatcherGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgDispatcherGrant")
            .field("org_id", &self.org_id)
            .field("dispatcher", &self.dispatcher)
            .field("capability_scope", &self.capability_scope)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .field("nonce", &self.nonce)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// OrgCapabilityGrant
// ---------------------------------------------------------------------------

/// B → A: "org `grantee_org` holds `rights` on capability
/// `capability` over `target_scope`", signed by provider org
/// `issuer_org`. Cross-org access is ALWAYS this grant — never
/// co-membership (Locked #2).
///
/// Wire format (318 bytes):
/// ```text
/// grant_id:         32 (random; ZERO is RESERVED — the OA-3
///                       owner-audience sentinel — and refused)
/// issuer_org:       32 (OrgId B — the verifying key)
/// grantee_org:      32 (OrgId A)
/// capability:       32 (CapabilityAuthorityId)
/// rights:            4 (u32 LE bitset: DISCOVER=1, INVOKE=2)
/// target_tag:        1 (0x01 = ExactNode, 0x02 = AnyNodeOwnedBy)
/// target_id:        32 (EntityId or OrgId per tag)
/// discovery_tag:     1 (0x00 = absent, 0x01 = present)
/// audience_handle:  32 (ZERO-filled when absent)
/// key_commitment:   32 (ZERO-filled when absent)
/// not_before:        8 (u64 unix seconds)
/// not_after:         8 (u64 unix seconds, exclusive)
/// nonce:             8
/// --- signed above (ORG_CAPABILITY_GRANT_SIG_DOMAIN prefixed) ---
/// signature:        64 (ed25519 by issuer_org)
/// ```
///
/// Structural rule, enforced at issue AND decode AND verify:
/// `rights ⊇ DISCOVER ⇔ discovery binding present`.
#[derive(Clone, PartialEq, Eq)]
pub struct OrgCapabilityGrant {
    /// Random per-grant id; zero reserved.
    pub grant_id: [u8; 32],
    /// The granting (provider) org — the verifying key.
    pub issuer_org: OrgId,
    /// The org being granted access.
    pub grantee_org: OrgId,
    /// The capability being granted, by authority id.
    pub capability: CapabilityAuthorityId,
    /// DISCOVER and/or INVOKE.
    pub rights: GrantRights,
    /// Which provider nodes the grant covers.
    pub target_scope: GrantTargetScope,
    /// Present iff `rights ⊇ DISCOVER` — the audience handle and
    /// key commitment (the raw key is out of band, in
    /// [`OrgAudienceSecret`]).
    pub discovery: Option<GrantedDiscoveryBinding>,
    /// Valid from (unix seconds).
    pub not_before: u64,
    /// Valid until (unix seconds, exclusive).
    pub not_after: u64,
    /// Random per-issue nonce.
    pub nonce: u64,
    /// ed25519 signature over the domain-prefixed payload.
    pub signature: [u8; 64],
}

const TARGET_TAG_EXACT_NODE: u8 = 0x01;
const TARGET_TAG_ANY_NODE_OWNED_BY: u8 = 0x02;
const DISCOVERY_TAG_ABSENT: u8 = 0x00;
const DISCOVERY_TAG_PRESENT: u8 = 0x01;

impl OrgCapabilityGrant {
    /// Size of the signed payload (everything before the
    /// signature).
    const SIGNED_PAYLOAD_SIZE: usize = 32 + 32 + 32 + 32 + 4 + 1 + 32 + 1 + 32 + 32 + 8 + 8 + 8; // 254

    /// Size of the domain-prefixed signing input.
    const SIGNING_INPUT_SIZE: usize =
        ORG_CAPABILITY_GRANT_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Total serialized size.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64; // 318

    /// The target-scope owner rule (Kyra OA2-F): an `AnyNodeOwnedBy(org)` target
    /// must name the ISSUER's own org. A grant B→A over `AnyNodeOwnedBy(C != B)`
    /// names providers owned by a FOREIGN org C and can never admit (admission
    /// requires the provider's owner == issuer), so it is refused rather than
    /// minted as a permanently-unusable credential. `ExactNode` carries no org —
    /// its owner is checked only at admission. Enforced at issue AND decode/verify.
    fn check_target_owner(
        issuer_org: &OrgId,
        target_scope: &GrantTargetScope,
    ) -> Result<(), OrgError> {
        match target_scope {
            GrantTargetScope::AnyNodeOwnedBy(org) if org != issuer_org => {
                Err(OrgError::TargetOrgNotIssuer)
            }
            _ => Ok(()),
        }
    }

    /// Issue a capability grant valid from now for
    /// `duration_secs`.
    ///
    /// The structural rule holds BY CONSTRUCTION: when `rights ⊇
    /// DISCOVER`, fresh audience material is minted (random
    /// handle, random key, commitment into the signed bytes) and
    /// the [`OrgAudienceSecret`] is returned alongside the grant
    /// for out-of-band delivery; otherwise no binding exists and
    /// `None` is returned. There is deliberately NO caller-supplied
    /// key surface — one DISCOVER grant, one unique handle, one
    /// unique key.
    pub fn try_issue(
        issuer: &OrgKeypair,
        grantee_org: OrgId,
        capability: CapabilityAuthorityId,
        rights: GrantRights,
        target_scope: GrantTargetScope,
        duration_secs: u64,
    ) -> Result<(Self, Option<OrgAudienceSecret>), OrgError> {
        // Re-validate the bits even though `GrantRights` values are
        // constructed through the checked API — the bitset is
        // `Copy` and could arrive from a decode path.
        let rights = GrantRights::try_from_bits(rights.bits())?;
        if duration_secs == 0 {
            return Err(OrgError::ZeroTtl);
        }
        if duration_secs > MAX_ORG_GRANT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        Self::check_target_owner(&issuer.org_id(), &target_scope)?;
        let mut grant_id = [0u8; 32];
        if let Err(e) = getrandom::fill(&mut grant_id) {
            eprintln!(
                "FATAL: OrgCapabilityGrant grant_id getrandom failure ({e:?}); aborting to avoid predictable grant id"
            );
            std::process::abort();
        }
        // Zero grant_id from the RNG is 2^-256 — but the reserved
        // check is cheap and the invariant is worth keeping
        // structural.
        if grant_id == [0u8; 32] {
            return Err(OrgError::ReservedGrantId);
        }
        let (secret, binding) = if rights.contains(GrantRights::DISCOVER) {
            let (secret, binding) = OrgAudienceSecret::mint(grant_id);
            (Some(secret), Some(binding))
        } else {
            (None, None)
        };
        let nonce = fresh_nonce("OrgCapabilityGrant");
        let now = current_timestamp();
        let grant = Self::issue_at(
            issuer,
            grant_id,
            grantee_org,
            capability,
            rights,
            target_scope,
            binding,
            now,
            now.saturating_add(duration_secs),
            nonce,
        );
        Ok((grant, secret))
    }

    /// Build and sign with fully explicit fields — the raw pin
    /// surface for golden vectors and structural-rule witnesses.
    /// Does NOT enforce the issue-path invariants; [`Self::verify`]
    /// and [`Self::from_bytes`] do.
    #[expect(
        clippy::too_many_arguments,
        reason = "raw golden-vector pin surface; the public API is try_issue"
    )]
    pub(crate) fn issue_at(
        issuer: &OrgKeypair,
        grant_id: [u8; 32],
        grantee_org: OrgId,
        capability: CapabilityAuthorityId,
        rights: GrantRights,
        target_scope: GrantTargetScope,
        discovery: Option<GrantedDiscoveryBinding>,
        not_before: u64,
        not_after: u64,
        nonce: u64,
    ) -> Self {
        let mut grant = Self {
            grant_id,
            issuer_org: issuer.org_id(),
            grantee_org,
            capability,
            rights,
            target_scope,
            discovery,
            not_before,
            not_after,
            nonce,
            signature: [0u8; 64],
        };
        grant.signature = issuer.sign(&grant.signing_input()).to_bytes();
        grant
    }

    /// Canonical signed payload — fixed offsets, little-endian.
    /// Presence tags keep the encoding injective across the
    /// zero-filled optional regions.
    pub(crate) fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
        let mut buf = [0u8; Self::SIGNED_PAYLOAD_SIZE];
        let mut off = 0;
        buf[off..off + 32].copy_from_slice(&self.grant_id);
        off += 32;
        buf[off..off + 32].copy_from_slice(self.issuer_org.as_bytes());
        off += 32;
        buf[off..off + 32].copy_from_slice(self.grantee_org.as_bytes());
        off += 32;
        buf[off..off + 32].copy_from_slice(self.capability.as_bytes());
        off += 32;
        buf[off..off + 4].copy_from_slice(&self.rights.bits().to_le_bytes());
        off += 4;
        match &self.target_scope {
            GrantTargetScope::ExactNode(entity) => {
                buf[off] = TARGET_TAG_EXACT_NODE;
                buf[off + 1..off + 33].copy_from_slice(entity.as_bytes());
            }
            GrantTargetScope::AnyNodeOwnedBy(org) => {
                buf[off] = TARGET_TAG_ANY_NODE_OWNED_BY;
                buf[off + 1..off + 33].copy_from_slice(org.as_bytes());
            }
        }
        off += 33;
        match &self.discovery {
            Some(binding) => {
                buf[off] = DISCOVERY_TAG_PRESENT;
                buf[off + 1..off + 33].copy_from_slice(&binding.audience_handle);
                buf[off + 33..off + 65].copy_from_slice(&binding.key_commitment);
            }
            None => {
                buf[off] = DISCOVERY_TAG_ABSENT;
                // handle + commitment stay zero
            }
        }
        off += 65;
        buf[off..off + 8].copy_from_slice(&self.not_before.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.not_after.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.nonce.to_le_bytes());
        buf
    }

    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..ORG_CAPABILITY_GRANT_SIG_DOMAIN.len()]
            .copy_from_slice(ORG_CAPABILITY_GRANT_SIG_DOMAIN);
        buf[ORG_CAPABILITY_GRANT_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
        buf
    }

    /// Verify structural validity and the signature, in order:
    /// window shape → TTL ceiling → reserved grant_id → rights
    /// bits (empty/unknown) → the DISCOVER ⇔ binding structural
    /// rule → `verify_strict` against `issuer_org`. Fields are
    /// public, so every invariant is re-checked here rather than
    /// trusted to the issue path.
    pub fn verify(&self) -> Result<(), OrgError> {
        if self.not_after <= self.not_before {
            return Err(OrgError::InvalidValidityWindow);
        }
        if self.not_after - self.not_before > MAX_ORG_GRANT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        if self.grant_id == [0u8; 32] {
            return Err(OrgError::ReservedGrantId);
        }
        let rights = GrantRights::try_from_bits(self.rights.bits())?;
        if rights.contains(GrantRights::DISCOVER) != self.discovery.is_some() {
            return Err(OrgError::DiscoveryBindingMismatch);
        }
        Self::check_target_owner(&self.issuer_org, &self.target_scope)?;
        let sig = Signature::from_bytes(&self.signature);
        self.issuer_org.verify(&self.signing_input(), &sig)
    }

    /// Signature + wall-clock validity with `skew_secs` tolerance
    /// on both bounds (ceiling-enforced).
    pub fn is_valid_with_skew(&self, skew_secs: u64) -> Result<(), OrgError> {
        self.is_valid_at_with_skew(current_timestamp(), skew_secs)
    }

    /// Explicit-time variant (Kyra E1 audit): validate against a
    /// caller-supplied `now_secs` instead of re-reading the wall
    /// clock, so one admission uses a single clock sample.
    pub fn is_valid_at_with_skew(&self, now_secs: u64, skew_secs: u64) -> Result<(), OrgError> {
        if skew_secs > MAX_TOKEN_CLOCK_SKEW_SECS {
            return Err(OrgError::ClockSkewTooLarge);
        }
        self.verify()?;
        check_time_bounds_at(self.not_before, self.not_after, now_secs, skew_secs)
    }

    /// `rights ⊇ INVOKE`.
    pub fn permits_invoke(&self) -> bool {
        self.rights.contains(GrantRights::INVOKE)
    }

    /// `rights ⊇ DISCOVER`.
    pub fn permits_discover(&self) -> bool {
        self.rights.contains(GrantRights::DISCOVER)
    }

    /// Serialize to canonical wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::WIRE_SIZE);
        buf.extend_from_slice(&self.signed_payload());
        buf.extend_from_slice(&self.signature);
        buf
    }

    /// Strict wire decode: exact length; known tags; canonical
    /// zero-fill under absent tags; the reserved-zero grant_id;
    /// rights bits; and the DISCOVER ⇔ binding structural rule —
    /// all BEFORE the caller ever sees a value (issue AND decode,
    /// plan §2.2). Decoding does NOT verify the signature.
    #[expect(
        clippy::unwrap_used,
        reason = "data.len() == WIRE_SIZE checked above; fixed-offset slices convert infallibly"
    )]
    pub fn from_bytes(data: &[u8]) -> Result<Self, OrgError> {
        if data.len() != Self::WIRE_SIZE {
            return Err(OrgError::InvalidFormat);
        }
        let grant_id: [u8; 32] = data[0..32].try_into().unwrap();
        if grant_id == [0u8; 32] {
            return Err(OrgError::ReservedGrantId);
        }
        let issuer_org = OrgId::from_bytes(data[32..64].try_into().unwrap());
        let grantee_org = OrgId::from_bytes(data[64..96].try_into().unwrap());
        let capability = CapabilityAuthorityId::from_bytes(data[96..128].try_into().unwrap());
        let rights =
            GrantRights::try_from_bits(u32::from_le_bytes(data[128..132].try_into().unwrap()))?;
        let target_bytes: [u8; 32] = data[133..165].try_into().unwrap();
        let target_scope = match data[132] {
            TARGET_TAG_EXACT_NODE => {
                GrantTargetScope::ExactNode(EntityId::from_bytes(target_bytes))
            }
            TARGET_TAG_ANY_NODE_OWNED_BY => {
                GrantTargetScope::AnyNodeOwnedBy(OrgId::from_bytes(target_bytes))
            }
            _ => return Err(OrgError::InvalidFormat),
        };
        let audience_handle: [u8; 32] = data[166..198].try_into().unwrap();
        let key_commitment: [u8; 32] = data[198..230].try_into().unwrap();
        let discovery = match data[165] {
            DISCOVERY_TAG_PRESENT => Some(GrantedDiscoveryBinding {
                audience_handle,
                key_commitment,
            }),
            DISCOVERY_TAG_ABSENT => {
                if audience_handle != [0u8; 32] || key_commitment != [0u8; 32] {
                    return Err(OrgError::InvalidFormat);
                }
                None
            }
            _ => return Err(OrgError::InvalidFormat),
        };
        if rights.contains(GrantRights::DISCOVER) != discovery.is_some() {
            return Err(OrgError::DiscoveryBindingMismatch);
        }
        Self::check_target_owner(&issuer_org, &target_scope)?;
        let not_before = u64::from_le_bytes(data[230..238].try_into().unwrap());
        let not_after = u64::from_le_bytes(data[238..246].try_into().unwrap());
        let nonce = u64::from_le_bytes(data[246..254].try_into().unwrap());
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&data[254..318]);
        Ok(Self {
            grant_id,
            issuer_org,
            grantee_org,
            capability,
            rights,
            target_scope,
            discovery,
            not_before,
            not_after,
            nonce,
            signature,
        })
    }
}

impl std::fmt::Debug for OrgCapabilityGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgCapabilityGrant")
            .field("grant_id", &hex::encode(&self.grant_id[..8]))
            .field("issuer_org", &self.issuer_org)
            .field("grantee_org", &self.grantee_org)
            .field("capability", &self.capability)
            .field("rights", &self.rights)
            .field("target_scope", &self.target_scope)
            .field("discovery", &self.discovery)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .field("nonce", &self.nonce)
            .finish()
    }
}

// Serde rides the canonical wire bytes for both grants — hex when
// human-readable, raw bytes otherwise; decode goes through
// `from_bytes`, so the strict structural contract holds in every
// serialized form (same discipline as `OrgMembershipCert`).
impl serde::Serialize for OrgDispatcherGrant {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let bytes = self.to_bytes();
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex::encode(&bytes))
        } else {
            serializer.serialize_bytes(&bytes)
        }
    }
}

impl<'de> serde::Deserialize<'de> for OrgDispatcherGrant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes = if deserializer.is_human_readable() {
            let hex_str = String::deserialize(deserializer)?;
            hex::decode(&hex_str).map_err(serde::de::Error::custom)?
        } else {
            <Vec<u8>>::deserialize(deserializer)?
        };
        Self::from_bytes(&bytes).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for OrgCapabilityGrant {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let bytes = self.to_bytes();
        if serializer.is_human_readable() {
            serializer.serialize_str(&hex::encode(&bytes))
        } else {
            serializer.serialize_bytes(&bytes)
        }
    }
}

impl<'de> serde::Deserialize<'de> for OrgCapabilityGrant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes = if deserializer.is_human_readable() {
            let hex_str = String::deserialize(deserializer)?;
            hex::decode(&hex_str).map_err(serde::de::Error::custom)?
        } else {
            <Vec<u8>>::deserialize(deserializer)?
        };
        Self::from_bytes(&bytes).map_err(serde::de::Error::custom)
    }
}

/// Random per-issue nonce, abort-on-entropy-failure (a predictable
/// nonce breaks the byte-distinct-renewal contract; same rationale
/// as `OrgMembershipCert::try_issue`).
fn fresh_nonce(context: &str) -> u64 {
    let mut nonce_bytes = [0u8; 8];
    if let Err(e) = getrandom::fill(&mut nonce_bytes) {
        eprintln!(
            "FATAL: {context} nonce getrandom failure ({e:?}); aborting to avoid predictable nonce"
        );
        std::process::abort();
    }
    u64::from_le_bytes(nonce_bytes)
}

/// Wall-clock window check with skew — identical semantics to
/// `OrgMembershipCert::check_time_bounds` (saturating on both
/// bounds; inclusive-expiry convention).
/// Window check at a caller-supplied `now` (unix seconds), so one
/// admission uses a single clock sample for every grant (Kyra E1
/// audit). The wall-clock convenience wrapper is
/// [`OrgDispatcherGrant::is_valid_with_skew`] /
/// [`OrgCapabilityGrant::is_valid_with_skew`], which pass
/// `current_timestamp()`.
fn check_time_bounds_at(
    not_before: u64,
    not_after: u64,
    now: u64,
    skew_secs: u64,
) -> Result<(), OrgError> {
    if now < not_before.saturating_sub(skew_secs) {
        return Err(OrgError::NotYetValid);
    }
    if now >= not_after.saturating_add(skew_secs) {
        return Err(OrgError::Expired);
    }
    Ok(())
}

// Wire sizes are load-bearing (the §2.3 proof rides a bounded RPC
// header): pin them at compile time.
const _: () = assert!(OrgDispatcherGrant::WIRE_SIZE == 185);
const _: () = assert!(OrgCapabilityGrant::WIRE_SIZE == 318);

#[cfg(test)]
mod tests {
    use super::*;

    fn org_b() -> OrgKeypair {
        OrgKeypair::from_bytes([0x42u8; 32])
    }

    fn org_a() -> OrgKeypair {
        OrgKeypair::from_bytes([0x77u8; 32])
    }

    fn dispatcher() -> EntityId {
        EntityId::from_bytes([0x24u8; 32])
    }

    fn provider() -> EntityId {
        EntityId::from_bytes([0x99u8; 32])
    }

    fn cap() -> CapabilityAuthorityId {
        CapabilityAuthorityId::for_tag("nrpc:oa2-echo")
    }

    #[test]
    fn capability_authority_id_is_deterministic_and_tag_separated() {
        assert_eq!(cap(), CapabilityAuthorityId::for_tag("nrpc:oa2-echo"));
        assert_ne!(cap(), CapabilityAuthorityId::for_tag("nrpc:oa2-echo2"));
        assert_ne!(
            cap(),
            CapabilityAuthorityId::for_tag("nrpc:oa2-ech"),
            "prefix tags must not collide"
        );
        // The id is a derive_key output, never the tag bytes.
        assert_ne!(&cap().0[..13], b"nrpc:oa2-echo");
    }

    #[test]
    fn dispatcher_grant_roundtrip_verify_and_scope() {
        let exact = OrgDispatcherGrant::try_issue(
            &org_a(),
            dispatcher(),
            DispatcherScope::Exact(cap()),
            3600,
        )
        .expect("issue exact");
        exact.verify().expect("verify");
        exact.is_valid_with_skew(0).expect("live");
        assert!(exact.covers_capability(&cap()));
        assert!(!exact.covers_capability(&CapabilityAuthorityId::for_tag("nrpc:other")));
        let decoded = OrgDispatcherGrant::from_bytes(&exact.to_bytes()).expect("decode");
        assert_eq!(decoded, exact);
        decoded.verify().expect("decoded verifies");

        let any = OrgDispatcherGrant::try_issue(&org_a(), dispatcher(), DispatcherScope::Any, 3600)
            .expect("issue any");
        assert!(any.covers_capability(&cap()));
        let decoded = OrgDispatcherGrant::from_bytes(&any.to_bytes()).expect("decode any");
        assert_eq!(decoded, any);
    }

    #[test]
    fn dispatcher_grant_any_scope_demands_canonical_zero_fill() {
        let any = OrgDispatcherGrant::issue_at(
            &org_a(),
            dispatcher(),
            DispatcherScope::Any,
            1_000,
            2_000,
            7,
        );
        let mut bytes = any.to_bytes();
        // Nonzero capability bytes under the Any tag: two byte
        // forms must never decode to one value.
        bytes[70] = 1;
        assert!(matches!(
            OrgDispatcherGrant::from_bytes(&bytes),
            Err(OrgError::InvalidFormat)
        ));
        // Unknown scope tag.
        let mut bytes = any.to_bytes();
        bytes[64] = 0x7F;
        assert!(matches!(
            OrgDispatcherGrant::from_bytes(&bytes),
            Err(OrgError::InvalidFormat)
        ));
    }

    #[test]
    fn capability_grant_invoke_only_roundtrip() {
        let (grant, secret) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("issue");
        assert!(secret.is_none(), "INVOKE-only mints no audience material");
        assert!(grant.discovery.is_none());
        assert!(grant.permits_invoke());
        assert!(!grant.permits_discover());
        grant.verify().expect("verify");
        grant.is_valid_with_skew(0).expect("live");
        let decoded = OrgCapabilityGrant::from_bytes(&grant.to_bytes()).expect("decode");
        assert_eq!(decoded, grant);
        decoded.verify().expect("decoded verifies");
    }

    #[test]
    fn discover_grant_always_mints_fresh_audience_material() {
        let issue = || {
            OrgCapabilityGrant::try_issue(
                &org_b(),
                org_a().org_id(),
                cap(),
                GrantRights::DISCOVER.union(GrantRights::INVOKE),
                GrantTargetScope::AnyNodeOwnedBy(org_b().org_id()),
                3600,
            )
            .expect("issue")
        };
        let (grant1, secret1) = issue();
        let (grant2, secret2) = issue();
        let secret1 = secret1.expect("DISCOVER mints a secret");
        let secret2 = secret2.expect("DISCOVER mints a secret");
        let binding1 = grant1.discovery.expect("binding in the signed grant");
        let binding2 = grant2.discovery.expect("binding in the signed grant");

        // One grant ⇔ one unique handle ⇔ one unique key.
        assert_ne!(grant1.grant_id, grant2.grant_id);
        assert_ne!(binding1.audience_handle, binding2.audience_handle);
        assert_ne!(secret1.discovery_key(), secret2.discovery_key());
        assert_ne!(binding1.key_commitment, binding2.key_commitment);

        // The out-of-band secret validates against its own grant's
        // signed binding — and ONLY its own.
        assert_eq!(secret1.grant_id, grant1.grant_id);
        assert!(secret1.matches_binding(&binding1));
        assert!(!secret1.matches_binding(&binding2));

        // The commitment is the pinned derive of the key.
        assert_eq!(
            binding1.key_commitment,
            audience_key_commitment(secret1.discovery_key())
        );
    }

    /// §2.6 (Kyra OA2-F): `matches_grant` pins EACH relation field
    /// independently — the earlier "secret1 vs grant2's binding" negative was a
    /// false-green (it changed BOTH handle AND commitment, so deleting the
    /// production commitment check would still reject on the differing handle).
    /// Mutate exactly one field at a time, and confirm an "installed" secret
    /// (round-tripped through its on-disk `encode_config` form) behaves the same.
    #[test]
    fn matches_grant_pins_each_field_independently() {
        let issue = || {
            OrgCapabilityGrant::try_issue(
                &org_b(),
                org_a().org_id(),
                cap(),
                GrantRights::DISCOVER,
                GrantTargetScope::ExactNode(provider()),
                3600,
            )
            .expect("issue")
        };
        let (grant, secret) = issue();
        let secret = secret.expect("DISCOVER mints a secret");

        // Own grant → true.
        assert!(
            secret.matches_grant(&grant),
            "the secret matches its own grant"
        );

        // Same handle, WRONG commitment → false (rejected on the COMMITMENT,
        // not merely on a differing handle).
        let mut wrong_commitment = grant.clone();
        wrong_commitment.discovery.as_mut().unwrap().key_commitment[0] ^= 0xFF;
        assert!(
            !secret.matches_grant(&wrong_commitment),
            "wrong commitment rejected even with a matching handle",
        );

        // Same commitment, WRONG handle → false.
        let mut wrong_handle = grant.clone();
        wrong_handle.discovery.as_mut().unwrap().audience_handle[0] ^= 0xFF;
        assert!(
            !secret.matches_grant(&wrong_handle),
            "wrong handle rejected even with a matching commitment",
        );

        // Matching binding, WRONG grant_id → false (the piece `matches_binding`
        // alone cannot express).
        let mut wrong_grant_id = grant.clone();
        wrong_grant_id.grant_id[0] ^= 0xFF;
        assert!(
            secret.matches_binding(wrong_grant_id.discovery.as_ref().unwrap()),
            "the binding still matches — only the grant_id differs",
        );
        assert!(
            !secret.matches_grant(&wrong_grant_id),
            "wrong grant_id rejected despite a matching binding",
        );

        // Grant with NO discovery binding → false.
        let (invoke_only, _none) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("invoke-only");
        assert!(
            !secret.matches_grant(&invoke_only),
            "a grant with no discovery binding never matches",
        );

        // The "installed" secret (encode_config round-trip) → same results.
        let reloaded = OrgAudienceSecret::decode_config(&secret.encode_config())
            .expect("decode_config round-trips the installed secret");
        assert!(reloaded.matches_grant(&grant));
        assert!(!reloaded.matches_grant(&wrong_commitment));
        assert!(!reloaded.matches_grant(&wrong_handle));
        assert!(!reloaded.matches_grant(&wrong_grant_id));
    }

    /// Kyra OA2-F: an `AnyNodeOwnedBy(org)` target whose org is NOT the issuer
    /// is a permanently-unusable grant (admission requires the provider's owner
    /// == issuer) — refused at issue AND decode/verify. `AnyNodeOwnedBy(issuer)`
    /// is fine; `ExactNode` carries no org and is checked only at admission.
    #[test]
    fn foreign_owner_any_node_target_is_refused_at_issue_and_decode() {
        // Issue path: self-owned OK, foreign-owned refused.
        assert!(OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::AnyNodeOwnedBy(org_b().org_id()),
            3600,
        )
        .is_ok());
        assert_eq!(
            OrgCapabilityGrant::try_issue(
                &org_b(),
                org_a().org_id(),
                cap(),
                GrantRights::INVOKE,
                GrantTargetScope::AnyNodeOwnedBy(org_a().org_id()),
                3600,
            )
            .map(|_| ())
            .unwrap_err(),
            OrgError::TargetOrgNotIssuer,
        );

        // Decode/verify path: forge a foreign-owner grant through the raw pin
        // surface (issue_at bypasses the issue check), then verify() and
        // from_bytes both reject it.
        let now = current_timestamp();
        let forged = OrgCapabilityGrant::issue_at(
            &org_b(),
            [7u8; 32],
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::AnyNodeOwnedBy(org_a().org_id()),
            None,
            now,
            now + 3600,
            1,
        );
        assert_eq!(forged.verify(), Err(OrgError::TargetOrgNotIssuer));
        assert_eq!(
            OrgCapabilityGrant::from_bytes(&forged.to_bytes())
                .map(|_| ())
                .unwrap_err(),
            OrgError::TargetOrgNotIssuer,
        );
    }

    #[test]
    fn structural_rule_enforced_at_decode_and_verify_both_directions() {
        // DISCOVER rights without a binding (crafted through the
        // raw pin surface — the public issue path cannot build it).
        let violating = OrgCapabilityGrant::issue_at(
            &org_b(),
            [9u8; 32],
            org_a().org_id(),
            cap(),
            GrantRights::DISCOVER,
            GrantTargetScope::ExactNode(provider()),
            None,
            1_000,
            2_000,
            7,
        );
        assert!(matches!(
            violating.verify(),
            Err(OrgError::DiscoveryBindingMismatch)
        ));
        assert!(matches!(
            OrgCapabilityGrant::from_bytes(&violating.to_bytes()),
            Err(OrgError::DiscoveryBindingMismatch)
        ));

        // A binding without DISCOVER rights.
        let (_, binding) = OrgAudienceSecret::mint([9u8; 32]);
        let violating = OrgCapabilityGrant::issue_at(
            &org_b(),
            [9u8; 32],
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            Some(binding),
            1_000,
            2_000,
            7,
        );
        assert!(matches!(
            violating.verify(),
            Err(OrgError::DiscoveryBindingMismatch)
        ));
        assert!(matches!(
            OrgCapabilityGrant::from_bytes(&violating.to_bytes()),
            Err(OrgError::DiscoveryBindingMismatch)
        ));
    }

    #[test]
    fn zero_grant_id_is_reserved_at_decode_and_verify() {
        let violating = OrgCapabilityGrant::issue_at(
            &org_b(),
            [0u8; 32],
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            None,
            1_000,
            2_000,
            7,
        );
        assert!(matches!(violating.verify(), Err(OrgError::ReservedGrantId)));
        assert!(matches!(
            OrgCapabilityGrant::from_bytes(&violating.to_bytes()),
            Err(OrgError::ReservedGrantId)
        ));
    }

    #[test]
    fn unknown_and_empty_rights_are_refused() {
        assert!(matches!(
            GrantRights::try_from_bits(0),
            Err(OrgError::EmptyRights)
        ));
        assert!(matches!(
            GrantRights::try_from_bits(0b100),
            Err(OrgError::UnknownRights)
        ));
        assert!(matches!(
            GrantRights::try_from_bits(0b111),
            Err(OrgError::UnknownRights)
        ));

        // On the wire: patch the rights field of a valid grant.
        let (grant, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("issue");
        let mut bytes = grant.to_bytes();
        bytes[128..132].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            OrgCapabilityGrant::from_bytes(&bytes),
            Err(OrgError::EmptyRights)
        ));
        bytes[128..132].copy_from_slice(&0b100u32.to_le_bytes());
        assert!(matches!(
            OrgCapabilityGrant::from_bytes(&bytes),
            Err(OrgError::UnknownRights)
        ));
    }

    #[test]
    fn grant_ttl_window_and_skew_discipline() {
        // Issue-side ceilings, both grant kinds.
        assert!(matches!(
            OrgDispatcherGrant::try_issue(&org_a(), dispatcher(), DispatcherScope::Any, 0),
            Err(OrgError::ZeroTtl)
        ));
        assert!(matches!(
            OrgDispatcherGrant::try_issue(
                &org_a(),
                dispatcher(),
                DispatcherScope::Any,
                MAX_ORG_GRANT_TTL_SECS + 1
            ),
            Err(OrgError::TtlTooLong)
        ));
        assert!(matches!(
            OrgCapabilityGrant::try_issue(
                &org_b(),
                org_a().org_id(),
                cap(),
                GrantRights::INVOKE,
                GrantTargetScope::ExactNode(provider()),
                0
            ),
            Err(OrgError::ZeroTtl)
        ));
        assert!(matches!(
            OrgCapabilityGrant::try_issue(
                &org_b(),
                org_a().org_id(),
                cap(),
                GrantRights::INVOKE,
                GrantTargetScope::ExactNode(provider()),
                MAX_ORG_GRANT_TTL_SECS + 1
            ),
            Err(OrgError::TtlTooLong)
        ));

        // Verify-side: reversed window; over-long window; each
        // crafted through the raw surface.
        let reversed = OrgDispatcherGrant::issue_at(
            &org_a(),
            dispatcher(),
            DispatcherScope::Any,
            2_000,
            1_000,
            7,
        );
        assert!(matches!(
            reversed.verify(),
            Err(OrgError::InvalidValidityWindow)
        ));
        let now = current_timestamp();
        let oversized = OrgDispatcherGrant::issue_at(
            &org_a(),
            dispatcher(),
            DispatcherScope::Any,
            now,
            now + MAX_ORG_GRANT_TTL_SECS + 10,
            7,
        );
        assert!(matches!(oversized.verify(), Err(OrgError::TtlTooLong)));

        // Skew ceiling enforced inside the check.
        let live =
            OrgDispatcherGrant::try_issue(&org_a(), dispatcher(), DispatcherScope::Any, 3600)
                .expect("issue");
        assert!(matches!(
            live.is_valid_with_skew(MAX_TOKEN_CLOCK_SKEW_SECS + 1),
            Err(OrgError::ClockSkewTooLarge)
        ));

        // Expired / not-yet-valid via explicit windows.
        let expired = OrgDispatcherGrant::issue_at(
            &org_a(),
            dispatcher(),
            DispatcherScope::Any,
            now.saturating_sub(2_000),
            now.saturating_sub(1_000),
            7,
        );
        assert!(matches!(
            expired.is_valid_with_skew(0),
            Err(OrgError::Expired)
        ));
        let future = OrgDispatcherGrant::issue_at(
            &org_a(),
            dispatcher(),
            DispatcherScope::Any,
            now + 10_000,
            now + 11_000,
            7,
        );
        assert!(matches!(
            future.is_valid_with_skew(0),
            Err(OrgError::NotYetValid)
        ));
    }

    #[test]
    fn target_scope_coverage_matrix() {
        let exact = GrantTargetScope::ExactNode(provider());
        assert!(exact.covers(&provider(), None));
        assert!(exact.covers(&provider(), Some(&org_b().org_id())));
        assert!(!exact.covers(&dispatcher(), Some(&org_b().org_id())));

        let owned = GrantTargetScope::AnyNodeOwnedBy(org_b().org_id());
        assert!(owned.covers(&provider(), Some(&org_b().org_id())));
        assert!(
            !owned.covers(&provider(), Some(&org_a().org_id())),
            "another org's node is not covered"
        );
        assert!(
            !owned.covers(&provider(), None),
            "an unowned node is nobody's node-owned-by"
        );
    }

    #[test]
    fn tampering_any_signed_field_fails_verification() {
        let (grant, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::DISCOVER.union(GrantRights::INVOKE),
            GrantTargetScope::AnyNodeOwnedBy(org_b().org_id()),
            3600,
        )
        .expect("issue");

        // Every signed region: flipping one byte must fail either
        // strict decode or signature verification — never pass.
        for offset in [0usize, 33, 65, 97, 134, 167, 199, 231, 239, 247] {
            let mut bytes = grant.to_bytes();
            bytes[offset] ^= 1;
            // A tampered byte either fails strict decode outright, or
            // decodes into a value whose signature no longer verifies
            // — never a value that passes verification.
            if let Ok(tampered) = OrgCapabilityGrant::from_bytes(&bytes) {
                assert!(
                    tampered.verify().is_err(),
                    "tamper at {offset} must not verify"
                );
            }
        }

        // Tampered signature bytes.
        let mut bytes = grant.to_bytes();
        bytes[300] ^= 1;
        let tampered = OrgCapabilityGrant::from_bytes(&bytes).expect("decodes");
        assert!(matches!(tampered.verify(), Err(OrgError::InvalidSignature)));

        // Wrong issuer: signature verifies only against issuer_org.
        let (foreign, _) = OrgCapabilityGrant::try_issue(
            &org_a(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            3600,
        )
        .expect("issue");
        let mut cross = foreign.clone();
        cross.issuer_org = org_b().org_id();
        assert!(cross.verify().is_err());
    }

    #[test]
    fn wire_length_is_strict() {
        let grant = OrgDispatcherGrant::try_issue(&org_a(), dispatcher(), DispatcherScope::Any, 60)
            .expect("issue");
        let bytes = grant.to_bytes();
        assert_eq!(bytes.len(), OrgDispatcherGrant::WIRE_SIZE);
        assert!(matches!(
            OrgDispatcherGrant::from_bytes(&bytes[..bytes.len() - 1]),
            Err(OrgError::InvalidFormat)
        ));
        let mut extended = bytes.clone();
        extended.push(0);
        assert!(matches!(
            OrgDispatcherGrant::from_bytes(&extended),
            Err(OrgError::InvalidFormat)
        ));

        let (grant, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider()),
            60,
        )
        .expect("issue");
        let bytes = grant.to_bytes();
        assert_eq!(bytes.len(), OrgCapabilityGrant::WIRE_SIZE);
        assert!(matches!(
            OrgCapabilityGrant::from_bytes(&bytes[..bytes.len() - 1]),
            Err(OrgError::InvalidFormat)
        ));
    }

    #[test]
    fn audience_secret_codec_roundtrip_and_redaction() {
        let (secret, _) = OrgAudienceSecret::mint([5u8; 32]);
        let encoded = secret.encode_config();
        let decoded = OrgAudienceSecret::decode_config(&encoded).expect("decode");
        assert_eq!(decoded.grant_id, secret.grant_id);
        assert_eq!(decoded.audience_handle, secret.audience_handle);
        assert_eq!(decoded.discovery_key(), secret.discovery_key());

        // Strictness: wrong length, wrong version.
        assert!(OrgAudienceSecret::decode_config(&encoded[..96]).is_err());
        let mut wrong_version = encoded;
        wrong_version[0] = 99;
        assert!(OrgAudienceSecret::decode_config(&wrong_version).is_err());

        // Debug NEVER prints the key.
        let debug = format!("{secret:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&hex::encode(secret.discovery_key())));
    }

    #[test]
    fn serde_rides_canonical_bytes_for_both_grants() {
        let dispatcher_grant = OrgDispatcherGrant::try_issue(
            &org_a(),
            dispatcher(),
            DispatcherScope::Exact(cap()),
            60,
        )
        .expect("issue");
        let json = serde_json::to_string(&dispatcher_grant).expect("json");
        assert_eq!(
            json,
            format!("\"{}\"", hex::encode(dispatcher_grant.to_bytes()))
        );
        let back: OrgDispatcherGrant = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, dispatcher_grant);

        let (grant, _) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::DISCOVER.union(GrantRights::INVOKE),
            GrantTargetScope::ExactNode(provider()),
            60,
        )
        .expect("issue");
        let json = serde_json::to_string(&grant).expect("json");
        let back: OrgCapabilityGrant = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, grant);
        // The postcard (non-human-readable) path round-trips too —
        // this is the form the §2.3 proof carries.
        let bytes = postcard::to_allocvec(&grant).expect("postcard");
        let back: OrgCapabilityGrant = postcard::from_bytes(&bytes).expect("postcard back");
        assert_eq!(back, grant);
    }

    /// Capture-once-pin-forever golden vectors (deterministic
    /// inputs through the raw pin surface). A byte change here is
    /// a wire-format break: bump the domain, never reinterpret.
    #[test]
    fn golden_vectors() {
        let dispatcher_grant = OrgDispatcherGrant::issue_at(
            &org_a(),
            dispatcher(),
            DispatcherScope::Exact(cap()),
            1_700_000_000,
            1_700_003_600,
            0x1122_3344_5566_7788,
        );
        assert_eq!(
            hex::encode(dispatcher_grant.to_bytes()),
            GOLDEN_DISPATCHER_GRANT_HEX,
            "OrgDispatcherGrant wire bytes drifted"
        );

        let binding = GrantedDiscoveryBinding {
            audience_handle: [0xAB; 32],
            key_commitment: audience_key_commitment(&[0xCD; 32]),
        };
        let capability_grant = OrgCapabilityGrant::issue_at(
            &org_b(),
            [0x11u8; 32],
            org_a().org_id(),
            cap(),
            GrantRights::DISCOVER.union(GrantRights::INVOKE),
            GrantTargetScope::AnyNodeOwnedBy(org_b().org_id()),
            Some(binding),
            1_700_000_000,
            1_700_003_600,
            0x8877_6655_4433_2211,
        );
        assert_eq!(
            hex::encode(capability_grant.to_bytes()),
            GOLDEN_CAPABILITY_GRANT_HEX,
            "OrgCapabilityGrant wire bytes drifted"
        );

        // The derive chain itself is pinned: context strings are
        // part of the wire contract.
        assert_eq!(
            hex::encode(CapabilityAuthorityId::for_tag("nrpc:oa2-echo").as_bytes()),
            GOLDEN_CAPABILITY_ID_HEX,
            "CapabilityAuthorityId derive drifted"
        );
        assert_eq!(
            hex::encode(audience_key_commitment(&[0xCD; 32])),
            GOLDEN_COMMITMENT_HEX,
            "audience key commitment derive drifted"
        );
    }

    const GOLDEN_DISPATCHER_GRANT_HEX: &str = "c853ad0f0cd2b619aea92ceec4fd56a24d6499d584ce79257e45cfd8139b60a7242424242424242424242424242424242424242424242424242424242424242401b7cf23907dfe3cad1152c9c5e14bec0bbdd0beeaafaff54ee27d5e5974788bab00f153650000000010ff5365000000008877665544332211ce5ee45cb913ab81f08b3013b3f5d5910558cbdb51febea077bd981f32956bef01ee5fbef4474599bf1e9112fda161ba2ce80660a2e8937878b8db144755d909";
    const GOLDEN_CAPABILITY_GRANT_HEX: &str = "11111111111111111111111111111111111111111111111111111111111111112152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12c853ad0f0cd2b619aea92ceec4fd56a24d6499d584ce79257e45cfd8139b60a7b7cf23907dfe3cad1152c9c5e14bec0bbdd0beeaafaff54ee27d5e5974788bab03000000022152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db1201abababababababababababababababababababababababababababababababab3338c46839907f71578cf4730fcf8eb0ec586ef8b496b453390fa38e38c33aa700f153650000000010ff53650000000011223344556677889afda595f45d1b61831dcec72c599da2aa168c2a5343c5872d3e650ab5bb70076d5e163081340d6f50eb0407278176a2685afc31e5bb3adc4f93f4e46ff80806";
    const GOLDEN_CAPABILITY_ID_HEX: &str =
        "b7cf23907dfe3cad1152c9c5e14bec0bbdd0beeaafaff54ee27d5e5974788bab";
    const GOLDEN_COMMITMENT_HEX: &str =
        "3338c46839907f71578cf4730fcf8eb0ec586ef8b496b453390fa38e38c33aa7";
}
