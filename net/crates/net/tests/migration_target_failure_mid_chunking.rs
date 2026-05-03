//! Regression: daemon migration must recover cleanly when the
//! target fails mid-chunking of a multi-chunk snapshot.
//!
//! Covered invariants:
//!
//! 1. Partially-delivered chunks held by the target-side
//!    [`SnapshotReassembler`] can be cancelled without panicking,
//!    and the reassembler's `latest_seq` bookkeeping is preserved
//!    so stale chunk replays stay rejected.
//! 2. Late straggler chunks for a cancelled migration do not
//!    panic the reassembler; they either land against fresh state
//!    or are rejected as stale.
//! 3. The orchestrator's state machine does not deadlock on a
//!    mid-chunk target failure — `abort_migration_with_reason`
//!    removes the record and a fresh migration to a *different*
//!    target is accepted immediately.
//! 4. The chunked `SnapshotReady` output of `chunk_snapshot` for
//!    the restarted migration is intact (index coverage, no holes).

#![cfg(feature = "net")]

use std::sync::Arc;

use bytes::Bytes;
use net::adapter::net::behavior::capability::CapabilityFilter;
use net::adapter::net::compute::{
    chunk_snapshot, DaemonError, DaemonHost, DaemonHostConfig, DaemonRegistry, MeshDaemon,
    MigrationFailureReason, MigrationMessage, MigrationOrchestrator, MigrationPhase,
    SnapshotReassembler, MAX_SNAPSHOT_CHUNK_SIZE,
};
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::state::causal::CausalEvent;

/// A daemon whose snapshot grows proportionally to `payload_size`
/// so the migration path is forced onto the multi-chunk code path.
struct BulkyDaemon {
    count: u64,
    payload_size: usize,
}

impl MeshDaemon for BulkyDaemon {
    fn name(&self) -> &str {
        "bulky"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        self.count += 1;
        Ok(vec![])
    }
    fn snapshot(&self) -> Option<Bytes> {
        // 8 bytes of counter, then `payload_size` bytes of pattern
        // data. The pattern makes it easy for the test to eyeball
        // rehydrated contents if assertions ever need it.
        let mut out = Vec::with_capacity(8 + self.payload_size);
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend((0..self.payload_size).map(|i| (i & 0xFF) as u8));
        Some(Bytes::from(out))
    }
    fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
        if state.len() < 8 {
            return Err(DaemonError::RestoreFailed("bad state size".into()));
        }
        self.count = u64::from_le_bytes(state[..8].try_into().unwrap());
        Ok(())
    }
}

fn register_bulky_daemon(
    registry: &DaemonRegistry,
    count: u64,
    payload_size: usize,
) -> (EntityKeypair, u32) {
    let kp = EntityKeypair::generate();
    let origin = kp.origin_hash();
    let host = DaemonHost::new(
        Box::new(BulkyDaemon {
            count,
            payload_size,
        }),
        kp.clone(),
        DaemonHostConfig::default(),
    );
    registry.register(host).unwrap();
    (kp, origin)
}

/// Extract `(snapshot_bytes, seq_through)` from a vector of
/// `SnapshotReady` chunks by reassembling them in chunk_index
/// order. Panics on any other variant; we know what we started.
fn expect_snapshot_ready(msgs: Vec<MigrationMessage>) -> (Vec<u8>, u64) {
    let mut chunks: Vec<(u32, Vec<u8>, u64)> = msgs
        .into_iter()
        .map(|m| match m {
            MigrationMessage::SnapshotReady {
                snapshot_bytes,
                seq_through,
                chunk_index,
                ..
            } => (chunk_index, snapshot_bytes, seq_through),
            other => panic!("expected SnapshotReady, got {:?}", other),
        })
        .collect();
    chunks.sort_by_key(|(i, _, _)| *i);
    let seq_through = chunks.first().map(|(_, _, s)| *s).unwrap_or(0);
    let mut bytes = Vec::new();
    for (_, chunk, _) in chunks {
        bytes.extend_from_slice(&chunk);
    }
    (bytes, seq_through)
}

