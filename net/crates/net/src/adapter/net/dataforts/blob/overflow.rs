//! Active-overflow controller (v0.3 P2).
//!
//! Push-side complement of [`super::migration::BlobMigrationController`].
//! Migration is *pull* (the local node decides to take an
//! advertised hot blob); overflow is *push* (the local node
//! decides to shed a cold blob and a remote node decides
//! whether to accept). The two surfaces parallel each other —
//! every reject reason on either side maps to a Prometheus
//! counter label so operators can dashboard both directions.
//!
//! See [`DATAFORTS_BLOB_OVERFLOW_PLAN.md`] for the full design.
//!
//! # P2 scope
//!
//! Pure-logic controller + tick driver + hysteresis state
//! machine. The actual wire push (`OverflowPush` RPC) lands in
//! P3; this module abstracts that away behind the
//! [`OverflowPushSink`] trait so the tick can be unit-tested
//! against a recorder without spinning up a real mesh.
//!
//! [`DATAFORTS_BLOB_OVERFLOW_PLAN.md`]: ../../../../../docs/plans/DATAFORTS_BLOB_OVERFLOW_PLAN.md

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::admission::OverflowReject;
use super::error::BlobError;
use super::mesh::OverflowConfig;
use super::refcount::BlobRefcountTable;
#[cfg(test)]
use crate::adapter::net::behavior::capability::CapabilityIndex;
use crate::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
use crate::adapter::net::behavior::{
    is_blob_storage_unhealthy, BlobCapability, CapabilitySet, GravityCapability, TopologyScope,
};
use crate::adapter::net::dataforts::gravity::BlobHeatRegistry;

/// Service-name token for the overflow-push nRPC channel.
/// The sender constructs a request on
/// `"{OVERFLOW_PUSH_SERVICE}.requests"` and listens on
/// `"{OVERFLOW_PUSH_SERVICE}.replies.<origin>"`; the receiver
/// registers a handler under the same service name via
/// [`crate::adapter::net::MeshNode::serve_overflow_push`].
///
/// Held as a const so a typo on either side surfaces at
/// compile time. The wire form is the literal string — no
/// version suffix (per-tag versioning lives inside the wire
/// payload, not the channel name).
pub const OVERFLOW_PUSH_SERVICE: &str = "dataforts.blob.overflow_push";

/// Wire request body for an overflow push. The sender encodes
/// this via postcard + drops it into the nRPC payload; the
/// receiver decodes, runs [`super::admission::should_accept_overflow_from`],
/// and on Admit opens the chunk channel against the local
/// adapter so the existing replication runtime can pull the
/// bytes.
///
/// The chunk bytes themselves do NOT ride this request — the
/// nRPC envelope carries the *nudge*, not the chunk payload.
/// `size_bytes` is the resolved chunk size so the receive-side
/// disk-gate can fire without round-tripping a `stat` call.
///
/// Wire layout: postcard's default `(field_order)` encoding.
/// The field order is locked here for forward compatibility;
/// adding new fields requires a versioned variant (the trait-
/// object polymorphism on the postcard side is rigid). A
/// future v2 would land as a separate type registered under
/// a new service-name token, with v1 receivers ignoring the
/// new channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverflowPush {
    /// 32-byte BLAKE3 hash of the chunk to push.
    pub blob_hash: [u8; 32],
    /// Wire size of the chunk in bytes. Drives the receive-
    /// side disk-gate.
    pub size_bytes: u64,
    /// Sender's canonical `node_id`. The receiver looks the
    /// sender's [`CapabilitySet`] up in its local
    /// [`CapabilityIndex`] keyed on this id; the admission
    /// check reads `overflow_enabled` + scope tags from the
    /// looked-up snapshot, not from the request body. Defends
    /// against a sender forging its caps via the request — the
    /// only authority is the verified capability index.
    pub sender_node_id: u64,
}

/// Wire response body. Sender-side observes the result and
/// either records the admission outcome (`Accepted`) or
/// dispatches the typed reject reason to the per-reason
/// counter family. The chunk-channel open on the receive
/// side happens *during* `Accepted` — by the time the sender
/// observes `Accepted`, the receiver has either successfully
/// opened the channel or returned a typed error variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverflowPushAck {
    /// Receiver ran admission, returned Admit, and the
    /// chunk channel open returned Ok. The bytes are now in
    /// flight via the existing replication runtime; the
    /// durability watermark observation (post-tick) is the
    /// sender's signal to drop the local copy.
    Accepted,
    /// Receiver ran admission, returned Reject. Carries the
    /// typed reason so the sender can break out per-reason
    /// counters + decide whether to retry against the same
    /// peer (e.g. `InsufficientDisk` won't change quickly; a
    /// different target is the right move) or pick a new one.
    Rejected(OverflowReject),
    /// Receiver ran admission, returned Admit, but the
    /// chunk channel open itself failed (the replication
    /// runtime couldn't spawn, a transient disk error, etc.).
    /// Wire-distinct from `Rejected` because the failure
    /// mode is "we wanted to take it, our local plumbing
    /// broke" rather than "we won't take it." Operators
    /// alarm on `OpenChunkFailed` more aggressively.
    OpenChunkFailed,
}

/// Output of [`BlobOverflowController::candidate_batch`]: the
/// list of candidates to push this tick, plus the precise
/// count of hashes that were attempted for target selection
/// but found no eligible peer. The pair lets the tick driver
/// report `rejected_no_target` accurately — distinguishing
/// "we tried and no peer qualified" from "we hit the per-tick
/// push cap and never tried the rest."
#[derive(Clone, Debug, Default)]
pub struct OverflowCandidateBatch {
    /// Candidates with a selected target peer, truncated to
    /// `config.max_pushes_per_tick`.
    pub candidates: Vec<BlobOverflowCandidate>,
    /// Number of hashes the controller attempted target
    /// selection for and got `None` back. Bounded above by
    /// `config.max_pushes_per_tick` (the loop breaks once
    /// `candidates.len()` reaches the cap, so further
    /// hashes are never tried). The tick driver routes
    /// this directly to `rejected_no_target` without
    /// double-counting truncated hashes.
    pub no_target_count: usize,
}

/// One overflow-push candidate the controller is considering
/// for this tick. The push controller already selected a
/// target peer for `hash`; the tick driver routes the actual
/// push call through [`OverflowPushSink::push`].
///
/// Equivalent shape to [`super::migration::BlobMigrationCandidate`]
/// with the direction reversed — `target_node_id` is the
/// receive-side, not the publisher.
#[derive(Clone, Debug)]
pub struct BlobOverflowCandidate {
    /// 32-byte chunk hash to push.
    pub hash: [u8; 32],
    /// Wire size of the chunk in bytes. Drives the receiver's
    /// `disk_free_gb` admission gate; the sender supplies it
    /// here so the receiver doesn't have to round-trip a
    /// `stat` call first.
    pub size_bytes: u64,
    /// node_id of the selected receive-side peer.
    pub target_node_id: u64,
    /// Snapshot of the target's capability set at selection
    /// time. The receive-side admission decision will re-read
    /// the index fresh; this snapshot is for the sender's
    /// dashboards / debug logs.
    pub target_caps: CapabilitySet,
    /// Decayed heat rate of `hash` at controller-tick time.
    /// Coldest candidates come first when the tick truncates
    /// to `max_pushes_per_tick`. `0.0` for hashes that haven't
    /// been read since their last full decay window — these
    /// are the prime overflow targets.
    pub cold_rate: f64,
}

