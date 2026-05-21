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
use crate::adapter::net::behavior::dataforts_capabilities::{
    GravityCapability, GreedyCapability, TopologyScope,
};
use crate::adapter::net::behavior::tag::{AxisSeparator, Tag};
use crate::adapter::net::behavior::TaxonomyAxis;

use super::adapter::BlobAdapter;
use super::admission::{should_migrate_blob_to, MigrateBlobReject, MigrateBlobVerdict};
use super::blob_ref::BlobRef;

/// `true` when `a` is at least as narrow as `b` in the
/// `Node < Zone < Region < Mesh` ordering. Mirrors
/// `scope_at_least_as_narrow` in `admission`; duplicated here so
/// the migration controller can compute the narrowest claim
/// across a set of heat advertisers without importing the
/// admission internals.
fn scope_at_least_as_narrow(a: TopologyScope, b: TopologyScope) -> bool {
    use TopologyScope::*;
    matches!(
        (a, b),
        (Node, _) | (Zone, Zone | Region | Mesh) | (Region, Region | Mesh) | (Mesh, Mesh)
    )
}

/// Choose the narrower of two scope claims (Node < Zone < Region
/// < Mesh). Used to floor publisher scope to the narrowest claim
/// across all peers advertising the same blob.
fn narrower_scope(a: TopologyScope, b: TopologyScope) -> TopologyScope {
    if scope_at_least_as_narrow(a, b) {
        a
    } else {
        b
    }
}

/// Replace the `dataforts.greedy.scope` and `dataforts.gravity.scope`
/// tags in `caps` with the supplied floors. Used to apply
/// cross-advertiser scope narrowing in
/// `BlobMigrationController::candidates` — a single cache holder
/// claiming a wider scope than the original publisher cannot
/// launder admission decisions when the controller intersects
/// every claimant's scope.
fn narrow_scope_tags_in(
    caps: &mut CapabilitySet,
    gravity_floor: TopologyScope,
    greedy_floor: TopologyScope,
) {
    caps.tags.retain(|t| match t {
        Tag::AxisValue { axis, key, .. } if *axis == TaxonomyAxis::Dataforts => {
            key != "greedy.scope" && key != "gravity.scope"
        }
        _ => true,
    });
    caps.tags.insert(Tag::AxisValue {
        axis: TaxonomyAxis::Dataforts,
        key: "greedy.scope".to_string(),
        value: greedy_floor.as_wire_str().to_string(),
        separator: AxisSeparator::Eq,
    });
    caps.tags.insert(Tag::AxisValue {
        axis: TaxonomyAxis::Dataforts,
        key: "gravity.scope".to_string(),
        value: gravity_floor.as_wire_str().to_string(),
        separator: AxisSeparator::Eq,
    });
}

