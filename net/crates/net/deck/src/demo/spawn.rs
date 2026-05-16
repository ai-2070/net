//! Orchestrates the demo's boot sequence:
//!
//! 1. Build the 5-node `ClusterHarness` (handshakes + bridge
//!    probes done by the harness itself).
//! 2. Register a `NodeAgentDaemon` on every node via the
//!    node's `MeshOsDaemonSdk`. The substrate folds each
//!    registration into its local snapshot's `daemons` set,
//!    so the deck's DAEMONS tab shows them naturally.
//! 3. Spawn one tokio task per node that drives the heartbeat
//!    daemon's `publish_log` at the cadence locked in
//!    `DECK_DEMO_PLAN.md` (~800 ms with jitter).
//! 4. Build a `DeckClient` against node[0]'s `MeshOsRuntime` —
//!    that's the operator's view of the cluster.
//! 5. Return a [`Harness`] that mirrors the shape of
//!    [`crate::runtime::Harness`] so `main.rs` can branch on
//!    the `demo` feature without changing the post-boot
//!    plumbing.

use std::sync::Arc;
use std::time::Duration;

use net_sdk::dataforts::MeshBlobAdapter;
use net_sdk::deck::{AdminVerifier, DeckClient, OperatorIdentity, OperatorRegistry};
use net_sdk::meshos::{
    EntityKeypair, LogLevel, LogLine, MeshOsDaemonHandle, MeshOsEvent, MeshOsHandle, NodeId,
};
use net_sdk::testing::ClusterHarness;
use tokio::task::JoinHandle;

use super::cluster::{build_cluster, DEMO_NODE_COUNT};
use super::daemons::{
    InferenceWorkerDaemon, NodeAgentDaemon, RolloutForgeDaemon, TrainerCanaryDaemon,
};
use super::dataforts::build_adapters;
use super::migrator;
use super::rpc_chatter;
use crate::streams::NrpcTail;

/// Per-node heartbeat cadence. Picks a slightly-staggered base
/// so the 5 nodes don't all emit on the same tick; jitter is
/// added per-emit so the LOGS tab doesn't read as mechanical.
const HEARTBEAT_BASE_INTERVAL: Duration = Duration::from_millis(800);

/// Returned by [`spawn`]. Mirrors `crate::runtime::Harness`'s
/// public surface (`deck` / `blob_adapters` / `this_node`) so
/// `main.rs` doesn't branch on whether it built the
/// single-node or multi-node runtime — only on which `spawn`
/// to call.
pub struct Harness {
    /// The N-node cluster. Owned so its `Drop` runs when the
    /// harness is dropped. The cluster's own Drop emits a hint
    /// if the caller forgot the async shutdown path; we drive
    /// the async shutdown explicitly from main.rs via
    /// [`Self::into_shutdown`]. The field is read only by
    /// `into_shutdown` (via `Option::take`); silence the
    /// dead-code lint until a future slice grows additional
    /// accessors on the cluster.
    #[allow(dead_code)]
    cluster: Option<ClusterHarness>,
    /// One handle per node. Kept on the harness so the daemons
    /// stay registered for the session; dropped on shutdown
    /// (auto-unregisters via `MeshOsDaemonHandle::Drop`).
    _heartbeat_handles: Vec<MeshOsDaemonHandle>,
    /// Handles for the replica / fork / standby trios pinned
    /// to node[0]. Same lifetime semantics as the heartbeat
    /// handles — dropping them on shutdown auto-unregisters.
    _group_handles: Vec<MeshOsDaemonHandle>,
    /// Tokio tasks driving the per-node heartbeat log emits.
    /// Aborted on drop (the harness goes away, tokio drops the
    /// handles, the spawned futures are cancelled).
    _heartbeat_tasks: Vec<JoinHandle<()>>,
    /// Phase 3 migration driver task — spawns a fresh
    /// compute-layer daemon on node[0] every ~30 s and
    /// migrates it to a rotating peer. Aborted on Drop.
    _migration_task: JoinHandle<()>,
    /// Phase 4: typed-RPC responder handles parked on nodes
    /// 0 and 1. Dropping these would unregister the `demo.echo`
    /// service mid-session.
    _rpc_responder_handles: Vec<net_sdk::mesh_rpc::ServeHandle>,
    /// Phase 4: per-requester loop tasks (nodes 2..N).
    /// Aborted on Drop.
    _rpc_requester_tasks: Vec<JoinHandle<()>>,
    /// `DeckClient` anchored on node[0]'s `MeshOsRuntime`. The
    /// deck observes node[0]'s snapshot fold (which includes
    /// the other 4 peers via the bridge probes).
    deck: Arc<DeckClient>,
    /// No blob adapters in Phase 1; Phase 2 wires per-node
    /// in-memory `Redex`-backed adapters.
    blob_adapters: Vec<Arc<MeshBlobAdapter>>,
    /// Node[0]'s 64-bit node id. The deck's UI uses this to
    /// disambiguate "this node" from remote peers.
    this_node: NodeId,
}

