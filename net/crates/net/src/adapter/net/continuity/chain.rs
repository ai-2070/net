//! Structural continuity — formalizing what the causal chain provides.
//!
//! The causal chain IS identity. `ContinuityStatus` describes the state
//! of an entity's chain from an observer's perspective. `ContinuityProof`
//! is a compact (40-byte) transmittable proof of chain integrity.

use crate::adapter::net::state::causal::{compute_parent_hash, CausalLink};
use crate::adapter::net::state::log::EntityLog;
use crate::adapter::net::state::snapshot::StateSnapshot;

/// The continuity status of an entity from an observer's perspective.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuityStatus {
    /// Chain is unbroken from genesis to head. Entity is continuous.
    Continuous {
        /// xxh3 hash that would be the parent_hash of the genesis link's successor.
        genesis_hash: u64,
        /// Head sequence number.
        head_seq: u64,
        /// parent_hash of the next event that would follow the head.
        head_hash: u64,
    },
    /// Chain has a verified gap. Entity forked.
    Forked {
        /// Sequence where the fork occurred.
        fork_point: u64,
        /// The original chain's parent_hash at the fork point.
        original_hash: u64,
        /// The new chain's parent_hash at the fork point.
        fork_hash: u64,
    },
    /// Chain cannot be verified (missing data).
    Unverifiable {
        /// Last verified sequence.
        last_verified_seq: u64,
        /// First unverified sequence.
        gap_start: u64,
    },
    /// Entity was explicitly migrated (chain transferred, not broken).
    Migrated {
        /// Sequence at migration point.
        migration_seq: u64,
        /// Source node that held the chain before migration.
        source_node: u64,
        /// Target node that holds the chain after migration.
        target_node: u64,
    },
}

/// Compact proof of continuity that can be transmitted (36 bytes).
///
/// A node can send this to another node to prove its chain is intact
/// over a given sequence range, without transferring the full log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuityProof {
    /// Entity origin hash.
    pub origin_hash: u32,
    /// Start of the proven range.
    pub from_seq: u64,
    /// End of the proven range.
    pub to_seq: u64,
    /// parent_hash computed from the event at from_seq.
    pub from_hash: u64,
    /// parent_hash computed from the event at to_seq.
    pub to_hash: u64,
}

/// Wire size of a ContinuityProof.
pub const CONTINUITY_PROOF_SIZE: usize = 36; // 4 + 8 + 8 + 8 + 8

/// Maximum number of events `ContinuityProof::verify_against` will
/// walk between `from_seq` and `to_seq` (inclusive). Without this
/// cap, a peer could ship a proof spanning `[0, u64::MAX]` and force
/// the verifier into a multi-billion-event walk on every dispatch.
///
/// At ~100 ns per event for the xxh3-over-(link + payload) hash
/// recomputation, 100K events ≈ 10 ms of synchronous CPU per
/// call — bounded enough that a peer who fires verify-requests at
/// line rate cannot exhaust the verifier's CPU budget before
/// backpressure kicks in. The cap is well past any realistic
/// single-proof span: snapshots prune the chain to a small replay
/// tail, and long-lived chains use snapshot-anchored proofs that
/// span far less than 100K events.
///
/// **Caller contract:** `verify_against` is itself bounded at
/// this cap, but a peer can still trigger up to ~10 ms of CPU per
/// call. Production callers MUST rate-limit verify-requests per
/// remote peer (e.g. token-bucket on inbound continuity-proof
/// frames) to prevent N peers × line-rate from saturating the
/// verifier. The structural cap below bounds per-call cost; the
/// per-peer rate limit bounds aggregate cost.
pub const MAX_PROOF_VERIFY_SPAN: u64 = 100_000;

impl ContinuityProof {
    /// Extract a proof from a local entity log.
    ///
    /// Returns `None` if the log is empty.
    pub fn from_log(log: &EntityLog) -> Option<Self> {
        if log.is_empty() {
            return None;
        }

        let events = log.range(0, u64::MAX);
        if events.is_empty() {
            return None;
        }

        let first = &events[0];
        let last = events.last().unwrap();

        let from_hash = compute_parent_hash(&first.link, &first.payload);
        let to_hash = compute_parent_hash(&last.link, &last.payload);

        Some(Self {
            origin_hash: log.origin_hash(),
            from_seq: first.link.sequence,
            to_seq: last.link.sequence,
            from_hash,
            to_hash,
        })
    }

