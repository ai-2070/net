//! In-process MeshOS runtime + (optional) static sample fixture.
//!
//! The deck always spawns a live `MeshOsRuntime` so its
//! snapshot reader is wired even when no real cluster is
//! attached. When the `samples` feature is enabled,
//! [`samples::install`] adds a small static fixture — 17
//! synthetic peers (via LocalityProbe/HealthProbe), 11
//! daemons across all four lineage groups — so the deck has
//! something to *monitor*. The samples don't generate
//! events; they're a steady-state fixture the operator
//! observes through the normal snapshot path.
//!
//! Without the feature the runtime stays empty; tabs render
//! their "waiting / no data" states until a real cluster
//! source is wired.

use std::sync::Arc;
use std::time::Duration;

use net_sdk::dataforts::MeshBlobAdapter;
use net_sdk::deck::{AdminVerifier, DeckClient, OperatorIdentity, OperatorRegistry};
use net_sdk::meshos::{
    EntityKeypair, MeshOsConfig, MeshOsDaemonSdk, MigrationSnapshotSource, NodeId,
};

/// Handle returned by [`spawn`]. Hold for the app lifetime;
/// dropping it tears the runtime down.
pub struct Harness {
    /// Keeps the runtime alive. Dropping the SDK shuts the
    /// underlying `MeshOsRuntime` down.
    _sdk: MeshOsDaemonSdk,
    /// Sample daemon handles. Kept on the harness so the
    /// daemons stay registered for the session.
    #[cfg(feature = "samples")]
    _daemons: Vec<net_sdk::meshos::MeshOsDaemonHandle>,
    /// Background log + mesh-event seeder task; the handle is
    /// kept on the harness so the task gets aborted when the
    /// harness drops (tokio cancellation on `JoinHandle::drop`).
    #[cfg(feature = "samples-logs")]
    _log_seeder_task: tokio::task::JoinHandle<()>,
    deck: Arc<DeckClient>,
    /// Registered `MeshBlobAdapter` instances. Samples mode
    /// wires several with distinct activity profiles so the
    /// DATAFORTS tab demonstrates multi-adapter behaviour;
    /// default mode leaves the vec empty until an operator
    /// registers their own. BLOBS reads from whichever adapter
    /// the operator cursors on the DATAFORTS list.
    blob_adapters: Vec<Arc<MeshBlobAdapter>>,
    /// The `this_node` id the substrate runtime was configured
    /// with. The App uses it for placement-based pivots and
    /// admin commits without hardcoding a literal that drifts
    /// from the runtime config.
    this_node: NodeId,
}

impl Harness {
    pub fn deck(&self) -> Arc<DeckClient> {
        Arc::clone(&self.deck)
    }

    /// All registered adapters in registration order. The
    /// DATAFORTS list iterates this; the BLOBS tab's
    /// inventory poller binds to the first one (index 0,
    /// matches DATAFORTS's starting cursor).
    pub fn blob_adapters(&self) -> Vec<Arc<MeshBlobAdapter>> {
        self.blob_adapters.clone()
    }

    /// The runtime's local node id. Plumbed into `App::new`
    /// so the UI never hardcodes a node-id literal.
    pub fn this_node(&self) -> NodeId {
        self.this_node
    }
}

