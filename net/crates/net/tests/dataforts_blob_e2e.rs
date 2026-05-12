//! End-to-end integration tests for the Dataforts v0.2 substrate-
//! owned blob CAS surface.
//!
//! Two scenarios on the live wire:
//!
//! 1. **Prefetch round-trip** (`mesh_blob_prefetch_replicates_chunks_from_peer`):
//!    A stores a blob → its chunk channel opens with replication
//!    config + A drives Leader → B calls `adapter.prefetch(&blob_ref)`
//!    which opens the same chunk channel on B with replication +
//!    transitions B to Replica → the per-chunk replication runtime
//!    catches B up → `adapter.fetch(&blob_ref)` on B returns the
//!    original bytes.
//!
//! 2. **Migration round-trip** (`gravity_migration_controller_fetches_hot_blob`):
//!    A stores + fetches a blob several times (builds blob heat)
//!    → A's `MeshNode::announce_blob_heat_batch` emits
//!    `heat:blob:<hex>=<rate>` tags through gossip → B observes
//!    them via `capability_index` → B's `drive_blob_migration_tick`
//!    admits and calls `adapter.prefetch` → bytes land on B.
//!
//! Both tests use `PlacementStrategy::Pinned([a_id, b_id])` because
//! the Phase-F placement-filter election isn't wired (same caveat
//! as `redex_replication_e2e.rs`); coords start in `Idle` and the
//! tests drive role transitions manually.
//!
//! Run: `cargo test --features dataforts,redex-disk --test
//! dataforts_blob_e2e`

#![cfg(feature = "dataforts")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::dataforts_capabilities::{
    BlobCapability, GravityCapability, GreedyCapability, TopologyScope,
};
use net::adapter::net::dataforts::blob::{
    drive_blob_migration_tick, BlobAdapter, BlobRef, MeshBlobAdapter,
};
use net::adapter::net::dataforts::gravity::{BlobHeatRegistry, BlobHeatSink, DataGravityPolicy};
use net::adapter::net::redex::{
    PlacementStrategy, Redex, ReplicaRole, ReplicationConfig, TransitionSignal,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250))
        // The production default is 10s — too long for an e2e
        // that re-announces caps multiple times within a single
        // test (initial publisher_caps, then per-tick heat:blob
        // emissions). The rate limit silently drops the second
        // broadcast under the production interval; the test
        // would never see the updated caps on the peer side.
        .with_min_announce_interval(Duration::from_millis(10));
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

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
}

/// Drive the per-chunk replication coordinators into Leader (A) /
/// Replica (B) for every constituent chunk of `blob_ref`. Manual
/// because Phase-F placement-filter election isn't wired; the
/// substrate's existing replication e2e drives the same path.
async fn drive_chunk_roles_for(blob_ref: &BlobRef, redex_a: &Arc<Redex>, redex_b: &Arc<Redex>) {
    let hashes: Vec<[u8; 32]> = match blob_ref {
        BlobRef::Small { hash, .. } => vec![*hash],
        BlobRef::Manifest { chunks, .. } => chunks.iter().map(|c| c.hash).collect(),
    };
    for hash in hashes {
        let channel = MeshBlobAdapter::chunk_channel_for_hash(&hash);
        let coord_a = redex_a
            .replication_coordinator_for(&channel)
            .expect("coord A");
        let coord_b = redex_b
            .replication_coordinator_for(&channel)
            .expect("coord B");
        coord_a
            .transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .expect("A → Replica");
        coord_a
            .transition_to(ReplicaRole::Candidate, TransitionSignal::MissedHeartbeats)
            .await
            .expect("A → Candidate");
        coord_a
            .transition_to(ReplicaRole::Leader, TransitionSignal::ElectionWon)
            .await
            .expect("A → Leader");
        coord_b
            .transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .expect("B → Replica");
    }
}

fn pinned_replication_cfg(a_id: u64, b_id: u64) -> ReplicationConfig {
    ReplicationConfig::new()
        .with_heartbeat_ms(150)
        .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id]))
}

