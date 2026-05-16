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

use net_sdk::dataforts::{BlobMetrics, MeshBlobAdapter};
use net_sdk::deck::{AdminVerifier, DeckClient, OperatorIdentity, OperatorRegistry};
use net_sdk::meshos::{EntityKeypair, MeshOsConfig, MeshOsDaemonSdk, MigrationSnapshotSource};

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
    deck: Arc<DeckClient>,
    /// Real `MeshBlobAdapter` instance — `Some` in samples
    /// mode (constructed against an in-memory `Redex` + seeded
    /// with synthetic stores so DATAFORTS + BLOBS render real
    /// data); `None` in default mode (operator wires their own
    /// adapter or leaves the tabs in their empty state).
    blob_adapter: Option<Arc<MeshBlobAdapter>>,
}

impl Harness {
    pub fn deck(&self) -> Arc<DeckClient> {
        Arc::clone(&self.deck)
    }

    /// Adapter-side metrics handle for the DATAFORTS tab.
    /// Sourced from the same `MeshBlobAdapter` BLOBS reads
    /// from, so the two surfaces stay coherent.
    pub fn blob_metrics(&self) -> Option<Arc<BlobMetrics>> {
        self.blob_adapter
            .as_ref()
            .map(|a| Arc::new(a.metrics().clone()))
    }