/// Spawn the in-process runtime. With the `samples` feature
/// installs the static sample fixture; otherwise the runtime
/// starts empty.
pub async fn spawn() -> color_eyre::Result<Harness> {
    // Faster tick than the production default so the UI's
    // snapshot refresh feels responsive. `this_node` is a
    // synthetic local id; real-cluster wiring would replace
    // this with the actual node identity.
    let mut cfg = MeshOsConfig::default();
    cfg.this_node = 0x0001;
    cfg.tick_interval = Duration::from_millis(250);
    let this_node = cfg.this_node;
    let dispatcher = Arc::new(net_sdk::meshos::LoggingDispatcher::new());

    // Single operator keypair used for both:
    //  1. the `OperatorRegistry` the verifier checks signatures
    //     against, and
    //  2. the `OperatorIdentity` the `DeckClient` signs with.
    // Threshold=1 — single-operator demo cluster. Real
    // deployments wire a populated registry + a higher M-of-N
    // threshold per `DECK_SDK_PLAN.md`.
    let operator_keypair = EntityKeypair::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(&operator_keypair);
    let verifier = Arc::new(AdminVerifier::new(Arc::new(registry), 1));

    // Synthetic migration source — only installed with the
    // `samples` feature so the MIGRATIONS tab has live data
    // to render in demo mode.
    #[cfg(feature = "samples")]
    let migration_source: Option<Arc<dyn MigrationSnapshotSource>> =
        Some(Arc::new(samples::SampleMigrationSnapshotSource));
    #[cfg(not(feature = "samples"))]
    let migration_source: Option<Arc<dyn MigrationSnapshotSource>> = None;

    let sdk = MeshOsDaemonSdk::start_with_verifier_and_migration_source(
        cfg,
        dispatcher,
        Some(verifier),
        migration_source,
    );

    let identity = OperatorIdentity::from_keypair(operator_keypair);
    let deck = Arc::new(DeckClient::from_runtime(sdk.runtime(), identity));

    #[cfg(feature = "samples")]
    let _daemons = samples::install(&sdk).await?;

    // Real `MeshBlobAdapter` set in samples mode — three
    // instances against in-memory `Redex` handles with
    // distinct disk caps + activity profiles so DATAFORTS
    // can demonstrate the multi-adapter list. Default mode
    // leaves the vec empty; operators wire their own.
    #[cfg(feature = "samples")]
    let blob_adapters = samples::install_blob_adapters().await;
    #[cfg(not(feature = "samples"))]
    let blob_adapters: Vec<Arc<MeshBlobAdapter>> = Vec::new();

    // Synthetic log + mesh-event seeder — fires a steady
    // stream of LogLine events through the runtime so LOGS /
    // FAILURES / AUDIT tabs and the NET.MAP MESH.EVENTS
    // section render representative content offline.
    #[cfg(feature = "samples-logs")]
    let _log_seeder_task = samples_logs::install(sdk.runtime().handle_clone());

    Ok(Harness {
        _sdk: sdk,
        #[cfg(feature = "samples")]
        _daemons,
        #[cfg(feature = "samples-logs")]
        _log_seeder_task,
        deck,
        blob_adapters,
        this_node,
    })
}

/// Static sample fixture — synthetic probes + grouped
/// daemons. No event seeding; the deck observes whatever
/// steady state the runtime + supervisor produce on their
/// own. Compiled in only when the `samples` feature is
/// enabled.
#[cfg(feature = "samples")]
mod samples {
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use net_sdk::capabilities::CapabilityFilter;
    use net_sdk::compute::CausalEvent;
    use net_sdk::dataforts::{publish_blob_ref, BlobAdapter, MeshBlobAdapter, Redex};
    use net_sdk::meshos::{
        ChainId, DaemonError, EntityKeypair, HealthProbe, InventoryProbe, LocalityProbe,
        MeshDaemon, MeshOsDaemonHandle, MeshOsDaemonSdk, MeshOsEvent, MigrationPhaseSnapshot,
        MigrationSnapshot, MigrationSnapshotSource, NodeHealth, NodeId, PeerInventory,
        PlacementIntent, ReplicaUpdate,
    };

    /// Construct three real `MeshBlobAdapter` instances against
    /// in-memory `Redex` handles with distinct configurations
    /// plus activity profiles so DATAFORTS demonstrates the
    /// multi-adapter list. No background ticking — the stores
    /// fire once at startup and the adapter's state stays
    /// steady from there, matching the rest of samples mode's
    /// "fixture, not event seeder" rule.
    pub async fn install_blob_adapters() -> Vec<Arc<MeshBlobAdapter>> {
        let mut out = Vec::new();
        // Primary: 1 TiB cap, the original sample workload.
        out.push(install_blob_adapter_one("deck-samples", 1u64 << 40, 5, 3).await);
        // Cold storage: smaller cap (512 GiB), fewer writes,
        // a few fetches.
        out.push(install_blob_adapter_one("cold-storage", 512u64 << 30, 2, 18).await);
        // Replicated: bigger cap (2 TiB), more stores, no
        // fetches — looks like a write-heavy backing tier.
        out.push(install_blob_adapter_one("replicated", 2u64 << 40, 11, 0).await);
        out
    }

