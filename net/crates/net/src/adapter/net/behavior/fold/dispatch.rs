//! Inbound dispatch — route envelope bytes to the right
//! [`Fold<K>`] by `kind` u16, verify the signature, hand off to
//! the typed apply path.
//!
//! The [`FoldRegistry`] is the type-erased entry point. Each
//! [`Fold<K>`] is wrapped in a [`FoldDispatchAdapter<K>`] that
//! implements the non-generic [`FoldDispatch`] trait by decoding
//! + verifying the envelope, cross-checking the decoded `kind`
//! against the adapter's [`FoldKind::KIND_ID`] (catches
//! misregistered folds and crossed-channel publishes), and
//! calling [`Fold::apply`] on the verified envelope.
//!
//! The registry holds an `Arc<dyn FoldDispatch>` per registered
//! `kind`. The dispatch hot path takes one `RwLock<HashMap>` read
//! lock for the lookup; the per-fold apply takes its own write
//! lock internally.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use super::metrics::FoldStats;
use super::state::ApplyOutcome;
use super::wire::SignedAnnouncement;
use super::wire::WireError;
use super::{Fold, FoldKind};
use crate::adapter::net::identity::EntityId;

/// Wire subprotocol slot for fold-channel traffic. The per-kind
/// demux happens inside the [`FoldRegistry`] (routing on the
/// envelope's `kind` u16), so one subprotocol covers every
/// `FoldKind`. Slots `0x1001..=0x10FF` are reserved for parallel
/// fold envelope shapes if a future design needs one.
pub const SUBPROTOCOL_FOLD: u16 = 0x1000;

/// Type-erased view of a single [`Fold<K>`] instance, suitable
/// for storage in a `HashMap<u16, Arc<dyn FoldDispatch>>`.
/// Implemented by [`FoldDispatchAdapter<K>`] for every concrete
/// `K: FoldKind`.
pub trait FoldDispatch: Send + Sync {
    /// `KIND_ID` of the wrapped fold. Returned by the adapter
    /// so the registry can cross-check on `register` and the
    /// dispatch path can reject envelopes whose decoded `kind`
    /// disagrees with the routing key.
    fn kind_id(&self) -> u16;

    /// Decode + verify + apply. Returns the apply outcome so
    /// metrics / audit can attribute the result; surfaces
    /// `WireError` for codec / verification failures and wraps
    /// any apply-side `FoldError` via `WireError::Apply`.
    fn dispatch(
        &self,
        bytes: &[u8],
        publisher: &crate::adapter::net::identity::EntityId,
    ) -> Result<ApplyOutcome, WireError>;

    /// Type-erased [`Fold::stats`]. The operator surface
    /// aggregates these across the registry via
    /// [`FoldRegistry::stats`] so a single `net fold list` call
    /// returns one row per registered fold.
    fn stats(&self) -> FoldStats;
}

/// Adapter that lifts a typed [`Fold<K>`] into the non-generic
/// [`FoldDispatch`] trait. Holds an `Arc<Fold<K>>` so multiple
/// dispatchers (and the application code that queries the fold
/// directly) share the same instance.
pub struct FoldDispatchAdapter<K: FoldKind> {
    fold: Arc<Fold<K>>,
}

impl<K: FoldKind> FoldDispatchAdapter<K> {
    /// Wrap a typed fold for registry insertion.
    pub fn new(fold: Arc<Fold<K>>) -> Self {
        Self { fold }
    }

    /// Borrow the underlying fold. Useful for tests that
    /// register a fold via the registry but want to inspect
    /// state through the typed API.
    pub fn fold(&self) -> &Arc<Fold<K>> {
        &self.fold
    }
}

impl<K: FoldKind> FoldDispatch for FoldDispatchAdapter<K> {
    fn kind_id(&self) -> u16 {
        K::KIND_ID
    }

    fn stats(&self) -> FoldStats {
        self.fold.stats()
    }

