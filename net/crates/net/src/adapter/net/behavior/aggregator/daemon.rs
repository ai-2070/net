//! [`AggregatorDaemon`] — long-running summarize loop spawned
//! against a live `MeshNode`.
//!
//! Built as a self-contained spawn-pattern type rather than as a
//! `MeshDaemon` trait impl because the existing `MeshDaemon`
//! trait is event-processor-shaped (`process(&CausalEvent) ->
//! Vec<Bytes>`) and lacks the `on_start` + `Tick(now)` lifecycle
//! hooks the aggregator design needs. Re-shaping `MeshDaemon` to
//! carry those hooks is its own substrate slice; this lives
//! alongside the trait until that lands.
//!
//! # Lifecycle
//!
//! - [`AggregatorDaemon::new`] — construct from
//!   [`AggregatorConfig`] + a live `MeshNode` handle. Validates
//!   every `fold_kind` resolves to a built-in or custom
//!   summarizer at construction time so configuration errors
//!   surface upfront.
//! - [`AggregatorDaemon::spawn`] — launch a background tokio
//!   task that loops at `config.summary_interval` until
//!   [`AggregatorDaemon::shutdown`] is called.
//! - [`AggregatorDaemon::latest_summaries`] — pull the most
//!   recent batch of summaries the loop produced. Operator
//!   tooling (`net aggregator inspect`, future Deck panel) reads
//!   through this.
//! - [`AggregatorDaemon::generation`] — monotonic tick counter,
//!   stamped onto every emitted `SummaryAnnouncement`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio::task::JoinHandle;

use super::summarizer::{
    resolve_summarizer, CapabilityFoldHandle, FoldHandle, SummarizerContext, SummaryAnnouncement,
    Summarizer,
};
use super::AggregatorConfig;
use crate::adapter::net::behavior::fold::capability::CapabilityFold;
use crate::adapter::net::behavior::fold::FoldKind;
use crate::adapter::net::MeshNode;

/// Configuration-validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregatorError {
    /// A `fold_kind` listed in [`AggregatorConfig::fold_kinds`]
    /// has no built-in summarizer and no custom override in
    /// [`AggregatorConfig::custom_summarizers`].
    UnregisteredFoldKind { kind: u16 },
    /// `fold_kinds` is empty — the daemon would do nothing.
    NoFoldKinds,
}

impl std::fmt::Display for AggregatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnregisteredFoldKind { kind } => write!(
                f,
                "fold_kind 0x{kind:04x} has no built-in summarizer and no \
                 custom override in AggregatorConfig::custom_summarizers"
            ),
            Self::NoFoldKinds => write!(f, "AggregatorConfig::fold_kinds is empty"),
        }
    }
}

impl std::error::Error for AggregatorError {}

/// Cap on the number of latest summaries retained per
/// [`AggregatorDaemon`]. Operator tooling pulls the latest batch
/// — older batches are dropped on the floor (no replay; the
/// substrate's replica-group publishers carry that
/// responsibility once the wire-publish slice lands).
const LATEST_SUMMARIES_CAP: usize = 32;

/// Long-running aggregator daemon. Construct, then `spawn()` to
/// launch its background task.
pub struct AggregatorDaemon {
    config: AggregatorConfig,
    mesh: Arc<MeshNode>,
    /// Pre-resolved summarizer per fold kind. Resolution runs at
    /// `new()` time so a missing-summarizer mis-configuration
    /// surfaces before the daemon spawns.
    summarizers: HashMap<u16, Arc<dyn Summarizer>>,
    /// Monotonic generation counter, bumped once per tick before
    /// summarization. Stamped onto every emitted
    /// [`SummaryAnnouncement`].
    generation: Arc<AtomicU64>,
    /// Latest batch of summaries the loop produced. Capped at
    /// `LATEST_SUMMARIES_CAP` entries — operator tooling reads
    /// through [`Self::latest_summaries`].
    latest: Arc<RwLock<Vec<SummaryAnnouncement>>>,
    /// Cooperative-shutdown flag. The background loop polls this
    /// between ticks; [`Self::shutdown`] sets it.
    shutdown: Arc<AtomicBool>,
}

