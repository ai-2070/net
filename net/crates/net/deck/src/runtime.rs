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
    /// Per-daemon log seeder task — emits daemon-tagged log
    /// lines from each thematic daemon's vocabulary so the
    /// LOGS tab shows real per-daemon chatter. Lifetime
    /// bounded to the harness's tokio runtime.
    #[cfg(feature = "samples")]
    _daemon_log_seeder_task: tokio::task::JoinHandle<()>,
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
    let (_daemons, _daemon_log_seeder_task) = samples::install(&sdk).await?;

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
        #[cfg(feature = "samples")]
        _daemon_log_seeder_task,
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
        LogLevel, LogLine, MeshDaemon, MeshOsDaemonHandle, MeshOsDaemonSdk, MeshOsEvent,
        MigrationPhaseSnapshot, MigrationSnapshot, MigrationSnapshotSource, NodeHealth, NodeId,
        PeerInventory, PlacementIntent, ReplicaUpdate,
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
                    // base substrate caps; thematic caps are
                    // assigned per-node-role so each peer
                    // advertises the workloads its physical
                    // role implies. NODE.PAGE caps tree exercises
                    // both single-chain (stage / vehicle nodes)
                    // and branching (GPU rack, sensor rigs)
                    // renderings.
                    let mut caps = std::collections::BTreeSet::new();
                    caps.insert("compute.daemon".to_string());
                    caps.insert("meshos.health".to_string());
                    // VC-readable capability names: every cap
                    // says what the node DOES rather than the
                    // wire protocol / hardware revision. A
                    // non-technical reader pattern-matches on
                    // `robot.arm`, `drone.camera`, `ai.model.chat`
                    // and gets the right mental model without
                    // knowing CAN-FD or BF16 or DMX.
                    match *id {
                        // ── Live event production ──────────────
                        0xa96f => {
                            // main stage — lighting + cue sync.
                            caps.insert("zone:stage".to_string());
                            caps.insert("stage.lighting".to_string());
                            caps.insert("stage.spotlights".to_string());
                            caps.insert("stage.cue-system".to_string());
                            caps.insert("stage.music-sync".to_string());
                        }
                        0xe9b8 => {
                            // side stage — lighter rig.
                            caps.insert("zone:stage".to_string());
                            caps.insert("stage.lighting".to_string());
                            caps.insert("stage.cue-system".to_string());
                        }
                        0xe685 => {
                            // front-of-house audio mix.
                            caps.insert("zone:audio".to_string());
                            caps.insert("audio.concert-mix".to_string());
                            caps.insert("sensor.microphone-array".to_string());
                        }
                        0xd4ff => {
                            // monitor mix — in-ear / wedge feeds.
                            caps.insert("zone:audio".to_string());
                            caps.insert("audio.monitor-mix".to_string());
                        }
                        0x3599 => {
                            // lighting + special effects rig.
                            caps.insert("zone:stage".to_string());
                            caps.insert("stage.lighting".to_string());
                            caps.insert("stage.pyrotechnics".to_string());
                            caps.insert("stage.fog-machine".to_string());
                        }
                        // ── Drone swarm ────────────────────────
                        0x372b => {
                            // ground station — swarm control.
                            caps.insert("zone:hangar".to_string());
                            caps.insert("drone.swarm-control".to_string());
                            caps.insert("drone.flight-controller".to_string());
                        }
                        0xeba8 => {
                            // scout drone — quadcopter, cinema cam.
                            caps.insert("zone:airspace".to_string());
                            caps.insert("drone.quadcopter".to_string());
                            caps.insert("drone.cinema-camera".to_string());
                            caps.insert("sensor.camera".to_string());
                            caps.insert("sensor.gps".to_string());
                        }
                        0x82ee => {
                            // follower drone — hex, thermal + lidar.
                            caps.insert("zone:airspace".to_string());
                            caps.insert("drone.hexacopter".to_string());
                            caps.insert("drone.thermal-camera".to_string());
                            caps.insert("sensor.lidar".to_string());
                        }
                        // ── AI inference cluster ───────────────
                        0xbdda => {
                            // gpu rack A — vision-grasp model.
                            caps.insert("zone:rack".to_string());
                            caps.insert("gpu.nvidia-blackwell".to_string());
                            caps.insert("gpu.tensor-cores".to_string());
                            caps.insert("ai.batch-inference".to_string());
                            caps.insert("ai.vision-grasp-model".to_string());
                        }
                        0x6dfb => {
                            // gpu rack B — chat model, streaming.
                            caps.insert("zone:rack".to_string());
                            caps.insert("gpu.nvidia-blackwell".to_string());
                            caps.insert("gpu.tensor-cores".to_string());
                            caps.insert("ai.live-inference".to_string());
                            caps.insert("ai.chat-model".to_string());
                        }
                        0x3c81 => {
                            // model cache — kv-cache + blob storage.
                            caps.insert("zone:rack".to_string());
                            caps.insert("ai.kv-cache".to_string());
                            caps.insert("dataforts.blob.storage".to_string());
                            caps.insert("greedy.cache".to_string());
                        }
                        // ── Robotics cell ──────────────────────
                        0xe068 => {
                            // 7-axis robot arm with parallel gripper.
                            caps.insert("zone:cell".to_string());
                            caps.insert("robot.7-axis-arm".to_string());
                            caps.insert("robot.gripper".to_string());
                            caps.insert("robot.motion-planning".to_string());
                        }
                        0xbf44 => {
                            // 6-axis arm on gantry — suction pickup.
                            caps.insert("zone:cell".to_string());
                            caps.insert("robot.6-axis-arm".to_string());
                            caps.insert("robot.suction-gripper".to_string());
                        }
                        // ── Autonomous vehicle mesh ────────────
                        0xf206 => {
                            // chase truck — L4 autonomous, full
                            // sensor stack.
                            caps.insert("zone:track".to_string());
                            caps.insert("vehicle.can-bus".to_string());
                            caps.insert("vehicle.autonomy-l4".to_string());
                            caps.insert("vehicle.360-sensor-fusion".to_string());
                            caps.insert("sensor.lidar".to_string());
                            caps.insert("sensor.radar".to_string());
                        }
                        0x6808 => {
                            // pit-lane support — ethercat + motion.
                            caps.insert("zone:pit".to_string());
                            caps.insert("vehicle.ethercat".to_string());
                            caps.insert("sensor.motion-tracker".to_string());
                        }
                        // ── Edge ───────────────────────────────
                        0xf83d => {
                            // edge drone — quad with depth cam.
                            caps.insert("zone:airspace".to_string());
                            caps.insert("drone.quadcopter".to_string());
                            caps.insert("drone.flight-controller".to_string());
                            caps.insert("sensor.depth-camera".to_string());
                        }
                        0x0fc2 => {
                            // vision rig — depth + RGB + grasp AI.
                            caps.insert("zone:cell".to_string());
                            caps.insert("sensor.camera".to_string());
                            caps.insert("sensor.depth-camera".to_string());
                            caps.insert("ai.vision-grasp-model".to_string());
                        }
                        _ => {}
                    }
                    // Greedy-cache + datafort participation is
                    // sprinkled in for the DATAFORTS list to
                    // demonstrate the multi-adapter view. KV-cache
                    // host already carries them above; this layer
                    // adds extra participation so the list isn't
                    // dominated by a single node role.
                    if i % 3 == 0 {
                        caps.insert("dataforts.blob.storage".to_string());
                    }
                    if degraded || i % 6 == 0 {
                        caps.insert("dataforts.blob.overflow".to_string());
                    }
                    if i % 4 == 0 {
                        caps.insert("greedy.cache".to_string());
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

    /// Per-daemon vocabulary table. Each row is a daemon name
    /// + a pool of log lines the per-daemon seeder cycles
    /// through. Five thematic domains across 11 daemons cover
    /// the full lineage matrix:
    ///
    /// - Solo: `stage_cues`, `vision_grasp_ai`
    /// - Replica × 3: `audio_mixer#replica`
    /// - Standby × 3: `pyro_safety#standby`
    /// - Fork × 3: `drone_swarm#fork@7`
    ///
    /// The `#suffix` convention is parsed by `crate::lineage`
    /// to recover group membership. Names + messages avoid
    /// industry jargon so a non-technical reader can recognize
    /// what each daemon does at a glance.
    const DAEMON_ROSTER: &[(&str, &[(LogLevel, &str)])] = &[
        ("stage_cues", STAGE_CUES_LOGS),
        ("vision_grasp_ai", VISION_GRASP_LOGS),
        ("audio_mixer#replica", AUDIO_MIXER_LOGS),
        ("audio_mixer#replica", AUDIO_MIXER_LOGS),
        ("audio_mixer#replica", AUDIO_MIXER_LOGS),
        ("pyro_safety#standby", PYRO_SAFETY_LOGS),
        ("pyro_safety#standby", PYRO_SAFETY_LOGS),
        ("pyro_safety#standby", PYRO_SAFETY_LOGS),
        ("drone_swarm#fork@7", DRONE_SWARM_LOGS),
        ("drone_swarm#fork@7", DRONE_SWARM_LOGS),
        ("drone_swarm#fork@7", DRONE_SWARM_LOGS),
    ];

    /// `stage_cues` — concert cue firing, lighting transitions,
    /// music-sync, pyrotech handshakes.
    const STAGE_CUES_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "cue 47 ready: scene/clear-back"),
        (LogLevel::Info, "lighting universe 1 rendered (512 channels)"),
        (LogLevel::Info, "follow-spot 2 assigned to operator 'rio'"),
        (LogLevel::Info, "scene transition: act 2, song 3"),
        (LogLevel::Info, "music-sync locked at 128.0 bpm"),
        (LogLevel::Info, "pyrotechnics safety gate cleared: zone 3 armed"),
        (LogLevel::Warn, "stage light 12 brownout — falling back"),
    ];

    /// `vision_grasp_ai` — robot-grasp inference using the
    /// vision-grasp model, depth fusion + AI cache reads.
    const VISION_GRASP_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "grasp candidates: 4 (top confidence 89%)"),
        (LogLevel::Info, "AI cache hit (98%) on batch 248"),
        (LogLevel::Info, "vision-grasp model v2.4 loaded (context 8192)"),
        (LogLevel::Info, "tracking 16 object keypoints"),
        (LogLevel::Info, "robot trajectory: 4 waypoints planned"),
        (LogLevel::Warn, "scene occluded — falling back to radar"),
    ];

    /// `audio_mixer#replica` — concert audio mixers processing
    /// the live bus + monitor sends.
    const AUDIO_MIXER_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "audio bus 3 limiter active (-2.4dB)"),
        (LogLevel::Info, "monitor send 7 routed to in-ear 3"),
        (LogLevel::Info, "channel 12 muted: feedback detected"),
        (LogLevel::Info, "vocal reverb applied (1.8s tail)"),
        (LogLevel::Info, "compressor: 4:1 ratio, 2ms attack"),
        (LogLevel::Warn, "input gain hot on channel 4 (-0.3 dB headroom)"),
    ];

    /// `pyro_safety#standby` — pyrotechnic safety system. The
    /// active one fires interlocks; warm standbys publish
    /// heartbeats + readiness checks.
    const PYRO_SAFETY_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "confetti canon 4 armed"),
        (LogLevel::Info, "interlock check: stage rail OK"),
        (LogLevel::Info, "abort signal cleared — gate green"),
        (LogLevel::Info, "weather check: wind 8mph, OK to fire"),
        (LogLevel::Info, "operator override authenticated"),
        (LogLevel::Warn, "humidity above safe threshold (78%) — derating"),
    ];

    /// `drone_swarm#fork@7` — drone-swarm coordinators, each
    /// driving a subset of the formation.
    const DRONE_SWARM_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "formation: V-shape, 7 drones, spacing 4.2m"),
        (LogLevel::Info, "waypoint 12 reached, holding position"),
        (LogLevel::Info, "wind compensation: 1.2m east drift"),
        (LogLevel::Info, "tracking target: chase truck"),
        (LogLevel::Info, "battery levels: median 78%, lowest 64%"),
        (LogLevel::Warn, "no-fly zone breach prevented"),
    ];

    /// Install probes + register the 11-daemon roster + seed
    /// replicas + spawn the per-daemon log seeder.
    /// Awaits the seeding completes before returning so the
    /// App starts against a fully populated steady-state
    /// snapshot rather than a partial one.
    pub async fn install(
        sdk: &MeshOsDaemonSdk,
    ) -> color_eyre::Result<(Vec<MeshOsDaemonHandle>, tokio::task::JoinHandle<()>)> {
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

        // Register each daemon from the roster and collect
        // (daemon_id, vocab) pairs for the seeder. The
        // `#suffix` portion of each name is parsed by
        // `crate::lineage` to recover group membership.
        let mut handles: Vec<MeshOsDaemonHandle> = Vec::with_capacity(DAEMON_ROSTER.len());
        let mut seeder_roster: Vec<(u64, &'static [(LogLevel, &'static str)])> =
            Vec::with_capacity(DAEMON_ROSTER.len());
        for (name, vocab) in DAEMON_ROSTER {
            let h = sdk.register_daemon(
                Box::new(SampleDaemon::new(*name)),
                EntityKeypair::generate(),
            )?;
            seeder_roster.push((h.daemon_id(), *vocab));
            handles.push(h);
        }

        // Spawn the seeder task on the runtime's main handle.
        // Daemon-tagged log lines flow through the same path
        // a real daemon's `publish_log` would take; the
        // substrate's snapshot fold stamps `node_id` on the
        // way through.
        let seeder_handle = sdk.runtime().handle_clone();
        let seeder_task = tokio::spawn(async move {
            run_daemon_log_seeder(seeder_handle, seeder_roster).await;
        });

        Ok((handles, seeder_task))
    }

    /// Round-robin daemon-log seeder. Cycles through the
    /// roster every tick, picking the next message in each
    /// daemon's vocabulary so every daemon publishes roughly
    /// in lockstep. Cadence is intentionally faster than the
    /// `samples-logs` node-level seeder so daemon chatter
    /// dominates the LOGS tail.
    async fn run_daemon_log_seeder(
        handle: net_sdk::meshos::MeshOsHandle,
        roster: Vec<(u64, &'static [(LogLevel, &'static str)])>,
    ) {
        if roster.is_empty() {
            return;
        }
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(450));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut i = 0usize;
        loop {
            ticker.tick().await;
            let (daemon_id, vocab) = roster[i % roster.len()];
            if vocab.is_empty() {
                i = i.wrapping_add(1);
                continue;
            }
            let (level, msg) = vocab[(i / roster.len()) % vocab.len()];
            let line = LogLine {
                level,
                daemon_id: Some(daemon_id),
                message: msg.to_string(),
            };
            if handle
                .publish(MeshOsEvent::LogLine(line))
                .await
                .is_err()
            {
                // Loop closed — substrate shutting down. Exit
                // cleanly; harness's `_sdk` drop already raced
                // ahead.
                break;
            }
            i = i.wrapping_add(1);
        }
    }
}