#[test]
fn target_failure_mid_chunking_releases_orchestrator_and_allows_retarget() {
    // 3× chunk size + slack forces ≥4 wire chunks.
    let payload_size = MAX_SNAPSHOT_CHUNK_SIZE * 3 + 4096;

    let source_reg = Arc::new(DaemonRegistry::new());
    let (_kp, origin) = register_bulky_daemon(&source_reg, 7, payload_size);

    // Orchestrator is co-resident with the source node (0x1111).
    let orch = MigrationOrchestrator::new(source_reg.clone(), 0x1111);

    // ---- Phase 0 → target #1 (0x2222) ----
    let first_target = 0x2222u64;
    let msg = orch.start_migration(origin, 0x1111, first_target).unwrap();
    let (snapshot_bytes, seq_through) = expect_snapshot_ready(msg);

    // Split the snapshot into wire chunks exactly as the
    // subprotocol handler would before fanning them out.
    let chunks = chunk_snapshot(origin, snapshot_bytes.clone(), seq_through).unwrap();
    assert!(
        chunks.len() >= 4,
        "need a multi-chunk snapshot to meaningfully exercise partial delivery; got {} chunks",
        chunks.len(),
    );

    // ---- Half-deliver the chunks into the target reassembler ----
    let mut target_reassembler = SnapshotReassembler::new();
    let half = chunks.len() / 2;
    for chunk in chunks.iter().take(half) {
        if let MigrationMessage::SnapshotReady {
            daemon_origin,
            snapshot_bytes,
            seq_through,
            chunk_index,
            total_chunks,
        } = chunk
        {
            let res = target_reassembler
                .feed(
                    *daemon_origin,
                    snapshot_bytes.clone(),
                    *seq_through,
                    *chunk_index,
                    *total_chunks,
                )
                .expect("partial feed must not error");
            assert!(
                res.is_none(),
                "reassembly should not complete before all chunks arrive",
            );
        }
    }
    assert_eq!(
        target_reassembler.pending_count(),
        1,
        "reassembler should hold exactly one in-flight reassembly",
    );

    // ---- Simulate mid-chunk target failure ----
    //
    // On the target side, the dispatcher cancels pending
    // reassembly when it loses the source session (or vice
    // versa). `cancel` must be panic-free and must retain
    // `latest_seq` so stale replays of the dropped chunks
    // continue to be rejected by `feed`.
    target_reassembler.cancel(origin);
    assert_eq!(
        target_reassembler.pending_count(),
        0,
        "cancel must evict pending reassembly",
    );

    // Stale replay: feeding one of the already-delivered
    // chunks against a *lower* seq_through must still be
    // rejected, proving `latest_seq` was preserved.
    if seq_through > 0 {
        if let MigrationMessage::SnapshotReady {
            daemon_origin,
            snapshot_bytes,
            chunk_index,
            total_chunks,
            ..
        } = &chunks[0]
        {
            let err = target_reassembler
                .feed(
                    *daemon_origin,
                    snapshot_bytes.clone(),
                    seq_through - 1,
                    *chunk_index,
                    *total_chunks,
                )
                .expect_err("stale seq_through must be rejected after cancel");
            match err {
                net::adapter::net::compute::orchestrator::ReassemblyError::StaleSeqThrough {
                    got,
                    latest,
                } => {
                    assert_eq!(got, seq_through - 1);
                    assert_eq!(latest, seq_through);
                }
                other => panic!("expected StaleSeqThrough, got {:?}", other),
            }
        }
    }

    // A straggler chunk for the same (origin, seq_through)
    // arriving after the cancel must not panic. The reassembler
    // simply starts a fresh partial reassembly — this is the
    // racy case where the wire got ahead of the dispatcher's
    // cancel.
    if let MigrationMessage::SnapshotReady {
        daemon_origin,
        snapshot_bytes,
        seq_through: st,
        chunk_index,
        total_chunks,
    } = &chunks[half]
    {
        let res = target_reassembler
            .feed(
                *daemon_origin,
                snapshot_bytes.clone(),
                *st,
                *chunk_index,
                *total_chunks,
            )
            .expect("late straggler must not error");
        assert!(res.is_none(), "one chunk shouldn't complete reassembly");
    }
    // Clean the straggler state before moving on.
    target_reassembler.cancel(origin);

    // ---- Orchestrator side: abort and retarget ----
    assert_eq!(orch.status(origin), Some(MigrationPhase::Transfer));
    assert!(orch.is_migrating(origin));

    let abort_msg = orch
        .abort_migration_with_reason(
            origin,
            MigrationFailureReason::StateFailed("target session lost mid-chunking".into()),
        )
        .expect("abort on an in-flight migration must succeed");
    match abort_msg {
        MigrationMessage::MigrationFailed {
            daemon_origin,
            reason,
        } => {
            assert_eq!(daemon_origin, origin);
            match reason {
                MigrationFailureReason::StateFailed(msg) => {
                    assert!(msg.contains("target session lost mid-chunking"));
                }
                other => panic!("expected StateFailed, got {:?}", other),
            }
        }
        other => panic!("expected MigrationFailed, got {:?}", other),
    }

    assert!(
        !orch.is_migrating(origin),
        "abort must evict the migration record so the origin is free to retarget",
    );
    assert_eq!(orch.status(origin), None);
    assert_eq!(orch.active_count(), 0);

    // ---- Retarget to a fresh node (0x3333) ----
    //
    // The orchestrator must let us start a brand-new migration
    // for the same daemon — the stuck record was evicted, so no
    // AlreadyMigrating error should fire.
    let second_target = 0x3333u64;
    let retry = orch
        .start_migration(origin, 0x1111, second_target)
        .expect("orchestrator must accept a fresh migration after abort");
    assert_eq!(orch.target_node(origin), Some(second_target));
    let (retry_bytes, retry_seq) = expect_snapshot_ready(retry);

    // Restarted migration chunks still decode cleanly, and a
    // second reassembler can complete the full snapshot — the
    // source-side failure recovery left no corrupted state.
    let retry_chunks = chunk_snapshot(origin, retry_bytes, retry_seq).unwrap();
    let mut second_reassembler = SnapshotReassembler::new();
    let mut completed: Option<Vec<u8>> = None;
    for chunk in &retry_chunks {
        if let MigrationMessage::SnapshotReady {
            daemon_origin,
            snapshot_bytes,
            seq_through,
            chunk_index,
            total_chunks,
        } = chunk
        {
            let res = second_reassembler
                .feed(
                    *daemon_origin,
                    snapshot_bytes.clone(),
                    *seq_through,
                    *chunk_index,
                    *total_chunks,
                )
                .expect("retry chunks must decode");
            if let Some(bytes) = res {
                completed = Some(bytes);
            }
        }
    }
    let rebuilt = completed.expect("all retry chunks delivered — reassembly must complete");
    assert!(!rebuilt.is_empty(), "reassembled snapshot must carry bytes");
    assert_eq!(second_reassembler.pending_count(), 0);
}