impl Harness {
    pub fn deck(&self) -> Arc<DeckClient> {
        Arc::clone(&self.deck)
    }

    pub fn blob_adapters(&self) -> Vec<Arc<MeshBlobAdapter>> {
        self.blob_adapters.clone()
    }

    pub fn this_node(&self) -> NodeId {
        self.this_node
    }

    /// Tear down the cluster cleanly. Awaits every node's
    /// `MeshOsDaemonSdk::shutdown`. Idempotent — calling it
    /// twice is a no-op via `ClusterHarness::shutdown`'s
    /// `shutdown_called` flag.
    pub async fn into_shutdown(mut self) -> color_eyre::Result<()> {
        if let Some(cluster) = self.cluster.take() {
            cluster
                .shutdown()
                .await
                .map_err(|e| color_eyre::eyre::eyre!("cluster shutdown: {e}"))?;
        }
        Ok(())
    }
}

/// Boot the demo cluster. `nrpc_tail` is the deck's shared
/// `NrpcTail` ring that Phase 4's observer bridge pushes into;
/// `main.rs` constructs it and clones one handle into the
/// demo so the deck's NRPC tab reads the same records.
pub async fn spawn(nrpc_tail: NrpcTail) -> color_eyre::Result<Harness> {
    eprintln!(
        "[deck demo] booting {} - node cluster on 127.0.0.1:<ephemeral>",
        DEMO_NODE_COUNT
    );
    let cluster = build_cluster()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("cluster boot: {e}"))?;
    eprintln!(
        "[deck demo] cluster up — {} nodes peered + snapshot folds converged",
        cluster.len()
    );

    // Operator identity. Shared across all nodes so admin
    // commits the deck issues are accepted by every node's
    // verifier.
    let operator_keypair = EntityKeypair::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(&operator_keypair);
    let _verifier = Arc::new(AdminVerifier::new(Arc::new(registry), 1));

    // Register a NodeAgentDaemon per node and start its log-
    // emitter task. The handles are stored on the harness so
    // they outlive this fn; dropping them auto-unregisters.
    let mut heartbeat_handles: Vec<MeshOsDaemonHandle> = Vec::with_capacity(cluster.len());
    let mut heartbeat_tasks: Vec<JoinHandle<()>> = Vec::with_capacity(cluster.len());
    for (i, node) in cluster.nodes().iter().enumerate() {
        let sdk = node
            .sdk
            .as_ref()
            .ok_or_else(|| color_eyre::eyre::eyre!("node[{i}] sdk missing"))?;
        let kp = EntityKeypair::generate();
        let daemon_id = kp.origin_hash();
        let handle = sdk
            .register_daemon(Box::new(NodeAgentDaemon), kp)
            .map_err(|e| color_eyre::eyre::eyre!("register NodeAgentDaemon on node[{i}]: {e}"))?;
        // `MeshOsDaemonHandle::Drop` auto-unregisters, so we
        // park the handle on the harness for the session.
        // Log publishing rides a separately-cloned
        // `MeshOsHandle` (the substrate's event-fan-in API) so
        // the task isn't owned by the daemon handle's
        // lifetime — keeps the per-task abort path
        // independent of the daemon-handle drop.
        heartbeat_handles.push(handle);
        let node_index = i;
        let node_id = node.node_id;
        let mesh_os_handle = sdk.runtime().handle_clone();
        let task = tokio::spawn(async move {
            run_heartbeat_loop(node_index, node_id, daemon_id, mesh_os_handle).await;
        });
        heartbeat_tasks.push(task);
    }

    // Register the group-flavored daemon trios on node[0]'s
    // `MeshOsDaemonSdk`. The substrate's snapshot is local-only,
    // so concentrating the group members on the observed node
    // is what makes the deck's GROUPS / CHAINS / DAEMONS tabs
    // render the full picture. The trade-off: the per-daemon
    // `placement` field on every member reads as node[0] in
    // the demo, which the operator can ignore — the lineage
    // visualization is what we're after for v1. A future slice
    // wires cluster-wide daemon visibility once the substrate
    // grows that primitive.
    let node0 = cluster.nth(0);
    let sdk0 = node0
        .sdk
        .as_ref()
        .ok_or_else(|| color_eyre::eyre::eyre!("node[0] sdk missing"))?;
    let mut group_handles: Vec<MeshOsDaemonHandle> = Vec::with_capacity(9);
    for replica_idx in 0..3 {
        let kp = EntityKeypair::generate();
        let h = sdk0
            .register_daemon(Box::new(InferenceWorkerDaemon), kp)
            .map_err(|e| {
                color_eyre::eyre::eyre!("register InferenceWorkerDaemon[{replica_idx}]: {e}")
            })?;
        group_handles.push(h);
    }
    for fork_idx in 0..3 {
        let kp = EntityKeypair::generate();
        let h = sdk0
            .register_daemon(Box::new(RolloutForgeDaemon), kp)
            .map_err(|e| color_eyre::eyre::eyre!("register RolloutForgeDaemon[{fork_idx}]: {e}"))?;
        group_handles.push(h);
    }
    for standby_idx in 0..3 {
        let kp = EntityKeypair::generate();
        let h = sdk0
            .register_daemon(Box::new(TrainerCanaryDaemon), kp)
            .map_err(|e| {
                color_eyre::eyre::eyre!("register TrainerCanaryDaemon[{standby_idx}]: {e}")
            })?;
        group_handles.push(h);
    }

    // Build the DeckClient anchored on node[0]'s MeshOsRuntime.
    let identity = OperatorIdentity::from_keypair(operator_keypair);
    let deck = Arc::new(DeckClient::from_runtime(sdk0.runtime(), identity));
    let this_node = node0.node_id;

    // Build one MeshBlobAdapter per node (Phase 2 of
    // DECK_DEMO_PLAN.md). Each adapter is in-memory `Redex`-
    // backed; we keep them all on the harness's
    // `blob_adapters` so the deck's BLOBS tail polls every
    // adapter into one merged inventory.
    let blob_adapters = build_adapters(DEMO_NODE_COUNT).await;

    // Phase 3: register the compute-layer migratable factory
    // on every node and kick off the periodic migration
    // driver. The `OrchestratorMigrationSnapshotSource` wired
    // by the cluster harness folds the orchestrator's
    // in-flight state into each node's snapshot.
    migrator::install_factories(&cluster)?;
    let migration_task = migrator::spawn_loop(&cluster);

    // Phase 4: install observer bridges into the shared
    // `NrpcTail` on every node, register typed `demo.echo`
    // responders on nodes 0+1, and spawn requester loops on
    // the remaining nodes.
    rpc_chatter::install_observers(&cluster, nrpc_tail);
    let rpc_responder_handles = rpc_chatter::install_responders(&cluster)?;
    let rpc_requester_tasks = rpc_chatter::spawn_requester_loops(&cluster);

    Ok(Harness {
        cluster: Some(cluster),
        _heartbeat_handles: heartbeat_handles,
        _group_handles: group_handles,
        _heartbeat_tasks: heartbeat_tasks,
        _migration_task: migration_task,
        _rpc_responder_handles: rpc_responder_handles,
        _rpc_requester_tasks: rpc_requester_tasks,
        deck,
        blob_adapters,
        this_node,
    })
}

