//! The daemon's subnet policy, configured only through TOML
//! (`AGGREGATOR_MULTI_HOST_PLAN.md`, slice Q0).
//!
//! Every daemon here is booted by the library's `boot()` from a config
//! file, the path `main()` runs. Nothing installs a policy through a
//! test-only setup path. The witness the later Phase 2 slices depend on:
//! a peer attached directly to the daemon is classified into a subnet from
//! its signed tags, as recorded on the daemon's own mesh.

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::{MeshNode, MeshNodeConfig, SocketBufferConfig, SubnetId};
use net_aggregator_daemon::{boot, BootedDaemon, Cli, DaemonError, SubnetRuleError};
use tempfile::NamedTempFile;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];
const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";
const DEADLINE: Duration = Duration::from_secs(5);

const TWO_REGIONS: &str = r#"
    [[subnet_rule]]
    tag_prefix = "region:"
    level = 0
    values = { "eu" = 1, "us" = 2 }
"#;

fn leaf_config() -> MeshNodeConfig {
    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
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

/// Boot from `extra` appended to a minimal production config.
async fn try_boot(extra: &str) -> (Result<BootedDaemon, DaemonError>, NamedTempFile) {
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"
            {extra}
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
    (boot(cli).await, cfg_file)
}

async fn boot_started(extra: &str) -> (BootedDaemon, NamedTempFile) {
    let (booted, file) = try_boot(extra).await;
    let booted = booted.expect("daemon boot");
    booted.mesh.start();
    (booted, file)
}

/// A leaf attached directly to the daemon (a single-hop routed
/// handshake, as the CLI attaches) that announces `tags`.
async fn announcing_leaf(hub: &BootedDaemon, tags: &[&str]) -> Arc<MeshNode> {
    let leaf = Arc::new(
        MeshNode::new(EntityKeypair::generate(), leaf_config())
            .await
            .expect("leaf MeshNode::new"),
    );
    leaf.start();
    leaf.connect_via(hub.bound_addr, &hub.public_key, hub.mesh.node_id())
        .await
        .expect("leaf attaches");
    let caps = tags
        .iter()
        .fold(CapabilitySet::new(), |caps, t| caps.add_tag(*t));
    leaf.announce_capabilities(caps).await.expect("announce");
    leaf
}

async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn toml_rules_classify_direct_peers_on_the_daemon_mesh() {
    let (hub, _cfg) = boot_started(TWO_REGIONS).await;
    assert!(hub.subnet_policy_fingerprint.is_some());

    let eu = announcing_leaf(&hub, &["region:eu"]).await;
    let us = announcing_leaf(&hub, &["region:us"]).await;
    let other = announcing_leaf(&hub, &["region:mars"]).await;
    let (eu_id, us_id, other_id) = (eu.node_id(), us.node_id(), other.node_id());

    assert!(
        wait_until(|| hub.mesh.peer_subnet(eu_id) == Some(SubnetId::new(&[1]))).await,
        "the eu peer classifies as 1, got {:?}",
        hub.mesh.peer_subnet(eu_id)
    );
    assert!(
        wait_until(|| hub.mesh.peer_subnet(us_id) == Some(SubnetId::new(&[2]))).await,
        "the us peer classifies as 2, got {:?}",
        hub.mesh.peer_subnet(us_id)
    );
    // A tag no rule maps is classified, as GLOBAL, not left unknown.
    assert!(
        wait_until(|| hub.mesh.peer_subnet(other_id) == Some(SubnetId::GLOBAL)).await,
        "an unmatched peer classifies as GLOBAL, got {:?}",
        hub.mesh.peer_subnet(other_id)
    );
}

/// Without `[[subnet_rule]]` the daemon boots exactly as before: no
/// policy, so it records no peer subnets. This is the gap Q0 closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_rules_no_policy_is_installed_and_no_subnet_is_recorded() {
    let (hub, _cfg) = boot_started("").await;
    assert!(hub.subnet_policy_fingerprint.is_none());

    let leaf = announcing_leaf(&hub, &["region:eu"]).await;
    let leaf_id = leaf.node_id();
    let filter = CapabilityFilter::new().require_tag("region:eu");
    assert!(
        wait_until(|| hub.mesh.find_nodes_by_filter(&filter).contains(&leaf_id)).await,
        "the daemon has processed the leaf's announcement"
    );
    assert_eq!(hub.mesh.peer_subnet(leaf_id), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invalid_rule_refuses_boot_with_its_index() {
    let (res, _cfg) = try_boot(
        r#"
        [[subnet_rule]]
        tag_prefix = "region:"
        level = 0
        values = { "eu" = 1 }

        [[subnet_rule]]
        tag_prefix = "rack:"
        level = 4
        values = { "a" = 1 }
        "#,
    )
    .await;
    match res {
        Err(DaemonError::SubnetRule(SubnetRuleError::Invalid { index: 1, .. })) => {}
        Err(other) => panic!("expected subnet_rule[1] refused, got {other}"),
        Ok(_) => panic!("expected subnet_rule[1] refused, but the daemon booted"),
    }
}

/// The fingerprint a booted daemon reports follows the declaration-order
/// rule: value-map order is ignored, rule order is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn booted_fingerprints_ignore_value_order_but_not_rule_order() {
    let rule_a_ab = r#"
        [[subnet_rule]]
        tag_prefix = "region:"
        level = 0
        values = { "eu" = 1, "us" = 2 }
    "#;
    let rule_a_ba = r#"
        [[subnet_rule]]
        tag_prefix = "region:"
        level = 0
        values = { "us" = 2, "eu" = 1 }
    "#;
    let rule_b = r#"
        [[subnet_rule]]
        tag_prefix = "zone:"
        level = 0
        values = { "north" = 9 }
    "#;
    let fp = |extra: String| async move {
        let (booted, _cfg) = try_boot(&extra).await;
        booted
            .expect("boot")
            .subnet_policy_fingerprint
            .expect("rules configured")
    };
    let ab = fp(rule_a_ab.to_string()).await;
    let ba = fp(rule_a_ba.to_string()).await;
    assert_eq!(ab, ba, "value-map order must not matter");

    let forward = fp(format!("{rule_a_ab}{rule_b}")).await;
    let reversed = fp(format!("{rule_b}{rule_a_ab}")).await;
    assert_ne!(forward, reversed, "rule order must matter");
}