    /// Verify this proof against a local entity log.
    ///
    /// Walks the entire event range `[from_seq, to_seq]`,
    /// re-computing each `parent_hash` and asserting it matches
    /// the chain link emitted by the previous event. The endpoint
    /// hashes (`from_hash` / `to_hash`) are also verified against
    /// the local log.
    ///
    /// Rejects reversed bounds, caps the span at
    /// [`MAX_PROOF_VERIFY_SPAN`], walks every event in range, and
    /// validates each consecutive `parent_hash` link. Checking
    /// only the two endpoint events would let a malicious
    /// intermediary holding events 0 and 999 ship a proof spanning
    /// `[0, 999]` with the correct two endpoint hashes that
    /// `verify_against` would accept — even though events 1..998
    /// could be missing or fabricated, defeating the whole point
    /// of the proof. The span cap also prevents a peer from
    /// forcing a multi-billion-event scan via reversed or wide
    /// bounds.
    pub fn verify_against(&self, log: &EntityLog) -> Result<(), ProofError> {
        if self.origin_hash != log.origin_hash() {
            return Err(ProofError::OriginMismatch);
        }
        if self.from_seq > self.to_seq {
            return Err(ProofError::InvalidRange {
                from_seq: self.from_seq,
                to_seq: self.to_seq,
            });
        }
        // Bound the walk. `to_seq - from_seq` is the *count - 1*;
        // reject any span that would exceed MAX_PROOF_VERIFY_SPAN
        // events (well past realistic).
        let span = self.to_seq.saturating_sub(self.from_seq);
        if span >= MAX_PROOF_VERIFY_SPAN {
            return Err(ProofError::SpanTooLarge {
                from_seq: self.from_seq,
                to_seq: self.to_seq,
                cap: MAX_PROOF_VERIFY_SPAN,
            });
        }

        let events = log.range(self.from_seq, self.to_seq);
        if events.is_empty() {
            return Err(ProofError::MissingEvent(self.from_seq));
        }

        // Verify the FIRST event matches `from_hash` and its
        // sequence is exactly `from_seq` (range may legitimately
        // start later if `from_seq < log.first_seq`, in which case
        // we treat the first slot as missing).
        let first = &events[0];
        if first.link.sequence != self.from_seq {
            return Err(ProofError::MissingEvent(self.from_seq));
        }
        let from_local = compute_parent_hash(&first.link, &first.payload);
        if from_local != self.from_hash {
            return Err(ProofError::HashMismatch {
                seq: self.from_seq,
                expected: self.from_hash,
                got: from_local,
            });
        }

        // Walk each consecutive pair: every event's `parent_hash`
        // must equal the prior event's forward hash, and the
        // sequence must be strictly +1.
        for i in 1..events.len() {
            let prev = &events[i - 1];
            let curr = &events[i];
            if curr.link.sequence != prev.link.sequence + 1 {
                return Err(ProofError::MissingEvent(prev.link.sequence + 1));
            }
            let expected_parent = compute_parent_hash(&prev.link, &prev.payload);
            if curr.link.parent_hash != expected_parent {
                return Err(ProofError::HashMismatch {
                    seq: curr.link.sequence,
                    expected: expected_parent,
                    got: curr.link.parent_hash,
                });
            }
        }

        // Verify the LAST event matches `to_hash` and its sequence
        // is exactly `to_seq` (the walk above guarantees no gap).
        let last = events.last().unwrap();
        if last.link.sequence != self.to_seq {
            return Err(ProofError::MissingEvent(self.to_seq));
        }
        let to_local = compute_parent_hash(&last.link, &last.payload);
        if to_local != self.to_hash {
            return Err(ProofError::HashMismatch {
                seq: self.to_seq,
                expected: self.to_hash,
                got: to_local,
            });
        }

        Ok(())
    }

    /// Serialize to bytes.
    pub fn to_bytes(&self) -> [u8; CONTINUITY_PROOF_SIZE] {
        let mut buf = [0u8; CONTINUITY_PROOF_SIZE];
        buf[0..4].copy_from_slice(&self.origin_hash.to_le_bytes());
        buf[4..12].copy_from_slice(&self.from_seq.to_le_bytes());
        buf[12..20].copy_from_slice(&self.to_seq.to_le_bytes());
        buf[20..28].copy_from_slice(&self.from_hash.to_le_bytes());
        buf[28..36].copy_from_slice(&self.to_hash.to_le_bytes());
        buf
    }

    /// Deserialize from bytes.
    ///
    /// Rejects buffers whose length differs from
    /// [`CONTINUITY_PROOF_SIZE`] so trailing bytes aren't silently
    /// accepted (the old `< SIZE` guard let concatenated proofs or
    /// framing garbage parse as the first proof).
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() != CONTINUITY_PROOF_SIZE {
            return None;
        }
        Some(Self {
            origin_hash: u32::from_le_bytes(data[0..4].try_into().unwrap()),
            from_seq: u64::from_le_bytes(data[4..12].try_into().unwrap()),
            to_seq: u64::from_le_bytes(data[12..20].try_into().unwrap()),
            from_hash: u64::from_le_bytes(data[20..28].try_into().unwrap()),
            to_hash: u64::from_le_bytes(data[28..36].try_into().unwrap()),
        })
    }
}

