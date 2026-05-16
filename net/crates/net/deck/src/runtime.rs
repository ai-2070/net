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
                    match *id {
                        // ── Live event production ──────────────
                        0xa96f => {
                            // main-stage: DMX universes + audio
                            // monitor sends + MIDI clock.
                            caps.insert("zone:stage".to_string());
                            caps.insert("stage.dmx.fixture".to_string());
                            caps.insert("stage.lighting.intelligent".to_string());
                            caps.insert("stage.cue.trigger".to_string());
                            caps.insert("stage.midi.bridge".to_string());
                        }
                        0xe9b8 => {
                            // side-stage: lighter rig — DMX
                            // fixture set + cue triggers only.
                            caps.insert("zone:stage".to_string());
                            caps.insert("stage.dmx.fixture".to_string());
                            caps.insert("stage.cue.trigger".to_string());
                        }
                        0xe685 => {
                            // foh-mix: front-of-house audio.
                            caps.insert("zone:foh".to_string());
                            caps.insert("stage.audio.mix.foh".to_string());
                            caps.insert("sensor.audio.array".to_string());
                        }
                        0xd4ff => {
                            // monitor-booth: monitor mixes.
                            caps.insert("zone:foh".to_string());
                            caps.insert("stage.audio.mix.monitor".to_string());
                        }
                        0x3599 => {
                            // dimmer-room: DMX + pyrotech gate.
                            caps.insert("zone:stage".to_string());
                            caps.insert("stage.dmx.fixture".to_string());
                            caps.insert("stage.pyro.gate".to_string());
                            caps.insert("stage.fog.ducted".to_string());
                        }
                        // ── Drone swarm ────────────────────────
                        0x372b => {
                            // ground-station: swarm coordinator.
                            caps.insert("zone:hangar".to_string());
                            caps.insert("drone.swarm.coord".to_string());
                            caps.insert("drone.fc.px4".to_string());
                        }
                        0xeba8 => {
                            // scout-3: quad with cinema payload.
                            caps.insert("zone:airspace".to_string());
                            caps.insert("drone.airframe.quad".to_string());
                            caps.insert("drone.payload.cinema".to_string());
                            caps.insert("sensor.camera.rgb".to_string());
                            caps.insert("sensor.gps.rtk".to_string());
                        }
                        0x82ee => {
                            // follower-1: hex with thermal +
                            // solid-state lidar.
                            caps.insert("zone:airspace".to_string());
                            caps.insert("drone.airframe.hex".to_string());
                            caps.insert("drone.payload.thermal".to_string());
                            caps.insert("sensor.lidar.solid_state".to_string());
                        }
                        // ── AI inference cluster ───────────────
                        0xbdda => {
                            // gpu-rack-a: Hopper-class GPUs
                            // running the openclaw vision-grasp
                            // model with FP8 batches.
                            caps.insert("zone:rack".to_string());
                            caps.insert("gpu.b300.h200".to_string());
                            caps.insert("gpu.tensor.fp8".to_string());
                            caps.insert("model.serving.batch".to_string());
                            caps.insert("model.harness.openclaw".to_string());
                        }
                        0x6dfb => {
                            // gpu-rack-b: streaming hermes chat
                            // agent on BF16.
                            caps.insert("zone:rack".to_string());
                            caps.insert("gpu.b300.h200".to_string());
                            caps.insert("gpu.tensor.bf16".to_string());
                            caps.insert("model.serving.stream".to_string());
                            caps.insert("model.harness.hermes".to_string());
                        }
                        0x3c81 => {
                            // model-cache: KV-cache host backed
                            // by blob storage.
                            caps.insert("zone:rack".to_string());
                            caps.insert("model.cache.kv".to_string());
                            caps.insert("dataforts.blob.storage".to_string());
                            caps.insert("greedy.cache".to_string());
                        }
                        // ── Robotics cell ──────────────────────
                        0xe068 => {
                            // arm-cell: 7-dof manipulator with
                            // parallel gripper and Jacobian-based
                            // motion planning.
                            caps.insert("zone:cell".to_string());
                            caps.insert("robot.arm.7dof".to_string());
                            caps.insert("robot.gripper.parallel".to_string());
                            caps.insert("robot.kinematics.jacobian".to_string());
                        }
                        0xbf44 => {
                            // gantry: 6-dof gantry-mounted arm,
                            // suction gripper.
                            caps.insert("zone:cell".to_string());
                            caps.insert("robot.arm.6dof".to_string());
                            caps.insert("robot.gripper.suction".to_string());
                        }
                        // ── Autonomous vehicle mesh ────────────
                        0xf206 => {
                            // chase-truck: CAN-FD + L4 ADAS,
                            // surround radar + spinning lidar.
                            caps.insert("zone:track".to_string());
                            caps.insert("vehicle.bus.canfd".to_string());
                            caps.insert("vehicle.adas.l4".to_string());
                            caps.insert("vehicle.fusion.surround".to_string());
                            caps.insert("sensor.lidar.spinning".to_string());
                            caps.insert("sensor.radar.fmcw".to_string());
                        }
                        0x6808 => {
                            // pit-lane: EtherCAT bus + IMU.
                            caps.insert("zone:pit".to_string());
                            caps.insert("vehicle.bus.ethercat".to_string());
                            caps.insert("sensor.imu.9dof".to_string());
                        }
                        // ── Edge ───────────────────────────────
                        0xf83d => {
                            // edge-drone: on-board ardupilot
                            // with a depth camera.
                            caps.insert("zone:airspace".to_string());
                            caps.insert("drone.airframe.quad".to_string());
                            caps.insert("drone.fc.ardupilot".to_string());
                            caps.insert("sensor.camera.depth".to_string());
                        }
                        0x0fc2 => {
                            // vision-rig: depth + RGB stack;
                            // hosts an openclaw harness for
                            // on-rig grasp inference.
                            caps.insert("zone:cell".to_string());
                            caps.insert("sensor.camera.rgb".to_string());
                            caps.insert("sensor.camera.depth".to_string());
                            caps.insert("model.harness.openclaw".to_string());
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
    /// - Solo: `mainstage_cue`, `openclaw_inference`
    /// - Replica × 3: `foh_mixer#replica`
    /// - Standby × 3: `pyro_gate#standby`
    /// - Fork × 3: `swarm_coord#fork@7`
    ///
    /// The `#suffix` convention is parsed by `crate::lineage`
    /// to recover group membership.
    const DAEMON_ROSTER: &[(&str, &[(LogLevel, &str)])] = &[
        ("mainstage_cue", MAINSTAGE_LOGS),
        ("openclaw_inference", OPENCLAW_LOGS),
        ("foh_mixer#replica", FOH_LOGS),
        ("foh_mixer#replica", FOH_LOGS),
        ("foh_mixer#replica", FOH_LOGS),
        ("pyro_gate#standby", PYRO_LOGS),
        ("pyro_gate#standby", PYRO_LOGS),
        ("pyro_gate#standby", PYRO_LOGS),
        ("swarm_coord#fork@7", SWARM_LOGS),
        ("swarm_coord#fork@7", SWARM_LOGS),
        ("swarm_coord#fork@7", SWARM_LOGS),
    ];

    /// `mainstage_cue` — DMX cue firing, lighting transitions,
    /// MIDI clock, pyrotech-gate handshakes.
    const MAINSTAGE_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "cue 47 ready: scene/clear-back"),
        (LogLevel::Info, "dmx universe 1 rendered, 512 channels"),
        (LogLevel::Info, "follow-spot 2 → operator 'rio'"),
        (LogLevel::Info, "scene transition: act2/song-3"),
        (LogLevel::Info, "midi clock locked at 128.0 bpm"),
        (LogLevel::Info, "pyro gate cleared: zone-3 armed"),
        (LogLevel::Warn, "fixture 12 brownout reported; falling back"),
    ];

    /// `openclaw_inference` — vision-grasp model serving on
    /// the gpu rack, depth fusion + kv-cache reads.
    const OPENCLAW_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "grasp candidates: 4 (top score 0.89)"),
        (LogLevel::Info, "kv-cache hit (98%) on batch 248"),
        (LogLevel::Info, "model openclaw-2.4, ctx 8192"),
        (LogLevel::Info, "depth fusion: 16 keypoints tracked"),
        (LogLevel::Info, "joint trajectory smoothed (4 waypoints)"),
        (LogLevel::Warn, "occluded scene detected; falling back to radar"),
    ];

    /// `foh_mixer#replica` — front-of-house audio mixers
    /// processing the live bus + monitor sends.
    const FOH_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "bus 3: -2.4 dB clip avoid"),
        (LogLevel::Info, "monitor send 7 → in-ear 3"),
        (LogLevel::Info, "channel 12 muted: feedback detected"),
        (LogLevel::Info, "reverb tail 1.8s applied to vocals"),
        (LogLevel::Info, "compressor 4:1 ratio, attack 2ms"),
        (LogLevel::Warn, "input gain hot on channel 4 (-0.3 dB headroom)"),
    ];

    /// `pyro_gate#standby` — pyrotechnic safety gate. The
    /// active one fires interlocks; warm standbys publish
    /// heartbeats + readiness checks.
    const PYRO_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "fixture 4 armed: confetti canon"),
        (LogLevel::Info, "interlock: stage rail OK"),
        (LogLevel::Info, "abort signal cleared, gate green"),
        (LogLevel::Info, "weather check: wind 8mph, OK"),
        (LogLevel::Info, "operator override authenticated (op:0x7f3a)"),
        (LogLevel::Warn, "humidity above gate threshold (78%) — derating"),
    ];

    /// `swarm_coord#fork@7` — drone-swarm coordinator forks,
    /// each driving a subset of the formation.
    const SWARM_LOGS: &[(LogLevel, &str)] = &[
        (LogLevel::Info, "formation: V-shape, 7 drones, spacing 4.2m"),
        (LogLevel::Info, "waypoint 12 reached, holding"),
        (LogLevel::Info, "wind compensation: +1.2m east drift"),
        (LogLevel::Info, "follow target locked: vehicle-04"),
        (LogLevel::Info, "battery telemetry: median 78%, min 64%"),
        (LogLevel::Warn, "geofence breach prevented (lat 51.522)"),
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
