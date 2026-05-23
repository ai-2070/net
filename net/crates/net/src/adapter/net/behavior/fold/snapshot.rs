//! Fold-state snapshot envelope.
//!
//! Each [`super::Fold`] can serialize its entries for restart
//! recovery: dump on graceful shutdown / periodic checkpoint,
//! restore on startup so a freshly-spawned node doesn't have to
//! wait for every publisher to re-announce before queries start
//! seeing real data.
//!
//! The snapshot stores `K::Payload` and `K::Key` directly — both
//! are bound by `Serialize + DeserializeOwned` on the
//! [`super::FoldKind`] trait — alongside the per-entry
//! bookkeeping needed to faithfully reconstruct an apply state
//! (`node_id`, `generation`, and TTL-relative timing).
//!
//! `Instant` is not serializable across process boundaries, so
//! the on-wire shape stores TTL-relative durations
//! (`received_offset_ns` and `expires_offset_ns`) relative to a
//! `taken_at` Unix-micros timestamp captured at dump time. On
//! restore the runtime re-anchors against the current
//! `Instant::now()` so freshness semantics survive the dump /
//! restore boundary.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::adapter::net::current_timestamp_micros as unix_micros_now;

use super::state::{FoldEntry, FoldState, NodeId};
use super::FoldKind;

/// On-wire representation of one entry in a [`FoldSnapshot`].
/// The shape mirrors [`FoldEntry`] except that the two `Instant`
/// fields become offsets relative to the snapshot's
/// `taken_at_unix_us` anchor — `Instant` itself has no portable
/// representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoldSnapshotEntry<K: FoldKind> {
    /// Domain-specific key. Re-hashes deterministically on
    /// restore because `K::Key: Hash + Eq + Clone`.
    pub key: K::Key,
    /// Payload, as it was at dump time.
    pub payload: K::Payload,
    /// Publisher that produced the entry.
    pub node_id: NodeId,
    /// Generation as recorded in the [`FoldEntry`]. Restored
    /// entries that lose to a higher-generation live
    /// announcement post-restore are replaced normally.
    pub generation: u64,
    /// Nanoseconds before `taken_at_unix_us` at which the entry
    /// was originally applied. Re-anchored on restore to
    /// `Instant::now() - Duration::from_nanos(received_offset_ns)`.
    pub received_offset_ns: u64,
    /// Nanoseconds AFTER `taken_at_unix_us` at which the entry
    /// is scheduled to expire. Negative values aren't
    /// representable in the unsigned type — entries already
    /// expired at dump time are dropped from the snapshot by
    /// [`FoldSnapshot::from_state`] rather than carried with a
    /// past expiry.
    pub expires_offset_ns: u64,
}

/// One fold's serialized state at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoldSnapshot<K: FoldKind> {
    /// [`super::FoldKind::KIND_ID`] of the producing fold —
    /// lets the restore path refuse a snapshot from the wrong
    /// fold kind (e.g. a `CapabilityFold` dump fed to a
    /// `RoutingFold` instance).
    pub kind: u16,
    /// Unix-micros timestamp at which the snapshot was taken,
    /// used as the anchor for the per-entry duration offsets.
    pub taken_at_unix_us: u64,
    /// All non-expired entries at snapshot time.
    pub entries: Vec<FoldSnapshotEntry<K>>,
}

impl<K: FoldKind> FoldSnapshot<K> {
    /// Snapshot the current state. Entries whose
    /// `expires_at < now` at snapshot time are dropped from the
    /// dump — they would be expired by the TTL sweeper on the
    /// other side of the restore anyway, and persisting them
    /// would just waste IO. Callers wanting an exact dump for
    /// diagnostics can read [`FoldState::entries`] directly.
    pub fn from_state(state: &FoldState<K>) -> Self {
        let now = Instant::now();
        let taken_at_unix_us = unix_micros_now();

        let mut entries = Vec::with_capacity(state.entries.len());
        for (key, entry) in state.entries.iter() {
            // Drop entries already past their TTL — see method
            // doc for the rationale.
            if entry.expires_at <= now {
                continue;
            }
            let received_offset_ns =
                now.saturating_duration_since(entry.received_at).as_nanos() as u64;
            let expires_offset_ns =
                entry.expires_at.saturating_duration_since(now).as_nanos() as u64;
            entries.push(FoldSnapshotEntry {
                key: key.clone(),
                payload: entry.payload.clone(),
                node_id: entry.node_id,
                generation: entry.generation,
                received_offset_ns,
                expires_offset_ns,
            });
        }

        Self {
            kind: K::KIND_ID,
            taken_at_unix_us,
            entries,
        }
    }

    /// Materialize a [`FoldEntry`] from a snapshot entry, anchored
    /// to a freshly-captured `Instant::now()` and aged by
    /// `elapsed_since_dump` — the wall-clock interval between the
    /// snapshot's `taken_at_unix_us` and "now". The restored
    /// `expires_at` consumes the elapsed downtime out of the
    /// remaining TTL so a dump → long-pause → restore can't extend
    /// an entry's lifetime past its original deadline; the
    /// restored `received_at` ages by the same interval so age-
    /// dependent freshness checks see a realistic timestamp.
    ///
    /// Returns `None` when the entry would already be expired —
    /// `elapsed_since_dump >= expires_offset_ns`. Callers
    /// ([`super::Fold::restore`]) skip such entries rather than
    /// installing them with `expires_at <= now` and waiting for
    /// the sweeper to clean them up.
    pub(super) fn rehydrate_entry(
        snap_entry: &FoldSnapshotEntry<K>,
        anchor: Instant,
        elapsed_since_dump: Duration,
    ) -> Option<FoldEntry<K>> {
        let expires_offset = Duration::from_nanos(snap_entry.expires_offset_ns);
        if elapsed_since_dump >= expires_offset {
            return None;
        }
        let remaining_ttl = expires_offset - elapsed_since_dump;
        let aged_received_offset = Duration::from_nanos(snap_entry.received_offset_ns)
            .saturating_add(elapsed_since_dump);
        let received_at = anchor.checked_sub(aged_received_offset).unwrap_or(anchor);
        let expires_at = anchor.checked_add(remaining_ttl).unwrap_or(anchor);
        Some(FoldEntry {
            payload: snap_entry.payload.clone(),
            node_id: snap_entry.node_id,
            generation: snap_entry.generation,
            received_at,
            expires_at,
        })
    }
}