    fn dispatch(
        &self,
        bytes: &[u8],
        publisher: &crate::adapter::net::identity::EntityId,
    ) -> Result<ApplyOutcome, WireError> {
        // Decode + verify in one shot. `decode_and_verify` runs
        // the postcard decode, the length / placeholder /
        // public-key checks, and the Ed25519 verify; the rest of
        // this function operates on a known-good envelope.
        let ann = SignedAnnouncement::<K::Payload>::decode_and_verify(bytes, publisher)?;

        // Cross-check the envelope's `kind` field against the
        // wrapped fold's `KIND_ID`. The registry routed us here
        // by the wire `kind` byte, so a mismatch means either
        // (a) the registry was constructed wrong (e.g. a fold
        // was registered under the wrong key) or (b) the
        // envelope was hand-crafted to lie about its kind.
        // Either way, refusing the apply is the safe move; the
        // signature already verified against the publisher so
        // we surface the mismatch back to the caller for logging.
        if ann.kind != K::KIND_ID {
            return Err(WireError::KindMismatch {
                got: ann.kind,
                expected: K::KIND_ID,
            });
        }

        Ok(self.fold.apply(ann)?)
    }
}

/// Registry of [`FoldDispatch`] adapters keyed by
/// [`FoldKind::KIND_ID`]. The central connection between an
/// inbound channel message (raw bytes + publisher identity) and
/// the right fold's apply path. Construct typed [`Fold<K>`]
/// instances, wrap each in a [`FoldDispatchAdapter<K>`], and
/// register them here.
pub struct FoldRegistry {
    folds: RwLock<HashMap<u16, Arc<dyn FoldDispatch>>>,
}

impl FoldRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            folds: RwLock::new(HashMap::new()),
        }
    }

    /// Register a typed fold under its [`FoldKind::KIND_ID`].
    /// Returns the previously-registered dispatcher under the
    /// same kind if any, so callers that legitimately want to
    /// replace a fold (e.g. swap a new index implementation in
    /// during operator-driven reconfiguration) can drop the
    /// old one cleanly.
    pub fn register<K: FoldKind>(&self, fold: Arc<Fold<K>>) -> Option<Arc<dyn FoldDispatch>> {
        let adapter = Arc::new(FoldDispatchAdapter::new(fold));
        self.folds
            .write()
            .insert(K::KIND_ID, adapter as Arc<dyn FoldDispatch>)
    }

    /// Remove a fold by kind. Returns the dropped dispatcher if
    /// one was registered.
    pub fn deregister(&self, kind: u16) -> Option<Arc<dyn FoldDispatch>> {
        self.folds.write().remove(&kind)
    }

    /// Number of registered folds.
    pub fn len(&self) -> usize {
        self.folds.read().len()
    }

    /// Whether the registry has no folds registered.
    pub fn is_empty(&self) -> bool {
        self.folds.read().is_empty()
    }

    /// Look up a registered dispatcher by kind. Used by tests
    /// and by the channel-integration adapter; the hot path uses
    /// [`Self::dispatch`] directly.
    pub fn get(&self, kind: u16) -> Option<Arc<dyn FoldDispatch>> {
        self.folds.read().get(&kind).cloned()
    }

    /// Aggregate [`FoldStats`] across every registered fold.
    /// The operator surface (`net fold list`, the Deck FOLDS
    /// panel) calls this once per sample tick. Returns in
    /// unspecified order; callers that want a canonical sort sort
    /// themselves.
    pub fn stats(&self) -> Vec<FoldStats> {
        self.folds
            .read()
            .values()
            .map(|adapter| adapter.stats())
            .collect()
    }

    /// Dispatch an inbound wire envelope to the right fold.
    ///
    /// The dispatch is two-step:
    /// 1. [`peek_kind`] reads the leading `kind: u16` varint to
    ///    pick the right adapter. This is unavoidable: the per-
    ///    fold adapter is typed on `K::Payload`, so we can't run
    ///    the full envelope decode until we know `K`.
    /// 2. The matched adapter runs the full
    ///    [`SignedAnnouncement::decode_and_verify`] (which also
    ///    re-reads the `kind` field as part of the struct
    ///    decode) and then `Fold::apply`.
    ///
    /// The leading varint thus pays for itself twice — once for
    /// routing, once during the typed decode — but the cost is
    /// ~10 ns of postcard varint work next to a ~50 µs Ed25519
    /// verify on the same envelope. Worth flagging here so
    /// future readers don't chase it as a hot-path concern.
    pub fn dispatch(
        &self,
        bytes: &[u8],
        publisher: &crate::adapter::net::identity::EntityId,
    ) -> Result<ApplyOutcome, DispatchError> {
        let kind = peek_kind(bytes).ok_or(DispatchError::Truncated)?;
        let adapter = self.get(kind).ok_or(DispatchError::UnknownKind(kind))?;
        adapter
            .dispatch(bytes, publisher)
            .map_err(DispatchError::Wire)
    }
}

