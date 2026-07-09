//! Deck SDK — Phase 1 (Rust) per
//! [`DECK_SDK_PLAN.md`](../../../../../../docs/plans/DECK_SDK_PLAN.md).
//!
//! Operator-side surface, the dual of `behavior::meshos::sdk`
//! (the daemon-author surface). Daemons author against
//! `MeshOsDaemonSdk`; operators command against `DeckClient`.
//!
//! # Phase 1 scope
//!
//! - [`DeckClient`] — composes a [`MeshOsRuntime`] with an
//!   [`OperatorIdentity`].
//! - [`OperatorIdentity`] — operator-key newtype around
//!   [`super::super::EntityKeypair`]. **Phase 1 is non-signing:**
//!   the substrate's channel-auth surface doesn't yet expose
//!   operator-key signing, so admin commits ride the local loop
//!   un-signed and the SDK records the operator id for audit
//!   correlation. The signing seam wires in when the substrate
//!   slice that adds it lands (per the plan's "substrate gaps").
//! - [`AdminCommands`] — typed methods for every
//!   [`super::meshos::AdminEvent`] variant. Each publishes the
//!   admin event onto the loop's event stream and returns a
//!   [`ChainCommit`] correlation handle.
//! - [`SnapshotStream`] — `Stream` over
//!   [`super::meshos::MeshOsSnapshotReader`] polled at a
//!   configurable cadence (defaults to the loop's tick interval).
//!
//! # Phase 1 deferrals
//!
//! - **Operator-signed commits.** The substrate's verifier doesn't
//!   yet check operator signatures; the SDK records the operator
//!   id on each commit but does not sign the event payload. Slated
//!   for the substrate slice that adds operator-key channel-auth.
//! - **Audit queries (`audit()`).** Need a signed admin chain to
//!   query against; deferred to a slice that lands after the
//!   substrate's admin-chain commit + signing path.
//! - **Log stream (`subscribe_logs()`).** Needs per-daemon /
//!   per-node log-chain binding through RedEX `tail()`; deferred.
//! - **ICE (`ice()`).** Phase 2 substrate work (`ForceDrain`,
//!   `ForceEvictReplica`, …, blast-radius simulator); Phase 3 SDK
//!   surface. Locked decision #4 of the plan: blast-radius
//!   simulation is mandatory before commit — substrate-side
//!   contract not yet written.
//!
//! # Error model
//!
//! [`DeckError`] uses the `<<deck-sdk-kind:KIND>>MSG` discriminator
//! format every cross-language SDK parses. Kinds shipped in Phase 1:
//! `unknown_node`, `chain_commit_failed`, `loop_closed`,
//! `queue_full`, `stream_closed`.
//!
//! # Example
//!
//! ```ignore
//! use net::adapter::net::behavior::deck::{DeckClient, OperatorIdentity};
//! use net::adapter::net::behavior::meshos::{MeshOsConfig, MeshOsRuntime};
//!
//! let runtime = MeshOsRuntime::start(MeshOsConfig::default(), dispatcher);
//! let identity = OperatorIdentity::generate();
//! let deck = DeckClient::from_runtime(&runtime, identity);
//!
//! let commit = deck
//!     .admin()
//!     .enter_maintenance(node_id, None)
//!     .await?;
//! tracing::info!(commit_id = commit.commit_id(), "drain proposed");
//!
//! use futures::StreamExt;
//! let mut snaps = deck.snapshots();
//! if let Some(Ok(snap)) = snaps.next().await {
//!     // …render the latest state…
//! }
//! ```

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use futures::Stream;
use tokio::time::{interval, Interval};

use super::meshos::{
    ice_proposal_signing_payload, simulate_ice_proposal, AdminEvent, BlastRadius, ChainId,
    IceActionProposal, MeshOsEvent, MeshOsHandle, MeshOsHandleError, MeshOsRuntime, MeshOsSnapshot,
    MeshOsSnapshotReader, NodeId,
};
use crate::adapter::net::behavior::aggregator::{AggregatorDaemon, SummaryAnnouncement};
use crate::adapter::net::identity::EntityKeypair;
use crate::adapter::net::subnet::SubnetId;
use crate::adapter::net::MeshNode;
use crate::adapter::net::{ChannelHash, Visibility};

/// Operator identity. Phase 1 holds the operator key as an
/// [`EntityKeypair`] (the same ed25519 type daemons use) plus a
/// derived 64-bit operator id. The signing seam will widen here
/// when the substrate slice that adds operator-key channel-auth
/// lands; until then commits ride the local loop and the SDK
/// records the operator id for audit correlation.
#[derive(Clone, Debug)]
pub struct OperatorIdentity {
    keypair: Arc<EntityKeypair>,
    operator_id: u64,
}

impl OperatorIdentity {
    /// Wrap an existing keypair as an operator identity. The
    /// operator id derives from the keypair's `origin_hash`.
    pub fn from_keypair(keypair: EntityKeypair) -> Self {
        let operator_id = keypair.origin_hash();
        Self {
            keypair: Arc::new(keypair),
            operator_id,
        }
    }

    /// Generate a fresh keypair + identity. Convenience for tests
    /// and the tooling that bootstraps a one-shot operator.
    pub fn generate() -> Self {
        Self::from_keypair(EntityKeypair::generate())
    }

    /// 64-bit operator id derived from the underlying keypair's
    /// `origin_hash`. Stable across the operator's lifetime.
    pub fn operator_id(&self) -> u64 {
        self.operator_id
    }

    /// Borrow the underlying keypair. **Use sparingly.** The
    /// SDK's own signing helpers
    /// ([`Self::sign_proposal`], [`Self::sign_admin_event`])
    /// cover the canonical signing flows; reach for this only
    /// when implementing a cross-language signing seam (e.g. a
    /// FFI binding that needs to call its own ed25519 lib over
    /// the same `(domain || issued_at || blast_hash || postcard)`
    /// payload shape). Calls outside that envelope risk
    /// drift between the SDK's signing bytes and what the
    /// substrate verifier rebuilds.
    pub fn keypair(&self) -> &EntityKeypair {
        &self.keypair
    }
}

/// SDK error surface. Carries the operator-readable message + a
/// stable kind discriminator usable from cross-language consumers
/// via the `<<deck-sdk-kind:KIND>>MSG` envelope.
#[derive(Clone, Debug, thiserror::Error)]
#[error("<<deck-sdk-kind:{kind}>>{message}")]
pub struct DeckError {
    /// Stable kind discriminator. Lowercase + underscore-only;
    /// cross-language SDKs parse the surrounding
    /// `<<deck-sdk-kind:…>>` envelope to extract this verbatim.
    pub kind: &'static str,
    /// Operator-readable message.
    pub message: String,
}

impl DeckError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl From<MeshOsHandleError> for DeckError {
    fn from(err: MeshOsHandleError) -> Self {
        match err {
            MeshOsHandleError::LoopClosed => Self::new("loop_closed", "MeshOS loop has exited"),
            MeshOsHandleError::QueueFull => Self::new(
                "queue_full",
                "MeshOS source channel at capacity — back off + retry",
            ),
        }
    }
}

/// Type alias for the admin-command error surface. Shares the
/// underlying [`DeckError`] envelope; admin commits surface
/// `loop_closed` / `queue_full` kinds.
pub type AdminError = DeckError;

/// Type alias for the ICE-surface error type. Shares the
/// underlying [`DeckError`] envelope; ICE commits add the
/// `simulation_required` / `insufficient_signatures` kinds.
pub type IceError = DeckError;

/// Correlation handle returned by every admin commit. Phase 1
/// represents "the admin event was accepted by the loop's event
/// queue"; the substrate slice that adds a signed admin chain
/// will widen this to carry the chain sequence + commit hash.
///
/// Always carries the issuing operator id so audit downstream
/// (when wired) can correlate commits to the operator that
/// issued them.
#[derive(Clone, Debug)]
pub struct ChainCommit {
    commit_id: u64,
    operator_id: u64,
    event_kind: &'static str,
    committed_at: SystemTime,
}

impl ChainCommit {
    /// Process-local correlation id, monotonically increasing
    /// across every commit a single [`DeckClient`] produces.
    pub fn commit_id(&self) -> u64 {
        self.commit_id
    }

    /// Id of the operator that issued the commit.
    pub fn operator_id(&self) -> u64 {
        self.operator_id
    }

    /// Discriminator for the admin event the commit carried
    /// (e.g. `"enter_maintenance"`, `"drop_replicas"`).
    pub fn event_kind(&self) -> &'static str {
        self.event_kind
    }

    /// Wall-clock timestamp at which the SDK accepted the commit.
    /// Distinct from any per-chain commit sequence the substrate
    /// will eventually expose.
    pub fn committed_at(&self) -> SystemTime {
        self.committed_at
    }
}

/// Tunables for [`DeckClient`].
#[derive(Clone, Debug)]
pub struct DeckClientConfig {
    /// Cadence at which [`SnapshotStream`] polls the runtime's
    /// snapshot reader. Defaults to 100ms — same order of
    /// magnitude as the default loop tick so the stream surfaces
    /// each post-reconcile snapshot once.
    pub snapshot_poll_interval: Duration,
    /// Minimum operator signatures required to commit an ICE
    /// proposal (see [`SimulatedIceProposal::commit`]). Plan
    /// locks this in at 2-of-N by default,
    /// substrate-verified; this slice ships single-signature
    /// (`1`) as the SDK-side default because substrate-side
    /// multi-operator verification hasn't shipped yet. Operators
    /// who want client-enforced multi-op gating ahead of the
    /// substrate slice can bump this knob.
    pub ice_signature_threshold: usize,
}

impl Default for DeckClientConfig {
    fn default() -> Self {
        Self {
            snapshot_poll_interval: Duration::from_millis(100),
            ice_signature_threshold: 1,
        }
    }
}

/// Compact at-a-glance rollup of the runtime's latest snapshot.
/// Built by [`DeckClient::status_summary`]; designed for the
/// operator UI's "is everything OK?" header — one pass over
/// the snapshot to count each cohort, plus the two cluster-
/// wide flags ([`Self::freeze_remaining_ms`] and
/// [`Self::local_maintenance_active`]) operators care most
/// about at first glance.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StatusSummary {
    /// Per-health-class peer counts.
    pub peers: PeerCounts,
    /// Per-lifecycle-and-restart-state daemon counts.
    pub daemons: DaemonCounts,
    /// Number of replica chains the snapshot tracks.
    pub replica_chains: usize,
    /// Number of avoid-list entries on this node.
    pub avoid_list_entries: usize,
    /// Depth of the ring of recently-emitted actions (count
    /// of entries in `MeshOsSnapshot::recently_emitted`).
    /// This is "what reconcile recently asked for," NOT "what
    /// is currently in flight" — the executor doesn't signal
    /// completion back to the loop, so the ring caps at
    /// `action_queue_capacity` and never drains on its own.
    pub recently_emitted_count: usize,
    /// Executor failure ring depth.
    pub recent_failure_count: usize,
    /// Admin audit ring depth (signed ICE bundles + unsigned
    /// admin events).
    pub admin_audit_ring_depth: usize,
    /// Milliseconds remaining on the cluster-wide ICE freeze.
    /// `None` if no freeze is in effect.
    pub freeze_remaining_ms: Option<u64>,
    /// `true` iff this node's local maintenance state is
    /// anything other than `Active`. Operators read this for
    /// "is this node in maintenance right now?".
    pub local_maintenance_active: bool,
}

/// Peer-health counts within a [`StatusSummary`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PeerCounts {
    /// Peers responding within the heartbeat window.
    pub healthy: usize,
    /// Peers responding but slow.
    pub degraded: usize,
    /// Peers unreachable.
    pub unreachable: usize,
    /// Peers without a health sample yet.
    pub unknown: usize,
}

/// Aggregate `SubnetGateway` counters surfaced by
/// [`DeckClient::gateway_stats`]. Plain value type so operator
/// tooling can render / serialize / diff snapshots without
/// reaching into the substrate's atomic counters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayStats {
    /// The mesh node's local subnet — same as
    /// [`DeckClient::local_subnet`], echoed here so a
    /// `gateway stats` rendering doesn't need a second
    /// accessor call.
    pub local_subnet: SubnetId,
    /// Total cross-subnet visibility decisions that resolved to
    /// "forward" (publish-fanout admitted; subscribe-gate
    /// admitted). Monotonic-increasing for the lifetime of the
    /// gateway.
    pub forwarded: u64,
    /// Total decisions that resolved to "drop." Monotonic.
    pub dropped: u64,
    /// Snapshot of every peer subnet the gateway is bridging to,
    /// sorted by raw bits. Sourced from `SubnetGateway::peer_subnets`.
    pub peer_subnets: Vec<SubnetId>,
    /// Number of explicit `(channel, target-subnets)` rules in
    /// the export table — what `gateway exports` enumerates.
    pub export_rules: u64,
}

/// One row in [`DeckClient::subnets_with_members`]'s rollup.
/// Carries the subnet, the sorted member-`node_id` set, and a
/// flag marking the local subnet so renderers don't need a
/// second pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubnetRollup {
    /// Subnet this row represents.
    pub subnet: SubnetId,
    /// Sorted set of `node_id`s known to belong to this subnet.
    /// Empty when the local subnet has no peers (the local node
    /// is only included if the caller supplied its node id).
    pub members: Vec<u64>,
    /// `true` when [`Self::subnet`] matches the local mesh
    /// node's subnet.
    pub is_local: bool,
}

/// One-shot snapshot returned by [`DeckClient::aggregator_snapshot`].
/// Bundles every field a renderer needs in a single struct so
/// callers don't pay for five per-field lock acquisitions per
/// frame. `summaries` is an `Arc` so the snapshot itself is
/// cheap to clone.
#[derive(Clone, Debug)]
pub struct AggregatorSnapshot {
    /// Subnet the aggregator is summarizing.
    pub source_subnet: SubnetId,
    /// `FoldKind::KIND_ID`s the aggregator is configured for.
    pub fold_kinds: Vec<u16>,
    /// Aggregator's monotonic tick counter.
    pub generation: u64,
    /// Aggregator's tick cadence.
    pub summary_interval: std::time::Duration,
    /// Buffered summaries — `Arc::clone`-cheap.
    pub summaries: Arc<Vec<SummaryAnnouncement>>,
}

/// Per-replica row in an [`AggregatorRegistryGroupSnapshot`].
/// One per replica in declaration order.
#[derive(Clone, Debug)]
pub struct AggregatorReplicaRow {
    /// Replica's monotonic tick counter.
    pub generation: u64,
    /// `true` when the replica's last tick was within
    /// `3 × summary_interval`.
    pub healthy: bool,
    /// Operator-facing diagnostic when `healthy == false`.
    pub diagnostic: Option<String>,
    /// Placement decision recorded at spawn time (only present
    /// when the group was spawned via
    /// `LifecycleGroup::spawn_with_placement`).
    pub placement_node_id: Option<u64>,
}

/// Snapshot of one registered aggregator group. Built by
/// [`DeckClient::aggregator_registry_snapshot`] and consumed by
/// `net aggregator ls` + the future Deck panel.
#[derive(Clone, Debug)]
pub struct AggregatorRegistryGroupSnapshot {
    /// Operator-chosen group name.
    pub name: String,
    /// 32-byte group seed for deterministic identity.
    pub group_seed: [u8; 32],
    /// Per-replica rows in declaration order.
    pub replicas: Vec<AggregatorReplicaRow>,
}

/// Snapshot of every aggregator group registered on the node.
#[derive(Clone, Debug, Default)]
pub struct AggregatorRegistrySnapshot {
    /// Groups in lexicographic order by name (matches the
    /// registry's `entries()` ordering).
    pub groups: Vec<AggregatorRegistryGroupSnapshot>,
}

/// Daemon counts within a [`StatusSummary`]. Lifecycle
/// counts are disjoint partitions of the registered set;
/// `crash_looping` / `backing_off` are orthogonal restart-
/// state markers that overlap with lifecycle (a `Stopped`
/// daemon can also be `BackingOff`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DaemonCounts {
    /// Currently running.
    pub running: usize,
    /// Start requested; awaiting confirmation.
    pub starting: usize,
    /// Stop requested; awaiting confirmation.
    pub stopping: usize,
    /// Not currently running.
    pub stopped: usize,
    /// In supervisor's BackingOff window. Orthogonal to
    /// lifecycle — a daemon in this state is typically also
    /// `Stopped`.
    pub backing_off: usize,
    /// In supervisor's CrashLooping window — has crossed the
    /// crash-loop threshold and is parked for a longer cooldown.
    pub crash_looping: usize,
}

