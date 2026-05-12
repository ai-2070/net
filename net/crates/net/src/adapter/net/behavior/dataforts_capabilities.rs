//! Dataforts capability typed projections (Phase v0.2 PR-2b).
//!
//! Per-node behavioral traits the substrate uses to gate blob
//! storage participation + greedy / gravity policies:
//!
//! - [`BlobCapability`] — does the node hold blob storage at all,
//!   how much disk is allocated, how much is currently free?
//!   Drives [`super::placement::Artifact::Blob`] eligibility +
//!   `disk_free_gb` weighting.
//! - [`GreedyCapability`] — does the node participate in greedy
//!   chain pulls? Scope + proximity bound the pull radius. The
//!   future `MeshNode::inbound_dispatch` gates `GreedyObserver`
//!   admission on this trait per-node.
//! - [`GravityCapability`] — does the node participate in heat-
//!   driven migration? Same scope + proximity semantics.
//! - [`TopologyScope`] — the four-variant topology boundary
//!   enum (`Node` / `Zone` / `Region` / `Mesh`) shared by both
//!   greedy and gravity. Operators map their failure-domain
//!   tags (`scope:zone:east-1a`, `scope:region:us-east-1`) to
//!   these enum values at policy time.
//!
//! All four types ride the existing [`super::capability::CapabilitySet`]
//! tag set via the `dataforts.*` axis — no new fields on the
//! storage struct, no wire-format break with v0.15 nodes that
//! don't know these tags (unknown axis-prefixed tags pass through
//! as `Tag::Legacy`). See
//! `docs/plans/DATAFORTS_BLOB_STORAGE_PLAN.md` § 7 for the
//! full design rationale.
//!
//! # Tag schema
//!
//! | Tag (sample) | Maps to |
//! |---|---|
//! | `dataforts.blob.storage` | `BlobCapability::storage = true` |
//! | `dataforts.blob.disk_total_gb=64` | `BlobCapability::disk_total_gb = 64` |
//! | `dataforts.blob.disk_free_gb=12` | `BlobCapability::disk_free_gb = 12` |
//! | `dataforts.greedy.enabled` | `GreedyCapability::enabled = true` |
//! | `dataforts.greedy.scope=zone` | `GreedyCapability::scope = Zone` |
//! | `dataforts.greedy.proximity=128` | `GreedyCapability::proximity = 128` |
//! | `dataforts.gravity.enabled` | `GravityCapability::enabled = true` |
//! | `dataforts.gravity.scope=region` | `GravityCapability::scope = Region` |
//! | `dataforts.gravity.proximity=200` | `GravityCapability::proximity = 200` |
//!
//! `dataforts:blob-storage-unhealthy` (note: cross-axis reserved
//! tag — colon, not dot) is the health-gate signal a node emits
//! when local disk crosses 95 %; the placement filter skips
//! `Artifact::Blob` placement against any node carrying that tag.

use serde::{Deserialize, Serialize};

use super::capability::CapabilitySet;
use super::tag::{Tag, TaxonomyAxis};

/// Topology boundary for greedy / gravity policies. Operators map
/// their `scope:*` capability tags (e.g. `scope:zone:rack-a`,
/// `scope:region:us-east-1`) to one of these enum values; the
/// substrate's policy layer interprets it as a hard boundary.
///
/// - [`TopologyScope::Node`] — same node only (debug / single-node
///   policy).
/// - [`TopologyScope::Zone`] — same failure-domain zone (rack /
///   power domain / availability zone). Operator-defined via
///   `scope:zone:*` tags.
/// - [`TopologyScope::Region`] — same region (typically datacenter
///   or cloud region). Operator-defined via `scope:region:*` tags.
/// - [`TopologyScope::Mesh`] — whole mesh (no scope constraint).
///   Default; v0.15-pre-capability behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TopologyScope {
    /// Same node only — debug / single-node policy.
    Node,
    /// Same failure-domain zone (rack / power domain / AZ).
    Zone,
    /// Same region (typically datacenter / cloud region).
    Region,
    /// Whole mesh — no scope constraint. Default.
    #[default]
    Mesh,
}

