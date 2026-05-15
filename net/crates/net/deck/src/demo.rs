//! In-process demo cluster. Gated behind `feature = "demo"`.
//!
//! Spawns a [`MeshOsDaemonSdk`] (which owns a real
//! [`MeshOsRuntime`]), registers a handful of example daemons,
//! and starts a tokio task that periodically publishes log
//! lines + signed admin events so the snapshot has something
//! interesting for every tab to render. Returns a
//! [`DeckClient`] bound to the same runtime — Deck reads the
//! snapshot through it just like it would against a real
//! cluster.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::compute::CausalEvent;
use net_sdk::deck::{DeckClient, OperatorIdentity};
use net_sdk::meshos::{
    DaemonError, EntityKeypair, HealthProbe, LocalityProbe, LogLevel, MeshDaemon, MeshOsConfig,
    MeshOsDaemonHandle, MeshOsDaemonSdk, NodeHealth, NodeId,
};

/// Handle to the running demo cluster. Hold for the app's
/// lifetime; dropping it aborts the seeder task + tears the
/// runtime down.
pub struct DemoHarness {
    /// Keeps the runtime + registered daemons alive.
    _sdk: MeshOsDaemonSdk,
    /// Aborted on drop. Owns the registered daemon handles so
    /// the seeder can `publish_log` on each cycle without
    /// fighting with the harness over ownership.
    seeder: tokio::task::JoinHandle<()>,
    deck: Arc<DeckClient>,
}

impl DemoHarness {
    pub fn deck(&self) -> Arc<DeckClient> {
        Arc::clone(&self.deck)
    }
}

impl Drop for DemoHarness {
    fn drop(&mut self) {
        self.seeder.abort();
    }
}

/// Stub daemon — `process` is a no-op; everything else uses
/// trait defaults (health = Healthy, saturation = 0.0). Just
/// exists so it shows up in `snapshot.daemons` and the log
/// lines have a real daemon id to ride on.
struct DemoDaemon {
    name: String,
}