    /// Single-adapter helper: constructs against a new Redex,
    /// stores `stores` synthetic chunks, then fires `fetches`
    /// re-fetches of the first stored blob so the fetch
    /// counter isn't zero.
    async fn install_blob_adapter_one(
        id: &str,
        cap_bytes: u64,
        stores: usize,
        fetches: usize,
    ) -> Arc<MeshBlobAdapter> {
        let redex = Arc::new(Redex::new());
        let adapter = MeshBlobAdapter::new(id, redex).with_disk_capacity(cap_bytes);
        let adapter = Arc::new(adapter);
        // Synthetic payloads — bytes vary so each landing has
        // a distinct content hash. The `id` is hashed into the
        // payload prefix too so different adapters store
        // different chunks (their refcount rings + the
        // resulting BLOBS view stay distinct per adapter).
        let mut stored = Vec::with_capacity(stores);
        for i in 0..stores {
            let payload = format!("{id}/blob-{i:03}-fixture-content").into_bytes();
            if let Ok(blob) =
                publish_blob_ref(adapter.as_ref(), format!("mesh://{id}/{i}"), &payload).await
            {
                stored.push(blob);
            }
        }
        if let Some(blob) = stored.first() {
            for _ in 0..fetches {
                let _ = BlobAdapter::fetch(adapter.as_ref(), blob).await;
            }
        }
        adapter
    }

    /// Synthetic migration snapshot source — returns a static
    /// list of in-flight migrations spread across the
    /// migration phases so the MIGRATIONS tab renders
    /// representative data in samples mode.
    pub struct SampleMigrationSnapshotSource;

    impl MigrationSnapshotSource for SampleMigrationSnapshotSource {
        fn list(&self) -> Vec<MigrationSnapshot> {
            vec![
                MigrationSnapshot {
                    daemon_origin: 0xdaee_0001,
                    phase: MigrationPhaseSnapshot::Snapshot,
                    elapsed_ms: 380,
                    source_node: 0x0001,
                    target_node: 0x0007,
                    age_in_phase_ms: 380,
                    snapshot_bytes: None,
                    retries: 0,
                    progress_pct: Some(10),
                    buffered_events: 12,
                },
                MigrationSnapshot {
                    daemon_origin: 0xdaee_0002,
                    phase: MigrationPhaseSnapshot::Transfer,
                    elapsed_ms: 1_240,
                    source_node: 0x0002,
                    target_node: 0x000A,
                    age_in_phase_ms: 820,
                    snapshot_bytes: Some(48 << 20),
                    retries: 1,
                    progress_pct: Some(30),
                    buffered_events: 47,
                },
                MigrationSnapshot {
                    daemon_origin: 0xdaee_0003,
                    phase: MigrationPhaseSnapshot::Replay,
                    elapsed_ms: 4_870,
                    source_node: 0x0003,
                    target_node: 0x0005,
                    age_in_phase_ms: 1_910,
                    snapshot_bytes: Some(112 << 20),
                    retries: 0,
                    progress_pct: Some(70),
                    buffered_events: 318,
                },
                MigrationSnapshot {
                    daemon_origin: 0xdaee_0004,
                    phase: MigrationPhaseSnapshot::Cutover,
                    elapsed_ms: 12_910,
                    source_node: 0x0004,
                    target_node: 0x0009,
                    age_in_phase_ms: 240,
                    snapshot_bytes: Some(7 << 20),
                    retries: 3,
                    progress_pct: Some(90),
                    buffered_events: 4,
                },
            ]
        }
    }