/// A stores a blob; B prefetches it; B reads back the original
/// bytes via the standard `adapter.fetch` path. Closes the loop
/// PR-5i opened: the prefetch isn't decision-only any more —
/// chunks actually arrive on the calling node via the per-chunk
/// replication runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_blob_prefetch_replicates_chunks_from_peer() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    let a_id = node_a.node_id();
    let b_id = node_b.node_id();
    let rep_cfg = pinned_replication_cfg(a_id, b_id);

    let adapter_a =
        MeshBlobAdapter::new("mesh-a", redex_a.clone()).with_replication(rep_cfg.clone());
    let adapter_b = MeshBlobAdapter::new("mesh-b", redex_b.clone()).with_replication(rep_cfg);

    // A stores a payload — the chunk's content channel opens with
    // the replication config armed on A's side. The replication
    // runtime spawns but the role is still Idle.
    let payload = b"hello from A to B via blob prefetch".to_vec();
    let hash: [u8; 32] = blake3::hash(&payload).into();
    let blob_ref = BlobRef::small(format!("mesh://{:?}", hash), hash, payload.len() as u64);
    adapter_a.store(&blob_ref, &payload).await.expect("A store");

    // B kicks off prefetch — opens the same channel on B with
    // replication enabled; runtime spawns Idle there too. The
    // chunk bytes haven't migrated yet because no coord is
    // Leader / Replica.
    adapter_b.prefetch(&blob_ref).await.expect("B prefetch");

    // Drive the per-chunk roles manually (same shape as
    // redex_replication_e2e — Phase-F election isn't wired).
    drive_chunk_roles_for(&blob_ref, &redex_a, &redex_b).await;

    // Wait for the replica's catch-up cycle to copy the chunk
    // bytes from A. The chunk file is single-event (the whole
    // Small blob lives at seq 0); B's `next_seq` becoming 1 is
    // the sentinel.
    let channel = MeshBlobAdapter::chunk_channel_for_hash(&hash);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let file_b = redex_b.get_file(&channel);
        if file_b.map(|f| f.next_seq() >= 1).unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let file_b = redex_b
        .get_file(&channel)
        .expect("B should have the chunk file by now");
    assert_eq!(
        file_b.next_seq(),
        1,
        "B's chunk file must have caught up to A's seq=1"
    );

    // Adapter-level read on B returns the original bytes.
    let fetched = adapter_b.fetch(&blob_ref).await.expect("B fetch");
    assert_eq!(fetched, payload);
}