impl Default for FoldRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Hook the mesh's inbound channel-dispatch path uses to route
/// fold announcements. `mesh.rs::dispatch_packet` installs an
/// `Arc<dyn FoldChannelRouter>` (typically a [`FoldRegistry`])
/// and routes every event from a `SUBPROTOCOL_FOLD` packet
/// through it.
///
/// The trait abstracts the registry away from the mesh so tests
/// can stub the router with a counting / inspecting impl.
/// `publisher` is the [`EntityId`] resolved at dispatch time
/// from the inbound session's `node_id` via the mesh's
/// `peer_entity_ids` map; the router uses it to verify the
/// announcement's signature.
pub trait FoldChannelRouter: Send + Sync {
    /// Route one wire envelope to the right fold. Errors are
    /// surfaced so the mesh dispatch arm can log + bump metrics;
    /// the mesh never lets a router error escape into the rest
    /// of the inbound pipeline (single-packet failures must not
    /// take down the dispatch loop).
    fn try_route(&self, publisher: &EntityId, bytes: &[u8]) -> Result<ApplyOutcome, DispatchError>;

    /// Aggregated [`FoldStats`] for every fold the router
    /// addresses. The operator surface (`net fold list`, the
    /// Deck FOLDS panel, the Prometheus exporter) calls into
    /// the router-trait object to read stats without knowing
    /// the underlying concrete type. Implementations that
    /// don't track per-fold stats return an empty `Vec`.
    fn stats(&self) -> Vec<FoldStats>;
}

impl FoldChannelRouter for FoldRegistry {
    fn try_route(&self, publisher: &EntityId, bytes: &[u8]) -> Result<ApplyOutcome, DispatchError> {
        self.dispatch(bytes, publisher)
    }

    fn stats(&self) -> Vec<FoldStats> {
        FoldRegistry::stats(self)
    }
}

/// Top-level errors the [`FoldRegistry::dispatch`] surfaces.
/// Distinct from [`WireError`] because the registry layer
/// surfaces routing-shaped failures (no fold for this kind,
/// truncated envelope) separately from per-fold codec /
/// verification errors.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    /// Envelope was shorter than the 1-3 byte postcard varint
    /// the wire `kind` field occupies (an empty buffer, or a
    /// continuation-byte-promised followup that's missing).
    #[error("envelope truncated before kind varint completes")]
    Truncated,

    /// No fold registered for the envelope's `kind`. The
    /// dispatch layer logs + drops; the `kind` is surfaced so
    /// operator dashboards can pick up "publisher is on the
    /// wrong wire schema."
    #[error("no fold registered for kind {0:#06x}")]
    UnknownKind(u16),

    /// Per-fold codec / verification / apply error. Wraps the
    /// underlying [`WireError`] so the caller can pattern-match
    /// against the specific failure mode.
    #[error("wire / verify / apply failed: {0}")]
    Wire(#[from] WireError),
}

/// Read the wire `kind: u16` varint from the head of an
/// envelope buffer. Returns `None` for buffers that don't
/// carry a complete varint at the head — the registry surfaces
/// that as `DispatchError::Truncated`.
///
/// Uses `postcard::take_from_bytes` so the varint shape stays in
/// lockstep with whatever postcard version the codec uses.
fn peek_kind(bytes: &[u8]) -> Option<u16> {
    let (kind, _rest) = postcard::take_from_bytes::<u16>(bytes).ok()?;
    Some(kind)
}
