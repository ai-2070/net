//! Capability sensing identity (plan §3.1–§3.3, v4.1).
//!
//! Two semantic levels, one routed unit:
//!
//! - [`CapabilityInterestKey`] — what the consumer wants: the
//!   capability, constraints C, provider-evaluated latency L,
//!   provider selector, result mode, and disclosure/audience. NO
//!   provider, NO generation. Drives local dedup, candidate
//!   resolution, aggregate projection, and branch lifecycle.
//! - [`ProviderInterestKey`] — the routed coalescing unit (interest
//!   plus resolved provider). The ONLY key entering the Layer-2
//!   table; it routes via `next_hop(provider)`, so the aggregation
//!   tree exists.
//! - [`ProviderObservationKey`] — interest plus provider plus THAT
//!   provider's announce generation: what attestations, caches, and
//!   continuity key on.
//!
//! Two interests against the same capability (720p@30 vs 4K@60) are
//! two keys, two observations, two lifecycles: one going Unknown
//! never touches the other and never suspends the capability entry.
//! The identity is 256-bit and domain-separated — a 64-bit key is an
//! adversarial-collision hazard when it *merges* interests.
//!
//! Time dimensions (plan §3.3):
//!
//! | dimension | rule |
//! |---|---|
//! | `work_latency` (L) | provider-evaluated predicate dims — exact match, inside the digest |
//! | `consumer_budget` | consumer-local end-to-end acceptance — NOT identity, NOT wire |
//! | `requested_sample_interval` (D) | min-dominance upstream — deliberately NOT in the digest |
//! | `soft_state_ttl` | per-downstream subscription lifetime — says nothing about evidence |
//!
//! D is a delivery-continuity interval, **not** an evidence-age
//! bound; the v1/v2 name `max_observation_staleness` is retired and
//! must not reappear (the mechanism bounds local arrival continuity,
//! not cryptographic age since provider evaluation).

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use super::super::super::identity::EntityId;

/// Domain-separation context for [`InterestSpec::interest_digest`]
/// (plan §3.2). Fed to `blake3::Hasher::new_derive_key`, so digests
/// from this module can never collide with any other blake3 use in
/// the tree (dataforts CAS, meshos) or with a plain hash of the same
/// bytes.
pub const INTEREST_DIGEST_DOMAIN: &str = "net.sensing.interest.v1";

/// Domain-separation context for
/// [`CanonicalConstraints::constraints_digest`] — the digest an
/// interest carries beside its inline constraint bytes so the
/// provider can detect truncation/tampering (plan §4.2).
pub const CONSTRAINTS_DIGEST_DOMAIN: &str = "net.sensing.constraints.v1";

/// Inline canonical constraints are capped (plan §5,
/// `max_constraint_bytes`): readiness predicates should be compact,
/// and a predicate needing a large object is probably smuggling a
/// workload descriptor. Larger inputs are `InvalidConstraints`;
/// CAS-backed constraints are a named follow-up (plan §9).
pub const MAX_CONSTRAINT_BYTES: usize = 1024;

/// Serde for the 32-byte identity newtypes ([`Digest256`],
/// [`AudienceScopeCommitment`], [`GroupRef`]): a lowercase hex
/// string (64 chars), matching the `Debug` rendering — JSON-friendly
/// and unambiguous. This is the SI-0 SEMANTIC serialization used by
/// the in-process frame shapes (plan §4.2,
/// [`super::frames::SensingInterestFrame`]); SI-1 freezes the real
/// wire codec and may choose a binary form without breaking these
/// types.
macro_rules! impl_hex32_serde {
    ($ty:ty) => {
        impl serde::Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                if serializer.is_human_readable() {
                    serializer.serialize_str(&hex::encode(self.0))
                } else {
                    // Compact binary form for the wire codec
                    // (postcard: varint length + 32 raw bytes = 33 B
                    // instead of the 65 B hex string) — tightened
                    // before any deployment existed, so no wire
                    // migration was needed.
                    serializer.serialize_bytes(&self.0)
                }
            }
        }

        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Bytes32Visitor;
                impl<'de> serde::de::Visitor<'de> for Bytes32Visitor {
                    type Value = [u8; 32];
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("32 raw bytes")
                    }
                    fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<[u8; 32], E> {
                        v.try_into()
                            .map_err(|_| E::custom("expected exactly 32 bytes"))
                    }
                    fn visit_byte_buf<E: serde::de::Error>(
                        self,
                        v: Vec<u8>,
                    ) -> Result<[u8; 32], E> {
                        self.visit_bytes(&v)
                    }
                }
                if deserializer.is_human_readable() {
                    let text = <String as serde::Deserialize>::deserialize(deserializer)?;
                    let decoded = hex::decode(&text).map_err(serde::de::Error::custom)?;
                    let bytes: [u8; 32] = decoded.try_into().map_err(|_| {
                        serde::de::Error::custom(concat!(
                            stringify!($ty),
                            ": expected 32 bytes of hex (64 chars)"
                        ))
                    })?;
                    Ok(Self(bytes))
                } else {
                    deserializer.deserialize_bytes(Bytes32Visitor).map(Self)
                }
            }
        }
    };
}

/// A 256-bit sensing digest (blake3 output). Serializes as a hex
/// string (see the `impl_hex32_serde` note above).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest256([u8; 32]);

impl_hex32_serde!(Digest256);

impl Digest256 {
    /// Wrap raw digest bytes (e.g. off a decoded frame in SI-1).
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Digest256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest256({})", hex::encode(self.0))
    }
}

/// The capability a readiness interest targets — the fold's
/// capability name, canonicalized as its UTF-8 bytes. Observations
/// pair it with the answering provider's announce generation
/// ([`ProviderObservationKey::capability_generation`]) so no
/// observation ever straddles a capability redefinition.
/// Serializes as a plain string (SI-0 semantic form, plan §4.2).
#[derive(
    Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct CapabilityId(String);