/// Per-tick report. Each field maps to a Prometheus counter
/// label (`dataforts_blob_overflow_*`) so operators can
/// dashboard the loop without hand-coding per-reason metrics.
/// Pre-tick state lives in [`step_overflow_hysteresis`]; this
/// report captures *only* the actions this tick took.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlobOverflowTickReport {
    /// Candidates that passed the controller's filters AND
    /// the sink's push call returned `Ok`. Counts pushes
    /// the wire-side accepted; durability watermark observation
    /// is the next layer (P3).
    pub admitted: u64,
    /// Candidates the controller computed but no overflow-
    /// enabled peer was reachable for. Bumps once per cold
    /// hash that found no target — the same hash on the next
    /// tick may find one as caps propagate.
    pub rejected_no_target: u64,
    /// Sink returned an error. Includes the receive-side
    /// admission rejections that P3 maps to typed
    /// `OverflowReject` variants, plus RPC transport errors,
    /// plus the local-side chunk-open errors. Operators
    /// disambiguate via the underlying wire counters; this
    /// counter is the aggregated send-side view.
    pub push_errors: u64,
    /// The hysteresis state at the start of the tick. `true`
    /// = the controller was already firing on the prior tick.
    /// Useful for telemetry: the difference between "ticks
    /// where overflow was firing the whole time" and "ticks
    /// where the state just transitioned high" is operator-
    /// meaningful (the latter signals a workload spike).
    pub was_active_at_start: bool,
    /// The hysteresis state at the end of the tick. The pair
    /// `(was_active_at_start, is_active_at_end)` documents
    /// the state machine transition this tick took.
    pub is_active_at_end: bool,
    /// Local disk-usage ratio at the start of the tick.
    /// `disk_used_bytes / disk_total_bytes`. Surfaced for
    /// operator dashboards.
    pub disk_ratio_at_start: f64,
    /// Local disk-usage ratio at the end of the tick. Equal
    /// to `disk_ratio_at_start` in P2 (the actual freed-bytes
    /// accounting needs the durability watermark observation
    /// from P3 + a re-poll of disk stats). Reserved for P3.
    pub disk_ratio_at_end: f64,
    /// Total bytes pushed this tick. Sum of `size_bytes` of
    /// every admitted candidate. P2 reports the *pushed*
    /// volume; *reclaimed* volume waits for P3's durability
    /// observation.
    pub pushed_bytes: u64,
}

/// Pure-logic hysteresis state machine. Given the prior
/// `active` state + the current `disk_ratio` + the two
/// thresholds, return whether this tick should fire pushes
/// and update `active` in place. Mirrors the existing
/// [`super::metrics::evaluate_health_gate`] discipline
/// (which uses identical 95 % / 85 % shape but for the
/// health-gate tag).
///
/// Hysteresis rule:
///
/// - `disk_ratio >= high_water` → active = `true`.
/// - `disk_ratio <= low_water` → active = `false`.
/// - `low_water < disk_ratio < high_water` → active = prior
///   value (stay where we were; the hysteresis band).
///
/// `low_water >= high_water` degenerates to "fire whenever
/// disk_ratio >= high_water, clear whenever disk_ratio <= low_water"
/// — operator misconfiguration but not unsafe; the active
/// state just doesn't get the hysteresis benefit.
///
/// Returns the post-tick `active` state. The function reads
/// from + writes to `active` under `Relaxed` ordering — the
/// caller is the single tick driver, so no cross-thread
/// ordering is needed; the atomic is for visibility across
/// adapter clones (operator dashboard reads from one clone,
/// tick fires on another).
pub fn step_overflow_hysteresis(
    active: &AtomicBool,
    disk_ratio: f64,
    high_water: f64,
    low_water: f64,
) -> bool {
    let was_active = active.load(Ordering::Relaxed);
    let now_active = if disk_ratio >= high_water {
        true
    } else if disk_ratio <= low_water {
        false
    } else {
        was_active
    };
    if now_active != was_active {
        active.store(now_active, Ordering::Relaxed);
    }
    now_active
}

/// Receive-side handler for the overflow nRPC. Implements
/// [`cortex::RpcHandler`] so it slots into [`MeshNode::serve_rpc`]
/// (under the [`OVERFLOW_PUSH_SERVICE`] service name). On each
/// incoming request:
///
/// 1. Decode the postcard-encoded [`OverflowPush`].
/// 2. Look up `sender_caps` in `capability_index` keyed on
///    `request.sender_node_id`.
/// 3. Run [`super::admission::should_accept_overflow_from`]
///    against the live `local_caps` snapshot + the sender's
///    caps + the chunk size.
/// 4. On Admit: build a [`super::blob_ref::BlobRef::small`]
///    from `(blob_hash, size_bytes)` and call
///    [`super::adapter::BlobAdapter::prefetch`] — this opens
///    the chunk channel with replication armed and the
///    existing per-chunk replication runtime pulls the
///    bytes from whoever advertises `causal:<hash>`
///    (typically the sender). Returns
///    [`OverflowPushAck::Accepted`] on success,
///    [`OverflowPushAck::OpenChunkFailed`] on local-plumbing
///    error.
/// 5. On Reject: wrap the typed [`OverflowReject`] in
///    [`OverflowPushAck::Rejected`] and return.
///
/// The handler holds `Arc<MeshNode>` so it reads live local
/// caps + the capability index at each call rather than a
/// build-time snapshot. Toggling `overflow_enabled` on the
/// adapter is observable immediately on the next inbound
/// push.
///
/// [`cortex::RpcHandler`]: crate::adapter::net::cortex::RpcHandler
/// [`MeshNode::serve_rpc`]: crate::adapter::net::MeshNode::serve_rpc
#[cfg(feature = "cortex")]
pub struct OverflowPushHandler {
    /// Reference to the local mesh node. Used for the
    /// capability-index lookup + the local-caps snapshot.
    /// Holds an `Arc` rather than a borrow because the
    /// handler is registered into the nRPC fold which owns
    /// it via `Arc<dyn RpcHandler>` — the handler outlives
    /// any single tick.
    pub mesh: Arc<crate::adapter::net::MeshNode>,
    /// The local blob adapter. The handler calls
    /// `adapter.prefetch(BlobRef)` on Admit to open the
    /// chunk channel. Held by `Arc` for the same reason as
    /// `mesh`; cheap to clone (the adapter is `Arc`-internal
    /// throughout).
    pub adapter: Arc<super::mesh::MeshBlobAdapter>,
}

#[cfg(feature = "cortex")]
impl OverflowPushHandler {
    /// Construct a handler. Operators wire this into the
    /// receiver-side via
    /// [`crate::adapter::net::MeshNode::serve_overflow_push`].
    pub fn new(
        mesh: Arc<crate::adapter::net::MeshNode>,
        adapter: Arc<super::mesh::MeshBlobAdapter>,
    ) -> Self {
        Self { mesh, adapter }
    }

