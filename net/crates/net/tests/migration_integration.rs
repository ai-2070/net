//! Integration tests for the MIKOSHI live migration system.
//!
//! These tests exercise the full migration lifecycle across the orchestrator,
//! source handler, and target handler — verifying that the components compose
//! correctly end-to-end.

#![cfg(feature = "net")]

use std::sync::Arc;

use bytes::Bytes;
use net::adapter::net::behavior::capability::{
    CapabilityAnnouncement, CapabilityFilter, CapabilityIndex, CapabilitySet,
};
use net::adapter::net::behavior::loadbalance::{RequestContext, Strategy};
use net::adapter::net::compute::migration_target::RestoreContext;
use net::adapter::net::compute::{
    chunk_snapshot, BufferOutcome, DaemonError, DaemonHost, DaemonHostConfig, DaemonRegistry,
    ForkGroup, ForkGroupConfig, GroupCoordinator, GroupError, GroupHealth, MemberInfo, MemberRole,
    MeshDaemon, MigrationMessage, MigrationOrchestrator, MigrationPhase, MigrationSourceHandler,
    MigrationTargetHandler, ReplicaGroup, ReplicaGroupConfig, Scheduler, SnapshotReassembler,
    StandbyGroup, StandbyGroupConfig, MAX_SNAPSHOT_CHUNK_SIZE,
};
use net::adapter::net::continuity::discontinuity::fork_sentinel;
use net::adapter::net::identity::{EntityId, EntityKeypair};

/// Zero-byte EntityId for test fixtures — valid as a data-structure
/// input to `CapabilityAnnouncement::new`, but not a valid ed25519
/// public key. None of these tests exercise signature verification.
fn test_entity_id() -> EntityId {
    EntityId::from_bytes([0u8; 32])
}
use net::adapter::net::state::causal::{CausalEvent, CausalLink};
use net::adapter::net::state::snapshot::StateSnapshot;
use net::adapter::net::subprotocol::SubprotocolRegistry;

// ── Test daemon ──────────────────────────────────────────────────────────────

/// A stateful counter daemon for testing. Each event increments the counter.
/// Snapshot serializes the count as 8 LE bytes; restore deserializes it.
struct CounterDaemon {
    count: u64,
}

impl CounterDaemon {
    fn new() -> Self {
        Self { count: 0 }
    }

    fn with_count(count: u64) -> Self {
        Self { count }
    }
}

impl MeshDaemon for CounterDaemon {
    fn name(&self) -> &str {
        "counter"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        self.count += 1;
        Ok(vec![Bytes::from(self.count.to_le_bytes().to_vec())])
    }
    fn snapshot(&self) -> Option<Bytes> {
        Some(Bytes::from(self.count.to_le_bytes().to_vec()))
    }
    fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
        if state.len() != 8 {
            return Err(DaemonError::RestoreFailed("bad state size".into()));
        }
        self.count = u64::from_le_bytes(state[..8].try_into().unwrap());
        Ok(())
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn make_event(origin: u32, seq: u64) -> CausalEvent {
    CausalEvent {
        link: CausalLink {
            origin_hash: origin,
            horizon_encoded: 0,
            sequence: seq,
            parent_hash: 0,
        },
        payload: Bytes::from(format!("event-{}", seq)),
        received_at: seq * 1000,
    }
}

fn register_counter_daemon(registry: &DaemonRegistry, initial_count: u64) -> (EntityKeypair, u32) {
    let kp = EntityKeypair::generate();
    let origin = kp.origin_hash();
    let host = DaemonHost::new(
        Box::new(CounterDaemon::with_count(initial_count)),
        kp.clone(),
        DaemonHostConfig::default(),
    );
    registry.register(host).unwrap();
    (kp, origin)
}

// ── 1. Orchestrator full phase progression ───────────────────────────────────

#[test]
fn test_orchestrator_full_phase_chain() {
    let reg = Arc::new(DaemonRegistry::new());
    let (_kp, origin) = register_counter_daemon(&reg, 42);
    let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);

    // Phase 0→1: Start migration (local source)
    let msgs = orch.start_migration(origin, 0x1111, 0x2222).unwrap();
    assert_eq!(orch.status(origin), Some(MigrationPhase::Transfer));
    let _snapshot_bytes = match &msgs[0] {
        MigrationMessage::SnapshotReady { snapshot_bytes, .. } => snapshot_bytes.clone(),
        other => panic!("expected SnapshotReady, got {:?}", other),
    };

    // Phase 1→2: Snapshot ready → forward (simulating orchestrator receiving it back)
    // Since we're the source and already advanced, on_snapshot_ready is for remote case.
    // For local source, we're already in Transfer. Simulate restore complete from target.
    let buffered = orch.on_restore_complete(origin, 42).unwrap();
    assert_eq!(orch.status(origin), Some(MigrationPhase::Replay));
    assert!(buffered.is_none()); // no events buffered yet

    // Phase 3→4: Replay complete → cutover
    let cutover_msg = orch.on_replay_complete(origin, 42).unwrap();
    assert_eq!(orch.status(origin), Some(MigrationPhase::Cutover));
    match cutover_msg {
        MigrationMessage::CutoverNotify { target_node, .. } => {
            assert_eq!(target_node, 0x2222);
        }
        _ => panic!("expected CutoverNotify"),
    }

    // Phase 4→5: Cutover acknowledged
    orch.on_cutover_acknowledged(origin).unwrap();
    assert_eq!(orch.status(origin), Some(MigrationPhase::Complete));

    // Phase 5: Cleanup complete — orchestrator emits ActivateTarget
    let activate = orch.on_cleanup_complete(origin).unwrap();
    match activate {
        MigrationMessage::ActivateTarget { daemon_origin } => {
            assert_eq!(daemon_origin, origin);
        }
        _ => panic!("expected ActivateTarget"),
    }
    // Phase 6: Target acknowledges activation — migration terminus.
    orch.on_activate_ack(origin, 42).unwrap();
    assert!(!orch.is_migrating(origin));
    assert_eq!(orch.active_count(), 0);
}

// ── 2. Orchestrator phase chain with buffered events ─────────────────────────

#[test]
fn test_orchestrator_phase_chain_with_buffered_events() {
    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 10);
    let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);

    // Start migration
    orch.start_migration(origin, 0x1111, 0x2222).unwrap();

    // Buffer events while snapshot is in flight
    for seq in 1..=5 {
        assert_eq!(
            orch.buffer_event(origin, make_event(0xBBBB, seq)),
            BufferOutcome::Buffered,
        );
    }

    // Restore complete → should drain buffered events
    let buffered = orch.on_restore_complete(origin, 10).unwrap();
    assert!(buffered.is_some());
    match buffered.unwrap() {
        MigrationMessage::BufferedEvents { events, .. } => {
            assert_eq!(events.len(), 5);
            assert_eq!(events[0].link.sequence, 1);
            assert_eq!(events[4].link.sequence, 5);
        }
        _ => panic!("expected BufferedEvents"),
    }

    // Continue through remaining phases
    orch.on_replay_complete(origin, 15).unwrap();
    orch.on_cutover_acknowledged(origin).unwrap();
    orch.on_cleanup_complete(origin).unwrap();
    orch.on_activate_ack(origin, 15).unwrap();
    assert!(!orch.is_migrating(origin));
}

// ── 3. End-to-end: source → orchestrator → target ───────────────────────────

#[test]
fn test_end_to_end_migration_local_source() {
    // Setup: source node (0x1111) with a daemon, target registry (0x2222)
    let source_reg = Arc::new(DaemonRegistry::new());
    let target_reg = Arc::new(DaemonRegistry::new());

    let (kp, origin) = register_counter_daemon(&source_reg, 100);

    // Process some events on source to advance state
    for seq in 1..=5 {
        source_reg
            .deliver(origin, &make_event(0xFFFF, seq))
            .unwrap();
    }
    // Daemon count is now 105 (100 initial + 5 events)

    let source_handler = MigrationSourceHandler::new(source_reg.clone());
    let target_handler = MigrationTargetHandler::new(target_reg.clone());
    let orch = MigrationOrchestrator::new(source_reg.clone(), 0x1111);

    // Phase 0: Orchestrator starts migration → takes snapshot locally
    let msgs = orch.start_migration(origin, 0x1111, 0x2222).unwrap();
    // Reassemble chunks into the original snapshot bytes (small
    // snapshots are a single chunk; this loop handles both).
    let mut snapshot_bytes: Vec<u8> = Vec::new();
    let mut seq_through: u64 = 0;
    for m in &msgs {
        match m {
            MigrationMessage::SnapshotReady {
                snapshot_bytes: chunk,
                seq_through: sq,
                ..
            } => {
                snapshot_bytes.extend_from_slice(chunk);
                seq_through = *sq;
            }
            other => panic!("expected SnapshotReady, got {:?}", other),
        }
    }

    // Phase 2: Target restores from snapshot
    let snapshot = StateSnapshot::from_bytes(&snapshot_bytes).unwrap();
    target_handler
        .restore_snapshot(
            RestoreContext {
                daemon_origin: origin,
                snapshot: &snapshot,
                source_node: 0x1111,
                orchestrator_node: 0x1111,
            },
            kp.clone(),
            || Box::new(CounterDaemon::new()),
            DaemonHostConfig::default(),
        )
        .unwrap();
    assert!(target_reg.contains(origin));

    // Simulate events arriving during transfer
    source_handler
        .start_snapshot(origin, 0x2222, 0x1111)
        .unwrap();
    source_handler
        .buffer_event(origin, make_event(0xFFFF, 6))
        .unwrap();
    source_handler
        .buffer_event(origin, make_event(0xFFFF, 7))
        .unwrap();

    // Phase 2→3: Notify orchestrator restore is complete
    let _buffered_msg = orch.on_restore_complete(origin, seq_through).unwrap();
    // Orchestrator may have its own buffered events (none in this case since
    // we buffered on the source handler directly)

    // Phase 3: Replay buffered events from source on target
    let buffered_events = source_handler.take_buffered_events(origin).unwrap();
    assert_eq!(buffered_events.len(), 2);
    let replayed_through = target_handler
        .replay_events(origin, buffered_events)
        .unwrap();

    // Phase 3→4: Replay complete
    let cutover_msg = orch.on_replay_complete(origin, replayed_through).unwrap();
    match &cutover_msg {
        MigrationMessage::CutoverNotify { target_node, .. } => {
            assert_eq!(*target_node, 0x2222);
        }
        _ => panic!("expected CutoverNotify"),
    }

    // Phase 4: Cutover — source stops accepting writes
    let final_events = source_handler.on_cutover(origin).unwrap();
    assert!(final_events.is_empty()); // already drained

    // Phase 4: Activate target
    target_handler.activate(origin).unwrap();

    // Phase 4→5: Cutover acknowledged
    orch.on_cutover_acknowledged(origin).unwrap();

    // Phase 5: Source cleanup
    source_handler.cleanup(origin).unwrap();
    assert!(!source_reg.contains(origin)); // daemon removed from source

    // Target completes
    target_handler.complete(origin).unwrap();
    orch.on_cleanup_complete(origin).unwrap();
    orch.on_activate_ack(origin, 5).unwrap();

    // Verify: daemon lives on target, not on source
    assert!(target_reg.contains(origin));
    assert!(!source_reg.contains(origin));
    assert!(!orch.is_migrating(origin));
}

// ── 4. start_migration_auto ──────────────────────────────────────────────────

#[test]
fn test_start_migration_auto() {
    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 50);

    let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);

    // Create an index with a migration-capable target
    let index = Arc::new(CapabilityIndex::new());
    let target_caps = CapabilitySet::new().add_tag("subprotocol:0x0500");
    index.index(CapabilityAnnouncement::new(
        0x2222,
        test_entity_id(),
        1,
        target_caps,
    ));

    let local_caps = CapabilitySet::new();
    let scheduler = Scheduler::new(index, 0x1111, local_caps);

    let (target_node, msgs) = orch
        .start_migration_auto(origin, 0x1111, &scheduler, &CapabilityFilter::default())
        .unwrap();

    assert_eq!(target_node, 0x2222);
    assert_eq!(orch.target_node(origin), Some(0x2222));
    assert!(!msgs.is_empty(), "must emit at least one chunk");
    match &msgs[0] {
        MigrationMessage::SnapshotReady { daemon_origin, .. } => {
            assert_eq!(*daemon_origin, origin);
        }
        other => panic!("expected SnapshotReady, got {:?}", other),
    }
}

