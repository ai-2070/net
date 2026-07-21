//! OA3-5 §3.2 — opaque scoped-announcement propagation: the outer relay frame
//! and the bounded relay dedup gate.
//!
//! A [`ScopedCapabilityAnnouncement`]
//! is flooded across the mesh so a consumer that shares the audience can
//! discover a provider it has no direct session with, WITHOUT any relay ever
//! learning the plaintext. The envelope itself stays byte-for-byte the signed
//! object OA3-2 defined; this module adds only the UNSIGNED transport wrapper (a
//! hop counter, exactly like plaintext CAP-ANN's `hop_count`) and the per-node
//! dedup state that stops a flood from looping (Kyra OA3-5 wire ruling — do NOT
//! add `hop_count` to the signed envelope).
//!
//! # §23 — "learning the plaintext" is the precise claim, and it is the only one
//!
//! A relay cannot open the descriptor: that needs the audience key, and the
//! AEAD binds provider‖owner_org‖handle‖grant_id‖generation‖expires_at, so an
//! envelope cannot be transplanted into another context either. The
//! ciphertext is padded to 256-byte buckets, so its length discloses only a
//! coarse bucket index rather than the capability name's length.
//!
//! What a relay DOES see, because routing needs it, is the envelope's
//! cleartext framing: `provider`, `owner_org`, `grant_id`, `audience_handle`,
//! `generation` and `expires_at`. `grant_id` and `audience_handle` are stable
//! for a grant's lifetime, so a relay on the path can build a persistent map
//! of WHICH providers serve WHICH cross-org grant, and the all-zero owner
//! sentinel explicitly labels owner-scoped envelopes — letting it count an
//! org's internal private-service announcements and watch their re-announce
//! cadence via `generation`.
//!
//! That is a metadata channel, not a confidentiality break, and it is
//! inherent: a relay that cannot read the routing fields cannot route. It is
//! documented here because the phrase "without any relay ever learning the
//! plaintext" is easy to read as "learning nothing", and an operator placing
//! an untrusted relay on a private-discovery path should know what that relay
//! can infer. (`grant_id` is arguably redundant with `audience_handle` on the
//! wire and could be dropped to narrow this — not done, because the ingest
//! selector keys on it and the change would be a wire break for a metadata
//! reduction, not an exposure fix.)

use std::collections::BTreeMap;

use parking_lot::Mutex;

use crate::adapter::net::behavior::capability::MAX_CAPABILITY_HOPS;
use crate::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
use crate::adapter::net::identity::EntityId;

/// Coarse relay-level clock-skew tolerance for the expiry pre-filter. A relay
/// only drops envelopes already dead by this margin; the PRECISE expiry window
/// (with the authority's configured skew) is re-enforced at local open. Generous
/// so a slightly-fast relay clock never strands a still-fresh flood.
const RELAY_EXPIRY_SKEW_SECS: u64 = 300;

/// The UNSIGNED outer frame carried on `SUBPROTOCOL_SCOPED_CAPABILITY_ANN`:
///
/// ```text
/// version(1) ‖ hop_count(1) ‖ canonical scoped-announcement bytes
/// ```
///
/// The provider signature inside the envelope covers every envelope field but
/// its own signature; the `hop_count` prefix is hop-authored transport metadata
/// a relay increments without touching (or needing the key to re-sign) the
/// signed envelope — exactly the model plaintext CAP-ANN uses for its own
/// unsigned `hop_count`. No inner length is encoded: `EventFrame` already bounds
/// each event, and the envelope's fixed-offset codec bounds itself, so the
/// remainder after the two-byte prefix is the whole envelope.
pub struct ScopedCapabilityRelayFrame<'a> {
    /// Forwarding depth: the origin (B2 send) stamps 0; each relay increments
    /// before re-broadcast. Capped at [`MAX_CAPABILITY_HOPS`].
    pub hop_count: u8,
    /// The canonical, still-opaque scoped-announcement bytes (borrowed).
    pub envelope: &'a [u8],
}

impl<'a> ScopedCapabilityRelayFrame<'a> {
    /// Current frame version. An unknown version decodes to `None` (fail-closed
    /// — never a best-effort reinterpretation of an unknown layout).
    pub const VERSION: u8 = 1;
    /// `version` + `hop_count`.
    const PREFIX_LEN: usize = 2;

    /// Encode `version ‖ hop_count ‖ envelope`.
    pub fn encode(hop_count: u8, envelope: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::PREFIX_LEN + envelope.len());
        out.push(Self::VERSION);
        out.push(hop_count);
        out.extend_from_slice(envelope);
        out
    }

    /// Strict decode: the exact version byte, a hop byte, and a NON-EMPTY
    /// envelope remainder. Borrows the envelope bytes (no copy).
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        // version + hop + at least one envelope byte.
        if bytes.len() < Self::PREFIX_LEN + 1 || bytes[0] != Self::VERSION {
            return None;
        }
        Some(Self {
            hop_count: bytes[1],
            envelope: &bytes[Self::PREFIX_LEN..],
        })
    }
}