/// Build a [`StatusSummary`] from a snapshot. One pass over
/// the per-peer and per-daemon maps; the rest of the rollup
/// is direct field reads.
fn build_status_summary(snap: &MeshOsSnapshot) -> StatusSummary {
    let mut peers = PeerCounts::default();
    for peer in snap.peers.values() {
        match peer.health {
            Some(super::meshos::PeerHealthSnapshot::Healthy) => peers.healthy += 1,
            Some(super::meshos::PeerHealthSnapshot::Degraded) => peers.degraded += 1,
            Some(super::meshos::PeerHealthSnapshot::Unreachable) => peers.unreachable += 1,
            None => peers.unknown += 1,
        }
    }
    let mut daemons = DaemonCounts::default();
    for d in snap.daemons.values() {
        match d.lifecycle {
            super::meshos::DaemonLifecycleSnapshot::Running => daemons.running += 1,
            super::meshos::DaemonLifecycleSnapshot::Starting => daemons.starting += 1,
            super::meshos::DaemonLifecycleSnapshot::Stopping => daemons.stopping += 1,
            super::meshos::DaemonLifecycleSnapshot::Stopped => daemons.stopped += 1,
        }
        match d.restart_state {
            super::meshos::RestartStateSnapshot::Idle => {}
            super::meshos::RestartStateSnapshot::BackingOff { .. } => daemons.backing_off += 1,
            super::meshos::RestartStateSnapshot::CrashLooping { .. } => daemons.crash_looping += 1,
        }
    }
    let maintenance_active = !matches!(
        snap.local_maintenance,
        super::meshos::MaintenanceStateSnapshot::Active
    );
    StatusSummary {
        peers,
        daemons,
        replica_chains: snap.replicas.len(),
        avoid_list_entries: snap.avoid_list.len(),
        recently_emitted_count: snap.recently_emitted.len(),
        recent_failure_count: snap.recent_failures.len(),
        admin_audit_ring_depth: snap.admin_audit.len(),
        freeze_remaining_ms: snap.freeze_remaining_ms,
        local_maintenance_active: maintenance_active,
    }
}

/// Operator-facing client. Composes a [`MeshOsHandle`] +
/// [`MeshOsSnapshotReader`] + [`OperatorIdentity`] into the
/// surface Deck-the-binary (and other operator tools) bind
/// against.
///
/// Constructed via [`Self::from_runtime`] (when the caller holds
/// the live runtime) or [`Self::new`] (when the caller already
/// has handle + reader and wants to compose explicitly).
#[derive(Clone)]
pub struct DeckClient {
    handle: MeshOsHandle,
    snapshot_reader: MeshOsSnapshotReader,
    identity: OperatorIdentity,
    config: DeckClientConfig,
    /// Wrapped in `Arc` so clones share the same counter —
    /// `commit_id`s stay monotonic across every fan-out use of
    /// a single `DeckClient`. Without this, a clone would
    /// produce its own counter and two siblings could emit
    /// colliding `commit_id`s for distinct commits.
    commit_seq: Arc<AtomicU64>,
    /// Optional operator-key registry. When present, every ICE
    /// commit verifies each [`OperatorSignature`] against the
    /// registered public key before publishing. When absent,
    /// signatures pass through unchecked — useful for local /
    /// in-process tests but unsafe for any deployment that
    /// hasn't yet wired up the substrate verifier.
    operator_registry: Option<Arc<OperatorRegistry>>,
    /// Optional reference to the running `MeshNode`. Wired in via
    /// [`Self::with_mesh`] when the operator surface needs to
    /// read subnet / gateway / channel state that doesn't live
    /// in the [`MeshOsSnapshot`]. `None` when the deck is running
    /// against a degenerate / mesh-less runtime (test harnesses,
    /// pre-attach CLI invocations); the subnet/gateway accessors
    /// gracefully report "no mesh installed" in that case.
    mesh: Option<Arc<MeshNode>>,
    /// Optional reference to a running [`AggregatorDaemon`].
    /// Wired in via [`Self::with_aggregator`] when an aggregator
    /// is running in-process alongside the deck. Powers the
    /// `AGGREGATORS` Deck panel + the
    /// `DeckClient::aggregator_*` accessors. `None` for decks
    /// that don't host an aggregator (most operator binaries —
    /// they're queriers, not hosts).
    aggregator: Option<Arc<AggregatorDaemon>>,
}

impl DeckClient {
    /// Explicit constructor. Use when the caller already holds
    /// a [`MeshOsHandle`] + [`MeshOsSnapshotReader`] (e.g. when
    /// composing with other subsystems that share the same
    /// runtime).
    pub fn new(
        handle: MeshOsHandle,
        snapshot_reader: MeshOsSnapshotReader,
        identity: OperatorIdentity,
        config: DeckClientConfig,
    ) -> Self {
        Self {
            handle,
            snapshot_reader,
            identity,
            config,
            commit_seq: Arc::new(AtomicU64::new(0)),
            operator_registry: None,
            mesh: None,
            aggregator: None,
        }
    }

    /// Install a live `MeshNode` reference so the subnet and
    /// gateway accessors ([`Self::local_subnet`],
    /// [`Self::known_subnets`], [`Self::gateway_stats`],
    /// [`Self::gateway_exports`], [`Self::channel_visibility`])
    /// can read substrate state that doesn't flow through the
    /// [`MeshOsSnapshot`] stream. Without this the accessors
    /// gracefully report "no mesh installed."
    pub fn with_mesh(mut self, mesh: Arc<MeshNode>) -> Self {
        self.mesh = Some(mesh);
        self
    }

    /// Install a live [`AggregatorDaemon`] reference. Powers the
    /// `aggregator_*` accessors + the Deck `AGGREGATORS` panel.
    pub fn with_aggregator(mut self, aggregator: Arc<AggregatorDaemon>) -> Self {
        self.aggregator = Some(aggregator);
        self
    }

    /// Convenience constructor that pulls the handle + snapshot
    /// reader off a live runtime. Borrows the runtime; the
    /// returned client outlives the borrow because both
    /// `MeshOsHandle` and `MeshOsSnapshotReader` are clone-shared.
    pub fn from_runtime(runtime: &MeshOsRuntime, identity: OperatorIdentity) -> Self {
        Self::new(
            runtime.handle_clone(),
            runtime.snapshot_reader().clone(),
            identity,
            DeckClientConfig::default(),
        )
    }

    /// Override the default [`DeckClientConfig`] on an existing
    /// client. Builder-style.
    pub fn with_config(mut self, config: DeckClientConfig) -> Self {
        self.config = config;
        self
    }

    /// Install an [`OperatorRegistry`] that gates every ICE
    /// commit on operator-signature verification. Without a
    /// registry installed, commits accept any signature bundle
    /// that meets the threshold — useful for local tests but
    /// not for deployment.
    pub fn with_operator_registry(mut self, registry: OperatorRegistry) -> Self {
        self.operator_registry = Some(Arc::new(registry));
        self
    }

    /// Borrow the installed operator registry, if any.
    pub fn operator_registry(&self) -> Option<&OperatorRegistry> {
        self.operator_registry.as_deref()
    }

    /// Borrow the operator identity.
    pub fn identity(&self) -> &OperatorIdentity {
        &self.identity
    }