impl CapabilityId {
    /// Wrap a capability name.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The capability name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who an interest's observations may be disclosed to (plan §4.9).
/// v1 is owner-root-only; the variant is in the digest from day one
/// so adding delegated classes later separates identities instead of
/// silently merging them.
///
/// Serde exists because the amended `ProviderRegistration` leg
/// carries the class for COMPLETE digest verification (plan §4.2,
/// review 7); identity always hashes [`Self::canonical_tag`], never
/// a serde encoding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum DisclosureClass {
    /// Disclosed only within the owner root's own infrastructure.
    Owner,
}

impl DisclosureClass {
    /// Canonical single-byte tag hashed into the interest digest
    /// (and, in SI-1, the wire form). Tags are append-only.
    pub const fn canonical_tag(self) -> u8 {
        match self {
            Self::Owner => 0,
        }
    }
}

/// The audience commitment bound into the interest digest (plan
/// §3.2): for v1, the canonical owner-root identity; for future
/// delegated groups, a hash of the scope/grant/audience-set
/// identity. Inside the digest so "interests belonging to different
/// audiences cannot coalesce" holds by construction, not by relay
/// diligence. Digest inclusion *separates semantic identities after
/// validation* — it never replaces the authenticated-session
/// identity check (plan §4.9). Serializes as a hex string (see the
/// `impl_hex32_serde` note on [`Digest256`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudienceScopeCommitment([u8; 32]);

impl_hex32_serde!(AudienceScopeCommitment);

impl AudienceScopeCommitment {
    /// v1 commitment: the owner root's entity identity.
    pub fn owner_root(root: &EntityId) -> Self {
        Self(*root.as_bytes())
    }

    /// Wrap raw commitment bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw commitment bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for AudienceScopeCommitment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AudienceScopeCommitment({})", hex::encode(self.0))
    }
}

/// The latency envelope L — the **provider-evaluated** dimensions of
/// the predicate (plan §3.3, review 5): things a provider can
/// honestly sign because they do not depend on any consumer's path.
/// Part of the predicate, so exact match, inside the digest. The
/// consumer's end-to-end budget is deliberately NOT here — see
/// [`ConsumerLatencyBudget`] (which correspondingly derives NO serde:
/// it must never ride the wire, structurally). Serde is the SI-0
/// semantic form (plan §4.2); identity always hashes
/// [`Self::canonical_bytes`], never a serde encoding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct WorkLatencyEnvelope {
    /// "I can *start* the work within this bound."
    pub provider_start_within: Option<Duration>,
    /// "First result/event within this bound after admission."
    pub first_event_after_admission: Option<Duration>,
}

impl WorkLatencyEnvelope {
    /// Envelope with only a start bound (the common case).
    pub const fn start_within(bound: Duration) -> Self {
        Self {
            provider_start_within: Some(bound),
            first_event_after_admission: None,
        }
    }

    /// Fixed-width injective canonical encoding — per dimension one
    /// presence byte + u128 LE nanoseconds (zeros when absent).
    pub fn canonical_bytes(&self) -> [u8; 34] {
        let mut out = [0u8; 34];
        for (slot, dim) in [self.provider_start_within, self.first_event_after_admission]
            .into_iter()
            .enumerate()
        {
            let base = slot * 17;
            if let Some(bound) = dim {
                out[base] = 1;
                out[base + 1..base + 17].copy_from_slice(&bound.as_nanos().to_le_bytes());
            }
        }
        out
    }
}

/// The consumer's end-to-end acceptance bound (plan §3.3, review 5).
/// **Local by definition** — never in the digest, never on the wire,
/// never provider-signed: a provider cannot know this consumer's
/// path cost, so `end_to_end_within` is checked at the consumer as
/// `route_estimate + provider estimated_start ≤ budget`. Two
/// consumers may legitimately derive different viability from the
/// same signed attestation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct ConsumerLatencyBudget {
    /// End-to-end bound from this consumer's perspective; `None`
    /// accepts any path.
    pub end_to_end_within: Option<Duration>,
}

impl ConsumerLatencyBudget {
    /// The local viability check: does a provider proof carrying
    /// `estimated_start` satisfy this budget over a path currently
    /// estimated at `route_estimate`? A missing provider estimate
    /// counts as "can start now" (the proof still had to project
    /// Ready through continuity to get here).
    pub fn admits(&self, route_estimate: Duration, estimated_start: Option<Duration>) -> bool {
        match self.end_to_end_within {
            None => true,
            Some(budget) => {
                route_estimate.saturating_add(estimated_start.unwrap_or(Duration::ZERO)) <= budget
            }
        }
    }
}

/// Why constraint bytes were rejected. Every variant maps onto the
/// evaluator's `InvalidConstraints` projection (plan §4.4); only
/// [`ConstraintError::DigestMismatch`] is additionally classified as
/// **protocol-invalid/security** — the sender claimed a digest its
/// own bytes don't hash to, which is malformed or tampered protocol
/// input, not merely an unevaluable predicate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConstraintError {
    /// Canonical form exceeds [`MAX_CONSTRAINT_BYTES`].
    Oversize {
        /// The offending encoded length.
        len: usize,
    },
    /// Byte stream ended mid-field.
    Truncated,
    /// Bytes remained after the declared entry count was decoded.
    TrailingBytes,
    /// Keys not in strictly ascending order — the input is not the
    /// canonical form (order-normalizing on receive would let two
    /// different byte strings claim one identity).
    NonCanonicalOrder,
    /// The same key appeared twice.
    DuplicateKey,
    /// A key or value was not valid UTF-8.
    InvalidUtf8,
    /// The inline bytes do not hash to the digest the interest
    /// carried beside them.
    DigestMismatch,
}

impl ConstraintError {
    /// Whether this rejection must increment the
    /// protocol-invalid/security counter rather than only the
    /// invalid-constraints counter (plan §4.4).
    pub const fn is_security_relevant(self) -> bool {
        matches!(self, Self::DigestMismatch)
    }
}

