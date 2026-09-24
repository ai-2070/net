//! The grant family: dispatcher grants, capability grants and the
//! out-of-band audience secret (OA-2).
//!
//! Core's `behavior/org_grant.rs`, ported byte-for-byte — the 185-B
//! dispatcher grant and 318-B capability grant wire layouts, their
//! domain-prefixed transcripts (`net-org-dispatcher-grant-v1`,
//! `net-org-capability-grant-v1`) and every acceptance check are
//! frozen against core's golden vectors. Core's `getrandom` draws
//! become caller-supplied bytes (`grant_id`, `audience_random`,
//! `nonce`) and its `SystemTime` reads become `now_unix_secs`
//! parameters; core's `std::process::abort()` sites have no
//! counterpart here — nothing in a browser tab gets aborted.
//!
//! The structural rule that shapes this whole family: **`rights ⊇
//! DISCOVER ⇔ discovery binding present`**, enforced at issue AND
//! decode AND verify. Grants carry commitments; the raw audience
//! key ([`OrgAudienceSecret`]) never rides a wire, never appears in
//! `Debug`, and is structurally non-serializable.

use crate::identity::hex_lower;
use crate::org::cert::{OrgError, OrgId, OrgKeypair};
use crate::org::entity::EntityId;

/// blake3 `derive_key` context for [`CapabilityAuthorityId`] (plan
/// §2.1). A context string, not a signing domain: the id is a
/// deterministic public name, enumerable by anyone who knows the
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

/// Maximum grant validity window (issue AND verify — the same
/// dual-enforcement discipline as `MAX_ORG_CERT_TTL_SECS`). The plan
/// pins grant lifetimes at "days–weeks"; 30 days keeps "weeks"
/// honest while leaving operational slack.
pub const MAX_ORG_GRANT_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// The deterministic authorization-scope name of a capability:
/// `blake3::derive_key("net-org-capability-v1", canonical tag bytes)`
/// (plan §2.1).
///
/// Authorization scope ONLY — never a locator and never a secret:
/// anyone who knows a capability tag can compute its id, and the id
/// appears in grants precisely so authority can name the capability
/// without carrying the (possibly private) descriptor. Derived
/// (non-constant-time) `PartialEq` is deliberate, as [`OrgId`]'s.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CapabilityAuthorityId(pub [u8; 32]);

impl CapabilityAuthorityId {
    /// Derive the id for a canonical capability tag (the exact wire
    /// form, e.g. `nrpc:billing-reconcile`).
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

impl core::fmt::Debug for CapabilityAuthorityId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "CapabilityAuthorityId({}...)", hex_lower(&self.0[..8]))
    }
}

impl core::fmt::Display for CapabilityAuthorityId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}...", hex_lower(&self.0[..8]))
    }
}

/// Grant rights bitset: `DISCOVER` and `INVOKE` are independent
/// (plan §2.2). Unknown bits are refused at issue and decode — wire
/// evolution is honest, so an old verifier never silently masks away
/// a right it does not understand.
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

    /// Strict wire decode: empty and unknown bits are typed errors,
    /// never masked.
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

impl core::fmt::Debug for GrantRights {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
/// TOFU-authenticated cryptographic identity — deliberately NOT the
/// derived 64-bit `node_id`: an org-signed authority object must not
/// be satisfiable by a ~2^32-work grinding collision on the short
/// id.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GrantTargetScope {
    /// Exactly this provider entity.
    ExactNode(EntityId),
    /// Any node owned by this org — reusable across discovered
    /// owner-org providers.
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
            Self::ExactNode(exact) => *exact == *entity,
            Self::AnyNodeOwnedBy(org) => owner == Some(org),
        }
    }
}