#[test]
fn abort_on_unknown_daemon_returns_daemon_not_found() {
    // Defensive companion: the failure-recovery path must also
    // surface a structured error when the abort races against a
    // cleanup that already evicted the record, rather than
    // panicking or silently returning a fabricated message.
    let reg = Arc::new(DaemonRegistry::new());
    let orch = MigrationOrchestrator::new(reg, 0x1111);

    let err = orch
        .abort_migration_with_reason(
            0xDEAD_BEEF,
            MigrationFailureReason::StateFailed("double abort".into()),
        )
        .expect_err("abort on a non-existent migration must error");

    match err {
        net::adapter::net::MigrationError::DaemonNotFound(id) => {
            assert_eq!(id, 0xDEAD_BEEF);
        }
        other => panic!("expected DaemonNotFound, got {:?}", other),
    }
}

#[test]
fn reassembler_cancel_is_idempotent_across_repeated_target_failures() {
    // If the dispatcher cancels the reassembler multiple times
    // (e.g. both `session_lost` and `MigrationFailed` arrive), we
    // must not panic and must not erase `latest_seq`.
    let mut r = SnapshotReassembler::new();
    let origin = 0xCAFE_BABEu32;
    let seq = 999u64;

    // Seed with one chunk from a 4-chunk snapshot.
    r.feed(origin, vec![0u8; 16], seq, 0, 4).unwrap();
    assert_eq!(r.pending_count(), 1);

    r.cancel(origin);
    r.cancel(origin);
    r.cancel(origin);
    assert_eq!(r.pending_count(), 0);

    // `latest_seq` is preserved — an older seq_through still
    // rejects.
    let err = r
        .feed(origin, vec![0u8; 16], seq - 1, 0, 4)
        .expect_err("latest_seq must survive repeated cancels");
    match err {
        net::adapter::net::compute::orchestrator::ReassemblyError::StaleSeqThrough {
            got,
            latest,
        } => {
            assert_eq!(got, seq - 1);
            assert_eq!(latest, seq);
        }
        other => panic!("expected StaleSeqThrough, got {:?}", other),
    }

    // Feeding a fresh, newer seq_through still works.
    r.feed(origin, vec![0u8; 16], seq + 1, 0, 1).unwrap();
    assert_eq!(r.pending_count(), 0);
}