    /// Stub daemon — `process` is a no-op; everything else
    /// uses trait defaults (health = Healthy). Just exists
    /// so it appears in `snapshot.daemons`.
    struct SampleDaemon {
        name: String,
    }

    impl SampleDaemon {
        fn new(name: impl Into<String>) -> Self {
            Self { name: name.into() }
        }
    }

    impl MeshDaemon for SampleDaemon {
        fn name(&self) -> &str {
            &self.name
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(Vec::new())
        }
    }

    /// 17 sample peers. Ids match `crate::nodes::NODES` so
    /// the `id.label` rendering resolves. Two peers Degraded
    /// so the UI exercises all three health states.
    const PEERS: &[(NodeId, u64, NodeHealth)] = &[
        (0xa96f, 41, NodeHealth::Healthy),
        (0xe9b8, 39, NodeHealth::Healthy),
        (0xe685, 12, NodeHealth::Healthy),
        (0xd4ff, 44, NodeHealth::Healthy),
        (0x3599, 47, NodeHealth::Healthy),
        (0x372b, 88, NodeHealth::Healthy),
        (0xeba8, 244, NodeHealth::Degraded),
        (0x82ee, 92, NodeHealth::Healthy),
        (0xbdda, 85, NodeHealth::Healthy),
        (0x6dfb, 31, NodeHealth::Healthy),
        (0x3c81, 18, NodeHealth::Healthy),
        (0xe068, 162, NodeHealth::Healthy),
        (0xbf44, 29, NodeHealth::Healthy),
        (0xf206, 167, NodeHealth::Healthy),
        (0xf83d, 159, NodeHealth::Healthy),
        (0x6808, 451, NodeHealth::Degraded),
        (0x0fc2, 73, NodeHealth::Healthy),
    ];

    struct SampleLocalityProbe;
    impl LocalityProbe for SampleLocalityProbe {
        fn rtt_samples(&self) -> Vec<(NodeId, Duration)> {
            // RTT values in the PEERS table are *milliseconds*
            // — matches what real cluster probes report and
            // what the snapshot fold stores in `rtt_ms`. The
            // map's radial layout reads `rtt_ms` directly.
            PEERS
                .iter()
                .map(|(id, ms, _)| (*id, Duration::from_millis(*ms)))
                .collect()
        }
    }

    struct SampleHealthProbe;
    impl HealthProbe for SampleHealthProbe {
        fn health_samples(&self) -> Vec<(NodeId, NodeHealth)> {
            PEERS.iter().map(|(id, _, h)| (*id, *h)).collect()
        }
    }