    /// Build an [`AdminCommands`] surface bound to this client.
    /// Each method publishes the corresponding admin event and
    /// returns a [`ChainCommit`].
    pub fn admin(&self) -> AdminCommands<'_> {
        AdminCommands { client: self }
    }

    /// Build an [`IceCommands`] surface — the break-glass
    /// operator path. Each method returns an [`IceProposal`]
    /// that must be `simulate()`d before `commit()` per the
    /// plan's locked decision #4.
    pub fn ice(&self) -> IceCommands<'_> {
        IceCommands { client: self }
    }

    /// Build an [`AuditQuery`] reading the substrate's ICE
    /// audit ring (every `SignedIceCommit` the loop's verifier
    /// observed — accepted or rejected). Fluent builder: chain
    /// `by_operator`, `between`, `force_only`, `recent` before
    /// `collect()`. See [`AuditQuery`] for the per-method
    /// semantics + the current Phase 1 scope (ICE only — the
    /// non-force admin chain query path lands when the
    /// substrate's signed admin chain ships).
    pub fn audit(&self) -> AuditQuery<'_> {
        AuditQuery::new(self)
    }

    /// Subscribe to the substrate's executor-failure ring.
    /// Returns a [`FailureStream`] that tails each
    /// [`super::meshos::FailureRecord`] as the executor records
    /// it. Same seq-watermark dedup pattern as
    /// [`AuditStream`] / [`LogStream`].
    ///
    /// `since_seq` seeds the initial watermark; passing `0`
    /// (the default) tails new failures from "now" onwards.
    pub fn subscribe_failures(&self, since_seq: u64) -> FailureStream {
        FailureStream::new(
            self.snapshot_reader.clone(),
            self.config.snapshot_poll_interval,
            since_seq,
        )
    }

    /// Subscribe to the substrate's per-node log ring with
    /// the given filter. Returns a [`LogStream`] that tails
    /// matching log lines as they arrive — same dedup pattern
    /// as the audit-tail stream (monotonic per-runtime `seq`
    /// watermark).
    ///
    /// `filter` defaults to "everything"; chain
    /// [`LogFilter::min_level`] / [`LogFilter::with_daemon`]
    /// / [`LogFilter::with_node`] to narrow the result.
    pub fn subscribe_logs(&self, filter: LogFilter) -> LogStream {
        LogStream::new(
            self.snapshot_reader.clone(),
            self.config.snapshot_poll_interval,
            filter,
        )
    }

    /// Open a [`SnapshotStream`] over the runtime's snapshot
    /// reader. The stream polls at
    /// [`DeckClientConfig::snapshot_poll_interval`] and emits a
    /// `Result<MeshOsSnapshot, DeckError>` on every poll.
    /// Closing the stream is a `drop`.
    pub fn snapshots(&self) -> SnapshotStream {
        SnapshotStream::new(
            self.snapshot_reader.clone(),
            self.config.snapshot_poll_interval,
        )
    }

    /// One-shot read of the runtime's latest snapshot.
    /// Synchronous — one atomic load on the snapshot pointer
    /// plus a clone of the underlying data. Use for one-off
    /// reads ("what's the freeze state right now?"); prefer
    /// [`Self::snapshots`] when iterating over many ticks.
    pub fn status(&self) -> MeshOsSnapshot {
        self.snapshot_reader.read()
    }

    /// Live-updating [`StatusSummary`] stream. Polls the
    /// snapshot reader at
    /// [`DeckClientConfig::snapshot_poll_interval`] and emits
    /// a new summary whenever the rollup actually changes
    /// (PartialEq dedup) — operator dashboards bind here for
    /// "render only when something is different." The first
    /// summary always emits.
    pub fn status_summary_stream(&self) -> StatusSummaryStream {
        StatusSummaryStream::new(
            self.snapshot_reader.clone(),
            self.config.snapshot_poll_interval,
        )
    }

    /// Roll the snapshot up into a compact at-a-glance status
    /// summary — daemon-health counts, peer-health counts,
    /// freeze / maintenance flags, queue depths. Useful for
    /// the operator UI's "is everything OK?" header. One
    /// snapshot load + a single iterator pass; no full clone.
    pub fn status_summary(&self) -> StatusSummary {
        build_status_summary(&self.snapshot_reader.load())
    }

    /// Borrow the latest peer summary. One snapshot load + a
    /// clone of just the peers map.
    pub fn peers(&self) -> std::collections::BTreeMap<NodeId, super::meshos::PeerSnapshot> {
        self.snapshot_reader.load().peers.clone()
    }

    /// This deck's local mesh node's `SubnetId`, or `None` when
    /// no `MeshNode` has been wired in via [`Self::with_mesh`].
    /// Powers `net subnet show`.
    pub fn local_subnet(&self) -> Option<SubnetId> {
        self.mesh.as_ref().map(|m| m.local_subnet())
    }

    /// Snapshot of every `(node_id, subnet_id)` pair the local
    /// mesh has cached from signature-verified capability
    /// announcements, sorted by `node_id`. Empty when no
    /// `MeshNode` is wired in. Powers `net subnet ls` and
    /// `net subnet tree`.
    pub fn known_subnets(&self) -> Vec<(u64, SubnetId)> {
        self.mesh
            .as_ref()
            .map(|m| m.known_subnets())
            .unwrap_or_default()
    }

    /// Group `known_subnets` into one row per subnet with sorted
    /// member ids. Pass `Some(node_id)` to include the local node
    /// under its own subnet's members (the CLI's `subnet ls`
    /// surface does this); pass `None` to omit the local node
    /// from members but still flag its row via
    /// [`SubnetRollup::is_local`] (the deck SUBNETS tab does this).
    ///
    /// The local subnet always appears as a row, even when no
    /// peers are known under it.
    pub fn subnets_with_members(&self, local_node_id: Option<u64>) -> Vec<SubnetRollup> {
        let local = self.local_subnet();
        let mut buckets: std::collections::BTreeMap<u32, std::collections::BTreeSet<u64>> =
            std::collections::BTreeMap::new();
        for (node_id, subnet) in self.known_subnets() {
            buckets.entry(subnet.raw()).or_default().insert(node_id);
        }
        if let Some(local_subnet) = local {
            let entry = buckets.entry(local_subnet.raw()).or_default();
            if let Some(id) = local_node_id {
                entry.insert(id);
            }
        }
        buckets
            .into_iter()
            .map(|(raw, members)| {
                let subnet = SubnetId::from_raw(raw);
                SubnetRollup {
                    subnet,
                    members: members.into_iter().collect(),
                    is_local: local == Some(subnet),
                }
            })
            .collect()
    }

    /// Aggregate gateway counters for `net gateway stats`.
    /// Returns `None` when the mesh has no installed
    /// `ChannelConfigRegistry` — in that case the gateway isn't
    /// built and there's nothing to report.
    pub fn gateway_stats(&self) -> Option<GatewayStats> {
        let gw = self.mesh.as_ref().and_then(|m| m.gateway())?;
        Some(GatewayStats {
            local_subnet: gw.local_subnet(),
            forwarded: gw.forwarded_count(),
            dropped: gw.dropped_count(),
            peer_subnets: gw.peer_subnets(),
            export_rules: gw.exports().len() as u64,
        })
    }

    /// Snapshot of the gateway's export table as
    /// `(channel_hash, target_subnets)` pairs, sorted by
    /// `channel_hash`. Empty when no gateway is installed.
    pub fn gateway_exports(&self) -> Vec<(u16, Vec<SubnetId>)> {
        self.mesh
            .as_ref()
            .and_then(|m| m.gateway())
            .map(|gw| gw.exports())
            .unwrap_or_default()
    }

    /// Resolve a channel name to its [`Visibility`] config, or
    /// `None` when no `ChannelConfigRegistry` has been installed
    /// or the name isn't registered. Falls back through the
    /// registry's prefix table via `get_by_name`. Powers
    /// `net channel visibility <name>`.
    pub fn channel_visibility(&self, channel_name: &str) -> Option<Visibility> {
        let mesh = self.mesh.as_ref()?;
        let registry = mesh.channel_configs()?;
        let cfg = registry.get_by_name(channel_name)?;
        Some(cfg.visibility)
    }

    /// Snapshot every registered channel as `(name, visibility)`
    /// pairs for `net channel ls`, sorted by name. Empty when no
    /// `ChannelConfigRegistry` has been installed.
    pub fn channels(&self) -> Vec<(String, Visibility)> {
        let Some(mesh) = self.mesh.as_ref() else {
            return Vec::new();
        };
        let Some(registry) = mesh.channel_configs() else {
            return Vec::new();
        };
        registry
            .snapshot()
            .into_iter()
            .map(|(name, cfg)| (name, cfg.visibility))
            .collect()
    }

    /// Lookup a channel's wire-`u16` hash for use with
    /// `gateway_exports`. `None` when no registry is installed
    /// or the channel isn't registered. Convenience for
    /// `net gateway export <channel> ...` to translate the
    /// human-readable channel name into the wire key the
    /// gateway's export table is keyed on.
    pub fn channel_wire_hash(&self, channel_name: &str) -> Option<u16> {
        let mesh = self.mesh.as_ref()?;
        let registry = mesh.channel_configs()?;
        let cfg = registry.get_by_name(channel_name)?;
        Some(cfg.channel_id.wire_hash())
    }

    /// Lookup a channel's canonical `ChannelHash` (u64). Same
    /// shape as [`Self::channel_wire_hash`] but returns the full
    /// 64-bit hash callers use for fold + ACL lookups.
    pub fn channel_canonical_hash(&self, channel_name: &str) -> Option<ChannelHash> {
        let mesh = self.mesh.as_ref()?;
        let registry = mesh.channel_configs()?;
        let cfg = registry.get_by_name(channel_name)?;
        Some(cfg.channel_id.hash())
    }

    /// `true` when a live [`AggregatorDaemon`] is installed via
    /// [`Self::with_aggregator`]. Lets the AGGREGATORS Deck panel
    /// discriminate between "no aggregator wired" and "aggregator
    /// wired but has nothing to report yet."
    pub fn aggregator_installed(&self) -> bool {
        self.aggregator.is_some()
    }

    /// Snapshot the running aggregator's latest summaries.
    /// Empty vec when no aggregator is installed.
    pub fn aggregator_summaries(&self) -> Vec<SummaryAnnouncement> {
        self.aggregator
            .as_ref()
            .map(|a| a.latest_summaries())
            .unwrap_or_default()
    }

    /// Cheap shared-snapshot variant of [`Self::aggregator_summaries`]
    /// — clones only the outer `Arc`. Hot-path callers (TUI render
    /// loops) should prefer this.
    pub fn aggregator_summaries_arc(&self) -> Arc<Vec<SummaryAnnouncement>> {
        self.aggregator
            .as_ref()
            .map(|a| a.latest_summaries_arc())
            .unwrap_or_else(|| Arc::new(Vec::new()))
    }

    /// One-call accessor that returns every aggregator field a
    /// renderer needs in one struct. Replaces the per-field hops
    /// (`aggregator_source_subnet` + `aggregator_fold_kinds` +
    /// `aggregator_generation` + `aggregator_summary_interval` +
    /// `aggregator_summaries`) — five lock acquisitions and two
    /// Vec clones per frame collapse to one struct construction
    /// and one Arc clone.
    ///
    /// Returns `None` when no aggregator is installed.
    pub fn aggregator_snapshot(&self) -> Option<AggregatorSnapshot> {
        let agg = self.aggregator.as_ref()?;
        let config = agg.config();
        Some(AggregatorSnapshot {
            source_subnet: config.source_subnet,
            fold_kinds: config.fold_kinds.clone(),
            generation: agg.generation(),
            summary_interval: config.summary_interval,
            summaries: agg.latest_summaries_arc(),
        })
    }

    /// Snapshot every aggregator group registered on the
    /// installed `MeshNode`'s
    /// [`AggregatorRegistry`](super::aggregator::registry::AggregatorRegistry).
    /// Returns `None` when no mesh is wired in or no registry has
    /// been installed via `MeshNode::set_aggregator_registry`.
    ///
    /// Used by `net aggregator ls` + the future
    /// Deck AGGREGATORS-list panel. Per-replica health is
    /// surfaced inline so CLI / TUI render one shot of data
    /// without follow-up calls.
    pub async fn aggregator_registry_snapshot(&self) -> Option<AggregatorRegistrySnapshot> {
        let mesh = self.mesh.as_ref()?;
        let registry = mesh.aggregator_registry()?;
        let entries = registry.entries();
        let mut groups = Vec::with_capacity(entries.len());
        for entry in entries {
            // One lock + outside-the-guard health-join per group,
            // via the entry's snapshot helper. Previously this
            // path took three sequential lock acquisitions
            // (`replicas` / `placements` / `health`) per group +
            // a slow `health()` blocked concurrent
            // `register`/`unregister` writers. See
            // `AggregatorGroupEntry::snapshot` for the rationale.
            let snap = entry.snapshot().await;
            let rows = snap
                .replicas
                .iter()
                .enumerate()
                .map(|(idx, replica)| {
                    let health = snap.healths.get(idx).cloned().unwrap_or(
                        crate::adapter::net::behavior::lifecycle::ReplicaHealth {
                            healthy: true,
                            diagnostic: None,
                        },
                    );
                    let placement_node_id = snap.placements.get(idx).map(|p| p.node_id);
                    AggregatorReplicaRow {
                        generation: replica.generation(),
                        healthy: health.healthy,
                        diagnostic: health.diagnostic,
                        placement_node_id,
                    }
                })
                .collect();
            groups.push(AggregatorRegistryGroupSnapshot {
                name: entry.name.clone(),
                group_seed: entry.group_seed,
                replicas: rows,
            });
        }
        Some(AggregatorRegistrySnapshot { groups })
    }

    /// Aggregator's monotonic tick counter, or `0` when none
    /// installed.
    pub fn aggregator_generation(&self) -> u64 {
        self.aggregator
            .as_ref()
            .map(|a| a.generation())
            .unwrap_or(0)
    }

    /// Aggregator's source subnet — what the daemon is summarizing.
    /// `None` when no aggregator is installed.
    pub fn aggregator_source_subnet(&self) -> Option<SubnetId> {
        self.aggregator.as_ref().map(|a| a.config().source_subnet)
    }

    /// List of fold-kind ids the aggregator is configured to
    /// summarize. Empty when no aggregator is installed.
    pub fn aggregator_fold_kinds(&self) -> Vec<u16> {
        self.aggregator
            .as_ref()
            .map(|a| a.config().fold_kinds.clone())
            .unwrap_or_default()
    }

    /// Aggregator's summary cadence, or zero when none installed.
    pub fn aggregator_summary_interval(&self) -> std::time::Duration {
        self.aggregator
            .as_ref()
            .map(|a| a.config().summary_interval)
            .unwrap_or_default()
    }

    /// Borrow the latest daemon summary keyed by daemon id.
    pub fn daemons(&self) -> std::collections::BTreeMap<u64, super::meshos::DaemonSnapshot> {
        self.snapshot_reader.load().daemons.clone()
    }

    /// Borrow the latest replica summary keyed by chain id.
    pub fn replicas(&self) -> std::collections::BTreeMap<ChainId, super::meshos::ReplicaSnapshot> {
        self.snapshot_reader.load().replicas.clone()
    }

    /// Read this node's local maintenance state.
    pub fn local_maintenance(&self) -> super::meshos::MaintenanceStateSnapshot {
        self.snapshot_reader.load().local_maintenance.clone()
    }

    /// Read the cluster-wide ICE freeze remaining time. `None`
    /// when no freeze is in effect.
    pub fn freeze_remaining_ms(&self) -> Option<u64> {
        self.snapshot_reader.load().freeze_remaining_ms
    }

    /// Borrow the latest executor-side failure ring. Bounded
    /// by [`super::meshos::RECENT_FAILURES_CAPACITY`]; ordered
    /// oldest-first (FIFO). Operators read this to see what
    /// the action dispatcher rejected recently. One snapshot
    /// load + a clone of just the failure ring.
    pub fn recent_failures(&self) -> Vec<super::meshos::FailureRecord> {
        self.snapshot_reader
            .load()
            .recent_failures
            .iter()
            .cloned()
            .collect()
    }

    /// Runtime-epoch identifier this MeshOsLoop stamped at
    /// startup. Stable for the lifetime of the loop task and
    /// changes on every restart. Consumers dedup'ing with
    /// `since(seq)` watermarks pair every saved watermark
    /// with this value — when it flips, reset the watermark
    /// to 0 rather than silently filtering post-restart
    /// records as "smaller than my last seq."
    pub fn runtime_epoch_id(&self) -> u64 {
        self.snapshot_reader.load().runtime_epoch_id
    }

    /// Highest `seq` currently visible on the admin-audit
    /// ring. Returns `0` when the ring is empty. Lets a
    /// caller's `since(seq)` pagination distinguish "ahead of
    /// the head" (caller's watermark > head_seq) from "no new
    /// records yet" (watermark == head_seq) — the audit
    /// stream itself swallows both cases silently.
    pub fn audit_head_seq(&self) -> u64 {
        self.snapshot_reader
            .load()
            .admin_audit
            .last()
            .map(|r| r.seq)
            .unwrap_or(0)
    }

    /// Highest `seq` currently visible on the log ring.
    /// Returns `0` when the ring is empty. Same purpose as
    /// [`Self::audit_head_seq`] for the log-stream surface.
    pub fn log_head_seq(&self) -> u64 {
        self.snapshot_reader
            .load()
            .log_ring
            .last()
            .map(|r| r.seq)
            .unwrap_or(0)
    }

    /// Highest `seq` currently visible on the failure ring.
    /// Returns `0` when the ring is empty.
    pub fn failure_head_seq(&self) -> u64 {
        self.snapshot_reader
            .load()
            .recent_failures
            .iter()
            .next_back()
            .map(|r| r.seq)
            .unwrap_or(0)
    }

    /// Like [`Self::recent_failures`] but keeps only entries
    /// with `recorded_at_ms > since_ms`. Pagination primitive:
    /// poll periodically, persist the max `recorded_at_ms`
    /// observed, and re-query with that value to surface only
    /// new failures.
    ///
    /// Note: same-ms collisions can land on the boundary —
    /// records sharing the cutoff value's exact ms might be
    /// missed if they were added between polls. The proper
    /// seq-based stream lands once [`super::meshos::FailureRecord`]
    /// gains a monotonic seq field (next substrate slice).
    pub fn recent_failures_since(&self, since_ms: u64) -> Vec<super::meshos::FailureRecord> {
        self.snapshot_reader
            .load()
            .recent_failures
            .iter()
            .filter(|r| r.recorded_at_ms > since_ms)
            .cloned()
            .collect()
    }

    /// Await a snapshot matching `predicate`. Event-driven (E-9/E-10):
    /// blocks on the loop's structural change signal (the snapshot
    /// reader's `subscribe_changes`) and re-tests `predicate` on each
    /// change, so a match is observed as soon as the loop publishes a
    /// structurally-changed snapshot rather than on the next poll tick.
    /// If the current snapshot already matches, resolves immediately.
    ///
    /// [`DeckClientConfig::snapshot_poll_interval`] is retained as a
    /// debounce ceiling, not a poll cadence: it bounds the re-test
    /// latency even if a publish edge is missed (the loop also
    /// republishes every Tick, so the ceiling is belt-and-suspenders).
    ///
    /// **Note on wedge risk:** if no snapshot ever matches the
    /// predicate, this future never resolves. Use
    /// [`Self::watch_timeout`] for bounded waits.
    pub async fn watch<F>(&self, mut predicate: F) -> MeshOsSnapshot
    where
        F: FnMut(&MeshOsSnapshot) -> bool,
    {
        // Check the current snapshot first — many "wait for
        // state X" calls land on a state that's already true.
        let snap = self.snapshot_reader.read();
        if predicate(&snap) {
            return snap;
        }
        let ceiling = self
            .config
            .snapshot_poll_interval
            .max(Duration::from_millis(1));
        // Subscribe ONCE and reuse the receiver across iterations: a
        // structural-change generation bumped between the predicate
        // re-test and the next `changed()` await is still observed (the
        // receiver tracks its seen generation), so this is
        // missed-wakeup-safe and the ceiling is a true backstop — even
        // when `snapshot_poll_interval` is set long for idle-quiet.
        let mut change_rx = self.snapshot_reader.subscribe_changes();
        loop {
            tokio::select! {
                biased;
                _ = change_rx.changed() => {}
                _ = tokio::time::sleep(ceiling) => {}
            }
            let snap = self.snapshot_reader.read();
            if predicate(&snap) {
                return snap;
            }
        }
    }

    /// Like [`Self::watch`] but with a bounded wait. Returns
    /// `Ok(snapshot)` on first match, `Err(DeckError)` with
    /// kind `watch_timeout` when `timeout` elapses without a
    /// match.
    pub async fn watch_timeout<F>(
        &self,
        predicate: F,
        timeout: Duration,
    ) -> Result<MeshOsSnapshot, DeckError>
    where
        F: FnMut(&MeshOsSnapshot) -> bool,
    {
        tokio::time::timeout(timeout, self.watch(predicate))
            .await
            .map_err(|_| {
                DeckError::new(
                    "watch_timeout",
                    format!(
                        "no snapshot matched the predicate within {} ms",
                        timeout.as_millis()
                    ),
                )
            })
    }

    fn next_commit_id(&self) -> u64 {
        // Start at 1 so a `0` commit id is recognizable as
        // "unset" downstream.
        self.commit_seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    async fn publish_admin(
        &self,
        event: AdminEvent,
        kind: &'static str,
    ) -> Result<ChainCommit, AdminError> {
        // When an operator registry is installed, sign the
        // admin event and route through SignedAdminCommit so
        // the substrate verifier sees the operator's signature.
        // Otherwise (in-process tests, dev mode) fall back to
        // the unsigned admin path. Per the plan's locked
        // decision #2 every Deck commit is signed in
        // production deployments — the substrate verifier is
        // the source of truth.
        let wire_event = if self.operator_registry.is_some() {
            let issued_at_ms = super::meshos::now_ms_since_unix_epoch();
            let signature = self.identity.sign_admin_event(&event, issued_at_ms);
            MeshOsEvent::SignedAdminCommit {
                event,
                signature,
                issued_at_ms,
            }
        } else {
            MeshOsEvent::AdminEvent(event)
        };
        self.handle
            .publish(wire_event)
            .await
            .map_err(AdminError::from)?;
        Ok(ChainCommit {
            commit_id: self.next_commit_id(),
            operator_id: self.identity.operator_id,
            event_kind: kind,
            committed_at: SystemTime::now(),
        })
    }

    async fn publish_signed_ice(
        &self,
        proposal: IceActionProposal,
        signatures: Vec<OperatorSignature>,
        issued_at_ms: u64,
        blast_hash: super::meshos::BlastRadiusHash,
        kind: &'static str,
    ) -> Result<ChainCommit, IceError> {
        self.handle
            .publish(MeshOsEvent::SignedIceCommit {
                proposal,
                signatures,
                issued_at_ms,
                blast_hash,
            })
            .await
            .map_err(IceError::from)?;
        Ok(ChainCommit {
            commit_id: self.next_commit_id(),
            operator_id: self.identity.operator_id,
            event_kind: kind,
            committed_at: SystemTime::now(),
        })
    }
}

/// Typed admin-command surface. Constructed via
/// [`DeckClient::admin`]; every method maps to one
/// [`super::meshos::AdminEvent`] variant.
///
/// Phase 1 publishes events onto the loop's event stream
/// directly (matching the substrate's current admin-event entry
/// path). When the substrate adds a signed admin chain, this
/// surface gains a signing step before the publish — the
/// per-method type signatures don't change.
pub struct AdminCommands<'a> {
    client: &'a DeckClient,
}

impl AdminCommands<'_> {
    /// Drain `node`'s workload over `drain_for`. Replicas
    /// migrate; daemons drain via
    /// [`crate::adapter::net::compute::DaemonControl::DrainStart`]
    /// once the loop sees the resulting `EnteringMaintenance`
    /// state. The duration is the wait-window from the loop's
    /// next tick; the substrate computes the absolute deadline
    /// at fold time so two replays of the same event stream
    /// produce identical `since` / `deadline` instants.
    pub async fn drain(
        &self,
        node: NodeId,
        drain_for: Duration,
    ) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::Drain { node, drain_for }, "drain")
            .await
    }

    /// Begin a maintenance window for `node`. `drain_for` is
    /// the drain-window duration; `None` defers to the cluster's
    /// configured default. The substrate-side fold computes the
    /// absolute deadline as `last_tick + drain_for`.
    pub async fn enter_maintenance(
        &self,
        node: NodeId,
        drain_for: Option<Duration>,
    ) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(
                AdminEvent::EnterMaintenance { node, drain_for },
                "enter_maintenance",
            )
            .await
    }

    /// End a maintenance window for `node`.
    pub async fn exit_maintenance(&self, node: NodeId) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::ExitMaintenance { node }, "exit_maintenance")
            .await
    }

    /// Mark `node` ineligible for new placements (existing
    /// workload stays).
    pub async fn cordon(&self, node: NodeId) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::Cordon { node }, "cordon")
            .await
    }

    /// Remove a prior cordon.
    pub async fn uncordon(&self, node: NodeId) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::Uncordon { node }, "uncordon")
            .await
    }

    /// Drop the listed replicas from `node`.
    pub async fn drop_replicas(
        &self,
        node: NodeId,
        chains: Vec<ChainId>,
    ) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::DropReplicas { node, chains }, "drop_replicas")
            .await
    }

    /// Force a placement recompute for `node`.
    pub async fn invalidate_placement(&self, node: NodeId) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(
                AdminEvent::InvalidatePlacement { node },
                "invalidate_placement",
            )
            .await
    }

    /// Force-restart every daemon on `node`.
    pub async fn restart_all_daemons(&self, node: NodeId) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(
                AdminEvent::RestartAllDaemons { node },
                "restart_all_daemons",
            )
            .await
    }

    /// Clear `node`'s local avoid list.
    pub async fn clear_avoid_list(&self, node: NodeId) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::ClearAvoidList { node }, "clear_avoid_list")
            .await
    }
}

/// Re-export the substrate-side signing types so the SDK
/// surface stays under `behavior::deck::*` for backwards
/// compatibility with the previous slice. The implementations
/// live alongside the rest of ICE in `behavior::meshos::ice`.
pub use super::meshos::{OperatorRegistry, OperatorSignature, VerifyError};