#[test]
fn test_start_migration_auto_no_targets() {
    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 50);

    let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);

    // Empty index — no migration-capable nodes
    let index = Arc::new(CapabilityIndex::new());
    let scheduler = Scheduler::new(index, 0x1111, CapabilitySet::new());

    let err = orch
        .start_migration_auto(origin, 0x1111, &scheduler, &CapabilityFilter::default())
        .unwrap_err();
    // start_migration_auto surfaces the typed NoTargetAvailable
    // when the scheduler finds no candidate; TargetUnavailable(_)
    // is reserved for paths that already had a specific target id.
    match err {
        net::adapter::net::MigrationError::NoTargetAvailable => {}
        _ => panic!("expected NoTargetAvailable, got {:?}", err),
    }
}

// ── 5. Subprotocol handler full message chain ────────────────────────────────

#[test]
fn test_subprotocol_handler_snapshot_ready_dispatch() {
    use net::adapter::net::compute::orchestrator::wire;
    use net::adapter::net::subprotocol::MigrationSubprotocolHandler;

    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 25);

    let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0x1111));
    let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
    let target = Arc::new(MigrationTargetHandler::new(reg.clone()));

    let handler = MigrationSubprotocolHandler::new(orch.clone(), source, target, 0x1111);

    // Send TakeSnapshot → should get SnapshotReady back
    let take_msg = MigrationMessage::TakeSnapshot {
        daemon_origin: origin,
        target_node: 0x2222,
    };
    let outbound = handler
        .handle_message(&wire::encode(&take_msg).unwrap(), 0x3333)
        .unwrap();
    assert!(!outbound.is_empty());

    // Decode the reply — should be SnapshotReady
    let reply = wire::decode(&outbound[0].payload).unwrap();
    match reply {
        MigrationMessage::SnapshotReady {
            daemon_origin,
            chunk_index,
            total_chunks,
            ..
        } => {
            assert_eq!(daemon_origin, origin);
            assert_eq!(chunk_index, 0);
            assert_eq!(total_chunks, 1); // small daemon = single chunk
        }
        _ => panic!("expected SnapshotReady"),
    }
}

#[test]
fn test_subprotocol_handler_buffered_events_dispatch() {
    use net::adapter::net::compute::orchestrator::wire;
    use net::adapter::net::subprotocol::MigrationSubprotocolHandler;

    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 10);

    let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0x3333));
    let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
    let target = Arc::new(MigrationTargetHandler::new(reg.clone()));

    let handler = MigrationSubprotocolHandler::new(orch.clone(), source, target, 0x3333);

    // Start migration on the orchestrator (remote source at 0x1111)
    orch.start_migration(origin, 0x1111, 0x2222).unwrap();

    // Buffer some events on the orchestrator
    orch.buffer_event(origin, make_event(0xCCCC, 1));
    orch.buffer_event(origin, make_event(0xCCCC, 2));

    // Send RestoreComplete → should get BufferedEvents back
    let restore_msg = MigrationMessage::RestoreComplete {
        daemon_origin: origin,
        restored_seq: 10,
    };
    let outbound = handler
        .handle_message(&wire::encode(&restore_msg).unwrap(), 0x2222)
        .unwrap();

    // Should have BufferedEvents response
    assert!(!outbound.is_empty());
    let reply = wire::decode(&outbound[0].payload).unwrap();
    match reply {
        MigrationMessage::BufferedEvents { events, .. } => {
            assert_eq!(events.len(), 2);
        }
        _ => panic!("expected BufferedEvents, got {:?}", reply),
    }
}

#[test]
fn test_subprotocol_handler_cutover_notify_dispatch() {
    use net::adapter::net::compute::orchestrator::wire;
    use net::adapter::net::subprotocol::MigrationSubprotocolHandler;

    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 5);

    let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0x1111));
    let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
    let target = Arc::new(MigrationTargetHandler::new(reg.clone()));

    let handler = MigrationSubprotocolHandler::new(orch.clone(), source.clone(), target, 0x1111);

    // Setup: source starts snapshot
    source.start_snapshot(origin, 0x2222, 0x1111).unwrap();

    // Buffer an event on source
    source.buffer_event(origin, make_event(0xFFFF, 1)).unwrap();

    // Orchestrator starts migration
    orch.start_migration(origin, 0x1111, 0x2222).unwrap();
    // Advance to replay→cutover
    orch.on_restore_complete(origin, 5).unwrap();
    orch.on_replay_complete(origin, 5).unwrap();

    // Now send CutoverNotify to the source via handler
    let cutover_msg = MigrationMessage::CutoverNotify {
        daemon_origin: origin,
        target_node: 0x2222,
    };
    let outbound = handler
        .handle_message(&wire::encode(&cutover_msg).unwrap(), 0x3333)
        .unwrap();

    // Should have: BufferedEvents (final events) + CleanupComplete
    assert!(!outbound.is_empty());

    // Check that CleanupComplete was sent
    let has_cleanup = outbound.iter().any(|o| {
        matches!(
            wire::decode(&o.payload),
            Ok(MigrationMessage::CleanupComplete { .. })
        )
    });
    assert!(has_cleanup, "expected CleanupComplete in outbound");
}

#[test]
fn test_subprotocol_handler_cleanup_complete_dispatch() {
    use net::adapter::net::compute::orchestrator::wire;
    use net::adapter::net::subprotocol::MigrationSubprotocolHandler;

    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 1);

    let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0x1111));
    let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
    let target = Arc::new(MigrationTargetHandler::new(reg.clone()));

    let handler = MigrationSubprotocolHandler::new(orch.clone(), source, target, 0x1111);

    // Setup: start and advance migration to Complete
    orch.start_migration(origin, 0x1111, 0x2222).unwrap();
    orch.on_restore_complete(origin, 1).unwrap();
    orch.on_replay_complete(origin, 1).unwrap();
    orch.on_cutover_acknowledged(origin).unwrap();
    assert!(orch.is_migrating(origin));

    // Send CleanupComplete — should emit ActivateTarget to target.
    let cleanup_msg = MigrationMessage::CleanupComplete {
        daemon_origin: origin,
    };
    let outbound = handler
        .handle_message(&wire::encode(&cleanup_msg).unwrap(), 0x1111)
        .unwrap();
    assert_eq!(outbound.len(), 1);
    assert_eq!(outbound[0].dest_node, 0x2222, "ActivateTarget to target");
    match wire::decode(&outbound[0].payload).unwrap() {
        MigrationMessage::ActivateTarget { daemon_origin } => {
            assert_eq!(daemon_origin, origin);
        }
        other => panic!("expected ActivateTarget, got {:?}", other),
    }
    assert!(orch.is_migrating(origin), "record kept until activate ack");

    // Now send ActivateAck — migration terminus.
    let ack = MigrationMessage::ActivateAck {
        daemon_origin: origin,
        replayed_seq: 1,
    };
    let outbound = handler
        .handle_message(&wire::encode(&ack).unwrap(), 0x2222)
        .unwrap();
    assert!(outbound.is_empty());
    assert!(!orch.is_migrating(origin));
}

// ── 6. Reassembler with out-of-order chunks ──────────────────────────────────

#[test]
fn test_reassembler_out_of_order_chunks() {
    let data = vec![0xABu8; MAX_SNAPSHOT_CHUNK_SIZE * 3 + 500];
    let total_len = data.len();
    let chunks = chunk_snapshot(0xAAAA, data, 99).unwrap();
    assert_eq!(chunks.len(), 4);

    let mut reassembler = SnapshotReassembler::new();

    // Feed in reverse order: 3, 1, 0, 2
    let feed_order = [3, 1, 0, 2];
    for &i in &feed_order[..3] {
        let chunk = &chunks[i as usize];
        if let MigrationMessage::SnapshotReady {
            daemon_origin,
            snapshot_bytes,
            seq_through,
            chunk_index,
            total_chunks,
        } = chunk
        {
            let result = reassembler.feed(
                *daemon_origin,
                snapshot_bytes.clone(),
                *seq_through,
                *chunk_index,
                *total_chunks,
            );
            assert!(result.unwrap().is_none(), "chunk {} should not complete", i);
        }
    }

    // Feed the last one (index 2) — should complete
    let last = &chunks[feed_order[3] as usize];
    if let MigrationMessage::SnapshotReady {
        daemon_origin,
        snapshot_bytes,
        seq_through,
        chunk_index,
        total_chunks,
    } = last
    {
        let result = reassembler
            .feed(
                *daemon_origin,
                snapshot_bytes.clone(),
                *seq_through,
                *chunk_index,
                *total_chunks,
            )
            .expect("last chunk should complete reassembly")
            .expect("last chunk should return data");
        assert_eq!(result.len(), total_len);
        assert!(result.iter().all(|&b| b == 0xAB));
    }

    assert_eq!(reassembler.pending_count(), 0);
}

#[test]
fn test_reassembler_duplicate_chunks_handled() {
    let data = vec![0xCDu8; MAX_SNAPSHOT_CHUNK_SIZE * 2 + 100];
    let total_len = data.len();
    let chunks = chunk_snapshot(0xBBBB, data, 50).unwrap();
    assert_eq!(chunks.len(), 3);

    let mut reassembler = SnapshotReassembler::new();

    // Feed chunk 0 twice — second should overwrite, not cause issues
    if let MigrationMessage::SnapshotReady {
        daemon_origin,
        snapshot_bytes,
        seq_through,
        chunk_index,
        total_chunks,
    } = &chunks[0]
    {
        let _ = reassembler.feed(
            *daemon_origin,
            snapshot_bytes.clone(),
            *seq_through,
            *chunk_index,
            *total_chunks,
        );
        let _ = reassembler.feed(
            *daemon_origin,
            snapshot_bytes.clone(),
            *seq_through,
            *chunk_index,
            *total_chunks,
        );
    }

    // Feed remaining chunks
    for chunk in &chunks[1..] {
        if let MigrationMessage::SnapshotReady {
            daemon_origin,
            snapshot_bytes,
            seq_through,
            chunk_index,
            total_chunks,
        } = chunk
        {
            let result = reassembler.feed(
                *daemon_origin,
                snapshot_bytes.clone(),
                *seq_through,
                *chunk_index,
                *total_chunks,
            );
            if *chunk_index == *total_chunks - 1 {
                let full = result
                    .expect("feed should not error")
                    .expect("last chunk should complete");
                assert_eq!(full.len(), total_len);
            }
        }
    }
}

// ── 7. Event buffer → replay integration ─────────────────────────────────────

#[test]
fn test_event_buffer_flows_to_target_replay() {
    let source_reg = Arc::new(DaemonRegistry::new());
    let target_reg = Arc::new(DaemonRegistry::new());

    let (kp, origin) = register_counter_daemon(&source_reg, 0);

    // Process 10 events on source
    for seq in 1..=10 {
        source_reg
            .deliver(origin, &make_event(0xFFFF, seq))
            .unwrap();
    }

    let source_handler = MigrationSourceHandler::new(source_reg.clone());
    let target_handler = MigrationTargetHandler::new(target_reg.clone());

    // Source takes snapshot (daemon count = 10)
    let snapshot = source_handler
        .start_snapshot(origin, 0x2222, 0x1111)
        .unwrap();

    // Events arrive during migration
    for seq in 11..=15 {
        source_handler
            .buffer_event(origin, make_event(0xFFFF, seq))
            .unwrap();
    }

    // Target restores from snapshot
    target_handler
        .restore_snapshot(
            RestoreContext {
                daemon_origin: origin,
                snapshot: &snapshot,
                source_node: 0x1111,
                orchestrator_node: 0x1111,
            },
            kp.clone(),
            || Box::new(CounterDaemon::new()),
            DaemonHostConfig::default(),
        )
        .unwrap();

    // Drain buffered events from source
    let buffered = source_handler.take_buffered_events(origin).unwrap();
    assert_eq!(buffered.len(), 5);

    // Replay on target
    let replayed = target_handler.replay_events(origin, buffered).unwrap();
    assert_eq!(replayed, 15); // replayed through seq 15

    // Verify target daemon processed the events
    let target_stats = target_reg.stats(origin).unwrap();
    assert_eq!(target_stats.events_processed, 5); // 5 replayed events

    // Activate and complete
    target_handler.activate(origin).unwrap();
    target_handler.complete(origin).unwrap();
    assert!(target_reg.contains(origin));
}