impl fmt::Display for ConstraintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversize { len } => {
                write!(
                    f,
                    "canonical constraints {len} B > {MAX_CONSTRAINT_BYTES} B cap"
                )
            }
            Self::Truncated => f.write_str("constraint bytes truncated mid-field"),
            Self::TrailingBytes => f.write_str("trailing bytes after final constraint entry"),
            Self::NonCanonicalOrder => f.write_str("constraint keys not strictly ascending"),
            Self::DuplicateKey => f.write_str("duplicate constraint key"),
            Self::InvalidUtf8 => f.write_str("constraint key/value not valid UTF-8"),
            Self::DigestMismatch => {
                f.write_str("inline constraint bytes do not match the claimed digest")
            }
        }
    }
}

impl std::error::Error for ConstraintError {}

/// Work characteristics C in canonical form: sorted, de-duplicated
/// key→value pairs with an injective length-prefixed encoding.
/// Exact-digest match only — no implication or subsumption between
/// constraint sets (plan §9).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CanonicalConstraints {
    entries: BTreeMap<String, String>,
}

impl CanonicalConstraints {
    /// Build from key→value pairs. Rejects duplicate keys (two
    /// values for one key has no canonical order) and canonical
    /// forms over [`MAX_CONSTRAINT_BYTES`].
    pub fn from_entries<I, K, V>(entries: I) -> Result<Self, ConstraintError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            if map.insert(k.into(), v.into()).is_some() {
                return Err(ConstraintError::DuplicateKey);
            }
        }
        let built = Self { entries: map };
        let len = built.canonical_bytes().len();
        if len > MAX_CONSTRAINT_BYTES {
            return Err(ConstraintError::Oversize { len });
        }
        Ok(built)
    }

    /// The canonical byte form: `u32 LE` entry count, then per entry
    /// (keys strictly ascending) `u32 LE` length + bytes for the key
    /// and again for the value. Length prefixes make the encoding
    /// injective — `{"ab": "c"}` and `{"a": "bc"}` can never encode
    /// to the same bytes, so they can never hash to one identity.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.entries.len() * 16);
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (k, v) in &self.entries {
            for part in [k.as_str(), v.as_str()] {
                out.extend_from_slice(&(part.len() as u32).to_le_bytes());
                out.extend_from_slice(part.as_bytes());
            }
        }
        out
    }

    /// Strict inverse of [`Self::canonical_bytes`]: the input must
    /// BE the canonical form. Unsorted or duplicate keys, truncation,
    /// and trailing bytes are all rejected rather than normalized —
    /// re-canonicalizing on receive would let two different byte
    /// strings claim one interest identity.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ConstraintError> {
        if bytes.len() > MAX_CONSTRAINT_BYTES {
            return Err(ConstraintError::Oversize { len: bytes.len() });
        }
        let mut cursor = bytes;
        let count = read_u32(&mut cursor)?;
        let mut entries = BTreeMap::new();
        let mut last_key: Option<String> = None;
        for _ in 0..count {
            let key = read_string(&mut cursor)?;
            let value = read_string(&mut cursor)?;
            if let Some(prev) = &last_key {
                match key.cmp(prev) {
                    std::cmp::Ordering::Equal => return Err(ConstraintError::DuplicateKey),
                    std::cmp::Ordering::Less => return Err(ConstraintError::NonCanonicalOrder),
                    std::cmp::Ordering::Greater => {}
                }
            }
            last_key = Some(key.clone());
            entries.insert(key, value);
        }
        if !cursor.is_empty() {
            return Err(ConstraintError::TrailingBytes);
        }
        Ok(Self { entries })
    }

    /// Parse inline constraint bytes AND validate them against the
    /// digest the interest carried beside them (plan §4.2). A
    /// mismatch is [`ConstraintError::DigestMismatch`] — security
    /// telemetry, not merely an unevaluable predicate.
    pub fn validate_inline(bytes: &[u8], claimed: &Digest256) -> Result<Self, ConstraintError> {
        let parsed = Self::parse_canonical(bytes)?;
        if parsed.constraints_digest() != *claimed {
            return Err(ConstraintError::DigestMismatch);
        }
        Ok(parsed)
    }

    /// Domain-separated digest of the canonical byte form — the
    /// `constraints_digest` an interest carries beside the inline
    /// bytes.
    pub fn constraints_digest(&self) -> Digest256 {
        let mut hasher = blake3::Hasher::new_derive_key(CONSTRAINTS_DIGEST_DOMAIN);
        hasher.update(&self.canonical_bytes());
        Digest256(*hasher.finalize().as_bytes())
    }

    /// Look up one constraint value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// Number of constraint entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no constraint entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn read_u32(cursor: &mut &[u8]) -> Result<u32, ConstraintError> {
    let (head, rest) = cursor
        .split_first_chunk::<4>()
        .ok_or(ConstraintError::Truncated)?;
    *cursor = rest;
    Ok(u32::from_le_bytes(*head))
}

fn read_string(cursor: &mut &[u8]) -> Result<String, ConstraintError> {
    let len = read_u32(cursor)? as usize;
    if cursor.len() < len {
        return Err(ConstraintError::Truncated);
    }
    let (bytes, rest) = cursor.split_at(len);
    *cursor = rest;
    String::from_utf8(bytes.to_vec()).map_err(|_| ConstraintError::InvalidUtf8)
}

/// A stable, owner-scoped group identity commitment (plan §4.10).
/// Interests address the group identity, never a copied member
/// list; local folds materialize membership. The commitment is an
/// opaque 32-byte identity — the fold's group machinery resolves it;
/// it is never a bearer secret. Serializes as a hex string (see the
/// `impl_hex32_serde` note on [`Digest256`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupRef([u8; 32]);

impl_hex32_serde!(GroupRef);