impl OperatorIdentity {
    /// Sign `proposal` with this operator's ed25519 key, stamped
    /// at `issued_at_ms` and bound to the simulator's
    /// pre-execution preview hash `blast_hash`. Thin
    /// SDK-ergonomic wrapper over the substrate's
    /// [`OperatorSignature::sign`] constructor.
    ///
    /// The substrate verifier rebuilds the same domain-tagged
    /// payload — including `blast_hash` — and rejects bundles
    /// whose hash is the
    /// [`super::meshos::SIMULATION_REQUIRED_SENTINEL`]
    /// (substrate enforcement of locked decision #4) or whose
    /// freshness window has expired. Coordinators collecting
    /// signatures from multiple operators must share the same
    /// `(issued_at_ms, blast_hash)` pair across the bundle —
    /// fetch both from the [`IceProposal`] handle.
    pub fn sign_proposal(
        &self,
        proposal: &IceActionProposal,
        issued_at_ms: u64,
        blast_hash: &super::meshos::BlastRadiusHash,
    ) -> OperatorSignature {
        OperatorSignature::sign(self.keypair(), proposal, issued_at_ms, blast_hash)
    }

    /// Sign an ordinary `AdminEvent` for the single-signature
    /// `SignedAdminCommit` path, stamped at `issued_at_ms`. The
    /// signature covers the substrate's
    /// [`super::meshos::admin_event_signing_payload(event, issued_at_ms)`]
    /// so the loop verifier and the SDK agree on the byte
    /// sequence.
    pub fn sign_admin_event(&self, event: &AdminEvent, issued_at_ms: u64) -> OperatorSignature {
        OperatorSignature::sign_admin(self.keypair(), event, issued_at_ms)
    }
}

/// SDK-side translation of substrate
/// [`VerifyError`] to the `<<deck-sdk-kind:KIND>>MSG` envelope
/// the SDK's [`IceProposal::commit`] returns.
fn verify_error_to_ice(err: VerifyError) -> IceError {
    let kind = err.kind();
    IceError::new(kind, err.to_string())
}

/// Break-glass operator surface. Constructed via
/// [`DeckClient::ice`]; each method returns an [`IceProposal`]
/// that must be `simulate()`d before `commit()` per the plan's
/// locked decision #4 (blast-radius simulation is mandatory
/// before any ICE commit).
pub struct IceCommands<'a> {
    client: &'a DeckClient,
}

impl<'a> IceCommands<'a> {
    /// Propose a cluster-wide freeze. The returned
    /// [`IceProposal`] must be simulated then committed.
    pub fn freeze_cluster(&self, ttl: Duration) -> IceProposal<'a> {
        IceProposal::new(self.client, IceActionProposal::FreezeCluster { ttl })
    }

    /// Propose flushing avoid-list entries under `scope`.
    /// See [`super::meshos::ice::IceActionProposal::FlushAvoidLists`]
    /// for the three scope semantics.
    pub fn flush_avoid_lists(&self, scope: super::meshos::AvoidScope) -> IceProposal<'a> {
        IceProposal::new(self.client, IceActionProposal::FlushAvoidLists { scope })
    }

    /// Propose force-evicting `victim` from `chain` bypassing
    /// the scheduler's rebalance cooldown. Only the chain's
    /// elected leader emits the resulting `RequestEviction`
    /// action; non-leader nodes fold the admin event silently.
    pub fn force_evict_replica(&self, chain: ChainId, victim: NodeId) -> IceProposal<'a> {
        IceProposal::new(
            self.client,
            IceActionProposal::ForceEvictReplica { chain, victim },
        )
    }

    /// Propose force-restarting `daemon` by resetting its
    /// supervisor backoff gate so reconcile fires `StartDaemon`
    /// on the next tick. No-op if the daemon is already in
    /// `Idle` backoff state.
    pub fn force_restart_daemon(&self, daemon: super::meshos::DaemonRef) -> IceProposal<'a> {
        IceProposal::new(
            self.client,
            IceActionProposal::ForceRestartDaemon { daemon },
        )
    }

    /// Propose force-placing `chain` on `target`, bypassing
    /// the placement scorer. Only the chain's elected leader
    /// emits the resulting `RequestPlacement` action with
    /// `target: Some(target)`; non-leader nodes fold silently.
    /// No-op if `target` is already a holder.
    pub fn force_cutover(&self, chain: ChainId, target: NodeId) -> IceProposal<'a> {
        IceProposal::new(
            self.client,
            IceActionProposal::ForceCutover { chain, target },
        )
    }

    /// Propose aborting an in-flight migration. The wire-form
    /// substrate plumbing records the commit on the audit
    /// ring; the dispatcher hookup that finds the running
    /// migration and stops it is future substrate work, so
    /// until that lands the proposal commits without
    /// actually halting the migration. The audit trail tracks
    /// the operator's intent regardless.
    pub fn kill_migration(&self, migration: super::meshos::MigrationId) -> IceProposal<'a> {
        IceProposal::new(self.client, IceActionProposal::KillMigration { migration })
    }

    /// Propose cancelling an in-effect cluster freeze.
    pub fn thaw_cluster(&self) -> IceProposal<'a> {
        IceProposal::new(self.client, IceActionProposal::ThawCluster)
    }
}

/// An ICE proposal — pre-simulation handle carrying the
/// underlying [`IceActionProposal`] plus the `issued_at_ms`
/// stamp pinned at construction.
///
/// Per the plan's locked decision #4 a [`Self::simulate`] call
/// must precede commit. The type-state split enforces this at
/// compile time: `IceProposal` does **not** expose `commit`;
/// only a successful `simulate()` returns a
/// [`SimulatedIceProposal`] whose `commit` is callable. A
/// caller that wants to commit must thread the proposal through
/// `simulate()` first — the type system rejects the alternative.
///
/// Both `IceProposal` and `SimulatedIceProposal` are `Send + Sync`
/// (no `Cell` / `RefCell` interior mutability), so callers can
/// move them across `tokio::spawn` boundaries freely.
pub struct IceProposal<'a> {
    client: &'a DeckClient,
    action: IceActionProposal,
    issued_at_ms: u64,
}

impl<'a> IceProposal<'a> {
    fn new(client: &'a DeckClient, action: IceActionProposal) -> Self {
        Self {
            client,
            action,
            issued_at_ms: super::meshos::now_ms_since_unix_epoch(),
        }
    }

    /// Borrow the underlying [`IceActionProposal`].
    pub fn action(&self) -> &IceActionProposal {
        &self.action
    }

    /// Milliseconds-since-`UNIX_EPOCH` stamp pinned at
    /// construction. Operators participating in the multi-sig
    /// flow read this from the [`SimulatedIceProposal`] (after
    /// simulating); the value is identical because `simulate()`
    /// preserves it.
    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// Pre-execution preview. Runs the substrate's pure
    /// simulator against the runtime's latest snapshot and
    /// returns a [`SimulatedIceProposal`] holding the result.
    /// The returned type is the only place
    /// [`SimulatedIceProposal::commit`] can be called — the
    /// type-state pattern enforces locked decision #4 at
    /// compile time.
    pub async fn simulate(self) -> Result<SimulatedIceProposal<'a>, IceError> {
        let snap = self.client.snapshot_reader.read();
        let blast = simulate_ice_proposal(&snap, &self.action);
        Ok(SimulatedIceProposal {
            client: self.client,
            action: self.action,
            issued_at_ms: self.issued_at_ms,
            blast,
        })
    }
}

/// An ICE proposal that has run [`IceProposal::simulate`] and
/// is ready for the operator signing + commit workflow.
/// Carries the simulator's [`BlastRadius`] output so the
/// Deck-the-binary UI can render the preview before the
/// operator approves; [`Self::commit`] hashes the
/// `BlastRadius` and signs the envelope with the operator key.
///
/// Every operator participating in the multi-signature
/// workflow must sign over the same
/// `(action, issued_at_ms, blast_hash)` triple — fetch via
/// [`Self::action`] / [`Self::issued_at_ms`] /
/// [`Self::blast_hash`].
pub struct SimulatedIceProposal<'a> {
    client: &'a DeckClient,
    action: IceActionProposal,
    issued_at_ms: u64,
    blast: BlastRadius,
}

impl<'a> SimulatedIceProposal<'a> {
    /// Borrow the simulator's pre-execution preview.
    pub fn blast_radius(&self) -> &BlastRadius {
        &self.blast
    }

    /// Borrow the underlying [`IceActionProposal`].
    pub fn action(&self) -> &IceActionProposal {
        &self.action
    }

    /// Milliseconds-since-`UNIX_EPOCH` stamp pinned at the
    /// original [`IceProposal`]'s construction; signatures
    /// must cover this exact value.
    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// Blake3 digest of the simulator's [`BlastRadius`].
    /// Signatures must cover this hash; the substrate verifier
    /// rebuilds the same payload and rejects bundles whose
    /// signatures don't bind to it.
    pub fn blast_hash(&self) -> super::meshos::BlastRadiusHash {
        super::meshos::blast_radius_hash(&self.blast)
    }

    /// Commit the proposal. Verifies
    /// `signatures.len() >= ice_signature_threshold` before
    /// publishing; returns
    /// `Err(IceError::insufficient_signatures)` otherwise.
    /// Substrate-side multi-operator-signature verification
    /// rebuilds the same domain-tagged signing envelope —
    /// including the blast-radius hash — and rejects any
    /// bundle whose signatures don't cover the exact
    /// `(action, issued_at_ms, blast_hash)` triple.
    pub async fn commit(self, signatures: &[OperatorSignature]) -> Result<ChainCommit, IceError> {
        let blast_hash = self.blast_hash();
        let threshold = self.client.config.ice_signature_threshold;
        if signatures.len() < threshold {
            return Err(IceError::new(
                "insufficient_signatures",
                format!(
                    "ICE commit requires {} operator signatures; got {}",
                    threshold,
                    signatures.len()
                ),
            ));
        }
        if let Some(registry) = self.client.operator_registry.as_ref() {
            // SDK-side gate: verify locally before publishing so
            // a malformed bundle fails fast with the right error
            // kind. The substrate-side verifier on the loop runs
            // the same check on every `SignedIceCommit` for the
            // belt-and-suspenders property: even an SDK that
            // skipped this gate gets rejected by the loop.
            let payload =
                ice_proposal_signing_payload(&self.action, self.issued_at_ms, &blast_hash);
            let mut unique_operators: std::collections::BTreeSet<u64> =
                std::collections::BTreeSet::new();
            for sig in signatures {
                registry
                    .verify(sig, &payload)
                    .map_err(verify_error_to_ice)?;
                unique_operators.insert(sig.operator_id);
            }
            // Mirror the substrate-side distinct-operator check
            // so duplicate signatures from a single operator can't
            // satisfy M-of-N at the SDK gate either.
            if unique_operators.len() < threshold {
                return Err(IceError::new(
                    "insufficient_signatures",
                    format!(
                        "ICE commit requires {} distinct operator signatures; got {} distinct",
                        threshold,
                        unique_operators.len()
                    ),
                ));
            }
            // Route via SignedIceCommit so the substrate verifier
            // sees the bundle. The inner AdminEvent folds only
            // after the loop's own verification passes. The
            // event_kind on the returned ChainCommit mirrors the
            // unsigned path so consumers see one stable
            // discriminator regardless of whether verification
            // was wired.
            let kind = self.action.kind();
            self.client
                .publish_signed_ice(
                    self.action,
                    signatures.to_vec(),
                    self.issued_at_ms,
                    blast_hash,
                    kind,
                )
                .await
        } else {
            // No registry: route via the unsigned admin path.
            // Useful for in-process tests where the SDK isn't
            // gating on identity at all. Freeze / thaw go
            // through the same path; the old `AdminCommands`
            // surface for those was removed because it
            // duplicated the ICE ceremony around the
            // simulate-before-commit gate (plan locked
            // decision #4).
            let kind = self.action.kind();
            let event = self.action.to_admin_event();
            self.client.publish_admin(event, kind).await
        }
    }
}

/// Audit-query builder reading the substrate's admin audit
/// ring. Constructed via [`DeckClient::audit`]; chain filter
/// methods, then call [`Self::collect`] to materialize the
/// matching entries.
///
/// # Scope
///
/// Reads the in-memory admin audit ring exported on every
/// [`super::meshos::MeshOsSnapshot`]. The ring carries every
/// admin commit the loop observed — signed ICE bundles AND
/// unsigned admin events — bounded at
/// [`super::meshos::DEFAULT_MAX_ADMIN_AUDIT_RECORDS`]. The
/// unbounded historical replay path is the eventual admin
/// audit subchain (substrate gap); this in-memory ring is the
/// near-history surface Deck-the-binary renders against.
///
/// Per-method semantics:
///
/// - [`Self::recent`] — keep the last N entries (newest-first
///   in the result). When unspecified, returns every entry on
///   the ring.
/// - [`Self::by_operator`] — keep entries whose
///   [`super::meshos::AdminAuditRecord::operator_ids`] include
///   the given id.
/// - [`Self::between`] — keep entries whose
///   `committed_at_ms` falls inside `[start_ms, end_ms]`
///   (inclusive).
/// - [`Self::force_only`] — restrict the result to ICE-class
///   admin events (`AdminEvent::is_ice` returns `true`).
///   Drops ordinary admin commits (`drain`, `cordon`, …) from
///   the result.
pub struct AuditQuery<'a> {
    client: &'a DeckClient,
    limit: Option<usize>,
    operator_filter: Option<u64>,
    time_range: Option<(u64, u64)>,
    force_only: bool,
    since_seq: Option<u64>,
}

impl<'a> AuditQuery<'a> {
    fn new(client: &'a DeckClient) -> Self {
        Self {
            client,
            limit: None,
            operator_filter: None,
            time_range: None,
            force_only: false,
            since_seq: None,
        }
    }

