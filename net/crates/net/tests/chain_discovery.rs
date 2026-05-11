//! Integration tests for the `Mesh::announce_chain` /
//! `announce_chain_range` / `withdraw_chain` / `find_chain_holders`
//! helpers.
//!
//! These are the Capability Phase B primitives from
//! `docs/plans/CAPABILITY_SYSTEM_PLAN.md` §B, the hard prerequisite
//! that `docs/plans/REDEX_DISTRIBUTED_PLAN.md` Phase C/D/E depend
//! on. Tests pin:
//!
//! - Idempotent advertise: announce_chain twice with different tips
//!   replaces the prior tag (no duplicate causal: tags accrue).
//! - withdraw_chain strips every variant for an origin_hash, leaving
//!   other chains' tags intact.
//! - find_chain_holders returns self after a local announce + every
//!   peer the capability index has indexed for that chain.
//! - find_chain_holders proximity-sorts the result (self first;
//!   unmeasured peers at the back).
//! - announce_chain_range emits the range form; the matcher still
//!   recognizes the node as a holder.
//!
//! Run: `cargo test --features net --test chain_discovery`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250));
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

#[tokio::test]
async fn announce_chain_self_indexes_as_holder() {
    let n = build_node().await;
    n.start();

    let origin_hash = 0xCAFE_BABE_DEAD_BEEFu64;
    n.announce_chain(origin_hash, 42)
        .await
        .expect("announce_chain");

    let holders = n.find_chain_holders(origin_hash);
    assert_eq!(
        holders,
        vec![n.node_id()],
        "self must be the sole holder after a local announce; got {holders:?}",
    );
}

#[tokio::test]
async fn announce_chain_replaces_prior_tip() {
    let n = build_node().await;
    n.start();

    let origin_hash = 0x1234_5678_9ABC_DEF0u64;
    // First announce — tip 100.
    n.announce_chain(origin_hash, 100).await.unwrap();
    // Re-announce with a different tip — must replace, not
    // accumulate. The replace-semantics are pinned at the pure-
    // function level by `Mesh::is_causal_for` / `replace_causal_tags`
    // unit tests in mesh.rs; here we verify the public-API
    // outcome: the node remains a holder after re-announce.
    n.announce_chain(origin_hash, 200).await.unwrap();
    let holders = n.find_chain_holders(origin_hash);
    assert_eq!(
        holders,
        vec![n.node_id()],
        "self still a holder after re-announce with different tip",
    );
}

#[tokio::test]
async fn withdraw_chain_strips_every_variant() {
    let n = build_node().await;
    n.start();

    let origin_hash = 0xAAAA_BBBB_CCCC_DDDDu64;
    n.announce_chain(origin_hash, 100).await.unwrap();
    n.announce_chain_range(origin_hash, 50, 100).await.unwrap();
    // After announce_chain_range, only the range form remains
    // (replace semantics).
    n.withdraw_chain(origin_hash).await.unwrap();

    let holders = n.find_chain_holders(origin_hash);
    assert!(
        holders.is_empty(),
        "withdraw must remove self from holders; got {holders:?}",
    );
}

#[tokio::test]
async fn withdraw_chain_preserves_other_chains() {
    let n = build_node().await;
    n.start();

    let chain_a = 0x1u64;
    let chain_b = 0x2u64;
    n.announce_chain(chain_a, 100).await.unwrap();
    n.announce_chain(chain_b, 200).await.unwrap();

    n.withdraw_chain(chain_a).await.unwrap();

    assert!(
        n.find_chain_holders(chain_a).is_empty(),
        "chain A must be withdrawn",
    );
    assert_eq!(
        n.find_chain_holders(chain_b),
        vec![n.node_id()],
        "chain B must survive the withdraw of A",
    );
}

#[tokio::test]
async fn announce_chain_range_indexes_as_holder() {
    let n = build_node().await;
    n.start();

    let origin_hash = 0xFEED_FACEu64;
    n.announce_chain_range(origin_hash, 10, 1000).await.unwrap();

    let holders = n.find_chain_holders(origin_hash);
    assert_eq!(holders, vec![n.node_id()]);
}