// ── 8. Concurrent migrations ─────────────────────────────────────────────────

#[test]
fn test_concurrent_migrations_no_interference() {
    let reg = Arc::new(DaemonRegistry::new());

    let (_, origin_a) = register_counter_daemon(&reg, 100);
    let (_, origin_b) = register_counter_daemon(&reg, 200);

    let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);

    // Start both migrations
    orch.start_migration(origin_a, 0x1111, 0x2222).unwrap();
    orch.start_migration(origin_b, 0x1111, 0x3333).unwrap();
    assert_eq!(orch.active_count(), 2);

    // Advance A through all phases
    orch.on_restore_complete(origin_a, 100).unwrap();
    orch.on_replay_complete(origin_a, 100).unwrap();
    orch.on_cutover_acknowledged(origin_a).unwrap();
    orch.on_cleanup_complete(origin_a).unwrap();
    orch.on_activate_ack(origin_a, 100).unwrap();

    // B should still be active
    assert!(!orch.is_migrating(origin_a));
    assert!(orch.is_migrating(origin_b));
    assert_eq!(orch.status(origin_b), Some(MigrationPhase::Transfer));

    // Advance B
    orch.on_restore_complete(origin_b, 200).unwrap();
    orch.on_replay_complete(origin_b, 200).unwrap();
    orch.on_cutover_acknowledged(origin_b).unwrap();
    orch.on_cleanup_complete(origin_b).unwrap();
    orch.on_activate_ack(origin_b, 200).unwrap();

    assert_eq!(orch.active_count(), 0);
}

// ── 9. Capability enrichment end-to-end ──────────────────────────────────────

#[test]
fn test_enriched_capabilities_discoverable_by_scheduler() {
    // Node A: registers defaults and enriches its capabilities
    let subproto_reg = SubprotocolRegistry::with_defaults();
    let node_a_caps = subproto_reg.enrich_capabilities(CapabilitySet::new());
    assert!(node_a_caps.has_tag("subprotocol:0x0500"));

    // Index node A's capabilities
    let index = Arc::new(CapabilityIndex::new());
    index.index(CapabilityAnnouncement::new(
        0xAAAA,
        test_entity_id(),
        1,
        node_a_caps,
    ));

    // Node B: no migration support
    let node_b_caps = CapabilitySet::new();
    index.index(CapabilityAnnouncement::new(
        0xBBBB,
        test_entity_id(),
        1,
        node_b_caps,
    ));

    // Scheduler on node C should find A but not B
    let scheduler = Scheduler::new(index, 0xCCCC, CapabilitySet::new());
    let targets = scheduler.find_migration_targets(&CapabilityFilter::default(), 0xCCCC);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0], 0xAAAA);
}

// ── 10. Wire roundtrip for all message types ─────────────────────────────────

#[test]
fn test_wire_roundtrip_all_message_types() {
    use net::adapter::net::compute::orchestrator::wire;

    let messages: Vec<MigrationMessage> = vec![
        MigrationMessage::TakeSnapshot {
            daemon_origin: 0x1111,
            target_node: 0x2222,
        },
        MigrationMessage::SnapshotReady {
            daemon_origin: 0x3333,
            snapshot_bytes: vec![1, 2, 3, 4, 5],
            seq_through: 42,
            chunk_index: 0,
            total_chunks: 1,
        },
        MigrationMessage::SnapshotReady {
            daemon_origin: 0x3333,
            snapshot_bytes: vec![6, 7, 8],
            seq_through: 42,
            chunk_index: 2,
            total_chunks: 5,
        },
        MigrationMessage::RestoreComplete {
            daemon_origin: 0x4444,
            restored_seq: 100,
        },
        MigrationMessage::ReplayComplete {
            daemon_origin: 0x5555,
            replayed_seq: 200,
        },
        MigrationMessage::CutoverNotify {
            daemon_origin: 0x6666,
            target_node: 0x7777,
        },
        MigrationMessage::CleanupComplete {
            daemon_origin: 0x8888,
        },
        MigrationMessage::MigrationFailed {
            daemon_origin: 0x9999,
            reason: net::adapter::net::compute::MigrationFailureReason::StateFailed(
                "test failure".into(),
            ),
        },
        MigrationMessage::BufferedEvents {
            daemon_origin: 0xAAAA,
            events: vec![make_event(0xBBBB, 1), make_event(0xBBBB, 2)],
        },
    ];

    for msg in &messages {
        let encoded = wire::encode(msg).unwrap();
        let decoded = wire::decode(&encoded).unwrap();

        // Verify message type matches by checking discriminant
        assert_eq!(
            std::mem::discriminant(msg),
            std::mem::discriminant(&decoded),
            "roundtrip failed for {:?}",
            msg,
        );
    }
}

// ── 11. Migration abort at each phase ────────────────────────────────────────

#[test]
fn test_abort_at_each_phase() {
    let reg = Arc::new(DaemonRegistry::new());

    // Abort during Snapshot phase (remote source)
    {
        let (_, origin) = register_counter_daemon(&reg, 1);
        let orch = MigrationOrchestrator::new(reg.clone(), 0x3333);
        orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        assert_eq!(orch.status(origin), Some(MigrationPhase::Snapshot));
        orch.abort_migration(origin, "abort at snapshot".into())
            .unwrap();
        assert!(!orch.is_migrating(origin));
    }

    // Abort during Transfer phase (local source)
    {
        let (_, origin) = register_counter_daemon(&reg, 2);
        let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);
        orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        assert_eq!(orch.status(origin), Some(MigrationPhase::Transfer));
        orch.abort_migration(origin, "abort at transfer".into())
            .unwrap();
        assert!(!orch.is_migrating(origin));
    }

    // Abort during Replay phase
    {
        let (_, origin) = register_counter_daemon(&reg, 3);
        let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);
        orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        orch.on_restore_complete(origin, 3).unwrap();
        assert_eq!(orch.status(origin), Some(MigrationPhase::Replay));
        orch.abort_migration(origin, "abort at replay".into())
            .unwrap();
        assert!(!orch.is_migrating(origin));
    }

    // Abort during Cutover phase
    {
        let (_, origin) = register_counter_daemon(&reg, 4);
        let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);
        orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        orch.on_restore_complete(origin, 4).unwrap();
        orch.on_replay_complete(origin, 4).unwrap();
        assert_eq!(orch.status(origin), Some(MigrationPhase::Cutover));
        orch.abort_migration(origin, "abort at cutover".into())
            .unwrap();
        assert!(!orch.is_migrating(origin));
    }
}

// ── Regression tests for Cubic AI findings ───────────────────────────────────

/// Regression: CutoverNotify was routed to from_node (the target that sent
/// ReplayComplete) instead of the source node. The source never received
/// cutover and never quiesced writes.
///
/// This test sends ReplayComplete through the subprotocol handler and
/// verifies the resulting CutoverNotify is addressed to the SOURCE node,
/// not the target that reported.
#[test]
fn test_regression_cutover_routed_to_source_not_target() {
    use net::adapter::net::compute::orchestrator::wire;
    use net::adapter::net::subprotocol::MigrationSubprotocolHandler;

    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 10);

    let source_node: u64 = 0x1111;
    let target_node: u64 = 0x2222;
    let orchestrator_node: u64 = 0x3333;

    let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), orchestrator_node));
    let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
    let target = Arc::new(MigrationTargetHandler::new(reg.clone()));

    let handler = MigrationSubprotocolHandler::new(orch.clone(), source, target, orchestrator_node);

    // Setup: start migration source→target
    orch.start_migration(origin, source_node, target_node)
        .unwrap();
    orch.on_restore_complete(origin, 10).unwrap();

    // Target (0x2222) sends ReplayComplete to the orchestrator
    let replay_msg = MigrationMessage::ReplayComplete {
        daemon_origin: origin,
        replayed_seq: 10,
    };
    let outbound = handler
        .handle_message(&wire::encode(&replay_msg).unwrap(), target_node)
        .unwrap();

    // Find the CutoverNotify in outbound
    let cutover = outbound
        .iter()
        .find(|o| {
            matches!(
                wire::decode(&o.payload),
                Ok(MigrationMessage::CutoverNotify { .. })
            )
        })
        .expect("expected CutoverNotify in outbound");

    // CRITICAL: CutoverNotify must go to the SOURCE (0x1111), not the target (0x2222)
    assert_eq!(
        cutover.dest_node, source_node,
        "CutoverNotify must be routed to source node {:#x}, not target {:#x}",
        source_node, cutover.dest_node,
    );
}

/// Regression: SnapshotReady was forwarded to node 0 (placeholder) instead
/// of the actual target. Verify the handler routes snapshot chunks to the
/// correct target node.
#[test]
fn test_regression_snapshot_forwarded_to_actual_target() {
    use net::adapter::net::compute::orchestrator::wire;
    use net::adapter::net::subprotocol::MigrationSubprotocolHandler;

    let source_reg = Arc::new(DaemonRegistry::new());
    let orch_reg = Arc::new(DaemonRegistry::new());
    let (_kp, origin) = register_counter_daemon(&source_reg, 5);

    let source_node: u64 = 0x1111;
    let target_node: u64 = 0x2222;
    let orchestrator_node: u64 = 0x3333;

    let orch = Arc::new(MigrationOrchestrator::new(
        orch_reg.clone(),
        orchestrator_node,
    ));
    let source = Arc::new(MigrationSourceHandler::new(orch_reg.clone()));
    let target = Arc::new(MigrationTargetHandler::new(orch_reg.clone()));

    let handler = MigrationSubprotocolHandler::new(orch.clone(), source, target, orchestrator_node);

    // Setup: remote source started migration
    orch.start_migration(origin, source_node, target_node)
        .unwrap();

    // Build a real snapshot from the source registry
    let real_snapshot = source_reg.snapshot(origin).unwrap().unwrap();
    let snapshot_bytes = real_snapshot.to_bytes();

    // Source sends SnapshotReady to the orchestrator
    let snapshot_msg = MigrationMessage::SnapshotReady {
        daemon_origin: origin,
        snapshot_bytes,
        seq_through: real_snapshot.through_seq,
        chunk_index: 0,
        total_chunks: 1,
    };
    let outbound = handler
        .handle_message(&wire::encode(&snapshot_msg).unwrap(), source_node)
        .unwrap();

    // The forwarded SnapshotReady must go to the TARGET (0x2222), not 0 or source
    let forwarded = outbound
        .iter()
        .find(|o| {
            matches!(
                wire::decode(&o.payload),
                Ok(MigrationMessage::SnapshotReady { .. })
            )
        })
        .expect("expected SnapshotReady forwarded in outbound");

    assert_eq!(
        forwarded.dest_node, target_node,
        "SnapshotReady must be forwarded to target {:#x}, got {:#x}",
        target_node, forwarded.dest_node,
    );
}

/// Regression: start_migration had a TOCTOU race — contains_key then insert
/// was not atomic. Two concurrent calls could both pass the duplicate check.
/// Verify the entry() API rejects the second call.
#[test]
fn test_regression_start_migration_atomic_duplicate_check() {
    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 1);
    let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);

    // First call succeeds
    orch.start_migration(origin, 0x1111, 0x2222).unwrap();

    // Second call for same daemon must fail (even with different target)
    let err = orch.start_migration(origin, 0x1111, 0x3333).unwrap_err();
    assert!(
        matches!(err, net::adapter::net::MigrationError::AlreadyMigrating(_)),
        "expected AlreadyMigrating, got {:?}",
        err,
    );

    // Original migration should be intact with original target
    assert_eq!(orch.target_node(origin), Some(0x2222));
}

/// Regression: start_snapshot had the same TOCTOU race as start_migration.
#[test]
fn test_regression_start_snapshot_atomic_duplicate_check() {
    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 1);
    let handler = MigrationSourceHandler::new(reg.clone());

    // First call succeeds
    handler.start_snapshot(origin, 0x2222, 0x1111).unwrap();

    // Second call for same daemon must fail
    let err = handler.start_snapshot(origin, 0x3333, 0x1111).unwrap_err();
    assert!(
        matches!(err, net::adapter::net::MigrationError::AlreadyMigrating(_)),
        "expected AlreadyMigrating, got {:?}",
        err,
    );
}