    /// Cap the result at the most-recent `limit` entries.
    /// Returned order is newest-first.
    ///
    /// `limit = 0` returns an empty result by design — useful
    /// in higher-level builder flows that compute the cap from
    /// runtime config (operator typed 0, no records wanted).
    /// Callers that want "as many as the ring holds" should
    /// omit `recent()` entirely; the builder's default returns
    /// every entry.
    pub fn recent(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Keep only entries that carry `op_id` in their
    /// `operator_ids` list. Records where one of several
    /// operators in a multi-op bundle matches still surface.
    pub fn by_operator(mut self, op_id: u64) -> Self {
        self.operator_filter = Some(op_id);
        self
    }

    /// Keep only entries whose `committed_at_ms` falls inside
    /// `[start_ms, end_ms]` (inclusive on both ends).
    /// Milliseconds since `UNIX_EPOCH`.
    pub fn between(mut self, start_ms: u64, end_ms: u64) -> Self {
        self.time_range = Some((start_ms, end_ms));
        self
    }

    /// Restrict the result to ICE force operations. The audit
    /// ring now interleaves ordinary signed-admin commits with
    /// ICE bundles, so this filter actively drops non-ICE
    /// records via [`super::meshos::AdminEvent::is_ice`].
    pub fn force_only(mut self) -> Self {
        self.force_only = true;
        self
    }

    /// Restrict the result to records with `seq > since_seq`.
    /// Pagination primitive: consumers persist the last-seen
    /// `seq` and resume from there across restarts. For
    /// [`Self::stream`] the value seeds the stream's
    /// watermark — the stream tails from `since_seq + 1`
    /// onward rather than from "first new record after now".
    ///
    /// `since(0)` means "from the beginning of what's still in
    /// the ring" — equivalent to omitting `since()` entirely
    /// in terms of what records are returned. To tail only
    /// records that arrive after subscribe time, pair this
    /// with [`DeckClient::audit_head_seq`]: read the head
    /// once, then `since(head)`. The same convention applies
    /// to [`LogFilter::since`] and
    /// [`DeckClient::subscribe_failures`].
    pub fn since(mut self, since_seq: u64) -> Self {
        self.since_seq = Some(since_seq);
        self
    }

    /// Materialize matching entries. Reads the runtime's
    /// latest snapshot, applies the configured filters, and
    /// returns the matching entries newest-first. Cheap — one
    /// snapshot read + a single iterator pass.
    pub fn collect(self) -> Vec<super::meshos::AdminAuditRecord> {
        let snap = self.client.snapshot_reader.read();
        let mut matched: Vec<super::meshos::AdminAuditRecord> = snap
            .admin_audit
            .iter()
            .filter(|r| {
                if let Some(since) = self.since_seq {
                    if r.seq <= since {
                        return false;
                    }
                }
                if let Some(op_id) = self.operator_filter {
                    if !r.operator_ids.contains(&op_id) {
                        return false;
                    }
                }
                if let Some((start, end)) = self.time_range {
                    if r.committed_at_ms < start || r.committed_at_ms > end {
                        return false;
                    }
                }
                if self.force_only && !r.event.is_ice() {
                    return false;
                }
                true
            })
            .cloned()
            .collect();
        // Ring order is oldest-first; the natural operator UI
        // shape is newest-first.
        matched.reverse();
        if let Some(limit) = self.limit {
            matched.truncate(limit);
        }
        matched
    }

    /// Tail mode: convert the query into an async stream that
    /// yields each matching audit record as it arrives on the
    /// substrate's ring. Polls the snapshot reader at
    /// `DeckClientConfig::snapshot_poll_interval`. The
    /// `recent(limit)` filter is ignored in tail mode — the
    /// stream emits continuously, not a bounded batch.
    ///
    /// Uses [`super::meshos::AdminAuditRecord::seq`] to dedup
    /// across polls so two commits in the same millisecond
    /// never collapse.
    pub fn stream(self) -> AuditStream {
        AuditStream::new(
            self.client.snapshot_reader.clone(),
            self.client.config.snapshot_poll_interval,
            AuditFilter {
                operator: self.operator_filter,
                time_range: self.time_range,
                force_only: self.force_only,
            },
            self.since_seq.unwrap_or(0),
        )
    }
}

/// Compiled audit filter the [`AuditStream`] re-applies on
/// every poll. Internal — exposed as `AuditQuery`'s builder
/// shape, not directly constructible by SDK consumers.
#[derive(Clone, Debug)]
struct AuditFilter {
    operator: Option<u64>,
    time_range: Option<(u64, u64)>,
    force_only: bool,
}

impl AuditFilter {
    fn matches(&self, record: &super::meshos::AdminAuditRecord) -> bool {
        if let Some(op_id) = self.operator {
            if !record.operator_ids.contains(&op_id) {
                return false;
            }
        }
        if let Some((start, end)) = self.time_range {
            if record.committed_at_ms < start || record.committed_at_ms > end {
                return false;
            }
        }
        if self.force_only && !record.event.is_ice() {
            return false;
        }
        true
    }
}

/// Audit-tail stream. Emits each matching
/// [`super::meshos::AdminAuditRecord`] as it lands on the
/// substrate's ring. Built via [`AuditQuery::stream`].
///
/// Dedup uses the per-runtime monotonic
/// [`super::meshos::AdminAuditRecord::seq`] field — the
/// stream tracks the highest seq it's emitted, and on each
/// poll yields records strictly above that watermark.
pub struct AuditStream {
    reader: super::meshos::MeshOsSnapshotReader,
    interval: Interval,
    filter: AuditFilter,
    last_seq: u64,
    /// Queue of records pending emission. Populated on a poll
    /// when multiple matching records arrived since the last
    /// tick; drained one-per-`poll_next` so the consumer sees
    /// each commit individually.
    queued: std::collections::VecDeque<super::meshos::AdminAuditRecord>,
}

impl AuditStream {
    fn new(
        reader: super::meshos::MeshOsSnapshotReader,
        poll_interval: Duration,
        filter: AuditFilter,
        initial_seq_watermark: u64,
    ) -> Self {
        let poll_interval = poll_interval.max(Duration::from_millis(1));
        Self {
            reader,
            interval: interval(poll_interval),
            filter,
            last_seq: initial_seq_watermark,
            queued: std::collections::VecDeque::new(),
        }
    }
}

/// Re-arm the task waker after an `Interval` Ready tick that produced
/// no record, returning the `Poll::Pending` the poll should yield.
///
/// tokio's [`Interval::poll_tick`] only registers a waker on *its own*
/// `Pending` path — after a consumed Ready tick that yields nothing it
/// leaves no waker behind, so a bare `stream.next().await` would park
/// forever. Every snapshot-polling deck stream that drains a Ready tick
/// without producing must call this; centralizing it keeps a new stream
/// type from silently reintroducing the park-forever bug.
#[inline]
fn rearm_after_empty_tick<T>(cx: &Context<'_>) -> Poll<Option<T>> {
    cx.waker().wake_by_ref();
    Poll::Pending
}

impl Stream for AuditStream {
    type Item = Result<super::meshos::AdminAuditRecord, DeckError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Drain any queued records first so the consumer sees
        // every record from a multi-record poll individually.
        if let Some(record) = self.queued.pop_front() {
            return Poll::Ready(Some(Ok(record)));
        }
        // Wait for the next poll tick.
        match self.interval.poll_tick(cx) {
            Poll::Ready(_) => {
                let snap = self.reader.read();
                let last_seq = self.last_seq;
                // Ring order is oldest-first; iterate forward.
                let mut max_seq = last_seq;
                for record in snap.admin_audit.iter().cloned() {
                    if record.seq <= last_seq {
                        continue;
                    }
                    if record.seq > max_seq {
                        max_seq = record.seq;
                    }
                    if self.filter.matches(&record) {
                        self.queued.push_back(record);
                    }
                }
                self.last_seq = max_seq;
                if let Some(record) = self.queued.pop_front() {
                    Poll::Ready(Some(Ok(record)))
                } else {
                    rearm_after_empty_tick(cx)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Log-stream filter. Default is "everything"; chain the
/// builder methods to narrow.
#[derive(Clone, Debug, Default)]
pub struct LogFilter {
    /// Minimum severity. Records below this are dropped.
    /// `None` matches every level.
    pub min_level: Option<super::meshos::LogLevel>,
    /// Restrict to a specific daemon. `None` matches every
    /// daemon (and substrate-level lines with `daemon_id =
    /// None`).
    pub daemon_id: Option<u64>,
    /// Restrict to a specific node. `None` matches every node.
    /// Future-relevant when remote log lines arrive via the
    /// per-daemon RedEX-tail integration.
    pub node_id: Option<NodeId>,
    /// Initial seq watermark — the stream tails from
    /// `since_seq + 1` onward rather than from "first new
    /// record after subscribe-time." Pagination primitive:
    /// consumers persist the last-seen seq and resume.
    pub since_seq: Option<u64>,
}

impl LogFilter {
    /// Empty filter — matches every record on the ring.
    pub fn new() -> Self {
        Self::default()
    }

    /// Restrict to records at or above `level`.
    pub fn min_level(mut self, level: super::meshos::LogLevel) -> Self {
        self.min_level = Some(level);
        self
    }

    /// Restrict to records originating from `daemon_id`.
    pub fn with_daemon(mut self, daemon_id: u64) -> Self {
        self.daemon_id = Some(daemon_id);
        self
    }

    /// Restrict to records originating from `node_id`.
    pub fn with_node(mut self, node_id: NodeId) -> Self {
        self.node_id = Some(node_id);
        self
    }

    /// Seed the stream's watermark to `since_seq`. The first
    /// record yielded has `seq > since_seq`. `since(0)` means
    /// "from the beginning of what's still in the ring" —
    /// to tail only post-subscribe records pair with
    /// [`DeckClient::log_head_seq`]: read the head once,
    /// then `since(head)`. Same convention as
    /// [`AuditQuery::since`].
    pub fn since(mut self, since_seq: u64) -> Self {
        self.since_seq = Some(since_seq);
        self
    }

    fn matches(&self, record: &super::meshos::LogRecord) -> bool {
        if let Some(min) = self.min_level {
            if record.level < min {
                return false;
            }
        }
        if let Some(id) = self.daemon_id {
            if record.daemon_id != Some(id) {
                return false;
            }
        }
        if let Some(node) = self.node_id {
            if record.node_id != Some(node) {
                return false;
            }
        }
        true
    }
}

/// Log-tail stream returned by [`DeckClient::subscribe_logs`].
/// Yields each matching [`super::meshos::LogRecord`] once.
/// Dedups across snapshot polls via the per-runtime monotonic
/// `LogRecord::seq` (same pattern as [`AuditStream`]).
pub struct LogStream {
    reader: super::meshos::MeshOsSnapshotReader,
    interval: Interval,
    filter: LogFilter,
    last_seq: u64,
    queued: std::collections::VecDeque<super::meshos::LogRecord>,
}

impl LogStream {
    fn new(
        reader: super::meshos::MeshOsSnapshotReader,
        poll_interval: Duration,
        filter: LogFilter,
    ) -> Self {
        let poll_interval = poll_interval.max(Duration::from_millis(1));
        let last_seq = filter.since_seq.unwrap_or(0);
        Self {
            reader,
            interval: interval(poll_interval),
            filter,
            last_seq,
            queued: std::collections::VecDeque::new(),
        }
    }
}

impl Stream for LogStream {
    type Item = Result<super::meshos::LogRecord, DeckError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(record) = self.queued.pop_front() {
            return Poll::Ready(Some(Ok(record)));
        }
        match self.interval.poll_tick(cx) {
            Poll::Ready(_) => {
                let snap = self.reader.read();
                let last_seq = self.last_seq;
                let mut max_seq = last_seq;
                for record in snap.log_ring.iter().cloned() {
                    if record.seq <= last_seq {
                        continue;
                    }
                    if record.seq > max_seq {
                        max_seq = record.seq;
                    }
                    if self.filter.matches(&record) {
                        self.queued.push_back(record);
                    }
                }
                self.last_seq = max_seq;
                if let Some(record) = self.queued.pop_front() {
                    Poll::Ready(Some(Ok(record)))
                } else {
                    rearm_after_empty_tick(cx)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Failure-tail stream returned by
/// [`DeckClient::subscribe_failures`]. Yields each new
/// [`super::meshos::FailureRecord`] the executor records.
/// Dedups across snapshot polls via the per-runtime
/// monotonic `FailureRecord::seq` (same pattern as
/// [`AuditStream`] / [`LogStream`]).
///
/// Chain-replay-derived failure records carry `seq = 0`;
/// they're naturally skipped because the watermark logic
/// is `seq > last_seq` and the initial watermark defaults
/// to 0.
pub struct FailureStream {
    reader: super::meshos::MeshOsSnapshotReader,
    interval: Interval,
    last_seq: u64,
    queued: std::collections::VecDeque<super::meshos::FailureRecord>,
}

impl FailureStream {
    fn new(
        reader: super::meshos::MeshOsSnapshotReader,
        poll_interval: Duration,
        initial_seq_watermark: u64,
    ) -> Self {
        let poll_interval = poll_interval.max(Duration::from_millis(1));
        Self {
            reader,
            interval: interval(poll_interval),
            last_seq: initial_seq_watermark,
            queued: std::collections::VecDeque::new(),
        }
    }
}

impl Stream for FailureStream {
    type Item = Result<super::meshos::FailureRecord, DeckError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(record) = self.queued.pop_front() {
            return Poll::Ready(Some(Ok(record)));
        }
        match self.interval.poll_tick(cx) {
            Poll::Ready(_) => {
                let snap = self.reader.read();
                let last_seq = self.last_seq;
                let mut max_seq = last_seq;
                for record in snap.recent_failures.iter().cloned() {
                    if record.seq <= last_seq {
                        continue;
                    }
                    if record.seq > max_seq {
                        max_seq = record.seq;
                    }
                    self.queued.push_back(record);
                }
                self.last_seq = max_seq;
                if let Some(record) = self.queued.pop_front() {
                    Poll::Ready(Some(Ok(record)))
                } else {
                    rearm_after_empty_tick(cx)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Shared, persistent snapshot change-generation receiver for the deck
/// `Stream` impls. Held in an async `Mutex` so the `'static` futures the
/// streams store in `pending` can re-borrow the SAME receiver each fire.
/// `watch::Receiver` tracks its seen generation, so re-arming after a
/// fire never misses a generation bumped in between — missed-wakeup-safe
/// even with a long ceiling, unlike a fresh subscription per arm (E-10).
type SharedSnapshotChangeRx = Arc<tokio::sync::Mutex<tokio::sync::watch::Receiver<u64>>>;

/// Build the `'static` "next structural change" future a deck `Stream`
/// stores in `pending`. Locks the shared receiver (uncontended — one
/// per stream, one in-flight future at a time) and awaits the next
/// change-generation bump.
fn next_snapshot_change(
    rx: SharedSnapshotChangeRx,
) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        let mut guard = rx.lock().await;
        // Err only if the loop's sender dropped (runtime gone) — then
        // the stream falls back to its ceiling.
        let _ = guard.changed().await;
    })
}

/// Stream over the runtime's snapshot reader. Event-driven (E-9/E-10):
/// wakes on the loop's structural change-generation bump or the ceiling
/// tick, re-reads, and emits. The configured cadence is retained as a
/// debounce ceiling (and the first poll emits immediately so a consumer
/// sees the initial state). Phase 1 emits on every wake (consumers
/// de-dupe if they care); the substrate's tail-with-replay path replaces
/// this with a chain-driven stream when it lands.
pub struct SnapshotStream {
    reader: MeshOsSnapshotReader,
    /// Debounce-ceiling timer — bounds latency if a publish edge is
    /// missed and fires immediately on the first poll.
    ceiling: Interval,
    /// Persistent change-generation receiver shared with `pending`'s
    /// futures, so re-arming after a fire never misses an intervening
    /// bump (missed-wakeup-safe).
    change_rx: SharedSnapshotChangeRx,
    /// In-flight "next structural change" future, re-armed each time it
    /// fires. The boxed future is `Send` but `!Sync`; the `Mutex`
    /// restores `Sync` (required by the pyo3/napi `#[pyclass]` wrappers)
    /// without an async lock — `poll_next` holds it only across the sync
    /// poll, never across an await.
    pending: parking_lot::Mutex<Pin<Box<dyn std::future::Future<Output = ()> + Send>>>,
}

impl SnapshotStream {
    fn new(reader: MeshOsSnapshotReader, poll_interval: Duration) -> Self {
        // Floor the interval so a zero-duration config doesn't
        // hot-spin the executor.
        let poll_interval = poll_interval.max(Duration::from_millis(1));
        let change_rx: SharedSnapshotChangeRx =
            Arc::new(tokio::sync::Mutex::new(reader.subscribe_changes()));
        let pending = parking_lot::Mutex::new(next_snapshot_change(change_rx.clone()));
        Self {
            reader,
            ceiling: interval(poll_interval),
            change_rx,
            pending,
        }
    }
}

impl Stream for SnapshotStream {
    type Item = Result<MeshOsSnapshot, DeckError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // `Self: Unpin` (all fields are), so project to `&mut Self` once
        // and borrow disjoint fields freely.
        let this = self.get_mut();
        // Poll the publish signal; re-arm for the next publish if it
        // fired. Then poll the ceiling so its waker stays registered
        // even when the change arm woke us. The ceiling's first tick is
        // immediate, so the initial poll emits current state.
        let changed = {
            let mut pending = this.pending.lock();
            let ready = pending.as_mut().poll(cx).is_ready();
            if ready {
                *pending = next_snapshot_change(this.change_rx.clone());
            }
            ready
        };
        let ticked = this.ceiling.poll_tick(cx).is_ready();
        if changed || ticked {
            Poll::Ready(Some(Ok(this.reader.read())))
        } else {
            Poll::Pending
        }
    }
}

/// Dedup'd [`StatusSummary`] stream. Returned by
/// [`DeckClient::status_summary_stream`]. Polls the snapshot
/// reader at the client's configured cadence, builds a fresh
/// summary, and yields it only when the summary differs from
/// the last emitted one (`PartialEq` dedup). The first poll
/// always emits — operators rendering a dashboard see the
/// initial state immediately, then change-driven updates from
/// there.
pub struct StatusSummaryStream {
    reader: super::meshos::MeshOsSnapshotReader,
    /// Debounce-ceiling timer (also fires immediately on first poll).
    ceiling: Interval,
    /// Persistent change-generation receiver shared with `pending`'s
    /// futures (missed-wakeup-safe re-arm; see [`SnapshotStream`]).
    change_rx: SharedSnapshotChangeRx,
    /// In-flight "next structural change" future, re-armed each time it
    /// fires. `Mutex` restores `Sync` over the `Send`-but-`!Sync` boxed
    /// future (the pyo3/napi `#[pyclass]` wrappers require it); the
    /// lock is held only across the sync poll, never across an await.
    pending: parking_lot::Mutex<Pin<Box<dyn std::future::Future<Output = ()> + Send>>>,
    last_emitted: Option<StatusSummary>,
}

impl StatusSummaryStream {
    fn new(reader: super::meshos::MeshOsSnapshotReader, poll_interval: Duration) -> Self {
        let poll_interval = poll_interval.max(Duration::from_millis(1));
        let change_rx: SharedSnapshotChangeRx =
            Arc::new(tokio::sync::Mutex::new(reader.subscribe_changes()));
        let pending = parking_lot::Mutex::new(next_snapshot_change(change_rx.clone()));
        Self {
            reader,
            ceiling: interval(poll_interval),
            change_rx,
            pending,
            last_emitted: None,
        }
    }
}

impl Stream for StatusSummaryStream {
    type Item = Result<StatusSummary, DeckError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        // Event-driven (E-9): wake on each publish or the ceiling tick,
        // build a fresh summary, and emit only when it differs from the
        // last (PartialEq dedup). Loop on a dedup-suppressed wake so the
        // re-armed publish future is re-polled (registering its waker)
        // before we park — otherwise a suppressed edge would drop the
        // event-driven wake and fall back to the ceiling.
        loop {
            let changed = {
                let mut pending = this.pending.lock();
                let ready = pending.as_mut().poll(cx).is_ready();
                if ready {
                    *pending = next_snapshot_change(this.change_rx.clone());
                }
                ready
            };
            let ticked = this.ceiling.poll_tick(cx).is_ready();
            if !changed && !ticked {
                return Poll::Pending;
            }
            let summary = build_status_summary(&this.reader.read());
            let should_emit = match &this.last_emitted {
                None => true,
                Some(prev) => prev != &summary,
            };
            if should_emit {
                this.last_emitted = Some(summary.clone());
                return Poll::Ready(Some(Ok(summary)));
            }
            // Unchanged — loop to re-poll both wake sources.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::meshos::{
        LoggingDispatcher, MaintenanceTransition, MeshOsAction, MeshOsConfig,
    };

    fn fast_config() -> MeshOsConfig {
        MeshOsConfig::default()
            .with_this_node(42)
            .with_tick_interval(Duration::from_millis(10))
            .with_event_queue_capacity(64)
            .with_action_queue_capacity(64)
    }

    #[tokio::test]
    async fn operator_identity_id_matches_keypair_origin_hash() {
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();
        let identity = OperatorIdentity::from_keypair(kp);
        assert_eq!(identity.operator_id(), origin);
    }

    #[tokio::test]
    async fn deck_subnet_and_gateway_accessors_default_to_empty_without_mesh() {
        // Pin the "no mesh installed" contract — the new
        // subnet/gateway/channel accessors must surface
        // sensible empties rather than panicking. CliContext
        // currently wires DeckClient without a MeshNode; this
        // is the path operator tooling sees today.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        assert_eq!(deck.local_subnet(), None);
        assert!(deck.known_subnets().is_empty());
        assert!(deck.gateway_stats().is_none());
        assert!(deck.gateway_exports().is_empty());
        assert_eq!(deck.channel_visibility("any/name"), None);
        assert!(deck.channels().is_empty());
        assert_eq!(deck.channel_wire_hash("any/name"), None);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn deck_with_mesh_surfaces_local_subnet_and_gateway_stats() {
        // Pin the "mesh installed" contract — `with_mesh` wires
        // the MeshNode reference through; the accessors then
        // return the substrate-level values. Uses
        // `set_channel_configs` to install a registry so the
        // gateway is built and `gateway_stats()` returns Some.
        use crate::adapter::net::{
            ChannelConfig, ChannelConfigRegistry, ChannelId, MeshNodeConfig, SubnetId, Visibility,
        };
        use std::net::SocketAddr;

        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);

        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut mesh_cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        mesh_cfg = mesh_cfg.with_subnet(SubnetId::new(&[3, 7]));
        let mut mesh = crate::adapter::net::MeshNode::new(EntityKeypair::generate(), mesh_cfg)
            .await
            .expect("MeshNode::new");
        let registry = Arc::new(ChannelConfigRegistry::new());
        let metrics_id = ChannelId::parse("internal/metrics").expect("channel id");
        registry.insert(
            ChannelConfig::new(metrics_id.clone()).with_visibility(Visibility::SubnetLocal),
        );
        mesh.set_channel_configs(registry);
        let mesh = Arc::new(mesh);

        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate())
            .with_mesh(mesh.clone());

        assert_eq!(deck.local_subnet(), Some(SubnetId::new(&[3, 7])));
        let stats = deck.gateway_stats().expect("gateway installed");
        assert_eq!(stats.local_subnet, SubnetId::new(&[3, 7]));
        assert_eq!(stats.forwarded, 0);
        assert_eq!(stats.dropped, 0);
        assert_eq!(stats.export_rules, 0);
        assert!(stats.peer_subnets.is_empty());

        // Channel-visibility lookup round-trips the configured
        // visibility for the one channel we registered.
        assert_eq!(
            deck.channel_visibility("internal/metrics"),
            Some(Visibility::SubnetLocal),
        );
        // The list surface mirrors the same channel.
        let channels = deck.channels();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].0, "internal/metrics");
        assert_eq!(channels[0].1, Visibility::SubnetLocal);
        // Wire-hash + canonical lookups resolve.
        assert_eq!(
            deck.channel_wire_hash("internal/metrics"),
            Some(metrics_id.wire_hash()),
        );
        assert_eq!(
            deck.channel_canonical_hash("internal/metrics"),
            Some(metrics_id.hash()),
        );

        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn deck_error_display_carries_kind_discriminator() {
        let err = DeckError::new("unknown_node", "node 99 is not in the cluster");
        let rendered = err.to_string();
        assert!(
            rendered.contains("<<deck-sdk-kind:unknown_node>>"),
            "expected discriminator envelope, got {rendered:?}",
        );
    }

    #[tokio::test]
    async fn admin_enter_maintenance_publishes_admin_event_and_returns_commit() {
        // Sanity that the SDK's admin path lands an AdminEvent on
        // the loop. We don't drive the executor here — the
        // assertion is "commit handle was returned + the loop's
        // fold saw the event," verified via the snapshot reader's
        // local_maintenance transition.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let identity = OperatorIdentity::generate();
        let deck = DeckClient::from_runtime(&runtime, identity.clone());
        let commit = deck
            .admin()
            .enter_maintenance(42, None)
            .await
            .expect("commit");
        assert_eq!(commit.operator_id(), identity.operator_id());
        assert_eq!(commit.event_kind(), "enter_maintenance");
        assert!(commit.commit_id() >= 1);

        // Give the loop a tick to fold + publish the post-state
        // snapshot. The maintenance enter triggers an
        // `EnteringMaintenance` discriminant; the snapshot reader
        // reflects it.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let snap = runtime.snapshot();
        assert!(
            !matches!(
                snap.local_maintenance,
                crate::adapter::net::behavior::meshos::MaintenanceStateSnapshot::Active
            ),
            "local maintenance should have transitioned out of Active, got {:?}",
            snap.local_maintenance,
        );

        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn admin_drop_replicas_publishes_with_supplied_chain_ids() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let commit = deck
            .admin()
            .drop_replicas(42, vec![1, 2, 3])
            .await
            .expect("commit");
        assert_eq!(commit.event_kind(), "drop_replicas");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn commit_ids_increment_monotonically_per_client() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let a = deck.admin().cordon(42).await.unwrap();
        let b = deck.admin().uncordon(42).await.unwrap();
        assert!(b.commit_id() > a.commit_id());
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn snapshot_stream_yields_a_snapshot_per_poll_interval() {
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(20),
                ..DeckClientConfig::default()
            },
        );

        let mut stream = deck.snapshots();
        // First tick lands immediately (tokio::time::interval
        // fires on first poll); collect two ticks and assert both
        // are Ok.
        let first = stream.next().await.expect("first").expect("ok");
        let second = stream.next().await.expect("second").expect("ok");
        // Same shape — both came from the same reader.
        assert_eq!(first.local_maintenance, second.local_maintenance);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn snapshot_stream_observes_admin_command_aftermath() {
        // The interesting end-to-end shape: issue an admin
        // command, then read a snapshot from the stream and
        // confirm the loop folded the event. Mirrors what
        // Deck-the-binary will see.
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            },
        );

        let _ = deck.admin().enter_maintenance(42, None).await.unwrap();

        // Skip a few stream frames so the loop's tick has folded
        // the admin event + published a fresh snapshot.
        let mut stream = deck.snapshots();
        let mut saw_transition = false;
        for _ in 0..20 {
            let snap = stream.next().await.expect("next").expect("ok");
            if !matches!(
                snap.local_maintenance,
                crate::adapter::net::behavior::meshos::MaintenanceStateSnapshot::Active
            ) {
                saw_transition = true;
                break;
            }
        }
        assert!(
            saw_transition,
            "stream should have surfaced a non-Active local_maintenance after enter_maintenance",
        );
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn change_signal_stays_quiet_on_idle_ticks_and_fires_on_structural_change() {
        // E-10 core guarantee. The loop publishes (and the snapshot's
        // time-projected fields advance) every tick, but the change
        // GENERATION must bump only on a genuine structural change — so
        // a consumer parked purely on the signal isn't woken by cosmetic
        // per-tick churn. fast_config ticks ~10ms, so several ticks pass
        // inside each sleep window below.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        let mut rx = deck.snapshot_reader.subscribe_changes();
        // Let the runtime settle (its initial structural publishes) then
        // clear the seen mark so we measure only what happens next.
        tokio::time::sleep(Duration::from_millis(120)).await;
        rx.borrow_and_update();

        // Many idle ticks elapse. age_ms etc. advance in the stored
        // snapshot every tick, but nothing structural changed — the
        // generation must not move.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !rx.has_changed().unwrap(),
            "change signal fired on idle ticks — per-tick time progression \
             must NOT count as a structural change",
        );

        // A real structural change (freeze commit: freeze_until None→Some)
        // must bump the generation promptly.
        let p = deck
            .ice()
            .freeze_cluster(Duration::from_secs(15))
            .simulate()
            .await
            .expect("simulate");
        let sig = deck
            .identity()
            .sign_proposal(p.action(), p.issued_at_ms(), &p.blast_hash());
        p.commit(&[sig]).await.expect("commit");

        tokio::time::timeout(Duration::from_secs(2), rx.changed())
            .await
            .expect("a structural change must fire the signal well inside the timeout")
            .expect("change sender alive");

        // And once the freeze is committed, the countdown ticking down
        // every tick must NOT keep bumping the generation.
        rx.borrow_and_update();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !rx.has_changed().unwrap(),
            "freeze countdown advancing must not bump the change generation",
        );

        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn admin_commit_after_runtime_shutdown_returns_loop_closed_error() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let _ = runtime.shutdown().await;
        // The runtime's `shutdown` future drains the loop and
        // drops the loop side of the publish channel. The SDK's
        // own handle is a clone; publishing on it returns
        // `LoopClosed` once the loop exits.
        let err = deck
            .admin()
            .cordon(42)
            .await
            .expect_err("publish after shutdown should fail");
        assert_eq!(err.kind, "loop_closed");
    }

    // Silence the unused-import warning for types we re-export but
    // don't construct directly in tests.
    #[allow(dead_code)]
    fn _ensure_action_types_are_in_scope() -> (MaintenanceTransition, MeshOsAction) {
        (
            MaintenanceTransition::EnteringMaintenance,
            MeshOsAction::CommitMaintenanceTransition {
                node: 0,
                target: MaintenanceTransition::EnteringMaintenance,
            },
        )
    }

    // Note: a "commit without simulate" test would not compile —
    // `IceProposal` does not expose `commit`; only the
    // `SimulatedIceProposal` returned from `IceProposal::simulate`
    // does. The type-state split enforces locked decision #4 at
    // compile time, so no runtime simulation-required gate exists
    // to test.

    #[tokio::test]
    async fn ice_proposal_commit_with_insufficient_signatures_fails() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        // Bump threshold above what we'll supply.
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(100),
                ice_signature_threshold: 2,
            },
        );
        let proposal = deck.ice().freeze_cluster(Duration::from_secs(10));
        let simulated = proposal.simulate().await.expect("simulate");
        let sig = deck.identity().sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let err = simulated
            .commit(&[sig])
            .await
            .expect_err("under-threshold commit should fail");
        assert_eq!(err.kind, "insufficient_signatures");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn ice_freeze_proposal_simulate_then_commit_lands_freeze_on_loop() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        let proposal = deck.ice().freeze_cluster(Duration::from_secs(30));
        let simulated = proposal.simulate().await.expect("simulate");
        // FreezeCluster sets the drain delay to the requested TTL.
        assert_eq!(
            simulated.blast_radius().estimated_drain_delay,
            Some(Duration::from_secs(30))
        );
        let sig = deck.identity().sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let commit = simulated.commit(&[sig]).await.expect("commit");
        assert_eq!(commit.event_kind(), "freeze_cluster");

        // Give the loop a tick + reconcile + publish to fold the
        // freeze through to the snapshot.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let snap = runtime.snapshot();
        assert!(
            snap.freeze_remaining_ms.is_some(),
            "freeze_remaining_ms should be set after committed freeze",
        );
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn ice_thaw_proposal_simulate_warns_no_op_when_unfrozen() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        let proposal = deck.ice().thaw_cluster();
        let simulated = proposal.simulate().await.expect("simulate");
        // Simulator on a non-frozen snapshot warns "no freeze to cancel."
        assert!(simulated.blast_radius().warnings.iter().any(|w| matches!(
            w,
            crate::adapter::net::behavior::meshos::BlastWarning::ThawHasNoFreezeToCancel
        )));
        let _ = runtime.shutdown().await;
    }