/// Per-node `NodeAgent` log loop. Emits a fresh AI-inference-
/// flavored log line at a jittered cadence around
/// `HEARTBEAT_BASE_INTERVAL`. The message corpus is small +
/// node-keyed so the LOGS tab reads as live inference traffic
/// (batches dispatched, tokens/s, cache hits, …) rather than
/// identical noise across nodes.
async fn run_heartbeat_loop(
    node_index: usize,
    node_id: NodeId,
    daemon_id: u64,
    handle: MeshOsHandle,
) {
    let messages = inference_corpus();
    let mut tick = 0u64;
    loop {
        // Deterministic-but-varied jitter so the demo doesn't
        // need an RNG dependency. ±150 ms around the base.
        let jitter_ms = ((tick.wrapping_mul(11) ^ node_id) % 300) as i64 - 150;
        let interval = HEARTBEAT_BASE_INTERVAL
            .saturating_add(Duration::from_millis(jitter_ms.max(0) as u64))
            .saturating_sub(Duration::from_millis((-jitter_ms).max(0) as u64));
        tokio::time::sleep(interval).await;
        let template = messages[(tick as usize + node_index) % messages.len()];
        // Templates carry up to two `{}` placeholders that get
        // filled with cheap deterministic numbers — batch ids,
        // ms latencies, tokens/s, etc. Looks like real workload
        // metrics without an RNG dependency.
        let n1 = (tick.wrapping_mul(37) ^ node_id) % 9_999;
        let n2 = ((tick.wrapping_mul(53) ^ (node_id >> 8)) % 480) + 20;
        let message = format!("gpu-{node_index} :: {}", fill_template(template, n1, n2),);
        let line = LogLine {
            level: LogLevel::Info,
            daemon_id: Some(daemon_id),
            message,
        };
        if handle.publish(MeshOsEvent::LogLine(line)).await.is_err() {
            // Loop closed — substrate shutting down. Exit
            // cleanly.
            break;
        }
        tick = tick.wrapping_add(1);
    }
}

