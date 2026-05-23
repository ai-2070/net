//! End-to-end integration test: boot the aggregator daemon via
//! the library's `boot` helper, connect a separate
//! `MeshNode`-backed `RegistryClient`, and verify the configured
//! groups round-trip across the wire.
//!
//! This is the closest thing to a "deployment test" the binary
//! has: it exercises the same code path `main()` runs, just
//! without the signal-wait loop, and asserts against the
//! booted state.

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::aggregator::RegistryClient;
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::{MeshNode, MeshNodeConfig, SocketBufferConfig};
use net_aggregator_daemon::{boot, drain_registry, Cli};
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

fn test_mesh_config(addr: std::net::SocketAddr) -> MeshNodeConfig {
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

async fn build_client_node() -> Arc<MeshNode> {
    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let kp = EntityKeypair::generate();
    Arc::new(
        MeshNode::new(kp, test_mesh_config(addr))
            .await
            .expect("client MeshNode::new"),
    )
}

async fn write_temp_config(toml: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("tempfile");
    let path = f.path().to_path_buf();
    {
        let mut handle = tokio::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .await
            .expect("open tempfile");
        handle
            .write_all(toml.as_bytes())
            .await
            .expect("write tempfile");
        handle.flush().await.expect("flush tempfile");
    }
    // Keep the NamedTempFile guard so the file isn't unlinked
    // until the test scope ends.
    let _ = &mut f;
    f
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_boots_two_groups_and_serves_registry_rpc() {
    // 1. Boot the daemon on an ephemeral port via the library
    //    helper.
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"

            [[group]]
            name = "alpha"
            source_subnet = "3.7"
            fold_kinds = [1]
            replica_count = 2
            summary_interval_ms = 50

            [[group]]
            name = "beta"
            source_subnet = "3.8"
            fold_kinds = [1]
            replica_count = 1
            summary_interval_ms = 50
        "#
    );
    let cfg_file = write_temp_config(&toml).await;
    let cli = Cli {
        config: cfg_file.path().to_path_buf(),
        listen: None,
        verbose: 0,
    };
    let booted = boot(cli).await.expect("daemon boot");
    assert_eq!(booted.registry.len(), 2);
    let names = booted.registry.names();
    assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);

    // 2. Build a separate MeshNode to act as the querier.
    let client_node = build_client_node().await;
    // Handshake against the daemon's MeshNode. `boot()`
    // deliberately leaves the daemon's mesh unstarted so the
    // handshake can drive its own socket exchange.
    let daemon_node_id = booted.mesh.node_id();
    let client_node_id = client_node.node_id();
    let daemon_clone = booted.mesh.clone();
    let accept = tokio::spawn(async move { daemon_clone.accept(client_node_id).await });
    client_node
        .connect(booted.bound_addr, &booted.public_key, daemon_node_id)
        .await
        .expect("connect to daemon");
    accept
        .await
        .expect("accept join")
        .expect("daemon accept");
    // Start both receive loops now that the handshake landed.
    booted.mesh.start();
    client_node.start();

    // 3. RegistryClient::list against the daemon.
    let client = RegistryClient::new(client_node).with_deadline(Duration::from_secs(2));
    let groups = client.list(daemon_node_id).await.expect("list");
    let resolved_names: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();
    assert_eq!(resolved_names, vec!["alpha".to_string(), "beta".to_string()]);
    let alpha = groups.iter().find(|g| g.name == "alpha").expect("alpha");
    assert_eq!(alpha.replicas.len(), 2);
    let beta = groups.iter().find(|g| g.name == "beta").expect("beta");
    assert_eq!(beta.replicas.len(), 1);
    for g in &groups {
        for r in &g.replicas {
            assert!(r.healthy, "expected healthy replicas at boot");
        }
    }

    // 4. Drain the registry to mirror the daemon's shutdown
    //    path. The test holds the only references; the booted
    //    drop chain releases the ServeHandle + Mesh.
    drain_registry(&booted.registry).await;
    assert!(booted.registry.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_rejects_unknown_fold_kind_at_boot() {
    // The daemon validates fold_kinds against the built-in
    // summarizers at startup (capability = 0x0001, reservation =
    // 0x0002). An operator typo like 0xDEAD must fail-fast so
    // the misconfig surfaces immediately.
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"

            [[group]]
            name = "bad"
            source_subnet = "3.7"
            fold_kinds = [0xDEAD]
            replica_count = 1
            summary_interval_ms = 50
        "#
    );
    let cfg_file = write_temp_config(&toml).await;
    let cli = Cli {
        config: cfg_file.path().to_path_buf(),
        listen: None,
        verbose: 0,
    };
    match boot(cli).await {
        Err(net_aggregator_daemon::DaemonError::AggregatorConfig { name, error }) => {
            assert_eq!(name, "bad");
            assert!(error.contains("unknown fold_kind"), "msg was: {error}");
        }
        Err(other) => panic!("expected AggregatorConfig, got {other:?}"),
        Ok(_) => panic!("expected AggregatorConfig, got Ok"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_and_unregister_rpc_round_trip_against_running_daemon() {
    // Pin the full operator loop: configure a template, boot
    // the daemon (no static groups), spawn a new group via the
    // RPC, list to confirm it's there, then unregister it.
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"

            [[template]]
            name = "primary"
            source_subnet = "3.7"
            fold_kinds = [1]
            summary_interval_ms = 50
        "#
    );
    let cfg_file = write_temp_config(&toml).await;
    let cli = Cli {
        config: cfg_file.path().to_path_buf(),
        listen: None,
        verbose: 0,
    };
    let booted = boot(cli).await.expect("daemon boot");
    assert_eq!(booted.registry.len(), 0);

    // Handshake.
    let client_node = build_client_node().await;
    let daemon_node_id = booted.mesh.node_id();
    let client_node_id = client_node.node_id();
    let daemon_clone = booted.mesh.clone();
    let accept = tokio::spawn(async move { daemon_clone.accept(client_node_id).await });
    client_node
        .connect(booted.bound_addr, &booted.public_key, daemon_node_id)
        .await
        .expect("connect");
    accept.await.expect("join").expect("accept");
    booted.mesh.start();
    client_node.start();

    let client = RegistryClient::new(client_node).with_deadline(Duration::from_secs(2));

    // Spawn against the configured template.
    let summary = client
        .spawn(daemon_node_id, "primary", "dynamic", 2)
        .await
        .expect("spawn");
    assert_eq!(summary.name, "dynamic");
    assert_eq!(summary.replicas.len(), 2);

    // List shows the new group.
    let groups = client.list(daemon_node_id).await.expect("list");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "dynamic");

    // Unregister returns existed=true.
    let existed = client
        .unregister(daemon_node_id, "dynamic")
        .await
        .expect("unregister");
    assert!(existed);
    // List is empty again.
    let groups = client.list(daemon_node_id).await.expect("list-after");
    assert!(groups.is_empty());
    // Second unregister returns existed=false.
    let existed = client
        .unregister(daemon_node_id, "dynamic")
        .await
        .expect("unregister-2");
    assert!(!existed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_unknown_template_returns_typed_server_error() {
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"
        "#
    );
    let cfg_file = write_temp_config(&toml).await;
    let cli = Cli {
        config: cfg_file.path().to_path_buf(),
        listen: None,
        verbose: 0,
    };
    let booted = boot(cli).await.expect("daemon boot");

    let client_node = build_client_node().await;
    let daemon_node_id = booted.mesh.node_id();
    let client_node_id = client_node.node_id();
    let daemon_clone = booted.mesh.clone();
    let accept = tokio::spawn(async move { daemon_clone.accept(client_node_id).await });
    client_node
        .connect(booted.bound_addr, &booted.public_key, daemon_node_id)
        .await
        .expect("connect");
    accept.await.expect("join").expect("accept");
    booted.mesh.start();
    client_node.start();

    let client = RegistryClient::new(client_node).with_deadline(Duration::from_secs(2));
    use net::adapter::net::behavior::aggregator::{RegistryClientError, RegistryRpcError};
    match client.spawn(daemon_node_id, "nope", "any", 1).await {
        Err(RegistryClientError::Server(RegistryRpcError::UnknownTemplate(t))) => {
            assert_eq!(t, "nope");
        }
        other => panic!("expected UnknownTemplate, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_rejects_duplicate_group_names_at_boot() {
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"

            [[group]]
            name = "dup"
            source_subnet = "3.7"
            fold_kinds = [1]
            replica_count = 1
            summary_interval_ms = 50

            [[group]]
            name = "dup"
            source_subnet = "3.8"
            fold_kinds = [1]
            replica_count = 1
            summary_interval_ms = 50
        "#
    );
    let cfg_file = write_temp_config(&toml).await;
    let cli = Cli {
        config: cfg_file.path().to_path_buf(),
        listen: None,
        verbose: 0,
    };
    match boot(cli).await {
        Err(net_aggregator_daemon::DaemonError::Registry(msg)) => {
            assert!(msg.contains("already registered"), "msg was: {msg}");
        }
        Err(other) => panic!("expected Registry, got {other:?}"),
        Ok(_) => panic!("expected Registry, got Ok"),
    }
}