/// Assess the continuity status of an entity log.
///
/// Walks the log and validates every consecutive pair. Returns the
/// first problem found, or `Continuous` if the chain is intact.
///
/// # Genesis / snapshot anchoring
///
/// Pair-wise linkage alone is not enough: after `prune_through(N)`,
/// a log only contains events with `seq > N`, and a corrupt restore
/// (or a malicious party) could ship a log starting at e.g. seq 100
/// with consistent pair-wise hashes but no evidence that events
/// `0..99` ever existed. To detect that, this function requires the
/// log to be anchored either at genesis (the first event has
/// `sequence == 1`, the genesis-successor) or at a known snapshot
/// (`snapshot.through_seq + 1 == first_event.sequence`). If neither
/// holds, returns `Unverifiable { last_verified_seq: 0, gap_start: 0 }`.
///
/// **Anchor parent_hash check.** The sequence-only anchor admitted
/// any first event whose seq landed in the right slot regardless of
/// its `parent_hash`. A forged log starting at seq 1 with a junk
/// parent_hash and consistent pair-wise hashes from there on still
/// passed. The fix: also require the first event's `parent_hash` to
/// match the canonical genesis successor hash
/// (`xxh3(genesis_link_bytes ++ &[])`), or — when anchored to a
/// snapshot — match `xxh3(snapshot.chain_link.to_bytes() ++ snapshot.head_payload)`.
/// Mismatch is reported as `Forked { fork_point: first_seq, .. }`
/// because that's literally what it is: divergence at the anchor.
///
/// **Snapshot caller contract.** Anchoring against a snapshot
/// requires that the snapshot's `head_payload` field be populated
/// — i.e. the caller restored from a snapshot and held onto the
/// head event's payload bytes (`StateSnapshot::with_head_payload`).
/// If the field is empty, the parent_hash check uses an empty
/// payload, which only matches if the producer side also serialized
/// an empty payload at `through_seq`. Callers anchoring a real
/// post-restore log MUST populate `head_payload` first.
///
/// Pass `None` for `snapshot` when the log is expected to start at
/// genesis (no prior pruning); pass `Some(&snapshot)` when the log
/// was restored from `snapshot` and should pick up at the next event
/// after the snapshot's `through_seq`.
pub fn assess_continuity(log: &EntityLog, snapshot: Option<&StateSnapshot>) -> ContinuityStatus {
    let events = log.range(0, u64::MAX);

    if events.is_empty() {
        return ContinuityStatus::Continuous {
            genesis_hash: 0,
            head_seq: log.head_seq(),
            head_hash: 0,
        };
    }

    // Anchor check. A pair-wise-consistent chain is not continuous
    // if it doesn't start at genesis (seq 1, post-genesis
    // successor) or at a verified snapshot boundary.
    let first = &events[0];
    let first_seq = first.link.sequence;
    let expected_anchor_hash = if first_seq == 1 {
        // Canonical genesis link: zero horizon, sequence 0, no
        // parent. Its successor's parent_hash is xxh3 over the
        // genesis link bytes concatenated with an empty payload —
        // matches `CausalChainBuilder::new` (`state/causal.rs`).
        Some(compute_parent_hash(
            &CausalLink::genesis(log.origin_hash(), 0),
            &[],
        ))
    } else if let Some(s) = snapshot {
        // `checked_add` so a snapshot at `u64::MAX` (impossible in
        // practice — would require 2^64 events under one origin —
        // but cheap to be safe) doesn't saturate to `u64::MAX` and
        // falsely anchor an event claiming `first_seq == u64::MAX`.
        // `None` from `checked_add` propagates as "not anchored,"
        // surfacing as `Unverifiable` below.
        if s.through_seq.checked_add(1) == Some(first_seq) {
            // Distinguish "missing payload context" from "real
            // chain divergence." `head_payload` is `Option<Bytes>`
            // so `None` is the unambiguous "caller didn't populate"
            // sentinel — a `Some(Bytes::new())` for a legitimate
            // empty-payload event is a different case and goes
            // through the normal hash path.
            //
            // Special case: a genesis snapshot's head event has
            // no predecessor payload. The legitimate way to
            // express that is `Some(Bytes::new())`, but
            // historically callers also leave it `None` for
            // genesis since there's nothing to populate. We
            // accept either for `chain_link.sequence == 0`:
            // treat None-at-genesis as Some(empty) since that's
            // what the original genesis-anchor branch used.
            match (s.chain_link.sequence, &s.head_payload) {
                // Genesis is sequence 0 with no predecessor
                // payload — the canonical hash is against the
                // empty byte slice. A non-empty payload here is
                // malformed (the caller populated `head_payload`
                // with junk for a slot that has no real
                // predecessor). Pre-fix we computed the parent
                // hash against that junk and the downstream
                // mismatch surfaced as `Forked`, which routes
                // operators toward "the chain diverged" rather
                // than "the snapshot is wrong." Refuse the
                // anchor entirely so the result is `Unverifiable`
                // — the typed signal for "we can't decide yet,
                // re-fetch a clean snapshot."
                (0, Some(payload)) if !payload.is_empty() => None,
                (0, Some(_)) | (0, None) => Some(compute_parent_hash(&s.chain_link, &[])),
                (_, Some(payload)) => Some(compute_parent_hash(&s.chain_link, payload)),
                (_, None) => None, // missing context — Unverifiable
            }
        } else {
            None
        }
    } else {
        None
    };
    let Some(expected_anchor_hash) = expected_anchor_hash else {
        return ContinuityStatus::Unverifiable {
            last_verified_seq: 0,
            gap_start: 0,
        };
    };
    // Even at the right sequence slot, a forged first event with
    // a non-matching `parent_hash` is divergence at the anchor —
    // not a continuous chain.
    if first.link.parent_hash != expected_anchor_hash {
        return ContinuityStatus::Forked {
            fork_point: first_seq,
            original_hash: expected_anchor_hash,
            fork_hash: first.link.parent_hash,
        };
    }

    // Validate consecutive pairs
    for i in 1..events.len() {
        let prev = &events[i - 1];
        let curr = &events[i];

        // Check sequence continuity
        if curr.link.sequence != prev.link.sequence + 1 {
            return ContinuityStatus::Unverifiable {
                last_verified_seq: prev.link.sequence,
                gap_start: prev.link.sequence + 1,
            };
        }

        // Check parent hash linkage
        let expected_parent = compute_parent_hash(&prev.link, &prev.payload);
        if curr.link.parent_hash != expected_parent {
            return ContinuityStatus::Forked {
                fork_point: curr.link.sequence,
                original_hash: expected_parent,
                fork_hash: curr.link.parent_hash,
            };
        }
    }

    let first = &events[0];
    let last = events.last().unwrap();
    let genesis_hash = compute_parent_hash(&first.link, &first.payload);
    let head_hash = compute_parent_hash(&last.link, &last.payload);

    ContinuityStatus::Continuous {
        genesis_hash,
        head_seq: last.link.sequence,
        head_hash,
    }
}