/// The discovery half of a DISCOVER grant, INSIDE the signed bytes:
/// the audience routing handle plus the key COMMITMENT. The raw key
/// is never here (commitments in, keys out).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GrantedDiscoveryBinding {
    /// Per-grant audience routing handle. Public-ish; reveals
    /// nothing but linkage.
    pub audience_handle: [u8; 32],
    /// `blake3::derive_key("net-org-audience-commit-v1",
    /// discovery_key)` — lets a holder of the out-of-band key
    /// validate it against the signed grant without the key ever
    /// riding the wire.
    pub key_commitment: [u8; 32],
}

impl core::fmt::Debug for GrantedDiscoveryBinding {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GrantedDiscoveryBinding")
            .field("audience_handle", &hex_lower(&self.audience_handle[..8]))
            .field("key_commitment", &hex_lower(&self.key_commitment[..8]))
            .finish()
    }
}

/// The commitment for a raw discovery key.
pub fn audience_key_commitment(discovery_key: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key(AUDIENCE_COMMIT_CONTEXT, discovery_key)
}

/// The LOCAL, out-of-band half of a DISCOVER grant: the raw audience
/// decryption key, bound to its grant. Delivered out of band to the
/// publishing and consuming nodes.
///
/// NEVER on the wire, never in a proof, never in `Debug` output —
/// and structurally non-serializable: the compile-time assertion
/// below refuses a build in which this type gains a serde
/// `Serialize` impl, so it can never become a member of any wire
/// object.
pub struct OrgAudienceSecret {
    /// The grant this key belongs to.
    pub grant_id: [u8; 32],
    /// The audience routing handle (matches the signed binding).
    pub audience_handle: [u8; 32],
    /// The audience decryption key. SECRET.
    discovery_key: [u8; 32],
}

/// Type-level assertion: `OrgAudienceSecret` must never implement
/// `serde::Serialize`. If it ever does, the blanket impl below
/// becomes ambiguous with the `()` impl and this constant fails to
/// compile (the `static_assertions::assert_not_impl_any` mechanism,
/// inlined to avoid a dependency).
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
    /// format): `version ‖ grant_id ‖ handle ‖ key`.
    pub const ENCODED_SIZE: usize = 1 + 32 + 32 + 32;

    /// Mint audience material for `grant_id` from 64 caller-supplied
    /// random bytes: `rng_bytes[0..32]` becomes the handle,
    /// `rng_bytes[32..64]` the secret key, and the signed-side
    /// binding commits to them.
    ///
    /// Core draws the 64 bytes from `getrandom` and aborts on
    /// failure; here the HOST owns entropy and must supply fresh
    /// bytes (a predictable audience key lets anyone decrypt scoped
    /// announcements). The parameter is taken by value and scrubbed
    /// on exit; the caller's own copy is the caller's to scrub.
    pub fn mint(grant_id: [u8; 32], mut rng_bytes: [u8; 64]) -> (Self, GrantedDiscoveryBinding) {
        let mut audience_handle = [0u8; 32];
        let mut discovery_key = [0u8; 32];
        audience_handle.copy_from_slice(&rng_bytes[..32]);
        discovery_key.copy_from_slice(&rng_bytes[32..]);
        use zeroize::Zeroize;
        rng_bytes.zeroize();
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

    /// Validate this secret against a grant's SIGNED binding: handle
    /// equal AND `key_commitment` equal to this key's commitment. A
    /// mismatch means the out-of-band material does not belong to
    /// the grant — reject locally, before any use.
    pub fn matches_binding(&self, binding: &GrantedDiscoveryBinding) -> bool {
        self.audience_handle == binding.audience_handle
            && audience_key_commitment(&self.discovery_key) == binding.key_commitment
    }

    /// Whole-object match against a capability GRANT: the secret is
    /// the out-of-band key for THIS grant iff its `grant_id` matches
    /// AND the grant carries a discovery binding this secret
    /// satisfies. Prefer this over [`Self::matches_binding`] at call
    /// sites — a bare binding cannot express the `grant_id`.
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
    ///
    /// # Caller obligation: scrub the returned buffer
    ///
    /// The returned array carries the raw 32-byte discovery key and
    /// has NO `Drop` of its own. Whoever calls this must scrub it
    /// (e.g. [`zeroize::Zeroize::zeroize`]) once the bytes are
    /// written, on EVERY exit path including error returns.
    pub fn encode_config(&self) -> [u8; Self::ENCODED_SIZE] {
        let mut buf = [0u8; Self::ENCODED_SIZE];
        buf[0] = ORG_AUDIENCE_SECRET_VERSION;
        buf[1..33].copy_from_slice(&self.grant_id);
        buf[33..65].copy_from_slice(&self.audience_handle);
        buf[65..97].copy_from_slice(&self.discovery_key);
        buf
    }

    /// Strict inverse of [`Self::encode_config`]: exact length,
    /// known version byte, and a NON-ZERO `grant_id` (zero is the
    /// RESERVED owner-audience sentinel — see
    /// [`OrgError::ReservedGrantId`] — and this is the one public
    /// constructor of a secret-bearing type that takes a
    /// caller-chosen id).
    ///
    /// # Caller obligation: scrub the input buffer
    ///
    /// `bytes` is caller-owned and carries the raw discovery key —
    /// read it into something that scrubs on drop rather than a
    /// plain `Vec<u8>`.
    pub fn decode_config(bytes: &[u8]) -> Result<Self, OrgError> {
        if bytes.len() != Self::ENCODED_SIZE || bytes[0] != ORG_AUDIENCE_SECRET_VERSION {
            return Err(OrgError::InvalidFormat);
        }
        if bytes[1..33].iter().all(|b| *b == 0) {
            return Err(OrgError::InvalidFormat);
        }
        Ok(Self {
            grant_id: bytes[1..33].try_into().unwrap(),
            audience_handle: bytes[33..65].try_into().unwrap(),
            discovery_key: bytes[65..97].try_into().unwrap(),
        })
    }
}