impl AggregatorDaemon {
    /// Construct an aggregator bound to a live `MeshNode`. Fails
    /// at validation time when any `fold_kind` is unregistered.
    pub fn new(config: AggregatorConfig, mesh: Arc<MeshNode>) -> Result<Self, AggregatorError> {
        if config.fold_kinds.is_empty() {
            return Err(AggregatorError::NoFoldKinds);
        }
        let mut summarizers: HashMap<u16, Arc<dyn Summarizer>> = HashMap::new();
        for kind in &config.fold_kinds {
            let s = resolve_summarizer(*kind, &config.custom_summarizers)
                .ok_or(AggregatorError::UnregisteredFoldKind { kind: *kind })?;
            summarizers.insert(*kind, s);
        }
        Ok(Self {
            config,
            mesh,
            summarizers,
            generation: Arc::new(AtomicU64::new(0)),
            latest: Arc::new(RwLock::new(Vec::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Spawn the background summarize loop and return its
    /// `JoinHandle`. The handle resolves when the loop exits
    /// (typically after [`Self::shutdown`] is called).
    pub fn spawn(self: Arc<Self>) -> JoinHandle<()> {
        let interval = self.config.summary_interval;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // First `tick` returns immediately; skip it so the
            // first summarization fires AFTER one full interval,
            // matching the cadence operators configured.
            ticker.tick().await;
            loop {
                if self.shutdown.load(Ordering::Acquire) {
                    return;
                }
                ticker.tick().await;
                self.tick_once();
            }
        })
    }

    /// Run one summarize tick synchronously. Public for tests
    /// (the background loop calls this once per
    /// `summary_interval`); production code spawns and lets the
    /// loop drive it.
    pub fn tick_once(&self) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let mut batch: Vec<SummaryAnnouncement> = Vec::new();
        for kind in &self.config.fold_kinds {
            let Some(summarizer) = self.summarizers.get(kind) else {
                // Resolution ran at `new` time — this should be
                // unreachable in practice.
                continue;
            };
            // Dispatch on the concrete fold kind. Built-in
            // summarizers downcast the `FoldHandle` to their
            // expected fold type; custom impls follow the same
            // pattern.
            let summaries = if *kind == CapabilityFold::KIND_ID {
                let fold = self.mesh.capability_fold();
                let handle = CapabilityFoldHandle(fold);
                let ctx = SummarizerContext {
                    source_subnet: self.config.source_subnet,
                    generation,
                    fold: &handle as &dyn FoldHandle,
                };
                summarizer.summarize(&ctx)
            } else {
                // ReservationFold / future fold kinds need
                // `MeshNode` accessors that don't exist yet
                // (`MeshNode::reservation_fold()` is a substrate
                // gap). Skip cleanly until those land.
                Vec::new()
            };
            batch.extend(summaries);
        }
        let mut latest = self.latest.write();
        // Append-and-cap: keep up to the cap's worth of the most
        // recent batches' worth of summaries.
        for s in batch {
            if latest.len() >= LATEST_SUMMARIES_CAP {
                latest.remove(0);
            }
            latest.push(s);
        }
    }

    /// Snapshot of the latest summaries the loop has produced.
    /// Caller gets a `Vec` clone — modifying it doesn't affect
    /// the daemon's internal buffer.
    pub fn latest_summaries(&self) -> Vec<SummaryAnnouncement> {
        self.latest.read().clone()
    }

    /// Current generation counter. Reflects the number of
    /// `tick_once` calls (background-loop + explicit) since
    /// [`Self::new`].
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Signal the background loop to exit. Idempotent. The
    /// spawned `JoinHandle` resolves after the current tick's
    /// `interval.tick()` await returns.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Borrow the config the daemon was constructed with.
    /// Operator tooling (`net aggregator inspect`) reads it.
    pub fn config(&self) -> &AggregatorConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::fold::capability::{
        CapabilityFold, CapabilityMembership,
    };
    use crate::adapter::net::behavior::fold::wire::SignedAnnouncement;
    use crate::adapter::net::behavior::fold::{EnvelopeMeta, NodeState};
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::{MeshNodeConfig, SubnetId};
    use std::collections::BTreeMap;
    use std::net::SocketAddr;
    use std::time::Duration;

    async fn build_mesh() -> Arc<MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    }

    fn sign_cap(
        kp: &EntityKeypair,
        publisher: u64,
        class: u64,
        state: NodeState,
    ) -> SignedAnnouncement<CapabilityMembership> {
        SignedAnnouncement::sign(
            kp,
            CapabilityFold::KIND_ID,
            class,
            publisher,
            1,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: class,
                tags: Vec::new(),
                hardware: None,
                state,
                region: None,
                price_quote: None,
                reflex_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
            },
        )
        .expect("sign")
    }