/// The OUTER identity a relay dedups on. Every field is authenticated by the
/// envelope's provider signature (a forged frame can't collide a real one's
/// key), and together they name exactly one `(provider, audience, generation)`
/// announcement — the same dedup identity the consumer store keys on, lifted to
/// the relay so a re-flooded duplicate is dropped before any AEAD or forward
/// work.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct RelayDedupKey {
    /// The publishing provider P (from the envelope, signature-covered).
    pub provider: EntityId,
    /// The grant id (owner-scoped envelopes carry the zero sentinel).
    pub grant_id: [u8; 32],
    /// The audience handle the envelope is sealed to.
    pub audience_handle: [u8; 32],
    /// The announcement generation (freshness / dedup ordering).
    pub generation: u64,
}

/// Bounded, expiring relay dedup gate — SEPARATE from the consumer
/// [`ScopedDiscoveryStore`](super::org_scoped_store::ScopedDiscoveryStore) (Kyra
/// OA3-5). It gates FORWARDING + local-open work on outer-identity freshness so
/// a flooded envelope is relayed at most once per node, without ever decrypting
/// or storing anything. Properties:
///
/// * an entry is admitted only AFTER the caller has verified the outer
///   signature — a malformed/unsigned envelope never primes the gate;
/// * entries expire on a LOCAL retention horizon, never the envelope's own
///   attacker-controlled `expires_at`;
/// * when full, an unseen identity is refused FAIL-CLOSED — an active seen-key is
///   never evicted merely to admit another, or a re-flooded duplicate for the
///   evicted key would restart the flood loop;
/// * occupancy is accounted PER INGRESS PEER and capped, so no single peer can
///   consume the whole gate and turn that fail-closed refusal into a mesh-wide
///   outage (§7 — see `MAX_ENTRIES_PER_PEER`).
#[derive(Default)]
pub struct ScopedAnnRelayGate {
    inner: Mutex<RelayGateInner>,
}

/// What the dedup gate decided about one delivered frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayAdmission {
    /// First sighting of this identity: forward it AND hand it to the local
    /// audience open/store step.
    Fresh,
    /// A duplicate that arrived on a strictly SHORTER path (§8). Forward the
    /// improved hop count so the subtree behind this node is not truncated —
    /// but do NOT re-ingest: the store already holds this identity, and
    /// re-running the AEAD open on a duplicate would hand an attacker free
    /// repeated work.
    ShorterPath,
    /// Duplicate at an equal-or-worse hop, ingress peer over budget, or the
    /// gate is globally full (both budget cases fail closed).
    Drop,
}

impl RelayAdmission {
    /// Whether this frame should be forwarded onward.
    pub fn forwards(self) -> bool {
        matches!(self, Self::Fresh | Self::ShorterPath)
    }
    /// Whether this frame should also be handed to the LOCAL open/store step.
    pub fn ingests_locally(self) -> bool {
        matches!(self, Self::Fresh)
    }
}

/// One remembered relay identity.
#[derive(Debug)]
struct SeenEntry {
    /// Local retention deadline (unix secs).
    deadline: u64,
    /// The authenticated ingress peer whose frame first admitted this identity.
    /// Held so the sweep can return the slot to that peer's budget.
    admitted_by: u64,
    /// The BEST (lowest) hop count seen for this identity so far (§8).
    ///
    /// `hop_count` is outside the provider signature, so a relay can inflate
    /// it; remembering the minimum lets a later, honest, shorter-path copy
    /// re-forward and repair a subtree that an inflated first sighting would
    /// otherwise have cut off for the whole generation.
    min_hop: u8,
}

#[derive(Default)]
struct RelayGateInner {
    /// GLOBAL dedup — one identity is relayed at most once per node no matter
    /// how many peers deliver it. Keyed on the outer identity ALONE, never on
    /// `(peer, identity)`: keying per peer would admit the same envelope once
    /// per ingress adjacency and reopen the flood amplification this gate
    /// exists to close.
    seen: BTreeMap<RelayDedupKey, SeenEntry>,
    /// Live occupancy per ingress peer, derived from `seen` and kept in step
    /// with it by every mutation. A peer with no live entries is removed, so
    /// this never grows past the number of peers actually holding slots.
    per_peer: BTreeMap<u64, usize>,
}

impl RelayGateInner {
    /// Drop horizon-passed entries, returning each slot to its admitting peer's
    /// budget. A horizon-passed key is fully forgotten and admissible again.
    fn sweep(&mut self, now_secs: u64) {
        let per_peer = &mut self.per_peer;
        self.seen.retain(|_, entry| {
            if now_secs < entry.deadline {
                return true;
            }
            if let Some(count) = per_peer.get_mut(&entry.admitted_by) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    per_peer.remove(&entry.admitted_by);
                }
            }
            false
        });
    }
}

