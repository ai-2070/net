//! Gravity-driven blob migration controller (PR-5j-d).
//!
//! Closes the loop opened by PR-5j-a/b/c. Each node periodically
//! scans the capability index for peer-advertised
//! `heat:blob:<hex>=<rate>` reserved tags (PR-5j-c emission shape),
//! consults [`should_migrate_blob_to`] against the local node's
//! capabilities + the publisher's advertised scope, and on admit
//! triggers a best-effort [`BlobAdapter::prefetch`] for the
//! referenced chunk. The prefetch in turn opens the chunk channel
//! against the local Redex with replication enabled, pulling bytes
//! from the publisher (or any other holder) over the existing
//! per-chunk replication runtime.
//!
//! Decision-only sub-surface lives at the top — operators wanting
//! to drive their own tick loop pull [`parse_blob_heat_tag`] +
//! [`BlobMigrationController::candidates`] +
//! [`super::should_migrate_blob_to`] and act however they like. The
//! async [`drive_blob_migration_tick`] helper composes them into
//! the standard observation → decide → act pipeline.

use std::sync::Arc;

use crate::adapter::net::behavior::capability::{CapabilityIndex, CapabilitySet};
use crate::adapter::net::behavior::tag::Tag;

use super::adapter::BlobAdapter;
use super::admission::{should_migrate_blob_to, MigrateBlobReject, MigrateBlobVerdict};
use super::blob_ref::BlobRef;

/// Parse a `heat:blob:<hex64>=<rate>` reserved tag into the
/// `(hash, rate)` pair. Returns `None` when the tag isn't a
/// blob-heat shape (chain-heat tags, unrelated reserved tags,
/// malformed hex, non-numeric rate).
///
/// Mirrors the producer-side encoder used by
/// `MeshNode::announce_blob_heat`.
pub fn parse_blob_heat_tag(tag: &Tag) -> Option<([u8; 32], f64)> {
    let body = match tag {
        Tag::Reserved { prefix, body } if prefix == "heat:" => body,
        _ => return None,
    };
    let rest = body.strip_prefix("blob:")?;
    let eq_idx = rest.find('=')?;
    let hex = &rest[..eq_idx];
    let rate_str = &rest[eq_idx + 1..];
    if hex.len() != 64 {
        return None;
    }
    let mut hash = [0u8; 32];
    for (i, byte) in hash.iter_mut().enumerate() {
        let pair = hex.get(i * 2..i * 2 + 2)?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    let rate: f64 = rate_str.parse().ok()?;
    if !rate.is_finite() {
        return None;
    }
    Some((hash, rate))
}

/// One candidate the migration controller is considering.
/// Surfaces the publisher's caps + the wire-form rate so a
/// caller wiring its own decision loop has the same inputs the
/// built-in controller does.
#[derive(Debug, Clone)]
pub struct BlobMigrationCandidate {
    /// 32-byte chunk hash from the `heat:blob:<hex>=<rate>` tag.
    pub hash: [u8; 32],
    /// node_id of the peer that advertised the heat tag.
    pub publisher_node_id: u64,
    /// Snapshot of the publisher's full capability set — the
    /// `should_migrate_blob_to` scope check reads this directly.
    pub publisher_caps: CapabilitySet,
    /// Wire-form rate. Operators dashboard it as the "hotness"
    /// of the hash on the peer that emitted the heat tag.
    pub rate: f64,
}

/// Decision-only surface. Holds a borrow of the local caps + the
/// capability index; `candidates` walks the index, parses every
/// `heat:blob:` tag, and bundles `(hash, publisher_node_id,
/// publisher_caps, rate)` quadruples back to the caller.
///
/// The controller is `Send + Sync` when its inputs are; clone-cheap
/// because it holds references rather than owned state.
pub struct BlobMigrationController<'a> {
    /// This node's advertised caps — read by
    /// `should_migrate_blob_to` for the local-side gate.
    pub local_caps: &'a CapabilitySet,
    /// The capability index this node maintains. Walked once per
    /// `candidates` call; the call is O(n_peers × n_tags_per_peer)
    /// and meant to run at the gravity tick cadence (every
    /// `DataGravityPolicy::emit_interval`, not per-event).
    pub capability_index: &'a CapabilityIndex,
}