    /// Pure typed handler logic. Decoded request goes in,
    /// typed ack comes out. Separate from the
    /// [`crate::adapter::net::cortex::RpcHandler`] impl so
    /// tests can drive the admission path without
    /// constructing an [`crate::adapter::net::cortex::RpcContext`].
    ///
    /// Reads live `user_caps_snapshot` + capability-fold
    /// state on each call, so an operator toggling
    /// `overflow_enabled` on the local node is observed by
    /// the next inbound push.
    pub async fn handle(&self, request: OverflowPush) -> OverflowPushAck {
        use super::adapter::BlobAdapter;
        use super::admission::{should_accept_overflow_from, OverflowVerdict};
        use super::blob_ref::BlobRef;

        // Synthesize sender caps from the capability fold.
        // Absent → use the empty default (which has
        // `overflow_enabled = false`); the admission gate
        // will then return `SenderNotOverflowing`.
        let sender_caps =
            super::super::super::behavior::fold::capability_bridge::synthesize_capability_set(
                self.mesh.capability_fold(),
                request.sender_node_id,
            );

        // Snapshot local caps fresh per request so a
        // concurrent `set_overflow_enabled(false)` is
        // observed immediately.
        let local_caps = self.mesh.user_caps_snapshot();

        let verdict = should_accept_overflow_from(&local_caps, &sender_caps, request.size_bytes);
        match verdict {
            OverflowVerdict::Reject(reason) => {
                // Bump the per-reason rejection counter on
                // the receive side. The sender's controller
                // bumps `push_errors_total` separately;
                // dashboards aggregate both surfaces.
                self.adapter.record_overflow_reject(reason);
                OverflowPushAck::Rejected(reason)
            }
            OverflowVerdict::Admit => {
                // Build the BlobRef::Small the prefetch path
                // wants. The URI is `mesh://<hex>` — opaque
                // to the adapter (content-hash is the
                // authoritative address) but the convention
                // matches existing migration code.
                let mut hex = String::with_capacity(64);
                for b in request.blob_hash {
                    use std::fmt::Write;
                    let _ = write!(&mut hex, "{:02x}", b);
                }
                let blob_ref = BlobRef::small(
                    format!("mesh://{}", hex),
                    request.blob_hash,
                    request.size_bytes,
                );
                match self.adapter.prefetch(&blob_ref).await {
                    Ok(()) => OverflowPushAck::Accepted,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            hash = %hex,
                            sender = request.sender_node_id,
                            "overflow push: prefetch failed after admit",
                        );
                        OverflowPushAck::OpenChunkFailed
                    }
                }
            }
        }
    }
}

#[cfg(feature = "cortex")]
#[async_trait]
impl crate::adapter::net::cortex::RpcHandler for OverflowPushHandler {
    async fn call(
        &self,
        ctx: crate::adapter::net::cortex::RpcContext,
    ) -> Result<
        crate::adapter::net::cortex::RpcResponsePayload,
        crate::adapter::net::cortex::RpcHandlerError,
    > {
        use crate::adapter::net::cortex::{RpcHandlerError, RpcResponsePayload, RpcStatus};

        // Decode the request body. Malformed bytes surface
        // as a typed Internal error — the caller sees
        // `RpcStatus::Internal` with a short diagnostic,
        // distinct from `Application(code)` which we use for
        // typed admission rejections.
        let request: OverflowPush = postcard::from_bytes(&ctx.payload.body)
            .map_err(|e| RpcHandlerError::Internal(format!("overflow push: decode failed: {e}")))?;

        let ack = self.handle(request).await;

        // Encode the ack into the response body. Encoding
        // failure is an internal bug (postcard for our typed
        // enum is total); surface as Internal.
        let body = postcard::to_allocvec(&ack).map_err(|e| {
            RpcHandlerError::Internal(format!("overflow push: encode ack failed: {e}"))
        })?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: bytes::Bytes::from(body),
        })
    }
}

/// Concrete [`OverflowPushSink`] implementation backed by a
/// [`MeshNode`]. Wraps the sender-side nRPC call: each
/// `push` invocation encodes the request, dispatches via
/// [`MeshNode::call`] under the [`OVERFLOW_PUSH_SERVICE`]
/// service name, decodes the typed [`OverflowPushAck`], and
/// maps the outcome to the [`OverflowPushSink::push`]
/// `Result` shape the controller expects.
///
/// Construct once per operator scheduler (the sink is cheap
/// to clone — holds an `Arc<MeshNode>`). Pass to
/// [`drive_blob_overflow_tick`] as `&dyn OverflowPushSink`.
///
/// [`MeshNode`]: crate::adapter::net::MeshNode
/// [`MeshNode::call`]: crate::adapter::net::MeshNode::call
#[cfg(feature = "cortex")]
pub struct MeshNodeOverflowPushSink {
    /// Reference to the local mesh. `Arc<MeshNode>` because
    /// `MeshNode::call` is defined on `&Arc<Self>` — the
    /// nRPC path needs the Arc to register the per-call
    /// reply-channel subscription.
    pub mesh: Arc<crate::adapter::net::MeshNode>,
}

#[cfg(feature = "cortex")]
impl MeshNodeOverflowPushSink {
    /// Wrap an existing mesh node as an overflow-push sink.
    /// `Arc::clone` is cheap; one sink per operator scheduler
    /// is the typical shape.
    pub fn new(mesh: Arc<crate::adapter::net::MeshNode>) -> Self {
        Self { mesh }
    }
}

#[cfg(feature = "cortex")]
#[async_trait]
impl OverflowPushSink for MeshNodeOverflowPushSink {
    async fn push(
        &self,
        hash: [u8; 32],
        size_bytes: u64,
        target_node_id: u64,
    ) -> Result<(), BlobError> {
        // Map an `OverflowPushAck::Rejected(reason)` /
        // `OverflowPushAck::OpenChunkFailed` to a typed
        // BlobError so the controller's `push_errors` counter
        // gets bumped uniformly. `Accepted` returns Ok.
        let ack = self
            .mesh
            .send_overflow_push(target_node_id, hash, size_bytes)
            .await?;
        match ack {
            OverflowPushAck::Accepted => Ok(()),
            OverflowPushAck::Rejected(reason) => Err(BlobError::Backend(format!(
                "overflow push to {target_node_id:#x} rejected: {reason:?}"
            ))),
            OverflowPushAck::OpenChunkFailed => Err(BlobError::Backend(format!(
                "overflow push to {target_node_id:#x} admitted but chunk open failed"
            ))),
        }
    }
}

/// Sink trait for the actual push action. P3 wires the
/// [`MeshNode`]-backed implementation that sends an
/// `OverflowPush` RPC and waits for the durability
/// watermark; P2 ships the trait + a recorder mock for
/// unit tests.
///
/// `push` is fire-once-per-tick per `(hash, target_node_id)`
/// pair — the controller dedups by hash before calling the
/// sink. Idempotent on the receive side anyway (an
/// already-stored chunk is a no-op store).
///
/// [`MeshNode`]: crate::adapter::net::MeshNode
#[async_trait]
pub trait OverflowPushSink: Send + Sync {
    /// Push `hash` (`size_bytes`) to the receive-side peer
    /// identified by `target_node_id`. Returns `Ok(())` when
    /// the wire-side acknowledgement landed; `Err(BlobError)`
    /// when the send failed for any reason (RPC transport
    /// error, receive-side admission rejection, chunk-open
    /// failure). The tick driver aggregates errors into the
    /// `push_errors` counter without disambiguating — wire-
    /// level counters break out per reason.
    async fn push(
        &self,
        hash: [u8; 32],
        size_bytes: u64,
        target_node_id: u64,
    ) -> Result<(), BlobError>;
}