    #[tokio::test]
    async fn new_validates_fold_kinds_against_summarizer_registry() {
        let mesh = build_mesh().await;
        // No fold kinds → NoFoldKinds.
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL);
        match AggregatorDaemon::new(cfg, mesh.clone()) {
            Err(AggregatorError::NoFoldKinds) => {}
            Err(other) => panic!("expected NoFoldKinds, got {other:?}"),
            Ok(_) => panic!("expected NoFoldKinds, got Ok"),
        }

        // Unknown fold kind without a custom override →
        // UnregisteredFoldKind.
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL).with_fold_kind(0xDEAD);
        match AggregatorDaemon::new(cfg, mesh.clone()) {
            Err(AggregatorError::UnregisteredFoldKind { kind }) => assert_eq!(kind, 0xDEAD),
            Err(other) => panic!("expected UnregisteredFoldKind, got {other:?}"),
            Ok(_) => panic!("expected UnregisteredFoldKind, got Ok"),
        }

        // Built-in kind (CapabilityFold) → ok.
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL).with_fold_kind(CapabilityFold::KIND_ID);
        AggregatorDaemon::new(cfg, mesh).expect("builtin kind validates");
    }

    #[tokio::test]
    async fn tick_once_summarizes_capability_fold_and_bumps_generation() {
        let mesh = build_mesh().await;
        // Prime the capability fold with two idle + one busy
        // publisher so the summary has non-zero bucket counts.
        let kp = EntityKeypair::generate();
        let fold = mesh.capability_fold();
        fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();
        fold.apply(sign_cap(&kp, 0xB, 2, NodeState::Idle)).unwrap();
        fold.apply(sign_cap(&kp, 0xC, 3, NodeState::Busy)).unwrap();

        let cfg = AggregatorConfig::new(SubnetId::new(&[3, 7]))
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(10));
        let agg = AggregatorDaemon::new(cfg, mesh).expect("new");

        assert_eq!(agg.generation(), 0);
        agg.tick_once();
        assert_eq!(agg.generation(), 1);
        let summaries = agg.latest_summaries();
        assert_eq!(summaries.len(), 1);
        let summary = &summaries[0];
        assert_eq!(summary.source_subnet, SubnetId::new(&[3, 7]));
        assert_eq!(summary.fold_kind, CapabilityFold::KIND_ID);
        assert_eq!(summary.generation, 1);
        // Lex-sorted: busy, faulty, idle, reserved.
        let idle = summary
            .buckets
            .iter()
            .find(|(n, _)| n == "idle")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        let busy = summary
            .buckets
            .iter()
            .find(|(n, _)| n == "busy")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(idle, 2);
        assert_eq!(busy, 1);

        // Second tick bumps generation again, retains the prior
        // summary in the latest-window.
        agg.tick_once();
        assert_eq!(agg.generation(), 2);
        assert_eq!(agg.latest_summaries().len(), 2);
    }

    #[tokio::test]
    async fn spawn_runs_until_shutdown_is_signalled() {
        let mesh = build_mesh().await;
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(20));
        let agg = Arc::new(AggregatorDaemon::new(cfg, mesh).expect("new"));
        let handle = agg.clone().spawn();

        // Wait long enough for at least one tick.
        tokio::time::sleep(Duration::from_millis(75)).await;
        let gen_during = agg.generation();
        assert!(
            gen_during >= 1,
            "expected at least one tick after 75ms (got {gen_during})"
        );

        agg.shutdown();
        // Loop exits within at most one interval after shutdown.
        let _ = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("loop exits within timeout")
            .expect("loop join clean");
        let gen_final = agg.generation();
        // Generation should have stopped advancing after shutdown
        // (allow at most one final tick to land if interval was
        // already ticking).
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(agg.generation() <= gen_final + 1);
    }

    #[tokio::test]
    async fn latest_summaries_capped_at_buffer_size() {
        let mesh = build_mesh().await;
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(10));
        let agg = AggregatorDaemon::new(cfg, mesh).expect("new");
        for _ in 0..(LATEST_SUMMARIES_CAP + 5) {
            agg.tick_once();
        }
        assert_eq!(agg.latest_summaries().len(), LATEST_SUMMARIES_CAP);
    }
}
