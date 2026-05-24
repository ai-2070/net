//! Demo fixtures for the SUBNETS / GATEWAYS / AGGREGATORS
//! panels. The demo's `ClusterHarness` boots N nodes flat under
//! `SubnetId::GLOBAL` — there's no real hierarchical subnet,
//! gateway counter, or AggregatorDaemon to render. These
//! fixtures fabricate enough state to make each panel show
//! something interesting under `--features demo` so demo
//! viewers see the panel surfaces rather than three "no X
//! wired" empty states.
//!
//! Numbers are stable (no RNG) so successive demo runs show the
//! same picture. The chosen scale matches the demo's narrative:
//! a multi-region AI inference fleet, with the demo's anchor
//! node living in `us-east.alpha-prod` (1.2).
//!
//! Each fixture is consumed by the matching panel's render
//! function as a fallback when the real `DeckClient` accessor
//! returns `None` / empty.

use std::sync::Arc;
use std::time::Duration;

use net_sdk::deck::{AggregatorSnapshot, GatewayStats, SubnetRollup, SummaryAnnouncement};
use net_sdk::subnets::{SubnetId, Visibility};

/// One resolved gateway-export row for the demo. Wraps the
/// substrate's `(channel_hash, targets)` tuple with the channel
/// name + visibility + reach the table needs.
#[derive(Clone, Debug)]
pub struct GatewayExportRow {
    pub channel_hash: u16,
    pub channel_name: Option<String>,
    pub visibility: Option<Visibility>,
    pub targets: Vec<SubnetId>,
    pub reach: u64,
}

/// Pinned anchor subnet for the demo — the "local" node lives in
/// `us-east.alpha-prod`. Two-level hierarchy: region (1=us-east)
/// → fleet (2=alpha-prod).
fn local_subnet() -> SubnetId {
    SubnetId::new(&[1, 2])
}

/// Fixture rollup for the SUBNETS panel.
///
/// Distributes the live demo cluster's real `(local_id, peer_ids)`
/// across six synthetic subnets so the panel reads as a
/// realistic multi-region tree AND each row's members actually
/// resolve against `MeshOsSnapshot.peers` — drilling into a
/// subnet shows real nodes the operator can pivot to. Previous
/// fixture used hard-coded hex IDs (0x1101..) that had no
/// counterparts in the snapshot, leaving every drill in an
/// "tagged but absent from snapshot" empty state.
///
/// Distribution (anchor row is `local_subnet()`):
///   1     — first quarter of peers (parent of 1.2 + 1.3)
///   1.2   — local node + ~quarter of peers
///   1.3   — ~quarter of peers
///   2.1   — ~quarter of peers
///   3.1   — remainder
///
/// Caller passes peers in deterministic order (BTreeMap key
/// iteration); rotating an empty list returns six rollups with
/// `members: vec![]` each so the panel still renders something
/// visible when the cluster has no peers wired yet.
pub fn subnets(local_id: u64, peer_ids: &[u64]) -> (Option<SubnetId>, Vec<SubnetRollup>) {
    let local = local_subnet();
    let n = peer_ids.len();
    // Slice the peer list into four roughly-equal buckets.
    let q = n / 4;
    let b1: Vec<u64> = peer_ids.iter().copied().take(q).collect();
    let b2: Vec<u64> = peer_ids.iter().copied().skip(q).take(q).collect();
    let b3: Vec<u64> = peer_ids.iter().copied().skip(2 * q).take(q).collect();
    let b4: Vec<u64> = peer_ids.iter().copied().skip(3 * q).collect();
    // 1.2 anchors the local node + first bucket's peers.
    let mut local_members = vec![local_id];
    local_members.extend(b2.iter().copied());

    let rollups = vec![
        SubnetRollup {
            subnet: SubnetId::new(&[1]),
            members: b1.clone(),
            is_local: false,
        },
        SubnetRollup {
            subnet: local,
            members: local_members,
            is_local: true,
        },
        SubnetRollup {
            subnet: SubnetId::new(&[1, 3]),
            members: b3.clone(),
            is_local: false,
        },
        SubnetRollup {
            subnet: SubnetId::new(&[2]),
            // Parent row with two children below — keep small
            // membership so the tree reads as "region with a
            // couple of fleets" rather than a flat dump.
            members: b1.iter().copied().take(b1.len() / 2).collect(),
            is_local: false,
        },
        SubnetRollup {
            subnet: SubnetId::new(&[2, 1]),
            members: b4.clone(),
            is_local: false,
        },
        SubnetRollup {
            subnet: SubnetId::new(&[3, 1]),
            // Cross-region fleet — reuse `b3` so the SUBNETS
            // table's HEALTH column shows realistic counts in
            // every row.
            members: b3,
            is_local: false,
        },
    ];
    (Some(local), rollups)
}