/// Active-overflow controller. Borrows the inputs it needs
/// (local caps, capability index, heat registry, refcount
/// table, config); the controller itself is stateless. The
/// hysteresis state lives in the caller as an `AtomicBool`
/// passed into [`drive_blob_overflow_tick`].
///
/// Lifetimes:
/// - `'a` — the controller's borrows. Typically the operator
///   constructs the controller per tick inside the scheduler
///   loop; the borrows are valid for the lifetime of the
///   tick await.
pub struct BlobOverflowController<'a> {
    /// Local node's capability set. Read for the local
    /// gravity scope (target-selection scope filter) and
    /// for the overflow-enabled self-check inside
    /// [`drive_blob_overflow_tick`] (skip the tick until the
    /// local `dataforts.blob.overflow` tag is visible on the
    /// snapshot — otherwise every push would round-trip an
    /// RPC and come back `Rejected(SenderNotOverflowing)`
    /// while the announce propagates).
    pub local_caps: &'a CapabilitySet,
    /// Fold of peer capability sets. The controller walks
    /// every overflow-enabled peer to score target selection.
    /// Migrated off the legacy CapabilityIndex per Phase 3b.
    pub capability_fold: &'a Fold<CapabilityFold>,
    /// Per-chunk heat registry. The controller walks every
    /// tracked hash, decays each rate to `now`, and ranks
    /// candidates coldest-first.
    pub heat_registry: &'a Arc<parking_lot::Mutex<BlobHeatRegistry>>,
    /// Per-hash refcount + pin table. Candidates are
    /// filtered against this: only `refcount == 0 &&
    /// !pinned` hashes are eligible for push in P2. The
    /// richer "all-references-are-cache" rule (which would
    /// allow shedding a chunk still held by a greedy cache
    /// entry) lands when per-source refcount inspection is
    /// added to [`BlobRefcountTable`].
    pub refcount: &'a BlobRefcountTable,
    /// Operator-tunable knobs. Read for `scope`,
    /// `max_pushes_per_tick`, and the high/low water
    /// thresholds (consumed by the hysteresis state machine).
    pub config: &'a OverflowConfig,
}

impl<'a> BlobOverflowController<'a> {
    /// Construct a controller from borrows. `new` is sugar
    /// for the struct literal — operators that prefer the
    /// builder shape can call this; tests usually use the
    /// literal for clarity.
    pub fn new(
        local_caps: &'a CapabilitySet,
        capability_fold: &'a Fold<CapabilityFold>,
        heat_registry: &'a Arc<parking_lot::Mutex<BlobHeatRegistry>>,
        refcount: &'a BlobRefcountTable,
        config: &'a OverflowConfig,
    ) -> Self {
        Self {
            local_caps,
            capability_fold,
            heat_registry,
            refcount,
            config,
        }
    }

    /// Compute every candidate for this tick — coldest first,
    /// truncated to `config.max_pushes_per_tick`. Convenience
    /// wrapper around [`Self::candidate_batch`] that drops the
    /// `no_target_count` companion when the caller only wants
    /// the push list.
    pub fn candidates(
        &self,
        now: Instant,
        size_for_hash: impl Fn([u8; 32]) -> Option<u64>,
    ) -> Vec<BlobOverflowCandidate> {
        self.candidate_batch(now, size_for_hash).candidates
    }

    /// Compute candidates + the precise `no_target` accounting
    /// for this tick. `size_for_hash` is an operator-supplied
    /// resolver (the controller doesn't know chunk sizes
    /// directly; `MeshBlobAdapter::stat_chunk` or an equivalent
    /// answers this).
    ///
    /// The function:
    ///
    /// 1. Snapshots `(hash, decayed_rate)` from the heat
    ///    registry under a brief read lock.
    /// 2. Filters out pinned hashes + hashes with nonzero
    ///    refcount + hashes whose `size_for_hash` returns
    ///    `None` (controller can't run the disk-gate without
    ///    a size; abstain rather than guess).
    /// 3. Sorts ascending by `(decayed_rate, hash)` — ties
    ///    broken by hash bytes for determinism.
    /// 4. For each candidate hash (in cold-first order),
    ///    walks the capability index for an overflow-
    ///    enabled peer with `disk_free_gb >= ceil(size /
    ///    1 GiB)` matching the local gravity scope; picks
    ///    the peer with the highest `disk_free_gb` (ties
    ///    broken by lowest `node_id`).
    /// 5. Counts a hash as `no_target` only when target
    ///    selection was actually *attempted* and failed —
    ///    hashes past the `max_pushes_per_tick` truncation
    ///    point were never tried and are NOT no-target.
    /// 6. Stops walking once
    ///    `candidates.len() >= config.max_pushes_per_tick`.
    pub fn candidate_batch(
        &self,
        now: Instant,
        size_for_hash: impl Fn([u8; 32]) -> Option<u64>,
    ) -> OverflowCandidateBatch {
        // Step 1: snapshot heat-registry entries.
        let snap: Vec<([u8; 32], f64)> = {
            let guard = self.heat_registry.lock();
            guard
                .iter()
                .map(|(h, c)| {
                    // Compute decayed rate without mutating
                    // the counter — keeps the iteration
                    // read-only so a concurrent fetch path
                    // bumping a different hash isn't
                    // blocked.
                    let elapsed = now.saturating_duration_since(c.last_update());
                    let half_life_s = c.half_life().as_secs_f64();
                    let rate = if half_life_s == 0.0 || c.rate() == 0.0 {
                        c.rate()
                    } else {
                        let half_lives = elapsed.as_secs_f64() / half_life_s;
                        if half_lives > 64.0 {
                            0.0
                        } else {
                            c.rate() * 0.5_f64.powf(half_lives)
                        }
                    };
                    (*h, rate)
                })
                .collect()
        };

        // Step 2: filter on pin / refcount / size. A hash
        // with `refcount > 0` is not pushable in P2 — the
        // per-source refcount split (cache vs fold) is a
        // future refinement.
        let mut filtered: Vec<([u8; 32], f64, u64)> = snap
            .into_iter()
            .filter_map(|(h, rate)| {
                let entry = self.refcount.get(&h)?;
                if entry.pinned {
                    return None;
                }
                if entry.refcount > 0 {
                    return None;
                }
                let size = size_for_hash(h)?;
                Some((h, rate, size))
            })
            .collect();

        // Step 3: stable sort coldest-first. Ties broken by
        // hash bytes for determinism. NaN is impossible
        // here (decayed rate is always finite + non-negative
        // when input is finite, and the heat-counter ensures
        // finite input), so `partial_cmp` is safe.
        filtered.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        // Step 4-5: target selection per hash. `no_target`
        // counts only hashes we ACTUALLY tried — the loop
        // breaks at `max_pushes_per_tick` so the tail of
        // `filtered` is never attempted and must not bump
        // the counter.
        let local_gravity = GravityCapability::from_capability_set(self.local_caps);
        let mut candidates: Vec<BlobOverflowCandidate> = Vec::new();
        let mut no_target_count: usize = 0;
        for (hash, cold_rate, size_bytes) in filtered {
            match self.pick_target(size_bytes, local_gravity.scope) {
                Some((target_node_id, target_caps)) => {
                    candidates.push(BlobOverflowCandidate {
                        hash,
                        size_bytes,
                        target_node_id,
                        target_caps,
                        cold_rate,
                    });
                }
                None => {
                    no_target_count += 1;
                }
            }
            if candidates.len() >= self.config.max_pushes_per_tick {
                break;
            }
        }
        OverflowCandidateBatch {
            candidates,
            no_target_count,
        }
    }

