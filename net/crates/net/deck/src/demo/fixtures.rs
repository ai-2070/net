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
use net_sdk::subnets::SubnetId;

/// Pinned anchor subnet for the demo — the "local" node lives in
/// `us-east.alpha-prod`. Two-level hierarchy: region (1=us-east)
/// → fleet (2=alpha-prod).
fn local_subnet() -> SubnetId {
    SubnetId::new(&[1, 2])
}

/// Fixture rollup for the SUBNETS panel. Returns `(local,
/// rollups)` so the render fn can swap both at once.
pub fn subnets() -> (Option<SubnetId>, Vec<SubnetRollup>) {
    let local = local_subnet();
    let rollups = vec![
        SubnetRollup {
            subnet: SubnetId::new(&[1]),
            members: vec![0x1001, 0x1002, 0x1003, 0x1004, 0x1005, 0x1006],
            is_local: false,
        },
        SubnetRollup {
            subnet: local,
            members: vec![0x1101, 0x1102, 0x1103, 0x1104, 0x1105, 0x1106, 0x1107, 0x1108],
            is_local: true,
        },
        SubnetRollup {
            subnet: SubnetId::new(&[1, 3]),
            members: vec![0x1301, 0x1302, 0x1303, 0x1304],
            is_local: false,
        },
        SubnetRollup {
            subnet: SubnetId::new(&[2]),
            members: vec![0x2001, 0x2002, 0x2003],
            is_local: false,
        },
        SubnetRollup {
            subnet: SubnetId::new(&[2, 1]),
            members: vec![0x2101, 0x2102, 0x2103, 0x2104, 0x2105, 0x2106],
            is_local: false,
        },
        SubnetRollup {
            subnet: SubnetId::new(&[3, 1]),
            members: vec![0x3101, 0x3102, 0x3103, 0x3104, 0x3105],
            is_local: false,
        },
    ];
    (Some(local), rollups)
}

/// Fixture stats + export-table for the GATEWAYS panel.
pub fn gateways() -> (GatewayStats, Vec<(u16, Vec<SubnetId>)>) {
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
        // `inference.batch.completed` → siblings + cross-region
        // peers for demand-shaping rollups.
        (0x4a17, vec![SubnetId::new(&[2, 1]), SubnetId::new(&[3, 1])]),
        // `forge.rollout.broadcast` → just the sibling fleet
        // (us-east.beta-staging) for staged rollouts.
        (0x9b22, vec![SubnetId::new(&[1, 3])]),
        // `capability.gpu-h100` → us-west only (cross-region
        // capability sharing for spillover scheduling).
        (0xe041, vec![SubnetId::new(&[2, 1])]),
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