/// AI-inference workload corpus — templated log lines so a VC
/// viewer reads the LOGS tab as "this is a real inference
/// fleet doing real work." Each template carries up to two
/// `{}` placeholders for batch-id / latency-ms / tokens-per-
/// second style numbers; `fill_template` substitutes them
/// from deterministic per-tick counters so the demo stays
/// reproducible.
fn inference_corpus() -> &'static [&'static str] {
    &[
        "dispatched batch {} to gpu-0 in {}ms",
        "prefill batch {} completed — tokens/s {}",
        "cache hit rate {}% on prompt-id {}",
        "decode step {} :: ttft {}ms",
        "embedding shard {} flushed to redex ({}KB)",
        "rollout cohort {} — A/B split p99 delta {}ms",
        "tokenizer queue depth {} → 4 :: backpressure clear",
        "kv cache eviction batch {} freed {}MB",
        "scheduler tick — queue depth {}",
        "inference completed for trace {} in {}ms",
        "weights sync verified :: epoch {}",
        "trainer canary sync_through advanced to {}",
        "speculative draft accepted ratio {}% :: batch {}",
        "telemetry flush {} records",
    ]
}

/// Substitute up to two `{}` placeholders left-to-right.
/// Trivial — full `format!` machinery is overkill for the
/// fixed corpus.
fn fill_template(tpl: &str, a: u64, b: u64) -> String {
    let mut out = String::with_capacity(tpl.len() + 12);
    let mut iter = tpl.split("{}");
    let first = iter.next().unwrap_or("");
    out.push_str(first);
    if let Some(rest) = iter.next() {
        out.push_str(&a.to_string());
        out.push_str(rest);
    }
    if let Some(rest) = iter.next() {
        out.push_str(&b.to_string());
        out.push_str(rest);
    }
    out
}