impl TopologyScope {
    /// Wire form (lowercase). Used as the `dataforts.greedy.scope=…`
    /// / `dataforts.gravity.scope=…` tag value.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Zone => "zone",
            Self::Region => "region",
            Self::Mesh => "mesh",
        }
    }

    /// Inverse of [`Self::as_wire_str`]; case-insensitive parse.
    /// Returns `None` for unknown values — caller decides whether
    /// to default to `Mesh` or surface a typed error.
    pub fn parse_wire(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "node" => Some(Self::Node),
            "zone" => Some(Self::Zone),
            "region" => Some(Self::Region),
            "mesh" => Some(Self::Mesh),
            _ => None,
        }
    }
}

/// Does this node participate in blob storage, and how much disk
/// is allocated to it?
///
/// `storage = false` is the default for nodes that don't carry
/// blobs (compute-only fleets); the placement filter skips such
/// nodes when scoring [`super::placement::Artifact::Blob`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BlobCapability {
    /// Does this node hold blobs at all? Default `false` keeps
    /// v0.15-era nodes invisible to the blob-placement gate.
    pub storage: bool,
    /// Operator-configured cap on blob disk usage in gibibytes.
    /// Independent of the RedEX disk cap; the MeshBlobAdapter
    /// owns this slice. `0` = unspecified.
    pub disk_total_gb: u64,
    /// Currently free space in gibibytes. Drives placement
    /// scoring (prefer nodes with more free space). Updated on
    /// heartbeat cadence; values older than one cadence are
    /// stale.
    pub disk_free_gb: u64,
}

impl BlobCapability {
    /// Convenience constructor for a storage-participating node.
    pub fn storage_participating(disk_total_gb: u64, disk_free_gb: u64) -> Self {
        Self {
            storage: true,
            disk_total_gb,
            disk_free_gb,
        }
    }

    /// Write this projection back into a [`CapabilitySet`] as a
    /// `dataforts.*` tag triple. Producer-side counterpart to
    /// [`Self::from_capability_set`] — operator code can build a
    /// capability set without round-tripping through `add_tag`
    /// strings. Tags emitted:
    ///
    /// - `dataforts.blob.storage` (presence) iff `storage = true`.
    /// - `dataforts.blob.disk_total_gb=<n>` iff `disk_total_gb > 0`.
    /// - `dataforts.blob.disk_free_gb=<n>` iff `disk_free_gb > 0`.
    ///
    /// Zero-valued fields are elided rather than emitted as
    /// `=0` — matches the parser's default-on-absent semantics
    /// so a round-trip through `from_capability_set` returns the
    /// same value. The returned `CapabilitySet` is a new value;
    /// `caps` is consumed in builder style.
    pub fn write_into(self, caps: CapabilitySet) -> CapabilitySet {
        let mut tags = caps.tags;
        if self.storage {
            tags.insert(Tag::AxisPresent {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.storage".to_string(),
            });
        }
        if self.disk_total_gb > 0 {
            tags.insert(Tag::AxisValue {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.disk_total_gb".to_string(),
                value: self.disk_total_gb.to_string(),
                separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
            });
        }
        if self.disk_free_gb > 0 {
            tags.insert(Tag::AxisValue {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.disk_free_gb".to_string(),
                value: self.disk_free_gb.to_string(),
                separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
            });
        }
        CapabilitySet { tags, ..caps }
    }