    /// Adapter handle for the BLOBS tab + future blob-browse
    /// surfaces. `None` when no adapter is wired.
    pub fn blob_adapter(&self) -> Option<Arc<MeshBlobAdapter>> {
        self.blob_adapter.clone()
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

    let sdk = MeshOsDaemonSdk::start_with_options(
        cfg,
        dispatcher,
        Some(verifier),
        migration_source,
    );

    let identity = OperatorIdentity::from_keypair(operator_keypair);
    let deck = Arc::new(DeckClient::from_runtime(sdk.runtime(), identity));

    #[cfg(feature = "samples")]
    let _daemons = samples::install(&sdk)?;

    // Real `MeshBlobAdapter` in samples mode — constructs
    // against an in-memory `Redex`, advertises a 1 TiB cap so
    // the disk gauge reads under the health-gate threshold,
    // and immediately stores a handful of synthetic chunks so
    // BLOBS has real entries to list + DATAFORTS shows real
    // store activity. Default mode leaves the field `None`;
    // operators wire their own adapter when they have one.
    #[cfg(feature = "samples")]
    let blob_adapter = Some(samples::install_blob_adapter().await);
    #[cfg(not(feature = "samples"))]
    let blob_adapter: Option<Arc<MeshBlobAdapter>> = None;

    Ok(Harness {
        _sdk: sdk,
        #[cfg(feature = "samples")]
        _daemons,
        deck,
        blob_adapter,
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

    /// Construct a real `MeshBlobAdapter` against an in-memory
    /// `Redex`, advertise a 1 TiB cap, then seed it with a
    /// handful of synthetic store + fetch operations so the
    /// DATAFORTS metrics + BLOBS inventory both render real
    /// data in samples mode. No background ticking — the
    /// stores fire once at startup and the adapter's state
    /// stays steady from there. The "samples are a fixture,
    /// not an event seeder" rule still holds.
    pub async fn install_blob_adapter() -> Arc<MeshBlobAdapter> {
        let redex = Arc::new(Redex::new());
        // 1 TiB cap — well above the seeded stored bytes so the
        // disk gauge reads green on the DATAFORTS tab. Matches
        // the prior `install_blob_metrics` capacity.
        let adapter = MeshBlobAdapter::new("deck-samples", redex)
            .with_disk_capacity(1u64 << 40);
        let adapter = Arc::new(adapter);
        // A handful of synthetic blobs — the BLOBS tab needs
        // entries to render rows; bytes vary so each landing
        // has a distinct content hash. After the loop the
        // adapter's metrics reflect (n stores, sum bytes
        // stored); BLOBS lists n chunks newest-first.
        let payloads: &[&[u8]] = &[
            b"deck-samples/blob-one-tiny",
            b"deck-samples/blob-two-a-little-larger-payload",
            b"deck-samples/blob-three-mid-sized-content-for-the-fixture",
            b"deck-samples/blob-four/some-bytes/here",
            b"deck-samples/blob-five-final-fixture-entry",
        ];
        // Stored blob_refs kept so we can immediately re-fetch
        // a few — the fetch counter on DATAFORTS otherwise
        // reads 0 at startup and the metric looks broken.
        let mut stored = Vec::with_capacity(payloads.len());
        for payload in payloads {
            // `publish_blob_ref` is the canonical store entry
            // point — computes the content hash, builds the
            // `BlobRef::Small`, and stores via the adapter's
            // own `store()` path. Bumps `blobs_stored_total` +
            // `bytes_stored_total` + the refcount table; the
            // chunk lands in the in-memory Redex.
            if let Ok(blob) = publish_blob_ref(
                adapter.as_ref(),
                format!("mesh://deck-samples/{}", payload.len()),
                payload,
            )
            .await
            {
                stored.push(blob);
            }
        }
        // A few extra fetches so the fetch counter isn't zero
        // when DATAFORTS first renders.
        if let Some(blob) = stored.first() {
            for _ in 0..3 {
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
                },
                MigrationSnapshot {
                    daemon_origin: 0xdaee_0002,
                    phase: MigrationPhaseSnapshot::Transfer,
                    elapsed_ms: 1_240,
                },
                MigrationSnapshot {
                    daemon_origin: 0xdaee_0003,
                    phase: MigrationPhaseSnapshot::Replay,
                    elapsed_ms: 4_870,
                },
                MigrationSnapshot {
                    daemon_origin: 0xdaee_0004,
                    phase: MigrationPhaseSnapshot::Cutover,
                    elapsed_ms: 12_910,
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
        (0xa96f,  41, NodeHealth::Healthy),
        (0xe9b8,  39, NodeHealth::Healthy),
        (0xe685,  12, NodeHealth::Healthy),
        (0xd4ff,  44, NodeHealth::Healthy),
        (0x3599,  47, NodeHealth::Healthy),
        (0x372b,  88, NodeHealth::Healthy),
        (0xeba8, 244, NodeHealth::Degraded),
        (0x82ee,  92, NodeHealth::Healthy),
        (0xbdda,  85, NodeHealth::Healthy),
        (0x6dfb,  31, NodeHealth::Healthy),
        (0x3c81,  18, NodeHealth::Healthy),
        (0xe068, 162, NodeHealth::Healthy),
        (0xbf44,  29, NodeHealth::Healthy),
        (0xf206, 167, NodeHealth::Healthy),
        (0xf83d, 159, NodeHealth::Healthy),
        (0x6808, 451, NodeHealth::Degraded),
        (0x0fc2,  73, NodeHealth::Healthy),
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
                        ((24 + (i as u64 * 3) % 32) << 30) as u64 // 24..56 GB
                    };
                    let mem_total: u64 = 64 << 30;
                    let disk_used: u64 = ((256 + (i as u64 * 47) % 512) << 30) as u64;
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
                    // base capabilities; degraded peers also
                    // advertise the dataforts overflow tag so
                    // operators can see it in the inventory
                    // detail panel later.
                    let mut caps = std::collections::BTreeSet::new();
                    caps.insert("compute.daemon".to_string());
                    caps.insert("meshos.health".to_string());
                    if degraded {
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
    /// so the REPLICAS tab has data to render under samples
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
    /// through the runtime handle. No background task — once
    /// the seed events fold, the snapshot is steady-state.
    pub fn install(sdk: &MeshOsDaemonSdk) -> color_eyre::Result<Vec<MeshOsDaemonHandle>> {
        sdk.runtime().add_locality_probe(Arc::new(SampleLocalityProbe));
        sdk.runtime().add_health_probe(Arc::new(SampleHealthProbe));
        sdk.runtime().add_inventory_probe(Arc::new(SampleInventoryProbe));

        // Replica seeding: fire one ReplicaUpdate per (chain,
        // holder) plus one PlacementIntent per chain. Run in a
        // background task because publish is async; the task
        // completes after a handful of awaits and naturally
        // drops.
        let handle = sdk.runtime().handle_clone();
        tokio::spawn(async move {
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
        });

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
