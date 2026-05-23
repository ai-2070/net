//! Rust SDK integration test for the `aggregator.registry` RPC
//! client surface (`RegistryClient` + `BoundRegistryClient`).
//!
//! Two `Mesh`es via the SDK's `MeshBuilder`: host installs an
//! `AggregatorRegistry` + a template-aware spawner; querier
//! drives list / spawn / unregister via the SDK wrappers.
//! This is the SDK-side smoke test — the substrate-level
//! round-trip is in `crates/net/tests/aggregator_registry_rpc.rs`.

#![cfg(feature = "net")]

use std::sync::Arc;
use std::time::Duration;

use net_sdk::aggregator::{
    snapshot_group, AggregatorConfig, AggregatorDaemon, AggregatorRegistry, BoundRegistryClient,
    LifecycleGroup, RegistryClient, RegistryClientError, RegistryRpcError, SpawnFn,
};
use net_sdk::mesh::{Mesh, MeshBuilder};

const PSK: [u8; 32] = [0x42u8; 32];

async fn two_meshes() -> (Mesh, Mesh, std::net::SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let addr_b = b.inner().local_addr();
    (a, b, addr_b)
}

async fn handshake(a: &Mesh, b: &Mesh, addr_b: std::net::SocketAddr) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

/// Build a `SpawnFn` that recognizes one template name
/// (`primary`) and spawns a 2-replica capability-fold
/// aggregator group from it.
fn primary_template_spawner(
    registry: Arc<AggregatorRegistry>,
    mesh: Arc<net::adapter::net::MeshNode>,
) -> SpawnFn {
    use net::adapter::net::behavior::fold::capability::CapabilityFold;
    use net::adapter::net::behavior::fold::FoldKind;
    use net::adapter::net::SubnetId;
    Box::new(move |req| {
        let registry = registry.clone();
        let mesh = mesh.clone();
        Box::pin(async move {
            if req.template_name != "primary" {
                return Err(RegistryRpcError::UnknownTemplate(req.template_name));
            }
            let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
                .with_fold_kind(CapabilityFold::KIND_ID)
                .with_interval(Duration::from_millis(80));
            let group =
                LifecycleGroup::<AggregatorDaemon>::spawn(req.replica_count, [0xCDu8; 32], {
                    let cfg = cfg.clone();
                    let mesh = mesh.clone();
                    move |_idx| {
                        Arc::new(
                            AggregatorDaemon::new(cfg.clone(), mesh.clone())
                                .expect("aggregator config validated"),
                        )
                    }
                })
                .await
                .map_err(|e| RegistryRpcError::SpawnRejected(format!("{e}")))?;
            let entry = registry
                .register(req.group_name.clone(), group)
                .map_err(|e| RegistryRpcError::SpawnRejected(format!("{e}")))?;
            Ok(snapshot_group(&entry).await)
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bound_registry_client_drives_list_spawn_unregister_against_remote_host() {
    // Build two meshes; B is the daemon-host, A is the
    // operator's querier.
    let (a, b, addr_b) = two_meshes().await;

    // Install the aggregator registry + the registry service
    // on B BEFORE the handshake so the receive loop sees them
    // when it starts.
    let registry: Arc<AggregatorRegistry> = Arc::new(AggregatorRegistry::new());
    let spawner = primary_template_spawner(registry.clone(), b.node_arc());
    let _serve = net_sdk::aggregator::install_aggregator_registry_service_with_spawner(
        &b, &registry, spawner,
    )
    .expect("install registry service");

    handshake(&a, &b, addr_b).await;

    // SDK wrapper: BoundRegistryClient binds A's mesh + B's
    // node_id at construction.
    let target = b.inner().node_id();
    let client =
        BoundRegistryClient::new(a.node_arc(), target).with_deadline(Duration::from_secs(2));
    assert_eq!(client.target_node_id(), target);

    // Initial list: empty.
    let groups = client.list().await.expect("list initial");
    assert!(groups.is_empty(), "expected empty registry initially");

    // Spawn against the configured template.
    let spawned = client
        .spawn("primary", "dynamic", 2)
        .await
        .expect("spawn primary");
    assert_eq!(spawned.name, "dynamic");
    assert_eq!(spawned.replicas.len(), 2);

    // List confirms.
    let groups = client.list().await.expect("list after spawn");
    let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, vec!["dynamic"]);

    // Unregister returns existed=true.
    let existed = client.unregister("dynamic").await.expect("unregister");
    assert!(existed);

    // List is empty again.
    let groups = client.list().await.expect("list after unregister");
    assert!(groups.is_empty());

    // Second unregister returns existed=false (idempotent).
    let existed = client
        .unregister("dynamic")
        .await
        .expect("unregister again");
    assert!(!existed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bound_client_unknown_template_surfaces_typed_server_error() {
    let (a, b, addr_b) = two_meshes().await;

    let registry: Arc<AggregatorRegistry> = Arc::new(AggregatorRegistry::new());
    let spawner = primary_template_spawner(registry.clone(), b.node_arc());
    let _serve = net_sdk::aggregator::install_aggregator_registry_service_with_spawner(
        &b, &registry, spawner,
    )
    .expect("install registry service");
    handshake(&a, &b, addr_b).await;

    let target = b.inner().node_id();
    let client =
        BoundRegistryClient::new(a.node_arc(), target).with_deadline(Duration::from_secs(2));

    match client.spawn("does-not-exist", "any", 1).await {
        Err(RegistryClientError::Server(RegistryRpcError::UnknownTemplate(t))) => {
            assert_eq!(t, "does-not-exist");
        }
        other => panic!("expected UnknownTemplate, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unbound_registry_client_can_target_multiple_nodes() {
    // The SDK's BoundRegistryClient is a convenience; the
    // unbound `RegistryClient` re-export from the SDK still
    // works for operators who want to talk to multiple
    // daemons.
    let (a, b, addr_b) = two_meshes().await;
    let registry: Arc<AggregatorRegistry> = Arc::new(AggregatorRegistry::new());
    let spawner = primary_template_spawner(registry.clone(), b.node_arc());
    let _serve = net_sdk::aggregator::install_aggregator_registry_service_with_spawner(
        &b, &registry, spawner,
    )
    .expect("install registry service");
    handshake(&a, &b, addr_b).await;

    let unbound = RegistryClient::new(a.node_arc()).with_deadline(Duration::from_secs(2));
    let groups = unbound.list(b.inner().node_id()).await.expect("list");
    assert!(groups.is_empty());
}