impl ScopedAnnRelayGate {
    /// Hard bound on tracked identities. A distinct-identity flood is refused
    /// fail-closed at the cap rather than growing without bound.
    const MAX_ENTRIES: usize = 8192;
    /// Hard bound on how many live entries ONE ingress peer may hold (§7).
    ///
    /// Without this, the fail-closed rule above is a denial-of-service
    /// primitive rather than a protection. The gate is keyed on the envelope's
    /// SELF-DECLARED provider identity: `ScopedCapabilityAnnouncement::from_bytes`
    /// verifies the outer signature against whatever provider key the sender
    /// chose, `OrgMembershipCert::from_bytes` does not verify at all, and a
    /// frame at `hop_count > 0` skips the direct-origin bind. So relay
    /// identities are free to mint — one session-authenticated peer could
    /// generate `MAX_ENTRIES` fresh keypairs, fill the gate for
    /// `RETENTION_SECS`, and every subsequent LEGITIMATE envelope would hit the
    /// capacity refusal. Because `decide_scoped_relay` returning `None`
    /// suppresses the local ingest as well as the forward, that is a total
    /// scoped-discovery blackout — and since the fill-phase envelopes are
    /// themselves forwarded, one ~3.6 MB burst propagates it across the
    /// connected mesh.
    ///
    /// Capping per-peer occupancy at an eighth of the gate bounds one peer's
    /// share and leaves the rest available to everyone else. Colluding peers
    /// can still combine, but each must hold an authenticated session — a far
    /// higher bar than minting keypairs, and one the transport already
    /// accounts for.
    const MAX_ENTRIES_PER_PEER: usize = Self::MAX_ENTRIES / 8;
    /// How long a seen identity is remembered, as a LOCAL cap (not the
    /// envelope's `expires_at`). Long enough to absorb a flood's settling +
    /// ordinary re-announce cadence; short enough that the bounded map drains.
    const RETENTION_SECS: u64 = 600;

    /// A fresh, empty gate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit `key`, delivered by authenticated ingress peer `from_node`, for
    /// forwarding + local open iff it is FRESH (unseen within the retention
    /// horizon) and both the per-peer and global budgets have room; record it
    /// when admitted.
    ///
    /// Returns `false` for a duplicate (drop before any AEAD / forward work),
    /// when `from_node` is over its own budget, or when the gate is globally
    /// full — the last two are fail-closed refusals, see the type docs.
    pub fn admit(
        &self,
        from_node: u64,
        key: RelayDedupKey,
        hop_count: u8,
        now_secs: u64,
    ) -> RelayAdmission {
        let mut inner = self.inner.lock();
        inner.sweep(now_secs);
        if let Some(existing) = inner.seen.get_mut(&key) {
            // §8 — a duplicate that arrived on a STRICTLY SHORTER path must be
            // re-forwarded, or one relay can truncate the flood for a whole
            // generation.
            //
            // `hop_count` rides OUTSIDE the provider's signature by design, so
            // a malicious relay can re-emit a legitimate envelope verbatim with
            // `hop_count = MAX - 1`. Each victim admitted it, ingested locally,
            // and forwarded NOTHING (it is at the hop boundary). When the
            // honest copy arrived seconds later over a real path it was a plain
            // duplicate and was dropped — so it was not forwarded either. One
            // well-connected relay therefore suppressed propagation to every
            // subtree behind its victims until the generation rolled.
            //
            // Remembering the best hop seen and re-forwarding on improvement
            // costs one byte per entry and makes the attack self-correcting:
            // the honest shorter-path copy repairs the subtree. The dedup
            // identity is NOT re-admitted for local ingest — the store already
            // holds it — so this cannot be used to replay AEAD work.
            if hop_count < existing.min_hop {
                existing.min_hop = hop_count;
                return RelayAdmission::ShorterPath;
            }
            return RelayAdmission::Drop; // duplicate at an equal-or-worse hop
        }
        // Per-peer budget FIRST, so a flooding peer exhausts its own share
        // before it can apply any pressure to the global cap.
        if inner.per_peer.get(&from_node).copied().unwrap_or(0) >= Self::MAX_ENTRIES_PER_PEER {
            return RelayAdmission::Drop;
        }
        if inner.seen.len() >= Self::MAX_ENTRIES {
            // FAIL-CLOSED at capacity: never evict an active seen-key to make
            // room, or a re-flooded duplicate for that key would be admitted
            // again and reflood. The horizon sweep above is the only reclaim.
            return RelayAdmission::Drop;
        }
        inner.seen.insert(
            key,
            SeenEntry {
                deadline: now_secs.saturating_add(Self::RETENTION_SECS),
                admitted_by: from_node,
                min_hop: hop_count,
            },
        );
        *inner.per_peer.entry(from_node).or_insert(0) += 1;
        RelayAdmission::Fresh
    }

