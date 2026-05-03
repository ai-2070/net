//! Log reconciliation after partition healing.
//!
//! Merges divergent EntityLogs from two sides of a partition using
//! CausalLink chain verification. Longest-chain-wins for conflicts,
//! with deterministic tiebreak. Losing chains become ForkRecords.

use crate::adapter::net::continuity::discontinuity::{fork_entity, ForkRecord};
use crate::adapter::net::state::causal::{validate_chain_link, CausalEvent, ChainError};
use crate::adapter::net::state::log::EntityLog;

/// Which side of the partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Our side (local).
    Ours,
    /// Their side (remote).
    Theirs,
}

/// How a conflict was resolved.
#[derive(Debug, Clone)]
pub enum ConflictResolution {
    /// One side wins (longer chain or deterministic tiebreak).
    Winner {
        /// Which side won.
        winning_side: Side,
        /// Fork record for the losing chain.
        fork_record: ForkRecord,
    },
}

/// Outcome of reconciling one entity's logs.
#[derive(Debug, Clone)]
pub enum ReconcileOutcome {
    /// Logs are identical or one is a strict prefix — no action needed.
    AlreadyConverged,
    /// One side has events the other doesn't, no conflict.
    Catchup {
        /// Entity origin hash.
        origin_hash: u32,
        /// Events to replay on the behind side.
        missing_events: Vec<CausalEvent>,
        /// Which side needs the events.
        behind_side: Side,
    },
    /// Both sides produced events during the split.
    Conflict {
        /// Entity origin hash.
        origin_hash: u32,
        /// Sequence where divergence begins.
        diverge_seq: u64,
        /// Resolution applied.
        resolution: ConflictResolution,
    },
}

