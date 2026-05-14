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
use std::time::{Duration, Instant, SystemTime};

use futures::Stream;
use tokio::time::{interval, Interval};

use super::meshos::{
    simulate_ice_proposal, AdminEvent, BlastRadius, ChainId, IceActionProposal, MeshOsEvent,
    MeshOsHandle, MeshOsHandleError, MeshOsRuntime, MeshOsSnapshot, MeshOsSnapshotReader, NodeId,
};
use crate::adapter::net::identity::EntityKeypair;

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

    /// Borrow the underlying keypair. The signing seam reads this
    /// when the substrate slice that adds operator-signed admin
    /// commits lands.
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
            MeshOsHandleError::LoopClosed => {
                Self::new("loop_closed", "MeshOS loop has exited")
            }
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
    /// Minimum operator signatures required to [`IceProposal::commit`]
    /// an ICE proposal. Plan locks this in at 2-of-N by default,
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

/// Operator-facing client. Composes a [`MeshOsHandle`] +
/// [`MeshOsSnapshotReader`] + [`OperatorIdentity`] into the
/// surface Deck-the-binary (and other operator tools) bind
/// against.
///
/// Constructed via [`Self::from_runtime`] (when the caller holds
/// the live runtime) or [`Self::new`] (when the caller already
/// has handle + reader and wants to compose explicitly).
pub struct DeckClient {
    handle: MeshOsHandle,
    snapshot_reader: MeshOsSnapshotReader,
    identity: OperatorIdentity,
    config: DeckClientConfig,
    commit_seq: AtomicU64,
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
            commit_seq: AtomicU64::new(0),
        }
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
        self.handle
            .publish(MeshOsEvent::AdminEvent(event))
            .await
            .map_err(AdminError::from)?;
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
    /// Drain `node`'s workload by `deadline`. Replicas migrate;
    /// daemons drain via [`crate::adapter::net::compute::DaemonControl::DrainStart`]
    /// once the loop sees the resulting `EnteringMaintenance` state.
    pub async fn drain(
        &self,
        node: NodeId,
        deadline: Instant,
    ) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::Drain { node, deadline }, "drain")
            .await
    }

    /// Begin a maintenance window for `node`. `deadline` is the
    /// drain deadline; `None` defers to the cluster's configured
    /// default.
    pub async fn enter_maintenance(
        &self,
        node: NodeId,
        deadline: Option<Instant>,
    ) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(
                AdminEvent::EnterMaintenance { node, deadline },
                "enter_maintenance",
            )
            .await
    }

    /// End a maintenance window for `node`.
    pub async fn exit_maintenance(
        &self,
        node: NodeId,
    ) -> Result<ChainCommit, AdminError> {
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
    pub async fn invalidate_placement(
        &self,
        node: NodeId,
    ) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(
                AdminEvent::InvalidatePlacement { node },
                "invalidate_placement",
            )
            .await
    }

    /// Force-restart every daemon on `node`.
    pub async fn restart_all_daemons(
        &self,
        node: NodeId,
    ) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(
                AdminEvent::RestartAllDaemons { node },
                "restart_all_daemons",
            )
            .await
    }

    /// Clear `node`'s local avoid list.
    pub async fn clear_avoid_list(
        &self,
        node: NodeId,
    ) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::ClearAvoidList { node }, "clear_avoid_list")
            .await
    }

    /// Pause reconcile-driven action emission cluster-wide for
    /// `ttl`. While in effect every node's reconcile pass
    /// returns an empty action vector; folds + chain commits
    /// keep running. The freeze auto-clears at `now + ttl`; an
    /// earlier [`Self::thaw_cluster`] cancels it.
    ///
    /// ICE break-glass surface. The Phase 2 substrate slice
    /// landed the freeze gate; the SDK exposes it on the
    /// existing [`AdminCommands`] surface for Phase 1 callers.
    /// Phase 3 lifts this onto a dedicated `IceCommands` surface
    /// with the proposal / simulate / multi-operator-sign
    /// discipline the plan locks in.
    pub async fn freeze_cluster(
        &self,
        ttl: Duration,
    ) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::FreezeCluster { ttl }, "freeze_cluster")
            .await
    }

    /// Cancel an in-effect cluster freeze. Idempotent — no-op
    /// if no freeze is in effect.
    pub async fn thaw_cluster(&self) -> Result<ChainCommit, AdminError> {
        self.client
            .publish_admin(AdminEvent::ThawCluster, "thaw_cluster")
            .await
    }
}