    /// Find the best overflow-receiver peer for a chunk of
    /// `size_bytes`. Filter rule:
    ///
    /// - `cap.blob.storage = true`
    /// - `cap.blob.overflow_enabled = true`
    /// - `cap.blob.disk_free_gb >= ceil(size / 1 GiB)`
    /// - peer's `cap.gravity.scope` covers `local_scope`
    ///   (the local node won't push outside its own scope
    ///   bound)
    /// - peer is not `dataforts:blob-storage-unhealthy`
    ///
    /// Ranking: highest `disk_free_gb` wins (greedy spread
    /// across peers); ties broken by lowest `node_id` for
    /// determinism.
    fn pick_target(
        &self,
        size_bytes: u64,
        local_scope: TopologyScope,
    ) -> Option<(u64, CapabilitySet)> {
        let required_gb = size_bytes.div_ceil(1 << 30);
        let mut best: Option<(u64, u64, CapabilitySet)> = None; // (disk_free_gb, node_id, caps)
        let publishers: Vec<u64> = self
            .capability_fold
            .with_state(|state| state.by_node.keys().copied().collect());
        for node_id in publishers {
            // Synthesize a CapabilitySet from the fold's tag set
            // for `node_id`. The downstream BlobCapability /
            // GravityCapability projections read tags via
            // `Tag::AxisPresent` / `Tag::AxisValue` patterns that
            // round-trip through CapabilitySet::add_tag, so
            // tag-based reads (storage / overflow / scope /
            // disk_total_gb / disk_free_gb) work identically.
            let caps = capability_bridge::synthesize_capability_set(
                self.capability_fold,
                node_id,
            );
            let peer_blob = BlobCapability::from_capability_set(&caps);
            if !peer_blob.storage || !peer_blob.overflow_enabled {
                continue;
            }
            if peer_blob.disk_free_gb < required_gb {
                continue;
            }
            if is_blob_storage_unhealthy(&caps) {
                continue;
            }
            // Local-scope-covers-peer-scope check. We're
            // pushing OUT, so the local node's scope bound
            // is the gate — peers outside our scope can't
            // receive our overflow.
            let peer_gravity = GravityCapability::from_capability_set(&caps);
            if !scope_covers(local_scope, peer_gravity.scope) {
                continue;
            }
            // Update best by disk_free_gb desc, then
            // node_id asc.
            match &best {
                None => best = Some((peer_blob.disk_free_gb, node_id, caps)),
                Some((d, n, _)) => {
                    let is_better = peer_blob.disk_free_gb > *d
                        || (peer_blob.disk_free_gb == *d && node_id < *n);
                    if is_better {
                        best = Some((peer_blob.disk_free_gb, node_id, caps));
                    }
                }
            }
        }
        best.map(|(_, node_id, caps)| (node_id, caps))
    }
}

/// Operator-supplied environmental borrows the
/// [`super::mesh::MeshBlobAdapter::drive_overflow_tick`]
/// convenience method threads through. Decouples the
/// per-tick wiring (capability index, heat registry, sink,
/// local caps, disk stats) from the adapter so the adapter
/// stays a stateless slot-in.
///
/// All fields are borrows. The lifetime parameter `'a` ties
/// the context to a single tick's await; operators
/// reconstruct the context each tick from the live state.
pub struct OverflowTickContext<'a> {
    /// The mesh's capability fold — read for target peer
    /// selection (overflow tag + scope + disk_free + health
    /// gate). Migrated off the legacy CapabilityIndex per
    /// Phase 3b.
    pub capability_fold: &'a Fold<CapabilityFold>,
    /// Per-chunk heat registry. The controller walks every
    /// tracked hash, decays each rate to `now`, and ranks
    /// candidates coldest-first.
    pub heat_registry: &'a Arc<parking_lot::Mutex<BlobHeatRegistry>>,
    /// Sink for the actual push action. Production wiring
    /// uses [`MeshNodeOverflowPushSink`]; tests use a
    /// recorder.
    pub sink: &'a dyn OverflowPushSink,
    /// Local caps snapshot — read for the local gravity
    /// scope (target-selection scope filter).
    pub local_caps: &'a CapabilitySet,
    /// Local disk usage in bytes. Numerator of the
    /// `disk_ratio` hysteresis input.
    pub disk_used_bytes: u64,
    /// Local disk total in bytes. Denominator. `0`
    /// short-circuits the tick.
    pub disk_total_bytes: u64,
}

/// Per-tick observables threaded through
/// [`drive_blob_overflow_tick`]. Bundles the inputs that
/// change every tick (disk stats + hysteresis handle + the
/// clock value) so the tick driver stays a 4-arg signature
/// even as the inputs grow.
///
/// Borrow-only: nothing here is owned. The hysteresis atomic
/// is shared with the adapter's `overflow_active` field
/// (P4); operator-driven tests can wire a fresh
/// `AtomicBool` for isolation. `now` is captured at the
/// tick call site so deterministic-simulation harnesses can
/// inject a fixed `Instant` without mocking the system clock.
pub struct OverflowTickObservation<'a> {
    /// Local disk usage in bytes — the numerator of the
    /// `disk_ratio` hysteresis input. `disk_used > disk_total`
    /// is clamped inside the driver (defense against
    /// misconfiguration).
    pub disk_used_bytes: u64,
    /// Local disk total in bytes — the denominator. `0`
    /// short-circuits the tick to "never fire" (an
    /// unconfigured disk cap shouldn't trigger pushes the
    /// moment any chunk lands).
    pub disk_total_bytes: u64,
    /// Shared hysteresis state. Read at tick start, updated
    /// by [`step_overflow_hysteresis`] to the post-tick
    /// state. Wired to [`super::mesh::MeshBlobAdapter`]'s
    /// `overflow_active` field in the production path.
    pub hysteresis_active: &'a AtomicBool,
    /// Clock value used to decay heat-registry rates. Pass
    /// `Instant::now()` in production; tests can fix this
    /// for reproducibility.
    pub now: Instant,
}

/// Drive one overflow tick.
///
/// Composes the hysteresis state machine + the controller's
/// candidate computation + the sink's push action into a
/// single async entry point. Operators call this from a
/// periodic task at `config.tick_interval_ms` cadence; the
/// function is idempotent against repeated calls (the
/// hysteresis state filters out spurious ticks).
///
/// Returns a [`BlobOverflowTickReport`] with per-reason
/// counters. Operators aggregate the report into Prometheus
/// metrics via
/// [`super::metrics::BlobMetrics::record_overflow_tick`].
pub async fn drive_blob_overflow_tick(
    controller: &BlobOverflowController<'_>,
    sink: &dyn OverflowPushSink,
    observation: OverflowTickObservation<'_>,
    size_for_hash: impl Fn([u8; 32]) -> Option<u64>,
) -> BlobOverflowTickReport {
    let OverflowTickObservation {
        disk_used_bytes,
        disk_total_bytes,
        hysteresis_active,
        now,
    } = observation;
    let mut report = BlobOverflowTickReport::default();
    let disk_ratio = if disk_total_bytes == 0 {
        // Adapter without a configured disk-cap reports
        // `disk_total = 0`; the safe default is "never
        // fire" rather than "always fire" (the latter
        // would push the moment any chunk lands on a
        // mis-configured node).
        0.0
    } else {
        disk_used_bytes as f64 / disk_total_bytes as f64
    };
    report.disk_ratio_at_start = disk_ratio;
    report.was_active_at_start = hysteresis_active.load(Ordering::Relaxed);

    let fire = step_overflow_hysteresis(
        hysteresis_active,
        disk_ratio,
        controller.config.high_water_ratio,
        controller.config.low_water_ratio,
    );
    report.is_active_at_end = fire;

    // Master switch gate. Even if disk crossed the high
    // water mark, a disabled overflow config means we
    // never push. Pin this so toggling `enabled = false`
    // is a hard stop.
    if !controller.config.enabled || !fire {
        report.disk_ratio_at_end = disk_ratio;
        return report;
    }

    // Sender-side self-check (plan § "Open design questions"
    // #5): the controller's `config.enabled` says the operator
    // wants overflow on, but the wire-level contract is
    // gated on `cap.blob.overflow` propagating through the
    // capability index. If the local node hasn't yet
    // advertised the tag (announce hasn't fired since
    // `set_overflow_enabled(true)`, or `local_caps` was
    // never rebuilt), every push would round-trip an RPC
    // just to get rejected `SenderNotOverflowing` by every
    // peer. Skip the tick cleanly until the tag is visible
    // on the sender's own caps snapshot; the next
    // `announce_capabilities` rebroadcast resolves the race.
    let local_blob = BlobCapability::from_capability_set(controller.local_caps);
    if !local_blob.overflow_enabled {
        tracing::debug!(
            "blob overflow: master switch on but local cap.blob.overflow not yet advertised; \
             skipping tick until announce_capabilities propagates the tag"
        );
        report.disk_ratio_at_end = disk_ratio;
        return report;
    }

    // Compute candidates in one pass. `candidate_batch`
    // tracks the no-target count inside its target-selection
    // loop, so truncated-by-`max_pushes_per_tick` hashes never
    // bump the counter (they were never tried).
    let batch = controller.candidate_batch(now, &size_for_hash);
    report.rejected_no_target = batch.no_target_count as u64;

    // Fire pushes. `max_pushes_per_tick = 0` is a valid
    // "trigger only, no real pushes" mode — the candidates
    // list will be empty so we drop straight through.
    for candidate in batch.candidates {
        match sink
            .push(
                candidate.hash,
                candidate.size_bytes,
                candidate.target_node_id,
            )
            .await
        {
            Ok(()) => {
                report.admitted += 1;
                report.pushed_bytes = report.pushed_bytes.saturating_add(candidate.size_bytes);
            }
            Err(e) => {
                tracing::trace!(
                    error = ?e,
                    hash = ?candidate.hash,
                    target = candidate.target_node_id,
                    "blob overflow: push failed; counted"
                );
                report.push_errors += 1;
            }
        }
    }

    // disk_ratio_at_end stays equal to start in P2; P3
    // wires the durability watermark + a fresh disk-stat
    // poll to surface the post-tick reclaim.
    report.disk_ratio_at_end = disk_ratio;
    report
}

