//! Subprotocol version negotiation.
//!
//! Peers exchange `SubprotocolManifest` messages during session setup to
//! determine which subprotocols they can communicate on. The negotiation
//! is a pure function — no network I/O.

use std::collections::HashSet;

use bytes::{Buf, BufMut};

use super::descriptor::{
    read_manifest_entry, write_manifest_entry, SubprotocolDescriptor, SubprotocolVersion,
    MANIFEST_ENTRY_SIZE,
};
use super::registry::SubprotocolRegistry;

/// A manifest of subprotocols supported by a peer.
///
/// Exchanged during session setup on `subprotocol_id = 0x0600`.
#[derive(Debug, Clone)]
pub struct SubprotocolManifest {
    /// Entries describing each supported subprotocol.
    pub entries: Vec<ManifestEntry>,
}

/// A single entry in a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Subprotocol ID.
    pub id: u16,
    /// Handler version.
    pub version: SubprotocolVersion,
    /// Minimum compatible version.
    pub min_compatible: SubprotocolVersion,
}

impl SubprotocolManifest {
    /// Build a manifest from the local registry.
    ///
    /// Entries are sorted by `id` so the wire bytes are
    /// deterministic across runs and builds. Walking
    /// `registry.list()` directly returns a `DashMap` iteration
    /// order that is non-deterministic, which would produce
    /// different `to_bytes()` output for identical content. Today
    /// the manifest is unsigned and not used in any digest dedup,
    /// so the non-determinism would be dormant — but the
    /// architecture comment at the top of this module describes
    /// the manifest as "exchanged during session setup," a
    /// surface that typically ends up signed once a security
    /// model is added; sort by id preempts that breakage.
    ///
    /// Forwarding-only descriptors (`handler_present == false`)
    /// are filtered out before serialization. The 6-byte wire
    /// format has no `handler_present` flag and `from_bytes`
    /// reconstructs every entry as `handler_present: true`.
    /// Without this filter, the receiver would believe the sender
    /// had a local handler for every id in the manifest and
    /// scheduling RPCs to that id would silently drop on the
    /// sender. The parallel `capability_tags()` discovery path
    /// already filters by `handler_present`; this direct-manifest
    /// path mirrors that filter so the two channels agree.
    /// Forwarding-only peers can still receive opaque-forward
    /// traffic; they just don't claim to handle it locally.
    pub fn from_registry(registry: &SubprotocolRegistry) -> Self {
        let mut entries: Vec<ManifestEntry> = registry
            .list()
            .into_iter()
            .filter(|d| d.handler_present)
            .map(|d| ManifestEntry {
                id: d.id,
                version: d.version,
                min_compatible: d.min_compatible,
            })
            .collect();
        entries.sort_by_key(|e| e.id);
        Self { entries }
    }

    /// Serialize to bytes.
    ///
    /// Wire format: `[count: u16][entries: count * 6 bytes]`
    #[expect(
        clippy::expect_used,
        reason = "subprotocol count is bounded well below u16::MAX (65535) by the subprotocol registry; an overrun is an upstream invariant violation"
    )]
    pub fn to_bytes(&self) -> Vec<u8> {
        let count = u16::try_from(self.entries.len()).expect("too many subprotocols");
        let mut buf = Vec::with_capacity(2 + self.entries.len() * MANIFEST_ENTRY_SIZE);
        buf.put_u16_le(count);
        for entry in &self.entries {
            let desc = SubprotocolDescriptor {
                id: entry.id,
                name: String::new(),
                version: entry.version,
                min_compatible: entry.min_compatible,
                handler_present: true,
            };
            write_manifest_entry(&desc, &mut buf);
        }
        buf
    }

    /// Deserialize from bytes.
    ///
    /// Previously this accepted trailing garbage past the declared
    /// `count` entries, and never de-duplicated entry `id`s. A peer
    /// could advertise the same subprotocol id twice — once with a
    /// strict version, once with a permissive one — and whichever
    /// landed last in `remote_by_id` would win, enabling a downgrade
    /// attack. Now both inputs are rejected with `None`, matching
    /// the strict-length contract on `IdentityEnvelope::from_bytes`
    /// and
    /// `PermissionToken::from_bytes`.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 2 {
            return None;
        }
        let mut cursor = data;
        let count = cursor.get_u16_le() as usize;
        let expected_body = count.checked_mul(MANIFEST_ENTRY_SIZE)?;

        // Strict-length: the cursor must hold exactly `count`
        // entries, no more, no less.
        if cursor.remaining() != expected_body {
            return None;
        }

        let mut entries = Vec::with_capacity(count);
        let mut seen_ids: HashSet<u16> = HashSet::with_capacity(count);
        for _ in 0..count {
            let (id, version, min_compatible) = read_manifest_entry(&mut cursor)?;
            // Reject duplicate ids — a peer can only declare each
            // subprotocol once.
            if !seen_ids.insert(id) {
                return None;
            }
            entries.push(ManifestEntry {
                id,
                version,
                min_compatible,
            });
        }

        Some(Self { entries })
    }
}