impl GroupRef {
    /// Wrap a group identity commitment.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw commitment bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for GroupRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GroupRef({})", hex::encode(self.0))
    }
}

/// One exact-match tag requirement inside a `Tags` selector. v1 is
/// exact conjunction only — no Boolean algebra on the wire (plan
/// §9). Whether a matching *assertion* is acceptable is a
/// provenance question answered at candidate resolution (§4.10),
/// not part of the match syntax.
#[derive(
    Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct TagMatch {
    /// Tag key.
    pub key: String,
    /// Required exact value.
    pub value: String,
}

/// The provider population an interest may be satisfied from (plan
/// §3.1). `AnyAuthorized` is the default — the provider is an
/// answer, not part of the question; the other variants are the
/// operator's explicit-surveillance overrides. The selector is
/// **inside the interest digest**: "any printer" must never coalesce
/// with "printer-7 only". Serde is the SI-0 semantic form (plan
/// §4.2); identity always hashes [`Self::canonical_bytes`], never a
/// serde encoding.
///
/// `PartialEq`/`Eq`/`Hash` are defined over [`Self::canonical_bytes`]
/// — NOT derived structurally — so equality and hashing agree with
/// the interest-digest identity: two selectors that differ only in
/// `Nodes`/`Tags` authorship order (e.g. `Nodes([7, 9])` vs
/// `Nodes([9, 1])` reordered) are digest-identical and must compare
/// and hash identically, or a caller keying/deduping on the selector
/// (rather than the digest) would split one interest into two.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ProviderSelector {
    /// Any provider the authority scope admits.
    AnyAuthorized,
    /// Exactly this node — the v3 provider-targeted case.
    Node(u64),
    /// An explicit node set (canonical: sorted, deduplicated — use
    /// [`ProviderSelector::nodes`]).
    Nodes(Vec<u64>),
    /// An owner-scoped group identity.
    Group(GroupRef),
    /// All-of tag conjunction (canonical: sorted, deduplicated —
    /// use [`ProviderSelector::tags`]).
    Tags(Vec<TagMatch>),
}

impl ProviderSelector {
    /// Whether this selector names no explicit destination and so
    /// routes through the scope-local sensing leader (plan §4.1
    /// two routing stages). `AnyAuthorized`/`Group`/`Tags` are
    /// provider-free; `Node`/`Nodes` carry explicit destinations and
    /// skip the rendezvous. The SI-7 merge-miss metric scopes to the
    /// provider-free set, where a second distinct upstream at one
    /// provider means two leaders resolved the same interest.
    pub fn is_provider_free(&self) -> bool {
        matches!(self, Self::AnyAuthorized | Self::Group(_) | Self::Tags(_))
    }

    /// Canonical `Nodes` selector: sorted and deduplicated, so
    /// authorship order can never split interest identity.
    pub fn nodes(mut ids: Vec<u64>) -> Self {
        ids.sort_unstable();
        ids.dedup();
        Self::Nodes(ids)
    }

    /// Canonical `Tags` selector: sorted by (key, value) and
    /// deduplicated.
    pub fn tags(mut matches: Vec<TagMatch>) -> Self {
        matches.sort();
        matches.dedup();
        Self::Tags(matches)
    }

    /// Injective canonical encoding hashed into the interest digest:
    /// a variant tag, then length-prefixed fields. `Nodes`/`Tags`
    /// are re-canonicalized defensively (sorting is idempotent).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::AnyAuthorized => out.push(0u8),
            Self::Node(id) => {
                out.push(1);
                out.extend_from_slice(&id.to_le_bytes());
            }
            Self::Nodes(ids) => {
                out.push(2);
                let mut ids = ids.clone();
                ids.sort_unstable();
                ids.dedup();
                out.extend_from_slice(&(ids.len() as u32).to_le_bytes());
                for id in ids {
                    out.extend_from_slice(&id.to_le_bytes());
                }
            }
            Self::Group(group) => {
                out.push(3);
                out.extend_from_slice(group.as_bytes());
            }
            Self::Tags(matches) => {
                out.push(4);
                let mut matches = matches.clone();
                matches.sort();
                matches.dedup();
                out.extend_from_slice(&(matches.len() as u32).to_le_bytes());
                for tag in matches {
                    for part in [tag.key.as_str(), tag.value.as_str()] {
                        out.extend_from_slice(&(part.len() as u32).to_le_bytes());
                        out.extend_from_slice(part.as_bytes());
                    }
                }
            }
        }
        out
    }
}

/// Canonical equality: two selectors are equal iff their
/// [`ProviderSelector::canonical_bytes`] match, so `Nodes`/`Tags`
/// authorship order never splits interest identity (the derived
/// structural `Eq` compared the raw `Vec`s and disagreed with the
/// digest).
impl PartialEq for ProviderSelector {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_bytes() == other.canonical_bytes()
    }
}

impl Eq for ProviderSelector {}

/// Hashes the canonical encoding, so `Hash` is consistent with the
/// canonical `Eq` above (equal selectors hash equal).
impl std::hash::Hash for ProviderSelector {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.canonical_bytes().hash(state);
    }
}

/// How much of the selected provider population must be observed
/// (plan §3.1). Separate from the selector because a population
/// alone is ambiguous ("any camera usable?" vs "observe each
/// camera"). Inside the digest: "any member of G" must never
/// coalesce with "each member of G". Serde is the SI-0 semantic form
/// (plan §4.2); identity always hashes [`Self::canonical_bytes`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum ResultMode {
    /// One viable provider satisfies the interest (the default).
    Any,
    /// Maintain up to K ranked viable providers.
    TopK(u16),
    /// Provider-indexed observation per member — explicit
    /// surveillance, guard-railed (plan §4.7).
    Each,
    /// At least K viable providers.
    Quorum(u16),
}