/// `local` scope covers `peer` iff a push from a node with
/// scope `local` can land on a node with scope `peer`. The
/// rule mirrors the migration controller's
/// `scope_at_least_as_narrow`: a Zone-scoped local node can
/// push to Zone / Region / Mesh peers (the peer's scope
/// covers the local one), but a Mesh-scoped local node
/// can't push to a Node-scoped peer (the peer's scope is
/// narrower and won't accept the cross-scope artifact).
///
/// `local == Mesh` covers any peer scope (mesh is the widest
/// scope — any peer is reachable). `local == Node` is the
/// degenerate case: only same-node receivers qualify; in
/// practice this is the "never push" config.
fn scope_covers(local: TopologyScope, peer: TopologyScope) -> bool {
    use TopologyScope::*;
    matches!(
        (local, peer),
        (Mesh, _) | (Region, Region | Mesh) | (Zone, Zone | Region | Mesh) | (Node, Node)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilityAnnouncement;
    use crate::adapter::net::dataforts::gravity::BlobHeatRegistry;
    use crate::adapter::net::identity::EntityId;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    fn hex64(byte: u8) -> ([u8; 32], String) {
        let mut h = [0u8; 32];
        h.fill(byte);
        let hex: String = h.iter().map(|b| format!("{:02x}", b)).collect();
        (h, hex)
    }

    /// Build a `CapabilitySet` for an overflow-enabled peer
    /// with `disk_free_gb` headroom and mesh-wide gravity.
    fn overflow_peer_caps(disk_free_gb: u64) -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag(format!("dataforts.blob.disk_free_gb={}", disk_free_gb))
            .add_tag("dataforts.blob.overflow")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.gravity.proximity=128")
    }

    /// Local node config: overflow-enabled, mesh scope.
    fn overflow_enabled_local_caps() -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.overflow")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.gravity.proximity=128")
    }

    /// One recorded push call captured by the mock sink:
    /// `(hash, size, target_node_id)`. Named so the type
    /// stays readable in the `OverflowPushRecorder` field
    /// + the `calls()` snapshot return.
    type RecordedPushCall = ([u8; 32], u64, u64);

    /// Shared call-log container the recorder mutates from
    /// inside an `&self` push method. `Arc<Mutex<Vec<_>>>`
    /// across clones so a test can hand a clone to the
    /// sink + inspect from the test body.
    type RecordedCallLog = Arc<parking_lot::Mutex<Vec<RecordedPushCall>>>;

    /// Recorder sink — records every push call's
    /// `(hash, size, target)` tuple. The `fail_count` toggle
    /// lets tests inject sink errors to exercise the
    /// `push_errors` counter.
    struct OverflowPushRecorder {
        calls: RecordedCallLog,
        fail_count: Arc<AtomicU64>,
    }

    impl OverflowPushRecorder {
        fn new() -> Self {
            Self {
                calls: Arc::new(parking_lot::Mutex::new(Vec::new())),
                fail_count: Arc::new(AtomicU64::new(0)),
            }
        }

        fn calls(&self) -> Vec<RecordedPushCall> {
            self.calls.lock().clone()
        }
    }

    #[async_trait]
    impl OverflowPushSink for OverflowPushRecorder {
        async fn push(
            &self,
            hash: [u8; 32],
            size_bytes: u64,
            target_node_id: u64,
        ) -> Result<(), BlobError> {
            if self.fail_count.load(Ordering::Relaxed) > 0 {
                self.fail_count.fetch_sub(1, Ordering::Relaxed);
                return Err(BlobError::NotFound("simulated push failure".to_string()));
            }
            self.calls.lock().push((hash, size_bytes, target_node_id));
            Ok(())
        }
    }

    /// Build a `BlobHeatRegistry` with a list of `(hash, rate)`
    /// pairs at the supplied `now` instant. Each entry is
    /// freshly seeded and bumped to its target rate via direct
    /// counter access.
    fn heat_registry_with(
        now: Instant,
        entries: &[([u8; 32], f64)],
    ) -> Arc<parking_lot::Mutex<BlobHeatRegistry>> {
        let mut reg = BlobHeatRegistry::new();
        for (hash, rate) in entries {
            let counter = reg.entry_mut(*hash, Duration::from_secs(60), now);
            // Bump `*rate` times to reach the target rate;
            // each bump adds 1.0 after decay. `now` is the
            // same for every bump so no decay happens.
            for _ in 0..(*rate as usize) {
                counter.bump(now);
            }
        }
        Arc::new(parking_lot::Mutex::new(reg))
    }

    /// Refcount table where every supplied hash is
    /// `refcount = 0, !pinned` — eligible for overflow.
    /// `store_observed` is the cheapest way to land an
    /// entry at refcount 0 + a recorded `first_seen` time.
    fn refcount_with_zero(hashes: &[[u8; 32]], now_ms: u64) -> BlobRefcountTable {
        let rc = BlobRefcountTable::new();
        for h in hashes {
            rc.store_observed(*h, 0, now_ms);
        }
        rc
    }

    fn cap_index_with(
        peers: &[(u64, [u8; 32], CapabilitySet)],
    ) -> (Fold<CapabilityFold>, CapabilityIndex) {
        let index = CapabilityIndex::new();
        let fold = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
        for (idx, (node_id, entity_bytes, caps)) in peers.iter().enumerate() {
            let entity = EntityId::from_bytes(*entity_bytes);
            capability_bridge::dual_apply(
                &fold,
                &index,
                CapabilityAnnouncement::new(*node_id, entity, 1 + idx as u64, caps.clone()),
            );
        }
        (fold, index)
    }

    // ========================================================================
    // step_overflow_hysteresis (pure-logic state machine)
    // ========================================================================

    #[test]
    fn hysteresis_fires_above_high_water() {
        let active = AtomicBool::new(false);
        assert!(step_overflow_hysteresis(&active, 0.90, 0.85, 0.70));
        assert!(active.load(Ordering::Relaxed));
    }

    #[test]
    fn hysteresis_clears_below_low_water() {
        let active = AtomicBool::new(true);
        assert!(!step_overflow_hysteresis(&active, 0.65, 0.85, 0.70));
        assert!(!active.load(Ordering::Relaxed));
    }

    #[test]
    fn hysteresis_holds_state_in_band() {
        // Between low (0.70) and high (0.85): hold prior
        // state regardless of which boundary was last
        // crossed.
        let active = AtomicBool::new(true);
        assert!(step_overflow_hysteresis(&active, 0.80, 0.85, 0.70));
        assert!(active.load(Ordering::Relaxed));

        let inactive = AtomicBool::new(false);
        assert!(!step_overflow_hysteresis(&inactive, 0.80, 0.85, 0.70));
        assert!(!inactive.load(Ordering::Relaxed));
    }

    #[test]
    fn hysteresis_boundary_inclusive() {
        // disk_ratio == high_water fires (>=);
        // disk_ratio == low_water clears (<=).
        let active = AtomicBool::new(false);
        assert!(step_overflow_hysteresis(&active, 0.85, 0.85, 0.70));
        let active2 = AtomicBool::new(true);
        assert!(!step_overflow_hysteresis(&active2, 0.70, 0.85, 0.70));
    }

    // ========================================================================
    // BlobOverflowController::candidates
    // ========================================================================

    #[test]
    fn controller_candidates_returns_coldest_first() {
        // Three hashes with rates 0.0 / 1.0 / 5.0 →
        // ordering A (cold) / B (warm) / C (hot).
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let (b, _) = hex64(0xBB);
        let (c, _) = hex64(0xCC);
        let heat = heat_registry_with(now, &[(a, 0.0), (b, 1.0), (c, 5.0)]);
        let refcount = refcount_with_zero(&[a, b, c], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);

        let cands = controller.candidates(now, |_| Some(1024));
        assert_eq!(cands.len(), 3);
        // a (rate 0.0) first; c (rate 5.0) last.
        assert_eq!(cands[0].hash, a);
        assert_eq!(cands[2].hash, c);
    }

    #[test]
    fn controller_skips_pinned_hashes() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let (b, _) = hex64(0xBB);
        let heat = heat_registry_with(now, &[(a, 0.0), (b, 0.0)]);
        let refcount = BlobRefcountTable::new();
        refcount.store_observed(a, 0, 1_000_000);
        refcount.pin(a, 1_000_000);
        refcount.store_observed(b, 0, 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);

        let cands = controller.candidates(now, |_| Some(1024));
        // Pinned `a` skipped; only unpinned `b` surfaces.
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].hash, b);
    }

    #[test]
    fn controller_skips_hashes_with_nonzero_refcount() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = BlobRefcountTable::new();
        refcount.incr(a, 1_000_000); // refcount = 1, not droppable
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        assert!(controller.candidates(now, |_| Some(1024)).is_empty());
    }

    #[test]
    fn controller_picks_highest_disk_free_target() {
        // Two peers, both overflow-enabled. Peer 99 has 40
        // GiB free; peer 88 has 80 GiB free. Greedy spread
        // → peer 88 wins.
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer_low = (99u64, [0x11; 32], overflow_peer_caps(40));
        let peer_high = (88u64, [0x22; 32], overflow_peer_caps(80));
        let (fold, _index) = cap_index_with(&[peer_low, peer_high]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);

        let cands = controller.candidates(now, |_| Some(1024));
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].target_node_id, 88);
    }

    #[test]
    fn controller_skips_peers_without_overflow_tag() {
        // Peer has storage + disk + scope BUT no overflow
        // tag → not a valid target.
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let no_overflow_peer_caps = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=80")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.gravity.proximity=128");
        let peer = (99u64, [0x11; 32], no_overflow_peer_caps);
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        assert!(controller.candidates(now, |_| Some(1024)).is_empty());
    }

    #[test]
    fn controller_skips_peers_with_insufficient_disk() {
        // Peer has 1 GiB free; we're pushing a 4 GiB blob →
        // no target.
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(1));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let four_gib: u64 = 4 * (1 << 30);
        assert!(controller.candidates(now, |_| Some(four_gib)).is_empty());
    }

    #[test]
    fn controller_truncates_to_max_pushes_per_tick() {
        let now = Instant::now();
        let hashes: Vec<[u8; 32]> = (0..5).map(|i| hex64(i as u8).0).collect();
        let entries: Vec<([u8; 32], f64)> = hashes.iter().map(|h| (*h, 0.0)).collect();
        let heat = heat_registry_with(now, &entries);
        let refcount = refcount_with_zero(&hashes, 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 2,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let cands = controller.candidates(now, |_| Some(1024));
        assert_eq!(
            cands.len(),
            2,
            "max_pushes_per_tick caps the candidate list"
        );
    }

    // ========================================================================
    // drive_blob_overflow_tick — end-to-end against the recorder sink
    // ========================================================================

    #[tokio::test]
    async fn tick_no_op_when_below_low_water() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        // disk_ratio = 0.50 — below low_water (0.70).
        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            OverflowTickObservation {
                disk_used_bytes: 500,
                disk_total_bytes: 1000,
                hysteresis_active: &active,
                now,
            },
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.admitted, 0);
        assert!(!report.is_active_at_end);
        assert_eq!(sink.calls().len(), 0);
    }

    #[tokio::test]
    async fn tick_fires_above_high_water_and_pushes_to_recorder() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        // disk_ratio = 0.90 — above high_water (0.85).
        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            OverflowTickObservation {
                disk_used_bytes: 900,
                disk_total_bytes: 1000,
                hysteresis_active: &active,
                now,
            },
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.admitted, 1);
        assert!(report.is_active_at_end);
        assert_eq!(report.pushed_bytes, 1024);
        let calls = sink.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, a);
        assert_eq!(calls[0].2, 99);
    }

    #[tokio::test]
    async fn tick_master_switch_off_skips_pushes_even_above_high_water() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: false, // master switch off
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            OverflowTickObservation {
                disk_used_bytes: 900,
                disk_total_bytes: 1000,
                hysteresis_active: &active,
                now,
            },
            |_| Some(1024),
        )
        .await;
        // Hysteresis still transitions (the disk state machine
        // is independent of the master switch — operators
        // dashboarding the active gauge should see it climb
        // before they enable). Pushes don't fire.
        assert!(report.is_active_at_end);
        assert_eq!(report.admitted, 0);
        assert_eq!(sink.calls().len(), 0);
    }

    #[tokio::test]
    async fn tick_records_push_errors_when_sink_fails() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();
        sink.fail_count.store(1, Ordering::Relaxed);

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            OverflowTickObservation {
                disk_used_bytes: 900,
                disk_total_bytes: 1000,
                hysteresis_active: &active,
                now,
            },
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.admitted, 0);
        assert_eq!(report.push_errors, 1);
        assert_eq!(report.pushed_bytes, 0);
    }

    #[tokio::test]
    async fn tick_records_no_target_when_no_overflow_enabled_peer() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        // Peer has no overflow tag.
        let no_overflow_peer_caps = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=80")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh");
        let peer = (99u64, [0x11; 32], no_overflow_peer_caps);
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            OverflowTickObservation {
                disk_used_bytes: 900,
                disk_total_bytes: 1000,
                hysteresis_active: &active,
                now,
            },
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.admitted, 0);
        assert_eq!(report.rejected_no_target, 1);
        assert_eq!(sink.calls().len(), 0);
    }

    #[tokio::test]
    async fn tick_skips_when_local_overflow_tag_not_advertised() {
        // `OverflowConfig.enabled = true` but `local_caps`
        // doesn't carry `dataforts.blob.overflow` — the
        // operator flipped the master switch on the adapter
        // but `announce_capabilities` hasn't rebuilt the
        // local caps snapshot yet. Sender-side self-check
        // (plan § Open design Q #5) must skip the tick:
        // every push would round-trip an RPC and come back
        // `Rejected(SenderNotOverflowing)`, wasting wire
        // and bumping `push_errors` without making progress.
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        // Local caps WITHOUT `dataforts.blob.overflow` tag.
        let local = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.gravity.proximity=128");
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            OverflowTickObservation {
                disk_used_bytes: 900,
                disk_total_bytes: 1000,
                hysteresis_active: &active,
                now,
            },
            |_| Some(1024),
        )
        .await;
        // Hysteresis still flips (disk genuinely crossed
        // the high-water mark — the gauge should reflect
        // that), but no pushes fire and no rejections
        // count.
        assert!(report.is_active_at_end);
        assert_eq!(report.admitted, 0);
        assert_eq!(report.push_errors, 0);
        assert_eq!(report.rejected_no_target, 0);
        assert_eq!(sink.calls().len(), 0);
    }

    #[tokio::test]
    async fn tick_no_target_excludes_truncated_hashes() {
        // Pre-pick count exceeds `max_pushes_per_tick`: every
        // attempted hash finds a target (so `rejected_no_target`
        // must stay 0), the truncated tail is never tried, and
        // exactly `max_pushes_per_tick` pushes fire. Regression
        // against the prior `pre_pick - candidates.len()` math
        // that conflated truncation with no-target.
        let now = Instant::now();
        let hashes: Vec<[u8; 32]> = (0..5).map(|i| hex64(i as u8).0).collect();
        let entries: Vec<([u8; 32], f64)> = hashes.iter().map(|h| (*h, 0.0)).collect();
        let heat = heat_registry_with(now, &entries);
        let refcount = refcount_with_zero(&hashes, 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(80));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 2,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            OverflowTickObservation {
                disk_used_bytes: 900,
                disk_total_bytes: 1000,
                hysteresis_active: &active,
                now,
            },
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.admitted, 2);
        assert_eq!(
            report.rejected_no_target, 0,
            "truncated hashes (never attempted) must NOT bump rejected_no_target"
        );
        assert_eq!(sink.calls().len(), 2);
    }

    #[tokio::test]
    async fn tick_no_target_counts_only_attempted_failures() {
        // Mix: two hashes need 4 GiB; peer offers 80 GiB so
        // both find targets. Two more hashes need 100 GiB; no
        // peer can take them → both bump `rejected_no_target`.
        // With max_pushes_per_tick=3 the loop stops after the
        // 3rd successful push attempt, so we should see
        // admitted=2 and no_target=2 (both attempted) — NOT a
        // capped diff that would mis-attribute the truncation.
        let now = Instant::now();
        // Order by hash bytes (sort is coldest-first, ties by
        // hash). Use distinct first bytes so order is
        // predictable.
        let (small1, _) = hex64(0x01);
        let (big1, _) = hex64(0x02);
        let (small2, _) = hex64(0x03);
        let (big2, _) = hex64(0x04);
        let heat = heat_registry_with(
            now,
            &[(small1, 0.0), (big1, 0.0), (small2, 0.0), (big2, 0.0)],
        );
        let refcount = refcount_with_zero(&[small1, big1, small2, big2], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(80));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 3,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        let size_for_hash = move |h: [u8; 32]| -> Option<u64> {
            if h == big1 || h == big2 {
                Some(100 * (1 << 30)) // 100 GiB — over peer's free
            } else {
                Some(1024) // tiny
            }
        };
        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            OverflowTickObservation {
                disk_used_bytes: 900,
                disk_total_bytes: 1000,
                hysteresis_active: &active,
                now,
            },
            size_for_hash,
        )
        .await;
        // 2 small hashes fit (admitted), 2 big hashes have no
        // target. The loop never hits the `max=3` cap because
        // there are only 4 candidates total.
        assert_eq!(report.admitted, 2);
        assert_eq!(report.rejected_no_target, 2);
    }

    #[tokio::test]
    async fn tick_zero_disk_total_never_fires() {
        // disk_total = 0 → ratio = 0.0 → always below high
        // water. Defends against misconfigured nodes that
        // would push the moment any chunk lands.
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let (fold, _index) = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &fold,&heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            OverflowTickObservation {
                disk_used_bytes: 500,
                disk_total_bytes: 0, // disk_total = 0
                hysteresis_active: &active,
                now,
            },
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.disk_ratio_at_start, 0.0);
        assert!(!report.is_active_at_end);
        assert_eq!(sink.calls().len(), 0);
    }

    // ========================================================================
    // scope_covers
    // ========================================================================

    #[test]
    fn scope_covers_mesh_covers_everything() {
        use TopologyScope::*;
        for peer in [Node, Zone, Region, Mesh] {
            assert!(scope_covers(Mesh, peer));
        }
    }

    #[test]
    fn scope_covers_zone_does_not_cover_node() {
        // Zone-scoped sender can't push to a Node-scoped
        // peer (peer is narrower; won't accept cross-scope).
        assert!(!scope_covers(TopologyScope::Zone, TopologyScope::Node));
        // But Zone-scoped sender CAN push to Zone / Region /
        // Mesh peers (peer's scope covers the sender's).
        assert!(scope_covers(TopologyScope::Zone, TopologyScope::Zone));
        assert!(scope_covers(TopologyScope::Zone, TopologyScope::Region));
        assert!(scope_covers(TopologyScope::Zone, TopologyScope::Mesh));
    }

    // ========================================================================
    // Wire types (P3) — postcard round-trip
    //
    // The receive side decodes `OverflowPush` from the nRPC payload and
    // encodes `OverflowPushAck` back; the sender's
    // `MeshNodeOverflowPushSink` does the inverse. Verify the encode +
    // decode are total inverses for every typed variant so a sender +
    // receiver on different builds can't observe wire-format divergence.
    // ========================================================================

    #[test]
    fn overflow_push_request_round_trips_postcard() {
        let req = OverflowPush {
            blob_hash: [0xAA; 32],
            size_bytes: 4 * (1 << 20),
            sender_node_id: 0xDEAD_BEEF_u64,
        };
        let bytes = postcard::to_allocvec(&req).expect("encode");
        let decoded: OverflowPush = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, req);
    }

    #[test]
    fn overflow_push_ack_accepted_round_trips() {
        let ack = OverflowPushAck::Accepted;
        let bytes = postcard::to_allocvec(&ack).expect("encode");
        let decoded: OverflowPushAck = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, ack);
    }

    #[test]
    fn overflow_push_ack_rejected_carries_typed_reason() {
        // Every `OverflowReject` variant round-trips inside
        // the ack. Operators dashboard the typed reason on
        // the sender side; the wire form must preserve it.
        for reason in [
            OverflowReject::NoStorageCap,
            OverflowReject::NotParticipating,
            OverflowReject::SenderNotOverflowing,
            OverflowReject::Unhealthy,
            OverflowReject::ScopeMismatch,
            OverflowReject::InsufficientDisk,
        ] {
            let ack = OverflowPushAck::Rejected(reason);
            let bytes = postcard::to_allocvec(&ack).expect("encode");
            let decoded: OverflowPushAck = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, ack, "ack with {:?} must round-trip", reason);
        }
    }

    #[test]
    fn overflow_push_ack_open_chunk_failed_round_trips() {
        let ack = OverflowPushAck::OpenChunkFailed;
        let bytes = postcard::to_allocvec(&ack).expect("encode");
        let decoded: OverflowPushAck = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, ack);
    }

    #[test]
    fn overflow_push_service_name_is_stable() {
        // Pin the wire-level service-name token. A change here
        // would silently break sender/receiver compatibility
        // across builds (both pieces are gated by feature flag
        // but ship in the same crate).
        assert_eq!(OVERFLOW_PUSH_SERVICE, "dataforts.blob.overflow_push");
    }
}