    /// Read a `BlobCapability` projection out of a [`CapabilitySet`]'s
    /// tag set. Tags absent → field defaults (`storage = false`,
    /// `disk_*_gb = 0`).
    pub fn from_capability_set(caps: &CapabilitySet) -> Self {
        let mut out = Self::default();
        for tag in &caps.tags {
            match tag {
                Tag::AxisPresent { axis, key }
                    if *axis == TaxonomyAxis::Dataforts && key == "blob.storage" =>
                {
                    out.storage = true;
                }
                Tag::AxisValue {
                    axis, key, value, ..
                } if *axis == TaxonomyAxis::Dataforts => match key.as_str() {
                    "blob.disk_total_gb" => {
                        out.disk_total_gb = value.parse().unwrap_or(0);
                    }
                    "blob.disk_free_gb" => {
                        out.disk_free_gb = value.parse().unwrap_or(0);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        out
    }
}

/// Does this node participate in greedy chain pulls, and within
/// what topology / proximity bounds?
///
/// `enabled = false` is the default — v0.15-era nodes don't run
/// greedy admission decisions on inbound events. Operators opt
/// nodes in via [`super::capability::CapabilitySet`] tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GreedyCapability {
    /// Does this node speculatively pull in-scope chains?
    pub enabled: bool,
    /// Topology boundary greedy is allowed to cross.
    pub scope: TopologyScope,
    /// Soft-preference weight, `0..=255`. `0` disables greedy
    /// even when `enabled = true` (operator override without
    /// flipping the flag); high values prefer near peers, low
    /// values tolerate farther peers under cost-tolerant
    /// policies.
    pub proximity: u8,
}

impl GreedyCapability {
    /// Write this projection back into a [`CapabilitySet`] as
    /// `dataforts.greedy.*` tags. Producer-side counterpart to
    /// [`Self::from_capability_set`]. Tags emitted:
    ///
    /// - `dataforts.greedy.enabled` (presence) iff `enabled`.
    /// - `dataforts.greedy.scope=<wire>` when `enabled` (the
    ///   scope claim is only meaningful for participating nodes;
    ///   emitting it on disabled nodes would mislead the parser).
    /// - `dataforts.greedy.proximity=<n>` iff `proximity > 0`.
    pub fn write_into(self, caps: CapabilitySet) -> CapabilitySet {
        let mut tags = caps.tags;
        if self.enabled {
            tags.insert(Tag::AxisPresent {
                axis: TaxonomyAxis::Dataforts,
                key: "greedy.enabled".to_string(),
            });
            tags.insert(Tag::AxisValue {
                axis: TaxonomyAxis::Dataforts,
                key: "greedy.scope".to_string(),
                value: self.scope.as_wire_str().to_string(),
                separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
            });
        }
        if self.proximity > 0 {
            tags.insert(Tag::AxisValue {
                axis: TaxonomyAxis::Dataforts,
                key: "greedy.proximity".to_string(),
                value: self.proximity.to_string(),
                separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
            });
        }
        CapabilitySet { tags, ..caps }
    }

    /// Read the projection from a [`CapabilitySet`].
    pub fn from_capability_set(caps: &CapabilitySet) -> Self {
        let mut out = Self::default();
        for tag in &caps.tags {
            match tag {
                Tag::AxisPresent { axis, key }
                    if *axis == TaxonomyAxis::Dataforts && key == "greedy.enabled" =>
                {
                    out.enabled = true;
                }
                Tag::AxisValue {
                    axis, key, value, ..
                } if *axis == TaxonomyAxis::Dataforts => match key.as_str() {
                    "greedy.scope" => {
                        if let Some(s) = TopologyScope::parse_wire(value) {
                            out.scope = s;
                        }
                    }
                    "greedy.proximity" => {
                        out.proximity = value.parse().unwrap_or(0);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        out
    }
}

/// Does this node participate in heat-driven migration, and
/// within what topology / proximity bounds?
///
/// Same shape as [`GreedyCapability`] — gravity is a long-term
/// drift policy that's orthogonal to greedy's per-event admission;
/// the two flip independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GravityCapability {
    /// Does this node accept heat-driven migrations?
    pub enabled: bool,
    /// Topology boundary gravity is allowed to migrate across.
    pub scope: TopologyScope,
    /// Soft-preference weight, `0..=255`. `0` disables gravity
    /// even when `enabled = true`.
    pub proximity: u8,
}

impl GravityCapability {
    /// Write this projection back into a [`CapabilitySet`] as
    /// `dataforts.gravity.*` tags. Producer-side counterpart to
    /// [`Self::from_capability_set`]. Tag emission shape mirrors
    /// [`GreedyCapability::write_into`].
    pub fn write_into(self, caps: CapabilitySet) -> CapabilitySet {
        let mut tags = caps.tags;
        if self.enabled {
            tags.insert(Tag::AxisPresent {
                axis: TaxonomyAxis::Dataforts,
                key: "gravity.enabled".to_string(),
            });
            tags.insert(Tag::AxisValue {
                axis: TaxonomyAxis::Dataforts,
                key: "gravity.scope".to_string(),
                value: self.scope.as_wire_str().to_string(),
                separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
            });
        }
        if self.proximity > 0 {
            tags.insert(Tag::AxisValue {
                axis: TaxonomyAxis::Dataforts,
                key: "gravity.proximity".to_string(),
                value: self.proximity.to_string(),
                separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
            });
        }
        CapabilitySet { tags, ..caps }
    }

    /// Read the projection from a [`CapabilitySet`].
    pub fn from_capability_set(caps: &CapabilitySet) -> Self {
        let mut out = Self::default();
        for tag in &caps.tags {
            match tag {
                Tag::AxisPresent { axis, key }
                    if *axis == TaxonomyAxis::Dataforts && key == "gravity.enabled" =>
                {
                    out.enabled = true;
                }
                Tag::AxisValue {
                    axis, key, value, ..
                } if *axis == TaxonomyAxis::Dataforts => match key.as_str() {
                    "gravity.scope" => {
                        if let Some(s) = TopologyScope::parse_wire(value) {
                            out.scope = s;
                        }
                    }
                    "gravity.proximity" => {
                        out.proximity = value.parse().unwrap_or(0);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        out
    }
}

/// Reserved cross-axis tag a node advertises when local blob
/// storage crosses 95 % disk — the placement filter skips
/// `Artifact::Blob` placement against any node carrying this tag.
/// Cleared when disk drops below 85 % (hysteresis).
pub const BLOB_STORAGE_UNHEALTHY_TAG: &str = "dataforts:blob-storage-unhealthy";

/// `true` iff the [`CapabilitySet`] carries the
/// [`BLOB_STORAGE_UNHEALTHY_TAG`] reserved tag. Pure-logic check;
/// no allocation.
pub fn is_blob_storage_unhealthy(caps: &CapabilitySet) -> bool {
    caps.tags.iter().any(|t| match t {
        // The tag is `dataforts:blob-storage-unhealthy`. The `Tag`
        // parser splits at the colon → prefix `dataforts:`, body
        // `blob-storage-unhealthy`.
        Tag::Reserved { prefix, body } => {
            prefix == "dataforts:" && body == "blob-storage-unhealthy"
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilitySet;

    #[test]
    fn topology_scope_wire_round_trip() {
        for s in [
            TopologyScope::Node,
            TopologyScope::Zone,
            TopologyScope::Region,
            TopologyScope::Mesh,
        ] {
            assert_eq!(TopologyScope::parse_wire(s.as_wire_str()), Some(s));
        }
        // Case-insensitive
        assert_eq!(TopologyScope::parse_wire("ZONE"), Some(TopologyScope::Zone));
        // Unknown → None
        assert_eq!(TopologyScope::parse_wire("galaxy"), None);
    }

    #[test]
    fn topology_scope_default_is_mesh() {
        assert_eq!(TopologyScope::default(), TopologyScope::Mesh);
    }

    #[test]
    fn blob_capability_default_is_non_participating() {
        let bc = BlobCapability::default();
        assert!(!bc.storage);
        assert_eq!(bc.disk_total_gb, 0);
        assert_eq!(bc.disk_free_gb, 0);
    }

    #[test]
    fn blob_capability_reads_storage_present_tag() {
        let caps = CapabilitySet::new().add_tag("dataforts.blob.storage");
        let bc = BlobCapability::from_capability_set(&caps);
        assert!(bc.storage);
    }

    #[test]
    fn blob_capability_reads_disk_gb_tags() {
        let caps = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=64")
            .add_tag("dataforts.blob.disk_free_gb=12");
        let bc = BlobCapability::from_capability_set(&caps);
        assert!(bc.storage);
        assert_eq!(bc.disk_total_gb, 64);
        assert_eq!(bc.disk_free_gb, 12);
    }

    #[test]
    fn greedy_capability_default_is_disabled() {
        let g = GreedyCapability::default();
        assert!(!g.enabled);
        assert_eq!(g.scope, TopologyScope::Mesh);
        assert_eq!(g.proximity, 0);
    }

    #[test]
    fn greedy_capability_reads_all_fields() {
        let caps = CapabilitySet::new()
            .add_tag("dataforts.greedy.enabled")
            .add_tag("dataforts.greedy.scope=zone")
            .add_tag("dataforts.greedy.proximity=200");
        let g = GreedyCapability::from_capability_set(&caps);
        assert!(g.enabled);
        assert_eq!(g.scope, TopologyScope::Zone);
        assert_eq!(g.proximity, 200);
    }

    #[test]
    fn gravity_capability_reads_all_fields() {
        let caps = CapabilitySet::new()
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=region")
            .add_tag("dataforts.gravity.proximity=64");
        let g = GravityCapability::from_capability_set(&caps);
        assert!(g.enabled);
        assert_eq!(g.scope, TopologyScope::Region);
        assert_eq!(g.proximity, 64);
    }

    #[test]
    fn greedy_and_gravity_dont_cross_read() {
        // Pin that a `dataforts.greedy.*` tag doesn't bleed into
        // GravityCapability and vice versa.
        let caps = CapabilitySet::new()
            .add_tag("dataforts.greedy.enabled")
            .add_tag("dataforts.greedy.proximity=255");
        let g = GravityCapability::from_capability_set(&caps);
        assert!(!g.enabled);
        assert_eq!(g.proximity, 0);
    }

    #[test]
    fn parse_handles_garbage_values_gracefully() {
        // Non-numeric values for *_gb / proximity → 0 default,
        // not panic.
        let caps = CapabilitySet::new()
            .add_tag("dataforts.blob.disk_total_gb=not-a-number")
            .add_tag("dataforts.greedy.proximity=oops");
        let bc = BlobCapability::from_capability_set(&caps);
        let g = GreedyCapability::from_capability_set(&caps);
        assert_eq!(bc.disk_total_gb, 0);
        assert_eq!(g.proximity, 0);
    }

    #[test]
    fn unknown_scope_value_falls_back_to_default() {
        let caps = CapabilitySet::new()
            .add_tag("dataforts.greedy.enabled")
            .add_tag("dataforts.greedy.scope=galaxy");
        let g = GreedyCapability::from_capability_set(&caps);
        // Unknown scope value stays at the field default (Mesh).
        assert_eq!(g.scope, TopologyScope::Mesh);
    }

    // --- Typed setters / round-trip (PR-5k) ---

    #[test]
    fn blob_capability_write_into_round_trips() {
        let original = BlobCapability::storage_participating(100, 42);
        let caps = original.write_into(CapabilitySet::new());
        let read_back = BlobCapability::from_capability_set(&caps);
        assert_eq!(read_back, original);
    }

    #[test]
    fn blob_capability_write_into_skips_zero_fields() {
        let bc = BlobCapability {
            storage: true,
            disk_total_gb: 0,
            disk_free_gb: 0,
        };
        let caps = bc.write_into(CapabilitySet::new());
        // Storage tag emitted; disk-gb tags not (matches the
        // parser's default-on-absent semantics).
        assert!(caps
            .tags
            .iter()
            .any(|t| matches!(t, Tag::AxisPresent { axis, key }
                if *axis == TaxonomyAxis::Dataforts && key == "blob.storage")));
        assert!(!caps.tags.iter().any(|t| matches!(t,
            Tag::AxisValue { axis, key, .. }
            if *axis == TaxonomyAxis::Dataforts && key == "blob.disk_total_gb")));
    }

    #[test]
    fn greedy_capability_write_into_round_trips() {
        let original = GreedyCapability {
            enabled: true,
            scope: TopologyScope::Zone,
            proximity: 128,
        };
        let caps = original.write_into(CapabilitySet::new());
        let read_back = GreedyCapability::from_capability_set(&caps);
        assert_eq!(read_back, original);
    }

    #[test]
    fn greedy_capability_disabled_skips_all_tags() {
        let g = GreedyCapability::default();
        let caps = g.write_into(CapabilitySet::new());
        assert!(caps.tags.is_empty(), "disabled greedy must emit no tags");
    }

    #[test]
    fn gravity_capability_write_into_round_trips() {
        let original = GravityCapability {
            enabled: true,
            scope: TopologyScope::Region,
            proximity: 64,
        };
        let caps = original.write_into(CapabilitySet::new());
        let read_back = GravityCapability::from_capability_set(&caps);
        assert_eq!(read_back, original);
    }

    #[test]
    fn capability_set_with_typed_builders_round_trip() {
        // Compose all three typed projections via the
        // CapabilitySet builder methods, then read each back.
        let blob = BlobCapability::storage_participating(100, 50);
        let greedy = GreedyCapability {
            enabled: true,
            scope: TopologyScope::Mesh,
            proximity: 128,
        };
        let gravity = GravityCapability {
            enabled: true,
            scope: TopologyScope::Region,
            proximity: 64,
        };
        let caps = CapabilitySet::new()
            .with_blob_capability(blob)
            .with_greedy_capability(greedy)
            .with_gravity_capability(gravity);
        assert_eq!(BlobCapability::from_capability_set(&caps), blob);
        assert_eq!(GreedyCapability::from_capability_set(&caps), greedy);
        assert_eq!(GravityCapability::from_capability_set(&caps), gravity);
    }
}
