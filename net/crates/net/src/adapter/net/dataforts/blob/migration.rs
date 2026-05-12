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

/// Manifest sibling list returned by an operator-supplied
/// [`drive_blob_migration_tick_with_manifest_resolver`]
/// callback.  Each entry is a `(chunk_hash, chunk_size)` pair —
/// the migration controller calls `adapter.prefetch` on each
/// pair as if it had been an independently-advertised
/// candidate, attributing the verdict to the originally-heated
/// hash so the per-reason counters stay coherent.
///
/// Empty vector is the "no siblings known" signal (equivalent to
/// the resolver returning `None`); the migration short-circuits
/// to the single-chunk path.
pub type ManifestSiblings = Vec<([u8; 32], u64)>;

/// Manifest-aware variant of [`drive_blob_migration_tick`]. Same
/// observation → decide → act loop, but each admitted heat
/// candidate first asks the operator-supplied `manifest_resolver`
/// whether the hot hash is the head of a manifest the local node
/// knows about. When it is, the controller proactively
/// `adapter.prefetch`-es every sibling chunk too — closing the
/// gap between "this single chunk is hot" and "this entire
/// manifest is likely about to be fetched."
///
/// `manifest_resolver: Fn([u8; 32]) -> Option<ManifestSiblings>`
/// is the operator's side-index hook:
///
/// - Return `None` (or an empty vec) to fall through to the
///   single-chunk prefetch path. Equivalent to the plain
///   [`drive_blob_migration_tick`] behavior.
/// - Return `Some(siblings)` to attach a sibling list. The
///   controller short-circuits any sibling that the
///   single-chunk path already prefetched (the heated hash
///   itself, plus dedup across siblings on duplicate candidates)
///   so a sibling list overlapping the candidate set bumps
///   `admitted` once per *distinct* hash. Sibling sizes drive
///   their own `should_migrate_blob_to` disk-free gate; sibling
///   verdicts route into the same per-reason rejection counters
///   as the top-level candidate.
///
/// Producer-side: an operator typically maintains the side index
/// at store time (`MeshBlobAdapter::store` of a
/// `BlobRef::Manifest` knows the chunk list) and exposes
/// `manifest_resolver` over an `Arc<DashMap>` shared with the
/// gravity tick task. Future refinements can carry the manifest
/// structure on the wire via a `manifest:<hex>:chunks=<list>`
/// capability tag family — outside this primitive's scope.
pub async fn drive_blob_migration_tick_with_manifest_resolver<A, F, M>(
    local_caps: &CapabilitySet,
    capability_index: &CapabilityIndex,
    adapter: &A,
    size_for_hash: F,
    manifest_resolver: M,
) -> BlobMigrationTickReport
where
    A: BlobAdapter + ?Sized,
    F: Fn([u8; 32]) -> Option<u64>,
    M: Fn([u8; 32]) -> Option<ManifestSiblings>,
{
    let controller = BlobMigrationController::new(local_caps, capability_index);
    let candidates = controller.candidates();
    let mut report = BlobMigrationTickReport::default();
    // Dedup across candidate + sibling expansions so a *successful*
    // prefetch of a hash isn't repeated when the same hash
    // surfaces later via a different candidate's manifest list.
    // Critically, only hashes whose prefetch actually fired (Admit
    // + Ok at the adapter) land in this set — a hash that was
    // rejected (e.g. InsufficientDisk against the first candidate's
    // publisher caps) can still be reconsidered when a later
    // candidate's caps reach it under different scope. Without
    // that, a hash that rejected once gets silently stranded for
    // the rest of the tick.
    let mut already_prefetched: std::collections::HashSet<[u8; 32]> =
        std::collections::HashSet::new();
    for candidate in candidates {
        let size = match size_for_hash(candidate.hash) {
            Some(s) => s,
            None => {
                report.skipped_unknown_size += 1;
                continue;
            }
        };
        // Skip if a *prior* admit already prefetched this hash —
        // this is the only short-circuit we apply pre-verdict.
        // Rejection histories from prior candidates don't carry
        // over.
        if already_prefetched.contains(&candidate.hash) {
            continue;
        }
        let verdict = should_migrate_blob_to(local_caps, &candidate.publisher_caps, size);
        match verdict {
            MigrateBlobVerdict::Admit => {
                let blob_ref = BlobRef::small(
                    format!("mesh://{}", hex32(&candidate.hash)),
                    candidate.hash,
                    size,
                );
                match adapter.prefetch(&blob_ref).await {
                    Ok(()) => {
                        report.admitted += 1;
                        already_prefetched.insert(candidate.hash);
                    }
                    Err(e) => {
                        tracing::trace!(
                            error = ?e,
                            hash = ?candidate.hash,
                            "blob migration: prefetch failed; counted"
                        );
                        report.prefetch_errors += 1;
                        // Don't insert: a failed prefetch hasn't
                        // committed the migration, so a later
                        // candidate is free to retry the same hash.
                    }
                }
                // Manifest expansion: query the resolver for
                // siblings of the heated hash. Sibling verdicts
                // share the same dedup discipline — rejected
                // siblings stay reconsiderable for the rest of
                // the tick.
                let siblings = manifest_resolver(candidate.hash).unwrap_or_default();
                for (sibling_hash, sibling_size) in siblings {
                    if already_prefetched.contains(&sibling_hash) {
                        continue;
                    }
                    let sibling_verdict =
                        should_migrate_blob_to(local_caps, &candidate.publisher_caps, sibling_size);
                    match sibling_verdict {
                        MigrateBlobVerdict::Admit => {
                            let blob_ref = BlobRef::small(
                                format!("mesh://{}", hex32(&sibling_hash)),
                                sibling_hash,
                                sibling_size,
                            );
                            match adapter.prefetch(&blob_ref).await {
                                Ok(()) => {
                                    report.admitted += 1;
                                    already_prefetched.insert(sibling_hash);
                                }
                                Err(e) => {
                                    tracing::trace!(
                                        error = ?e,
                                        hash = ?sibling_hash,
                                        "blob migration: manifest sibling prefetch failed"
                                    );
                                    report.prefetch_errors += 1;
                                }
                            }
                        }
                        MigrateBlobVerdict::Reject(r) => report.record_reject(r),
                    }
                }
            }
            MigrateBlobVerdict::Reject(r) => report.record_reject(r),
        }
    }
    report
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
        assert!(candidates
            .iter()
            .any(|c| c.hash == h1 && (c.rate - 0.50).abs() < 1e-6));
        assert!(candidates
            .iter()
            .any(|c| c.hash == h2 && (c.rate - 0.30).abs() < 1e-6));
        assert!(candidates.iter().all(|c| c.publisher_node_id == 99));
    }

    #[test]
    fn controller_ignores_chain_heat_shape_tags() {
        // Chain-heat (`heat:<origin_hex>=<rate>`) and blob-heat
        // (`heat:blob:<hash_hex>=<rate>`) share the `heat:`
        // prefix but address different things. Only blob-heat
        // bodies should surface as migration candidates; a peer
        // whose only `heat:` tag is the chain-heat shape must
        // produce zero candidates.
        let mut peer_caps = CapabilitySet::default();
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
        let report = drive_blob_migration_tick(&local, &index, &adapter, |_| Some(four_gib)).await;
        assert_eq!(report.admitted, 0);
        assert_eq!(report.rejected_insufficient_disk, 1);
    }

    // --- Manifest-aware migration (PR-5o) ---

    #[tokio::test]
    async fn manifest_resolver_prefetches_every_sibling_chunk() {
        // Heat surfaces for the manifest's first chunk only;
        // the resolver returns the full sibling list; the
        // controller prefetches every chunk.
        let publisher_caps = publisher_caps_with_heat(0x60, "mesh", "0.50");
        let index = index_with_peer_heat(50, publisher_caps, 0xFF);
        let local = participating_local("mesh", 128, 50);
        let adapter = PrefetchRecorder::new();
        let calls = adapter.calls.clone();

        let (head_hash, _) = hex64(0x60);
        let (s1, _) = hex64(0x61);
        let (s2, _) = hex64(0x62);
        let (s3, _) = hex64(0x63);
        let siblings_for_head = vec![(s1, 1024), (s2, 2048), (s3, 4096)];

        let report = drive_blob_migration_tick_with_manifest_resolver(
            &local,
            &index,
            &adapter,
            |_h| Some(1024),
            |h| {
                if h == head_hash {
                    Some(siblings_for_head.clone())
                } else {
                    None
                }
            },
        )
        .await;
        assert_eq!(report.admitted, 4, "head + 3 siblings = 4 admits");
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 4);
        assert_eq!(report.prefetch_errors, 0);
    }

    #[tokio::test]
    async fn manifest_resolver_dedups_overlapping_sibling_and_candidate_lists() {
        // Resolver claims a sibling list that includes the head
        // hash itself plus a chunk that was independently
        // heat-advertised. The dedup set short-circuits both,
        // so prefetch fires exactly twice.
        let (head_hash, head_hex) = hex64(0x70);
        let (sibling_hash, sibling_hex) = hex64(0x71);
        let mut publisher = CapabilitySet::new()
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.greedy.scope=mesh");
        publisher.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.40", head_hex),
        });
        publisher.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.30", sibling_hex),
        });
        let index = index_with_peer_heat(99, publisher, 0xAB);
        let local = participating_local("mesh", 128, 50);
        let adapter = PrefetchRecorder::new();
        let calls = adapter.calls.clone();

        // Resolver returns the head AND a sibling that's already
        // advertised — the controller must not double-prefetch.
        let report = drive_blob_migration_tick_with_manifest_resolver(
            &local,
            &index,
            &adapter,
            |_h| Some(1024),
            move |h| {
                if h == head_hash {
                    Some(vec![(head_hash, 1024), (sibling_hash, 2048)])
                } else {
                    None
                }
            },
        )
        .await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "dedup must collapse head + already-advertised sibling to 2 distinct prefetches"
        );
        // admitted is the same — counts distinct prefetches.
        assert_eq!(report.admitted, 2);
    }

    #[tokio::test]
    async fn manifest_resolver_none_falls_through_to_single_chunk_path() {
        // Equivalent to plain drive_blob_migration_tick when the
        // resolver always returns None.
        let publisher_caps = publisher_caps_with_heat(0x80, "mesh", "0.50");
        let index = index_with_peer_heat(50, publisher_caps, 0xCD);
        let local = participating_local("mesh", 128, 50);
        let adapter = PrefetchRecorder::new();
        let calls = adapter.calls.clone();

        let report = drive_blob_migration_tick_with_manifest_resolver(
            &local,
            &index,
            &adapter,
            |_h| Some(1024),
            |_h| None,
        )
        .await;
        assert_eq!(report.admitted, 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn manifest_sibling_rejection_routes_into_per_reason_counters() {
        // Head admits + prefetches; one sibling exceeds disk
        // free; the rejection counts against the same per-reason
        // bucket the single-chunk path uses.
        let publisher_caps = publisher_caps_with_heat(0x90, "mesh", "0.50");
        let index = index_with_peer_heat(50, publisher_caps, 0xEF);
        let local = participating_local("mesh", 128, 1); // 1 GiB free
        let adapter = PrefetchRecorder::new();
        let calls = adapter.calls.clone();

        let (head_hash, _) = hex64(0x90);
        let (sibling_hash, _) = hex64(0x91);
        let four_gib: u64 = 4 * (1 << 30);

        let report = drive_blob_migration_tick_with_manifest_resolver(
            &local,
            &index,
            &adapter,
            move |h| {
                if h == sibling_hash {
                    Some(four_gib) // too big
                } else {
                    Some(1024)
                }
            },
            move |h| {
                if h == head_hash {
                    Some(vec![(sibling_hash, four_gib)])
                } else {
                    None
                }
            },
        )
        .await;
        assert_eq!(report.admitted, 1, "head admits");
        assert_eq!(
            report.rejected_insufficient_disk, 1,
            "sibling rejects on disk gate"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// Regression for the manifest dedup trap: a sibling that
    /// rejects against one candidate's `publisher_caps` must not
    /// be silently consumed from the dedup set; it stays
    /// reconsiderable when the same hash surfaces under a later
    /// candidate's manifest. Pre-fix shape inserted into
    /// `already_prefetched` *before* the sibling verdict ran, so
    /// a rejected sibling was stranded for the rest of the tick
    /// and rejection counters under-counted. Fix moved insertion
    /// to post-Admit+Ok-prefetch.
    ///
    /// Observable difference: two candidates whose manifest
    /// resolvers both return the same `huge_sibling` hash (which
    /// rejects `InsufficientDisk` against the local node). Post-
    /// fix, the rejection count is 2 (one per candidate's
    /// expansion); pre-fix would have been 1 (the second
    /// expansion's verdict was skipped via the pre-verdict dedup
    /// insert).
    #[tokio::test]
    async fn rejected_sibling_stays_reconsiderable_across_candidates() {
        let (a_top_hash, a_hex) = hex64(0xC1);
        let (b_top_hash, b_hex) = hex64(0xC2);
        let (shared_sibling_hash, _shared_hex) = hex64(0xC3);

        // Two peers, each advertising a heated top-level hash.
        // Both publishers use mesh scope so the top-level verdict
        // admits; the shared sibling rejects via InsufficientDisk
        // because the local node has only 1 GiB free.
        let mut a_caps = CapabilitySet::new()
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.greedy.scope=mesh");
        a_caps.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.50", a_hex),
        });
        let mut b_caps = CapabilitySet::new()
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.greedy.scope=mesh");
        b_caps.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.50", b_hex),
        });
        let index = CapabilityIndex::new();
        let entity_a = EntityId::from_bytes([0x11; 32]);
        let entity_b = EntityId::from_bytes([0x22; 32]);
        index.index(
            crate::adapter::net::behavior::capability::CapabilityAnnouncement::new(
                100, entity_a, 1, a_caps,
            ),
        );
        index.index(
            crate::adapter::net::behavior::capability::CapabilityAnnouncement::new(
                200, entity_b, 1, b_caps,
            ),
        );

        // Local: 1 GiB free; the 4 GiB sibling fails the disk
        // gate. Top-level hashes are 1 KiB each, so they admit.
        let local = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=1")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.gravity.proximity=128");

        let four_gib: u64 = 4 * (1 << 30);
        let adapter = PrefetchRecorder::new();
        let calls = adapter.calls.clone();
        let report = drive_blob_migration_tick_with_manifest_resolver(
            &local,
            &index,
            &adapter,
            move |h| {
                if h == shared_sibling_hash {
                    Some(four_gib) // exceeds local 1 GiB free
                } else {
                    Some(1024) // top-level hashes are small
                }
            },
            move |h| {
                if h == a_top_hash || h == b_top_hash {
                    Some(vec![(shared_sibling_hash, four_gib)])
                } else {
                    None
                }
            },
        )
        .await;

        // Both top-level hashes admit + prefetch.
        assert_eq!(report.admitted, 2);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
        // The shared sibling rejects InsufficientDisk under BOTH
        // candidates' expansions — post-fix the second
        // expansion's verdict runs because the rejected sibling
        // wasn't trapped in the dedup set. Pre-fix this would
        // have been 1.
        assert_eq!(
            report.rejected_insufficient_disk, 2,
            "rejected siblings must remain reconsiderable across candidates; \
             pre-fix would have been 1 (dedup ate the second expansion)"
        );
    }
}
