//! Conditional readiness identity (plan §3.1–§3.3).
//!
//! All sensing state — wire, table, fold overlay — is keyed by the
//! FULL conditional sensing identity ([`ReadinessKey`]), never less.
//! Two interests against the same capability (720p@30 vs 4K@60) are
//! two keys, two observations, two lifecycles: one going Unknown
//! never touches the other and never suspends the capability entry.
//!
//! The identity is 256-bit and domain-separated. A 64-bit key is an
//! adversarial-collision hazard when it *merges* interests; short
//! indices may be derived for local tables but are never the wire or
//! semantic identity.
//!
//! Three time dimensions, three rules (plan §3.3):
//!
//! | dimension | rule |
//! |---|---|
//! | `work_latency` (L) | part of the readiness predicate — exact match, inside the digest |
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

/// A 256-bit sensing digest (blake3 output).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest256([u8; 32]);

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
/// capability name, canonicalized as its UTF-8 bytes. Paired with
/// [`ReadinessKey::capability_generation`] (the announcement's
/// per-origin monotonic `version`) so a readiness identity can never
/// straddle a capability redefinition.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
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
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
/// identity check (plan §4.9).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudienceScopeCommitment([u8; 32]);

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

/// The latency envelope L — "can the provider *start* this work
/// within L". Part of the readiness **predicate**, so it matches
/// exactly (inside the digest); it is not a sampling or delivery
/// parameter (plan §3.3).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WorkLatencyEnvelope {
    /// Upper bound on time-to-start the provider must be able to
    /// honor for the predicate to evaluate Ready.
    pub max_start: Duration,
}

impl WorkLatencyEnvelope {
    /// Fixed-width canonical encoding (u128 LE nanoseconds) — the
    /// form hashed into the interest digest.
    pub fn canonical_bytes(&self) -> [u8; 16] {
        self.max_start.as_nanos().to_le_bytes()
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

/// The digest-bearing half of an interest: everything that defines
/// the readiness **predicate identity** (plan §3.2) — and nothing
/// that doesn't. `requested_sample_interval` is deliberately absent:
/// stricter sampling dominates looser (§3.3), so D must not split
/// otherwise-identical interests.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InterestSpec {
    /// Capability the predicate targets.
    pub capability_id: CapabilityId,
    /// The provider's announce version this predicate was compiled
    /// against; a generation change is a new identity.
    pub capability_generation: u64,
    /// Work characteristics C.
    pub constraints: CanonicalConstraints,
    /// Latency envelope L.
    pub work_latency: WorkLatencyEnvelope,
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
    ///     capability_generation ||
    ///     len(canonical(C)) || canonical(C) ||
    ///     canonical(L) || disclosure_class_tag || audience_commitment)
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
        hasher.update(&self.capability_generation.to_le_bytes());
        let constraint_bytes = self.constraints.canonical_bytes();
        hasher.update(&(constraint_bytes.len() as u64).to_le_bytes());
        hasher.update(&constraint_bytes);
        hasher.update(&self.work_latency.canonical_bytes());
        hasher.update(&[self.disclosure_class.canonical_tag()]);
        hasher.update(self.audience.as_bytes());
        Digest256(*hasher.finalize().as_bytes())
    }
}

/// The full conditional sensing identity (plan §3.1). Every table,
/// observation, and fold-overlay entry is keyed by this — never by
/// capability alone.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ReadinessKey {
    /// Provider node id.
    pub provider: u64,
    /// Capability the interest targets.
    pub capability_id: CapabilityId,
    /// Provider's announce version (see [`InterestSpec`]).
    pub capability_generation: u64,
    /// The 256-bit interest identity ([`InterestSpec::interest_digest`]).
    pub interest_digest: Digest256,
}

impl ReadinessKey {
    /// Key a spec against a provider.
    pub fn for_interest(provider: u64, spec: &InterestSpec) -> Self {
        Self {
            provider,
            capability_id: spec.capability_id.clone(),
            capability_generation: spec.capability_generation,
            interest_digest: spec.interest_digest(),
        }
    }
}

/// A consumer's registration: the predicate identity plus the two
/// non-identity time dimensions (plan §3.3).
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
            capability_generation: 10,
            constraints: CanonicalConstraints::from_entries([
                ("fps", "60"),
                ("resolution", "3840x2160"),
            ])
            .unwrap(),
            work_latency: WorkLatencyEnvelope {
                max_start: Duration::from_millis(250),
            },
            disclosure_class: DisclosureClass::Owner,
            audience: audience(0xAA),
        }
    }

    #[test]
    fn digest_is_deterministic() {
        assert_eq!(spec().interest_digest(), spec().interest_digest());
    }

    #[test]
    fn requested_sample_interval_never_splits_identity() {
        // §3.3: D is min-dominated, not identity-bearing. Two
        // registrations that differ ONLY in D (and ttl) must key to
        // the same ReadinessKey — this is the tripwire against D
        // ever migrating into the digest.
        let strict = InterestRegistration {
            spec: spec(),
            requested_sample_interval: Duration::from_millis(50),
            soft_state_ttl: Duration::from_secs(30),
        };
        let loose = InterestRegistration {
            spec: spec(),
            requested_sample_interval: Duration::from_secs(5),
            soft_state_ttl: Duration::from_secs(300),
        };
        assert_eq!(
            ReadinessKey::for_interest(7, &strict.spec),
            ReadinessKey::for_interest(7, &loose.spec),
        );
    }

    #[test]
    fn work_latency_is_identity_bearing() {
        let mut other = spec();
        other.work_latency.max_start = Duration::from_millis(251);
        assert_ne!(spec().interest_digest(), other.interest_digest());
    }

    #[test]
    fn capability_generation_is_identity_bearing() {
        let mut other = spec();
        other.capability_generation = 11;
        assert_ne!(spec().interest_digest(), other.interest_digest());
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
    fn readiness_key_binds_the_full_identity() {
        let base = ReadinessKey::for_interest(7, &spec());
        assert_ne!(base, ReadinessKey::for_interest(8, &spec()));
        let mut other = spec();
        other.constraints = CanonicalConstraints::from_entries([("fps", "30")]).unwrap();
        assert_ne!(base, ReadinessKey::for_interest(7, &other));
        assert_eq!(base.interest_digest, spec().interest_digest());
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