/// Reconcile a single entity's log against remote events.
///
/// `our_log`: local EntityLog.
/// `their_events`: events from the remote side.
/// `split_seq`: sequence number at the partition point (from horizon snapshot).
///
/// Returns `Err` if `their_events` fail chain validation (broken parent-hash
/// linkage). Callers should treat this as a protocol violation from the remote.
pub fn reconcile_entity(
    our_log: &EntityLog,
    their_events: &[CausalEvent],
    split_seq: u64,
) -> Result<ReconcileOutcome, ChainError> {
    // Validate the remote chain before trusting it for reconciliation.
    // Pass the local origin AND the expected first-sequence so a
    // remote cannot win a "longest chain" conflict by submitting a
    // well-formed chain anchored to a fabricated origin_hash or
    // starting at an arbitrary sequence unrelated to our split.
    verify_remote_chain(our_log.origin_hash(), their_events, Some(split_seq))?;

    let our_events = our_log.after(split_seq);

    // Both sides empty after split — converged
    if our_events.is_empty() && their_events.is_empty() {
        return Ok(ReconcileOutcome::AlreadyConverged);
    }

    // Only one side has events — catchup
    if our_events.is_empty() {
        return Ok(ReconcileOutcome::Catchup {
            origin_hash: our_log.origin_hash(),
            missing_events: their_events.to_vec(),
            behind_side: Side::Ours,
        });
    }

    if their_events.is_empty() {
        return Ok(ReconcileOutcome::Catchup {
            origin_hash: our_log.origin_hash(),
            missing_events: our_events.into_iter().cloned().collect(),
            behind_side: Side::Theirs,
        });
    }

    // Both sides have events — find divergence point
    let mut diverge_idx = None;
    let min_len = our_events.len().min(their_events.len());

    for i in 0..min_len {
        if our_events[i].link.parent_hash != their_events[i].link.parent_hash
            || our_events[i].link.sequence != their_events[i].link.sequence
        {
            diverge_idx = Some(i);
            break;
        }
        // Same link — check payload too
        if our_events[i].payload != their_events[i].payload {
            diverge_idx = Some(i);
            break;
        }
    }

    Ok(match diverge_idx {
        None if our_events.len() == their_events.len() => {
            // Identical chains
            ReconcileOutcome::AlreadyConverged
        }
        None if our_events.len() > their_events.len() => {
            // Our chain is longer — they need catchup
            let missing: Vec<CausalEvent> = our_events[their_events.len()..]
                .iter()
                .map(|e| (*e).clone())
                .collect();
            ReconcileOutcome::Catchup {
                origin_hash: our_log.origin_hash(),
                missing_events: missing,
                behind_side: Side::Theirs,
            }
        }
        None => {
            // Their chain is longer — we need catchup
            let missing: Vec<CausalEvent> = their_events[our_events.len()..].to_vec();
            ReconcileOutcome::Catchup {
                origin_hash: our_log.origin_hash(),
                missing_events: missing,
                behind_side: Side::Ours,
            }
        }
        Some(idx) => {
            // Conflict at idx
            let diverge_seq = if idx < our_events.len() {
                our_events[idx].link.sequence
            } else {
                their_events[idx].link.sequence
            };

            let our_len = our_events.len() - idx;
            let their_len = their_events.len() - idx;

            // Longest chain wins. Tie: lower payload hash, then
            // lexicographic on the divergent payload bytes, then
            // on `(parent_hash, sequence)`. The trailing
            // `(parent_hash, sequence)` tier is what lets us
            // resolve the case where divergence at idx came from
            // a `parent_hash` / `sequence` mismatch but the
            // payloads happen to be byte-identical (e.g. after
            // pruning + re-issue of the same payload under a
            // different parent). Pre-fix that case fell into
            // `Ordering::Equal` and panicked the reconciliation
            // worker via `unreachable!()` — a malformed-but-
            // signed remote chain crashed the worker thread. Now
            // we fall through to a deterministic tiebreak on the
            // chain-link metadata that's *guaranteed* to differ
            // (the divergence detection above only enters this
            // branch when at least one of those bytes differs or
            // the payloads do).
            let winning_side = if our_len > their_len {
                Side::Ours
            } else if their_len > our_len {
                Side::Theirs
            } else {
                let our_payload = &our_events[idx].payload;
                let their_payload = &their_events[idx].payload;
                let our_hash = xxhash_rust::xxh3::xxh3_64(our_payload);
                let their_hash = xxhash_rust::xxh3::xxh3_64(their_payload);
                let our_link = &our_events[idx].link;
                let their_link = &their_events[idx].link;
                use std::cmp::Ordering::{Equal, Greater, Less};
                match our_hash
                    .cmp(&their_hash)
                    .then_with(|| our_payload.as_ref().cmp(their_payload.as_ref()))
                    .then_with(|| our_link.parent_hash.cmp(&their_link.parent_hash))
                    .then_with(|| our_link.sequence.cmp(&their_link.sequence))
                {
                    Less => Side::Ours,
                    Greater => Side::Theirs,
                    Equal => {
                        // True equality across payload + link
                        // metadata means the divergence detection
                        // promoted us into this branch on byte-
                        // identical events — likely a future
                        // detection-contract change. Pick a
                        // deterministic side rather than
                        // panicking; logging is the diagnostic
                        // signal. The choice is `Side::Ours`
                        // arbitrarily (peer A and peer B both
                        // pick "their own" side, which surfaces
                        // as `AlreadyConverged` on the next
                        // round once both forks merge).
                        tracing::warn!(
                            idx,
                            origin_hash = our_log.origin_hash(),
                            "reconcile_entity: divergence detected but events \
                             are byte-identical across payload + link metadata. \
                             Selecting Side::Ours arbitrarily; investigate \
                             the divergence detector."
                        );
                        Side::Ours
                    }
                }
            };

            // Fork the losing chain
            let origin_hash = our_log.origin_hash();
            let (_, fork_record, _) = fork_entity(origin_hash, diverge_seq, None);

            ReconcileOutcome::Conflict {
                origin_hash,
                diverge_seq,
                resolution: ConflictResolution::Winner {
                    winning_side,
                    fork_record,
                },
            }
        }
    })
}