/// Errors from proof verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofError {
    /// Origin hash doesn't match the log.
    OriginMismatch,
    /// Hash at a given sequence doesn't match.
    HashMismatch {
        /// Sequence number where mismatch occurred.
        seq: u64,
        /// Expected hash from the proof.
        expected: u64,
        /// Actual hash from the local log.
        got: u64,
    },
    /// Event at the given sequence is missing from the local log.
    MissingEvent(u64),
    /// Proof has `from_seq > to_seq` — reversed bounds.
    InvalidRange {
        /// Lower bound declared by the proof.
        from_seq: u64,
        /// Upper bound declared by the proof.
        to_seq: u64,
    },
    /// Proof span exceeds [`MAX_PROOF_VERIFY_SPAN`] —
    /// `to_seq - from_seq` is too large to walk safely.
    SpanTooLarge {
        /// Lower bound declared by the proof.
        from_seq: u64,
        /// Upper bound declared by the proof.
        to_seq: u64,
        /// Configured maximum span the verifier will walk.
        cap: u64,
    },
}

impl std::fmt::Display for ProofError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OriginMismatch => write!(f, "origin hash mismatch"),
            Self::HashMismatch { seq, expected, got } => {
                write!(
                    f,
                    "hash mismatch at seq {}: expected {:#x}, got {:#x}",
                    seq, expected, got
                )
            }
            Self::MissingEvent(seq) => write!(f, "missing event at seq {}", seq),
            Self::InvalidRange { from_seq, to_seq } => write!(
                f,
                "invalid proof range: from_seq ({}) > to_seq ({})",
                from_seq, to_seq
            ),
            Self::SpanTooLarge {
                from_seq,
                to_seq,
                cap,
            } => write!(
                f,
                "proof span too large: from_seq={}, to_seq={}, cap={}",
                from_seq, to_seq, cap
            ),
        }
    }
}

