//! Capability announcements relayed through a real `net-aggregator-daemon`
//! acting as the hub (`AGGREGATOR_MULTI_HOST_PLAN.md`, slice R1).
//!
//! `tests/capability_multihop.rs` in the core crate covers hub relay with
//! bare `MeshNode` hubs. These tests run the same shapes through the daemon
//! itself: booted from TOML by the library's `boot()` (the path `main()` runs),
//! with both leaves attached by routed handshake (`connect_via`), the way the
//! CLI attaches. Neither leaf has a direct session with the other.
//!
//! Three cases:
//! 1. **Replay** — the provider announces before the consumer attaches; the
//!    hub replays the held announcement on attach.
//! 2. **Flood** — both are attached first, then the provider announces; the
//!    hub forwards it live. No routed-hub test covered this before.
//! 3. **Supersede** — the provider re-announces with a different tag; the
//!    consumer sees the new set and loses the old tag.

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::{MeshNode, MeshNodeConfig, SocketBufferConfig};
use net_aggregator_daemon::{boot, BootedDaemon, Cli};
use tempfile::NamedTempFile;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];
const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

/// How long a leaf waits to see a relayed announcement. Generous: the
/// relay is a single forward on loopback, so a pass is normally a few
/// tens of milliseconds and only a broken path reaches the deadline.
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(5);

/// Past the hub's last replay pass for a new session (2 s), with margin.
const REPLAY_PASSES_DONE: Duration = Duration::from_millis(2_500);

fn leaf_config() -> MeshNodeConfig {
    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250))
        // The origin coalesces announcements inside this window and sends
        // the newest at the window's end. At the 10 s default, the
        // supersede case would be measuring the origin's rate limit, not
        // the hub's relay.
        .with_min_announce_interval(Duration::from_millis(100));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

/// Boot the daemon from a production-shaped TOML file and start its
/// receive loop. A routed handshake needs the responder's dispatch loop
/// running before msg1 arrives, and `boot()` leaves the mesh unstarted.
async fn boot_hub() -> (BootedDaemon, NamedTempFile) {
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"

            [[group]]
            name = "hub-summary"
            fold_kinds = [1]
            replica_count = 1
            summary_interval_ms = 100
        "#
    );
    let cfg_file = NamedTempFile::new().expect("tempfile");
    std::fs::write(cfg_file.path(), toml).expect("write config");
    let cli = Cli {
        config: cfg_file.path().to_path_buf(),
        listen: None,
        verbose: 0,
        print_bootstrap: false,
        insecure_permissions: false,
    };
    let booted = boot(cli).await.expect("daemon boot");
    booted.mesh.start();
    // Keep the config file alive for the daemon's lifetime.
    (booted, cfg_file)
}

/// A leaf node attached to the hub by routed handshake, as the CLI and
/// `join`ed devices attach.
async fn attach_leaf(hub: &BootedDaemon) -> Arc<MeshNode> {
    let leaf = Arc::new(
        MeshNode::new(EntityKeypair::generate(), leaf_config())
            .await
            .expect("leaf MeshNode::new"),
    );
    leaf.start();
    leaf.connect_via(hub.bound_addr, &hub.public_key, hub.mesh.node_id())
        .await
        .expect("leaf attaches to the daemon hub");
    leaf
}

async fn wait_until<F>(node: &Arc<MeshNode>, mut cond: F) -> bool
where
    F: FnMut(&MeshNode) -> bool,
{
    let deadline = tokio::time::Instant::now() + DISCOVERY_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        if cond(node) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond(node)
}

fn tag(name: &str) -> CapabilityFilter {
    CapabilityFilter::new().require_tag(name)
}

/// Case 1: the provider announces, then the consumer attaches. Discovery
/// comes from the hub's replay of the held, origin-signed announcement.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replay_reaches_a_consumer_that_attaches_after_the_announcement() {
    let (hub, _cfg) = boot_hub().await;
    let provider = attach_leaf(&hub).await;
    provider
        .announce_capabilities(CapabilitySet::new().add_tag("relay-replay-tool"))
        .await
        .expect("provider announce");
    let provider_id = provider.node_id();
    assert!(
        wait_until(&hub.mesh, |n| n
            .find_nodes_by_filter(&tag("relay-replay-tool"))
            .contains(&provider_id))
        .await,
        "the daemon itself learns the provider's tag"
    );

    let consumer = attach_leaf(&hub).await;
    assert!(
        wait_until(&consumer, |n| n
            .find_nodes_by_filter(&tag("relay-replay-tool"))
            .contains(&provider_id))
        .await,
        "the consumer discovers the provider from the daemon's replay"
    );
}

/// Case 2: both leaves are attached before the provider announces.
/// Discovery must come from the daemon's live forward, not from replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flood_reaches_a_consumer_attached_before_the_announcement() {
    let (hub, _cfg) = boot_hub().await;
    let provider = attach_leaf(&hub).await;
    let consumer = attach_leaf(&hub).await;
    let (provider_id, consumer_id) = (provider.node_id(), consumer.node_id());
    // Both sessions must be in the hub's peer table before the announce,
    // or this would silently become a replay test.
    let hub_mesh = hub.mesh.clone();
    assert!(
        wait_until(&hub_mesh, |n| n.peer_session_id(provider_id).is_some()
            && n.peer_session_id(consumer_id).is_some())
        .await,
        "the daemon holds sessions with both leaves"
    );
    // The hub replays held announcements to a newly attached peer twice,
    // at 250 ms and 2 s after the session opens (`REPLAY_SETTLE` in
    // `mesh.rs`). Wait out both passes so a pass below can only come from
    // the live forward, not from a late replay.
    tokio::time::sleep(REPLAY_PASSES_DONE).await;

    provider
        .announce_capabilities(CapabilitySet::new().add_tag("relay-flood-tool"))
        .await
        .expect("provider announce");
    assert!(
        wait_until(&consumer, |n| n
            .find_nodes_by_filter(&tag("relay-flood-tool"))
            .contains(&provider_id))
        .await,
        "the consumer discovers the provider from the daemon's live forward"
    );
}

/// Case 3: the provider re-announces a different capability set. The
/// consumer sees the new tag and stops matching the old one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reannouncement_supersedes_the_relayed_set() {
    let (hub, _cfg) = boot_hub().await;
    let provider = attach_leaf(&hub).await;
    let consumer = attach_leaf(&hub).await;
    let provider_id = provider.node_id();

    provider
        .announce_capabilities(CapabilitySet::new().add_tag("relay-old-tool"))
        .await
        .expect("first announce");
    assert!(
        wait_until(&consumer, |n| n
            .find_nodes_by_filter(&tag("relay-old-tool"))
            .contains(&provider_id))
        .await,
        "the consumer sees the first set"
    );

    provider
        .announce_capabilities(CapabilitySet::new().add_tag("relay-new-tool"))
        .await
        .expect("second announce");
    assert!(
        wait_until(&consumer, |n| n
            .find_nodes_by_filter(&tag("relay-new-tool"))
            .contains(&provider_id)
            && !n
                .find_nodes_by_filter(&tag("relay-old-tool"))
                .contains(&provider_id))
        .await,
        "the consumer sees the new set and no longer matches the old tag"
    );
}