impl<'a> BlobMigrationController<'a> {
    /// Build a controller view over `local_caps` + the supplied
    /// capability index. No state is captured; the walk happens
    /// inside `candidates`.
    pub fn new(local_caps: &'a CapabilitySet, capability_index: &'a CapabilityIndex) -> Self {
        Self {
            local_caps,
            capability_index,
        }
    }

    /// Walk every peer in `capability_index` and surface
    /// `(hash, publisher_node_id, publisher_caps, rate)` for each
    /// `heat:blob:<hex>=<rate>` tag advertised. Duplicate hashes
    /// (multiple peers advertising the same blob) surface as
    /// independent candidates — the caller's tie-breaker picks
    /// the migration target.
    pub fn candidates(&self) -> Vec<BlobMigrationCandidate> {
        let mut out = Vec::new();
        for node_id in self.capability_index.all_nodes() {
            let caps = match self.capability_index.get(node_id) {
                Some(c) => c,
                None => continue,
            };
            for tag in &caps.tags {
                if let Some((hash, rate)) = parse_blob_heat_tag(tag) {
                    out.push(BlobMigrationCandidate {
                        hash,
                        publisher_node_id: node_id,
                        publisher_caps: caps.clone(),
                        rate,
                    });
                }
            }
        }
        out
    }
}

/// Outcome of one `drive_blob_migration_tick` pass. Counters give
/// operators a per-tick view of how many migrations the gravity
/// loop is admitting / rejecting / failing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobMigrationTickReport {
    /// Candidates that passed `should_migrate_blob_to` + had a
    /// known size + reached the `adapter.prefetch` call without
    /// error.
    pub admitted: u64,
    /// Candidates the local-side decision rejected, split by
    /// reason. Cumulative across the tick.
    pub rejected_no_storage: u64,
    /// `gravity.enabled` absent in local caps.
    pub rejected_gravity_disabled: u64,
    /// `gravity.proximity == 0` in local caps.
    pub rejected_proximity_zero: u64,
    /// Local node unhealthy (`dataforts:blob-storage-unhealthy`).
    pub rejected_unhealthy: u64,
    /// Publisher's scope outside the local gravity scope boundary.
    pub rejected_scope_mismatch: u64,
    /// Local disk-free below the size requirement.
    pub rejected_insufficient_disk: u64,
    /// Candidates skipped because the size resolver returned
    /// `None`. The controller can't run the disk-free gate without
    /// a size, so these neither admit nor reject — they pass
    /// through and surface as a separate counter.
    pub skipped_unknown_size: u64,
    /// Candidates that admitted the decision but failed at the
    /// `adapter.prefetch` await.
    pub prefetch_errors: u64,
}

impl BlobMigrationTickReport {
    /// Sum of every rejected_* counter.
    pub fn total_rejected(&self) -> u64 {
        self.rejected_no_storage
            + self.rejected_gravity_disabled
            + self.rejected_proximity_zero
            + self.rejected_unhealthy
            + self.rejected_scope_mismatch
            + self.rejected_insufficient_disk
    }

    fn record_reject(&mut self, r: MigrateBlobReject) {
        match r {
            MigrateBlobReject::NoStorageCap => self.rejected_no_storage += 1,
            MigrateBlobReject::GravityDisabled => self.rejected_gravity_disabled += 1,
            MigrateBlobReject::ProximityZero => self.rejected_proximity_zero += 1,
            MigrateBlobReject::Unhealthy => self.rejected_unhealthy += 1,
            MigrateBlobReject::ScopeMismatch => self.rejected_scope_mismatch += 1,
            MigrateBlobReject::InsufficientDisk => self.rejected_insufficient_disk += 1,
        }
    }
}