impl core::fmt::Debug for OrgAudienceSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OrgAudienceSecret")
            .field("grant_id", &hex_lower(&self.grant_id[..8]))
            .field("audience_handle", &hex_lower(&self.audience_handle[..8]))
            .field("discovery_key", &"[REDACTED]")
            .finish()
    }
}

impl Drop for OrgAudienceSecret {
    fn drop(&mut self) {
        // Scrub the key on drop. `forbid(unsafe_code)` rules out the
        // volatile write core uses; `zeroize` performs one inside its
        // own crate (a volatile write plus a compiler fence).
        use zeroize::Zeroize;
        self.discovery_key.zeroize();
    }
}

/// A → S: "entity `dispatcher` may act FOR org `org_id`" over
/// `capability_scope`. Fixed one-hop and org-root-signed: there are
/// no delegation chains in v1, so verification is one signature
/// against the org root, never a chain walk.
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
/// Holding one is never invocation authority: admission verifies the
/// full per-call proof, and the provider's own policy is always
/// final.
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
    /// Per-issue nonce (caller-supplied randomness).
    pub nonce: u64,
    /// ed25519 signature over the domain-prefixed payload.
    pub signature: [u8; 64],
}

const DISPATCHER_SCOPE_TAG_EXACT: u8 = 0x01;
const DISPATCHER_SCOPE_TAG_ANY: u8 = 0x02;

impl OrgDispatcherGrant {
    /// Size of the signed payload (everything before the signature).
    const SIGNED_PAYLOAD_SIZE: usize = 32 + 32 + 1 + 32 + 8 + 8 + 8; // 121

    /// Size of the domain-prefixed signing input.
    const SIGNING_INPUT_SIZE: usize =
        ORG_DISPATCHER_GRANT_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Total serialized size.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64; // 185