    /// Static inventory fixture — each peer gets a synthetic
    /// resource snapshot keyed off the peer index so the values
    /// vary across the fleet without drifting. The two Degraded
    /// peers also report higher saturation + memory pressure
    /// (matches the on-screen story: hot peers + degraded
    /// health). All peers advertise the same software version
    /// in samples mode; one peer reports as forked-from another
    /// so the fork-origin column has something to render.
    struct SampleInventoryProbe;
    impl InventoryProbe for SampleInventoryProbe {
        fn inventory_samples(&self) -> Vec<(NodeId, PeerInventory)> {
            PEERS
                .iter()
                .enumerate()
                .map(|(i, (id, _, h))| {
                    let degraded = matches!(h, NodeHealth::Degraded);
                    // CPU load avg: degraded peers run hot.
                    let cpu = if degraded {
                        2.4 + (i as f64 * 0.07)
                    } else {
                        0.5 + (i as f64 * 0.13).fract()
                    };
                    // Memory: degraded peers near cap.
                    let mem_used: u64 = if degraded {
                        (62 + (i as u64 % 4)) << 30 // ~62 GB
                    } else {
                        (24 + (i as u64 * 3) % 32) << 30 // 24..56 GB
                    };
                    let mem_total: u64 = 64 << 30;
                    let disk_used: u64 = (256 + (i as u64 * 47) % 512) << 30;
                    let disk_total: u64 = 1u64 << 40; // 1 TiB
                                                      // Saturation: rises with health pressure;
                                                      // degraded peers cross the 0.8 amber/red
                                                      // threshold the LIST tab shades on.
                    let sat: f32 = if degraded {
                        0.82 + ((i as f32 * 0.03) % 0.1)
                    } else {
                        0.22 + ((i as f32 * 0.07) % 0.5)
                    };
                    // Capability set: every peer carries the
                    // base capabilities; specialty peers (the
                    // gpu rig, edge box, and lab bench) advertise
                    // deeper namespaces so the NODE-page caps
                    // tree exercises both the single-chain and
                    // branching renderings.
                    let mut caps = std::collections::BTreeSet::new();
                    caps.insert("compute.daemon".to_string());
                    caps.insert("meshos.health".to_string());
                    // ~every third peer participates as a remote
                    // datafort so the DATAFORTS list has more than
                    // just the local node to render. Overflow on a
                    // subset (every 6th) so the gauge mix isn't
                    // uniform.
                    if i % 3 == 0 {
                        caps.insert("dataforts.blob.storage".to_string());
                    }
                    if degraded || i % 6 == 0 {
                        caps.insert("dataforts.blob.overflow".to_string());
                    }
                    if i % 4 == 0 {
                        caps.insert("greedy.cache".to_string());
                    }
                    match *id {
                        0xbdda => {
                            // gpu-rig: GPU-family compute fanout
                            caps.insert("compute.gpu.cuda".to_string());
                            caps.insert("compute.gpu.tensor".to_string());
                            caps.insert("compute.gpu.rocm".to_string());
                        }
                        0xf83d => {
                            // edge: light sensor suite
                            caps.insert("sensor.lidar".to_string());
                            caps.insert("sensor.temp.cel".to_string());
                        }
                        0x0fc2 => {
                            // lab-bench: full sensor stack
                            caps.insert("sensor.lidar".to_string());
                            caps.insert("sensor.radar.shortwave".to_string());
                            caps.insert("sensor.radar.longwave".to_string());
                            caps.insert("sensor.temp.cel".to_string());
                        }
                        _ => {}
                    }
                    let inv = PeerInventory {
                        cpu_load_1m: Some(cpu),
                        mem_used_bytes: Some(mem_used),
                        mem_total_bytes: Some(mem_total),
                        disk_used_bytes: Some(disk_used),
                        disk_total_bytes: Some(disk_total),
                        saturation_trend: Some(sat),
                        capability_set: caps,
                        software_version: Some("0.17.0".to_string()),
                        // The last peer's fixture demonstrates
                        // the fork-of column: it reports as
                        // forked from peer 0 (0xa96f).
                        forked_from: if i == PEERS.len() - 1 {
                            Some(PEERS[0].0)
                        } else {
                            None
                        },
                    };
                    (*id, inv)
                })
                .collect()
        }
    }

    /// Eight synthetic chains spread across the peer fixture
    /// so the CHAINS tab has data to render under samples
    /// mode. Holders are drawn from `PEERS`; chains are sized
    /// 2–4 to exercise the over/under/ok column tags. The
    /// last chain is intentionally undersized so the
    /// summary line reports `1 under`.
    const CHAINS: &[(ChainId, u32, &[NodeId])] = &[
        (0xc001, 3, &[0xa96f, 0xe9b8, 0x372b]),
        (0xc002, 3, &[0xd4ff, 0x3599, 0xf206]),
        (0xc003, 2, &[0xbdda, 0xe068]),
        (0xc004, 3, &[0x82ee, 0x6dfb, 0x3c81]),
        (0xc005, 4, &[0xa96f, 0xe9b8, 0xe685, 0xd4ff]),
        (0xc006, 3, &[0x372b, 0xeba8, 0xbf44]),
        (0xc007, 2, &[0xe068, 0xf83d]),
        (0xc008, 3, &[0x6808, 0x0fc2]), // intentionally under by 1
    ];

