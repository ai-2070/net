//! End-to-end smoke tests for the Phase 6c capability-aggregation
//! surface — exercises `Fold::aggregate` + `Fold::capacity_ranking`
//! through the public `MeshNode` surface (announce path → fold
//! population → query) rather than the in-process unit-test path
//! the framework tests use.
//!
//! Each binding (sdk-ts, sdk-py, sdk-go) ships a parallel smoke
//! suite that exercises the same shape through its FFI; together
//! they pin the wire-shape contract across the matrix
//! Rust core ↔ JSON encoder ↔ FFI plumbing ↔ Rust core.
//!
//! Run: `cargo test --features net --test capability_aggregation_e2e`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use net::adapter::net::behavior::fold::{Aggregation, CapacityQuery, GroupBy, TagMatcher};
use net::adapter::net::behavior::tag::Tag;
use net::adapter::net::identity::EntityId;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

/// Build a `scope:region:<name>` reserved tag — the fold's
/// `translate_announcement` parses this prefix to populate
/// `CapabilityMembership::region` (which `GroupBy::Region` reads).
/// Reserved prefixes can't go through `CapabilitySet::add_tag`
/// (it routes through `Tag::parse_user`, which rejects reserved
/// prefixes by design), so we insert the typed `Tag::Reserved`
/// directly.
fn region_tag(name: &str) -> Tag {
    Tag::Reserved {
        prefix: "scope:".to_string(),
        body: format!("region:{name}"),
    }
}

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Inject three synthetic publishers with realistic GPU cap sets so
/// the aggregation queries have multiple buckets to bucket into.
/// Uses the test-inject helper — same path the binding-side
/// `_testInjectSyntheticPeer` / `_test_inject_synthetic_peer`
/// helpers go through, just constructing richer cap announcements.
fn prime_fixture(node: &MeshNode) {
    // Publisher 0xA: h100 / us-east / 8 GPUs.
    let mut caps_a = CapabilitySet::new()
        .add_tag("hardware.gpu")
        .add_tag("hardware.gpu.h100")
        .add_tag("hardware.gpu.count=8")
        .add_tag("software.python=3.11");
    caps_a.tags.insert(region_tag("us-east"));
    node.test_inject_capability_announcement(CapabilityAnnouncement::new(
        0xA,
        EntityId::from_bytes([0xAA; 32]),
        1,
        caps_a,
    ));

    // Publisher 0xB: h100 / us-east / 4 GPUs.
    let mut caps_b = CapabilitySet::new()
        .add_tag("hardware.gpu")
        .add_tag("hardware.gpu.h100")
        .add_tag("hardware.gpu.count=4")
        .add_tag("software.python=3.12");
    caps_b.tags.insert(region_tag("us-east"));
    node.test_inject_capability_announcement(CapabilityAnnouncement::new(
        0xB,
        EntityId::from_bytes([0xBB; 32]),
        1,
        caps_b,
    ));

    // Publisher 0xC: a100 / us-west / 2 GPUs.
    let mut caps_c = CapabilitySet::new()
        .add_tag("hardware.gpu")
        .add_tag("hardware.gpu.a100")
        .add_tag("hardware.gpu.count=2")
        .add_tag("software.python=3.11");
    caps_c.tags.insert(region_tag("us-west"));
    node.test_inject_capability_announcement(CapabilityAnnouncement::new(
        0xC,
        EntityId::from_bytes([0xCC; 32]),
        1,
        caps_c,
    ));
}

#[tokio::test]
async fn aggregate_by_region_counts_publishers() {
    let node = build_node().await;
    prime_fixture(&node);
    let rows = node
        .capability_fold()
        .aggregate(None, GroupBy::Region, Aggregation::Count);
    assert_eq!(
        rows,
        vec![("us-east".to_string(), 2), ("us-west".to_string(), 1)],
    );
}

#[tokio::test]
async fn aggregate_by_tag_stem_buckets_per_gpu_type() {
    let node = build_node().await;
    prime_fixture(&node);
    let rows = node.capability_fold().aggregate(
        Some(TagMatcher::Prefix {
            value: "hardware.gpu".into(),
        }),
        GroupBy::TagStem {
            prefix: "hardware.gpu".into(),
        },
        Aggregation::Count,
    );
    let map: std::collections::HashMap<String, u64> = rows.into_iter().collect();
    assert_eq!(map.get("h100").copied(), Some(2));
    assert_eq!(map.get("a100").copied(), Some(1));
    assert_eq!(
        map.get("count").copied(),
        Some(3),
        "all three publishers carry a hardware.gpu.count=N tag"
    );
}

#[tokio::test]
async fn aggregate_sum_numeric_tag_sums_per_bucket() {
    let node = build_node().await;
    prime_fixture(&node);
    let rows = node.capability_fold().aggregate(
        None,
        GroupBy::Region,
        Aggregation::SumNumericTag {
            axis_key: "hardware.gpu.count".into(),
        },
    );
    assert_eq!(
        rows,
        vec![("us-east".to_string(), 12), ("us-west".to_string(), 2)],
        "us-east = 8 (0xA) + 4 (0xB); us-west = 2 (0xC)",
    );
}

#[tokio::test]
async fn capacity_ranking_breaks_down_state_per_region() {
    let node = build_node().await;
    prime_fixture(&node);
    let rows = node.capability_fold().capacity_ranking(
        CapacityQuery {
            group_by: GroupBy::Region,
            sum_axis_key: Some("hardware.gpu.count".into()),
            ..CapacityQuery::default()
        },
        |_node_id| None,
    );
    // Sorted by `available` desc: us-east (2) before us-west (1).
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].bucket, "us-east");
    assert_eq!(rows[0].available, 2);
    assert_eq!(rows[0].summed_capacity, Some(12));
    assert_eq!(rows[1].bucket, "us-west");
    assert_eq!(rows[1].available, 1);
    assert_eq!(rows[1].summed_capacity, Some(2));
}

#[tokio::test]
async fn capacity_ranking_filters_by_rtt() {
    let node = build_node().await;
    prime_fixture(&node);
    // Only 0xA (us-east) has a known RTT under the threshold; 0xB
    // and 0xC are dropped. us-east bucket count drops to 1.
    let rows = node.capability_fold().capacity_ranking(
        CapacityQuery {
            group_by: GroupBy::Region,
            max_rtt_ms: Some(50),
            ..CapacityQuery::default()
        },
        |node_id| {
            if node_id == 0xA {
                Some(10)
            } else {
                None
            }
        },
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bucket, "us-east");
    assert_eq!(rows[0].available, 1);
}

#[tokio::test]
async fn aggregate_version_range_picks_python_3_11_only() {
    let node = build_node().await;
    prime_fixture(&node);
    // 0xA and 0xC carry software.python=3.11; 0xB carries 3.12.
    // Note: parse-as-semver requires `3.11.0` form. The publishers
    // emit `3.11` which doesn't parse — so the matcher returns 0
    // entries. This pin documents the limitation; operators using
    // VersionRange should publish canonical semver values.
    let rows = node.capability_fold().aggregate(
        Some(TagMatcher::VersionRange {
            axis_key: "software.python".into(),
            min: Some("3.11.0".into()),
            max: Some("3.11.9".into()),
        }),
        GroupBy::Publisher,
        Aggregation::Count,
    );
    assert!(
        rows.is_empty(),
        "VersionRange requires canonical semver (`3.11.0`) — `3.11` doesn't parse; \
         publishers using bare-major.minor values won't match. Pin this to \
         document the contract.",
    );
}
