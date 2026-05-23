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

use parking_lot::RwLock;
use tokio::task::JoinHandle;

use async_trait::async_trait;
use bytes::Bytes;

use super::lifecycle::{LifecycleDaemon, LifecycleError};
use super::summarizer::{
    resolve_summarizer, CapabilityFoldHandle, FoldHandle, ReservationFoldHandle,
    SummarizerContext, SummaryAnnouncement, Summarizer,
};
use super::AggregatorConfig;
use crate::adapter::net::behavior::fold::capability::CapabilityFold;
use crate::adapter::net::behavior::fold::reservation::ReservationFold;
use crate::adapter::net::behavior::fold::FoldKind;
use crate::adapter::net::{
    AdapterError, ChannelConfig, ChannelId, ChannelName, MeshNode, PublishConfig,
};

/// Configuration-validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregatorError {
    /// A `fold_kind` listed in [`AggregatorConfig::fold_kinds`]
    /// has no built-in summarizer and no custom override in
    /// [`AggregatorConfig::custom_summarizers`].
    UnregisteredFoldKind {
        /// Kind id that failed to resolve.
        kind: u16,
    },
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

/// Publish-path failures from
/// [`AggregatorDaemon::tick_and_publish`]. Distinct from
/// `AggregatorError` so callers can distinguish
/// construction-time validation from runtime publish errors.
#[derive(Debug)]
pub enum AggregatorPublishError {
    /// `postcard::to_allocvec` failed to encode a summary.
    /// Doesn't carry the codec error directly so the wire type
    /// stays free of cross-crate dependencies.
    Encode(String),
    /// `MeshNode::publish` failed for the per-kind summary
    /// channel.
    Publish(AdapterError),
    /// A computed summary channel name failed validation.
    /// Should be unreachable in practice — the formatter only
    /// produces lowercase / digit / slash characters — but kept
    /// as a typed variant so a future channel-name spec change
    /// surfaces cleanly.
    InvalidChannelName(String),
}

impl std::fmt::Display for AggregatorPublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(msg) => write!(f, "encode failed: {msg}"),
            Self::Publish(e) => write!(f, "publish failed: {e}"),
            Self::InvalidChannelName(msg) => write!(f, "invalid channel name: {msg}"),
        }
    }
}

