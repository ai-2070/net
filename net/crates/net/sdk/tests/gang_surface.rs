//! Rust SDK smoke test for the gang-claim scheduler surface.
//!
//! Exercises `sdk/src/gang.rs` value types + the `Mesh` gang methods:
//! reserve / release (including the unheld→`Lost` contract), and a
//! single-node publish → match → claim round-trip over self-announced
//! capability + island folds. If a public type or method disappears,
//! this test stops compiling.

#![cfg(feature = "net")]

use net_sdk::capabilities::CapabilitySet;
use net_sdk::gang::{
    CapabilityFilter, CapabilityQuery, ClaimOutcome, IslandRecord, MatchCriteria, NumericFilter,
    SelectionPolicy, UnitSet,
};
use net_sdk::mesh::MeshBuilder;

const PSK: [u8; 32] = [0x5b; 32];

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

async fn solo_mesh() -> net_sdk::mesh::Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn reserve_release_and_unheld_release_is_lost() {
    let mesh = solo_mesh().await;
    let until = now_us() + 60_000_000;

    // Reserve a fresh island → Won; release it → Won.
    assert_eq!(
        mesh.reserve_island(0xA0, until).await.unwrap(),
        ClaimOutcome::Won,
    );
    assert_eq!(mesh.release_island(0xA0).await.unwrap(), ClaimOutcome::Won);

    // Releasing an island this node never held → Lost (not a false Won).
    assert_eq!(
        mesh.release_island(0xBEEF).await.unwrap(),
        ClaimOutcome::Lost,
    );
}

#[tokio::test]
async fn single_node_publishes_matches_and_claims_its_own_island() {
    let mesh = solo_mesh().await;

    // Self-announce a GPU capability + an island this node hosts. Both
    // self-index locally, so the node's own scheduler can see them
    // without any peer convergence.
    mesh.announce_capabilities(CapabilitySet::new().add_tag("gpu:h100"))
        .await
        .unwrap();
    mesh.publish_island_topology(IslandRecord {
        id: 0xD0,
        units: UnitSet::new(vec![0, 1, 2, 3, 4, 5, 6, 7]),
        host: 0, // overwritten with this node's id by publish
        capabilities: vec!["model:a1".into()],
        load: 0.1,
        p50_latency_us: 800,
    })
    .await
    .unwrap();

    let criteria = MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec!["gpu:h100".into()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    };

    // The node sees its own island...
    assert_eq!(mesh.match_islands(&criteria), vec![0xD0]);
    // ...and claims (reserves) it.
    let claimed = mesh
        .claim_island(&criteria, now_us() + 60_000_000)
        .await
        .unwrap();
    assert_eq!(claimed, Some(0xD0));
}