    /// Release an identity admitted by [`Self::admit`] whose LOCAL ingest was
    /// refused by a condition that can clear on its own (§24).
    ///
    /// The gate is primed before the local open/store runs — it has to be,
    /// because forwarding must not wait on an audience open a relay may not be
    /// able to perform. Without this, a fail-closed refusal (poisoned
    /// revocation view, publication race, store at capacity) consumed the
    /// identity for the full retention horizon with nothing stored, and every
    /// re-delivery of that generation was dropped as a duplicate.
    ///
    /// Only ever called for a Retryable disposition: releasing after a FINAL
    /// decision would re-open the dedup gate for an envelope this node has
    /// already judged, which is the loop suppression the gate exists for.
    /// Returns the per-peer slot too, so a peer is not charged for an
    /// admission that did not stick.
    pub fn release(&self, key: &RelayDedupKey) {
        let mut inner = self.inner.lock();
        if let Some(entry) = inner.seen.remove(key) {
            if let Some(count) = inner.per_peer.get_mut(&entry.admitted_by) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    inner.per_peer.remove(&entry.admitted_by);
                }
            }
        }
    }
    /// Number of tracked (currently-remembered) relay identities. Used by the
    /// module unit tests and the live relay witness's `_for_test` seam to
    /// confirm a relay ADMITTED (and thus forwarded) an envelope it can't store.
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().seen.len()
    }

    /// Test shim preserving the pre-§8 boolean shape: admit at hop 0 and report
    /// whether this was a FRESH sighting. Every pre-existing gate witness is
    /// about dedup / budgets / retention rather than hop improvement, so
    /// keeping their assertions verbatim keeps them honest about what they
    /// test; the §8 hop behaviour has its own witnesses.
    #[cfg(test)]
    fn admit_fresh(&self, from_node: u64, key: RelayDedupKey, now_secs: u64) -> bool {
        self.admit(from_node, key, 0, now_secs) == RelayAdmission::Fresh
    }
}

/// The outcome of an ADMITTED scoped-relay decision (see [`decide_scoped_relay`]).
pub struct ScopedRelayAdmit {
    /// The outer-verified envelope, handed to the independent LOCAL open/store
    /// step (a relay with no matching audience simply stores nothing).
    pub envelope: ScopedCapabilityAnnouncement,
    /// The hop-incremented frame to forward to peers-except-ingress, or `None`
    /// at the hop boundary (the frame has reached its forwarding depth).
    pub forward: Option<Vec<u8>>,
    /// The dedup identity this admission occupies, so the caller can RELEASE
    /// it if the local ingest turns out to be retryable (§24).
    pub dedup_key: RelayDedupKey,
    /// Whether to also run the LOCAL audience open/store step.
    ///
    /// `false` for a §8 shorter-path re-forward: the identity is already in the
    /// store, so re-opening the AEAD would be duplicated work an attacker could
    /// solicit by replaying the same envelope at ever-lower hop counts.
    pub ingest_locally: bool,
}