#[cfg(test)]
mod tests {
    //! End-to-end smoke tests for the demo's Phase 1 slice.
    //! Boots the real 5-node cluster + registers the
    //! per-node `NodeAgentDaemon`s and asserts the deck's
    //! observable surfaces line up: snapshot has the 4 remote
    //! peers, each node carries a registered daemon, and the
    //! log loop emits records within a generous budget.
    //!
    //! Slow tests by their nature (UDP loopback handshakes +
    //! tokio sleeps in the log loop). Marked `flavor =
    //! "multi_thread"` so the bridge probes' tick loop runs
    //! independently of the test future.
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn demo_boots_and_logs_appear() {
        let nrpc_tail = NrpcTail::new(1024);
        let harness = spawn(nrpc_tail.clone())
            .await
            .expect("demo spawn must succeed");
        // The deck observes node[0]; its snapshot should
        // include the other 4 peers via the bridge probes
        // and one registered NodeAgentDaemon for itself.
        // Wait long enough for at least a few heartbeat
        // emits (~800 ms base; allow 3 s for headroom).
        tokio::time::sleep(Duration::from_secs(3)).await;
        let snap = harness.deck.status();
        assert_eq!(
            snap.peers.len(),
            8,
            "node[0] snapshot must see 8 remote peers"
        );
        // Node[0] hosts: 1 NodeAgentDaemon + 3 InferenceWorkerDaemons
        // (replica) + 3 RolloutForgeDaemons (fork) + 3 TrainerCanaryDaemons
        // (standby) = 10 registered locally.
        assert_eq!(
            snap.daemons.len(),
            10,
            "node[0] should show 1 heartbeat + 9 group daemons; got {}",
            snap.daemons.len()
        );
        // Verify each group name appears the expected number of
        // times. The deck's `lineage::group_daemons` will fold
        // these into one row per group.
        let count_with_name =
            |name: &str| -> usize { snap.daemons.values().filter(|d| d.name == name).count() };
        assert_eq!(count_with_name("node_agent"), 1);
        assert_eq!(count_with_name("inference_worker#replica"), 3);
        assert_eq!(count_with_name("rollout_forge#fork@7"), 3);
        assert_eq!(count_with_name("trainer_canary#standby"), 3);
        // Logs ride the same fold; expect non-trivial volume.
        assert!(
            snap.log_ring.len() >= 2,
            "log_ring should carry heartbeat lines (got {})",
            snap.log_ring.len()
        );
        // Phase 2: one MeshBlobAdapter per demo node.
        assert_eq!(
            harness.blob_adapters.len(),
            9,
            "demo should wire 9 blob adapters (one per node)"
        );
        // Phase 4: requester loops fire at ~250 ms; after 3 s
        // the observer should have logged a non-trivial number
        // of calls. Generous floor — CI variance + handshake
        // bring-up can eat a few hundred ms before the first
        // calls flow.
        let nrpc_records = nrpc_tail.snapshot();
        assert!(
            !nrpc_records.is_empty(),
            "Phase 4: NrpcTail should carry observer-recorded calls within 3 s"
        );
        // Clean shutdown — the cluster's into_shutdown drains
        // every node's MeshOsDaemonSdk so no tasks leak.
        harness.into_shutdown().await.expect("demo shutdown clean");
    }
}