/// A heats a blob via repeated fetches, ticks blob-heat tags
/// onto its `CapabilitySet`, gossip carries the tags to B, and
/// B's migration controller admits the candidate + calls
/// `adapter.prefetch`. End-to-end gravity migration on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gravity_migration_controller_fetches_hot_blob() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    let a_id = node_a.node_id();
    let b_id = node_b.node_id();
    let rep_cfg = pinned_replication_cfg(a_id, b_id);

    // A: adapter + heat registry. Fetch-path bumps the registry.
    let heat_registry_a = Arc::new(parking_lot::Mutex::new(BlobHeatRegistry::new()));
    let adapter_a = MeshBlobAdapter::new("mesh-a", redex_a.clone())
        .with_replication(rep_cfg.clone())
        .with_blob_heat(
            heat_registry_a.clone(),
            net::adapter::net::dataforts::blob::DEFAULT_BLOB_HEAT_HALF_LIFE,
        );
    let adapter_b = MeshBlobAdapter::new("mesh-b", redex_b.clone()).with_replication(rep_cfg);

    // A announces participating caps so B's chain_caps lookup
    // surfaces them. Includes scope=mesh + gravity=enabled so the
    // controller's `should_migrate_blob_to` admits.
    let publisher_caps = CapabilitySet::new()
        .with_blob_capability(BlobCapability::storage_participating(100, 50))
        .with_greedy_capability(GreedyCapability {
            enabled: true,
            scope: TopologyScope::Mesh,
            proximity: 128,
        })
        .with_gravity_capability(GravityCapability {
            enabled: true,
            scope: TopologyScope::Mesh,
            proximity: 128,
        });
    node_a
        .announce_capabilities(publisher_caps)
        .await
        .expect("A announce");

    // Wait for B's capability index to learn A's caps before
    // emitting the heat tag — the heat-tick announce is a re-
    // announce that needs the prior version to have landed.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let tags = node_b
            .capability_index()
            .get(a_id)
            .map(|c| c.tags.len())
            .unwrap_or(0);
        if tags >= 9 {
            // publisher_caps emits 9 dataforts.* tags (3 blob +
            // 3 greedy + 3 gravity).
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A: store + fetch repeatedly to build heat.
    let payload = b"hot blob bytes".to_vec();
    let hash: [u8; 32] = blake3::hash(&payload).into();
    let blob_ref = BlobRef::small(format!("mesh://{:?}", hash), hash, payload.len() as u64);
    adapter_a.store(&blob_ref, &payload).await.expect("A store");
    for _ in 0..8 {
        adapter_a.fetch(&blob_ref).await.expect("A fetch");
    }

    // A: tick the heat registry — emits `heat:blob:<hex>=<rate>`
    // tags onto A's local CapabilitySet + announces.
    let policy = DataGravityPolicy::default();
    let sink: &dyn BlobHeatSink = &*node_a;
    let emitted = adapter_a
        .tick_blob_heat(&policy, sink)
        .await
        .expect("A tick_blob_heat");
    assert!(
        emitted >= 1,
        "A's tick must emit at least one heat:blob entry (got {emitted})"
    );
    // Sanity: A's own capability_index should now carry a
    // heat:blob:<hex> tag for itself. Self-announces skip the
    // unauth-heat filter, so A's local view is the ground truth
    // for whether the tick wrote the tag at all.
    let a_has_blob_heat = node_a
        .capability_index()
        .get(a_id)
        .map(|c| {
            c.tags
                .iter()
                .any(|t| t.to_string().starts_with("heat:blob:"))
        })
        .unwrap_or(false);
    assert!(
        a_has_blob_heat,
        "A's local capability_index must carry the heat:blob tag after tick"
    );

    // Wait for B's capability_index to absorb the new tags via
    // gossip — the test polls for at least one peer carrying a
    // `heat:blob:<hex>=*` tag.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut saw_blob_heat = false;
    let mut last_seen_tags: Vec<String> = Vec::new();
    while tokio::time::Instant::now() < deadline {
        if let Some(caps) = node_b.capability_index().get(a_id) {
            last_seen_tags = caps.tags.iter().map(|t| t.to_string()).collect();
            saw_blob_heat = caps.tags.iter().any(|t| match t {
                net::adapter::net::behavior::tag::Tag::Reserved { prefix, body } => {
                    prefix == "heat:" && body.starts_with("blob:")
                }
                _ => false,
            });
            if saw_blob_heat {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        saw_blob_heat,
        "B's capability_index must surface A's heat:blob tags within 5s; \
         last seen B-side tags: {:?}",
        last_seen_tags,
    );

    // B: drive the migration tick. `should_migrate_blob_to`
    // admits (B has its own participating caps below), and the
    // controller calls `adapter_b.prefetch(blob_ref)` for the
    // hot hash → chunk channel opens on B with replication.
    let local_caps_b = CapabilitySet::new()
        .with_blob_capability(BlobCapability::storage_participating(100, 50))
        .with_gravity_capability(GravityCapability {
            enabled: true,
            scope: TopologyScope::Mesh,
            proximity: 128,
        });
    let report = drive_blob_migration_tick(
        &local_caps_b,
        node_b.capability_index().as_ref(),
        &adapter_b,
        |_h| Some(payload.len() as u64),
    )
    .await;
    assert!(
        report.admitted >= 1,
        "migration tick must admit at least one hot blob; got report={report:?}"
    );
    assert_eq!(report.prefetch_errors, 0, "no prefetch errors expected");

    // Drive the per-chunk roles so the prefetched channel can
    // actually catch up from A.
    drive_chunk_roles_for(&blob_ref, &redex_a, &redex_b).await;

    // Wait for B's chunk file to populate.
    let channel = MeshBlobAdapter::chunk_channel_for_hash(&hash);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let file_b = redex_b.get_file(&channel);
        if file_b.map(|f| f.next_seq() >= 1).unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let fetched = adapter_b.fetch(&blob_ref).await.expect("B fetch");
    assert_eq!(
        fetched, payload,
        "B's adapter must serve the migrated blob bytes"
    );
}