/// OA3-5 §3.2 relay decision — PURE, no I/O, so it is deterministically
/// unit-testable without sockets or a live authority. Given a raw inbound frame
/// and its authenticated ingress `from_node`, it:
///
/// 1. rejects the unresolved `from_node == 0` sentinel;
/// 2. strictly decodes the outer relay frame;
/// 3. structurally decodes + OUTER-VERIFIES the envelope (`from_bytes` is
///    verified-by-construction) — a malformed / unsigned / forged envelope never
///    reaches the dedup gate;
/// 4. binds the DIRECT origin when `hop_count == 0` (the session peer must be the
///    provider); for `hop_count > 0` the session identifies only the relay and
///    the envelope signature identifies the provider;
/// 5. drops an already-expired envelope (coarse, local-skew bound);
/// 6. dedups on the OUTER identity through `gate` — a re-flood is dropped before
///    any forward or AEAD work, and the gate is fail-closed when full.
///
/// Returns the verified envelope plus the frame to forward (present iff below the
/// hop cap) when ADMITTED; `None` when the frame is dropped. The caller performs
/// the actual forward and the independent local open/store.
pub fn decide_scoped_relay(
    frame_bytes: &[u8],
    from_node: u64,
    gate: &ScopedAnnRelayGate,
    now_secs: u64,
) -> Option<ScopedRelayAdmit> {
    // A session-authenticated peer is always a real node id; 0 is the
    // unresolved sentinel and must never prime the gate or be forwarded from.
    if from_node == 0 {
        return None;
    }
    let frame = ScopedCapabilityRelayFrame::decode(frame_bytes)?;
    // Structural + bounds + the provider's outer signature. A decode/verify
    // failure returns before the gate, so a malformed frame never primes dedup.
    let envelope = ScopedCapabilityAnnouncement::from_bytes(frame.envelope).ok()?;
    // Direct-origin binding: a hop-0 frame claims to come straight from the
    // provider, so the authenticated ingress peer MUST be that provider.
    if frame.hop_count == 0 && envelope.provider().node_id() != from_node {
        return None;
    }
    // Coarse expiry: never forward an already-dead envelope.
    if envelope.expires_at().saturating_add(RELAY_EXPIRY_SKEW_SECS) <= now_secs {
        return None;
    }
    // Bounded relay dedup on the outer identity — fail-closed when full.
    let key = RelayDedupKey {
        provider: envelope.provider().clone(),
        grant_id: *envelope.grant_id(),
        audience_handle: *envelope.audience_handle(),
        generation: envelope.generation(),
    };
    let key_for_result = key.clone();
    let admission = gate.admit(from_node, key, frame.hop_count, now_secs);
    if !admission.forwards() {
        return None;
    }
    // Forward while below the hop cap; only the outer hop prefix is rewritten —
    // the signed envelope bytes are preserved verbatim.
    let forward = if frame.hop_count < MAX_CAPABILITY_HOPS - 1 {
        Some(ScopedCapabilityRelayFrame::encode(
            frame.hop_count.saturating_add(1),
            frame.envelope,
        ))
    } else {
        None
    };
    Some(ScopedRelayAdmit {
        envelope,
        forward,
        dedup_key: key_for_result,
        ingest_locally: admission.ingests_locally(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // §8 — a relay must not be able to truncate the flood.
    // ------------------------------------------------------------------

    /// §24 — releasing a retryable identity lets the SAME generation be
    /// reconsidered; a final one stays deduped.
    ///
    /// The gate is primed before the local ingest runs, so a fail-closed
    /// refusal (poisoned revocation view, publication race, store at capacity)
    /// consumed the identity for the full 600 s retention with nothing stored,
    /// and every re-delivery of that generation was dropped as a duplicate. It
    /// self-healed only because the next periodic emission bumps the
    /// generation — incidental, and gone entirely if generation ever becomes
    /// change-triggered rather than emission-triggered.
    ///
    /// Both halves matter: release must restore admissibility, and NOT
    /// releasing must keep the loop suppression the gate exists for.
    #[test]
    fn a_released_identity_is_admissible_again_and_a_retained_one_is_not() {
        let gate = ScopedAnnRelayGate::new();
        const PEER: u64 = 9;
        let key = key_n(1, 7);

        assert_eq!(
            gate.admit(PEER, key.clone(), 0, 1_000),
            RelayAdmission::Fresh,
        );
        // Retained: the ordinary duplicate path is untouched.
        assert_eq!(
            gate.admit(PEER, key.clone(), 0, 1_000),
            RelayAdmission::Drop,
            "without a release, a re-delivery is still a duplicate — this is \
             the loop suppression the gate exists for",
        );

        // Released (the local ingest was refused by a transient condition).
        gate.release(&key);
        assert_eq!(gate.len(), 0, "the identity must leave the gate");
        assert_eq!(
            gate.admit(PEER, key.clone(), 0, 1_000),
            RelayAdmission::Fresh,
            "the same generation must be reconsidered after a retryable \
             refusal, not swallowed for the retention horizon",
        );

        // The per-peer budget is returned too, or a peer would be charged for
        // an admission that did not stick and could be starved by its own
        // retries.
        gate.release(&key);
        for index in 0..ScopedAnnRelayGate::MAX_ENTRIES_PER_PEER as u64 {
            assert_eq!(
                gate.admit(PEER, key_n(index + 100, 1), 0, 1_000),
                RelayAdmission::Fresh,
                "peer budget leaked across release at index {index}",
            );
        }
    }

    /// Releasing an identity the gate never admitted is a no-op, not a panic
    /// or an underflow of the per-peer budget.
    #[test]
    fn releasing_an_unknown_identity_is_harmless() {
        let gate = ScopedAnnRelayGate::new();
        gate.release(&key_n(42, 1));
        assert_eq!(gate.len(), 0);
        assert_eq!(gate.admit(7, key_n(42, 1), 0, 1_000), RelayAdmission::Fresh,);
    }
    /// The attack: `hop_count` rides OUTSIDE the provider's signature, so a
    /// malicious relay can re-emit a legitimate envelope verbatim at the hop
    /// boundary. The victim admits it, ingests locally, and forwards NOTHING.
    /// When the honest copy arrives over a real short path it used to be a
    /// plain duplicate and was dropped — so it was not forwarded either, and
    /// every subtree behind the victim went dark for the whole generation.
    ///
    /// The gate now remembers the best hop seen and re-admits for FORWARDING
    /// on a strict improvement, so the honest copy repairs the subtree.
    #[test]
    fn a_shorter_path_duplicate_is_re_forwarded() {
        let gate = ScopedAnnRelayGate::new();
        const MALICIOUS: u64 = 0xBAD;
        const HONEST: u64 = 0x600D;
        let key = key_n(1, 7);

        // Inflated first sighting: admitted, ingested, at the hop boundary.
        assert_eq!(
            gate.admit(MALICIOUS, key.clone(), MAX_CAPABILITY_HOPS - 1, 1_000),
            RelayAdmission::Fresh,
        );

        // The honest, genuinely-closer copy: re-forwarded, NOT re-ingested.
        let honest = gate.admit(HONEST, key.clone(), 0, 1_000);
        assert_eq!(honest, RelayAdmission::ShorterPath);
        assert!(
            honest.forwards(),
            "the shorter-path copy must be forwarded, or the subtree behind \
             this node stays truncated for the whole generation",
        );
        assert!(
            !honest.ingests_locally(),
            "it must NOT be re-ingested — the store already holds this \
             identity, and re-opening the AEAD would let a peer solicit \
             repeated crypto work by replaying at ever-lower hops",
        );

        // Ratchet: once the best hop is 0, nothing improves on it, so an
        // attacker cannot use this path to re-forward without bound.
        assert_eq!(
            gate.admit(HONEST, key.clone(), 0, 1_000),
            RelayAdmission::Drop,
        );
        assert_eq!(gate.admit(MALICIOUS, key, 5, 1_000), RelayAdmission::Drop);
    }

    /// The improvement is strict, and it does not re-open the dedup gate: a
    /// duplicate at an equal or WORSE hop is still dropped. Without this a
    /// relay could re-forward the same identity indefinitely by replaying it.
    #[test]
    fn an_equal_or_worse_hop_duplicate_is_still_dropped() {
        let gate = ScopedAnnRelayGate::new();
        const PEER: u64 = 7;
        let key = key_n(2, 3);

        assert_eq!(
            gate.admit(PEER, key.clone(), 4, 1_000),
            RelayAdmission::Fresh
        );
        assert_eq!(
            gate.admit(PEER, key.clone(), 4, 1_000),
            RelayAdmission::Drop,
            "equal hop is not an improvement",
        );
        assert_eq!(
            gate.admit(PEER, key.clone(), 9, 1_000),
            RelayAdmission::Drop,
            "a worse hop is not an improvement",
        );
        assert_eq!(
            gate.admit(PEER, key, 3, 1_000),
            RelayAdmission::ShorterPath,
            "a strictly better hop is",
        );
    }

    fn provider_n(index: u64) -> EntityId {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&index.to_le_bytes());
        EntityId::from_bytes(bytes)
    }

    fn key_n(index: u64, generation: u64) -> RelayDedupKey {
        RelayDedupKey {
            provider: provider_n(index),
            grant_id: [0u8; 32],
            audience_handle: [0x11; 32],
            generation,
        }
    }

    #[test]
    fn frame_round_trips_and_preserves_hop() {
        let env = b"opaque-envelope-bytes";
        let framed = ScopedCapabilityRelayFrame::encode(3, env);
        let decoded = ScopedCapabilityRelayFrame::decode(&framed).expect("decode");
        assert_eq!(decoded.hop_count, 3);
        assert_eq!(decoded.envelope, env);
    }

    #[test]
    fn frame_decode_is_strict() {
        // Too short: version + hop but no envelope byte.
        assert!(
            ScopedCapabilityRelayFrame::decode(&[ScopedCapabilityRelayFrame::VERSION, 0]).is_none()
        );
        assert!(ScopedCapabilityRelayFrame::decode(&[]).is_none());
        // Wrong version.
        let mut framed = ScopedCapabilityRelayFrame::encode(0, b"x");
        framed[0] = ScopedCapabilityRelayFrame::VERSION.wrapping_add(1);
        assert!(ScopedCapabilityRelayFrame::decode(&framed).is_none());
    }

    /// An arbitrary authenticated ingress peer for the gate unit tests.
    const PEER: u64 = 0xA1;

    #[test]
    fn gate_admits_a_fresh_identity_once() {
        let gate = ScopedAnnRelayGate::new();
        assert!(
            gate.admit_fresh(PEER, key_n(1, 7), 1_000),
            "first sighting admits"
        );
        assert!(
            !gate.admit_fresh(PEER, key_n(1, 7), 1_000),
            "the identical identity is a duplicate"
        );
        // A different generation for the same provider is a distinct identity.
        assert!(
            gate.admit_fresh(PEER, key_n(1, 8), 1_000),
            "newer generation is fresh"
        );
        // A different provider is distinct too.
        assert!(
            gate.admit_fresh(PEER, key_n(2, 7), 1_000),
            "different provider is fresh"
        );
    }

    /// Dedup is GLOBAL, not per ingress peer: the same envelope arriving from a
    /// second adjacency is still a duplicate. Keying the gate on
    /// `(peer, identity)` would have been the easy way to bound per-peer
    /// occupancy, and it would have reopened exactly the flood amplification
    /// this gate exists to close — one relay per adjacency.
    #[test]
    fn dedup_is_global_across_ingress_peers() {
        let gate = ScopedAnnRelayGate::new();
        assert!(gate.admit_fresh(PEER, key_n(1, 7), 1_000));
        assert!(
            !gate.admit_fresh(PEER + 1, key_n(1, 7), 1_000),
            "a second peer delivering the SAME identity is still a duplicate"
        );
        assert_eq!(gate.len(), 1);
    }

    #[test]
    fn gate_expires_on_the_local_horizon() {
        let gate = ScopedAnnRelayGate::new();
        assert!(gate.admit_fresh(PEER, key_n(1, 7), 1_000));
        // Still within the retention horizon: a duplicate is dropped.
        assert!(!gate.admit_fresh(
            PEER,
            key_n(1, 7),
            1_000 + ScopedAnnRelayGate::RETENTION_SECS - 1
        ));
        // Past the horizon: the identity is fully forgotten and admissible again.
        assert!(gate.admit_fresh(
            PEER,
            key_n(1, 7),
            1_000 + ScopedAnnRelayGate::RETENTION_SECS
        ));
    }

    #[test]
    fn gate_is_bounded_fail_closed_and_never_evicts_active() {
        let gate = ScopedAnnRelayGate::new();
        // Fill to the GLOBAL cap. Spread across enough peers that no single one
        // hits its own budget first — the per-peer rule is exercised separately
        // below; this test is about the global bound.
        let per_peer = ScopedAnnRelayGate::MAX_ENTRIES_PER_PEER;
        for index in 0..ScopedAnnRelayGate::MAX_ENTRIES {
            let peer = (index / per_peer) as u64;
            assert!(gate.admit_fresh(peer, key_n(index as u64, 1), 1_000));
        }
        assert_eq!(gate.len(), ScopedAnnRelayGate::MAX_ENTRIES);
        // A brand-new identity at capacity is refused fail-closed — nothing is
        // evicted (every entry is in-horizon). Delivered by a FRESH peer, so
        // the refusal is the global cap and not a per-peer budget.
        assert!(
            !gate.admit_fresh(u64::MAX, key_n(u64::MAX, 1), 1_000),
            "an unseen identity is refused when full"
        );
        assert_eq!(gate.len(), ScopedAnnRelayGate::MAX_ENTRIES);
        // A duplicate of a still-active key stays a duplicate (it was NOT
        // evicted to admit the flood above).
        assert!(!gate.admit_fresh(0, key_n(0, 1), 1_000));
        // Once the horizon passes, the whole set is reclaimed and new identities
        // are admissible again.
        assert!(gate.admit_fresh(
            u64::MAX,
            key_n(u64::MAX, 1),
            1_000 + ScopedAnnRelayGate::RETENTION_SECS
        ));
    }

    /// §7 — one peer must not be able to blackhole scoped discovery.
    ///
    /// The gate is keyed on the envelope's SELF-DECLARED provider identity:
    /// `from_bytes` verifies the outer signature against whatever key the
    /// sender chose, `OrgMembershipCert::from_bytes` does not verify at all,
    /// and `hop_count > 0` skips the direct-origin bind. Relay identities are
    /// therefore free to mint. Combined with the fail-closed capacity rule,
    /// one session-authenticated peer could fill all 8192 slots for the full
    /// 600 s retention — and because `decide_scoped_relay` returning `None`
    /// suppresses the local ingest as well as the forward, that is a total
    /// scoped-discovery blackout, propagated mesh-wide by the fill-phase
    /// envelopes' own forwarding.
    ///
    /// Red-witness: deleting the per-peer budget check lets the flooder fill
    /// the gate and the honest peer's admit returns false.
    #[test]
    fn one_peer_cannot_starve_another_out_of_the_gate() {
        let gate = ScopedAnnRelayGate::new();
        const FLOODER: u64 = 0xBAD;
        const HONEST: u64 = 0x600D;

        // The flooder mints distinct identities as fast as it likes.
        let mut admitted = 0usize;
        for index in 0..ScopedAnnRelayGate::MAX_ENTRIES as u64 {
            if gate.admit_fresh(FLOODER, key_n(index, 1), 1_000) {
                admitted += 1;
            }
        }
        assert_eq!(
            admitted,
            ScopedAnnRelayGate::MAX_ENTRIES_PER_PEER,
            "a single peer is capped at its own budget",
        );
        assert!(
            gate.len() < ScopedAnnRelayGate::MAX_ENTRIES,
            "the flooder must not have consumed the whole gate",
        );

        // An honest peer's legitimate announcement still gets through.
        assert!(
            gate.admit_fresh(HONEST, key_n(u64::MAX, 1), 1_000),
            "an honest peer must still be admitted after a flood",
        );
    }

    /// The per-peer budget is RECLAIMED as entries age out — it is a live
    /// occupancy cap, not a lifetime quota. Without the sweep's bookkeeping a
    /// peer would be permanently throttled after one busy window.
    #[test]
    fn per_peer_budget_is_returned_when_entries_expire() {
        let gate = ScopedAnnRelayGate::new();
        const PEER_A: u64 = 7;
        for index in 0..ScopedAnnRelayGate::MAX_ENTRIES_PER_PEER as u64 {
            assert!(gate.admit_fresh(PEER_A, key_n(index, 1), 1_000));
        }
        // At budget: refused.
        assert!(!gate.admit_fresh(PEER_A, key_n(u64::MAX, 1), 1_000));

        // Past the horizon the slots return and the peer is admissible again.
        let later = 1_000 + ScopedAnnRelayGate::RETENTION_SECS;
        assert!(gate.admit_fresh(PEER_A, key_n(u64::MAX, 1), later));
        assert_eq!(gate.len(), 1, "the expired window was fully reclaimed");
    }

    // ---------------- decide_scoped_relay ----------------

    /// Build a valid owner-scoped relay frame at `hop` and return it with the
    /// provider's derived node id (the required ingress for a hop-0 frame).
    fn build_owner_frame(
        hop: u8,
        provider_seed: u8,
        generation: u64,
        expires_at: u64,
    ) -> (Vec<u8>, u64) {
        use crate::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
        use crate::adapter::net::behavior::org_authority::OwnerAudienceCredential;
        use crate::adapter::net::identity::EntityKeypair;

        let provider = EntityKeypair::from_bytes([provider_seed; 32]);
        let org = OrgKeypair::from_bytes([1u8; 32]);
        let credential = OwnerAudienceCredential::generate(org.org_id());
        let cert = OrgMembershipCert::issue_at(
            &org,
            provider.entity_id().clone(),
            5,
            0,
            1_000_000,
            0x1234,
        );
        let envelope = ScopedCapabilityAnnouncement::build_owner(
            &provider,
            org.org_id(),
            cert,
            credential.audience_handle,
            credential.discovery_key(),
            generation,
            expires_at,
            b"owner-descriptor",
        )
        .expect("build owner envelope");
        let provider_node = provider.entity_id().node_id();
        (
            ScopedCapabilityRelayFrame::encode(hop, &envelope.to_bytes()),
            provider_node,
        )
    }

    #[test]
    fn decide_relay_rejects_the_unresolved_from_node() {
        let gate = ScopedAnnRelayGate::new();
        let (frame, _) = build_owner_frame(1, 0x20, 1, 1_000_000);
        assert!(
            decide_scoped_relay(&frame, 0, &gate, 1_000).is_none(),
            "from_node == 0 (unresolved) is refused"
        );
        assert_eq!(gate.len(), 0, "a rejected frame never primes the gate");
        // The same frame from a real ingress is admitted (hop > 0 skips origin).
        assert!(decide_scoped_relay(&frame, 0xABCD, &gate, 1_000).is_some());
    }

    #[test]
    fn decide_relay_drops_a_malformed_frame_without_priming_the_gate() {
        let gate = ScopedAnnRelayGate::new();
        // Valid frame prefix, garbage envelope → outer verify (`from_bytes`) fails.
        let bad = ScopedCapabilityRelayFrame::encode(1, b"not-a-valid-envelope");
        assert!(decide_scoped_relay(&bad, 0xABCD, &gate, 1_000).is_none());
        // A bad version prefix is rejected at frame decode.
        assert!(decide_scoped_relay(&[0xFF, 0x00, 0x01], 0xABCD, &gate, 1_000).is_none());
        assert_eq!(
            gate.len(),
            0,
            "malformed / unsigned frames never prime the dedup gate"
        );
    }

    #[test]
    fn decide_relay_binds_the_direct_origin_at_hop_zero() {
        let gate = ScopedAnnRelayGate::new();
        let (frame, provider_node) = build_owner_frame(0, 0x21, 1, 1_000_000);
        // A hop-0 (direct) frame whose ingress is NOT the provider is refused.
        assert!(
            decide_scoped_relay(&frame, provider_node ^ 1, &gate, 1_000).is_none(),
            "a hop-0 frame from the wrong session peer is refused"
        );
        assert_eq!(gate.len(), 0);
        // The provider's own session admits it.
        assert!(decide_scoped_relay(&frame, provider_node, &gate, 1_000).is_some());
    }

    #[test]
    fn decide_relay_rejects_an_expired_envelope() {
        let gate = ScopedAnnRelayGate::new();
        let (frame, _) = build_owner_frame(1, 0x22, 1, 1_000);
        assert!(
            decide_scoped_relay(&frame, 0xABCD, &gate, 1_000 + RELAY_EXPIRY_SKEW_SECS + 1)
                .is_none(),
            "an envelope dead past the relay skew is dropped"
        );
        assert_eq!(gate.len(), 0);
    }

    #[test]
    fn decide_relay_admits_once_and_forwards_below_the_cap() {
        let gate = ScopedAnnRelayGate::new();
        let (frame, _) = build_owner_frame(1, 0x23, 1, 1_000_000);
        let admit = decide_scoped_relay(&frame, 0xABCD, &gate, 1_000).expect("admitted");
        let fwd = admit.forward.expect("forwarded below the cap");
        let decoded = ScopedCapabilityRelayFrame::decode(&fwd).expect("decode forward");
        assert_eq!(decoded.hop_count, 2, "hop incremented on forward");
        // The envelope bytes are preserved verbatim across the forward.
        let orig = ScopedCapabilityRelayFrame::decode(&frame).unwrap();
        assert_eq!(decoded.envelope, orig.envelope);
        // A re-flood of the identical outer identity is dropped — no second fanout.
        assert!(
            decide_scoped_relay(&frame, 0xABCD, &gate, 1_000).is_none(),
            "a duplicate frame does not re-forward"
        );
    }

    #[test]
    fn decide_relay_admits_but_does_not_forward_at_the_hop_boundary() {
        let gate = ScopedAnnRelayGate::new();
        // A frame already at the last forwardable hop is still admitted (so the
        // local node opens it) but yields NO forward frame.
        let (frame, _) = build_owner_frame(MAX_CAPABILITY_HOPS - 1, 0x24, 1, 1_000_000);
        let admit =
            decide_scoped_relay(&frame, 0xABCD, &gate, 1_000).expect("admitted at boundary");
        assert!(
            admit.forward.is_none(),
            "a frame at the hop boundary is not forwarded"
        );
    }
}