    /// Install probes + register 11 grouped daemons + seed
    /// the snapshot's `replicas` map by publishing
    /// `ReplicaUpdate::Added` + `PlacementIntent` events
    /// through the runtime handle. Awaits the seeding completes
    /// before returning so the App starts against a fully
    /// populated steady-state snapshot rather than a partial
    /// one.
    pub async fn install(
        sdk: &MeshOsDaemonSdk,
    ) -> color_eyre::Result<Vec<MeshOsDaemonHandle>> {
        sdk.runtime()
            .add_locality_probe(Arc::new(SampleLocalityProbe));
        sdk.runtime().add_health_probe(Arc::new(SampleHealthProbe));
        sdk.runtime()
            .add_inventory_probe(Arc::new(SampleInventoryProbe));

        // Replica seeding: fire one ReplicaUpdate per (chain,
        // holder) plus one PlacementIntent per chain. Done
        // inline (no detached spawn) so the harness drop can't
        // race with partial population.
        let handle = sdk.runtime().handle_clone();
        for (chain, desired, holders) in CHAINS {
            let _ = handle
                .publish(MeshOsEvent::PlacementIntent(PlacementIntent {
                    chain: *chain,
                    desired_replicas: *desired,
                }))
                .await;
            for holder in holders.iter() {
                let _ = handle
                    .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                        chain: *chain,
                        holder: *holder,
                    }))
                    .await;
            }
        }

        // 11 daemons across all four lineage groups. The
        // `#suffix` convention in each name is parsed by
        // `crate::lineage` to recover group membership.
        Ok(vec![
            sdk.register_daemon(
                Box::new(SampleDaemon::new("mikoshi")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("telemetry")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("gravity#replica")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("gravity#replica")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("gravity#replica")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("anti_entr#standby")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("anti_entr#standby")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("anti_entr#standby")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("drift_corr#fork@42")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("drift_corr#fork@42")),
                EntityKeypair::generate(),
            )?,
            sdk.register_daemon(
                Box::new(SampleDaemon::new("drift_corr#fork@42")),
                EntityKeypair::generate(),
            )?,
        ])
    }
}

/// Synthetic log + mesh-event seeder. Spawns a tokio task
/// that loops at a fixed cadence publishing
/// `MeshOsEvent::LogLine` records against the runtime
/// handle. Independent of the `samples` feature — the
/// substrate accepts log events whether or not any daemons
/// are registered. Daemon-tagged lines reference
/// synthetic origin hashes (matching the migration
/// fixture's `0xdaee_xxxx` range); node-level lines fall
/// through to the substrate's `node.0x<this_node>` source
/// attribution via the snapshot fold.
///
/// The seeder exits when the runtime closes — `publish`
/// returns `Err(LoopClosed)` once the SDK drops, and the
/// loop breaks. No abort wiring needed; the JoinHandle on
/// the harness keeps the task referenced for the operator
/// session and naturally ends on shutdown.
#[cfg(feature = "samples-logs")]
mod samples_logs {
    use std::time::Duration;

    use net_sdk::meshos::{LogLevel, LogLine, MeshOsEvent, MeshOsHandle};