    /// Test helper: a non-sentinel blast-radius hash to use in
    /// tests that don't construct a real `BlastRadius` (they're
    /// exercising the signature plumbing, not the simulation
    /// gate).
    const TEST_BLAST_HASH: super::super::meshos::BlastRadiusHash =
        [1u8; super::super::meshos::BLAST_RADIUS_HASH_LEN];

    /// Static assertion: every public stream type returned by
    /// the SDK must be `Send` so callers can move them across
    /// `tokio::spawn` boundaries. A future internal-field swap
    /// to a `!Send` type silently breaks every downstream
    /// `spawn` consumer; pinning the property here keeps that
    /// regression out of CI. The ICE proposal type-state pair
    /// also pins `Send + Sync` for the same reason.
    fn _assert_proposal_send_sync_static_check() {
        fn _assert_send<T: Send>() {}
        fn _assert_send_sync<T: Send + Sync>() {}
        _assert_send_sync::<IceProposal<'static>>();
        _assert_send_sync::<SimulatedIceProposal<'static>>();
        _assert_send::<SnapshotStream>();
        _assert_send::<StatusSummaryStream>();
        _assert_send::<AuditStream>();
        _assert_send::<LogStream>();
        _assert_send::<FailureStream>();
    }

    #[tokio::test]
    async fn operator_signature_carries_issuing_operator_id() {
        let identity = OperatorIdentity::generate();
        let proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(60),
        };
        let sig = identity.sign_proposal(
            &proposal,
            super::super::meshos::now_ms_since_unix_epoch(),
            &TEST_BLAST_HASH,
        );
        assert_eq!(sig.operator_id, identity.operator_id());
        // ed25519 signatures are 64 bytes.
        assert_eq!(sig.signature.len(), 64);
    }

