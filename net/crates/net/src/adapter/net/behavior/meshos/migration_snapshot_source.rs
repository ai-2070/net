//! Migration-snapshot source seam.
//!
//! The MeshOS event loop calls
//! [`MigrationSnapshotSource::list`] on every snapshot publish
//! and embeds the result in
//! [`super::snapshot::MeshOsSnapshot::in_flight_migrations`].
//! The ICE blast-radius simulator
//! ([`super::ice::simulate`]) reads the same field on the
//! published snapshot to enumerate which daemon a
//! [`super::event::AdminEvent::KillMigration`] target would
//! affect.
//!
//! Mirrors the [`super::migration_aborter`] pattern: trait +
//! NoOp + production adapter wrapping a real
//! [`crate::adapter::net::compute::MigrationOrchestrator`].
//! Tests + bootstrap leave the no-op default installed; the
//! snapshot's `in_flight_migrations` field reads empty.

use std::sync::Arc;

use super::snapshot::{MigrationPhaseSnapshot, MigrationSnapshot};

/// Trait the event loop calls on every snapshot publish. The
/// returned `Vec` is embedded verbatim in the snapshot;
/// production impls keep the call cheap (the
/// [`OrchestratorMigrationSnapshotSource`] adapter is one
/// DashMap iteration).
pub trait MigrationSnapshotSource: Send + Sync + 'static {
    /// List the in-flight migrations this node hosts. Called
    /// once per snapshot publish.
    fn list(&self) -> Vec<MigrationSnapshot>;
}

/// No-op source. The default. Returns an empty `Vec` — the
/// snapshot's `in_flight_migrations` field reads empty unless
/// a production source is wired.
#[derive(Debug, Default)]
pub struct NoOpMigrationSnapshotSource;

impl MigrationSnapshotSource for NoOpMigrationSnapshotSource {
    fn list(&self) -> Vec<MigrationSnapshot> {
        Vec::new()
    }
}

/// Production source — wraps a
/// [`crate::adapter::net::compute::MigrationOrchestrator`] and
/// translates each in-flight migration into a wire-form
/// [`MigrationSnapshot`] for the snapshot's
/// `in_flight_migrations` field.
pub struct OrchestratorMigrationSnapshotSource {
    orchestrator: Arc<crate::adapter::net::compute::MigrationOrchestrator>,
}

impl OrchestratorMigrationSnapshotSource {
    /// Wrap an orchestrator.
    pub fn new(orchestrator: Arc<crate::adapter::net::compute::MigrationOrchestrator>) -> Self {
        Self { orchestrator }
    }
}

impl MigrationSnapshotSource for OrchestratorMigrationSnapshotSource {
    fn list(&self) -> Vec<MigrationSnapshot> {
        self.orchestrator
            .list_migrations()
            .into_iter()
            .map(|item| {
                let phase = MigrationPhaseSnapshot::from(item.phase);
                MigrationSnapshot {
                    daemon_origin: item.daemon_origin,
                    phase,
                    elapsed_ms: item.elapsed_ms,
                    source_node: item.source_node,
                    target_node: item.target_node,
                    age_in_phase_ms: item.age_in_phase_ms,
                    snapshot_bytes: item.snapshot_bytes,
                    retries: item.retries,
                    progress_pct: phase_progress_pct(phase),
                    buffered_events: item.buffered_events,
                }
            })
            .collect()
    }
}

/// Coarse phase-ordinal → percentage projection. Honest about
/// the substrate-side limitation: byte-level progress requires
/// the orchestrator to track `(bytes_done, bytes_total)` per
/// active phase, which isn't wired today. The deck consumes
/// this for an at-a-glance pipeline indicator alongside the
/// PHASE column; finer reporting plugs in here when the
/// orchestrator gains progress callbacks.
fn phase_progress_pct(phase: MigrationPhaseSnapshot) -> Option<u8> {
    // `MigrationPhaseSnapshot` is `#[non_exhaustive]`; the
    // wildcard `_` arm guards against a future variant landing
    // before this function gets updated. The current crate
    // sees all listed variants as exhaustive — the allow
    // silences the same-crate unreachable lint without giving
    // up the cross-crate forward-compat.
    #[allow(unreachable_patterns)]
    Some(match phase {
        MigrationPhaseSnapshot::Snapshot => 10,
        MigrationPhaseSnapshot::Transfer => 30,
        MigrationPhaseSnapshot::Restore => 50,
        MigrationPhaseSnapshot::Replay => 70,
        MigrationPhaseSnapshot::Cutover => 90,
        MigrationPhaseSnapshot::Complete => 100,
        _ => return None,
    })
}

/// Convenient `Arc`-wrapped default; the loop holds an
/// `Arc<dyn MigrationSnapshotSource>` internally.
pub(crate) fn no_op_arc() -> Arc<dyn MigrationSnapshotSource> {
    Arc::new(NoOpMigrationSnapshotSource)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_op_returns_empty_vec() {
        let s = NoOpMigrationSnapshotSource;
        assert!(s.list().is_empty());
    }

    /// Bench-style sanity that the production adapter compiles
    /// against a freshly constructed orchestrator (which has
    /// zero in-flight migrations) and returns the same empty
    /// list. Real integration tests live alongside the
    /// orchestrator + the event-loop wiring.
    #[test]
    fn orchestrator_adapter_returns_empty_for_idle_orchestrator() {
        use crate::adapter::net::compute::{DaemonRegistry, MigrationOrchestrator};
        let registry = Arc::new(DaemonRegistry::new());
        let orch = Arc::new(MigrationOrchestrator::new(registry, 7));
        let s = OrchestratorMigrationSnapshotSource::new(orch);
        assert!(s.list().is_empty());
    }
}