    /// Issue a dispatcher grant valid from `now_unix_secs` for
    /// `duration_secs`. Rejects zero and over-ceiling TTLs with
    /// typed errors (same discipline as the membership cert).
    /// `nonce` is the caller's randomness (core draws it from
    /// `getrandom`).
    pub fn try_issue(
        org: &OrgKeypair,
        dispatcher: EntityId,
        capability_scope: DispatcherScope,
        duration_secs: u64,
        now_unix_secs: u64,
        nonce: u64,
    ) -> Result<Self, OrgError> {
        if duration_secs == 0 {
            return Err(OrgError::ZeroTtl);
        }
        if duration_secs > MAX_ORG_GRANT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        Ok(Self::issue_at(
            org,
            dispatcher,
            capability_scope,
            now_unix_secs,
            now_unix_secs.saturating_add(duration_secs),
            nonce,
        ))
    }

    /// Build and sign with fully explicit fields. `pub` as the
    /// deterministic golden-vector pin surface (core keeps it
    /// `pub(crate)`; the conformance tests live outside the crate).
    pub fn issue_at(
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
        grant.signature = org.sign(&grant.signing_input());
        grant
    }

    /// Canonical signed payload — fixed offsets, little-endian. The
    /// scope tag byte keeps the encoding injective even though `Any`
    /// zero-fills the capability field.
    fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
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

