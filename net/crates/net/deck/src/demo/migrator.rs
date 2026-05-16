//! Phase 3 of `DECK_DEMO_PLAN.md` — real migrations.
//!
//! Registers a `"migratable"` factory on every node's
//! `DaemonRuntime` and runs a background tokio task that
//! spawns a fresh daemon on node[0] every ~30 s and calls
//! `start_migration` to ship it to a rotating target peer.
//! The substrate's `MigrationOrchestrator` drives the
//! 6-phase machine; the `OrchestratorMigrationSnapshotSource`
//! wired into each node's `MeshOsDaemonSdk` at boot folds the
//! in-flight state into `snapshot.in_flight_migrations`. The
//! deck's MIGRATIONS tab consumes that field naturally.
//!
//! New daemons each cycle so the loop doesn't have to track
//! where the daemon "is" after each successful migration — a
//! v2 enhancement.

use std::time::Duration;

use bytes::Bytes;
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::compute::{
    CausalEvent, DaemonHostConfig, DaemonRuntime, MeshDaemon as ComputeMeshDaemon,
};
use net_sdk::identity::Identity;
// Two re-exports of the substrate's compute `DaemonError`:
// - `net_sdk::meshos::DaemonError` — the trait's `process`
//   return type (substrate-internal name surfaces here).
// - `net_sdk::ComputeDaemonError` — the crate-root alias the
//   migration loop pattern-matches on for `Migration*` variants.
// Same underlying enum; the two names exist to disambiguate
// from the meshos-layer SdkError on the public surface.
use net_sdk::meshos::DaemonError as TraitDaemonError;
use net_sdk::meshos::NodeId;
use net_sdk::testing::ClusterHarness;
use net_sdk::ComputeDaemonError;

/// Factory `kind` the migration loop registers on every
/// node's DaemonRuntime. The string is internal to the demo —
/// the deck doesn't observe it.
const MIGRATABLE_KIND: &str = "demo.migratable";

/// Wait between migration cycles. Per
/// `DECK_DEMO_PLAN.md`'s Phase 3 cadence ("every ~30 s")
/// — long enough that the operator sees the migration progress
/// through phases on screen, short enough that the
/// MIGRATIONS tab is rarely fully empty.
const MIGRATION_CYCLE_INTERVAL: Duration = Duration::from_secs(30);

/// Inter-migration delay before checking phase completion.
/// Gives the substrate's 6-phase state machine room to
/// transition without the next cycle racing.
const MIGRATION_COMPLETION_WAIT: Duration = Duration::from_secs(8);

/// Compute-layer daemon used as the migration subject. Stateless,
/// no inbound processing — the substrate's migration path only
/// needs the daemon to be registered + spawnable, not to do work.
struct MigratableDaemon;

impl ComputeMeshDaemon for MigratableDaemon {
    fn name(&self) -> &str {
        "demo_migratable"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, TraitDaemonError> {
        Ok(vec![])
    }
}

/// Register `demo.migratable` on every node's DaemonRuntime.
/// Idempotent — `register_factory` rejects duplicate kinds on
/// the same runtime, so callers must invoke this exactly once
/// per harness lifetime. Returns `Ok(())` on success.
pub fn install_factories(harness: &ClusterHarness) -> Result<(), color_eyre::Report> {
    for (i, node) in harness.nodes().iter().enumerate() {
        let rt = node
            .daemon_runtime
            .as_ref()
            .ok_or_else(|| color_eyre::eyre::eyre!("node[{i}] daemon_runtime missing"))?;
        rt.register_factory(MIGRATABLE_KIND, || Box::new(MigratableDaemon))
            .map_err(|e| {
                color_eyre::eyre::eyre!("register_factory on node[{i}]: {e:?}")
            })?;
    }
    Ok(())
}

/// Spawn the migration loop task. The returned `JoinHandle`
/// must live for the harness session — drop it on shutdown to
/// abort the loop.
pub fn spawn_loop(harness: &ClusterHarness) -> tokio::task::JoinHandle<()> {
    // Snapshot the per-node DaemonRuntime + NodeId set the loop
    // needs. Cloning DaemonRuntime is cheap (Arc-shared).
    let runtimes: Vec<DaemonRuntime> = harness
        .nodes()
        .iter()
        .filter_map(|n| n.daemon_runtime.clone())
        .collect();
    let node_ids: Vec<NodeId> = harness.nodes().iter().map(|n| n.node_id).collect();
    let total = node_ids.len();
    tokio::spawn(async move {
        run_loop(runtimes, node_ids, total).await;
    })
}

async fn run_loop(runtimes: Vec<DaemonRuntime>, node_ids: Vec<NodeId>, total: usize) {
    if total < 2 {
        // Migrations need at least 2 nodes; the demo always
        // boots 5 but guard against future single-node use.
        return;
    }
    let source_rt = runtimes[0].clone();
    let source_node_id = node_ids[0];
    let mut cycle = 0u64;
    loop {
        // Cycle target: round-robin through node[1..N]. node[0]
        // is the canonical source so the demo keeps a stable
        // anchor in the topology.
        let target_idx = 1 + (cycle as usize % (total - 1));
        let target_node_id = node_ids[target_idx];

        // Spawn a fresh daemon on node[0] for this cycle.
        let identity = Identity::generate();
        let origin_hash = identity.keypair().origin_hash();
        match source_rt
            .spawn(MIGRATABLE_KIND, identity, DaemonHostConfig::default())
            .await
        {
            Ok(_handle) => {
                // Immediately migrate it. The orchestrator
                // records the in-flight state; the
                // MigrationSnapshotSource picks it up on the
                // next snapshot publish.
                let migrate_result = source_rt
                    .start_migration(origin_hash, source_node_id, target_node_id)
                    .await;
                match migrate_result {
                    Ok(_h) => {
                        // Migration in flight. Wait for the
                        // 6-phase state machine to settle
                        // before the next cycle.
                        tokio::time::sleep(MIGRATION_COMPLETION_WAIT).await;
                    }
                    Err(ComputeDaemonError::Migration(_))
                    | Err(ComputeDaemonError::MigrationFailed(_)) => {
                        // Cross-node UDP migration on loopback
                        // is best-effort; some attempts may fail
                        // when the target's DaemonRuntime hasn't
                        // finished accepting yet. Don't kill the
                        // loop — log via stderr and try the
                        // next cycle.
                        eprintln!(
                            "[deck demo] migration cycle {cycle} \
                             from node[0]->node[{target_idx}] failed; \
                             continuing"
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "[deck demo] migration cycle {cycle} unexpected error: \
                             {e:?}"
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "[deck demo] migration cycle {cycle} spawn failed: {e:?}"
                );
            }
        }
        // Wait the cycle interval before the next attempt.
        tokio::time::sleep(MIGRATION_CYCLE_INTERVAL).await;
        cycle = cycle.wrapping_add(1);
    }
}