/// Decode a single lowercase-hex ASCII byte into its nibble
/// value (0..=15). Returns `None` for any byte outside
/// `b'0'..=b'9'` or `b'a'..=b'f'`. Used by
/// [`parse_blob_heat_tag`] in place of `u8::from_str_radix(_, 16)`
/// per dataforts perf #185.
#[inline]
fn nibble_lowercase(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

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
    // Reject mixed-case hex — the producer-side encoder writes
    // lowercase, and accepting uppercase here lets two tags with
    // the same logical hash but different cased representations
    // create distinct gravity-decision entries downstream. Tighten
    // the wire shape to the encoder's canonical form.
    if hex.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    // Per dataforts perf #185: pre-fix this used
    // `u8::from_str_radix(pair, 16)` per byte, which routes
    // through the general-purpose radix parser (UTF-8 decode +
    // range check + multiply-accumulate). The path here is
    // already known-shape after the validations above (length =
    // 64, lowercase). A two-nibble lookup is enough and skips the
    // sub-slice + parser dispatch on every byte. Returns `None`
    // on any non-hex character so the existing reject-bad-hex
    // contract is preserved.
    let mut hash = [0u8; 32];
    let bytes = hex.as_bytes();
    for (i, byte) in hash.iter_mut().enumerate() {
        let hi = nibble_lowercase(bytes[2 * i])?;
        let lo = nibble_lowercase(bytes[2 * i + 1])?;
        *byte = (hi << 4) | lo;
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
    ///
    /// ## Cross-advertiser scope narrowing
    ///
    /// `should_migrate_blob_to` reads the publisher's claimed
    /// scope via `dataforts.{greedy,gravity}.scope` tags on
    /// `publisher_caps`. The heat emitter is whichever peer
    /// advertised the heat tag — that can be the original
    /// publisher or any cache holder. A malicious cache holder
    /// advertising a *wider* scope than the original publisher
    /// would otherwise launder admission decisions on every
    /// target: the heat-emitter caps drive the verdict.
    ///
    /// To defend against this, each candidate's `publisher_caps`
    /// has its scope tags floored to the *narrowest* gravity /
    /// greedy scope across every peer that advertises heat for
    /// the same hash. Only peers with the respective `enabled`
    /// flag contribute — a peer not participating in
    /// gravity/greedy makes no scope claim and is excluded.
    /// Identical hashes from a single advertiser are unchanged
    /// (narrowest of one = that one). Multiple advertisers
    /// claiming different scopes collapse to the narrowest, which
    /// is what the most conservative publisher in the set would
    /// have wanted.
    pub fn candidates(&self) -> Vec<BlobMigrationCandidate> {
        // Pass 1: walk every peer, record (node_id, hash, rate)
        // tuples + aggregate the narrowest scope claim per hash.
        // Cache each peer's caps once (NOT per heat tag) so a peer
        // with N blob-heat tags doesn't force N clones of its full
        // CapabilitySet into the intermediate buffer.
        struct ScopeFloor {
            gravity: Option<TopologyScope>,
            greedy: Option<TopologyScope>,
        }
        let mut raw: Vec<(u64, [u8; 32], f64)> = Vec::new();
        let mut floors: std::collections::HashMap<[u8; 32], ScopeFloor> =
            std::collections::HashMap::new();
        let mut peer_caps_cache: std::collections::HashMap<u64, CapabilitySet> =
            std::collections::HashMap::new();

        for node_id in self.capability_index.all_nodes() {
            let caps = match self.capability_index.get(node_id) {
                Some(c) => c,
                None => continue,
            };
            // Peer's typed scope claims (only valid when the
            // matching `enabled` flag is set; an unparticipating
            // peer makes no claim and is excluded from the floor).
            let peer_gravity = GravityCapability::from_capability_set(&caps);
            let peer_greedy = GreedyCapability::from_capability_set(&caps);

            let mut emitted_any = false;
            for tag in &caps.tags {
                if let Some((hash, rate)) = parse_blob_heat_tag(tag) {
                    let entry = floors.entry(hash).or_insert(ScopeFloor {
                        gravity: None,
                        greedy: None,
                    });
                    if peer_gravity.enabled {
                        entry.gravity = Some(match entry.gravity {
                            Some(prev) => narrower_scope(prev, peer_gravity.scope),
                            None => peer_gravity.scope,
                        });
                    }
                    if peer_greedy.enabled {
                        entry.greedy = Some(match entry.greedy {
                            Some(prev) => narrower_scope(prev, peer_greedy.scope),
                            None => peer_greedy.scope,
                        });
                    }
                    raw.push((node_id, hash, rate));
                    emitted_any = true;
                }
            }
            if emitted_any {
                peer_caps_cache.insert(node_id, caps);
            }
        }

        // Pass 2: emit candidates, applying the per-hash floor.
        // One CapabilitySet clone per candidate at emission time
        // (so each candidate carries its own owned, narrowed
        // view); the per-peer cache above is the source of truth
        // and was populated with exactly one snapshot per peer.
        let mut out = Vec::with_capacity(raw.len());
        for (node_id, hash, rate) in raw {
            let mut caps = match peer_caps_cache.get(&node_id) {
                Some(c) => c.clone(),
                None => continue, // shouldn't happen — defensive
            };
            if let Some(floor) = floors.get(&hash) {
                if let (Some(g), Some(gr)) = (floor.gravity, floor.greedy) {
                    narrow_scope_tags_in(&mut caps, g, gr);
                } else if let Some(g) = floor.gravity {
                    let cur_greedy = GreedyCapability::from_capability_set(&caps).scope;
                    narrow_scope_tags_in(&mut caps, g, cur_greedy);
                } else if let Some(gr) = floor.greedy {
                    let cur_gravity = GravityCapability::from_capability_set(&caps).scope;
                    narrow_scope_tags_in(&mut caps, cur_gravity, gr);
                }
                // No enabled peer made any scope claim: leave caps as-is.
            }
            out.push(BlobMigrationCandidate {
                hash,
                publisher_node_id: node_id,
                publisher_caps: caps,
                rate,
            });
        }
        out
    }
}

/// Per-peer admit budget applied inside one tick of the migration
/// loop. `MAX_BLOB_HEAT_TAGS_PER_ANNOUNCE` (256) bounds how many
/// `heat:blob:` tags a single peer can survive the wire filter
/// with; without an additional per-peer cap at the controller, a
/// single adversarial peer could still drive 256 chunk-channel
/// opens (each spawning a replication runtime) per tick. The
/// budget caps it at a tighter steady-state number; operators
/// running honest hot working sets larger than this can construct
/// the controller manually and call `candidates()` themselves.
pub const DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK: usize = 32;

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
    /// Candidates that passed every verdict gate but were skipped
    /// because the per-peer admit budget for this tick was
    /// exhausted. Operators monitor this counter to detect peers
    /// trying to drive prefetch-volume amplification past the
    /// wire-side `MAX_BLOB_HEAT_TAGS_PER_ANNOUNCE` cap; a high
    /// rate sustained over many ticks usually means the cap is
    /// too tight for legitimate working sets and should be
    /// raised, OR that the peer is adversarial.
    pub skipped_peer_budget: u64,
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
    // Per-peer admit count for this tick. Capped at
    // DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK to bound the
    // chunk-channel-open amplification a single peer can drive
    // even after surviving the wire-side heat-tag filter.
    let mut peer_admits: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
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
                let count = peer_admits.entry(candidate.publisher_node_id).or_insert(0);
                if *count >= DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK {
                    report.skipped_peer_budget += 1;
                    continue;
                }
                *count += 1;
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
    // Per-peer admit budget — see drive_blob_migration_tick.
    // Sibling prefetches charge against the heat-emitter peer's
    // budget too; one heat tag can't smuggle in an unbounded
    // sibling fan-out.
    let mut peer_admits: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
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
                let count = peer_admits.entry(candidate.publisher_node_id).or_insert(0);
                if *count >= DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK {
                    report.skipped_peer_budget += 1;
                    continue;
                }
                *count += 1;
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
                            let count = peer_admits.entry(candidate.publisher_node_id).or_insert(0);
                            if *count >= DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK {
                                report.skipped_peer_budget += 1;
                                continue;
                            }
                            *count += 1;
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

use super::hex32;

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

    /// Pin dataforts perf #185: the table-lookup hex decode
    /// inside `parse_blob_heat_tag` produces the same hash as
    /// the legacy `u8::from_str_radix(pair, 16)` form for every
    /// valid lowercase-hex shape. Exercise the full nibble range
    /// (corner cases all-zero / all-`f` / ascending) so a
    /// regression in the nibble table (off-by-one on `b - b'a'`,
    /// missing the `0..=9` arm, or returning the wrong nibble
    /// half for the high/low split) surfaces here.
    #[test]
    fn parse_blob_heat_tag_decode_matches_from_str_radix_byte_for_byte() {
        let cases: [[u8; 32]; 4] = [
            [0x00; 32],
            [0xFF; 32],
            {
                let mut a = [0u8; 32];
                for (i, b) in a.iter_mut().enumerate() {
                    *b = i as u8;
                }
                a
            },
            [
                0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe, 0xf0, 0x0d, 0xfa, 0xce, 0x1b, 0xad,
                0xd0, 0x0d, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0xfe, 0xdc, 0xba, 0x98,
                0x76, 0x54, 0x32, 0x10,
            ],
        ];
        for expected in &cases {
            let mut hex = String::with_capacity(64);
            for b in expected {
                use std::fmt::Write as _;
                write!(hex, "{:02x}", b).unwrap();
            }
            let tag = Tag::Reserved {
                prefix: "heat:".to_string(),
                body: format!("blob:{}=1.00", hex),
            };
            let (got, _rate) = parse_blob_heat_tag(&tag).expect("must parse");
            assert_eq!(&got, expected, "table-lookup decode must match legacy");
        }

        // Negative case: a non-hex char (after the lowercase-only
        // gate) must still be rejected — the new lookup returns
        // `None`, mirroring `from_str_radix`'s error.
        let bad = Tag::Reserved {
            prefix: "heat:".to_string(),
            // 'g' is past 'f' so the nibble table rejects it
            // even though it survives the lowercase gate.
            body: format!("blob:g{}=1.00", "0".repeat(63)),
        };
        assert!(parse_blob_heat_tag(&bad).is_none());
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

    /// A wider-scope cache holder advertising heat for a blob
    /// must not be able to launder the migration verdict past a
    /// narrower-scope publisher's intent. Candidates from both
    /// peers see their `publisher_caps` floored to the narrowest
    /// scope (Zone), so a local target with Region scope rejects
    /// admission even though the cache holder's raw caps were
    /// Mesh.
    #[test]
    fn cross_advertiser_scope_is_floored_to_narrowest_claim() {
        let (hash, hex) = hex64(0xCA);
        // Original publisher: gravity scope = Zone (narrow).
        let mut publisher = CapabilitySet::new()
            .with_gravity_capability(crate::adapter::net::behavior::GravityCapability {
                enabled: true,
                scope: TopologyScope::Zone,
                proximity: 128,
            })
            .with_greedy_capability(crate::adapter::net::behavior::GreedyCapability {
                enabled: true,
                scope: TopologyScope::Zone,
                proximity: 128,
            });
        publisher.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.50", hex),
        });
        // Malicious cache holder: gravity scope = Mesh (wide).
        let mut cache_holder = CapabilitySet::new()
            .with_gravity_capability(crate::adapter::net::behavior::GravityCapability {
                enabled: true,
                scope: TopologyScope::Mesh,
                proximity: 128,
            })
            .with_greedy_capability(crate::adapter::net::behavior::GreedyCapability {
                enabled: true,
                scope: TopologyScope::Mesh,
                proximity: 128,
            });
        cache_holder.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.40", hex),
        });
        let index = CapabilityIndex::new();
        index.index(
            crate::adapter::net::behavior::capability::CapabilityAnnouncement::new(
                10,
                EntityId::from_bytes([0xAA; 32]),
                1,
                publisher,
            ),
        );
        index.index(
            crate::adapter::net::behavior::capability::CapabilityAnnouncement::new(
                20,
                EntityId::from_bytes([0xBB; 32]),
                1,
                cache_holder,
            ),
        );

        let local = CapabilitySet::default();
        let controller = BlobMigrationController::new(&local, &index);
        let candidates = controller.candidates();
        assert_eq!(candidates.len(), 2);
        for c in &candidates {
            assert_eq!(c.hash, hash);
            let gravity = GravityCapability::from_capability_set(&c.publisher_caps);
            let greedy = GreedyCapability::from_capability_set(&c.publisher_caps);
            assert_eq!(
                gravity.scope,
                TopologyScope::Zone,
                "gravity scope floors to narrowest across all advertisers"
            );
            assert_eq!(
                greedy.scope,
                TopologyScope::Zone,
                "greedy scope floors to narrowest across all advertisers"
            );
        }
    }

    /// The narrowing must IGNORE peers that don't actually
    /// participate in gravity / greedy (no `.enabled` flag). A
    /// peer claiming scope=Node but not enabled doesn't make a
    /// real scope claim and shouldn't sink the floor.
    #[test]
    fn scope_narrowing_ignores_unparticipating_peers() {
        let (_, hex) = hex64(0xCB);
        // Real publisher: gravity Zone (enabled).
        let mut publisher = CapabilitySet::new().with_gravity_capability(
            crate::adapter::net::behavior::GravityCapability {
                enabled: true,
                scope: TopologyScope::Zone,
                proximity: 128,
            },
        );
        publisher.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.50", hex),
        });
        // A peer that announces a Node-scope tag but doesn't have
        // `gravity.enabled`. Their unparticipating claim must NOT
        // pull the floor to Node.
        let mut tag_only = CapabilitySet::new();
        tag_only.tags.insert(Tag::AxisValue {
            axis: TaxonomyAxis::Dataforts,
            key: "gravity.scope".to_string(),
            value: "node".to_string(),
            separator: AxisSeparator::Eq,
        });
        tag_only.tags.insert(Tag::Reserved {
            prefix: "heat:".to_string(),
            body: format!("blob:{}=0.30", hex),
        });
        let index = CapabilityIndex::new();
        index.index(
            crate::adapter::net::behavior::capability::CapabilityAnnouncement::new(
                30,
                EntityId::from_bytes([0xCC; 32]),
                1,
                publisher,
            ),
        );
        index.index(
            crate::adapter::net::behavior::capability::CapabilityAnnouncement::new(
                40,
                EntityId::from_bytes([0xDD; 32]),
                1,
                tag_only,
            ),
        );

        let local = CapabilitySet::default();
        let controller = BlobMigrationController::new(&local, &index);
        let candidates = controller.candidates();
        for c in &candidates {
            let gravity = GravityCapability::from_capability_set(&c.publisher_caps);
            assert_eq!(
                gravity.scope,
                TopologyScope::Zone,
                "unparticipating peer's tag must not narrow the floor"
            );
        }
    }

    /// Per-peer admit budget: a single peer advertising N >
    /// DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK heat tags must
    /// see only the budgeted number reach `adapter.prefetch`. The
    /// overflow surfaces via `skipped_peer_budget`.
    #[tokio::test]
    async fn drive_tick_caps_per_peer_admits_at_budget() {
        let mut publisher_caps = CapabilitySet::new()
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.greedy.scope=mesh");
        // Stuff in 2× the budget of distinct blob-heat tags.
        let flood = DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK * 2;
        for i in 0..flood {
            let mut hash = [0u8; 32];
            hash[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            let mut hex = String::with_capacity(64);
            for b in &hash {
                use std::fmt::Write;
                let _ = write!(hex, "{:02x}", b);
            }
            publisher_caps.tags.insert(Tag::Reserved {
                prefix: "heat:".to_string(),
                body: format!("blob:{}=0.50", hex),
            });
        }
        let index = index_with_peer_heat(77, publisher_caps, 0x77);
        let local = participating_local("mesh", 128, 1024);
        let adapter = PrefetchRecorder::new();
        let calls = adapter.calls.clone();
        let report = drive_blob_migration_tick(&local, &index, &adapter, |_| Some(1024)).await;

        assert_eq!(
            report.admitted as usize, DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK,
            "per-peer admit budget caps the prefetch fan-out"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed) as usize,
            DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK
        );
        assert_eq!(
            report.skipped_peer_budget as usize,
            flood - DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK,
            "overflow tags route into skipped_peer_budget"
        );
    }

    /// Two peers each fully use their budget; the per-peer counters
    /// are independent so total admits = 2 × budget.
    #[tokio::test]
    async fn drive_tick_per_peer_budgets_are_independent() {
        let make_caps_with_heat = |scope: &str, seed_base: u8, count: usize| {
            let mut caps = CapabilitySet::new()
                .add_tag(format!("dataforts.gravity.scope={}", scope))
                .add_tag(format!("dataforts.greedy.scope={}", scope));
            for i in 0..count {
                let mut hash = [0u8; 32];
                hash[0] = seed_base;
                hash[1..9].copy_from_slice(&(i as u64).to_le_bytes());
                let mut hex = String::with_capacity(64);
                for b in &hash {
                    use std::fmt::Write;
                    let _ = write!(hex, "{:02x}", b);
                }
                caps.tags.insert(Tag::Reserved {
                    prefix: "heat:".to_string(),
                    body: format!("blob:{}=0.50", hex),
                });
            }
            caps
        };
        let flood = DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK + 4;
        let index = CapabilityIndex::new();
        let entity_a = EntityId::from_bytes([0xA1; 32]);
        let entity_b = EntityId::from_bytes([0xB2; 32]);
        index.index(
            crate::adapter::net::behavior::capability::CapabilityAnnouncement::new(
                111,
                entity_a,
                1,
                make_caps_with_heat("mesh", 0xA0, flood),
            ),
        );
        index.index(
            crate::adapter::net::behavior::capability::CapabilityAnnouncement::new(
                222,
                entity_b,
                1,
                make_caps_with_heat("mesh", 0xB0, flood),
            ),
        );
        let local = participating_local("mesh", 128, 1024);
        let adapter = PrefetchRecorder::new();
        let report = drive_blob_migration_tick(&local, &index, &adapter, |_| Some(1024)).await;

        assert_eq!(
            report.admitted as usize,
            2 * DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK,
            "two peers each hit their own budget independently"
        );
        assert_eq!(
            report.skipped_peer_budget as usize,
            2 * (flood - DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK),
        );
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