/// Node-level mesh-event seeder. Publishes `MeshOsEvent::LogLine`
/// records with `daemon_id = None` so the substrate stamps the
/// local node as the source — these surface in the NET.MAP
/// MESH.EVENTS panel as `node.0x<this_node>` chatter, separate
/// from the per-daemon vocabulary that ships under the
/// `samples` flag.
///
/// Independent of `samples` — operators running `samples-logs`
/// alone still get a populated MESH.EVENTS feed; stacking with
/// `samples` adds daemon-tagged chatter on top.
///
/// Exits when the runtime closes — `publish` returns
/// `Err(LoopClosed)` once the SDK drops and the loop breaks.
#[cfg(feature = "samples-logs")]
mod samples_logs {
    use std::time::Duration;

    use net_sdk::meshos::{LogLevel, LogLine, MeshOsEvent, MeshOsHandle};

    /// Spawn the seeder and return its `JoinHandle` for the
    /// harness to hold.
    pub fn install(handle: MeshOsHandle) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { run(handle).await })
    }

    /// Per-tick cadence. Slower than the per-daemon seeder so
    /// mesh-level chatter doesn't drown out daemon vocabulary
    /// when both flags are enabled.
    const TICK: Duration = Duration::from_millis(1_300);

    /// Node-level (no `daemon_id`) log fixtures. The
    /// substrate stamps `node_id = this_node` on these so
    /// they render as `node.0x<this_node>` in the
    /// MESH.EVENTS section. Phrasing favours plain English so
    /// a non-technical reader can follow what the cluster is
    /// doing without knowing the substrate vocabulary.
    const NODE_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "main-stage joined the cluster"),
        (LogLevel::Info, "replica chain 0xc005 placement assigned"),
        (LogLevel::Warn, "follower-1 link degraded — 244ms latency"),
        (LogLevel::Info, "operator override cleared safety freeze"),
        (LogLevel::Warn, "peer avoid list growing (52 entries)"),
        (LogLevel::Info, "storage GC: 142 unused chunks freed (3.2 GB)"),
        (LogLevel::Error, "resource probe crashed — sample skipped"),
        (LogLevel::Info, "cluster snapshot: 17 nodes, 11 daemons, 8 chains"),
        (LogLevel::Warn, "action queue 91% full (58 / 64)"),
        (LogLevel::Info, "admin action accepted: drain follower-1"),
        (LogLevel::Info, "signed operator action verified"),
        (LogLevel::Warn, "pit-lane link degraded — 451ms latency"),
        (LogLevel::Info, "cluster freeze lifted; no backlog"),
        (LogLevel::Error, "ai-gpu-2 storage unhealthy — disk at 95%"),
    ];

    /// Inner loop. Walks the node-level fixture; daemon-level
    /// chatter is now owned by the per-daemon seeder under
    /// `samples`, so this module's sole job is the mesh-wide
    /// event feed.
    async fn run(handle: MeshOsHandle) {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut i = 0usize;
        loop {
            ticker.tick().await;
            let (level, msg) = NODE_LOGS[i % NODE_LOGS.len()];
            let line = LogLine {
                level,
                daemon_id: None,
                message: msg.to_string(),
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

/// Synthetic nRPC traffic injector. Pushes call records into
/// the deck's `NrpcTail` on a fixed cadence so the NRPC tab
/// demonstrates the call ring under `samples-logs`. The real
/// nRPC observer wire-up (when the substrate exposes it) will
/// replace this seeder without touching the tail consumer.
#[cfg(feature = "samples-logs")]
pub fn spawn_nrpc_seeder(
    tail: crate::streams::NrpcTail,
    this_node: net_sdk::meshos::NodeId,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move { nrpc_seeder::run(tail, this_node).await })
}

#[cfg(feature = "samples-logs")]
mod nrpc_seeder {
    use std::time::Duration;

    use crate::streams::{NrpcCall, NrpcStatus, NrpcTail};

    /// Cadence between injected calls. ~150ms = ~6/s, dense
    /// enough to fill the call ring quickly without overwhelming
    /// the render path.
    const TICK: Duration = Duration::from_millis(150);

    /// Method vocabulary, paired with a typical request /
    /// response byte-count and a base latency. The seeder picks
    /// one per tick and rotates through the call cast so every
    /// row reads as a plausible cluster RPC.
    const METHODS: &[(&str, u32, u32, u32)] = &[
        // (method, req_bytes, resp_bytes, base_latency_ms)
        ("ai.inference.grasp_request", 12_288, 2_048, 18),
        ("ai.inference.chat_complete", 2_048, 65_536, 240),
        ("ai.cache.kv_lookup", 256, 4_096, 2),
        ("audio.mixer.set_channel_level", 32, 16, 1),
        ("audio.mixer.subscribe_meters", 64, 128, 1),
        ("drone.swarm.assign_waypoint", 96, 32, 4),
        ("drone.flight.publish_telemetry", 128, 16, 1),
        ("robot.arm.move_to_pose", 192, 32, 6),
        ("robot.gripper.set_force", 24, 16, 1),
        ("sensor.lidar.subscribe_pointcloud", 64, 524_288, 3),
        ("sensor.camera.request_frame", 32, 1_048_576, 8),
        ("stage.cues.fire_cue", 96, 32, 2),
        ("stage.lights.set_universe", 1_024, 16, 1),
        ("vehicle.fusion.publish_track", 512, 16, 2),
        ("storage.blob.fetch", 64, 524_288, 12),
        ("storage.blob.store", 65_536, 32, 14),
    ];

    /// Caller / callee pairs. Pulled from the `nodes::NODES`
    /// fixture so cross-tab id pivots still resolve to the
    /// labeled rows. `this_node` is also a valid endpoint —
    /// some calls originate locally.
    const CALL_PAIRS: &[(u64, u64)] = &[
        // AI rack consumers
        (0xfc2, 0xbdda),   // camera-system → ai-gpu-1 (vision grasp)
        (0xe068, 0xbdda),  // robot-arm → ai-gpu-1
        (0xe9b8, 0x6dfb),  // side-stage → ai-gpu-2 (chat assistant)
        (0xbdda, 0x3c81),  // ai-gpu-1 → ai-cache
        (0x6dfb, 0x3c81),  // ai-gpu-2 → ai-cache
        // Audio / stage
        (0xe685, 0xa96f),  // concert-audio → main-stage (cue sync)
        (0xd4ff, 0xe685),  // monitor-mix → concert-audio
        (0xa96f, 0x3599),  // main-stage → stage-lighting (cue→lights)
        (0xa96f, 0x3599),  // main-stage → stage-lighting (pyro)
        // Drone swarm
        (0x372b, 0xeba8),  // ground-station → scout-3
        (0x372b, 0x82ee),  // ground-station → follower-1
        (0xeba8, 0xbdda),  // scout-3 → ai-gpu-1 (track ID)
        // Vehicle
        (0xf206, 0x6808),  // chase-truck → pit-lane
        (0xf206, 0xbdda),  // chase-truck → ai-gpu-1 (perception)
        // Robotics
        (0xe068, 0xbf44),  // robot-arm → assembly-line
        (0xfc2, 0xe068),   // camera-system → robot-arm
    ];

    fn unix_now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    pub async fn run(tail: NrpcTail, _this_node: net_sdk::meshos::NodeId) {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut i = 0usize;
        loop {
            ticker.tick().await;
            let (method, req_bytes, resp_bytes, base_latency) =
                METHODS[i % METHODS.len()];
            // Decouple the call-pair index from the method
            // index so (caller, callee, method) cycles over
            // METHODS.len() × CALL_PAIRS.len() distinct triples
            // instead of locking in lockstep at the GCD. Without
            // this the same tuple recurred every 16 ticks; with
            // it the cycle is 256.
            let (caller, callee) = CALL_PAIRS[(i / METHODS.len()) % CALL_PAIRS.len()];
            // Deterministic-but-varied jitter so latencies don't
            // read as identical across rows. Pseudo-random via
            // index arithmetic (no rand crate dependency).
            let jitter = ((i as u32).wrapping_mul(2_654_435_761) % 16) as i32 - 4;
            let latency_ms = ((base_latency as i32).saturating_add(jitter)).max(1) as u32;
            // Status mix: ~85% Ok, ~7% InFlight, ~4% Error,
            // ~4% Timeout. Picked by `i % 100` so the mix is
            // exact over each 100-call window.
            let bucket = i % 100;
            let status = if bucket < 85 {
                NrpcStatus::Ok
            } else if bucket < 92 {
                NrpcStatus::InFlight
            } else if bucket < 96 {
                NrpcStatus::Error(error_reason_for(method).to_string())
            } else {
                NrpcStatus::Timeout
            };
            tail.push(NrpcCall {
                ts_ms: unix_now_ms(),
                caller,
                callee,
                method: method.to_string(),
                latency_ms,
                status,
                request_bytes: req_bytes,
                response_bytes: resp_bytes,
            });
            i = i.wrapping_add(1);
        }
    }

    /// Method-flavoured error reasons. Reads as a real failure
    /// rather than a generic "Error".
    fn error_reason_for(method: &str) -> &'static str {
        if method.starts_with("ai.") {
            "model overloaded"
        } else if method.starts_with("audio.") {
            "channel muted"
        } else if method.starts_with("drone.") {
            "geofence violation"
        } else if method.starts_with("robot.") {
            "kinematic singularity"
        } else if method.starts_with("sensor.") {
            "sensor offline"
        } else if method.starts_with("stage.") {
            "interlock open"
        } else if method.starts_with("storage.") {
            "checksum mismatch"
        } else {
            "remote rejected"
        }
    }
}