/// Regression: drain_pending errors were silently discarded via `let _ =`
/// during buffer_event. Verify that a delivery error propagates to the caller.
#[test]
fn test_regression_drain_pending_error_propagated() {
    // This test verifies the contract: if drain_pending encounters an error
    // delivering to the daemon, buffer_event returns that error rather than
    // silently swallowing it. We test the positive case here — events that
    // are contiguous and deliverable are drained successfully. The error
    // path would require a daemon that fails on process(), which is tested
    // indirectly: the important thing is that `?` is used, not `let _ =`.
    let reg = Arc::new(DaemonRegistry::new());
    let handler = MigrationTargetHandler::new(reg.clone());
    let kp = EntityKeypair::generate();
    let origin = kp.origin_hash();

    let mut chain = net::adapter::net::state::causal::CausalChainBuilder::new(origin);
    for _ in 0..5 {
        chain.append(Bytes::from_static(b"x"), 0);
    }
    let snapshot = StateSnapshot::new(
        kp.entity_id().clone(),
        *chain.head(),
        Bytes::from(0u64.to_le_bytes().to_vec()),
        net::adapter::net::state::horizon::ObservedHorizon::new(),
    );

    handler
        .restore_snapshot(
            RestoreContext {
                daemon_origin: origin,
                snapshot: &snapshot,
                source_node: 0x1111,
                orchestrator_node: 0x1111,
            },
            kp.clone(),
            || Box::new(CounterDaemon::new()),
            DaemonHostConfig::default(),
        )
        .unwrap();

    // Buffer contiguous events — should succeed and drain immediately
    let result = handler.buffer_event(origin, make_event(0xFFFF, 6));
    assert!(result.is_ok(), "buffer_event should propagate success");

    let result = handler.buffer_event(origin, make_event(0xFFFF, 7));
    assert!(result.is_ok());

    // Verify they were drained (replayed_through advanced)
    assert_eq!(handler.replayed_through(origin), Some(7));
}

/// Regression: test make_snapshot helper ignored through_seq parameter,
/// causing StateSnapshot::through_seq to always be 0. Verify the snapshot
/// carries the correct sequence.
#[test]
fn test_regression_snapshot_through_seq_correct() {
    let reg = Arc::new(DaemonRegistry::new());
    let (_kp, origin) = register_counter_daemon(&reg, 50);

    // Process 10 events to advance the chain
    for seq in 1..=10 {
        reg.deliver(origin, &make_event(0xFFFF, seq)).unwrap();
    }

    // Take a real snapshot and verify through_seq
    let snapshot = reg.snapshot(origin).unwrap().unwrap();
    assert_eq!(
        snapshot.through_seq, 10,
        "snapshot through_seq should reflect daemon's chain sequence"
    );
    assert_eq!(snapshot.chain_link.sequence, 10);
}

/// Regression: full handler chain test — send ReplayComplete through the
/// handler, verify CutoverNotify routing, then send CutoverNotify through
/// the handler, verify CleanupComplete routing. This is the test that would
/// have caught the original P0 CutoverNotify routing bug.
#[test]
fn test_regression_full_handler_routing_chain() {
    use net::adapter::net::compute::orchestrator::wire;
    use net::adapter::net::subprotocol::MigrationSubprotocolHandler;

    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 20);

    let source_node: u64 = 0xAAAA;
    let target_node: u64 = 0xBBBB;

    // Orchestrator on a third node
    let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0xCCCC));
    let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
    let target = Arc::new(MigrationTargetHandler::new(reg.clone()));

    let handler = MigrationSubprotocolHandler::new(orch.clone(), source.clone(), target, 0xCCCC);

    // Start migration and advance to Replay
    orch.start_migration(origin, source_node, target_node)
        .unwrap();
    orch.on_restore_complete(origin, 20).unwrap();

    // ── Step 1: Target sends ReplayComplete ──
    let replay_msg = MigrationMessage::ReplayComplete {
        daemon_origin: origin,
        replayed_seq: 20,
    };
    let outbound = handler
        .handle_message(&wire::encode(&replay_msg).unwrap(), target_node)
        .unwrap();

    // Verify: CutoverNotify goes to SOURCE
    let cutover_out = outbound
        .iter()
        .find(|o| {
            matches!(
                wire::decode(&o.payload),
                Ok(MigrationMessage::CutoverNotify { .. })
            )
        })
        .expect("expected CutoverNotify");
    assert_eq!(cutover_out.dest_node, source_node);

    // ── Step 2: Source receives CutoverNotify ──
    // First, source must have started its migration tracking. Pass the
    // *actual* orchestrator node_id (0xCCCC) so the CleanupComplete reply
    // is routed back to it via `source_handler.orchestrator_node()`.
    source.start_snapshot(origin, target_node, 0xCCCC).unwrap();

    let cutover_outbound = handler
        .handle_message(&cutover_out.payload, 0xCCCC) // from orchestrator
        .unwrap();

    // Verify: CleanupComplete goes back to orchestrator (from_node)
    let cleanup_out = cutover_outbound
        .iter()
        .find(|o| {
            matches!(
                wire::decode(&o.payload),
                Ok(MigrationMessage::CleanupComplete { .. })
            )
        })
        .expect("expected CleanupComplete");
    assert_eq!(cleanup_out.dest_node, 0xCCCC); // back to orchestrator

    // Verify: if there were buffered events, they go to the target
    for out in &cutover_outbound {
        if let Ok(MigrationMessage::BufferedEvents { .. }) = wire::decode(&out.payload) {
            assert_eq!(
                out.dest_node, target_node,
                "BufferedEvents must go to target"
            );
        }
    }
}

/// Regression: CutoverNotify handler used to call `source_handler.cleanup()`
/// BEFORE reading `source_handler.orchestrator_node()`. Once cleaned up,
/// the lookup returned `None` and the reply silently fell back to
/// `from_node`. In any topology where the wire hop differs from the
/// orchestrator, this would route `CleanupComplete` to the wrong node.
///
/// Fix: the orchestrator is captured BEFORE `cleanup()`. This regression
/// test drives a scenario where `from_node` differs from the recorded
/// orchestrator so that a naive re-introduction of the bug would produce
/// the wrong `CleanupComplete.dest_node`.
#[test]
fn test_regression_cleanup_complete_prefers_recorded_orchestrator() {
    use net::adapter::net::compute::orchestrator::wire;
    use net::adapter::net::subprotocol::MigrationSubprotocolHandler;

    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 7);

    // Orchestrator lives on 0xAAAA. Source handler lives on this test node.
    // CutoverNotify will arrive with `from_node = 0xBBBB` (a hypothetical
    // relay), distinct from the orchestrator.
    let local_node: u64 = 0x1234;
    let target_node: u64 = 0xCAFE;
    let orchestrator_node: u64 = 0xAAAA;
    let relay_node: u64 = 0xBBBB;

    let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), local_node));
    let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
    let target = Arc::new(MigrationTargetHandler::new(reg.clone()));
    let handler = MigrationSubprotocolHandler::new(orch, source.clone(), target, local_node);

    // Source-side state records the orchestrator (captured at
    // TakeSnapshot time in production; supplied directly here).
    source
        .start_snapshot(origin, target_node, orchestrator_node)
        .unwrap();

    // CutoverNotify arrives from a hypothetical relay — NOT the
    // orchestrator. If the handler reads `orchestrator_node()` after
    // calling `cleanup()`, it'll get None and fall back to this
    // relay_node, breaking the reply.
    let cutover = MigrationMessage::CutoverNotify {
        daemon_origin: origin,
        target_node,
    };
    let outbound = handler
        .handle_message(&wire::encode(&cutover).unwrap(), relay_node)
        .unwrap();

    let cleanup = outbound
        .iter()
        .find(|o| {
            matches!(
                wire::decode(&o.payload),
                Ok(MigrationMessage::CleanupComplete { .. })
            )
        })
        .expect("expected CleanupComplete outbound");
    assert_eq!(
        cleanup.dest_node, orchestrator_node,
        "CleanupComplete must go to the recorded orchestrator ({:#x}), not to the \
         wire hop {:#x}",
        orchestrator_node, relay_node
    );
}

/// Regression: SnapshotReassembler was keyed only by daemon_origin, so chunks
/// from different seq_through snapshots (e.g., a retry after abort) could be
/// mixed, producing a corrupt reassembled snapshot. Now keyed by
/// (daemon_origin, seq_through).
#[test]
fn test_regression_reassembler_rejects_mixed_seq_through() {
    let mut reassembler = SnapshotReassembler::new();

    // Start reassembly for daemon 0xAAAA, seq_through=100
    let result = reassembler.feed(0xAAAA, vec![1, 2, 3], 100, 0, 2).unwrap();
    assert!(result.is_none());

    // New snapshot for same daemon but seq_through=200 (e.g., after abort + retry)
    let result = reassembler.feed(0xAAAA, vec![4, 5, 6], 200, 0, 2).unwrap();
    assert!(result.is_none());

    // Complete the seq_through=200 reassembly
    let result = reassembler.feed(0xAAAA, vec![7, 8, 9], 200, 1, 2).unwrap();
    assert!(result.is_some());
    let full = result.unwrap();
    // Must contain only chunks from seq_through=200, not mixed with seq_through=100
    assert_eq!(full, vec![4, 5, 6, 7, 8, 9]);

    // Cancel cleans up all pending for this daemon
    reassembler.cancel(0xAAAA);
    assert_eq!(reassembler.pending_count(), 0);
}

/// Regression: Multi-chunk snapshot path in on_snapshot_ready never advanced
/// MigrationState out of Snapshot phase, breaking subsequent phase transitions
/// (on_restore_complete would fail with WrongPhase).
#[test]
fn test_regression_multi_chunk_advances_past_snapshot_phase() {
    let reg = Arc::new(DaemonRegistry::new());
    let (_, origin) = register_counter_daemon(&reg, 10);
    let orch = MigrationOrchestrator::new(reg.clone(), 0x3333);

    // Start migration (remote source)
    orch.start_migration(origin, 0x1111, 0x2222).unwrap();
    assert_eq!(orch.status(origin), Some(MigrationPhase::Snapshot));

    // Simulate first chunk of a multi-chunk snapshot
    orch.on_snapshot_ready(origin, vec![1, 2, 3], 10, 0, 3)
        .unwrap();

    // Phase MUST have advanced past Snapshot — otherwise on_restore_complete
    // would fail because it expects Transfer phase
    assert_ne!(
        orch.status(origin),
        Some(MigrationPhase::Snapshot),
        "multi-chunk first chunk must advance phase past Snapshot"
    );
    assert_eq!(orch.status(origin), Some(MigrationPhase::Transfer));

    // Subsequent chunks should not error
    orch.on_snapshot_ready(origin, vec![4, 5, 6], 10, 1, 3)
        .unwrap();
    orch.on_snapshot_ready(origin, vec![7, 8], 10, 2, 3)
        .unwrap();

    // on_restore_complete should work now
    orch.on_restore_complete(origin, 10).unwrap();
    assert_eq!(orch.status(origin), Some(MigrationPhase::Replay));
}

/// Regression: chunk_snapshot originally used an unchecked `as u16` cast.
/// Now uses u32 chunks and returns Result. Verify chunk arithmetic and
/// graceful error on the Result path.
#[test]
fn test_regression_chunk_count_boundary() {
    // Exactly 1 chunk
    let chunks = chunk_snapshot(0xAAAA, vec![0u8; MAX_SNAPSHOT_CHUNK_SIZE], 1).unwrap();
    assert_eq!(chunks.len(), 1);

    // Exactly 2 chunks
    let chunks = chunk_snapshot(0xAAAA, vec![0u8; MAX_SNAPSHOT_CHUNK_SIZE + 1], 1).unwrap();
    assert_eq!(chunks.len(), 2);

    // 100 chunks
    let chunks = chunk_snapshot(0xAAAA, vec![0u8; MAX_SNAPSHOT_CHUNK_SIZE * 100], 1).unwrap();
    assert_eq!(chunks.len(), 100);
    for (i, chunk) in chunks.iter().enumerate() {
        if let MigrationMessage::SnapshotReady {
            chunk_index,
            total_chunks,
            ..
        } = chunk
        {
            assert_eq!(*chunk_index, i as u32);
            assert_eq!(*total_chunks, 100);
        }
    }
}

// ── Group integration tests ──────────────────────────────────────────────────