    #[tokio::test]
    async fn operator_registry_verifies_a_well_formed_signature() {
        let identity = OperatorIdentity::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(identity.keypair());

        let proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(60),
        };
        let ts = super::super::meshos::now_ms_since_unix_epoch();
        let sig = identity.sign_proposal(&proposal, ts, &TEST_BLAST_HASH);
        let payload = ice_proposal_signing_payload(&proposal, ts, &TEST_BLAST_HASH);
        registry.verify(&sig, &payload).expect("valid signature");
    }

    #[tokio::test]
    async fn operator_registry_rejects_unknown_operator() {
        let registry = OperatorRegistry::new();
        let identity = OperatorIdentity::generate();
        let proposal = IceActionProposal::ThawCluster;
        let ts = super::super::meshos::now_ms_since_unix_epoch();
        let sig = identity.sign_proposal(&proposal, ts, &TEST_BLAST_HASH);
        let payload = ice_proposal_signing_payload(&proposal, ts, &TEST_BLAST_HASH);
        let err = registry
            .verify(&sig, &payload)
            .expect_err("unregistered operator should not verify");
        assert_eq!(err.kind(), "not_authorized");
    }

    #[tokio::test]
    async fn operator_registry_rejects_tampered_signature_bytes() {
        let identity = OperatorIdentity::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(identity.keypair());

        let proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(10),
        };
        let ts = super::super::meshos::now_ms_since_unix_epoch();
        let mut sig = identity.sign_proposal(&proposal, ts, &TEST_BLAST_HASH);
        // Flip one byte in the signature.
        sig.signature[0] ^= 0x01;
        let payload = ice_proposal_signing_payload(&proposal, ts, &TEST_BLAST_HASH);
        let err = registry
            .verify(&sig, &payload)
            .expect_err("tampered signature should not verify");
        assert_eq!(err.kind(), "signature_invalid");
    }

    #[tokio::test]
    async fn operator_registry_rejects_signature_for_wrong_payload() {
        // A signature over `FreezeCluster { 10s }` should not
        // verify against the payload of a different proposal.
        // This is the contract the substrate verifier will rely
        // on to reject signature reuse across proposals.
        let identity = OperatorIdentity::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(identity.keypair());

        let signed_proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(10),
        };
        let other_proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(60),
        };
        let ts = super::super::meshos::now_ms_since_unix_epoch();
        let sig = identity.sign_proposal(&signed_proposal, ts, &TEST_BLAST_HASH);
        let payload = ice_proposal_signing_payload(&other_proposal, ts, &TEST_BLAST_HASH);
        let err = registry
            .verify(&sig, &payload)
            .expect_err("cross-proposal signature should not verify");
        assert_eq!(err.kind(), "signature_invalid");
    }

    #[tokio::test]
    async fn operator_registry_rejects_wrong_length_signature() {
        let identity = OperatorIdentity::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(identity.keypair());

        let proposal = IceActionProposal::ThawCluster;
        let sig = OperatorSignature {
            operator_id: identity.operator_id(),
            signature: vec![0; 32], // wrong length
        };
        let payload = ice_proposal_signing_payload(
            &proposal,
            super::super::meshos::now_ms_since_unix_epoch(),
            &TEST_BLAST_HASH,
        );
        let err = registry
            .verify(&sig, &payload)
            .expect_err("wrong-length signature should not verify");
        assert_eq!(err.kind(), "signature_invalid");
    }

    #[tokio::test]
    async fn ice_commit_with_registry_rejects_an_unverified_signature() {
        // Build a two-operator bundle where one signature is
        // tampered. The threshold is met (2 sigs supplied) but
        // verification rejects the bundle.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let op_a = OperatorIdentity::generate();
        let op_b = OperatorIdentity::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(op_a.keypair());
        registry.register(op_b.keypair());
        let deck = DeckClient::new(
            runtime.handle_clone(),
            runtime.snapshot_reader().clone(),
            op_a.clone(),
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(100),
                ice_signature_threshold: 2,
            },
        )
        .with_operator_registry(registry);

        let proposal = deck.ice().freeze_cluster(Duration::from_secs(15));
        let simulated = proposal.simulate().await.expect("simulate");
        let sig_a = op_a.sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let mut sig_b = op_b.sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        sig_b.signature[3] ^= 0xFF; // tamper

        let err = simulated
            .commit(&[sig_a, sig_b])
            .await
            .expect_err("commit with tampered sig should fail");
        assert_eq!(err.kind, "signature_invalid");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn ice_flush_avoid_lists_proposal_simulate_and_commit_round_trips() {
        use super::super::meshos::AvoidScope;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let proposal = deck.ice().flush_avoid_lists(AvoidScope::OnPeer { peer: 5 });
        let simulated = proposal.simulate().await.expect("simulate");
        // OnPeer scope flushes everywhere; without registered
        // peers in the snapshot the affected_nodes list is
        // empty but the warning still fires.
        assert!(simulated.blast_radius().warnings.iter().any(|w| matches!(
            w,
            crate::adapter::net::behavior::meshos::BlastWarning::AvoidFlushRecoversPeer { peer: 5 }
        )));
        // commit through the unsigned path (no registry installed).
        let sig = deck.identity().sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let commit = simulated.commit(&[sig]).await.expect("commit");
        assert_eq!(commit.event_kind(), "flush_avoid_lists");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn status_summary_stream_emits_initial_summary_immediately() {
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(10),
                ..DeckClientConfig::default()
            },
        );
        let mut stream = deck.status_summary_stream();
        let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("first timed out")
            .expect("first closed")
            .expect("first ok");
        // Steady-state idle cluster — every count is zero.
        assert!(first.freeze_remaining_ms.is_none());
        assert!(!first.local_maintenance_active);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn status_summary_stream_dedups_unchanged_summaries() {
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(10),
                ..DeckClientConfig::default()
            },
        );
        let mut stream = deck.status_summary_stream();
        // First emission lands.
        let _ = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("first")
            .expect("closed")
            .expect("ok");
        // No state change — the stream should park (dedup).
        let second = tokio::time::timeout(Duration::from_millis(80), stream.next()).await;
        assert!(
            second.is_err(),
            "stream should not re-emit unchanged summary"
        );
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn status_summary_stream_re_emits_on_freeze_state_change() {
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(10),
                ..DeckClientConfig::default()
            },
        );
        let mut stream = deck.status_summary_stream();
        let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("first")
            .expect("closed")
            .expect("ok");
        assert!(first.freeze_remaining_ms.is_none());

        // Freeze the cluster — the next polled summary will
        // differ (`freeze_remaining_ms` flips to `Some`), so
        // the stream re-emits + the audit ring depth bumps.
        let p = deck
            .ice()
            .freeze_cluster(Duration::from_secs(30))
            .simulate()
            .await
            .expect("simulate");
        let sig = deck
            .identity()
            .sign_proposal(p.action(), p.issued_at_ms(), &p.blast_hash());
        p.commit(&[sig]).await.expect("freeze");
        let after_freeze = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("after_freeze timed out")
            .expect("after_freeze closed")
            .expect("after_freeze ok");
        assert!(after_freeze.freeze_remaining_ms.is_some());
        assert!(after_freeze.admin_audit_ring_depth >= 1);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn status_summary_reflects_steady_state_idle_cluster() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let summary = deck.status_summary();
        assert_eq!(summary.peers, PeerCounts::default());
        assert_eq!(summary.daemons, DaemonCounts::default());
        assert_eq!(summary.replica_chains, 0);
        assert_eq!(summary.recently_emitted_count, 0);
        assert!(summary.freeze_remaining_ms.is_none());
        assert!(!summary.local_maintenance_active);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn status_summary_flags_freeze_after_freeze_cluster_commit() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let p = deck
            .ice()
            .freeze_cluster(Duration::from_secs(30))
            .simulate()
            .await
            .expect("simulate");
        let sig = deck
            .identity()
            .sign_proposal(p.action(), p.issued_at_ms(), &p.blast_hash());
        p.commit(&[sig]).await.expect("freeze");
        tokio::time::sleep(Duration::from_millis(60)).await;
        let summary = deck.status_summary();
        assert!(summary.freeze_remaining_ms.is_some());
        // Audit ring should have at least one entry now.
        assert!(summary.admin_audit_ring_depth >= 1);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn status_summary_flags_local_maintenance_after_enter_maintenance() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        // `fast_config` pins this_node = 42; target the same.
        deck.admin()
            .enter_maintenance(42, None)
            .await
            .expect("commit");
        tokio::time::sleep(Duration::from_millis(60)).await;
        let summary = deck.status_summary();
        assert!(
            summary.local_maintenance_active,
            "local_maintenance_active should flip on after enter_maintenance",
        );
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn subscribe_failures_yields_seeded_dispatcher_rejection() {
        use crate::adapter::net::behavior::meshos::DispatchError;
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        dispatcher.fail_next(DispatchError::drop("first"));
        let runtime = MeshOsRuntime::start(fast_config(), Arc::clone(&dispatcher));
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            },
        );
        let mut stream = deck.subscribe_failures(0);

        deck.admin().enter_maintenance(42, None).await.unwrap();

        let record = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("timed out")
            .expect("closed")
            .expect("ok");
        // The executor stamps strictly-positive seqs; chain-
        // replay-derived records carry seq=0.
        assert!(record.seq > 0);
        assert!(record.reason.contains("first"));
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn subscribe_failures_since_seq_drops_already_seen() {
        use crate::adapter::net::behavior::meshos::DispatchError;
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        dispatcher.fail_next(DispatchError::drop("first"));
        let runtime = MeshOsRuntime::start(fast_config(), Arc::clone(&dispatcher));
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            },
        );

        deck.admin().enter_maintenance(42, None).await.unwrap();
        // Wait for the first failure to land on the ring.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut seq_seen = 0u64;
        while std::time::Instant::now() < deadline {
            let all = deck.recent_failures();
            if let Some(r) = all.last() {
                seq_seen = r.seq;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(seq_seen > 0);

        // Subscribe with the seq already-seen as the
        // watermark; no new failures => stream parks.
        let mut stream = deck.subscribe_failures(seq_seen);
        let parked = tokio::time::timeout(Duration::from_millis(60), stream.next()).await;
        assert!(parked.is_err(), "no new failures means parked stream");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn recent_failures_surfaces_dispatcher_rejections() {
        use crate::adapter::net::behavior::meshos::DispatchError;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        dispatcher.fail_next(DispatchError::drop("synthetic rejection"));
        let runtime = MeshOsRuntime::start(fast_config(), Arc::clone(&dispatcher));
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        // Drive an admin event whose dispatched action will be
        // rejected — reconcile emits `CommitMaintenanceTransition`
        // for an empty-workload enter_maintenance.
        deck.admin()
            .enter_maintenance(42, None)
            .await
            .expect("commit");

        // Poll up to 2s for the failure to land on the ring.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut got: Vec<crate::adapter::net::behavior::meshos::FailureRecord> = Vec::new();
        while std::time::Instant::now() < deadline {
            got = deck.recent_failures();
            if !got.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !got.is_empty(),
            "recent_failures should reflect the seeded dispatcher rejection",
        );
        assert!(got[0].reason.contains("synthetic rejection"));
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn recent_failures_since_drops_records_at_or_below_cutoff() {
        use crate::adapter::net::behavior::meshos::DispatchError;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        dispatcher.fail_next(DispatchError::drop("first failure"));
        let runtime = MeshOsRuntime::start(fast_config(), Arc::clone(&dispatcher));
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        deck.admin()
            .enter_maintenance(42, None)
            .await
            .expect("commit");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut all: Vec<crate::adapter::net::behavior::meshos::FailureRecord> = Vec::new();
        while std::time::Instant::now() < deadline {
            all = deck.recent_failures();
            if !all.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!all.is_empty(), "seed failure should land");

        // Set the watermark to the existing record's ms — the
        // since filter uses `>` so the same record is dropped.
        let cutoff = all[0].recorded_at_ms;
        let after = deck.recent_failures_since(cutoff);
        assert!(
            after.iter().all(|r| r.recorded_at_ms > cutoff),
            "since filter should drop records at the cutoff",
        );
        // The seed record itself shouldn't appear (its ms ==
        // cutoff).
        assert!(after.iter().all(|r| r.reason != "first failure"));
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn per_field_accessors_match_full_snapshot_contents() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        let snap = deck.status();
        assert_eq!(deck.peers(), snap.peers);
        assert_eq!(deck.daemons(), snap.daemons);
        assert_eq!(deck.replicas(), snap.replicas);
        assert_eq!(deck.local_maintenance(), snap.local_maintenance);
        assert_eq!(deck.freeze_remaining_ms(), snap.freeze_remaining_ms);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn status_returns_freshest_snapshot_synchronously() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        // Default state — local maintenance is Active.
        let s = deck.status();
        assert!(matches!(
            s.local_maintenance,
            crate::adapter::net::behavior::meshos::MaintenanceStateSnapshot::Active
        ));

        // Issue a freeze; subsequent status() should see the
        // freeze once the loop folds.
        let p = deck
            .ice()
            .freeze_cluster(Duration::from_secs(20))
            .simulate()
            .await
            .expect("simulate");
        let sig = deck
            .identity()
            .sign_proposal(p.action(), p.issued_at_ms(), &p.blast_hash());
        p.commit(&[sig]).await.expect("commit");
        tokio::time::sleep(Duration::from_millis(60)).await;
        let s = deck.status();
        assert!(s.freeze_remaining_ms.is_some());
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn watch_resolves_immediately_when_predicate_already_true() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        // Default state matches "no freeze in effect" — should
        // resolve immediately.
        let snap = tokio::time::timeout(
            Duration::from_millis(50),
            deck.watch(|s| s.freeze_remaining_ms.is_none()),
        )
        .await
        .expect("watch should not block when predicate already holds");
        assert!(snap.freeze_remaining_ms.is_none());
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn watch_resolves_when_predicate_becomes_true_after_admin_commit() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(10),
                ..DeckClientConfig::default()
            },
        );

        // Spawn a watcher that waits for a freeze.
        let deck_handle = deck.snapshot_reader.clone();
        let watcher = {
            let identity = deck.identity().clone();
            let config = deck.config.clone();
            let handle = deck.handle.clone();
            // Build a sibling client that shares the same
            // snapshot reader — `DeckClient` isn't `Clone` and
            // a real consumer would `Arc::clone` the outer
            // handle, but spawning shows the watch is non-
            // blocking on the SDK level.
            let client = DeckClient::new(handle, deck_handle.clone(), identity, config);
            tokio::spawn(async move { client.watch(|s| s.freeze_remaining_ms.is_some()).await })
        };

        // Wait a beat so the watcher is in its sleep loop.
        tokio::time::sleep(Duration::from_millis(40)).await;
        let p = deck
            .ice()
            .freeze_cluster(Duration::from_secs(15))
            .simulate()
            .await
            .expect("simulate");
        let sig = deck
            .identity()
            .sign_proposal(p.action(), p.issued_at_ms(), &p.blast_hash());
        p.commit(&[sig]).await.expect("commit");

        let snap = tokio::time::timeout(Duration::from_secs(2), watcher)
            .await
            .expect("watcher should resolve")
            .expect("join");
        assert!(snap.freeze_remaining_ms.is_some());
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn watch_is_event_driven_resolving_far_under_the_poll_ceiling() {
        // E-9: arm a deliberately long 30s poll-interval ceiling. If the
        // watch still resolves in well under a second after the commit,
        // it can only have woken on the loop's snapshot-publish signal —
        // a regression to interval-polling would wait ~30s and trip the
        // inner 2s timeout.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_secs(30),
                ..DeckClientConfig::default()
            },
        );

        let watcher = {
            let client = DeckClient::new(
                deck.handle.clone(),
                deck.snapshot_reader.clone(),
                deck.identity().clone(),
                deck.config.clone(),
            );
            tokio::spawn(async move { client.watch(|s| s.freeze_remaining_ms.is_some()).await })
        };

        tokio::time::sleep(Duration::from_millis(40)).await;
        let started = std::time::Instant::now();
        let p = deck
            .ice()
            .freeze_cluster(Duration::from_secs(15))
            .simulate()
            .await
            .expect("simulate");
        let sig = deck
            .identity()
            .sign_proposal(p.action(), p.issued_at_ms(), &p.blast_hash());
        p.commit(&[sig]).await.expect("commit");

        let snap = tokio::time::timeout(Duration::from_secs(2), watcher)
            .await
            .expect("watch must resolve far inside the 30s ceiling")
            .expect("join");
        assert!(snap.freeze_remaining_ms.is_some());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "watch took {:?}, expected ≪ 30s ceiling — not event-driven",
            started.elapsed(),
        );
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn watch_timeout_returns_watch_timeout_error_when_predicate_never_holds() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(10),
                ..DeckClientConfig::default()
            },
        );

        let err = deck
            .watch_timeout(
                |s| s.freeze_remaining_ms.is_some(),
                Duration::from_millis(80),
            )
            .await
            .expect_err("predicate never holds, should time out");
        assert_eq!(err.kind, "watch_timeout");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_since_filter_drops_records_at_or_below_watermark() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        deck.admin().cordon(42).await.unwrap();
        deck.admin().uncordon(42).await.unwrap();
        deck.admin().invalidate_placement(42).await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;

        let all = deck.audit().collect();
        assert_eq!(all.len(), 3);
        // Pick the middle record's seq as the watermark; only
        // the newest record (seq strictly greater) should
        // surface.
        let middle_seq = all[1].seq;
        let after_middle = deck.audit().since(middle_seq).collect();
        assert_eq!(after_middle.len(), 1, "since should keep only seq > middle");
        assert!(after_middle[0].seq > middle_seq);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_stream_since_seeds_initial_watermark() {
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            },
        );

        // Land three commits before subscribing.
        deck.admin().cordon(42).await.unwrap();
        deck.admin().uncordon(42).await.unwrap();
        deck.admin().invalidate_placement(42).await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;

        let all = deck.audit().collect();
        assert_eq!(all.len(), 3);
        // Resume from the middle entry's seq. Stream should
        // yield only the newest entry (seq strictly above
        // middle) then park.
        let middle_seq = all[1].seq;
        let mut stream = deck.audit().since(middle_seq).stream();
        let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("timed out")
            .expect("closed")
            .expect("ok");
        assert!(next.seq > middle_seq);
        // No more records — stream parks.
        let parked = tokio::time::timeout(Duration::from_millis(40), stream.next()).await;
        assert!(
            parked.is_err(),
            "stream should park after watermark catches up"
        );
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn log_filter_since_seeds_stream_watermark() {
        use crate::adapter::net::behavior::meshos::{LogLine, MeshOsEvent};
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            },
        );

        // Publish three lines and snapshot the middle seq.
        for i in 0..3 {
            runtime
                .handle()
                .publish(MeshOsEvent::LogLine(LogLine::info(
                    None,
                    format!("msg {i}"),
                )))
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(80)).await;

        let snap = runtime.snapshot();
        assert_eq!(snap.log_ring.len(), 3);
        let middle_seq = snap.log_ring[1].seq;

        // Subscribe with since() seeded to the middle seq.
        // Stream should yield only the third line (seq > middle).
        let mut stream = deck.subscribe_logs(LogFilter::new().since(middle_seq));
        let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("timed out")
            .expect("closed")
            .expect("ok");
        assert!(next.seq > middle_seq);
        assert_eq!(next.message, "msg 2");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn subscribe_logs_yields_published_log_lines_in_seq_order() {
        use crate::adapter::net::behavior::meshos::{LogLevel, LogLine, MeshOsEvent};
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            },
        );

        let mut stream = deck.subscribe_logs(LogFilter::new());
        for (i, level) in [LogLevel::Info, LogLevel::Warn, LogLevel::Error]
            .into_iter()
            .enumerate()
        {
            runtime
                .handle()
                .publish(MeshOsEvent::LogLine(LogLine {
                    level,
                    daemon_id: Some(7),
                    message: format!("msg {}", i),
                }))
                .await
                .unwrap();
        }

        let r1 = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("r1 timed out")
            .expect("r1 closed")
            .expect("r1 ok");
        let r2 = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("r2 timed out")
            .expect("r2 closed")
            .expect("r2 ok");
        let r3 = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("r3 timed out")
            .expect("r3 closed")
            .expect("r3 ok");
        assert!(r1.seq < r2.seq);
        assert!(r2.seq < r3.seq);
        assert_eq!(r1.level, LogLevel::Info);
        assert_eq!(r3.level, LogLevel::Error);
        // The loop stamps `node_id = Some(this_node)` on every
        // locally-published line.
        assert_eq!(r1.node_id, Some(42));
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn subscribe_logs_min_level_filter_drops_below_threshold() {
        use crate::adapter::net::behavior::meshos::{LogLevel, LogLine, MeshOsEvent};
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            },
        );

        let mut stream = deck.subscribe_logs(LogFilter::new().min_level(LogLevel::Warn));
        runtime
            .handle()
            .publish(MeshOsEvent::LogLine(LogLine::info(None, "info dropped")))
            .await
            .unwrap();
        runtime
            .handle()
            .publish(MeshOsEvent::LogLine(LogLine::warn(None, "warn kept")))
            .await
            .unwrap();

        let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("next timed out")
            .expect("next closed")
            .expect("next ok");
        assert_eq!(next.level, LogLevel::Warn);
        assert_eq!(next.message, "warn kept");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn subscribe_logs_with_daemon_filter_keeps_only_matching_daemon() {
        use crate::adapter::net::behavior::meshos::{LogLine, MeshOsEvent};
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            },
        );

        let mut stream = deck.subscribe_logs(LogFilter::new().with_daemon(7));
        runtime
            .handle()
            .publish(MeshOsEvent::LogLine(LogLine::info(
                Some(99),
                "other daemon",
            )))
            .await
            .unwrap();
        runtime
            .handle()
            .publish(MeshOsEvent::LogLine(LogLine::info(
                Some(7),
                "target daemon",
            )))
            .await
            .unwrap();

        let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("next timed out")
            .expect("next closed")
            .expect("next ok");
        assert_eq!(next.daemon_id, Some(7));
        assert_eq!(next.message, "target daemon");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_stream_emits_one_record_per_signed_commit_in_order() {
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            },
        );

        let mut stream = deck.audit().stream();
        // No commits yet — the stream should yield nothing for
        // at least one tick.
        let first_attempt = tokio::time::timeout(Duration::from_millis(40), stream.next()).await;
        assert!(first_attempt.is_err(), "stream should park when no records");

        // Issue three commits; the stream should yield three
        // records in seq order.
        deck.admin().cordon(42).await.unwrap();
        deck.admin().uncordon(42).await.unwrap();
        deck.admin().invalidate_placement(42).await.unwrap();

        let r1 = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("r1 timed out")
            .expect("r1 closed")
            .expect("r1 ok");
        let r2 = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("r2 timed out")
            .expect("r2 closed")
            .expect("r2 ok");
        let r3 = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("r3 timed out")
            .expect("r3 closed")
            .expect("r3 ok");

        // Stream emits in seq order (substrate guarantees seq
        // strictly increases).
        assert!(r1.seq < r2.seq);
        assert!(r2.seq < r3.seq);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_stream_dedups_already_seen_records_across_polls() {
        // Two consecutive polls on the same snapshot must NOT
        // re-emit records. The seq-based watermark guarantees
        // this even when the snapshot ring carries records
        // older than the stream's watermark.
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(10),
                ..DeckClientConfig::default()
            },
        );

        deck.admin().cordon(42).await.unwrap();
        let mut stream = deck.audit().stream();
        let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("first timed out")
            .expect("first closed")
            .expect("first ok");

        // No new commits — the stream should park (not
        // re-emit `first`).
        let second_attempt = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
        assert!(
            second_attempt.is_err(),
            "stream should not re-emit seen record"
        );

        // Issue another commit; only the new one shows up.
        deck.admin().uncordon(42).await.unwrap();
        let second = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("second timed out")
            .expect("second closed")
            .expect("second ok");
        assert!(second.seq > first.seq);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn audit_stream_rearms_waker_after_empty_tick() {
        // Regression for #25: after `poll_next` consumes the
        // interval's Ready tick but finds no matching record, it
        // MUST re-register a waker (tokio's `Interval::poll_tick`
        // leaves none behind once its Ready tick is taken).
        // Without the explicit `wake_by_ref`, a bare
        // `audit_stream.next().await` parks forever. We drive
        // `poll_next` manually with a counting waker under paused
        // time so the assertion is deterministic and can never
        // hang the suite on regression.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::{Context, Poll, Wake, Waker};

        struct CountingWaker(AtomicUsize);
        impl Wake for CountingWaker {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(10),
                ..DeckClientConfig::default()
            },
        );

        let mut stream = deck.audit().stream();
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);

        // First poll: the interval's initial tick fires immediately
        // (tokio intervals are ready on first poll), the ring is
        // empty, so we hit the empty branch and must re-arm. Pin on
        // the stack to poll directly.
        let mut pinned = std::pin::pin!(&mut stream);
        let first = pinned.as_mut().poll_next(&mut cx);
        assert!(
            matches!(first, Poll::Pending),
            "empty ring should yield Pending, got {first:?}"
        );
        assert!(
            counter.0.load(Ordering::SeqCst) >= 1,
            "poll_next must re-register a waker after consuming an empty tick \
             (otherwise the stream parks forever)"
        );

        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_stream_applies_force_only_filter_in_tail_mode() {
        // Mix ordinary (Cordon) and ICE (ThawCluster). With
        // force_only(), the stream yields only the ICE entry.
        use futures::StreamExt;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(10),
                ..DeckClientConfig::default()
            },
        );

        let mut stream = deck.audit().force_only().stream();
        deck.admin().cordon(42).await.unwrap();
        let thaw = deck
            .ice()
            .thaw_cluster()
            .simulate()
            .await
            .expect("simulate");
        let sig =
            deck.identity()
                .sign_proposal(thaw.action(), thaw.issued_at_ms(), &thaw.blast_hash());
        thaw.commit(&[sig]).await.unwrap();

        let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("next timed out")
            .expect("next closed")
            .expect("next ok");
        // Only the ThawCluster (ICE) should pass the filter.
        assert!(next.event.is_ice());
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_query_returns_empty_when_no_ice_commits_observed() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let results = deck.audit().recent(10).collect();
        assert!(results.is_empty());
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_query_returns_recent_entries_newest_first() {
        // Two unsigned commits — they reach the loop via the
        // unsigned admin path because no registry is installed.
        // The audit ring only records `SignedIceCommit` events,
        // so unsigned commits don't appear. We instead publish
        // `SignedIceCommit` directly (no verifier installed, so
        // outcome = Unverified — but the ring records every
        // attempt regardless).
        use crate::adapter::net::behavior::meshos::{IceActionProposal, MeshOsEvent};
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        for ttl_secs in [10, 20, 30] {
            runtime
                .handle()
                .publish(MeshOsEvent::SignedIceCommit {
                    proposal: IceActionProposal::FreezeCluster {
                        ttl: Duration::from_secs(ttl_secs),
                    },
                    signatures: Vec::new(),
                    issued_at_ms: super::super::meshos::now_ms_since_unix_epoch(),
                    blast_hash: TEST_BLAST_HASH,
                })
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
        let all = deck.audit().collect();
        assert_eq!(all.len(), 3, "ring should hold all three entries");
        // Newest-first ordering: the 30s freeze is the last
        // commit submitted, so it should be the first result.
        assert!(matches!(
            all[0].event,
            AdminEvent::FreezeCluster { ttl } if ttl == Duration::from_secs(30)
        ));
        assert!(matches!(
            all[2].event,
            AdminEvent::FreezeCluster { ttl } if ttl == Duration::from_secs(10)
        ));

        let recent_one = deck.audit().recent(1).collect();
        assert_eq!(recent_one.len(), 1);
        assert!(matches!(
            recent_one[0].event,
            AdminEvent::FreezeCluster { ttl } if ttl == Duration::from_secs(30)
        ));
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_query_filters_by_operator_id() {
        use crate::adapter::net::behavior::meshos::{IceActionProposal, MeshOsEvent};
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let op_a = OperatorIdentity::generate();
        let op_b = OperatorIdentity::generate();
        let deck = DeckClient::from_runtime(&runtime, op_a.clone());

        // Commit from op_a.
        let proposal_a = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(10),
        };
        let ts_a = super::super::meshos::now_ms_since_unix_epoch();
        let sig_a = OperatorSignature::sign(op_a.keypair(), &proposal_a, ts_a, &TEST_BLAST_HASH);
        runtime
            .handle()
            .publish(MeshOsEvent::SignedIceCommit {
                proposal: proposal_a,
                signatures: vec![sig_a],
                issued_at_ms: ts_a,
                blast_hash: TEST_BLAST_HASH,
            })
            .await
            .unwrap();
        // Commit from op_b.
        let proposal_b = IceActionProposal::ThawCluster;
        let ts_b = super::super::meshos::now_ms_since_unix_epoch();
        let sig_b = OperatorSignature::sign(op_b.keypair(), &proposal_b, ts_b, &TEST_BLAST_HASH);
        runtime
            .handle()
            .publish(MeshOsEvent::SignedIceCommit {
                proposal: proposal_b,
                signatures: vec![sig_b],
                issued_at_ms: ts_b,
                blast_hash: TEST_BLAST_HASH,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;

        let filtered = deck.audit().by_operator(op_a.operator_id()).collect();
        assert_eq!(filtered.len(), 1);
        assert!(matches!(
            filtered[0].event,
            AdminEvent::FreezeCluster { .. }
        ));
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_query_force_only_drops_ordinary_admin_keeps_ice() {
        // Mix ordinary admin (Cordon) with ICE (ThawCluster);
        // force_only() should keep only the ICE entry.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        deck.admin().cordon(42).await.expect("cordon");
        let thaw = deck
            .ice()
            .thaw_cluster()
            .simulate()
            .await
            .expect("simulate");
        let sig =
            deck.identity()
                .sign_proposal(thaw.action(), thaw.issued_at_ms(), &thaw.blast_hash());
        thaw.commit(&[sig]).await.expect("thaw");
        tokio::time::sleep(Duration::from_millis(80)).await;

        let baseline = deck.audit().collect();
        assert_eq!(
            baseline.len(),
            2,
            "ring should hold both ordinary and ICE commits"
        );
        let force_only = deck.audit().force_only().collect();
        assert_eq!(force_only.len(), 1, "force_only should drop Cordon");
        assert!(force_only[0].event.is_ice());
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn admin_commit_routes_through_signed_path_when_registry_installed() {
        // With an OperatorRegistry installed, the SDK's
        // AdminCommands signs every admin event and routes via
        // SignedAdminCommit; the substrate verifier accepts +
        // the audit ring shows the operator + Accepted outcome.
        use std::sync::Arc as SArc;
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let identity = OperatorIdentity::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(identity.keypair());
        let verifier = SArc::new(crate::adapter::net::behavior::meshos::AdminVerifier::new(
            SArc::new(registry.clone()),
            1,
        ));
        let runtime = MeshOsRuntime::start_with_all(
            fast_config(),
            dispatcher,
            Default::default(),
            Default::default(),
            SArc::new(crate::adapter::net::compute::DaemonRegistry::new()),
            None,
            Some(verifier),
        );
        let deck =
            DeckClient::from_runtime(&runtime, identity.clone()).with_operator_registry(registry);

        let commit = deck.admin().cordon(42).await.expect("commit");
        assert_eq!(commit.event_kind(), "cordon");

        // Audit ring should show the commit with Accepted
        // outcome + the issuing operator id.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let entries = deck.audit().collect();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].outcome,
            crate::adapter::net::behavior::meshos::VerificationOutcome::Accepted
        ));
        assert_eq!(entries[0].operator_ids, vec![identity.operator_id()]);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn admin_commit_falls_back_to_unsigned_when_no_registry_installed() {
        // Without a registry the SDK routes through the
        // legacy unsigned admin path. Audit ring still records
        // the commit but with Unverified outcome.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        deck.admin().cordon(42).await.expect("commit");
        tokio::time::sleep(Duration::from_millis(80)).await;

        let entries = deck.audit().collect();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].outcome,
            crate::adapter::net::behavior::meshos::VerificationOutcome::Unverified
        ));
        assert!(entries[0].operator_ids.is_empty());
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_ring_records_unsigned_admin_with_unverified_outcome() {
        // Locked decision #2: every admin event the loop sees
        // is on the audit ring. Unsigned ones surface as
        // Unverified.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        deck.admin().cordon(42).await.expect("cordon");
        deck.admin()
            .drop_replicas(42, vec![1, 2])
            .await
            .expect("drop_replicas");
        tokio::time::sleep(Duration::from_millis(80)).await;

        let entries = deck.audit().collect();
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert!(matches!(
                entry.outcome,
                crate::adapter::net::behavior::meshos::VerificationOutcome::Unverified
            ));
            assert!(entry.operator_ids.is_empty());
        }
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn audit_query_between_filters_outside_window() {
        use crate::adapter::net::behavior::meshos::{IceActionProposal, MeshOsEvent};
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        runtime
            .handle()
            .publish(MeshOsEvent::SignedIceCommit {
                proposal: IceActionProposal::ThawCluster,
                signatures: Vec::new(),
                issued_at_ms: super::super::meshos::now_ms_since_unix_epoch(),
                blast_hash: TEST_BLAST_HASH,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;

        // A window entirely in the past — no entries should
        // match.
        let past_only = deck.audit().between(0, 1).collect();
        assert!(past_only.is_empty());

        // A window covering "now" — should match.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let around_now = deck
            .audit()
            .between(now_ms - 10_000, now_ms + 10_000)
            .collect();
        assert_eq!(around_now.len(), 1);
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn ice_force_restart_daemon_proposal_round_trips() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let daemon = super::super::meshos::DaemonRef {
            id: 7,
            name: "telemetry".into(),
        };
        let proposal = deck.ice().force_restart_daemon(daemon.clone());
        let simulated = proposal.simulate().await.expect("simulate");
        assert_eq!(simulated.blast_radius().affected_daemons, vec![daemon]);
        let sig = deck.identity().sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let commit = simulated.commit(&[sig]).await.expect("commit");
        assert_eq!(commit.event_kind(), "force_restart_daemon");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn ice_kill_migration_proposal_round_trips_and_audits() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let proposal = deck.ice().kill_migration(123);
        let simulated = proposal.simulate().await.expect("simulate");
        let sig = deck.identity().sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let commit = simulated.commit(&[sig]).await.expect("commit");
        assert_eq!(commit.event_kind(), "kill_migration");

        // Confirms the commit lands on the audit ring even
        // though the dispatcher integration is pending.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let entries = deck.audit().force_only().collect();
        assert!(entries
            .iter()
            .any(|r| matches!(r.event, AdminEvent::KillMigration { migration: 123 })));
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn ice_force_cutover_proposal_round_trips() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let proposal = deck.ice().force_cutover(100, 42);
        let simulated = proposal.simulate().await.expect("simulate");
        assert_eq!(simulated.blast_radius().affected_replicas, vec![100]);
        assert_eq!(simulated.blast_radius().affected_nodes, vec![42]);
        let sig = deck.identity().sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let commit = simulated.commit(&[sig]).await.expect("commit");
        assert_eq!(commit.event_kind(), "force_cutover");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn ice_force_evict_replica_proposal_round_trips() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
        let proposal = deck.ice().force_evict_replica(100, 7);
        let simulated = proposal.simulate().await.expect("simulate");
        assert_eq!(simulated.blast_radius().affected_replicas, vec![100]);
        assert_eq!(simulated.blast_radius().affected_nodes, vec![7]);
        let sig = deck.identity().sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let commit = simulated.commit(&[sig]).await.expect("commit");
        assert_eq!(commit.event_kind(), "force_evict_replica");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn ice_commit_with_registry_accepts_a_valid_multi_op_bundle() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let op_a = OperatorIdentity::generate();
        let op_b = OperatorIdentity::generate();
        let mut registry = OperatorRegistry::new();
        registry.register(op_a.keypair());
        registry.register(op_b.keypair());
        let deck = DeckClient::new(
            runtime.handle_clone(),
            runtime.snapshot_reader().clone(),
            op_a.clone(),
            DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(100),
                ice_signature_threshold: 2,
            },
        )
        .with_operator_registry(registry);

        let proposal = deck.ice().freeze_cluster(Duration::from_secs(15));
        let simulated = proposal.simulate().await.expect("simulate");
        let sig_a = op_a.sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let sig_b = op_b.sign_proposal(
            simulated.action(),
            simulated.issued_at_ms(),
            &simulated.blast_hash(),
        );
        let commit = simulated
            .commit(&[sig_a, sig_b])
            .await
            .expect("valid multi-op bundle should commit");
        assert_eq!(commit.event_kind(), "freeze_cluster");
        let _ = runtime.shutdown().await;
    }
}