/// Operator signature over an [`IceActionProposal`]. Phase 3a
/// placeholder — the substrate doesn't yet verify operator-key
/// channel-auth, so the bytes here are not yet a real ed25519
/// signature over a deterministic encoding of the proposal.
/// The shape is what the substrate slice that lands signature
/// verification will accept (`operator_id` + `signature` blob),
/// so consumers of the SDK don't need to change call sites when
/// the real signing path goes live.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperatorSignature {
    /// Issuing operator's id (from [`OperatorIdentity::operator_id`]).
    pub operator_id: u64,
    /// Signature bytes. Phase 3a: opaque placeholder. Future:
    /// ed25519 signature over a deterministic postcard encoding
    /// of the [`IceActionProposal`].
    pub signature: Vec<u8>,
}

impl OperatorSignature {
    /// Build a placeholder signature for `proposal` using
    /// `identity`'s operator id. The bytes are deterministic
    /// (postcard-encoded proposal payload) so two operators
    /// "signing" the same proposal produce reproducible inputs
    /// — useful for the multi-operator-bundle tests that will
    /// land alongside the real signing path.
    pub fn sign(identity: &OperatorIdentity, proposal: &IceActionProposal) -> Self {
        // Postcard-encode the proposal as the deterministic
        // payload. The real signing path will run ed25519 over
        // this same byte sequence; until that lands, we carry the
        // encoded payload itself as a placeholder so tests can
        // confirm "the same proposal produces the same signature
        // input" without depending on the substrate verifier.
        let payload = postcard::to_allocvec(proposal).unwrap_or_default();
        Self {
            operator_id: identity.operator_id(),
            signature: payload,
        }
    }
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

    /// Propose cancelling an in-effect cluster freeze.
    pub fn thaw_cluster(&self) -> IceProposal<'a> {
        IceProposal::new(self.client, IceActionProposal::ThawCluster)
    }
}

/// An ICE proposal — opaque-ish handle carrying the underlying
/// [`IceActionProposal`] plus a bit of per-proposal state.
/// Per the plan's locked decision #4 a [`Self::simulate`] call
/// must precede [`Self::commit`]; a `commit` against an
/// un-simulated proposal returns [`IceError`] with kind
/// `simulation_required`.
pub struct IceProposal<'a> {
    client: &'a DeckClient,
    action: IceActionProposal,
    simulated: std::cell::Cell<bool>,
}

impl<'a> IceProposal<'a> {
    fn new(client: &'a DeckClient, action: IceActionProposal) -> Self {
        Self {
            client,
            action,
            simulated: std::cell::Cell::new(false),
        }
    }

    /// Borrow the underlying [`IceActionProposal`]. Useful for
    /// the multi-operator signing flow: each operator signs the
    /// same proposal payload, then submits the bundle through
    /// one client's `commit()` call.
    pub fn action(&self) -> &IceActionProposal {
        &self.action
    }

    /// Pre-execution preview. Runs the substrate's pure simulator
    /// against the runtime's latest snapshot; flags the proposal
    /// as simulated so [`Self::commit`] will accept it.
    pub async fn simulate(&self) -> Result<BlastRadius, IceError> {
        let snap = self.client.snapshot_reader.read();
        let blast = simulate_ice_proposal(&snap, &self.action);
        self.simulated.set(true);
        Ok(blast)
    }

