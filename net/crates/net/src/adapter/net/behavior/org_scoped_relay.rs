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
///   outage (§7 — see [`Self::MAX_ENTRIES_PER_PEER`]).
#[derive(Default)]
pub struct ScopedAnnRelayGate {
    inner: Mutex<RelayGateInner>,
}

/// One remembered relay identity.
#[derive(Debug)]
struct SeenEntry {
    /// Local retention deadline (unix secs).
    deadline: u64,
    /// The authenticated ingress peer whose frame first admitted this identity.
    /// Held so the sweep can return the slot to that peer's budget.
    admitted_by: u64,
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
    pub fn admit(&self, from_node: u64, key: RelayDedupKey, now_secs: u64) -> bool {
        let mut inner = self.inner.lock();
        inner.sweep(now_secs);
        if inner.seen.contains_key(&key) {
            return false; // duplicate — already relayed this outer identity
        }
        // Per-peer budget FIRST, so a flooding peer exhausts its own share
        // before it can apply any pressure to the global cap.
        if inner.per_peer.get(&from_node).copied().unwrap_or(0) >= Self::MAX_ENTRIES_PER_PEER {
            return false;
        }
        if inner.seen.len() >= Self::MAX_ENTRIES {
            // FAIL-CLOSED at capacity: never evict an active seen-key to make
            // room, or a re-flooded duplicate for that key would be admitted
            // again and reflood. The horizon sweep above is the only reclaim.
            return false;
        }
        inner.seen.insert(
            key,
            SeenEntry {
                deadline: now_secs.saturating_add(Self::RETENTION_SECS),
                admitted_by: from_node,
            },
        );
        *inner.per_peer.entry(from_node).or_insert(0) += 1;
        true
    }

    /// Number of tracked (currently-remembered) relay identities. Used by the
    /// module unit tests and the live relay witness's `_for_test` seam to
    /// confirm a relay ADMITTED (and thus forwarded) an envelope it can't store.
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().seen.len()
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
    if !gate.admit(from_node, key, now_secs) {
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
    Some(ScopedRelayAdmit { envelope, forward })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            gate.admit(PEER, key_n(1, 7), 1_000),
            "first sighting admits"
        );
        assert!(
            !gate.admit(PEER, key_n(1, 7), 1_000),
            "the identical identity is a duplicate"
        );
        // A different generation for the same provider is a distinct identity.
        assert!(
            gate.admit(PEER, key_n(1, 8), 1_000),
            "newer generation is fresh"
        );
        // A different provider is distinct too.
        assert!(
            gate.admit(PEER, key_n(2, 7), 1_000),
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
        assert!(gate.admit(PEER, key_n(1, 7), 1_000));
        assert!(
            !gate.admit(PEER + 1, key_n(1, 7), 1_000),
            "a second peer delivering the SAME identity is still a duplicate"
        );
        assert_eq!(gate.len(), 1);
    }

    #[test]
    fn gate_expires_on_the_local_horizon() {
        let gate = ScopedAnnRelayGate::new();
        assert!(gate.admit(PEER, key_n(1, 7), 1_000));
        // Still within the retention horizon: a duplicate is dropped.
        assert!(!gate.admit(
            PEER,
            key_n(1, 7),
            1_000 + ScopedAnnRelayGate::RETENTION_SECS - 1
        ));
        // Past the horizon: the identity is fully forgotten and admissible again.
        assert!(gate.admit(
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
            assert!(gate.admit(peer, key_n(index as u64, 1), 1_000));
        }
        assert_eq!(gate.len(), ScopedAnnRelayGate::MAX_ENTRIES);
        // A brand-new identity at capacity is refused fail-closed — nothing is
        // evicted (every entry is in-horizon). Delivered by a FRESH peer, so
        // the refusal is the global cap and not a per-peer budget.
        assert!(
            !gate.admit(u64::MAX, key_n(u64::MAX, 1), 1_000),
            "an unseen identity is refused when full"
        );
        assert_eq!(gate.len(), ScopedAnnRelayGate::MAX_ENTRIES);
        // A duplicate of a still-active key stays a duplicate (it was NOT
        // evicted to admit the flood above).
        assert!(!gate.admit(0, key_n(0, 1), 1_000));
        // Once the horizon passes, the whole set is reclaimed and new identities
        // are admissible again.
        assert!(gate.admit(
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
            if gate.admit(FLOODER, key_n(index, 1), 1_000) {
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
            gate.admit(HONEST, key_n(u64::MAX, 1), 1_000),
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
            assert!(gate.admit(PEER_A, key_n(index, 1), 1_000));
        }
        // At budget: refused.
        assert!(!gate.admit(PEER_A, key_n(u64::MAX, 1), 1_000));

        // Past the horizon the slots return and the peer is admissible again.
        let later = 1_000 + ScopedAnnRelayGate::RETENTION_SECS;
        assert!(gate.admit(PEER_A, key_n(u64::MAX, 1), later));
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
        let credential = OwnerAudienceCredential::generate();
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