#[tokio::test]
async fn announce_chain_range_rejects_degenerate_range() {
    let n = build_node().await;
    n.start();

    let origin_hash = 0xDEFAu64;
    // start_seq >= end_seq is a no-op; no tag emitted.
    n.announce_chain_range(origin_hash, 100, 100).await.unwrap();
    n.announce_chain_range(origin_hash, 100, 50).await.unwrap();

    let holders = n.find_chain_holders(origin_hash);
    assert!(
        holders.is_empty(),
        "degenerate range must not emit a tag; got {holders:?}",
    );
}

#[tokio::test]
async fn announce_chain_layers_on_announce_capabilities_baseline() {
    use net::adapter::net::behavior::capability::CapabilityFilter;

    let n = build_node().await;
    n.start();

    // Initial announce_capabilities establishes a baseline with a
    // single tag.
    let baseline = CapabilitySet::default().add_tag("hardware.gpu");
    n.announce_capabilities(baseline).await.unwrap();

    // Layer a chain tag on top.
    let origin_hash = 0x42u64;
    n.announce_chain(origin_hash, 7).await.unwrap();

    // The baseline `hardware.gpu` tag must survive the
    // announce_chain. Verify via the public find_nodes_by_filter
    // path — self matches a filter that requires the baseline tag.
    let filter = CapabilityFilter::new().require_tag("hardware.gpu");
    let baseline_match = n.find_nodes_by_filter(&filter);
    assert!(
        baseline_match.contains(&n.node_id()),
        "baseline hardware.gpu tag was lost during announce_chain",
    );
    // And the chain tag must be discoverable.
    assert_eq!(
        n.find_chain_holders(origin_hash),
        vec![n.node_id()],
        "chain tag never landed",
    );
}

#[tokio::test]
async fn find_chain_holders_returns_self_first() {
    let n = build_node().await;
    n.start();

    // Self announces the chain. The proximity-sort puts self at
    // the front of the holders list because RTT(self) = 0.
    let origin_hash = 0x1u64;
    n.announce_chain(origin_hash, 1).await.unwrap();

    let holders = n.find_chain_holders(origin_hash);
    assert!(!holders.is_empty());
    assert_eq!(
        holders[0],
        n.node_id(),
        "self must be first in the holders list"
    );
}

#[tokio::test]
async fn find_chain_holders_empty_for_unknown_chain() {
    let n = build_node().await;
    n.start();

    let holders = n.find_chain_holders(0xFFFF_FFFF_FFFF_FFFFu64);
    assert!(
        holders.is_empty(),
        "no node should be a holder for an unannounced chain",
    );
}

#[tokio::test]
async fn withdraw_chain_is_idempotent() {
    let n = build_node().await;
    n.start();

    let origin_hash = 0x77u64;
    // Withdraw without an announce: no-op, doesn't error.
    n.withdraw_chain(origin_hash).await.unwrap();
    // Repeat after a real announce + withdraw.
    n.announce_chain(origin_hash, 5).await.unwrap();
    n.withdraw_chain(origin_hash).await.unwrap();
    n.withdraw_chain(origin_hash).await.unwrap();

    let holders = n.find_chain_holders(origin_hash);
    assert!(holders.is_empty());
}

#[tokio::test]
async fn similar_hex_prefix_does_not_false_match() {
    // Chain `0x100` renders as `0000000000000100`; chain `0x1000`
    // renders as `0000000000001000`. The shorter doesn't share a
    // prefix with the longer at the byte the matcher checks (the
    // delimiter follows the full 16-hex). Pin that withdraw_chain
    // on one doesn't strip the other.
    let n = build_node().await;
    n.start();

    let short = 0x100u64;
    let long = 0x1000u64;
    n.announce_chain(short, 1).await.unwrap();
    n.announce_chain(long, 2).await.unwrap();

    // Withdraw `short` — `long` must survive.
    n.withdraw_chain(short).await.unwrap();
    assert!(n.find_chain_holders(short).is_empty());
    assert_eq!(n.find_chain_holders(long), vec![n.node_id()]);
}