    /// Commit the proposal. Returns
    /// `Err(IceError::simulation_required)` if [`Self::simulate`]
    /// hasn't been called on this proposal. Verifies
    /// `signatures.len() >= ice_signature_threshold` before
    /// publishing; returns
    /// `Err(IceError::insufficient_signatures)` otherwise.
    /// Substrate-side multi-operator-signature verification is
    /// a future slice — until then the SDK enforces the threshold
    /// locally and accepts placeholder [`OperatorSignature`]
    /// payloads.
    pub async fn commit(
        self,
        signatures: &[OperatorSignature],
    ) -> Result<ChainCommit, IceError> {
        if !self.simulated.get() {
            return Err(IceError::new(
                "simulation_required",
                "ICE commits require a successful simulate() before commit() per \
                 DECK_SDK_PLAN.md locked decision #4",
            ));
        }
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
        let admin = self.client.admin();
        match self.action {
            IceActionProposal::FreezeCluster { ttl } => admin.freeze_cluster(ttl).await,
            IceActionProposal::ThawCluster => admin.thaw_cluster().await,
        }
    }
}

/// Stream over the runtime's snapshot reader. Polls at the
/// configured cadence; emits a `Result<MeshOsSnapshot, DeckError>`
/// per poll. Phase 1 emits on every poll (consumers de-dupe if
/// they care); the substrate's tail-with-replay path replaces
/// this with a chain-driven stream when it lands.
pub struct SnapshotStream {
    reader: MeshOsSnapshotReader,
    interval: Interval,
}

impl SnapshotStream {
    fn new(reader: MeshOsSnapshotReader, poll_interval: Duration) -> Self {
        // Floor the interval so a zero-duration config doesn't
        // hot-spin the executor.
        let poll_interval = poll_interval.max(Duration::from_millis(1));
        Self {
            reader,
            interval: interval(poll_interval),
        }
    }
}

impl Stream for SnapshotStream {
    type Item = Result<MeshOsSnapshot, DeckError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.interval.poll_tick(cx) {
            Poll::Ready(_) => Poll::Ready(Some(Ok(self.reader.read()))),
            Poll::Pending => Poll::Pending,
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
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate())
            .with_config(DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(20),
                ..DeckClientConfig::default()
            });

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
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate())
            .with_config(DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(15),
                ..DeckClientConfig::default()
            });

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

    #[tokio::test]
    async fn ice_proposal_commit_without_prior_simulate_returns_simulation_required() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

        let proposal = deck.ice().freeze_cluster(Duration::from_secs(10));
        let sig = OperatorSignature::sign(deck.identity(), proposal.action());
        let err = proposal
            .commit(&[sig])
            .await
            .expect_err("commit without simulate should fail");
        assert_eq!(err.kind, "simulation_required");
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn ice_proposal_commit_with_insufficient_signatures_fails() {
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
        // Bump threshold above what we'll supply.
        let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate())
            .with_config(DeckClientConfig {
                snapshot_poll_interval: Duration::from_millis(100),
                ice_signature_threshold: 2,
            });
        let proposal = deck.ice().freeze_cluster(Duration::from_secs(10));
        let _blast = proposal.simulate().await.expect("simulate");
        let sig = OperatorSignature::sign(deck.identity(), proposal.action());
        let err = proposal
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
        let blast = proposal.simulate().await.expect("simulate");
        // FreezeCluster sets the drain delay to the requested TTL.
        assert_eq!(blast.estimated_drain_delay, Some(Duration::from_secs(30)));
        let sig = OperatorSignature::sign(deck.identity(), proposal.action());
        let commit = proposal.commit(&[sig]).await.expect("commit");
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
        let blast = proposal.simulate().await.expect("simulate");
        // Simulator on a non-frozen snapshot warns "no freeze to cancel."
        assert!(blast
            .warnings
            .iter()
            .any(|w| matches!(
                w,
                crate::adapter::net::behavior::meshos::BlastWarning::ThawHasNoFreezeToCancel
            )));
        let _ = runtime.shutdown().await;
    }

    #[tokio::test]
    async fn operator_signature_carries_issuing_operator_id() {
        let identity = OperatorIdentity::generate();
        let proposal = IceActionProposal::FreezeCluster {
            ttl: Duration::from_secs(60),
        };
        let sig = OperatorSignature::sign(&identity, &proposal);
        assert_eq!(sig.operator_id, identity.operator_id());
        assert!(!sig.signature.is_empty(), "signature payload should encode the proposal");
    }
}