/// Fixture stats + resolved export rows for the GATEWAYS
/// panel. REACH values are pre-computed against the matching
/// subnets fixture (2.1 = 6, 3.1 = 5, 1.3 = 4 members) so the
/// numbers in this table line up with what SUBNETS shows.
pub fn gateways() -> (GatewayStats, Vec<GatewayExportRow>) {
    let stats = GatewayStats {
        local_subnet: local_subnet(),
        forwarded: 124_587,
        dropped: 392,
        peer_subnets: vec![
            SubnetId::new(&[1, 3]),
            SubnetId::new(&[2, 1]),
            SubnetId::new(&[3, 1]),
        ],
        export_rules: 3,
    };
    let exports = vec![
        GatewayExportRow {
            channel_hash: 0x4a17,
            channel_name: Some("swarm.telemetry.pose".into()),
            visibility: Some(Visibility::Exported),
            targets: vec![SubnetId::new(&[2, 1]), SubnetId::new(&[3, 1])],
            // 2.1 (6 nodes) + 3.1 (5 nodes) = 11
            reach: 11,
        },
        GatewayExportRow {
            channel_hash: 0x9b22,
            channel_name: Some("swarm.mission.broadcast".into()),
            visibility: Some(Visibility::Exported),
            targets: vec![SubnetId::new(&[1, 3])],
            // 1.3 (4 nodes)
            reach: 4,
        },
        GatewayExportRow {
            channel_hash: 0xe041,
            channel_name: Some("capability.tether.relay".into()),
            visibility: Some(Visibility::Exported),
            targets: vec![SubnetId::new(&[2, 1])],
            // 2.1 (6 nodes)
            reach: 6,
        },
    ];
    (stats, exports)
}

/// Fixture aggregator snapshot — buckets a drone-swarm fleet
/// by platform capability + current mission state. Picked
/// because it reads as "obvious distributed coordination"
/// without being on-the-nose AI-inference.
pub fn aggregator() -> AggregatorSnapshot {
    let source = local_subnet();
    // Capability fold — bucket by airframe class + payload.
    let capability = SummaryAnnouncement {
        source_subnet: source,
        fold_kind: 0x0001,
        generation: 142,
        buckets: vec![
            ("class:quadcopter.payload:optical".into(), 28),
            ("class:quadcopter.payload:lidar".into(), 14),
            ("class:fixed-wing.payload:thermal".into(), 9),
            ("class:vtol.payload:multispectral".into(), 6),
            ("class:tether.payload:relay".into(), 3),
        ],
    };
    // Reservation fold — bucket by current mission state.
    let reservation = SummaryAnnouncement {
        source_subnet: source,
        fold_kind: 0x0002,
        generation: 142,
        buckets: vec![
            ("loiter".into(), 22),
            ("transit".into(), 14),
            ("survey".into(), 11),
            ("recharge".into(), 8),
            ("return-to-base".into(), 3),
            ("lost-link".into(), 2),
        ],
    };
    AggregatorSnapshot {
        source_subnet: source,
        fold_kinds: vec![0x0001, 0x0002],
        generation: 142,
        summary_interval: Duration::from_secs(30),
        summaries: Arc::new(vec![capability, reservation]),
    }
}
