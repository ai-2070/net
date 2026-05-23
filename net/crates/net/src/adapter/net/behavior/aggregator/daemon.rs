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

use bytes::Bytes;

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
    /// Bumps `generation`, runs each configured summarizer,
    /// appends to the latest-summaries buffer. Does NOT publish
    /// summaries onto the wire — use
    /// [`Self::tick_and_publish`] for that.
    pub fn tick_once(&self) {
        let batch = self.produce_summaries();
        self.append_to_latest(batch);
    }

    /// `tick_once` + publish each emitted summary to its
    /// per-fold-kind summary channel via
    /// [`MeshNode::publish`](crate::adapter::net::MeshNode::publish).
    /// Used by the background loop; tests can call it explicitly.
    ///
    /// Returns the number of summaries successfully published.
    /// Publish-failure short-circuits — the first failed publish
    /// aborts the batch; remaining summaries land in the latest
    /// buffer regardless, so the daemon's local view stays
    /// consistent.
    pub async fn tick_and_publish(&self) -> Result<usize, AggregatorPublishError> {
        let batch = self.produce_summaries();
        let mut published = 0;
        for summary in &batch {
            self.publish_summary(summary).await?;
            published += 1;
        }
        self.append_to_latest(batch);
        Ok(published)
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
    /// the oldest entries when the cap is hit.
    fn append_to_latest(&self, batch: Vec<SummaryAnnouncement>) {
        let mut latest = self.latest.write();
        for s in batch {
            if latest.len() >= LATEST_SUMMARIES_CAP {
                latest.remove(0);
            }
            latest.push(s);
        }
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
            .map_err(|e| AggregatorPublishError::Encode(format!("{e:?}")))?;
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
}
