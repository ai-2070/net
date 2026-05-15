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

use net_sdk::deck::{AdminVerifier, DeckClient, OperatorIdentity, OperatorRegistry};
use net_sdk::meshos::{EntityKeypair, MeshOsConfig, MeshOsDaemonSdk};

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
}

impl Harness {
    pub fn deck(&self) -> Arc<DeckClient> {
        Arc::clone(&self.deck)
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

    let sdk = MeshOsDaemonSdk::start_with_verifier(cfg, dispatcher, verifier);

    let identity = OperatorIdentity::from_keypair(operator_keypair);
    let deck = Arc::new(DeckClient::from_runtime(sdk.runtime(), identity));

    #[cfg(feature = "samples")]
    let _daemons = samples::install(&sdk)?;

    Ok(Harness {
        _sdk: sdk,
        #[cfg(feature = "samples")]
        _daemons,
        deck,
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
    use net_sdk::meshos::{
        ChainId, DaemonError, EntityKeypair, HealthProbe, LocalityProbe, MeshDaemon,
        MeshOsDaemonHandle, MeshOsDaemonSdk, MeshOsEvent, NodeHealth, NodeId,
        PlacementIntent, ReplicaUpdate,
    };

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
            PEERS
                .iter()
                .map(|(id, us, _)| (*id, Duration::from_micros(*us)))
                .collect()
        }
    }

    struct SampleHealthProbe;
    impl HealthProbe for SampleHealthProbe {
        fn health_samples(&self) -> Vec<(NodeId, NodeHealth)> {
            PEERS.iter().map(|(id, _, h)| (*id, *h)).collect()
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