/// Validate that a sequence of remote events forms a valid chain anchored
/// to `expected_origin`.
///
/// Checks:
/// - the first event's `origin_hash` matches `expected_origin` (so an
///   attacker cannot submit an internally well-formed chain for a
///   fabricated entity and have it win a "longest chain" conflict),
/// - when `expected_first_seq` is `Some(split_seq)`: the first event's
///   sequence is exactly `split_seq + 1`, so a remote cannot submit
///   a chain starting at an arbitrary sequence unrelated to our split
///   point (zip-wise divergence detection in `reconcile_entity`
///   compares index-by-index, so mismatched starting sequences would
///   otherwise produce bogus "conflict" outcomes), and
/// - parent_hash / sequence / origin linkage between consecutive events.
///
/// Returns `Ok(())` on an empty slice (nothing to trust).
pub fn verify_remote_chain(
    expected_origin: u32,
    events: &[CausalEvent],
    expected_first_seq: Option<u64>,
) -> Result<(), ChainError> {
    if let Some(first) = events.first() {
        if first.link.origin_hash != expected_origin {
            return Err(ChainError::OriginMismatch {
                expected: expected_origin,
                got: first.link.origin_hash,
            });
        }
        if let Some(split_seq) = expected_first_seq {
            // `checked_add(1)` rather than `saturating_add(1)`: at
            // `split_seq == u64::MAX` there's no legitimate next
            // sequence — the chain is already at the 64-bit ceiling,
            // so any remote claiming to extend it is malformed by
            // construction. Saturating would silently accept
            // `first.link.sequence == u64::MAX` as a continuation of
            // a chain that already hit the end (cubic code review P3).
            let expected = split_seq.checked_add(1).ok_or(ChainError::SequenceGap {
                expected: u64::MAX,
                got: first.link.sequence,
            })?;
            if first.link.sequence != expected {
                return Err(ChainError::SequenceGap {
                    expected,
                    got: first.link.sequence,
                });
            }
        }
    }
    for i in 1..events.len() {
        validate_chain_link(&events[i - 1].link, &events[i - 1].payload, &events[i].link)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::state::causal::CausalChainBuilder;
    use bytes::Bytes;

    fn build_divergent_logs(
        shared_events: usize,
        our_extra: usize,
        their_extra: usize,
    ) -> (EntityLog, Vec<CausalEvent>, u64) {
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();
        let mut log = EntityLog::new(kp.entity_id().clone());
        let mut builder = CausalChainBuilder::new(origin);

        // Shared prefix
        for i in 0..shared_events {
            let event = builder
                .append(Bytes::from(format!("shared-{}", i)), 0)
                .unwrap();
            log.append(event).unwrap();
        }

        let split_seq = builder.sequence();

        // Our side continues
        let mut our_builder = CausalChainBuilder::from_head(
            *builder.head(),
            Bytes::from(format!("shared-{}", shared_events - 1)),
        );
        for i in 0..our_extra {
            let event = our_builder
                .append(Bytes::from(format!("ours-{}", i)), 0)
                .unwrap();
            log.append(event).unwrap();
        }

        // Their side diverges from the same point
        let mut their_builder = CausalChainBuilder::from_head(
            *builder.head(),
            Bytes::from(format!("shared-{}", shared_events - 1)),
        );
        let mut their_events = Vec::new();
        for i in 0..their_extra {
            let event = their_builder
                .append(Bytes::from(format!("theirs-{}", i)), 0)
                .unwrap();
            their_events.push(event);
        }

        (log, their_events, split_seq)
    }

    #[test]
    fn test_already_converged() {
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();
        let mut log = EntityLog::new(kp.entity_id().clone());
        let mut builder = CausalChainBuilder::new(origin);

        for i in 0..5 {
            let event = builder.append(Bytes::from(format!("e{}", i)), 0).unwrap();
            log.append(event).unwrap();
        }

        // No events after split
        let result = reconcile_entity(&log, &[], 5).unwrap();
        assert!(matches!(result, ReconcileOutcome::AlreadyConverged));
    }

    #[test]
    fn test_catchup_we_are_behind() {
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();
        let log = EntityLog::new(kp.entity_id().clone());
        let mut builder = CausalChainBuilder::new(origin);

        // They have events, we don't
        let their_events: Vec<CausalEvent> = (0..3)
            .map(|i| {
                builder
                    .append(Bytes::from(format!("theirs-{}", i)), 0)
                    .unwrap()
            })
            .collect();

        let result = reconcile_entity(&log, &their_events, 0).unwrap();
        match result {
            ReconcileOutcome::Catchup {
                behind_side,
                missing_events,
                ..
            } => {
                assert_eq!(behind_side, Side::Ours);
                assert_eq!(missing_events.len(), 3);
            }
            other => panic!("expected Catchup, got {:?}", other),
        }
    }

    #[test]
    fn test_catchup_they_are_behind() {
        let (log, _, split_seq) = build_divergent_logs(3, 2, 0);

        let result = reconcile_entity(&log, &[], split_seq).unwrap();
        match result {
            ReconcileOutcome::Catchup {
                behind_side,
                missing_events,
                ..
            } => {
                assert_eq!(behind_side, Side::Theirs);
                assert_eq!(missing_events.len(), 2);
            }
            other => panic!("expected Catchup, got {:?}", other),
        }
    }

    #[test]
    fn test_conflict_longest_wins() {
        let (log, their_events, split_seq) = build_divergent_logs(3, 5, 2);

        let result = reconcile_entity(&log, &their_events, split_seq).unwrap();
        match result {
            ReconcileOutcome::Conflict {
                resolution:
                    ConflictResolution::Winner {
                        winning_side,
                        fork_record,
                    },
                ..
            } => {
                assert_eq!(winning_side, Side::Ours); // 5 > 2
                assert!(fork_record.verify());
            }
            other => panic!("expected Conflict, got {:?}", other),
        }
    }

    #[test]
    fn test_conflict_they_win() {
        let (log, their_events, split_seq) = build_divergent_logs(3, 1, 4);

        let result = reconcile_entity(&log, &their_events, split_seq).unwrap();
        match result {
            ReconcileOutcome::Conflict {
                resolution: ConflictResolution::Winner { winning_side, .. },
                ..
            } => {
                assert_eq!(winning_side, Side::Theirs); // 4 > 1
            }
            other => panic!("expected Conflict, got {:?}", other),
        }
    }

    #[test]
    fn test_conflict_tiebreak_deterministic() {
        // Equal length chains — deterministic tiebreak on parent_hash
        let (log, their_events, split_seq) = build_divergent_logs(3, 2, 2);

        let result = reconcile_entity(&log, &their_events, split_seq).unwrap();
        assert!(matches!(
            result,
            ReconcileOutcome::Conflict {
                resolution: ConflictResolution::Winner { .. },
                ..
            }
        ));
    }

    #[test]
    fn test_verify_remote_chain_valid() {
        let kp = EntityKeypair::generate();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());

        let events: Vec<CausalEvent> = (0..5)
            .map(|i| builder.append(Bytes::from(format!("e{}", i)), 0).unwrap())
            .collect();

        assert!(verify_remote_chain(kp.origin_hash(), &events, None).is_ok());
    }

    #[test]
    fn test_verify_remote_chain_broken() {
        let kp = EntityKeypair::generate();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());

        let mut events: Vec<CausalEvent> = (0..3)
            .map(|i| builder.append(Bytes::from(format!("e{}", i)), 0).unwrap())
            .collect();

        // Tamper with the middle event
        events[1].link.parent_hash = 0xBADBADBAD;

        assert!(verify_remote_chain(kp.origin_hash(), &events, None).is_err());
    }

    #[test]
    fn test_verify_remote_chain_empty_is_ok() {
        // Nothing to trust, nothing to reject.
        assert!(verify_remote_chain(0xDEADBEEF, &[], None).is_ok());
    }

    #[test]
    fn test_regression_verify_remote_chain_rejects_wrong_start_sequence() {
        // Regression (LOW, BUGS.md): `verify_remote_chain` validated
        // that the remote chain was internally linked but never
        // checked that it actually started at the agreed split point.
        // A remote could submit a well-formed chain anchored at an
        // arbitrary sequence, and `reconcile_entity`'s index-by-index
        // divergence detection would produce a bogus conflict outcome.
        //
        // Fix: pass `Some(split_seq)` and require the first event's
        // `sequence` to equal `split_seq + 1`.
        let kp = EntityKeypair::generate();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        // Build a chain covering seqs 1..=10.
        let events: Vec<CausalEvent> = (0..10)
            .map(|i| builder.append(Bytes::from(format!("e{i}")), 0).unwrap())
            .collect();
        // Take the tail starting at seq 5 (so the remote-submitted
        // chain "starts at" seq 5).
        let tail: Vec<CausalEvent> = events[4..].to_vec();

        // Split seq was 0 (so expected first is seq 1). The remote
        // starts at seq 5 — must be rejected.
        let rejected = verify_remote_chain(kp.origin_hash(), &tail, Some(0));
        assert!(
            matches!(rejected, Err(ChainError::SequenceGap { .. })),
            "remote chain starting at seq 5 must be rejected when split_seq = 0",
        );

        // With matching expected (split_seq = 4 → expect first = 5),
        // the same chain is accepted.
        assert!(
            verify_remote_chain(kp.origin_hash(), &tail, Some(4)).is_ok(),
            "remote chain starting at seq 5 must be accepted when split_seq = 4",
        );
    }

    // ---- Regression tests for Cubic AI findings ----

    #[test]
    fn test_regression_tiebreak_perspective_independent() {
        // Regression: tiebreak used parent_hash, which is identical on both
        // sides of a divergence (both diverge from the same parent). Each
        // side would declare itself the winner. Now uses payload hash.
        let (log, their_events, split_seq) = build_divergent_logs(3, 2, 2);

        let result = reconcile_entity(&log, &their_events, split_seq).unwrap();

        // The result must be a conflict with a winner
        let winning_side = match &result {
            ReconcileOutcome::Conflict {
                resolution: ConflictResolution::Winner { winning_side, .. },
                ..
            } => *winning_side,
            other => panic!("expected Conflict, got {:?}", other),
        };

        // Now simulate the OTHER side's perspective: they have their_events
        // in their log, and our post-split events are "theirs"
        let our_post_split: Vec<CausalEvent> =
            log.after(split_seq).iter().map(|e| (*e).clone()).collect();

        // Build the other side's log
        let _kp = EntityKeypair::from_bytes([0x42u8; 32]); // deterministic for both sides
        let origin = log.origin_hash();
        let mut their_log = EntityLog::new(log.entity_id().clone());
        let mut builder = CausalChainBuilder::new(origin);

        // Replay shared prefix
        for i in 0..3 {
            let event = builder
                .append(Bytes::from(format!("shared-{}", i)), 0)
                .unwrap();
            their_log.append(event).unwrap();
        }

        // Replay their divergent events
        let _their_builder =
            CausalChainBuilder::from_head(*builder.head(), Bytes::from("shared-2".to_string()));
        for event in &their_events {
            their_log.append(event.clone()).unwrap();
        }

        let other_result = reconcile_entity(&their_log, &our_post_split, split_seq).unwrap();

        let other_winning_side = match &other_result {
            ReconcileOutcome::Conflict {
                resolution: ConflictResolution::Winner { winning_side, .. },
                ..
            } => *winning_side,
            other => panic!("expected Conflict from other side, got {:?}", other),
        };

        // Both sides must agree on the same winner (from their own perspective)
        // If we say "Ours wins", they must say "Theirs wins" (= us), and vice versa.
        assert_ne!(
            winning_side, other_winning_side,
            "both sides must agree: if we say Ours, they must say Theirs"
        );
    }

    #[test]
    fn test_regression_reconcile_rejects_broken_remote_chain() {
        // Regression: reconcile_entity accepted remote events without
        // validating chain integrity. A malicious or corrupted remote
        // could send events with broken parent-hash linkage, and the
        // reconciliation logic would trust them — potentially accepting
        // a forged chain as the winner in a conflict.
        //
        // Fix: reconcile_entity now calls verify_remote_chain() before
        // processing. Broken chains return Err(ChainError).
        let kp = EntityKeypair::generate();
        let log = EntityLog::new(kp.entity_id().clone());
        let mut builder = CausalChainBuilder::new(kp.origin_hash());

        let mut their_events: Vec<CausalEvent> = (0..3)
            .map(|i| builder.append(Bytes::from(format!("e{}", i)), 0).unwrap())
            .collect();

        // Tamper with the chain — break parent-hash linkage
        their_events[2].link.parent_hash = 0xDEADBEEF;

        let result = reconcile_entity(&log, &their_events, 0);
        assert!(
            result.is_err(),
            "reconcile_entity must reject events with broken chain linkage"
        );
    }

    #[test]
    fn test_regression_verify_remote_chain_rejects_origin_forgery() {
        // Regression: verify_remote_chain only validated internal linkage
        // (i in 1..len), so the first event's origin_hash was never checked
        // against the local entity's origin_hash. A remote could submit an
        // internally well-formed chain for a fabricated origin, and, because
        // `reconcile_entity` uses longest-chain-wins, win the conflict —
        // effectively replacing our entity's history.
        //
        // Fix: verify_remote_chain now takes an expected_origin and rejects
        // chains whose first event does not match.
        let ours = EntityKeypair::generate();
        let theirs = EntityKeypair::generate();
        assert_ne!(
            ours.origin_hash(),
            theirs.origin_hash(),
            "precondition: distinct origin hashes"
        );

        // Build an internally-consistent chain anchored to a *different* origin.
        let mut builder = CausalChainBuilder::new(theirs.origin_hash());
        let forged: Vec<CausalEvent> = (0..5)
            .map(|i| {
                builder
                    .append(Bytes::from(format!("forged-{}", i)), 0)
                    .unwrap()
            })
            .collect();

        // Chain itself is internally valid under `theirs`...
        assert!(verify_remote_chain(theirs.origin_hash(), &forged, None).is_ok());

        // ...but must be rejected when we claim it belongs to `ours`.
        let rejected = verify_remote_chain(ours.origin_hash(), &forged, None);
        assert!(
            matches!(rejected, Err(ChainError::OriginMismatch { .. })),
            "verify_remote_chain must reject a chain whose first event's origin \
             doesn't match the expected origin, got {:?}",
            rejected
        );
    }

    #[test]
    fn test_regression_reconcile_rejects_foreign_origin_chain() {
        // Integration: reconcile_entity must refuse a chain that is internally
        // valid but anchored to a foreign origin_hash. Without this, a longer
        // foreign chain would win the longest-chain tiebreak and be accepted
        // as our entity's truth.
        let ours = EntityKeypair::generate();
        let theirs = EntityKeypair::generate();

        let our_log = EntityLog::new(ours.entity_id().clone());

        let mut foreign_builder = CausalChainBuilder::new(theirs.origin_hash());
        let foreign_events: Vec<CausalEvent> = (0..10)
            .map(|i| {
                foreign_builder
                    .append(Bytes::from(format!("foreign-{}", i)), 0)
                    .unwrap()
            })
            .collect();

        let result = reconcile_entity(&our_log, &foreign_events, 0);
        assert!(
            matches!(result, Err(ChainError::OriginMismatch { .. })),
            "reconcile_entity must reject chains anchored to a foreign origin, got {:?}",
            result
        );
    }
}