impl ResultMode {
    /// Fixed-width canonical encoding: variant tag + u16 parameter
    /// (zero where unused).
    pub const fn canonical_bytes(&self) -> [u8; 3] {
        match self {
            Self::Any => [0, 0, 0],
            Self::TopK(k) => {
                let b = k.to_le_bytes();
                [1, b[0], b[1]]
            }
            Self::Each => [2, 0, 0],
            Self::Quorum(k) => {
                let b = k.to_le_bytes();
                [3, b[0], b[1]]
            }
        }
    }
}

/// The digest-bearing interest: everything that defines the
/// **capability-interest identity** (plan §3.2) — and nothing that
/// doesn't. `requested_sample_interval` is absent (min-dominance);
/// **provider identity and capability generation are absent** — a
/// capability-directed interest cannot bind one provider's
/// generation; generation binding lives one level down, in
/// [`ProviderObservationKey`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InterestSpec {
    /// Capability the predicate targets.
    pub capability_id: CapabilityId,
    /// Work characteristics C.
    pub constraints: CanonicalConstraints,
    /// Latency envelope L.
    pub work_latency: WorkLatencyEnvelope,
    /// The provider population (plan §3.1).
    pub providers: ProviderSelector,
    /// The required cardinality over that population.
    pub result_mode: ResultMode,
    /// Disclosure class (v1: owner).
    pub disclosure_class: DisclosureClass,
    /// Audience commitment (v1: canonical owner-root identity).
    pub audience: AudienceScopeCommitment,
}

impl InterestSpec {
    /// The canonical interest identity (plan §3.2):
    ///
    /// ```text
    /// interest_digest = blake3_derive_key("net.sensing.interest.v1",
    ///     len(capability_id) || capability_id ||
    ///     len(canonical(C)) || canonical(C) ||
    ///     canonical(L) ||
    ///     len(canonical(selector)) || canonical(selector) ||
    ///     canonical(result_mode) ||
    ///     disclosure_class_tag || audience_commitment)
    /// ```
    ///
    /// Variable-length fields are length-prefixed and the remaining
    /// fields are fixed-width, so the pre-image is injective: no two
    /// distinct specs feed the hasher the same byte stream.
    pub fn interest_digest(&self) -> Digest256 {
        let mut hasher = blake3::Hasher::new_derive_key(INTEREST_DIGEST_DOMAIN);
        let id_bytes = self.capability_id.as_str().as_bytes();
        hasher.update(&(id_bytes.len() as u64).to_le_bytes());
        hasher.update(id_bytes);
        let constraint_bytes = self.constraints.canonical_bytes();
        hasher.update(&(constraint_bytes.len() as u64).to_le_bytes());
        hasher.update(&constraint_bytes);
        hasher.update(&self.work_latency.canonical_bytes());
        let selector_bytes = self.providers.canonical_bytes();
        hasher.update(&(selector_bytes.len() as u64).to_le_bytes());
        hasher.update(&selector_bytes);
        hasher.update(&self.result_mode.canonical_bytes());
        hasher.update(&[self.disclosure_class.canonical_tag()]);
        hasher.update(self.audience.as_bytes());
        Digest256(*hasher.finalize().as_bytes())
    }

    /// Key this spec (convenience over
    /// [`CapabilityInterestKey::for_spec`]).
    pub fn key(&self) -> CapabilityInterestKey {
        CapabilityInterestKey::for_spec(self)
    }
}

/// The capability-interest identity (plan §3.2) — what the consumer
/// wants, free of any provider identity. Coalescing, the per-hop
/// interest table, and the fold overlay key on this.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CapabilityInterestKey {
    /// Capability the interest targets (indexing convenience; also
    /// bound inside the digest).
    pub capability_id: CapabilityId,
    /// The 256-bit interest identity
    /// ([`InterestSpec::interest_digest`]).
    pub interest_digest: Digest256,
}

impl CapabilityInterestKey {
    /// Key a spec.
    pub fn for_spec(spec: &InterestSpec) -> Self {
        Self {
            capability_id: spec.capability_id.clone(),
            interest_digest: spec.interest_digest(),
        }
    }
}

/// The routed coalescing unit (plan §3.2, review 5): a
/// provider-targeted interest. This is the ONLY key that enters the
/// Layer-2 per-hop table — it routes via `next_hop(provider)`, so
/// the aggregation tree exists (its root is the provider), and
/// interests from consumers that resolved to the same provider merge
/// at fan-in exactly as in v3. The capability-interest key above
/// drives local dedup, resolution, aggregation, and branch
/// lifecycle; it never travels provider-free.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProviderInterestKey {
    /// The capability interest this branch serves.
    pub interest: CapabilityInterestKey,
    /// The resolved provider this branch targets.
    pub provider: u64,
}

impl ProviderInterestKey {
    /// Key a resolved branch.
    pub fn new(interest: CapabilityInterestKey, provider: u64) -> Self {
        Self { interest, provider }
    }
}

/// The provider-observation identity (plan §3.2) — which provider,
/// under which of ITS announce generations, currently answers the
/// interest. Attestations, observation cells, relay caches, and
/// per-provider continuity key on this; a provider generation change
/// mints a new observation key while the interest above survives
/// untouched.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProviderObservationKey {
    /// The capability interest being answered.
    pub interest: CapabilityInterestKey,
    /// The answering provider.
    pub provider: u64,
    /// That provider's announce version.
    pub capability_generation: u64,
}

impl ProviderObservationKey {
    /// Key a provider's answer to an interest.
    pub fn new(interest: CapabilityInterestKey, provider: u64, capability_generation: u64) -> Self {
        Self {
            interest,
            provider,
            capability_generation,
        }
    }
}