    /// Domain-prefixed signing input:
    /// `ORG_DISPATCHER_GRANT_SIG_DOMAIN ‖ signed_payload`.
    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..ORG_DISPATCHER_GRANT_SIG_DOMAIN.len()]
            .copy_from_slice(ORG_DISPATCHER_GRANT_SIG_DOMAIN);
        buf[ORG_DISPATCHER_GRANT_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
        buf
    }

    /// Verify structural validity and the signature: window shape,
    /// TTL ceiling (issue AND verify), then `verify_strict` against
    /// `org_id`. No wall-clock or floor checks — those are contextual
    /// ([`Self::is_valid_at_with_skew`]; floors apply to the
    /// membership cert, not grants, in v1).
    pub fn verify(&self) -> Result<(), OrgError> {
        if self.not_after <= self.not_before {
            return Err(OrgError::InvalidValidityWindow);
        }
        if self.not_after - self.not_before > MAX_ORG_GRANT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        self.org_id.verify(&self.signing_input(), &self.signature)
    }

    /// Signature + wall-clock validity against caller-supplied
    /// `now_secs` with `skew_secs` tolerance on both bounds
    /// (skew-ceiling-enforced, same as the cert).
    pub fn is_valid_at_with_skew(&self, now_secs: u64, skew_secs: u64) -> Result<(), OrgError> {
        if skew_secs > crate::org::MAX_TOKEN_CLOCK_SKEW_SECS {
            return Err(OrgError::ClockSkewTooLarge);
        }
        self.verify()?;
        check_time_bounds_at(self.not_before, self.not_after, now_secs, skew_secs)
    }

    /// Does this grant's scope cover `capability`?
    pub fn covers_capability(&self, capability: &CapabilityAuthorityId) -> bool {
        match &self.capability_scope {
            DispatcherScope::Exact(exact) => *exact == *capability,
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

impl core::fmt::Debug for OrgDispatcherGrant {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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

/// B → A: "org `grantee_org` holds `rights` on capability
/// `capability` over `target_scope`", signed by provider org
/// `issuer_org`. Cross-org access is ALWAYS this grant — never
/// co-membership (Locked #2).
///
/// Wire format (318 bytes):
/// ```text
/// grant_id:         32 (caller-supplied random; ZERO is RESERVED —
///                       the OA-3 owner-audience sentinel — refused)
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
/// nonce:             8 (u64, caller-supplied)
/// --- signed above (ORG_CAPABILITY_GRANT_SIG_DOMAIN prefixed) ---
/// signature:        64 (ed25519 by issuer_org)
/// ```
///
/// Structural rule, enforced at issue AND decode AND verify:
/// `rights ⊇ DISCOVER ⇔ discovery binding present`.
#[derive(Clone, PartialEq, Eq)]
pub struct OrgCapabilityGrant {
    /// Per-grant id; zero reserved.
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
    /// Present iff `rights ⊇ DISCOVER` — the audience handle and key
    /// commitment (the raw key is out of band, in
    /// [`OrgAudienceSecret`]).
    pub discovery: Option<GrantedDiscoveryBinding>,
    /// Valid from (unix seconds).
    pub not_before: u64,
    /// Valid until (unix seconds, exclusive).
    pub not_after: u64,
    /// Per-issue nonce (caller-supplied randomness).
    pub nonce: u64,
    /// ed25519 signature over the domain-prefixed payload.
    pub signature: [u8; 64],
}

const TARGET_TAG_EXACT_NODE: u8 = 0x01;
const TARGET_TAG_ANY_NODE_OWNED_BY: u8 = 0x02;
const DISCOVERY_TAG_ABSENT: u8 = 0x00;
const DISCOVERY_TAG_PRESENT: u8 = 0x01;

impl OrgCapabilityGrant {
    /// Size of the signed payload (everything before the signature).
    const SIGNED_PAYLOAD_SIZE: usize = 32 + 32 + 32 + 32 + 4 + 1 + 32 + 1 + 32 + 32 + 8 + 8 + 8; // 254

    /// Size of the domain-prefixed signing input.
    const SIGNING_INPUT_SIZE: usize =
        ORG_CAPABILITY_GRANT_SIG_DOMAIN.len() + Self::SIGNED_PAYLOAD_SIZE;

    /// Total serialized size.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64; // 318

    /// The target-scope owner rule: an `AnyNodeOwnedBy(org)` target
    /// must name the ISSUER's own org. A grant B→A over
    /// `AnyNodeOwnedBy(C != B)` names providers owned by a foreign
    /// org C and can NEVER admit (admission requires the provider's
    /// owner == issuer), so it is refused rather than minted as a
    /// permanently-unusable credential. `ExactNode` carries no org —
    /// its owner is checked only at admission. Enforced at issue AND
    /// decode/verify.
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

    /// Issue a capability grant valid from `now_unix_secs` for
    /// `duration_secs`.
    ///
    /// The structural rule holds BY CONSTRUCTION: when `rights ⊇
    /// DISCOVER`, `audience_random` MUST be `Some` — its 64 bytes
    /// mint the audience material (handle, key, commitment into the
    /// signed bytes) and the [`OrgAudienceSecret`] is returned
    /// alongside the grant for out-of-band delivery. `Some` without
    /// DISCOVER (or `None` with it) is
    /// [`OrgError::DiscoveryBindingMismatch`]: the randomness seam
    /// enforces the same rule the wire does. `grant_id` and `nonce`
    /// are the caller's randomness (core draws both from
    /// `getrandom`); `grant_id` may not be the reserved zero.
    #[allow(clippy::too_many_arguments)]
    pub fn try_issue(
        issuer: &OrgKeypair,
        grantee_org: OrgId,
        capability: CapabilityAuthorityId,
        rights: GrantRights,
        target_scope: GrantTargetScope,
        duration_secs: u64,
        grant_id: [u8; 32],
        audience_random: Option<[u8; 64]>,
        now_unix_secs: u64,
        nonce: u64,
    ) -> Result<(Self, Option<OrgAudienceSecret>), OrgError> {
        // Re-validate the bits even though `GrantRights` values are
        // constructed through the checked API — the bitset is `Copy`
        // and could arrive from a decode path.
        let rights = GrantRights::try_from_bits(rights.bits())?;
        if duration_secs == 0 {
            return Err(OrgError::ZeroTtl);
        }
        if duration_secs > MAX_ORG_GRANT_TTL_SECS {
            return Err(OrgError::TtlTooLong);
        }
        Self::check_target_owner(&issuer.org_id(), &target_scope)?;
        if grant_id == [0u8; 32] {
            return Err(OrgError::ReservedGrantId);
        }
        let (secret, binding) = match (rights.contains(GrantRights::DISCOVER), audience_random) {
            (true, Some(random)) => {
                let (secret, binding) = OrgAudienceSecret::mint(grant_id, random);
                (Some(secret), Some(binding))
            }
            (false, None) => (None, None),
            // The structural rule at the randomness seam: DISCOVER
            // needs audience material; a binding without DISCOVER
            // smuggles audience material into a grant that confers
            // no discovery.
            _ => return Err(OrgError::DiscoveryBindingMismatch),
        };
        let grant = Self::issue_at(
            issuer,
            grant_id,
            grantee_org,
            capability,
            rights,
            target_scope,
            binding,
            now_unix_secs,
            now_unix_secs.saturating_add(duration_secs),
            nonce,
        );
        Ok((grant, secret))
    }

    /// Build and sign with fully explicit fields — the raw pin
    /// surface for golden vectors and structural-rule witnesses.
    /// Does NOT enforce the issue-path invariants; [`Self::verify`]
    /// and [`Self::from_bytes`] do.
    #[allow(clippy::too_many_arguments)]
    pub fn issue_at(
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
        grant.signature = issuer.sign(&grant.signing_input());
        grant
    }

    /// Canonical signed payload — fixed offsets, little-endian.
    /// Presence tags keep the encoding injective across the
    /// zero-filled optional regions.
    fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
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

    /// Domain-prefixed signing input:
    /// `ORG_CAPABILITY_GRANT_SIG_DOMAIN ‖ signed_payload`.
    fn signing_input(&self) -> [u8; Self::SIGNING_INPUT_SIZE] {
        let mut buf = [0u8; Self::SIGNING_INPUT_SIZE];
        buf[..ORG_CAPABILITY_GRANT_SIG_DOMAIN.len()]
            .copy_from_slice(ORG_CAPABILITY_GRANT_SIG_DOMAIN);
        buf[ORG_CAPABILITY_GRANT_SIG_DOMAIN.len()..].copy_from_slice(&self.signed_payload());
        buf
    }

    /// Verify structural validity and the signature, in order:
    /// window shape → TTL ceiling → reserved grant_id → rights bits
    /// (empty/unknown) → the DISCOVER ⇔ binding structural rule →
    /// target-owner rule → `verify_strict` against `issuer_org`.
    /// Fields are public, so every invariant is re-checked here
    /// rather than trusted to the issue path.
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
        self.issuer_org
            .verify(&self.signing_input(), &self.signature)
    }

    /// Signature + wall-clock validity against caller-supplied
    /// `now_secs` with `skew_secs` tolerance on both bounds
    /// (skew-ceiling-enforced).
    pub fn is_valid_at_with_skew(&self, now_secs: u64, skew_secs: u64) -> Result<(), OrgError> {
        if skew_secs > crate::org::MAX_TOKEN_CLOCK_SKEW_SECS {
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
    /// rights bits; and the DISCOVER ⇔ binding structural rule — all
    /// BEFORE the caller ever sees a value (issue AND decode). The
    /// target-owner rule follows. Decoding does NOT verify the
    /// signature.
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

impl core::fmt::Debug for OrgCapabilityGrant {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OrgCapabilityGrant")
            .field("grant_id", &hex_lower(&self.grant_id[..8]))
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
            serializer.serialize_str(&hex_lower(&bytes))
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
            crate::identity::unhex(&hex_str).map_err(serde::de::Error::custom)?
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
            serializer.serialize_str(&hex_lower(&bytes))
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
            crate::identity::unhex(&hex_str).map_err(serde::de::Error::custom)?
        } else {
            <Vec<u8>>::deserialize(deserializer)?
        };
        Self::from_bytes(&bytes).map_err(serde::de::Error::custom)
    }
}

/// Wall-clock window check with skew — identical semantics to
/// `OrgMembershipCert::check_time_bounds_at` (saturating on both
/// bounds; inclusive-expiry convention: `now == not_after` is
/// expired).
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

// Wire sizes are load-bearing (the call proof rides a bounded RPC
// header): pin them at compile time.
const _: () = assert!(OrgDispatcherGrant::WIRE_SIZE == 185);
const _: () = assert!(OrgCapabilityGrant::WIRE_SIZE == 318);