    /// Spawn the seeder and return its `JoinHandle` for the
    /// harness to hold.
    pub fn install(handle: MeshOsHandle) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { run(handle).await })
    }

    /// Per-tick cadence. Fast enough to fill the LOGS tail in
    /// a few minutes of demo operation; slow enough not to
    /// overwhelm the render path or the substrate's log-ring
    /// rotation.
    const TICK: Duration = Duration::from_millis(700);

    /// Daemon-tagged log fixtures. Each is a `(daemon_id,
    /// level, message)` triple. The daemon ids match the
    /// migration fixture's hex range so cross-tab pivots
    /// from MIGRATIONS → LOGS land on familiar identifiers
    /// when both feature flags are enabled.
    const DAEMON_LOGS: &[(u64, LogLevel, &str)] = &[
        (0xdaee_0001, LogLevel::Info, "snapshot taken (12 events buffered)"),
        (0xdaee_0001, LogLevel::Info, "transfer chunk 1/3 acked"),
        (0xdaee_0002, LogLevel::Info, "transfer started (48 MB)"),
        (0xdaee_0002, LogLevel::Warn, "rtt to target rising (147ms p95)"),
        (0xdaee_0003, LogLevel::Info, "replay buffer drained (318 events)"),
        (0xdaee_0003, LogLevel::Info, "cutover candidate selected"),
        (0xdaee_0004, LogLevel::Info, "cutover acked by target"),
        (0xdaee_0004, LogLevel::Error, "drain deadline elapsed; retrying"),
        (0xdaee_0005, LogLevel::Warn, "saturation crossed 0.85 threshold"),
        (0xdaee_0005, LogLevel::Info, "pressure relief: shedding to peer 0x82ee"),
        (0xdaee_0006, LogLevel::Error, "chunk fetch failed: HashMismatch"),
        (0xdaee_0006, LogLevel::Info, "retrying fetch from alternate adapter"),
        (0xdaee_0007, LogLevel::Info, "fold replayed through seq 4_812_993"),
        (0xdaee_0008, LogLevel::Warn, "backoff window extended to 30s"),
    ];

    /// Node-level (no `daemon_id`) log fixtures. The
    /// substrate stamps `node_id = this_node` on these so
    /// they render as `node.0x<this_node>` in the
    /// MESH.EVENTS section.
    const NODE_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "peer 0xa96f handshake completed"),
        (LogLevel::Info, "placement intent recorded for chain 0xc005"),
        (LogLevel::Warn, "peer 0xeba8 entered Degraded — rtt 244ms"),
        (LogLevel::Info, "freeze gate cleared (operator: 0x7f3a)"),
        (LogLevel::Warn, "avoid list grew past soft cap (52 entries)"),
        (LogLevel::Info, "GC swept 142 quiescent chunks (3.2 GB)"),
        (LogLevel::Error, "inventory probe panicked — skipped this tick"),
        (LogLevel::Info, "snapshot publish: 17 peers, 11 daemons, 8 chains"),
        (LogLevel::Warn, "action queue depth approaching cap (58 / 64)"),
        (LogLevel::Info, "admin commit accepted: drain 0x82ee"),
        (LogLevel::Info, "ICE bundle verified — 1-of-1 operator threshold"),
        (LogLevel::Warn, "peer 0x6808 entered Degraded — rtt 451ms"),
        (LogLevel::Info, "cluster freeze lifted; 0 backlogged commits"),
        (LogLevel::Error, "datafort 0x6dfb: storage adapter unhealthy (95%)"),
    ];

    /// Inner loop. Walks both fixtures in interleaved order
    /// so the LOGS tab gets a steady mix of daemon-tagged
    /// and node-level lines; breaks cleanly when the
    /// substrate's loop closes (publishes start failing).
    async fn run(handle: MeshOsHandle) {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut i = 0usize;
        loop {
            ticker.tick().await;
            // Alternate daemon / node every tick — at TICK
            // = 700ms that's ~85 lines/minute, enough to fill
            // the LOGS tail visibly without burying real
            // operator-driven entries.
            let line = if i.is_multiple_of(2) {
                let (daemon_id, level, msg) = DAEMON_LOGS[(i / 2) % DAEMON_LOGS.len()];
                LogLine {
                    level,
                    daemon_id: Some(daemon_id),
                    message: msg.to_string(),
                }
            } else {
                let (level, msg) = NODE_LOGS[(i / 2) % NODE_LOGS.len()];
                LogLine {
                    level,
                    daemon_id: None,
                    message: msg.to_string(),
                }
            };
            if handle.publish(MeshOsEvent::LogLine(line)).await.is_err() {
                // Loop closed — substrate is shutting down.
                // Exit gracefully; the harness's `_sdk` drop
                // already raced ahead of us.
                break;
            }
            i = i.wrapping_add(1);
        }
    }
}