impl std::error::Error for AggregatorPublishError {}

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
    /// `LATEST_SUMMARIES_CAP` entries.
    ///
    /// Held as `Arc<Vec<...>>` so reads (`latest_summaries_arc`)
    /// are an O(1) Arc clone instead of a deep Vec clone. Writes
    /// rebuild the inner Vec then swap the Arc — copy cost is
    /// bounded by the cap so the rebuild is cheap.
    latest: Arc<RwLock<Arc<Vec<SummaryAnnouncement>>>>,
    /// Cooperative-shutdown flag. The background loop polls this
    /// between ticks; [`Self::shutdown`] sets it.
    shutdown: Arc<AtomicBool>,
    /// JoinHandle of the background loop spawned via
    /// [`LifecycleDaemon::on_start`]. Held under a `Mutex` so
    /// `on_stop` can take ownership and await it without racing
    /// the spawn path.
    background: parking_lot::Mutex<Option<JoinHandle<()>>>,
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
            latest: Arc::new(RwLock::new(Arc::new(Vec::with_capacity(LATEST_SUMMARIES_CAP)))),
            shutdown: Arc::new(AtomicBool::new(false)),
            background: parking_lot::Mutex::new(None),
        })
    }

    /// Spawn the background summarize loop and return its
    /// `JoinHandle`. The handle resolves when the loop exits
    /// (typically after [`Self::shutdown`] is called).
    ///
    /// The loop calls [`Self::tick_and_publish`] on each tick so
    /// summaries fan out to subscribers in addition to landing in
    /// the in-memory buffer. Publish errors are logged at `warn`
    /// and the loop continues — a transiently-wedged peer
    /// shouldn't stop subsequent ticks from publishing.
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
                if let Err(e) = self.tick_and_publish().await {
                    tracing::warn!(
                        error = %e,
                        source_subnet = %self.config.source_subnet,
                        "aggregator: tick_and_publish failed; loop continues",
                    );
                }
            }
        })
    }

    /// Run one summarize tick synchronously. Public for tests
    /// (the background loop calls [`Self::tick_and_publish`]
    /// once per `summary_interval`); production code spawns and
    /// lets the loop drive it.
    ///
    /// Bumps `generation`, runs each configured summarizer, then
    /// appends novel summaries to the latest-summaries buffer.
    /// Summaries whose `(source_subnet, buckets)` match the most
    /// recent entry for the same fold-kind are dropped — see
    /// [`Self::tick_and_publish`] for the rationale. Does NOT
    /// publish onto the wire.
    ///
    /// Returns the novel summaries just appended (empty when the
    /// tick was a no-op). Callers that need the freshly-produced
    /// batch — e.g. the `SummarizeNow` RPC path — avoid a full
    /// re-scan of the latest-summaries buffer by reading the
    /// return value directly.
    pub fn tick_once(&self) -> Vec<SummaryAnnouncement> {
        let batch = self.produce_summaries();
        let novel = self.filter_novel(batch);
        self.append_to_latest(novel.clone());
        novel
    }

    /// `tick_once` + publish each novel summary to its
    /// per-fold-kind summary channel via
    /// [`MeshNode::publish`](crate::adapter::net::MeshNode::publish).
    /// Used by the background loop; tests can call it explicitly.
    ///
    /// A summary is "novel" when its `(source_subnet, buckets)`
    /// differs from the most recent entry already in the
    /// latest-summaries buffer for the same `fold_kind`. Folds
    /// that change rarely (capability, reservation) otherwise
    /// republish byte-identical summaries every tick.
    ///
    /// Returns the number of summaries successfully published.
    /// Publish-failure short-circuits — the first failed publish
    /// aborts the batch; preceding summaries still land in the
    /// latest buffer so the daemon's local view stays consistent.
    pub async fn tick_and_publish(&self) -> Result<usize, AggregatorPublishError> {
        let batch = self.produce_summaries();
        let novel = self.filter_novel(batch);
        let mut published = 0;
        let mut kept: Vec<SummaryAnnouncement> = Vec::with_capacity(novel.len());
        for summary in novel {
            self.publish_summary(&summary).await?;
            published += 1;
            kept.push(summary);
        }
        self.append_to_latest(kept);
        Ok(published)
    }

    /// Drop summaries whose `(source_subnet, buckets)` matches
    /// the most recent prior entry in the latest buffer for the
    /// same fold-kind. Generation is intentionally not part of
    /// the equality — generation always advances tick-to-tick.
    fn filter_novel(&self, batch: Vec<SummaryAnnouncement>) -> Vec<SummaryAnnouncement> {
        if batch.is_empty() {
            return batch;
        }
        let latest = self.latest_summaries_arc();
        batch
            .into_iter()
            .filter(|summary| {
                let prev = latest
                    .iter()
                    .rev()
                    .find(|s| s.fold_kind == summary.fold_kind);
                match prev {
                    None => true,
                    Some(prev) => {
                        prev.source_subnet != summary.source_subnet
                            || prev.buckets != summary.buckets
                    }
                }
            })
            .collect()
    }

    /// Compute one tick's worth of summaries. Pure side-effect-
    /// per-call (bumps the generation counter), but doesn't
    /// mutate the latest-summaries buffer. Split out so
    /// [`Self::tick_once`] and [`Self::tick_and_publish`] share
    /// the per-fold-kind dispatch.
    fn produce_summaries(&self) -> Vec<SummaryAnnouncement> {
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
            } else if *kind == ReservationFold::KIND_ID {
                let fold = self.mesh.reservation_fold();
                let handle = ReservationFoldHandle(fold);
                let ctx = SummarizerContext {
                    source_subnet: self.config.source_subnet,
                    generation,
                    fold: &handle as &dyn FoldHandle,
                };
                summarizer.summarize(&ctx)
            } else {
                // Future fold kinds add an arm here. Custom
                // summarizers registered against arbitrary kind
                // ids reach the substrate via this branch when a
                // future fold-handle accessor lands on MeshNode.
                Vec::new()
            };
            batch.extend(summaries);
        }
        batch
    }

    /// Append a batch to the latest-summaries buffer, evicting
    /// the oldest entries (FIFO) when the cap is hit. Rebuilds
    /// the inner `Arc<Vec>` so concurrent readers holding the
    /// prior Arc continue seeing a consistent snapshot.
    fn append_to_latest(&self, batch: Vec<SummaryAnnouncement>) {
        if batch.is_empty() {
            return;
        }
        let mut slot = self.latest.write();
        let mut new_vec: Vec<SummaryAnnouncement> = (**slot).clone();
        for s in batch {
            if new_vec.len() >= LATEST_SUMMARIES_CAP {
                new_vec.remove(0);
            }
            new_vec.push(s);
        }
        *slot = Arc::new(new_vec);
    }

    /// Publish one summary onto its per-fold-kind summary
    /// channel. Encoding is postcard, matching the
    /// [`super::query_service`] wire format so receivers can
    /// decode the same shape from either the RPC reply or the
    /// channel fan-out.
    async fn publish_summary(
        &self,
        summary: &SummaryAnnouncement,
    ) -> Result<(), AggregatorPublishError> {
        let channel = self.summary_channel_name(summary.fold_kind)?;
        let publisher = self
            .mesh
            .channel_publisher(channel, PublishConfig::default());
        let bytes = postcard::to_allocvec(summary)
            .map_err(|e| AggregatorPublishError::Encode(e.to_string()))?;
        self.mesh
            .publish(&publisher, Bytes::from(bytes))
            .await
            .map_err(AggregatorPublishError::Publish)?;
        Ok(())
    }

    /// Canonical summary channel name for `fold_kind`. One
    /// summary channel per fold-kind per host; source-subnet
    /// discrimination is carried on the payload's
    /// `source_subnet` field. Format: `"summary/<hex_kind>"`.
    pub fn summary_channel_name(
        &self,
        fold_kind: u16,
    ) -> Result<ChannelName, AggregatorPublishError> {
        let name = format!("summary/{fold_kind:#06x}");
        ChannelName::new(&name).map_err(|e| {
            AggregatorPublishError::InvalidChannelName(format!("{name}: {e:?}"))
        })
    }

    /// Register every configured fold-kind's summary channel in
    /// `mesh`'s [`ChannelConfigRegistry`] with the aggregator's
    /// `summary_visibility`. Idempotent — `insert` replaces by
    /// name so a re-call is a no-op. Returns the count of
    /// channels registered.
    ///
    /// Operators that want visibility-enforced delivery (e.g.
    /// `Visibility::ParentVisible` so summaries reach the
    /// parent subnet but not siblings) call this once after
    /// `install_query_service`. Without it, summaries publish on
    /// the wire but the gateway sees no visibility config and
    /// falls back to its default behavior.
    pub fn register_summary_channels(&self) -> Result<usize, AggregatorPublishError> {
        let Some(registry) = self.mesh.channel_configs() else {
            // No registry installed — nothing to register. Not
            // an error; the gateway falls back to defaults.
            return Ok(0);
        };
        let mut registered = 0;
        for kind in &self.config.fold_kinds {
            let channel_name = self.summary_channel_name(*kind)?;
            let channel_id = ChannelId::parse(channel_name.as_str()).map_err(|e| {
                AggregatorPublishError::InvalidChannelName(format!(
                    "{}: {e:?}",
                    channel_name.as_str()
                ))
            })?;
            let cfg = ChannelConfig::new(channel_id).with_visibility(self.config.summary_visibility);
            registry.insert(cfg);
            registered += 1;
        }
        Ok(registered)
    }

    /// Snapshot of the latest summaries the loop has produced.
    /// Caller gets a `Vec` clone — modifying it doesn't affect
    /// the daemon's internal buffer.
    ///
    /// Hot-path callers (TUI render loops, RPC handlers) should
    /// prefer [`Self::latest_summaries_arc`] which avoids the
    /// deep clone.
    pub fn latest_summaries(&self) -> Vec<SummaryAnnouncement> {
        (**self.latest.read()).clone()
    }

    /// Cheap snapshot accessor: clones only the outer `Arc`. Use
    /// for hot-path readers (TUI render, fold-query RPC) that
    /// only need read-only access.
    pub fn latest_summaries_arc(&self) -> Arc<Vec<SummaryAnnouncement>> {
        Arc::clone(&*self.latest.read())
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

#[async_trait]
impl LifecycleDaemon for AggregatorDaemon {
    fn name(&self) -> &str {
        "aggregator"
    }

    async fn on_start(self: Arc<Self>) -> Result<(), LifecycleError> {
        let handle = self.clone().spawn();
        *self.background.lock() = Some(handle);
        Ok(())
    }

    async fn on_stop(&self) {
        self.shutdown.store(true, Ordering::Release);
        let handle = self.background.lock().take();
        if let Some(h) = handle {
            // Wait up to one interval + a small buffer so the
            // loop's `ticker.tick().await` returns and the
            // shutdown check fires before we abort.
            let deadline = self.config.summary_interval + std::time::Duration::from_millis(100);
            let _ = tokio::time::timeout(deadline, h).await;
        }
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

        // Second tick with mutated fold state produces a novel
        // summary; generation advances and the new summary lands
        // alongside the first.
        agg.mesh
            .capability_fold()
            .apply(sign_cap(&kp, 0xD, 4, NodeState::Idle))
            .unwrap();
        agg.tick_once();
        assert_eq!(agg.generation(), 2);
        assert_eq!(agg.latest_summaries().len(), 2);
    }

    #[tokio::test]
    async fn tick_skips_summary_when_buckets_are_unchanged() {
        // Pin the change-detection guard: a no-op tick (fold
        // state unchanged) advances `generation` but does NOT
        // append a duplicate summary to the latest buffer.
        let mesh = build_mesh().await;
        let kp = EntityKeypair::generate();
        let fold = mesh.capability_fold();
        fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();

        let cfg = AggregatorConfig::new(SubnetId::new(&[3]))
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_secs(60));
        let agg = AggregatorDaemon::new(cfg, mesh.clone()).expect("new");

        agg.tick_once();
        assert_eq!(agg.latest_summaries().len(), 1);
        let first_gen = agg.generation();
        agg.tick_once();
        assert!(agg.generation() > first_gen, "generation must advance");
        assert_eq!(
            agg.latest_summaries().len(),
            1,
            "unchanged fold state must not append a duplicate summary"
        );

        // Once fold state changes, the next tick lands a novel
        // summary.
        fold.apply(sign_cap(&kp, 0xB, 2, NodeState::Busy)).unwrap();
        agg.tick_once();
        assert_eq!(agg.latest_summaries().len(), 2);
    }

    #[tokio::test]
    async fn tick_and_publish_skips_publish_when_buckets_are_unchanged() {
        // Companion to `tick_skips_summary_when_buckets_are_unchanged`
        // — the wire-publish path also short-circuits unchanged
        // summaries, returning a published count of zero.
        let mesh = build_mesh().await;
        let kp = EntityKeypair::generate();
        let fold = mesh.capability_fold();
        fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();

        let cfg = AggregatorConfig::new(SubnetId::new(&[3]))
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_secs(60));
        let agg = AggregatorDaemon::new(cfg, mesh).expect("new");

        let first = agg.tick_and_publish().await.expect("first");
        assert_eq!(first, 1, "first tick publishes");
        let second = agg.tick_and_publish().await.expect("second");
        assert_eq!(second, 0, "unchanged buckets skip publish");
        assert_eq!(agg.latest_summaries().len(), 1);
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
        let kp = EntityKeypair::generate();
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(10));
        let agg = AggregatorDaemon::new(cfg, mesh.clone()).expect("new");
        // Mutate fold state between ticks so each summary is
        // novel (change-detection would otherwise drop dupes).
        for i in 0..(LATEST_SUMMARIES_CAP as u64 + 5) {
            mesh.capability_fold()
                .apply(sign_cap(
                    &kp,
                    0xA00 + i,
                    i + 1,
                    if i % 2 == 0 {
                        NodeState::Idle
                    } else {
                        NodeState::Busy
                    },
                ))
                .unwrap();
            agg.tick_once();
        }
        assert_eq!(agg.latest_summaries().len(), LATEST_SUMMARIES_CAP);
    }

    #[tokio::test]
    async fn summary_channel_name_renders_kind_as_hex_under_summary_prefix() {
        let mesh = build_mesh().await;
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL).with_fold_kind(CapabilityFold::KIND_ID);
        let agg = AggregatorDaemon::new(cfg, mesh).expect("new");
        let name = agg
            .summary_channel_name(CapabilityFold::KIND_ID)
            .expect("channel name");
        assert_eq!(name.as_str(), "summary/0x0001");
        let name = agg.summary_channel_name(0x0042).expect("channel name");
        assert_eq!(name.as_str(), "summary/0x0042");
    }

    #[tokio::test]
    async fn register_summary_channels_inserts_with_configured_visibility() {
        use crate::adapter::net::{ChannelConfigRegistry, Visibility};
        let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        let mut mesh = MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new");
        let registry = std::sync::Arc::new(ChannelConfigRegistry::new());
        mesh.set_channel_configs(registry);
        let mesh = std::sync::Arc::new(mesh);

        let agg_cfg = AggregatorConfig::new(SubnetId::new(&[3, 7]))
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_visibility(Visibility::ParentVisible);
        let agg = AggregatorDaemon::new(agg_cfg, mesh.clone()).expect("new");

        let count = agg.register_summary_channels().expect("register");
        assert_eq!(count, 1);
        let registered = mesh
            .channel_configs()
            .expect("registry")
            .get_by_name("summary/0x0001")
            .expect("channel registered");
        assert_eq!(registered.visibility, Visibility::ParentVisible);
    }

    #[tokio::test]
    async fn register_summary_channels_idempotent_on_re_call() {
        use crate::adapter::net::{ChannelConfigRegistry, Visibility};
        let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        let mut mesh = MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new");
        mesh.set_channel_configs(std::sync::Arc::new(ChannelConfigRegistry::new()));
        let mesh = std::sync::Arc::new(mesh);

        let agg_cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_visibility(Visibility::Global);
        let agg = AggregatorDaemon::new(agg_cfg, mesh.clone()).expect("new");

        let first = agg.register_summary_channels().expect("first");
        let second = agg.register_summary_channels().expect("second");
        assert_eq!(first, second);
        // Registry should still have the single entry — no
        // duplicates accumulated.
        assert_eq!(mesh.channel_configs().expect("registry").len(), 1);
    }

    #[tokio::test]
    async fn register_summary_channels_noop_without_registry() {
        // Mesh with no installed registry → register returns 0.
        let mesh = build_mesh().await;
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL).with_fold_kind(CapabilityFold::KIND_ID);
        let agg = AggregatorDaemon::new(cfg, mesh).expect("new");
        let count = agg.register_summary_channels().expect("register");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn reservation_fold_summarizer_buckets_by_state_label() {
        // Pin the second built-in summarizer end-to-end now that
        // `MeshNode::reservation_fold()` exposes the fold handle.
        // Publish two reservations in distinct states; the
        // `ReservationFoldSummarizer` produces one bucket per
        // observed state label.
        use crate::adapter::net::behavior::fold::reservation::{
            ReservationAnnouncement, ReservationFold, ReservationState,
        };
        use crate::adapter::net::behavior::fold::wire::SignedAnnouncement;

        let mesh = build_mesh().await;
        let kp = EntityKeypair::generate();
        let res_fold = mesh.reservation_fold();
        let fresh_deadline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64 + 60_000_000)
            .unwrap_or(0);
        res_fold
            .apply(
                SignedAnnouncement::sign(
                    &kp,
                    ReservationFold::KIND_ID,
                    0,
                    0xA,
                    1,
                    EnvelopeMeta::default(),
                    ReservationAnnouncement {
                        resource_id: 0xCAFE,
                        state: ReservationState::Reserved {
                            holder: 0xA,
                            until_unix_us: fresh_deadline,
                        },
                    },
                )
                .unwrap(),
            )
            .unwrap();
        res_fold
            .apply(
                SignedAnnouncement::sign(
                    &kp,
                    ReservationFold::KIND_ID,
                    0,
                    0xB,
                    1,
                    EnvelopeMeta::default(),
                    ReservationAnnouncement {
                        resource_id: 0xBEEF,
                        state: ReservationState::Free,
                    },
                )
                .unwrap(),
            )
            .unwrap();

        let cfg = AggregatorConfig::new(SubnetId::new(&[3, 7]))
            .with_fold_kind(ReservationFold::KIND_ID);
        let agg = AggregatorDaemon::new(cfg, mesh).expect("new");
        agg.tick_once();
        let summaries = agg.latest_summaries();
        assert_eq!(summaries.len(), 1);
        let summary = &summaries[0];
        assert_eq!(summary.fold_kind, ReservationFold::KIND_ID);
        let bucket = |name: &str| -> u64 {
            summary
                .buckets
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, c)| *c)
                .unwrap_or(0)
        };
        assert_eq!(bucket("free"), 1);
        // `Reserved { holder, until_unix_us }` debug-renders with
        // the named-fields shape; the summarizer's lowercase
        // `format!("{:?}").to_lowercase()` produces a bucket name
        // that starts with `reserved { ... }`. Assert by prefix.
        let reserved_count: u64 = summary
            .buckets
            .iter()
            .filter(|(n, _)| n.starts_with("reserved"))
            .map(|(_, c)| *c)
            .sum();
        assert_eq!(reserved_count, 1);
    }

    #[tokio::test]
    async fn tick_and_publish_advances_generation_and_appends_to_latest() {
        // Single-node test: publish has no subscribers (the
        // mesh has no peers), so the publish path succeeds with
        // zero recipients. The summary still lands in the
        // latest-summaries buffer and generation advances.
        let mesh = build_mesh().await;
        let kp = EntityKeypair::generate();
        let fold = mesh.capability_fold();
        fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();

        let cfg = AggregatorConfig::new(SubnetId::new(&[3]))
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_secs(60));
        let agg = AggregatorDaemon::new(cfg, mesh).expect("new");

        let before = agg.generation();
        let published = agg.tick_and_publish().await.expect("tick_and_publish");
        assert_eq!(published, 1, "one capability-fold summary should publish");
        assert_eq!(agg.generation(), before + 1);
        assert_eq!(agg.latest_summaries().len(), 1);
    }

    #[tokio::test]
    async fn lifecycle_handle_drives_tick_and_stop_halts_the_loop() {
        // End-to-end pin: wrap an AggregatorDaemon in a
        // LifecycleHandle, let the loop tick a few times, then
        // `stop()` and verify the generation stops advancing.
        use crate::adapter::net::behavior::aggregator::lifecycle::LifecycleHandle;
        let mesh = build_mesh().await;
        let kp = EntityKeypair::generate();
        let fold = mesh.capability_fold();
        fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();

        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(20));
        let agg: Arc<AggregatorDaemon> = Arc::new(AggregatorDaemon::new(cfg, mesh).expect("new"));
        let agg_trait: Arc<
            dyn crate::adapter::net::behavior::aggregator::lifecycle::LifecycleDaemon,
        > = agg.clone();

        let handle = LifecycleHandle::start(agg_trait).await.expect("start");
        tokio::time::sleep(Duration::from_millis(85)).await;
        let gen_during = agg.generation();
        assert!(
            gen_during >= 1,
            "expected at least one tick after 85ms (got {gen_during})"
        );

        handle.stop().await;
        let gen_at_stop = agg.generation();
        tokio::time::sleep(Duration::from_millis(80)).await;
        // After stop returns, no further ticks may land.
        assert_eq!(
            agg.generation(),
            gen_at_stop,
            "generation must not advance after LifecycleHandle::stop()"
        );
    }

    #[tokio::test]
    async fn lifecycle_on_start_is_idempotent_about_shutdown_flag() {
        // Pin a subtle invariant: re-entering on_start does not
        // observe a stale shutdown flag (the loop polls before
        // tick, so a fresh on_start after stop would never run if
        // the flag stayed set). Validate by direct LifecycleDaemon
        // trait calls.
        use crate::adapter::net::behavior::aggregator::lifecycle::LifecycleDaemon;
        let mesh = build_mesh().await;
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(15));
        let agg = Arc::new(AggregatorDaemon::new(cfg, mesh).expect("new"));
        assert_eq!(LifecycleDaemon::name(&*agg), "aggregator");
        LifecycleDaemon::on_start(agg.clone())
            .await
            .expect("on_start");
        tokio::time::sleep(Duration::from_millis(40)).await;
        LifecycleDaemon::on_stop(&*agg).await;
        let gen_after_first = agg.generation();
        assert!(gen_after_first >= 1);
    }
}