/// Result of negotiation between two peers.
#[derive(Debug, Clone)]
pub struct NegotiatedSet {
    /// Subprotocol IDs that both peers support at compatible versions.
    pub compatible: HashSet<u16>,
    /// Subprotocol IDs where version mismatch was detected.
    /// Tuple: (id, local_version, remote_version).
    pub incompatible: Vec<(u16, SubprotocolVersion, SubprotocolVersion)>,
}

impl NegotiatedSet {
    /// Check if a subprotocol is negotiated (compatible on both sides).
    #[inline]
    pub fn is_compatible(&self, id: u16) -> bool {
        self.compatible.contains(&id)
    }

    /// Number of compatible subprotocols.
    #[inline]
    pub fn compatible_count(&self) -> usize {
        self.compatible.len()
    }
}

/// Negotiate subprotocol compatibility between local and remote manifests.
///
/// Pure function — no I/O. For each subprotocol present on both sides,
/// checks that each peer's version satisfies the other's minimum requirement.
pub fn negotiate(local: &SubprotocolManifest, remote: &SubprotocolManifest) -> NegotiatedSet {
    let mut compatible = HashSet::new();
    let mut incompatible = Vec::new();

    // Index remote entries by ID for O(1) lookup
    let remote_by_id: std::collections::HashMap<u16, &ManifestEntry> =
        remote.entries.iter().map(|e| (e.id, e)).collect();

    for local_entry in &local.entries {
        if let Some(remote_entry) = remote_by_id.get(&local_entry.id) {
            // Both sides have this subprotocol — check version compatibility
            if local_entry.version.satisfies(remote_entry.min_compatible)
                && remote_entry.version.satisfies(local_entry.min_compatible)
            {
                compatible.insert(local_entry.id);
            } else {
                incompatible.push((local_entry.id, local_entry.version, remote_entry.version));
            }
        }
        // If remote doesn't have it, skip — not an error, just not negotiated
    }

    NegotiatedSet {
        compatible,
        incompatible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u16, major: u8, minor: u8) -> ManifestEntry {
        ManifestEntry {
            id,
            version: SubprotocolVersion::new(major, minor),
            min_compatible: SubprotocolVersion::new(major, 0),
        }
    }

    fn entry_strict(id: u16, major: u8, minor: u8) -> ManifestEntry {
        ManifestEntry {
            id,
            version: SubprotocolVersion::new(major, minor),
            min_compatible: SubprotocolVersion::new(major, minor),
        }
    }

    #[test]
    fn test_negotiate_compatible() {
        let local = SubprotocolManifest {
            entries: vec![entry(0x0400, 1, 1), entry(0x0500, 1, 0)],
        };
        let remote = SubprotocolManifest {
            entries: vec![entry(0x0400, 1, 0), entry(0x0500, 1, 2)],
        };

        let result = negotiate(&local, &remote);
        assert!(result.is_compatible(0x0400));
        assert!(result.is_compatible(0x0500));
        assert!(result.incompatible.is_empty());
    }

    #[test]
    fn test_negotiate_incompatible() {
        let local = SubprotocolManifest {
            entries: vec![entry_strict(0x0400, 2, 0)],
        };
        let remote = SubprotocolManifest {
            entries: vec![entry_strict(0x0400, 1, 0)],
        };

        let result = negotiate(&local, &remote);
        assert!(!result.is_compatible(0x0400));
        assert_eq!(result.incompatible.len(), 1);
        assert_eq!(result.incompatible[0].0, 0x0400);
    }

    #[test]
    fn test_negotiate_disjoint() {
        let local = SubprotocolManifest {
            entries: vec![entry(0x0400, 1, 0)],
        };
        let remote = SubprotocolManifest {
            entries: vec![entry(0x0500, 1, 0)],
        };

        let result = negotiate(&local, &remote);
        assert!(result.compatible.is_empty());
        assert!(result.incompatible.is_empty()); // not incompatible, just absent
    }

    #[test]
    fn test_negotiate_empty() {
        let local = SubprotocolManifest { entries: vec![] };
        let remote = SubprotocolManifest {
            entries: vec![entry(0x0400, 1, 0)],
        };

        let result = negotiate(&local, &remote);
        assert!(result.compatible.is_empty());
    }

    #[test]
    fn test_manifest_roundtrip() {
        let manifest = SubprotocolManifest {
            entries: vec![
                entry(0x0400, 1, 1),
                entry(0x0500, 2, 3),
                entry(0x1000, 1, 0),
            ],
        };

        let bytes = manifest.to_bytes();
        let parsed = SubprotocolManifest::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.entries[0].id, 0x0400);
        assert_eq!(parsed.entries[0].version, SubprotocolVersion::new(1, 1));
        assert_eq!(parsed.entries[1].id, 0x0500);
        assert_eq!(parsed.entries[2].id, 0x1000);
    }

    #[test]
    fn test_manifest_from_bytes_too_short() {
        assert!(SubprotocolManifest::from_bytes(&[]).is_none());
        assert!(SubprotocolManifest::from_bytes(&[1]).is_none());

        // count=1 but no entry data
        assert!(SubprotocolManifest::from_bytes(&[1, 0]).is_none());
    }

    #[test]
    fn test_from_registry() {
        let reg = SubprotocolRegistry::with_defaults();
        let manifest = SubprotocolManifest::from_registry(&reg);
        assert!(manifest.entries.len() >= 4);
    }

    #[test]
    fn test_negotiate_partial_overlap() {
        let local = SubprotocolManifest {
            entries: vec![
                entry(0x0400, 1, 0),
                entry(0x0500, 1, 0),
                entry(0x1000, 1, 0),
            ],
        };
        let remote = SubprotocolManifest {
            entries: vec![entry(0x0400, 1, 0), entry(0x2000, 1, 0)],
        };

        let result = negotiate(&local, &remote);
        assert_eq!(result.compatible_count(), 1);
        assert!(result.is_compatible(0x0400));
        assert!(!result.is_compatible(0x0500)); // local only
        assert!(!result.is_compatible(0x1000)); // local only
        assert!(!result.is_compatible(0x2000)); // remote only
    }

    /// Regression: BUG_REPORT.md #10 — `from_bytes` previously
    /// accepted manifests with trailing garbage and with duplicate
    /// `id` entries. Both conditions enable downgrade attacks: a
    /// peer can advertise the same id twice with different
    /// versions and the last-write-wins behavior in `remote_by_id`
    /// silently picks whichever copy lands second.
    #[test]
    fn from_bytes_rejects_trailing_garbage_and_duplicate_ids() {
        // Build a valid 1-entry manifest, then append junk bytes.
        let manifest = SubprotocolManifest {
            entries: vec![entry(0x0400, 1, 0)],
        };
        let mut bytes = manifest.to_bytes().to_vec();
        // Sanity: the round-trip works as-is.
        assert!(SubprotocolManifest::from_bytes(&bytes).is_some());

        // Append a stray byte. Must reject.
        bytes.push(0xff);
        assert!(
            SubprotocolManifest::from_bytes(&bytes).is_none(),
            "trailing garbage must be rejected (#10)"
        );

        // Build a manifest with duplicate id 0x0400 — declare
        // count=2 then write the same id twice with different
        // versions. The historic bug let this through; the fix
        // rejects it.
        let mut buf = bytes::BytesMut::new();
        use bytes::BufMut;
        buf.put_u16_le(2);
        let dup1 = SubprotocolDescriptor {
            id: 0x0400,
            name: String::new(),
            version: SubprotocolVersion::new(1, 0),
            min_compatible: SubprotocolVersion::new(1, 0),
            handler_present: true,
        };
        let dup2 = SubprotocolDescriptor {
            id: 0x0400,
            name: String::new(),
            version: SubprotocolVersion::new(0, 1), // permissive version
            min_compatible: SubprotocolVersion::new(0, 1),
            handler_present: true,
        };
        write_manifest_entry(&dup1, &mut buf);
        write_manifest_entry(&dup2, &mut buf);
        assert!(
            SubprotocolManifest::from_bytes(&buf).is_none(),
            "duplicate id must be rejected — without this guard a peer \
             can advertise both `causal v1.0` and `causal v0.1` and \
             trigger a downgrade (#10)"
        );
    }

    // ========================================================================
    // from_registry must produce deterministic byte output
    // ========================================================================

    /// `from_registry().to_bytes()` is deterministic across
    /// invocations — entries are sorted by `id` rather than left in
    /// `DashMap` iteration order. Pre-fix, two calls on the same
    /// registry could produce different byte sequences, which
    /// silently breaks any signed-manifest or "same manifest? skip
    /// re-negotiation" optimisation that lives downstream of this
    /// API.
    #[test]
    fn from_registry_produces_deterministic_byte_output() {
        let reg = SubprotocolRegistry::with_defaults();
        // Take ten samples — DashMap iteration order across multiple
        // shards is reliably non-deterministic given a populated
        // registry, so ten samples either all match (proving the
        // sort-by-id makes the output stable) or quickly diverge
        // (proving the sort is missing).
        let baseline = SubprotocolManifest::from_registry(&reg).to_bytes();
        for i in 0..10 {
            let sample = SubprotocolManifest::from_registry(&reg).to_bytes();
            assert_eq!(
                sample, baseline,
                "from_registry().to_bytes() iteration {i} must match baseline — \
                 non-determinism here breaks any future signed-manifest scheme",
            );
        }
    }

    /// Entries inside a manifest from `from_registry` are sorted by
    /// `id` ascending. This is the structural invariant the byte-
    /// determinism test rests on.
    #[test]
    fn from_registry_returns_entries_sorted_by_id() {
        let reg = SubprotocolRegistry::with_defaults();
        let manifest = SubprotocolManifest::from_registry(&reg);
        let ids: Vec<u16> = manifest.entries.iter().map(|e| e.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "manifest entries must be sorted by id");
    }

    // ========================================================================
    // from_registry must NOT advertise forwarding-only descriptors
    // ========================================================================

    /// A forwarding-only descriptor (registered via
    /// `.forwarding_only()`) must not appear in the manifest. Pre-
    /// fix it was advertised, then `from_bytes` reconstructed it
    /// with `handler_present: true` (the wire format has no
    /// handler_present flag), and `negotiate()` marked it
    /// compatible — but the sender had no local handler so any
    /// dispatched RPC silently dropped. The parallel
    /// `capability_tags()` discovery path already filters by
    /// `handler_present`; this test pins that the manifest path
    /// agrees.
    #[test]
    fn from_registry_excludes_forwarding_only_descriptors() {
        let reg = SubprotocolRegistry::new();
        // Register a real handler at 0x0400.
        reg.register(SubprotocolDescriptor::new(
            0x0400,
            "real",
            SubprotocolVersion::new(1, 0),
        ));
        // Register a forwarding-only entry at 0x1000.
        reg.register(
            SubprotocolDescriptor::new(0x1000, "forward", SubprotocolVersion::new(1, 0))
                .forwarding_only(),
        );
        // Sanity: registry has both.
        assert_eq!(reg.list().len(), 2);

        let manifest = SubprotocolManifest::from_registry(&reg);
        let ids: std::collections::HashSet<u16> = manifest.entries.iter().map(|e| e.id).collect();
        assert!(ids.contains(&0x0400), "real handler must be advertised",);
        assert!(
            !ids.contains(&0x1000),
            "forwarding-only entry must NOT be advertised",
        );
    }

    /// `capability_tags()` and the manifest path agree on which
    /// subprotocols the local node actually handles. This is the
    /// invariant the dispatch layer relies on.
    #[test]
    fn from_registry_and_capability_tags_advertise_the_same_subprotocols() {
        let reg = SubprotocolRegistry::new();
        reg.register(SubprotocolDescriptor::new(
            0x0400,
            "a",
            SubprotocolVersion::new(1, 0),
        ));
        reg.register(SubprotocolDescriptor::new(
            0x0500,
            "b",
            SubprotocolVersion::new(1, 0),
        ));
        reg.register(
            SubprotocolDescriptor::new(0x1000, "c", SubprotocolVersion::new(1, 0))
                .forwarding_only(),
        );

        let manifest = SubprotocolManifest::from_registry(&reg);
        let manifest_ids: std::collections::HashSet<u16> =
            manifest.entries.iter().map(|e| e.id).collect();
        let tag_ids: std::collections::HashSet<u16> = reg
            .capability_tags()
            .iter()
            .filter_map(|t| {
                // capability_tag format: "subprotocol:0x0400"
                t.strip_prefix("subprotocol:0x")
                    .and_then(|hex| u16::from_str_radix(hex, 16).ok())
            })
            .collect();
        assert_eq!(
            manifest_ids, tag_ids,
            "manifest and capability_tags must advertise the same subprotocols",
        );
    }

    /// CR-31: tighten the cross-channel parity. The HashSet
    /// comparison above pins which subprotocols are advertised
    /// but NOT the order in which they're advertised. The
    /// determinism fix made `from_registry` deterministically sorted by id,
    /// but if `capability_tags()` consumed downstream as an
    /// ordered byte stream (e.g. fed into a hash for "same caps?
    /// skip re-announce" optimisation), divergence between the
    /// two channels' orderings would silently invalidate the
    /// dedup. We pin matching ascending-by-id order on BOTH so a
    /// future change touching either side surfaces here.
    #[test]
    fn from_registry_and_capability_tags_have_matching_ascending_id_order() {
        let reg = SubprotocolRegistry::new();
        // Register out of id order on purpose.
        reg.register(SubprotocolDescriptor::new(
            0x0500,
            "b",
            SubprotocolVersion::new(1, 0),
        ));
        reg.register(SubprotocolDescriptor::new(
            0x0400,
            "a",
            SubprotocolVersion::new(1, 0),
        ));
        reg.register(SubprotocolDescriptor::new(
            0x0700,
            "d",
            SubprotocolVersion::new(1, 0),
        ));
        reg.register(SubprotocolDescriptor::new(
            0x0600,
            "c",
            SubprotocolVersion::new(1, 0),
        ));

        let manifest = SubprotocolManifest::from_registry(&reg);
        let manifest_ids: Vec<u16> = manifest.entries.iter().map(|e| e.id).collect();

        let tag_ids: Vec<u16> = reg
            .capability_tags()
            .iter()
            .filter_map(|t| {
                t.strip_prefix("subprotocol:0x")
                    .and_then(|hex| u16::from_str_radix(hex, 16).ok())
            })
            .collect();

        // CR-31 + Cubic P2: pin BOTH channels emit in ascending
        // id order — DO NOT sort `tag_ids` before comparing.
        // The earlier shape sorted tag_ids, which only verified
        // they contained the same SET of ids (already pinned by
        // `from_registry_and_capability_tags_advertise_the_same_subprotocols`).
        // The whole point of CR-31 was to catch order-divergence,
        // so the comparison must be against the sorted manifest
        // verbatim with NO sort on the tag_ids side.
        let mut expected_sorted = manifest_ids.clone();
        expected_sorted.sort();

        assert_eq!(
            manifest_ids, expected_sorted,
            "from_registry must emit entries in ascending id order"
        );
        assert_eq!(
            tag_ids, expected_sorted,
            "Cubic P2: capability_tags must emit ids in ascending order to \
             match from_registry. Pre-fix this test sorted tag_ids before \
             comparing, which silently let an unordered capability_tags() \
             pass. Once both channels' bytes are consumed by a downstream \
             digest optimisation (CR-31), order divergence \
             becomes a silent dedup-bypass.\nGot:      {:?}\nExpected: {:?}",
            tag_ids, expected_sorted
        );
    }
}