/// A consumer's registration: the predicate identity plus the
/// non-identity, consumer-local dimensions (plan §3.3).
#[derive(Clone, Debug)]
pub struct InterestRegistration {
    /// The digest-bearing predicate identity.
    pub spec: InterestSpec,
    /// D — how often the consumer wants evidence sampled/delivered.
    /// A delivery-continuity interval, NOT an evidence-age bound;
    /// aggregates upstream by min-dominance
    /// ([`strictest_sample_interval`]).
    pub requested_sample_interval: Duration,
    /// Subscription lifetime — per-downstream soft-state expiry;
    /// says nothing about evidence.
    pub soft_state_ttl: Duration,
    /// This consumer's end-to-end acceptance bound — local viability
    /// only, never identity, never wire (§3.3).
    pub consumer_budget: ConsumerLatencyBudget,
}

/// Min-dominance aggregation for D (plan §3.3): the strictest
/// requested interval satisfies every looser one, which is exactly
/// why D is not part of the interest identity. `None` when there are
/// no live downstream intervals (no upstream interest to register).
pub fn strictest_sample_interval<I>(intervals: I) -> Option<Duration>
where
    I: IntoIterator<Item = Duration>,
{
    intervals.into_iter().min()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audience(byte: u8) -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([byte; 32])
    }

    fn spec() -> InterestSpec {
        InterestSpec {
            capability_id: CapabilityId::new("video.transcode"),
            constraints: CanonicalConstraints::from_entries([
                ("fps", "60"),
                ("resolution", "3840x2160"),
            ])
            .unwrap(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_millis(250)),
            providers: ProviderSelector::AnyAuthorized,
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience: audience(0xAA),
        }
    }

    #[test]
    fn digest_is_deterministic() {
        assert_eq!(spec().interest_digest(), spec().interest_digest());
    }

    /// The two digest domains are distinct blake3 derive-keys, so the
    /// interest digest and the constraints digest live in separate
    /// hash spaces — a constraints digest can never be mistaken for
    /// (or collide with) an interest digest. Both are `pub` and
    /// re-exported through `super::super` for symmetry.
    #[test]
    fn digest_domains_are_separated() {
        assert_ne!(INTEREST_DIGEST_DOMAIN, CONSTRAINTS_DIGEST_DOMAIN);
        // Reachable through the umbrella module path, not only the
        // identity submodule.
        assert_eq!(
            super::super::CONSTRAINTS_DIGEST_DOMAIN,
            CONSTRAINTS_DIGEST_DOMAIN,
        );
    }

    /// SI-7: the merge-miss metric scopes to provider-free selectors
    /// (routed through the leader). `Node`/`Nodes` name explicit
    /// destinations and skip the rendezvous, so multiple direct
    /// surveillants of one provider are intended, not a miss.
    #[test]
    fn provider_free_discriminates_leader_routed_from_direct() {
        assert!(ProviderSelector::AnyAuthorized.is_provider_free());
        assert!(ProviderSelector::Group(GroupRef::from_bytes([7; 32])).is_provider_free());
        assert!(ProviderSelector::tags(vec![TagMatch {
            key: "gpu".into(),
            value: "h100".into(),
        }])
        .is_provider_free());
        assert!(!ProviderSelector::Node(7).is_provider_free());
        assert!(!ProviderSelector::nodes(vec![7, 9]).is_provider_free());
    }

    #[test]
    fn consumer_local_dimensions_never_split_identity() {
        // §3.3: D is min-dominated and the end-to-end budget is
        // local viability — neither is identity-bearing. Two
        // registrations differing ONLY in D, ttl, and budget must
        // key to the same interest: the tripwire against either
        // migrating into the digest.
        let strict = InterestRegistration {
            spec: spec(),
            requested_sample_interval: Duration::from_millis(50),
            soft_state_ttl: Duration::from_secs(30),
            consumer_budget: ConsumerLatencyBudget {
                end_to_end_within: Some(Duration::from_millis(400)),
            },
        };
        let loose = InterestRegistration {
            spec: spec(),
            requested_sample_interval: Duration::from_secs(5),
            soft_state_ttl: Duration::from_secs(300),
            consumer_budget: ConsumerLatencyBudget::default(),
        };
        assert_eq!(strict.spec.key(), loose.spec.key());
    }

    #[test]
    fn budget_admission_is_route_relative() {
        // Review 5: two consumers, the SAME provider proof
        // (estimated_start = 300 ms), different route estimates —
        // different viability. The aggregate is local by definition.
        let budget = ConsumerLatencyBudget {
            end_to_end_within: Some(Duration::from_millis(500)),
        };
        let start = Some(Duration::from_millis(300));
        assert!(budget.admits(Duration::from_millis(150), start));
        assert!(!budget.admits(Duration::from_millis(250), start));
        // No budget accepts any path; a missing provider estimate
        // counts as "can start now".
        assert!(ConsumerLatencyBudget::default().admits(Duration::from_secs(10), start));
        assert!(budget.admits(Duration::from_millis(500), None));
    }

    #[test]
    fn capability_generation_never_splits_interest_identity() {
        // v4 §3.2: a capability-directed interest cannot bind one
        // provider's generation — different printers have different
        // generations. Generation lives in the OBSERVATION key: the
        // same interest, answered by one provider across a
        // generation bump, is one interest but two observations.
        let key = spec().key();
        let gen10 = ProviderObservationKey::new(key.clone(), 7, 10);
        let gen11 = ProviderObservationKey::new(key.clone(), 7, 11);
        assert_eq!(gen10.interest, gen11.interest);
        assert_ne!(gen10, gen11);
        // And two providers answering one interest are two
        // observations under one interest.
        let other = ProviderObservationKey::new(key, 8, 10);
        assert_eq!(gen10.interest, other.interest);
        assert_ne!(gen10, other);
    }

    #[test]
    fn provider_selector_is_identity_bearing() {
        // §3.2: "any printer" must never coalesce with "printer-7
        // only", nor with a different node's surveillance.
        let mut node7 = spec();
        node7.providers = ProviderSelector::Node(7);
        let mut node8 = spec();
        node8.providers = ProviderSelector::Node(8);
        assert_ne!(spec().interest_digest(), node7.interest_digest());
        assert_ne!(node7.interest_digest(), node8.interest_digest());
    }

    #[test]
    fn result_mode_is_identity_bearing() {
        // §3.2: "any member of G" must never coalesce with "each
        // member of G".
        let mut each = spec();
        each.result_mode = ResultMode::Each;
        let mut quorum = spec();
        quorum.result_mode = ResultMode::Quorum(2);
        assert_ne!(spec().interest_digest(), each.interest_digest());
        assert_ne!(each.interest_digest(), quorum.interest_digest());
        assert_ne!(
            ResultMode::TopK(2).canonical_bytes(),
            ResultMode::Quorum(2).canonical_bytes(),
        );
    }

    #[test]
    fn selectors_canonicalize_order_and_duplicates_away() {
        // Authorship order must not split identity.
        let ab = ProviderSelector::nodes(vec![9, 3, 3, 7]);
        let ba = ProviderSelector::nodes(vec![7, 9, 3]);
        assert_eq!(ab.canonical_bytes(), ba.canonical_bytes());

        let tag = |k: &str, v: &str| TagMatch {
            key: k.into(),
            value: v.into(),
        };
        let t1 = ProviderSelector::tags(vec![
            tag("site", "factory-7"),
            tag("modality", "thermal"),
            tag("site", "factory-7"),
        ]);
        let t2 = ProviderSelector::tags(vec![tag("modality", "thermal"), tag("site", "factory-7")]);
        assert_eq!(t1.canonical_bytes(), t2.canonical_bytes());

        // But different populations are different identities.
        let t3 = ProviderSelector::tags(vec![tag("modality", "rgb"), tag("site", "factory-7")]);
        assert_ne!(t1.canonical_bytes(), t3.canonical_bytes());

        let mut spec_t1 = spec();
        spec_t1.providers = t1;
        let mut spec_t3 = spec();
        spec_t3.providers = t3;
        assert_ne!(spec_t1.interest_digest(), spec_t3.interest_digest());
    }

    #[test]
    fn selector_eq_and_hash_track_canonical_identity_not_authorship_order() {
        // 2026-07-15 review §8: Eq/Hash must agree with
        // canonical_bytes()/interest_digest(), so RAW-variant selectors
        // (built WITHOUT the canonicalizing constructors) that differ
        // only in authorship order still compare and hash equal —
        // otherwise a caller keying/deduping on the selector rather
        // than the digest splits one interest into two.
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let hash = |sel: &ProviderSelector| {
            let mut h = DefaultHasher::new();
            sel.hash(&mut h);
            h.finish()
        };
        let tag = |k: &str, v: &str| TagMatch {
            key: k.into(),
            value: v.into(),
        };

        // Raw variants, reordered + duplicated — bypassing `nodes()`.
        let a = ProviderSelector::Nodes(vec![9, 7, 3, 3]);
        let b = ProviderSelector::Nodes(vec![3, 7, 9]);
        assert_eq!(a, b, "reordered node sets are one identity");
        assert_eq!(hash(&a), hash(&b), "equal selectors must hash equal");
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());

        let t1 = ProviderSelector::Tags(vec![tag("site", "f7"), tag("modality", "thermal")]);
        let t2 = ProviderSelector::Tags(vec![tag("modality", "thermal"), tag("site", "f7")]);
        assert_eq!(t1, t2);
        assert_eq!(hash(&t1), hash(&t2));

        // Genuinely different populations stay distinct.
        assert_ne!(a, ProviderSelector::Nodes(vec![3, 7]));

        // A spec keyed via the raw-variant selector coalesces with one
        // built through the canonicalizing constructor.
        let mut spec_raw = spec();
        spec_raw.providers = ProviderSelector::Nodes(vec![9, 7, 3]);
        let mut spec_ctor = spec();
        spec_ctor.providers = ProviderSelector::nodes(vec![3, 9, 7]);
        assert_eq!(spec_raw.providers, spec_ctor.providers);
        assert_eq!(spec_raw.interest_digest(), spec_ctor.interest_digest());
    }

    #[test]
    fn work_latency_is_identity_bearing() {
        let mut other = spec();
        other.work_latency.provider_start_within = Some(Duration::from_millis(251));
        assert_ne!(spec().interest_digest(), other.interest_digest());
        // Presence itself is identity-bearing too, and the two
        // dimensions are position-distinct (injective encoding).
        let mut absent = spec();
        absent.work_latency.provider_start_within = None;
        assert_ne!(spec().interest_digest(), absent.interest_digest());
        let swapped = WorkLatencyEnvelope {
            provider_start_within: None,
            first_event_after_admission: Some(Duration::from_millis(250)),
        };
        assert_ne!(
            WorkLatencyEnvelope::start_within(Duration::from_millis(250)).canonical_bytes(),
            swapped.canonical_bytes(),
        );
    }

    #[test]
    fn capability_id_is_identity_bearing() {
        let mut other = spec();
        other.capability_id = CapabilityId::new("video.transcodf");
        assert_ne!(spec().interest_digest(), other.interest_digest());
    }

    #[test]
    fn audience_commitment_is_identity_bearing() {
        // v3.1: two audiences with identical predicates must not
        // coalesce — by digest construction, not relay diligence.
        let mut other = spec();
        other.audience = audience(0xBB);
        assert_ne!(spec().interest_digest(), other.interest_digest());
    }

    #[test]
    fn constraints_are_identity_bearing() {
        let mut other = spec();
        other.constraints =
            CanonicalConstraints::from_entries([("fps", "30"), ("resolution", "3840x2160")])
                .unwrap();
        assert_ne!(spec().interest_digest(), other.interest_digest());
    }

    #[test]
    fn canonicalization_is_insertion_order_insensitive() {
        let ab = CanonicalConstraints::from_entries([("a", "1"), ("b", "2")]).unwrap();
        let ba = CanonicalConstraints::from_entries([("b", "2"), ("a", "1")]).unwrap();
        assert_eq!(ab.canonical_bytes(), ba.canonical_bytes());
        assert_eq!(ab.constraints_digest(), ba.constraints_digest());
    }

    #[test]
    fn length_prefixes_keep_the_encoding_injective() {
        // The classic concatenation ambiguity: without length
        // prefixes {"ab":"c"} and {"a":"bc"} would feed a hasher the
        // same bytes and merge two different predicates.
        let ab_c = CanonicalConstraints::from_entries([("ab", "c")]).unwrap();
        let a_bc = CanonicalConstraints::from_entries([("a", "bc")]).unwrap();
        assert_ne!(ab_c.canonical_bytes(), a_bc.canonical_bytes());
        assert_ne!(ab_c.constraints_digest(), a_bc.constraints_digest());
    }

    #[test]
    fn parse_round_trips_canonical_bytes() {
        let original =
            CanonicalConstraints::from_entries([("a", "1"), ("b", ""), ("c", "x")]).unwrap();
        let parsed = CanonicalConstraints::parse_canonical(&original.canonical_bytes()).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn parse_rejects_every_non_canonical_form() {
        let one = |k: &str, v: &str| {
            let mut out = Vec::new();
            for part in [k, v] {
                out.extend_from_slice(&(part.len() as u32).to_le_bytes());
                out.extend_from_slice(part.as_bytes());
            }
            out
        };
        // Entries out of order.
        let mut unsorted = 2u32.to_le_bytes().to_vec();
        unsorted.extend(one("b", "2"));
        unsorted.extend(one("a", "1"));
        assert_eq!(
            CanonicalConstraints::parse_canonical(&unsorted),
            Err(ConstraintError::NonCanonicalOrder),
        );
        // Duplicate key.
        let mut dup = 2u32.to_le_bytes().to_vec();
        dup.extend(one("a", "1"));
        dup.extend(one("a", "2"));
        assert_eq!(
            CanonicalConstraints::parse_canonical(&dup),
            Err(ConstraintError::DuplicateKey),
        );
        // Truncated mid-field.
        let good = CanonicalConstraints::from_entries([("a", "1")])
            .unwrap()
            .canonical_bytes();
        assert_eq!(
            CanonicalConstraints::parse_canonical(&good[..good.len() - 1]),
            Err(ConstraintError::Truncated),
        );
        // Trailing garbage.
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            CanonicalConstraints::parse_canonical(&trailing),
            Err(ConstraintError::TrailingBytes),
        );
        // Invalid UTF-8 key.
        let mut bad_utf8 = 1u32.to_le_bytes().to_vec();
        bad_utf8.extend_from_slice(&2u32.to_le_bytes());
        bad_utf8.extend_from_slice(&[0xFF, 0xFE]);
        bad_utf8.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            CanonicalConstraints::parse_canonical(&bad_utf8),
            Err(ConstraintError::InvalidUtf8),
        );
    }

    #[test]
    fn oversize_is_rejected_on_build_and_parse() {
        let big = "x".repeat(MAX_CONSTRAINT_BYTES);
        assert!(matches!(
            CanonicalConstraints::from_entries([("k", big.as_str())]),
            Err(ConstraintError::Oversize { .. }),
        ));
        let bytes = vec![0u8; MAX_CONSTRAINT_BYTES + 1];
        assert!(matches!(
            CanonicalConstraints::parse_canonical(&bytes),
            Err(ConstraintError::Oversize { .. }),
        ));
    }

    #[test]
    fn digest_mismatch_is_the_only_security_relevant_rejection() {
        let constraints = CanonicalConstraints::from_entries([("a", "1")]).unwrap();
        let bytes = constraints.canonical_bytes();
        // Right digest → parses.
        assert!(
            CanonicalConstraints::validate_inline(&bytes, &constraints.constraints_digest())
                .is_ok()
        );
        // Wrong digest → DigestMismatch, security-relevant.
        let wrong = Digest256::from_bytes([0u8; 32]);
        let err = CanonicalConstraints::validate_inline(&bytes, &wrong).unwrap_err();
        assert_eq!(err, ConstraintError::DigestMismatch);
        assert!(err.is_security_relevant());
        // Plain decode failures are InvalidConstraints, NOT security
        // events.
        assert!(!ConstraintError::Truncated.is_security_relevant());
        assert!(!ConstraintError::Oversize { len: 0 }.is_security_relevant());
        assert!(!ConstraintError::NonCanonicalOrder.is_security_relevant());
    }

    #[test]
    fn constraints_and_interest_digests_are_domain_separated() {
        // The constraints digest of C and the interest digest of a
        // spec are computed with different derive_key contexts, so
        // even a contrived pre-image overlap cannot collide them.
        let constraints = CanonicalConstraints::from_entries([("a", "1")]).unwrap();
        assert_ne!(constraints.constraints_digest(), spec().interest_digest(),);
    }

    #[test]
    fn interest_key_binds_the_predicate_not_the_provider() {
        let base = spec().key();
        // Constraints split the interest…
        let mut other = spec();
        other.constraints = CanonicalConstraints::from_entries([("fps", "30")]).unwrap();
        assert_ne!(base, other.key());
        // …while the answering provider never does: providers vary
        // beneath one interest as observation keys.
        assert_eq!(base.interest_digest, spec().interest_digest());
        assert_ne!(
            ProviderObservationKey::new(base.clone(), 7, 1),
            ProviderObservationKey::new(base, 8, 1),
        );
    }

    #[test]
    fn strictest_sample_interval_is_min_dominance() {
        assert_eq!(strictest_sample_interval([]), None);
        assert_eq!(
            strictest_sample_interval([
                Duration::from_millis(500),
                Duration::from_millis(50),
                Duration::from_secs(5),
            ]),
            Some(Duration::from_millis(50)),
        );
    }
}