/// Drive one tick of the migration loop: walk every
/// `heat:blob:<hex>=<rate>` advertisement reachable through
/// `capability_index`, decide against the local node's
/// capabilities via [`should_migrate_blob_to`], and on admit
/// kick off a best-effort [`BlobAdapter::prefetch`] for the
/// referenced chunk. Returns a [`BlobMigrationTickReport`] with
/// per-reason counters so the caller can dashboard the loop.
///
/// `size_for_hash` resolves the chunk's wire size in bytes.
/// Without it the disk-free gate inside `should_migrate_blob_to`
/// can't run, so unknown-size candidates are skipped and
/// surfaced via [`BlobMigrationTickReport::skipped_unknown_size`].
/// In production the resolver typically reads from a side index
/// the publisher maintains (e.g. an `EventMeta` projection) or a
/// fixed-size assumption tied to deployment policy.
///
/// Errors from `adapter.prefetch` count into
/// `prefetch_errors` but never propagate — the controller is
/// fire-and-forget at the wire level, matching the
/// "greedy-style" semantics of the rest of the dataforts layer.
pub async fn drive_blob_migration_tick<A, F>(
    local_caps: &CapabilitySet,
    capability_index: &CapabilityIndex,
    adapter: &A,
    size_for_hash: F,
) -> BlobMigrationTickReport
where
    A: BlobAdapter + ?Sized,
    F: Fn([u8; 32]) -> Option<u64>,
{
    let controller = BlobMigrationController::new(local_caps, capability_index);
    let candidates = controller.candidates();
    let mut report = BlobMigrationTickReport::default();
    for candidate in candidates {
        let size = match size_for_hash(candidate.hash) {
            Some(s) => s,
            None => {
                report.skipped_unknown_size += 1;
                continue;
            }
        };
        let verdict = should_migrate_blob_to(local_caps, &candidate.publisher_caps, size);
        match verdict {
            MigrateBlobVerdict::Admit => {
                // Build a `BlobRef::Small` from the hash + size —
                // even when the underlying blob is a Manifest, the
                // chunk channel for the manifest body hash will
                // open via `prefetch`. Real Manifest migration is
                // a follow-up that probably attaches a side index
                // (hash → manifest) so the controller can prefetch
                // the constituent chunk channels in one shot.
                let blob_ref = BlobRef::small(
                    format!("mesh://{}", hex32(&candidate.hash)),
                    candidate.hash,
                    size,
                );
                match adapter.prefetch(&blob_ref).await {
                    Ok(()) => report.admitted += 1,
                    Err(e) => {
                        tracing::trace!(
                            error = ?e,
                            hash = ?candidate.hash,
                            "blob migration: prefetch failed; counted"
                        );
                        report.prefetch_errors += 1;
                    }
                }
            }
            MigrateBlobVerdict::Reject(r) => report.record_reject(r),
        }
    }
    report
}

/// `Arc`-wrapped form of [`drive_blob_migration_tick`] for
/// operators that want to share a single adapter handle across
/// the gravity tick task and the rest of the runtime. Mirrors the
/// trait-object call shape used elsewhere in this module.
pub async fn drive_blob_migration_tick_arc<F>(
    local_caps: &CapabilitySet,
    capability_index: &CapabilityIndex,
    adapter: Arc<dyn BlobAdapter>,
    size_for_hash: F,
) -> BlobMigrationTickReport
where
    F: Fn([u8; 32]) -> Option<u64>,
{
    drive_blob_migration_tick(local_caps, capability_index, &*adapter, size_for_hash).await
}