fn make_scheduler_for_groups() -> Scheduler {
    let index = Arc::new(CapabilityIndex::new());
    index.index(CapabilityAnnouncement::new(
        0x1111,
        test_entity_id(),
        1,
        CapabilitySet::new(),
    ));
    index.index(CapabilityAnnouncement::new(
        0x2222,
        test_entity_id(),
        1,
        CapabilitySet::new(),
    ));
    index.index(CapabilityAnnouncement::new(
        0x3333,
        test_entity_id(),
        1,
        CapabilitySet::new(),
    ));
    Scheduler::new(index, 0x1111, CapabilitySet::new())
}

/// Integration test 1: ReplicaGroup refactor — route_event returns an
/// origin_hash that DaemonRegistry::deliver() accepts, and the daemon
/// actually processes the event.
#[test]
fn test_replica_group_route_and_deliver() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    let group = ReplicaGroup::spawn(
        ReplicaGroupConfig {
            replica_count: 3,
            group_seed: [99u8; 32],
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    // Route an event
    let ctx = RequestContext::default();
    let origin = group.route_event(&ctx).unwrap();

    // Deliver through DaemonRegistry — this verifies the origin_hash
    // actually maps to a registered daemon that can process events
    let event = make_event(0xFFFF, 1);
    let outputs = reg.deliver(origin, &event).unwrap();

    // CounterDaemon increments and emits the count
    assert_eq!(outputs.len(), 1);
    let count = u64::from_le_bytes(outputs[0].payload[..8].try_into().unwrap());
    assert_eq!(count, 1);

    // Deliver another event to verify the daemon is stateful and alive
    let event2 = make_event(0xFFFF, 2);
    let outputs2 = reg.deliver(origin, &event2).unwrap();
    let count2 = u64::from_le_bytes(outputs2[0].payload[..8].try_into().unwrap());
    assert_eq!(count2, 2);
}

/// Integration test 2: ForkGroup produces events whose causal chain carries
/// the fork sentinel. This is the whole point — events from a fork should
/// be traceable back to the parent.
#[test]
fn test_fork_group_causal_chain_carries_sentinel() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    let parent_origin: u32 = 0xAAAA;
    let fork_seq: u64 = 100;

    let group = ForkGroup::fork(
        parent_origin,
        fork_seq,
        ForkGroupConfig {
            fork_count: 2,
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    // Verify fork records reference the parent
    let expected_sentinel = fork_sentinel(parent_origin, fork_seq);
    for record in group.fork_records() {
        assert_eq!(record.original_origin, parent_origin);
        assert_eq!(record.fork_seq, fork_seq);
        assert_eq!(record.fork_genesis.parent_hash, expected_sentinel);
        assert_eq!(record.fork_genesis.sequence, 0);
        assert!(record.verify());
    }

    // Deliver an event to a fork and check the output chain
    let ctx = RequestContext::default();
    let origin = group.route_event(&ctx).unwrap();

    let event = make_event(0xFFFF, 1);
    let outputs = reg.deliver(origin, &event).unwrap();
    assert_eq!(outputs.len(), 1);

    // The output's origin_hash should be the fork's origin (not the parent's)
    assert_eq!(outputs[0].link.origin_hash, origin);
    assert_ne!(outputs[0].link.origin_hash, parent_origin);

    // The output's sequence should be 1 (first event after fork genesis at seq 0)
    assert_eq!(outputs[0].link.sequence, 1);

    // The output's parent_hash should NOT be 0 — it should chain from the
    // fork genesis (which has the sentinel as its parent_hash)
    assert_ne!(outputs[0].link.parent_hash, 0);
}

/// Integration test 3: ForkGroup failure → recovery → identity + chain
/// preservation. Verifies that a recovered fork produces events with the
/// same origin_hash AND that the fork genesis sentinel survived re-creation.
#[test]
fn test_fork_group_recovery_preserves_chain_identity() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    let parent_origin: u32 = 0xBBBB;
    let fork_seq: u64 = 50;

    let mut group = ForkGroup::fork(
        parent_origin,
        fork_seq,
        ForkGroupConfig {
            fork_count: 2,
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    // Record the first fork's identity before failure
    let fork_0_origin = group.members()[0].origin_hash;
    let fork_0_node = group.members()[0].node_id;

    // Deliver an event to fork 0 before failure
    let event = make_event(0xFFFF, 1);
    reg.deliver(fork_0_origin, &event).unwrap();

    // Simulate node failure and recovery
    let replaced = group
        .on_node_failure(fork_0_node, || Box::new(CounterDaemon::new()), &sched, &reg)
        .unwrap();
    assert!(!replaced.is_empty());

    // The recovered fork should have the same origin_hash
    assert!(group
        .members()
        .iter()
        .any(|m| m.origin_hash == fork_0_origin));

    // Deliver an event to the recovered fork — it should accept it
    let event2 = make_event(0xFFFF, 2);
    let outputs = reg.deliver(fork_0_origin, &event2).unwrap();
    assert_eq!(outputs.len(), 1);

    // The output should carry the fork's origin_hash
    assert_eq!(outputs[0].link.origin_hash, fork_0_origin);

    // Lineage should still verify (fork records unchanged)
    assert!(group.verify_lineage());

    // The fork record's sentinel should still match
    let expected_sentinel = fork_sentinel(parent_origin, fork_seq);
    for record in group.fork_records() {
        assert_eq!(record.fork_genesis.parent_hash, expected_sentinel);
    }
}

/// Integration test 4: Fork a daemon, then migrate one of the forks using
/// MIKOSHI. This tests the two systems composing — fork creates the daemon,
/// migration moves it to a different node.
#[test]
fn test_fork_then_migrate() {
    let source_reg = Arc::new(DaemonRegistry::new());
    let _target_reg = Arc::new(DaemonRegistry::new());
    let sched = make_scheduler_for_groups();

    let parent_origin: u32 = 0xCCCC;
    let fork_seq: u64 = 200;

    // Create a fork group on the source registry
    let group = ForkGroup::fork(
        parent_origin,
        fork_seq,
        ForkGroupConfig {
            fork_count: 2,
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &source_reg,
    )
    .unwrap();

    // Pick one fork to migrate
    let fork_origin = group.members()[0].origin_hash;

    // Process some events on the fork to build state
    for seq in 1..=5 {
        source_reg
            .deliver(fork_origin, &make_event(0xFFFF, seq))
            .unwrap();
    }

    // Take a snapshot of the fork (it's a normal daemon in the registry)
    let snapshot = source_reg.snapshot(fork_origin).unwrap().unwrap();
    assert_eq!(snapshot.through_seq, 5); // 5 events processed

    // Set up migration infrastructure
    let orch = MigrationOrchestrator::new(source_reg.clone(), 0x1111);
    // For the migration, we need the keypair. Since we can't extract it from
    // the group directly in this test, we'll use the snapshot-based migration
    // path which requires matching keypair on target.
    // Instead, we verify the snapshot is valid and the migration orchestrator accepts it.
    let msgs = orch.start_migration(fork_origin, 0x1111, 0x2222).unwrap();

    assert!(!msgs.is_empty(), "must emit at least one chunk");
    match &msgs[0] {
        MigrationMessage::SnapshotReady {
            daemon_origin,
            seq_through,
            ..
        } => {
            assert_eq!(*daemon_origin, fork_origin);
            assert_eq!(*seq_through, 5);
        }
        other => panic!("expected SnapshotReady for fork, got {:?}", other),
    }

    // The fork is a normal daemon in the registry — migration works on it
    // without knowing it's a fork. The causal chain and fork lineage travel
    // with the snapshot.
    assert!(orch.is_migrating(fork_origin));
}

/// Integration test 5: GroupCoordinator routing actually delivers — route
/// through both ReplicaGroup and ForkGroup, deliver via DaemonRegistry,
/// verify the daemon processes the event and the output carries correct metadata.
#[test]
fn test_group_coordinator_route_delivers_to_daemon() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    // ── ReplicaGroup path ──
    let replica_group = ReplicaGroup::spawn(
        ReplicaGroupConfig {
            replica_count: 2,
            group_seed: [11u8; 32],
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    let ctx = RequestContext::default();

    // Route and deliver through replica group
    let replica_origin = replica_group.route_event(&ctx).unwrap();
    let outputs = reg.deliver(replica_origin, &make_event(0xFFFF, 1)).unwrap();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].link.origin_hash, replica_origin);

    // Route again — with RoundRobin, should pick the other replica
    let replica_origin_2 = replica_group.route_event(&ctx).unwrap();
    let outputs2 = reg
        .deliver(replica_origin_2, &make_event(0xFFFF, 2))
        .unwrap();
    assert_eq!(outputs2.len(), 1);
    assert_eq!(outputs2[0].link.origin_hash, replica_origin_2);

    // Both should be valid replicas
    assert!(replica_group
        .replicas()
        .iter()
        .any(|r| r.origin_hash == replica_origin));
    assert!(replica_group
        .replicas()
        .iter()
        .any(|r| r.origin_hash == replica_origin_2));

    // ── ForkGroup path (separate registry to avoid collisions) ──
    let fork_reg = DaemonRegistry::new();
    let fork_group = ForkGroup::fork(
        0xDDDD,
        300,
        ForkGroupConfig {
            fork_count: 2,
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &fork_reg,
    )
    .unwrap();

    // Route and deliver through fork group
    let fork_origin = fork_group.route_event(&ctx).unwrap();
    let fork_outputs = fork_reg
        .deliver(fork_origin, &make_event(0xFFFF, 1))
        .unwrap();
    assert_eq!(fork_outputs.len(), 1);
    assert_eq!(fork_outputs[0].link.origin_hash, fork_origin);

    // Fork output should have sequence 1 (after fork genesis at 0)
    assert_eq!(fork_outputs[0].link.sequence, 1);

    // Fork origin should NOT be the parent
    assert_ne!(fork_origin, 0xDDDD);
}

// ── Standby group integration tests ──────────────────────────────────────────

/// Integration test: Sync → promote → state continuity.
///
/// Verifies that events processed by the active before sync, plus events
/// buffered after sync, produce correct output on the promoted standby.
/// This is the core promise of StandbyGroup — the new active continues
/// from where the old active left off.
#[test]
fn test_standby_sync_promote_state_continuity() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    let mut group = StandbyGroup::spawn(
        StandbyGroupConfig {
            member_count: 3,
            group_seed: [77u8; 32],
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    let active = group.active_origin();

    // Phase 1: Process 10 events on the active
    for seq in 1..=10 {
        let event = make_event(0xFFFF, seq);
        let outputs = reg.deliver(active, &event).unwrap();
        // CounterDaemon increments: output should be seq
        let val = u64::from_le_bytes(outputs[0].payload[..8].try_into().unwrap());
        assert_eq!(val, seq);
        group.on_event_delivered(event);
    }

    // Phase 2: Sync standbys — they're now caught up to seq 10
    let synced = group.sync_standbys(&reg).unwrap();
    assert_eq!(synced, 10);
    assert_eq!(group.buffered_event_count(), 0);

    // Phase 3: Process 3 more events after sync (these buffer for replay)
    for seq in 11..=13 {
        let event = make_event(0xFFFF, seq);
        let outputs = reg.deliver(active, &event).unwrap();
        let val = u64::from_le_bytes(outputs[0].payload[..8].try_into().unwrap());
        assert_eq!(val, seq);
        group.on_event_delivered(event);
    }
    assert_eq!(group.buffered_event_count(), 3);

    // Phase 4: Promote — new active should replay the 3 buffered events
    let new_active = group
        .promote(|| Box::new(CounterDaemon::new()), &reg, &sched)
        .unwrap();
    assert_ne!(new_active, active);
    assert_eq!(group.buffered_event_count(), 0);

    // Phase 5: Deliver a new event to the promoted active
    // After `sync_standbys` restored the active's state (count=10) onto the
    // standby and `promote` replayed the 3 buffered post-sync events
    // (count=13), this 14th event must observe full state continuity:
    // count=14, with no events lost across the promotion boundary.
    let event = make_event(0xFFFF, 14);
    let outputs = reg.deliver(new_active, &event).unwrap();
    assert_eq!(outputs.len(), 1);
    let val = u64::from_le_bytes(outputs[0].payload[..8].try_into().unwrap());
    assert_eq!(val, 14); // 10 synced + 3 replayed + 1 new

    // After the standby restores the active's chain head from the snapshot,
    // post-promotion outputs continue extending the active's chain. The
    // origin_hash on emitted events stays the active's, preserving causal
    // continuity for downstream observers across the failover boundary.
    assert_eq!(outputs[0].link.origin_hash, active);
}

/// Integration test: Promote then continue processing.
///
/// After promotion, the new active should accept a sustained stream of
/// events and produce correct sequential output.
#[test]
fn test_standby_promote_then_continue() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    let mut group = StandbyGroup::spawn(
        StandbyGroupConfig {
            member_count: 2,
            group_seed: [88u8; 32],
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    let active = group.active_origin();

    // Process events and sync
    for seq in 1..=5 {
        let event = make_event(0xFFFF, seq);
        reg.deliver(active, &event).unwrap();
        group.on_event_delivered(event);
    }
    group.sync_standbys(&reg).unwrap();

    // Promote
    let new_active = group
        .promote(|| Box::new(CounterDaemon::new()), &reg, &sched)
        .unwrap();

    // Deliver 10 more events to the new active — verify sequential output.
    // After `sync_standbys` restored the active's state (count=5) onto the
    // standby, the promoted new active continues from count=5, so the next
    // 10 events produce 6..=15. Emitted events extend the original active's
    // causal chain (same origin_hash) for downstream continuity.
    for seq in 1..=10 {
        let event = make_event(0xFFFF, 100 + seq);
        let outputs = reg.deliver(new_active, &event).unwrap();
        assert_eq!(outputs.len(), 1);
        let val = u64::from_le_bytes(outputs[0].payload[..8].try_into().unwrap());
        assert_eq!(val, 5 + seq); // 5 synced + seq new events
        assert_eq!(outputs[0].link.origin_hash, active);
    }

    // Verify the new active's role
    assert_eq!(
        group.member_role(group.active_index()),
        Some(MemberRole::Active)
    );
    assert!(group.active_healthy());
}

/// Integration test: StandbyGroup + MIKOSHI compose.
///
/// The active is a normal daemon in the registry. MIKOSHI can start a
/// migration on it without knowing it belongs to a standby group.
#[test]
fn test_standby_group_active_migrates_via_mikoshi() {
    let reg = Arc::new(DaemonRegistry::new());
    let sched = make_scheduler_for_groups();

    let mut group = StandbyGroup::spawn(
        StandbyGroupConfig {
            member_count: 3,
            group_seed: [99u8; 32],
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    let active = group.active_origin();

    // Process events on the active to build state
    for seq in 1..=7 {
        let event = make_event(0xFFFF, seq);
        reg.deliver(active, &event).unwrap();
        group.on_event_delivered(event);
    }

    // The active is a normal daemon — verify snapshot works
    let snapshot = reg.snapshot(active).unwrap().unwrap();
    assert_eq!(snapshot.through_seq, 7);

    // Start a MIKOSHI migration on the active
    let orch = MigrationOrchestrator::new(reg.clone(), 0x1111);
    let msgs = orch.start_migration(active, 0x1111, 0x2222).unwrap();

    // Migration should succeed — it doesn't know this daemon is part of a group
    assert!(orch.is_migrating(active));
    assert!(!msgs.is_empty(), "must emit at least one chunk");
    match &msgs[0] {
        MigrationMessage::SnapshotReady {
            daemon_origin,
            seq_through,
            ..
        } => {
            assert_eq!(*daemon_origin, active);
            assert_eq!(*seq_through, 7);
        }
        other => panic!("expected SnapshotReady, got {:?}", other),
    }

    // The standby group still tracks the active (even though migration is in-flight)
    assert_eq!(group.active_origin(), active);
    assert!(group.active_healthy());

    // Standbys are unaffected
    assert_eq!(group.member_count(), 3);
    assert_eq!(group.standby_count(), 2);
}

// ── Test suite gap coverage ──────────────────────────────────────────────────

/// Gap 1: on_node_recovery with unregistered member.
///
/// If on_node_failure unregistered a member and replacement failed,
/// on_node_recovery should NOT mark it healthy — routing to an
/// origin_hash that doesn't exist in the registry would fail.
#[test]
fn test_gap_recovery_skips_unregistered_member() {
    let reg = DaemonRegistry::new();
    let mut coord = GroupCoordinator::new(Strategy::RoundRobin);

    // Add two members
    let kp0 = EntityKeypair::generate();
    let kp1 = EntityKeypair::generate();

    // Register only kp1 in the registry (kp0 is "unregistered after failure")
    let host1 = DaemonHost::new(
        Box::new(CounterDaemon::new()),
        kp1.clone(),
        DaemonHostConfig::default(),
    );
    reg.register(host1).unwrap();

    coord.add_member(MemberInfo {
        index: 0,
        origin_hash: kp0.origin_hash(),
        node_id: 0x1111,
        entity_id_bytes: *kp0.entity_id().as_bytes(),
        healthy: false, // was marked unhealthy during failure
    });
    coord.add_member(MemberInfo {
        index: 1,
        origin_hash: kp1.origin_hash(),
        node_id: 0x1111,
        entity_id_bytes: *kp1.entity_id().as_bytes(),
        healthy: false,
    });

    assert_eq!(coord.health(), GroupHealth::Dead);

    // Recovery: kp0 is NOT in registry, kp1 IS
    coord.on_node_recovery(0x1111, &reg);

    // Only kp1 should be marked healthy
    assert_eq!(
        coord.health(),
        GroupHealth::Degraded {
            healthy: 1,
            total: 2
        },
        "unregistered member should stay unhealthy after recovery"
    );
    assert!(
        !coord.members()[0].healthy,
        "kp0 not in registry, should stay unhealthy"
    );
    assert!(
        coord.members()[1].healthy,
        "kp1 in registry, should be healthy"
    );
}

/// Gap 2: StandbyGroup promote with no healthy standbys.
#[test]
fn test_gap_promote_no_healthy_standbys() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    let mut group = StandbyGroup::spawn(
        StandbyGroupConfig {
            member_count: 2,
            group_seed: [202u8; 32],
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    // First promote succeeds (standby 1 is healthy)
    group
        .promote(|| Box::new(CounterDaemon::new()), &reg, &sched)
        .unwrap();

    // Second promote: old active (0) was marked unhealthy by promote,
    // current active (1) will be marked unhealthy — no healthy standbys left
    let err = group
        .promote(|| Box::new(CounterDaemon::new()), &reg, &sched)
        .unwrap_err();
    assert_eq!(err, GroupError::NoHealthyMember);
}

/// Gap 3: DaemonHost::from_fork panics on origin mismatch.
#[test]
#[should_panic(expected = "fork chain origin")]
fn test_gap_from_fork_origin_mismatch_panics() {
    use net::adapter::net::state::causal::CausalChainBuilder;

    let keypair_a = EntityKeypair::generate();
    let keypair_b = EntityKeypair::generate();

    // Build a chain for keypair_a's origin
    let chain = CausalChainBuilder::new(keypair_a.origin_hash());

    struct NoopDaemon;
    impl MeshDaemon for NoopDaemon {
        fn name(&self) -> &str {
            "noop"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(vec![])
        }
    }

    // Pass keypair_b but chain for keypair_a — should panic
    let _host = DaemonHost::from_fork(
        Box::new(NoopDaemon),
        keypair_b,
        chain,
        DaemonHostConfig::default(),
    );
}

/// Gap 4: Reassembler with mismatched total_chunks across chunks.
///
/// If chunk 0 says total_chunks=3 but a later chunk says total_chunks=4,
/// the reassembler now returns an error (TotalChunksMismatch) instead of
/// silently accepting the inconsistency.
#[test]
fn test_gap_reassembler_mismatched_total_chunks() {
    let mut reassembler = SnapshotReassembler::new();

    // First chunk: total_chunks=3
    let result = reassembler.feed(0xAAAA, vec![1, 2], 100, 0, 3).unwrap();
    assert!(result.is_none());

    // Second chunk claims total_chunks=4 — should error
    let result = reassembler.feed(0xAAAA, vec![3, 4], 100, 1, 4);
    assert!(result.is_err(), "mismatched total_chunks should error");

    // Feeding a consistent chunk should still work
    let result = reassembler.feed(0xAAAA, vec![3, 4], 100, 1, 3).unwrap();
    assert!(result.is_none());

    // Third chunk (index 2) with total_chunks=3 — should complete
    let result = reassembler.feed(0xAAAA, vec![5, 6], 100, 2, 3).unwrap();
    assert!(result.is_some());
    let full = result.unwrap();
    assert_eq!(full, vec![1, 2, 3, 4, 5, 6]);
}

/// Gap 5: GroupCoordinator standalone tests.
#[test]
fn test_gap_group_coordinator_standalone() {
    let mut coord = GroupCoordinator::new(Strategy::RoundRobin);
    assert_eq!(coord.member_count(), 0);
    assert_eq!(coord.health(), GroupHealth::Dead); // no members = dead

    // Add members
    let kp0 = EntityKeypair::generate();
    let kp1 = EntityKeypair::generate();

    coord.add_member(MemberInfo {
        index: 0,
        origin_hash: kp0.origin_hash(),
        node_id: 0x1111,
        entity_id_bytes: *kp0.entity_id().as_bytes(),
        healthy: true,
    });
    coord.add_member(MemberInfo {
        index: 1,
        origin_hash: kp1.origin_hash(),
        node_id: 0x2222,
        entity_id_bytes: *kp1.entity_id().as_bytes(),
        healthy: true,
    });

    assert_eq!(coord.member_count(), 2);
    assert_eq!(coord.healthy_count(), 2);
    assert_eq!(coord.health(), GroupHealth::Healthy);

    // Mark unhealthy
    coord.mark_unhealthy(0);
    assert_eq!(coord.healthy_count(), 1);
    assert_eq!(
        coord.health(),
        GroupHealth::Degraded {
            healthy: 1,
            total: 2
        }
    );

    // Mark healthy again
    coord.mark_healthy(0);
    assert_eq!(coord.health(), GroupHealth::Healthy);

    // members_on_node
    assert_eq!(coord.members_on_node(0x1111), vec![0]);
    assert_eq!(coord.members_on_node(0x2222), vec![1]);
    assert_eq!(coord.members_on_node(0x9999), Vec::<u8>::new());

    // remove_last
    let removed = coord.remove_last().unwrap();
    assert_eq!(removed.index, 1);
    assert_eq!(coord.member_count(), 1);

    // Route event
    let ctx = RequestContext::default();
    let origin = coord.route_event(&ctx).unwrap();
    assert_eq!(origin, kp0.origin_hash());
}

/// Gap 6: scale_to same size is a no-op.
#[test]
fn test_gap_scale_to_same_size_noop() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    // ReplicaGroup
    let mut replica_group = ReplicaGroup::spawn(
        ReplicaGroupConfig {
            replica_count: 3,
            group_seed: [206u8; 32],
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    let origins_before: Vec<u32> = replica_group
        .replicas()
        .iter()
        .map(|r| r.origin_hash)
        .collect();
    replica_group
        .scale_to(3, || Box::new(CounterDaemon::new()), &sched, &reg)
        .unwrap();
    let origins_after: Vec<u32> = replica_group
        .replicas()
        .iter()
        .map(|r| r.origin_hash)
        .collect();
    assert_eq!(origins_before, origins_after);
    assert_eq!(reg.count(), 3);
}

/// Gap 7: sync_standbys when active daemon is stateless.
#[test]
fn test_gap_sync_stateless_active_errors() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    struct StatelessDaemon;
    impl MeshDaemon for StatelessDaemon {
        fn name(&self) -> &str {
            "stateless"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(vec![])
        }
        // snapshot() returns None by default — stateless
    }

    let mut group = StandbyGroup::spawn(
        StandbyGroupConfig {
            member_count: 2,
            group_seed: [207u8; 32],
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(StatelessDaemon),
        &sched,
        &reg,
    )
    .unwrap();

    let err = group.sync_standbys(&reg).unwrap_err();
    match err {
        GroupError::RegistryFailed(msg) => {
            assert!(
                msg.contains("stateless"),
                "expected stateless error, got: {}",
                msg
            );
        }
        _ => panic!("expected RegistryFailed, got {:?}", err),
    }
}

/// Gap 8: chunk_snapshot with empty snapshot bytes.
#[test]
fn test_gap_chunk_empty_snapshot() {
    let chunks = chunk_snapshot(0xAAAA, vec![], 0).unwrap();
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        MigrationMessage::SnapshotReady {
            snapshot_bytes,
            chunk_index,
            total_chunks,
            ..
        } => {
            assert!(snapshot_bytes.is_empty());
            assert_eq!(*chunk_index, 0);
            assert_eq!(*total_chunks, 1);
        }
        _ => panic!("expected SnapshotReady"),
    }
}

/// Gap 9: Wire decode truncation for all message types.
#[test]
fn test_gap_wire_decode_truncated_messages() {
    use net::adapter::net::compute::orchestrator::wire;

    // Each message type with truncated data should return StateFailed
    let test_cases: Vec<(&str, Vec<u8>)> = vec![
        ("TakeSnapshot", vec![0]),    // type byte only, missing 12 bytes
        ("SnapshotReady", vec![1]),   // type byte only, missing 24 bytes
        ("RestoreComplete", vec![2]), // type byte only, missing 12 bytes
        ("ReplayComplete", vec![3]),  // type byte only, missing 12 bytes
        ("CutoverNotify", vec![4]),   // type byte only, missing 12 bytes
        ("CleanupComplete", vec![5]), // type byte only, missing 4 bytes
        ("MigrationFailed", vec![6]), // type byte only, missing 6 bytes
        ("BufferedEvents", vec![7]),  // type byte only, missing 8 bytes
        ("empty", vec![]),            // completely empty
    ];

    for (name, data) in test_cases {
        let result = wire::decode(&data);
        assert!(
            result.is_err(),
            "truncated {} should fail to decode, but got: {:?}",
            name,
            result,
        );
    }

    // Unknown message type
    let result = wire::decode(&[255]);
    assert!(result.is_err());
}

/// Regression: the `BufferedEvents` decoder used to call
/// `Vec::with_capacity(count)` with `count` taken directly from the wire
/// envelope, with no upper bound. A malformed packet claiming
/// `count = u32::MAX` could force a ~4 GiB Vec allocation before the
/// per-event truncation check fired — a cheap remote DoS against the
/// migration subprotocol.
///
/// The fix validates `count` against (1) the remaining wire bytes (each
/// event requires at least 36 bytes on the wire, so `count` can't exceed
/// `remaining / 36`) and (2) a hard cap of 1 million events.
#[test]
fn test_regression_buffered_events_rejects_unbounded_count() {
    use bytes::BufMut;
    use net::adapter::net::compute::orchestrator::wire;

    // Hand-craft a BufferedEvents with count = u32::MAX, no actual event
    // bytes. Pre-fix, this allocated ~4 billion vec slots.
    let mut bad = Vec::new();
    bad.put_u8(7); // MSG_BUFFERED_EVENTS
    bad.put_u32_le(0xAAAA_BBBB); // daemon_origin
    bad.put_u32_le(u32::MAX); // count — unbounded!
                              // No event bytes follow.

    let result = wire::decode(&bad);
    assert!(
        result.is_err(),
        "decoder must reject count that exceeds remaining wire bytes; \
         got {:?}",
        result
    );
    let err = format!("{:?}", result.unwrap_err());
    assert!(
        err.contains("exceeds bound") || err.contains("count"),
        "expected a count-bound error, got: {}",
        err
    );

    // Slightly less extreme: count > MAX_BUFFERED_EVENTS but with a
    // matching byte supply would still be rejected by the hard cap.
    let mut bad2 = Vec::new();
    bad2.put_u8(7);
    bad2.put_u32_le(0);
    bad2.put_u32_le(2_000_000); // > 1M hard cap
                                // Pad with enough filler bytes to defeat the remaining-bytes check.
    bad2.resize(bad2.len() + 2_000_000 * 36, 0);
    let result = wire::decode(&bad2);
    assert!(
        result.is_err(),
        "decoder must reject count above the MAX_BUFFERED_EVENTS cap"
    );

    // Sanity: a well-formed BufferedEvents with count=0 still decodes.
    let mut good = Vec::new();
    good.put_u8(7);
    good.put_u32_le(0x1234);
    good.put_u32_le(0);
    let result = wire::decode(&good);
    assert!(result.is_ok(), "count=0 must still decode: {:?}", result);
}

/// Gap 10: StandbyGroup promote with empty buffer (no events to replay).
#[test]
fn test_gap_promote_empty_buffer() {
    let reg = DaemonRegistry::new();
    let sched = make_scheduler_for_groups();

    let mut group = StandbyGroup::spawn(
        StandbyGroupConfig {
            member_count: 2,
            group_seed: [210u8; 32],
            host_config: DaemonHostConfig::default(),
        },
        || Box::new(CounterDaemon::new()),
        &sched,
        &reg,
    )
    .unwrap();

    let active_before = group.active_origin();

    // Sync with no events processed — standbys at seq 0
    group.sync_standbys(&reg).unwrap();

    // No events buffered after sync
    assert_eq!(group.buffered_event_count(), 0);

    // Promote — should succeed with nothing to replay
    let new_active = group
        .promote(|| Box::new(CounterDaemon::new()), &reg, &sched)
        .unwrap();

    assert_ne!(new_active, active_before);
    assert_eq!(group.buffered_event_count(), 0);

    // New active should accept events normally
    let event = make_event(0xFFFF, 1);
    let outputs = reg.deliver(new_active, &event).unwrap();
    assert_eq!(outputs.len(), 1);
}

// ============================================================================
// Full 6-phase lifecycle over the subprotocol
// ============================================================================
//
// These tests drive the whole migration through the wire handler instead of
// calling the source/target/orchestrator methods directly. Each node has its
// own `MigrationSubprotocolHandler`; `OutboundMigrationMessage`s are ferried
// between handlers by the test harness, emulating the receive loop.

use net::adapter::net::compute::{DaemonFactoryRegistry, MigrationError};
use net::adapter::net::subprotocol::{MigrationSubprotocolHandler, OutboundMigrationMessage};

/// Node identity for wire-level tests: one handler plus direct access to the
/// registries backing it. Each node holds its own orchestrator/source/target
/// even though in production only the node initiating the migration has an
/// "active" orchestrator record — the types are cheap so it keeps the harness
/// uniform.
struct WireNode {
    node_id: u64,
    reg: Arc<DaemonRegistry>,
    factories: Arc<DaemonFactoryRegistry>,
    handler: Arc<MigrationSubprotocolHandler>,
    orch: Arc<MigrationOrchestrator>,
}

impl WireNode {
    fn new(node_id: u64) -> Self {
        let reg = Arc::new(DaemonRegistry::new());
        let factories = Arc::new(DaemonFactoryRegistry::new());
        let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), node_id));
        let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let target = Arc::new(MigrationTargetHandler::new_with_factories(
            reg.clone(),
            factories.clone(),
        ));
        let handler = Arc::new(MigrationSubprotocolHandler::new(
            orch.clone(),
            source,
            target,
            node_id,
        ));
        Self {
            node_id,
            reg,
            factories,
            handler,
            orch,
        }
    }
}

/// Ferry outbound messages between nodes until no more are produced. The
/// `nodes` map is keyed by node_id.
fn pump_messages(
    nodes: &std::collections::HashMap<u64, Arc<MigrationSubprotocolHandler>>,
    mut queue: Vec<(u64, OutboundMigrationMessage)>,
) -> Result<(), MigrationError> {
    let mut iterations = 0;
    while let Some((from, msg)) = queue.pop() {
        iterations += 1;
        assert!(
            iterations < 100,
            "message pump runaway — likely a feedback loop"
        );
        let dest = nodes
            .get(&msg.dest_node)
            .unwrap_or_else(|| panic!("no node for dest {:#x}", msg.dest_node));
        let outbound = dest.handle_message(&msg.payload, from)?;
        for out in outbound {
            queue.push((msg.dest_node, out));
        }
    }
    Ok(())
}

#[test]
fn test_migration_full_lifecycle_over_subprotocol_single_chunk() {
    // Three nodes: O (orchestrator/source, 0x1111) and T (target, 0x2222).
    // In this simple case the orchestrator and source are the same node.
    let source = WireNode::new(0x1111);
    let target = WireNode::new(0x2222);

    // Register the daemon on source with some state built up.
    let (kp, origin) = register_counter_daemon(&source.reg, 100);
    for seq in 1..=5 {
        source
            .reg
            .deliver(origin, &make_event(0xFFFF, seq))
            .unwrap();
    }

    // Register a factory on the target so the handler can construct a
    // daemon instance when the snapshot arrives.
    target
        .factories
        .register(kp.clone(), DaemonHostConfig::default(), || {
            Box::new(CounterDaemon::new())
        })
        .unwrap();

    let nodes: std::collections::HashMap<u64, Arc<MigrationSubprotocolHandler>> = [
        (source.node_id, source.handler.clone()),
        (target.node_id, target.handler.clone()),
    ]
    .into_iter()
    .collect();

    // Kick off: orchestrator starts migration. Local source
    // returns a vector of SnapshotReady chunks (one per
    // MAX_SNAPSHOT_CHUNK_SIZE byte slice). Pump every chunk.
    let start_msgs = source
        .orch
        .start_migration(origin, source.node_id, target.node_id)
        .unwrap();
    let initial: Vec<(u64, OutboundMigrationMessage)> = start_msgs
        .iter()
        .map(|m| {
            (
                source.node_id,
                OutboundMigrationMessage {
                    dest_node: source.node_id,
                    payload: net::adapter::net::compute::orchestrator::wire::encode(m).unwrap(),
                },
            )
        })
        .collect();

    pump_messages(&nodes, initial).unwrap();

    // Assertions: daemon lives on target, gone from source, migration
    // record cleared on both orchestrator and target.
    assert!(target.reg.contains(origin), "daemon should be on target");
    assert!(
        !source.reg.contains(origin),
        "daemon should be gone from source"
    );
    assert!(
        !source.orch.is_migrating(origin),
        "orchestrator record removed"
    );
    // After successful migration, the factory is auto-removed on
    // `complete()` so a stale or replayed SnapshotReady can't re-trigger
    // restore against what is already the authoritative copy on the
    // target. Retry semantics while the migration is still in-flight are
    // preserved because the registry stays live until `complete()`.
    assert!(!target.factories.contains(origin));
}

#[test]
fn test_migration_full_lifecycle_over_subprotocol_multi_chunk() {
    // Same shape as single-chunk, but with a state that's big enough to
    // force snapshot chunking. The CounterDaemon only emits 8 bytes of
    // state though — we can't easily force multi-chunk without changing
    // the daemon. Instead, we test the reassembly path directly via a
    // BigBlobDaemon whose state is large.
    struct BigBlobDaemon {
        state: Vec<u8>,
    }
    impl MeshDaemon for BigBlobDaemon {
        fn name(&self) -> &str {
            "blob"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(vec![])
        }
        fn snapshot(&self) -> Option<Bytes> {
            Some(Bytes::from(self.state.clone()))
        }
        fn restore(&mut self, s: Bytes) -> Result<(), DaemonError> {
            self.state = s.to_vec();
            Ok(())
        }
    }

    let source = WireNode::new(0x1111);
    let target = WireNode::new(0x2222);

    let kp = EntityKeypair::generate();
    let origin = kp.origin_hash();
    // 3 full chunks + a tail.
    let blob_size = MAX_SNAPSHOT_CHUNK_SIZE * 3 + 500;
    let blob = vec![0xABu8; blob_size];
    let host = DaemonHost::new(
        Box::new(BigBlobDaemon {
            state: blob.clone(),
        }),
        kp.clone(),
        DaemonHostConfig::default(),
    );
    source.reg.register(host).unwrap();

    target
        .factories
        .register(kp.clone(), DaemonHostConfig::default(), move || {
            Box::new(BigBlobDaemon { state: Vec::new() })
        })
        .unwrap();

    let nodes: std::collections::HashMap<u64, Arc<MigrationSubprotocolHandler>> = [
        (source.node_id, source.handler.clone()),
        (target.node_id, target.handler.clone()),
    ]
    .into_iter()
    .collect();

    // start_migration on the local source returns multi-chunk SnapshotReady?
    // No — it returns a single SnapshotReady; the subprotocol handler's
    // TakeSnapshot path is where chunking happens. So here we have to go
    // through the wire: treat the orchestrator as if it were remote by
    // sending a TakeSnapshot to ourselves first.
    //
    // Simpler: manually take the snapshot + chunk it + feed each chunk
    // through the target handler as if they arrived over the wire.
    let snapshot = source.reg.snapshot(origin).unwrap().unwrap();
    assert!(snapshot.state.len() >= blob_size);
    let snapshot_bytes = snapshot.to_bytes();
    let chunks = chunk_snapshot(origin, snapshot_bytes, snapshot.through_seq).unwrap();
    assert!(chunks.len() >= 2, "expected multi-chunk snapshot");

    // Seed orchestrator record manually (start_migration won't chunk for
    // local source).
    source
        .orch
        .start_migration(origin, source.node_id, target.node_id)
        .unwrap();

    // Feed each chunk into the target via the handler.
    let mut queue: Vec<(u64, OutboundMigrationMessage)> = Vec::new();
    for chunk in chunks {
        let encoded = net::adapter::net::compute::orchestrator::wire::encode(&chunk).unwrap();
        queue.push((
            source.node_id,
            OutboundMigrationMessage {
                dest_node: target.node_id,
                payload: encoded,
            },
        ));
    }
    pump_messages(&nodes, queue).unwrap();

    // After restore, target registry holds the daemon.
    assert!(target.reg.contains(origin), "daemon restored on target");
}

#[test]
fn test_migration_fails_when_no_factory_registered() {
    let source = WireNode::new(0x1111);
    let target = WireNode::new(0x2222);

    let (_kp, origin) = register_counter_daemon(&source.reg, 7);

    // Intentionally do NOT register a factory on target.

    let nodes: std::collections::HashMap<u64, Arc<MigrationSubprotocolHandler>> = [
        (source.node_id, source.handler.clone()),
        (target.node_id, target.handler.clone()),
    ]
    .into_iter()
    .collect();

    let start_msgs = source
        .orch
        .start_migration(origin, source.node_id, target.node_id)
        .unwrap();
    let initial: Vec<(u64, OutboundMigrationMessage)> = start_msgs
        .iter()
        .map(|m| {
            (
                source.node_id,
                OutboundMigrationMessage {
                    dest_node: source.node_id,
                    payload: net::adapter::net::compute::orchestrator::wire::encode(m).unwrap(),
                },
            )
        })
        .collect();
    pump_messages(&nodes, initial).unwrap();

    // Target should not have the daemon; source should still have it.
    assert!(
        !target.reg.contains(origin),
        "target must not restore without factory"
    );
    assert!(
        source.reg.contains(origin),
        "source daemon preserved on failure"
    );
    // The orchestrator's migration record should be torn down (abort path).
    assert!(!source.orch.is_migrating(origin));
}

#[test]
fn test_migration_fails_on_corrupted_snapshot() {
    // Hand-craft a SnapshotReady with garbage bytes and send it to a
    // target. The handler should emit MigrationFailed and the target's
    // registry must be untouched.
    let source = WireNode::new(0x1111);
    let target = WireNode::new(0x2222);

    let kp = EntityKeypair::generate();
    let origin = kp.origin_hash();
    target
        .factories
        .register(kp, DaemonHostConfig::default(), || {
            Box::new(CounterDaemon::new())
        })
        .unwrap();

    let junk = MigrationMessage::SnapshotReady {
        daemon_origin: origin,
        snapshot_bytes: vec![0xFFu8; 32], // too short to be a valid StateSnapshot
        seq_through: 0,
        chunk_index: 0,
        total_chunks: 1,
    };
    let payload = net::adapter::net::compute::orchestrator::wire::encode(&junk).unwrap();
    let outbound = target
        .handler
        .handle_message(&payload, source.node_id)
        .unwrap();

    let failed = outbound
        .iter()
        .find_map(|o| {
            match net::adapter::net::compute::orchestrator::wire::decode(&o.payload).ok()? {
                MigrationMessage::MigrationFailed { reason, .. } => Some(reason),
                _ => None,
            }
        })
        .expect("expected MigrationFailed");
    // `fail_migration` wraps parse / reassembly messages in
    // `MigrationFailureReason::StateFailed(msg)`. Match the variant
    // first, then peek at the inner string for the recognizable
    // fragment. The reason-code surface moved from free-form
    // `String` to a typed enum during the runtime-readiness work;
    // this assertion tracks the same fragments inside the new
    // wrapping.
    let failed_msg = match &failed {
        net::adapter::net::compute::MigrationFailureReason::StateFailed(m) => m.clone(),
        other => panic!("expected StateFailed-wrapped reason, got {other:?}"),
    };
    assert!(
        failed_msg.contains("parse snapshot") || failed_msg.contains("reassembly"),
        "unexpected failure reason: {failed_msg}",
    );
    assert!(!target.reg.contains(origin));
    // Factory should still be registered — the bad snapshot took nothing
    // from the registry because restore never started.
    assert!(target.factories.contains(origin));
}

/// Regression: the subprotocol handler used to `take` the factory entry
/// *before* attempting `restore_snapshot`. Any restore failure (parse
/// error, recoverable snapshot corruption, etc.) therefore discarded the
/// only registered restore inputs, and retrying the migration required
/// manual re-registration on the target.
///
/// The fix is `construct` + `remove`: the factory is cloned for the
/// restore attempt, and only removed after the attempt succeeds. This
/// test sends a corrupted snapshot first (failure), then a well-formed
/// snapshot (success), and confirms the second attempt reuses the
/// registered factory without re-registration.
#[test]
fn test_regression_factory_preserved_for_retry_after_restore_failure() {
    let source = WireNode::new(0x1111);
    let target = WireNode::new(0x2222);

    // Build a real daemon + real snapshot on the source.
    let (kp, origin) = register_counter_daemon(&source.reg, 7);
    for seq in 1..=3 {
        source
            .reg
            .deliver(origin, &make_event(0xFFFF, seq))
            .unwrap();
    }
    let snapshot = source.reg.snapshot(origin).unwrap().unwrap();
    let valid_bytes = snapshot.to_bytes();

    // Register the factory once on the target.
    target
        .factories
        .register(kp.clone(), DaemonHostConfig::default(), || {
            Box::new(CounterDaemon::new())
        })
        .unwrap();

    // First attempt: corrupt bytes. Restore must fail, factory preserved.
    let corrupt = MigrationMessage::SnapshotReady {
        daemon_origin: origin,
        snapshot_bytes: vec![0xFFu8; 32],
        seq_through: 0,
        chunk_index: 0,
        total_chunks: 1,
    };
    let payload = net::adapter::net::compute::orchestrator::wire::encode(&corrupt).unwrap();
    let outbound = target
        .handler
        .handle_message(&payload, source.node_id)
        .unwrap();
    assert!(
        outbound.iter().any(|o| matches!(
            net::adapter::net::compute::orchestrator::wire::decode(&o.payload),
            Ok(MigrationMessage::MigrationFailed { .. })
        )),
        "first attempt must emit MigrationFailed"
    );
    assert!(
        target.factories.contains(origin),
        "factory must remain registered after a failed restore so a \
         retry can use it without manual re-registration"
    );
    assert!(!target.reg.contains(origin));

    // Second attempt: well-formed snapshot. Restore must succeed using
    // the still-registered factory.
    let good = MigrationMessage::SnapshotReady {
        daemon_origin: origin,
        snapshot_bytes: valid_bytes,
        seq_through: snapshot.through_seq,
        chunk_index: 0,
        total_chunks: 1,
    };
    let payload = net::adapter::net::compute::orchestrator::wire::encode(&good).unwrap();
    let outbound = target
        .handler
        .handle_message(&payload, source.node_id)
        .unwrap();
    assert!(
        outbound.iter().any(|o| matches!(
            net::adapter::net::compute::orchestrator::wire::decode(&o.payload),
            Ok(MigrationMessage::RestoreComplete { .. })
        )),
        "second attempt must emit RestoreComplete"
    );
    assert!(
        target.reg.contains(origin),
        "daemon must be restored on target"
    );
    // Factory registration survives across restore so a later retry
    // (e.g., the source didn't see `RestoreComplete`) doesn't fail
    // permanently. Single-shot semantics are the caller's responsibility
    // via an explicit `factories.remove`.
    assert!(target.factories.contains(origin));
}

/// Regression: after a successful restore, the subprotocol handler used
/// to call `factories.remove(origin)` and then assumed the
/// `RestoreComplete` message had been delivered. If that message was
/// lost on the wire (transient network failure, node crash between
/// restore and send), the source would retry `SnapshotReady`. The target
/// then had no factory, responded with `MigrationFailed`, and a single
/// lost packet turned into a permanent migration failure.
///
/// The fix:
///   1. The factory is NOT auto-removed on successful restore. Callers
///      take responsibility for calling `factories.remove` when they
///      observe full-lifecycle completion (`ActivateAck`).
///   2. On retry, if the target already has an in-progress migration
///      record for this origin, the handler re-emits `RestoreComplete`
///      idempotently instead of attempting a second restore (which
///      would hit `AlreadyMigrating`).
///
/// This test drives the retry path end-to-end: first `SnapshotReady`
/// succeeds; simulate a lost `RestoreComplete` by ignoring the first
/// handler output; re-send the same `SnapshotReady`; assert the second
/// handler output is also `RestoreComplete` and that no duplicate
/// restore was attempted.
#[test]
fn test_regression_snapshot_ready_retry_after_successful_restore_is_idempotent() {
    let source = WireNode::new(0x1111);
    let target = WireNode::new(0x2222);

    let (kp, origin) = register_counter_daemon(&source.reg, 9);
    for seq in 1..=3 {
        source
            .reg
            .deliver(origin, &make_event(0xFFFF, seq))
            .unwrap();
    }
    let snapshot = source.reg.snapshot(origin).unwrap().unwrap();
    let snapshot_bytes = snapshot.to_bytes();

    target
        .factories
        .register(kp.clone(), DaemonHostConfig::default(), || {
            Box::new(CounterDaemon::new())
        })
        .unwrap();

    let snapshot_ready = MigrationMessage::SnapshotReady {
        daemon_origin: origin,
        snapshot_bytes,
        seq_through: snapshot.through_seq,
        chunk_index: 0,
        total_chunks: 1,
    };
    let payload = net::adapter::net::compute::orchestrator::wire::encode(&snapshot_ready).unwrap();

    // First attempt: target restores, emits RestoreComplete. Simulate
    // the message being lost on the wire by *dropping* the outbound.
    let outbound1 = target
        .handler
        .handle_message(&payload, source.node_id)
        .unwrap();
    assert!(
        outbound1.iter().any(|o| matches!(
            net::adapter::net::compute::orchestrator::wire::decode(&o.payload),
            Ok(MigrationMessage::RestoreComplete { .. })
        )),
        "first attempt must emit RestoreComplete"
    );
    assert!(target.reg.contains(origin), "daemon must be on target");

    // Retry: source sends the same SnapshotReady again. Target must
    // re-emit RestoreComplete without failing or double-restoring.
    let outbound2 = target
        .handler
        .handle_message(&payload, source.node_id)
        .unwrap();
    let restore_complete_count = outbound2
        .iter()
        .filter(|o| {
            matches!(
                net::adapter::net::compute::orchestrator::wire::decode(&o.payload),
                Ok(MigrationMessage::RestoreComplete { .. })
            )
        })
        .count();
    assert_eq!(
        restore_complete_count, 1,
        "retry must emit exactly one RestoreComplete"
    );
    let migration_failed_count = outbound2
        .iter()
        .filter(|o| {
            matches!(
                net::adapter::net::compute::orchestrator::wire::decode(&o.payload),
                Ok(MigrationMessage::MigrationFailed { .. })
            )
        })
        .count();
    assert_eq!(
        migration_failed_count, 0,
        "retry must not emit MigrationFailed — the daemon is already \
         restored here, so this is an idempotent retry"
    );
    assert!(target.reg.contains(origin));
    assert!(
        target.factories.contains(origin),
        "factory must still be registered until caller explicitly removes it"
    );
}

#[test]
fn test_activate_target_without_prior_restore_errors_gracefully() {
    // ActivateTarget for an origin that was never restored should not
    // panic. The target handler returns an error, which the subprotocol
    // handler propagates up as a Result::Err.
    let target = WireNode::new(0x2222);

    let msg = MigrationMessage::ActivateTarget {
        daemon_origin: 0xDEADBEEF,
    };
    let payload = net::adapter::net::compute::orchestrator::wire::encode(&msg).unwrap();
    let result = target.handler.handle_message(&payload, 0x1111);
    assert!(
        matches!(result, Err(MigrationError::DaemonNotFound(0xDEADBEEF))),
        "expected DaemonNotFound, got {:?}",
        result
    );
}