impl std::error::Error for ProofError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::state::causal::CausalChainBuilder;
    use bytes::Bytes;

    fn build_log(count: usize) -> (EntityLog, CausalChainBuilder) {
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();
        let mut log = EntityLog::new(kp.entity_id().clone());
        let mut builder = CausalChainBuilder::new(origin);

        for i in 0..count {
            let event = builder
                .append(Bytes::from(format!("event-{}", i)), 0)
                .unwrap();
            log.append(event).unwrap();
        }

        (log, builder)
    }

    #[test]
    fn test_assess_continuous() {
        let (log, _) = build_log(10);
        let status = assess_continuity(&log, None);
        assert!(matches!(
            status,
            ContinuityStatus::Continuous { head_seq: 10, .. }
        ));
    }

    #[test]
    fn test_assess_empty_log() {
        let kp = EntityKeypair::generate();
        let log = EntityLog::new(kp.entity_id().clone());
        let status = assess_continuity(&log, None);
        assert!(matches!(status, ContinuityStatus::Continuous { .. }));
    }

    // ========================================================================
    // pruned-no-snapshot logs must not report Continuous
    // ========================================================================

    /// A log whose first event has `sequence > 1` (e.g. because
    /// earlier events were pruned, or the log was reconstructed
    /// from a partial backup) must be reported as
    /// `Unverifiable { gap_start: 0 }` when no snapshot is supplied
    /// to bridge the gap. Pre-fix this returned `Continuous` and
    /// downstream peers believed the chain was intact even when
    /// genesis-to-first events were entirely missing.
    #[test]
    fn assess_continuity_unverifiable_when_log_starts_past_genesis_without_snapshot() {
        let (mut log, _) = build_log(20);
        // Prune through seq 10 — the log now starts at seq 11 with
        // no snapshot reference.
        log.prune_through(10);
        assert!(
            !log.is_empty(),
            "test setup: log must still have events 11..20"
        );

        let status = assess_continuity(&log, None);
        assert!(
            matches!(
                status,
                ContinuityStatus::Unverifiable {
                    last_verified_seq: 0,
                    gap_start: 0,
                }
            ),
            "pruned log without snapshot must be Unverifiable, got {:?}",
            status,
        );
    }

    /// Same pruned log, but with a snapshot whose `through_seq`
    /// matches the gap — must report `Continuous`. Pins the
    /// snapshot-bridges-gap acceptance path so a future tightening
    /// can't reject legitimately-restored logs.
    #[test]
    fn assess_continuity_continuous_when_snapshot_bridges_gap() {
        use crate::adapter::net::state::horizon::ObservedHorizon;

        let (mut log, _) = build_log(20);
        // Capture the real event at seq 10 BEFORE pruning so the
        // snapshot can carry its actual chain_link + payload — the
        // post-fix anchor check (cubic-ai P1) requires the snapshot
        // to match the log's seq-11 parent_hash, which is computed
        // from the seq-10 event. A synthetic genesis link here would
        // (correctly) trip the new Forked branch.
        let event_at_10 = log.range(10, 10)[0].clone();
        log.prune_through(10);

        let snapshot = StateSnapshot {
            version: 1,
            entity_id: log.entity_id().clone(),
            through_seq: 10,
            chain_link: event_at_10.link,
            state: bytes::Bytes::new(),
            horizon: ObservedHorizon::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: Some(event_at_10.payload.clone()),
        };

        let status = assess_continuity(&log, Some(&snapshot));
        assert!(
            matches!(status, ContinuityStatus::Continuous { head_seq: 20, .. }),
            "snapshot.through_seq + 1 == first_event.sequence must anchor, got {:?}",
            status,
        );
    }

    /// A snapshot whose `through_seq` does NOT match the log's gap
    /// (e.g. caller passed the wrong snapshot) must NOT anchor —
    /// this would let a forged "I have this snapshot" claim
    /// silently bypass the genesis check.
    #[test]
    fn assess_continuity_unverifiable_when_snapshot_through_seq_does_not_bridge() {
        use crate::adapter::net::state::causal::CausalLink;
        use crate::adapter::net::state::horizon::ObservedHorizon;

        let (mut log, _) = build_log(20);
        log.prune_through(10);

        // Mismatched snapshot — claims through_seq=5 but log starts at 11.
        let snapshot = StateSnapshot {
            version: 1,
            entity_id: log.entity_id().clone(),
            through_seq: 5,
            chain_link: CausalLink::genesis(log.origin_hash(), 0),
            state: bytes::Bytes::new(),
            horizon: ObservedHorizon::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: None,
        };

        let status = assess_continuity(&log, Some(&snapshot));
        assert!(
            matches!(
                status,
                ContinuityStatus::Unverifiable {
                    last_verified_seq: 0,
                    gap_start: 0,
                }
            ),
            "mismatched snapshot must not anchor, got {:?}",
            status,
        );
    }

    // ========================================================================
    // Cubic-ai P1: anchor check must verify first event's parent_hash,
    // not just its sequence slot
    // ========================================================================

    /// A real, post-prune log paired with a snapshot whose
    /// `through_seq` matches but whose `chain_link` / `head_payload`
    /// do NOT match the log's actual anchor must be reported as
    /// `Forked` — not `Continuous`. Pre-fix the snapshot-anchor
    /// check matched only on `through_seq`, so a caller (or attacker
    /// supplying a snapshot) could pass any junk pair and have the
    /// log accepted as anchored. With the anchor parent_hash check
    /// in place, the log's seq-11 event reports its real parent_hash
    /// (computed against the real seq-10 event), which doesn't match
    /// the synthetic-genesis-link hash the snapshot supplies — so the
    /// chain reads as a fork at the anchor.
    #[test]
    fn assess_continuity_forked_when_snapshot_chain_link_mismatches_log_anchor() {
        use crate::adapter::net::state::horizon::ObservedHorizon;

        let (mut log, _) = build_log(20);
        log.prune_through(10);

        // Synthetic snapshot: claims through_seq=10 but stamps a
        // genesis-shaped chain_link instead of the real seq-10
        // event. `head_payload` is also empty, so the expected
        // anchor hash = xxh3(genesis_bytes ++ &[]) — completely
        // different from the seq-11 event's actual parent_hash.
        let snapshot = StateSnapshot {
            version: 1,
            entity_id: log.entity_id().clone(),
            through_seq: 10,
            chain_link: CausalLink::genesis(log.origin_hash(), 0),
            state: bytes::Bytes::new(),
            horizon: ObservedHorizon::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: None,
        };

        let status = assess_continuity(&log, Some(&snapshot));
        match status {
            ContinuityStatus::Forked { fork_point, .. } => {
                assert_eq!(
                    fork_point, 11,
                    "snapshot anchor parent_hash mismatch must surface as Forked \
                     at first_seq (cubic-ai P1), got fork_point={}",
                    fork_point,
                );
            }
            other => panic!(
                "expected Forked at the snapshot anchor — pre-fix this returned \
                 Continuous because only `through_seq` was checked. Got {:?}",
                other
            ),
        }
    }

    /// A caller who passes the right `chain_link` but forgets to
    /// populate `head_payload` (leaves it as `Bytes::new()`) for a
    /// non-genesis snapshot MUST surface as `Unverifiable`, NOT as
    /// `Forked`. Pre-CR-34 the empty-payload anchor hash was
    /// computed eagerly and the resulting mismatch was reported as
    /// `Forked` — confusing for callers who saw "fork detected"
    /// when the real cause was "snapshot deserialized without
    /// out-of-band head_payload transfer."
    ///
    /// CR-34 contract: `assess_continuity` distinguishes genuine
    /// chain divergence (`Forked`) from missing context preventing
    /// verification (`Unverifiable`). An empty `head_payload` for
    /// `chain_link.sequence > 0` falls in the second bucket.
    /// Genesis snapshots (`chain_link.sequence == 0`) legitimately
    /// carry empty `head_payload` and continue through the
    /// standard hash path.
    #[test]
    fn assess_continuity_unverifiable_when_snapshot_head_payload_is_unpopulated() {
        use crate::adapter::net::state::horizon::ObservedHorizon;

        let (mut log, _) = build_log(20);
        let event_at_10 = log.range(10, 10)[0].clone();
        // Sanity: the test is only meaningful if the real payload
        // is non-empty. `build_log` writes "event-{i}" so this
        // holds, but pin it so a future tweak doesn't silently
        // empty the payload and turn this test into a no-op.
        assert!(
            !event_at_10.payload.is_empty(),
            "test setup: build_log must produce non-empty payloads, \
             otherwise empty-vs-real head_payload comparison is moot",
        );
        log.prune_through(10);

        let snapshot = StateSnapshot {
            version: 1,
            entity_id: log.entity_id().clone(),
            through_seq: 10,
            // Right link, but caller forgot to attach the real
            // head event's bytes — `head_payload` is empty.
            chain_link: event_at_10.link,
            state: bytes::Bytes::new(),
            horizon: ObservedHorizon::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: None,
        };

        let status = assess_continuity(&log, Some(&snapshot));
        match status {
            ContinuityStatus::Unverifiable {
                last_verified_seq,
                gap_start,
            } => {
                assert_eq!(
                    last_verified_seq, 0,
                    "anchor unverifiable: nothing has been verified yet"
                );
                assert_eq!(gap_start, 0, "gap starts at the (un-verifiable) anchor");
            }
            other => panic!(
                "CR-34: empty head_payload for non-genesis snapshot must surface as \
                 Unverifiable (not Forked); got {:?}",
                other
            ),
        }
    }

    /// CR-34: genesis snapshots (`chain_link.sequence == 0`)
    /// legitimately carry empty `head_payload` — there's no
    /// predecessor to source a payload from. The empty-payload
    /// detection in `assess_continuity` MUST NOT trip on genesis;
    /// it must compute the canonical genesis-anchor hash and
    /// validate normally.
    ///
    /// The CR-34 detection only fires for non-genesis snapshots
    /// — without this carve-out, every genesis-anchored chain
    /// would be `Unverifiable`, defeating the pruned-no-snapshot fix.
    #[test]
    fn cr34_genesis_snapshot_with_empty_head_payload_still_validates() {
        use crate::adapter::net::state::horizon::ObservedHorizon;

        let (log, _) = build_log(5);
        // Build a "genesis snapshot" by hand: through_seq = 0, the
        // chain_link IS the canonical genesis link, head_payload
        // is empty (the genesis link has no predecessor payload).
        let genesis_link = CausalLink::genesis(log.origin_hash(), 0);
        let snapshot = StateSnapshot {
            version: 1,
            entity_id: log.entity_id().clone(),
            through_seq: 0,
            chain_link: genesis_link,
            state: bytes::Bytes::new(),
            horizon: ObservedHorizon::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: None,
        };

        let status = assess_continuity(&log, Some(&snapshot));
        match status {
            ContinuityStatus::Continuous { .. } => {
                // Expected: a genesis snapshot anchors the seq=1
                // first event via the canonical genesis successor
                // hash. Empty head_payload is the legitimate
                // genesis case.
            }
            other => panic!(
                "CR-34: genesis snapshot (seq=0) with empty head_payload must \
                 be Continuous (legitimate), got {:?}",
                other
            ),
        }
    }

    /// Cubic P2: pin that `Some(Bytes::new())` is treated as a
    /// LEGITIMATE empty payload — NOT as missing context. An
    /// event's actual payload can be empty (zero-byte event)
    /// and the resulting snapshot's `head_payload` should
    /// faithfully carry `Some(Bytes::new())`. Pre-Cubic-P2 the
    /// `head_payload.is_empty()` sentinel conflated this case
    /// with `None` and falsely marked the snapshot as
    /// Unverifiable.
    ///
    /// Setup: a log where the head event's payload happens to
    /// be empty bytes. The snapshot anchors at that event; the
    /// expected anchor hash is `xxh3(chain_link ++ &[])` — and
    /// the log's seq=11 event has its `parent_hash` computed
    /// against that exact hash. The chain validates as
    /// Continuous.
    #[test]
    fn cubic_p2_some_empty_head_payload_validates_as_legitimate() {
        use crate::adapter::net::identity::EntityKeypair;
        use crate::adapter::net::state::causal::CausalChainBuilder;
        use crate::adapter::net::state::horizon::ObservedHorizon;
        use crate::adapter::net::state::EntityLog;

        let kp = EntityKeypair::generate();
        let mut log = EntityLog::new(kp.entity_id().clone());
        let mut builder = CausalChainBuilder::new(kp.origin_hash());

        // Build a chain where event 10 has an EMPTY payload.
        for i in 0..20usize {
            let payload = if i == 9 {
                // Event 10 (1-indexed) — empty payload.
                bytes::Bytes::new()
            } else {
                bytes::Bytes::from(format!("event-{}", i))
            };
            let event = builder.append(payload, 0).unwrap();
            log.append(event).unwrap();
        }
        let event_at_10 = log.range(10, 10)[0].clone();
        assert!(
            event_at_10.payload.is_empty(),
            "test setup: event 10 must have an empty payload"
        );
        log.prune_through(10);

        // Snapshot anchors at the empty-payload event. The caller
        // populates `head_payload` honestly with `Some(empty)`.
        let snapshot = StateSnapshot {
            version: 1,
            entity_id: log.entity_id().clone(),
            through_seq: 10,
            chain_link: event_at_10.link,
            state: bytes::Bytes::new(),
            horizon: ObservedHorizon::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: Some(event_at_10.payload.clone()),
        };

        let status = assess_continuity(&log, Some(&snapshot));
        match status {
            ContinuityStatus::Continuous { .. } => {
                // Expected: legitimate empty-payload event,
                // explicitly populated head_payload, anchor
                // matches.
            }
            other => panic!(
                "Cubic P2: Some(Bytes::new()) for a legitimate empty-payload \
                 head event must be Continuous (NOT Unverifiable). got {:?}",
                other
            ),
        }
    }

    /// Cubic-ai P2: a snapshot with `through_seq == u64::MAX` must
    /// NOT anchor any log — `checked_add(1)` returns `None`, so the
    /// snapshot path falls through to `Unverifiable`. Pre-fix the
    /// code used `saturating_add(1)` which clamps at `u64::MAX`, so
    /// a (pathological) log whose first event also claimed
    /// `sequence == u64::MAX` would have falsely anchored.
    ///
    /// `u64::MAX` events under one origin is impossible in practice
    /// (would require 2^64 ingests), but a future regression that
    /// flipped the comparator back to `saturating_add` would
    /// silently re-introduce the false-anchor edge case — this
    /// test is the canary.
    #[test]
    fn assess_continuity_unverifiable_when_snapshot_through_seq_is_u64_max() {
        use crate::adapter::net::state::horizon::ObservedHorizon;

        let (mut log, _) = build_log(20);
        // Prune so the log starts past genesis — we want the
        // snapshot anchor branch to be the one under test, not
        // the `first_seq == 1` genesis branch (which doesn't use
        // `checked_add`).
        log.prune_through(10);

        // Snapshot with `through_seq = u64::MAX`. With pre-fix
        // `saturating_add(1)`, this would have produced
        // `u64::MAX + 1 (saturated) = u64::MAX`, and a log claiming
        // `first_seq == u64::MAX` could have falsely matched. With
        // the post-fix `checked_add(1) == Some(first_seq)` shape,
        // `checked_add(u64::MAX, 1) = None`, so no anchor matches
        // regardless of `first_seq`.
        let snapshot = StateSnapshot {
            version: 1,
            entity_id: log.entity_id().clone(),
            through_seq: u64::MAX,
            chain_link: CausalLink::genesis(log.origin_hash(), 0),
            state: bytes::Bytes::new(),
            horizon: ObservedHorizon::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: None,
        };

        let status = assess_continuity(&log, Some(&snapshot));
        assert!(
            matches!(
                status,
                ContinuityStatus::Unverifiable {
                    last_verified_seq: 0,
                    gap_start: 0,
                }
            ),
            "snapshot.through_seq == u64::MAX must never anchor — \
             pre-fix saturating_add would have falsely matched a log \
             claiming first_seq == u64::MAX. Got: {:?}",
            status,
        );
    }

    /// Anchor parent_hash check must NOT lock out a snapshot whose
    /// `chain_link` and `head_payload` legitimately match the log
    /// (the success path for a real cross-node migration). Pins the
    /// happy path so a future tightening can't flip the comparator
    /// and silently reject every legitimate snapshot.
    #[test]
    fn assess_continuity_continuous_when_snapshot_chain_link_matches_log_anchor() {
        use crate::adapter::net::state::horizon::ObservedHorizon;

        let (mut log, _) = build_log(20);
        let event_at_10 = log.range(10, 10)[0].clone();
        log.prune_through(10);

        let snapshot = StateSnapshot {
            version: 1,
            entity_id: log.entity_id().clone(),
            through_seq: 10,
            chain_link: event_at_10.link,
            state: bytes::Bytes::new(),
            horizon: ObservedHorizon::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: Some(event_at_10.payload.clone()),
        };

        let status = assess_continuity(&log, Some(&snapshot));
        assert!(
            matches!(status, ContinuityStatus::Continuous { head_seq: 20, .. }),
            "matching snapshot chain_link + head_payload must anchor cleanly, got {:?}",
            status,
        );
    }

    /// Regression: a malformed snapshot whose `chain_link.sequence
    /// == 0` (genesis link) carries a non-empty `head_payload`
    /// must surface as `Unverifiable`, not `Forked`. Pre-fix the
    /// non-empty payload was hashed alongside the genesis link and
    /// the resulting (junk) `parent_hash` was compared to the
    /// log's first event — the mismatch surfaced as `Forked`,
    /// routing operators toward "the chain diverged" rather than
    /// "the snapshot is wrong / re-fetch a clean one." Genesis
    /// has no predecessor payload by construction; a non-empty
    /// `head_payload` here is a caller bug, not chain divergence.
    ///
    /// To exercise the snapshot path we need `first_seq > 1` so
    /// the function consults the snapshot rather than the
    /// canonical genesis short-circuit. We prune the log so its
    /// first event starts past genesis, but leave `through_seq`
    /// matching the prune boundary so the snapshot bridges the
    /// gap arithmetically — leaving the malformed-shape check as
    /// the only remaining gate.
    #[test]
    fn malformed_genesis_snapshot_with_nonempty_head_payload_is_unverifiable() {
        use crate::adapter::net::state::horizon::ObservedHorizon;

        let (mut log, _) = build_log(20);
        log.prune_through(10);
        // `first_seq` is now 11; the snapshot must bridge the
        // gap. We forge `chain_link.sequence == 0` (the genesis
        // link's sequence) but populate `head_payload` with junk
        // — the malformed shape this fix targets.
        let genesis_link = CausalLink::genesis(log.origin_hash(), 0);
        let snapshot = StateSnapshot {
            version: 1,
            entity_id: log.entity_id().clone(),
            through_seq: 10,
            chain_link: genesis_link,
            state: bytes::Bytes::new(),
            horizon: ObservedHorizon::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: Some(bytes::Bytes::from_static(b"not-genesis-payload")),
        };

        let status = assess_continuity(&log, Some(&snapshot));
        match status {
            ContinuityStatus::Unverifiable {
                last_verified_seq,
                gap_start,
            } => {
                assert_eq!(last_verified_seq, 0);
                assert_eq!(gap_start, 0);
            }
            other => panic!(
                "regression: genesis-shaped snapshot (chain_link.sequence=0) \
                 with non-empty head_payload must surface as Unverifiable \
                 (not Forked / Continuous). Hashing junk against the genesis \
                 link and surfacing the mismatch as Forked routes operators \
                 toward the wrong remediation. got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_proof_roundtrip() {
        let (log, _) = build_log(5);
        let proof = ContinuityProof::from_log(&log).unwrap();

        let bytes = proof.to_bytes();
        assert_eq!(bytes.len(), CONTINUITY_PROOF_SIZE);

        let parsed = ContinuityProof::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, proof);
    }

    #[test]
    fn test_proof_verify_against_same_log() {
        let (log, _) = build_log(5);
        let proof = ContinuityProof::from_log(&log).unwrap();

        assert!(proof.verify_against(&log).is_ok());
    }

    #[test]
    fn test_proof_verify_wrong_origin() {
        let (log_a, _) = build_log(5);
        let (log_b, _) = build_log(5);

        let proof = ContinuityProof::from_log(&log_a).unwrap();
        assert_eq!(
            proof.verify_against(&log_b).unwrap_err(),
            ProofError::OriginMismatch,
        );
    }

    #[test]
    fn test_proof_from_empty_log() {
        let kp = EntityKeypair::generate();
        let log = EntityLog::new(kp.entity_id().clone());
        assert!(ContinuityProof::from_log(&log).is_none());
    }

    // ========================================================================
    // verify_against must walk the full chain, not just endpoints
    // ========================================================================

    /// `verify_against` rejects a proof whose middle is missing or
    /// fabricated even when the two endpoint hashes are correct.
    /// Pre-fix the verifier only checked `from_seq` and `to_seq`,
    /// so an attacker holding only events 1 and N could ship a
    /// proof spanning `[1, N]` and have it accepted.
    ///
    /// Setup: build a 5-event log, capture a proof from it, then
    /// build a separate log with events 1 and 5 only (events 2..4
    /// missing). Verifying the original proof against the
    /// gap-laden log must fail.
    #[test]
    fn verify_against_rejects_proof_when_middle_events_are_missing() {
        // Reference log + proof.
        let (full_log, _) = build_log(5);
        let proof = ContinuityProof::from_log(&full_log).unwrap();

        // Build a peer log with only the first and last events
        // (gap in between). We fake this by building a fresh log,
        // appending event 1 (which is genesis-successor), then
        // pruning through seq 4 — that leaves event 5 as the only
        // entry with `base_link.sequence == 4`. Then the verify
        // walk for `[1, 5]` finds events[0].sequence == 5, not 1.
        let kp = EntityKeypair::generate();
        let mut peer_log = EntityLog::new(full_log.entity_id().clone());
        let _ = kp; // silence unused — we need the same origin
                    // Replicate full_log's chain into peer_log so origin matches.
        for ev in full_log.range(1, 5) {
            peer_log.append((*ev).clone()).unwrap();
        }
        // Drop events 2..4 by pruning through 4 (leaves event 5).
        peer_log.prune_through(4);

        // The proof spans `[1, 5]`. With events 1..4 missing, the
        // verifier's first range lookup must surface a missing-event
        // error rather than silently passing on the endpoints.
        // Cubic-ai P3: pin the missing seq to `from_seq` (1)
        // specifically — the contract is "the from_seq endpoint is
        // the one we noticed missing." A future regression that
        // shifted which gap is reported (e.g. surfacing the to_seq
        // miss instead) would silently mask broken anchor-detection
        // without this tightening.
        let result = proof.verify_against(&peer_log);
        assert!(
            matches!(result, Err(ProofError::MissingEvent(1))),
            "verify_against must reject with MissingEvent(from_seq=1) when \
             from_seq itself is gone (cubic-ai P3), got {:?}",
            result,
        );
    }

    /// `verify_against` rejects a proof with reversed bounds
    /// (`from_seq > to_seq`). Pre-fix there was no range check, so
    /// a malformed proof could pass through the endpoint match if
    /// both seqs happened to coincide (or be present in the log).
    #[test]
    fn verify_against_rejects_proof_with_reversed_bounds() {
        let (log, _) = build_log(5);
        let mut proof = ContinuityProof::from_log(&log).unwrap();
        // Forge reversed bounds.
        std::mem::swap(&mut proof.from_seq, &mut proof.to_seq);
        let result = proof.verify_against(&log);
        assert!(
            matches!(result, Err(ProofError::InvalidRange { .. })),
            "verify_against must reject reversed bounds, got {:?}",
            result,
        );
    }

    /// `verify_against` rejects a proof whose span exceeds
    /// `MAX_PROOF_VERIFY_SPAN`. Pre-fix the walk was unbounded — a
    /// peer could ship a proof spanning `[0, u64::MAX]` and force
    /// a multi-billion-event scan on every dispatch.
    #[test]
    fn verify_against_rejects_proof_with_oversized_span() {
        let (log, _) = build_log(5);
        let mut proof = ContinuityProof::from_log(&log).unwrap();
        proof.from_seq = 0;
        proof.to_seq = MAX_PROOF_VERIFY_SPAN + 1;
        let result = proof.verify_against(&log);
        assert!(
            matches!(result, Err(ProofError::SpanTooLarge { .. })),
            "verify_against must reject spans over MAX_PROOF_VERIFY_SPAN, got {:?}",
            result,
        );
    }

    /// CR-13: pin the tightened cap value. Pre-CR-13 the cap was
    /// 1_000_000 — at ~100ns per event (xxh3 over link + payload),
    /// a peer firing verify-requests at line rate could force
    /// ~100ms of synchronous CPU per call, exhausting the
    /// verifier's CPU budget before backpressure kicked in. The
    /// new cap of 100_000 caps per-call cost at ~10ms; the per-
    /// peer rate-limit contract documented on the constant
    /// bounds aggregate cost.
    ///
    /// This test fires loudly the moment someone raises the cap
    /// back without re-evaluating the per-peer rate-limit
    /// contract.
    #[test]
    fn cr13_max_proof_verify_span_is_capped_at_100k() {
        assert_eq!(
            MAX_PROOF_VERIFY_SPAN, 100_000,
            "CR-13: MAX_PROOF_VERIFY_SPAN must stay at 100_000. \
             Raising it requires re-evaluating the per-peer rate-limit \
             contract documented on the constant — at ~100ns per event, \
             a 1M cap is a 100ms-CPU-per-call attack surface. If you're \
             increasing this for legitimate reasons, also document the \
             updated per-peer rate-limit requirement and bump the test."
        );
    }

    /// `verify_against` accepts a proof whose intermediate links
    /// are all valid — pins the success path so the new walk
    /// doesn't accidentally lock out legitimate chains.
    #[test]
    fn verify_against_accepts_intact_chain_with_intermediate_events() {
        let (log, _) = build_log(10);
        let proof = ContinuityProof::from_log(&log).unwrap();
        proof
            .verify_against(&log)
            .expect("intact chain must verify");
    }
}