/// Helper to format a chunk hash as a 64-char hex string.
/// Local to this module so we don't widen the public surface.
fn hex32(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::super::error::BlobError;
    use super::*;
    use crate::adapter::net::behavior::tag::AxisSeparator;
    use crate::adapter::net::behavior::TaxonomyAxis;
    use crate::adapter::net::identity::EntityId;

    fn hex64(seed: u8) -> ([u8; 32], String) {
        let mut h = [0u8; 32];
        h[0] = seed;
        let mut s = String::with_capacity(64);
        for b in &h {
            use std::fmt::Write;
            let _ = write!(s, "{:02x}", b);
        }
        (h, s)
    }

    // --- parse_blob_heat_tag ---

    #[test]
    fn parse_blob_heat_tag_round_trip() {
        let (h, hex) = hex64(0x42);
        let tag = Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.75", hex),
        };
        let (got_hash, rate) = parse_blob_heat_tag(&tag).expect("must parse");
        assert_eq!(got_hash, h);
        assert!((rate - 0.75).abs() < 1e-6);
    }

    #[test]
    fn parse_blob_heat_tag_rejects_chain_heat_shape() {
        let chain_tag = Tag::Reserved {
            prefix: "heat:".to_string(),
            body: "deadbeefcafebabe=0.50".to_string(),
        };
        assert!(parse_blob_heat_tag(&chain_tag).is_none());
    }

    #[test]
    fn parse_blob_heat_tag_rejects_axis_tag() {
        let axis = Tag::AxisValue {
            axis: TaxonomyAxis::Dataforts,
            key: "blob.storage".to_string(),
            value: "1".to_string(),
            separator: AxisSeparator::Eq,
        };
        assert!(parse_blob_heat_tag(&axis).is_none());
    }

    #[test]
    fn parse_blob_heat_tag_rejects_malformed_hex() {
        let bad = Tag::Reserved {
            prefix: "heat:".to_string(),
            body: "blob:zzzz=0.50".to_string(),
        };
        assert!(parse_blob_heat_tag(&bad).is_none());
        let short = Tag::Reserved {
            prefix: "heat:".to_string(),
            body: "blob:dead=0.50".to_string(),
        };
        assert!(parse_blob_heat_tag(&short).is_none());
    }

    #[test]
    fn parse_blob_heat_tag_rejects_non_finite_rate() {
        let (_h, hex) = hex64(0x01);
        let nan_tag = Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=NaN", hex),
        };
        assert!(parse_blob_heat_tag(&nan_tag).is_none());
    }

    // --- BlobMigrationController::candidates ---

    fn index_with_peer_heat(
        peer_id: u64,
        peer_caps: CapabilitySet,
        peer_seed: u8,
    ) -> CapabilityIndex {
        let index = CapabilityIndex::new();
        let entity = EntityId::from_bytes([peer_seed; 32]);
        let ann = crate::adapter::net::behavior::capability::CapabilityAnnouncement::new(
            peer_id, entity, 1, peer_caps,
        );
        index.index(ann);
        index
    }

    #[test]
    fn controller_lists_one_candidate_per_blob_heat_tag() {
        let (h1, hex1) = hex64(0x10);
        let (h2, hex2) = hex64(0x20);
        let mut peer_caps = CapabilitySet::default();
        peer_caps.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.50", hex1),
        });
        peer_caps.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.30", hex2),
        });
        let index = index_with_peer_heat(99, peer_caps, 0xAA);
        let local = CapabilitySet::default();
        let controller = BlobMigrationController::new(&local, &index);
        let candidates = controller.candidates();
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().any(|c| c.hash == h1 && (c.rate - 0.50).abs() < 1e-6));
        assert!(candidates.iter().any(|c| c.hash == h2 && (c.rate - 0.30).abs() < 1e-6));
        assert!(candidates.iter().all(|c| c.publisher_node_id == 99));
    }

    #[test]
    fn controller_skips_peers_without_blob_heat_tags() {
        let mut peer_caps = CapabilitySet::default();
        // Chain-heat shape — should NOT surface as a blob candidate.
        peer_caps.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: "deadbeefcafebabe=0.50".to_string(),
        });
        let index = index_with_peer_heat(7, peer_caps, 0xBB);
        let local = CapabilitySet::default();
        let candidates = BlobMigrationController::new(&local, &index).candidates();
        assert!(candidates.is_empty());
    }

    // --- drive_blob_migration_tick decision routing ---

    /// Mock adapter that records prefetch calls; used to assert
    /// the controller routes admit verdicts to the adapter.
    struct PrefetchRecorder {
        calls: std::sync::Arc<std::sync::atomic::AtomicU64>,
        fail: bool,
    }
    impl PrefetchRecorder {
        fn new() -> Self {
            Self {
                calls: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
                fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                calls: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
                fail: true,
            }
        }
    }
    #[async_trait::async_trait]
    impl BlobAdapter for PrefetchRecorder {
        fn adapter_id(&self) -> &str {
            "test-prefetch-recorder"
        }
        async fn store(&self, _: &BlobRef, _: &[u8]) -> Result<(), BlobError> {
            unreachable!()
        }
        async fn fetch(&self, _: &BlobRef) -> Result<Vec<u8>, BlobError> {
            unreachable!()
        }
        async fn fetch_range(
            &self,
            _: &BlobRef,
            _: std::ops::Range<u64>,
        ) -> Result<Vec<u8>, BlobError> {
            unreachable!()
        }
        async fn exists(&self, _: &BlobRef) -> Result<bool, BlobError> {
            unreachable!()
        }
        async fn prefetch(&self, _: &BlobRef) -> Result<(), BlobError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.fail {
                Err(BlobError::Backend("test prefetch failure".into()))
            } else {
                Ok(())
            }
        }
    }

    fn participating_local(scope: &str, proximity: u8, disk_free_gb: u64) -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag(format!("dataforts.blob.disk_free_gb={}", disk_free_gb))
            .add_tag("dataforts.gravity.enabled")
            .add_tag(format!("dataforts.gravity.scope={}", scope))
            .add_tag(format!("dataforts.gravity.proximity={}", proximity))
    }

    fn publisher_caps_with_heat(hash_seed: u8, scope: &str, rate: &str) -> CapabilitySet {
        let (_h, hex) = hex64(hash_seed);
        let mut caps = CapabilitySet::new()
            .add_tag(format!("dataforts.gravity.scope={}", scope))
            .add_tag(format!("dataforts.greedy.scope={}", scope));
        caps.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}={}", hex, rate),
        });
        caps
    }

    #[tokio::test]
    async fn drive_tick_admits_and_calls_prefetch_on_participating_local() {
        let publisher_caps = publisher_caps_with_heat(0x10, "mesh", "0.75");
        let index = index_with_peer_heat(99, publisher_caps, 0xAA);
        let local = participating_local("mesh", 128, 50);
        let adapter = PrefetchRecorder::new();
        let calls = adapter.calls.clone();
        let report = drive_blob_migration_tick(&local, &index, &adapter, |_| Some(1024)).await;
        assert_eq!(report.admitted, 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(report.total_rejected(), 0);
        assert_eq!(report.prefetch_errors, 0);
    }

    #[tokio::test]
    async fn drive_tick_rejects_when_local_lacks_blob_storage() {
        let publisher_caps = publisher_caps_with_heat(0x20, "mesh", "0.50");
        let index = index_with_peer_heat(50, publisher_caps, 0xBB);
        // Local: no `dataforts.blob.storage` tag → NoStorageCap.
        let local = CapabilitySet::new()
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.gravity.proximity=128");
        let adapter = PrefetchRecorder::new();
        let calls = adapter.calls.clone();
        let report = drive_blob_migration_tick(&local, &index, &adapter, |_| Some(1024)).await;
        assert_eq!(report.admitted, 0);
        assert_eq!(report.rejected_no_storage, 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn drive_tick_skips_when_size_resolver_returns_none() {
        let publisher_caps = publisher_caps_with_heat(0x30, "mesh", "0.40");
        let index = index_with_peer_heat(50, publisher_caps, 0xCC);
        let local = participating_local("mesh", 128, 50);
        let adapter = PrefetchRecorder::new();
        let calls = adapter.calls.clone();
        let report = drive_blob_migration_tick(&local, &index, &adapter, |_| None).await;
        assert_eq!(report.skipped_unknown_size, 1);
        assert_eq!(report.admitted, 0);
        assert_eq!(report.total_rejected(), 0);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn drive_tick_counts_prefetch_errors_without_propagating() {
        let publisher_caps = publisher_caps_with_heat(0x40, "mesh", "0.90");
        let index = index_with_peer_heat(50, publisher_caps, 0xDD);
        let local = participating_local("mesh", 128, 50);
        let adapter = PrefetchRecorder::failing();
        let calls = adapter.calls.clone();
        let report = drive_blob_migration_tick(&local, &index, &adapter, |_| Some(1024)).await;
        assert_eq!(report.admitted, 0);
        assert_eq!(report.prefetch_errors, 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn drive_tick_rejects_when_disk_free_insufficient() {
        let publisher_caps = publisher_caps_with_heat(0x50, "mesh", "0.50");
        let index = index_with_peer_heat(50, publisher_caps, 0xEE);
        // Local has only 1 GiB free; we ask for a 4 GiB blob.
        let local = participating_local("mesh", 128, 1);
        let adapter = PrefetchRecorder::new();
        let four_gib: u64 = 4 * (1 << 30);
        let report =
            drive_blob_migration_tick(&local, &index, &adapter, |_| Some(four_gib)).await;
        assert_eq!(report.admitted, 0);
        assert_eq!(report.rejected_insufficient_disk, 1);
    }
}