impl DemoDaemon {
    fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl MeshDaemon for DemoDaemon {
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

/// Bring up the demo: one runtime, four daemons, a seeder
/// task that publishes log lines + signed admin events in a
/// loop. Returns a `DemoHarness` whose `deck()` getter hands
/// the `DeckClient` to the rest of the app.
pub async fn spawn() -> color_eyre::Result<DemoHarness> {
    // Faster tick than the production default so the snapshot
    // updates feel live during demo. `MeshOsConfig` is
    // `#[non_exhaustive]` so we can't use a struct literal —
    // mutate a default instead. `this_node` is set to a
    // synthetic local id (`0x0001`) that doesn't appear in
    // the deck's `nodes` fixture, so every fixture peer shows
    // up as a remote in the snapshot.
    let mut cfg = MeshOsConfig::default();
    cfg.this_node = 0x0001;
    cfg.tick_interval = Duration::from_millis(250);
    let dispatcher = Arc::new(net_sdk::meshos::LoggingDispatcher::new());
    let sdk = MeshOsDaemonSdk::start(cfg, dispatcher);

    // Install synthetic probes so `snapshot.peers` populates
    // with the 17 fixture peers (matching `nodes::NODES`).
    // RTTs and health are deterministic per peer so the UI
    // doesn't flicker each tick.
    sdk.runtime()
        .add_locality_probe(Arc::new(DemoLocalityProbe));
    sdk.runtime()
        .add_health_probe(Arc::new(DemoHealthProbe));

    // Register a mix of group kinds so the deck's lineage
    // inference exercises every flavor. Phase A uses a
    // name-suffix convention to encode lineage — see
    // `src/lineage.rs` for the parser:
    //
    //   <kind>             standalone
    //   <kind>#replica     ReplicaGroup member (one entry per index)
    //   <kind>#standby     StandbyGroup member (lowest id = active)
    //   <kind>#fork@<seq>  ForkGroup fork (one entry per index)
    let daemons = vec![
        // standalone
        sdk.register_daemon(
            Box::new(DemoDaemon::new("mikoshi")),
            EntityKeypair::generate(),
        )?,
        sdk.register_daemon(
            Box::new(DemoDaemon::new("telemetry")),
            EntityKeypair::generate(),
        )?,
        // replica group "gravity" × 3
        sdk.register_daemon(
            Box::new(DemoDaemon::new("gravity#replica")),
            EntityKeypair::generate(),
        )?,
        sdk.register_daemon(
            Box::new(DemoDaemon::new("gravity#replica")),
            EntityKeypair::generate(),
        )?,
        sdk.register_daemon(
            Box::new(DemoDaemon::new("gravity#replica")),
            EntityKeypair::generate(),
        )?,
        // standby group "anti_entr" × 3 (1 active + 2 warm)
        sdk.register_daemon(
            Box::new(DemoDaemon::new("anti_entr#standby")),
            EntityKeypair::generate(),
        )?,
        sdk.register_daemon(
            Box::new(DemoDaemon::new("anti_entr#standby")),
            EntityKeypair::generate(),
        )?,
        sdk.register_daemon(
            Box::new(DemoDaemon::new("anti_entr#standby")),
            EntityKeypair::generate(),
        )?,
        // fork group "drift_corr" × 3 forked from parent at seq=42
        sdk.register_daemon(
            Box::new(DemoDaemon::new("drift_corr#fork@42")),
            EntityKeypair::generate(),
        )?,
        sdk.register_daemon(
            Box::new(DemoDaemon::new("drift_corr#fork@42")),
            EntityKeypair::generate(),
        )?,
        sdk.register_daemon(
            Box::new(DemoDaemon::new("drift_corr#fork@42")),
            EntityKeypair::generate(),
        )?,
    ];

    // Build a DeckClient against the same runtime. Operator
    // identity is generated fresh — real deployments load it
    // from the maintenance node's identity store.
    let identity = OperatorIdentity::from_keypair(EntityKeypair::generate());
    let deck = Arc::new(DeckClient::from_runtime(sdk.runtime(), identity));

    let seeder = tokio::spawn(seed_events(daemons, Arc::clone(&deck)));

    Ok(DemoHarness {
        _sdk: sdk,
        seeder,
        deck,
    })
}

// ───────────────────────── synthetic probes ─────────────────────────

/// 17 fake peers + per-peer RTT µs + per-peer health. The ids
/// match the deck's `nodes::NODES` fixture so `id.label`
/// renders correctly. Most peers are Healthy; a couple are
/// Degraded so the LIST + NET.MAP views show all three states.
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

struct DemoLocalityProbe;
impl LocalityProbe for DemoLocalityProbe {
    fn rtt_samples(&self) -> Vec<(NodeId, Duration)> {
        PEERS
            .iter()
            .map(|(id, us, _)| (*id, Duration::from_micros(*us)))
            .collect()
    }
}

struct DemoHealthProbe;
impl HealthProbe for DemoHealthProbe {
    fn health_samples(&self) -> Vec<(NodeId, NodeHealth)> {
        PEERS.iter().map(|(id, _, h)| (*id, *h)).collect()
    }
}

async fn seed_events(daemons: Vec<MeshOsDaemonHandle>, deck: Arc<DeckClient>) {
    let messages: &[(usize, LogLevel, &str)] = &[
        (0, LogLevel::Info,  "tick t=482·31  pending=0  drift=0.0"),
        (1, LogLevel::Info,  "gravity_pull 0x285e → 0x6dfb hot=0.71"),
        (2, LogLevel::Info,  "anti-entropy cycle ok · 0 reflows"),
        (3, LogLevel::Info,  "snapshot taken seq=4912 size=12.4KB"),
        (0, LogLevel::Info,  "process_event seq=4913 latency=38ns"),
        (3, LogLevel::Warn,  "channel buffer 76% · throttling"),
        (2, LogLevel::Info,  "drift_correct nudge: −2.1ms vs anchor"),
        (1, LogLevel::Info,  "cool 0x4b04 rate=0.10 evictable"),
        (3, LogLevel::Error, "retry budget exhausted · backoff 5s"),
        (0, LogLevel::Info,  "migrated to 0xbf44 ← 0x6dfb (cutover 280ns)"),
        (1, LogLevel::Info,  "rebalance chain.0xc1 holders 2→3"),
        (2, LogLevel::Warn,  "anchor late by 2.1ms · nudging"),
    ];
    let mut interval = tokio::time::interval(Duration::from_millis(600));
    let mut step: u64 = 0;
    loop {
        interval.tick().await;

        // One log line per cycle, rotating through the canned
        // set so each daemon contributes lines at different
        // levels. publish_log is sync + non-blocking; if the
        // runtime's event queue is full, the line drops.
        let (slot, level, msg) = messages[(step as usize) % messages.len()];
        if let Some(d) = daemons.get(slot) {
            let _ = d.publish_log(level, msg.to_string());
        }

        // Every fourth tick fire a signed admin event so the
        // audit ring populates too. Cordon and uncordon
        // alternate so the cluster doesn't end up cordoning
        // every node it owns.
        if step % 4 == 0 {
            let node_id: u64 = 1 + ((step / 4) % 8);
            let _ = if step % 8 == 0 {
                deck.admin().cordon(node_id).await
            } else {
                deck.admin().uncordon(node_id).await
            };
        }

        step = step.wrapping_add(1);
    }
}
