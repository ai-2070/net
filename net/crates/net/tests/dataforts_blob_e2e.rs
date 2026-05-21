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
        BlobRef::Tree { root_hash, .. } => {
            // Walk the local tree on A to enumerate every chunk
            // hash the blob references (tree nodes + data + parity).
            // Each hash gets its own per-chunk replication
            // coordinator transition; without this every Tree-blob
            // e2e would short-circuit on the original panic.
            walk_tree_hashes_on_a(*root_hash, redex_a).await
        }
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

/// Walk a Tree-encoded blob's local representation on A to
/// collect every chunk hash the blob references: tree-node
/// chunks (root + internals + leaves) AND the leaves' data /
/// parity chunks. Used by `drive_chunk_roles_for` so the e2e
/// drives the per-chunk replication coordinator for every
/// constituent chunk, mirroring the v0.2 Manifest case.
async fn walk_tree_hashes_on_a(root_hash: [u8; 32], redex_a: &Arc<Redex>) -> Vec<[u8; 32]> {
    use net::adapter::net::dataforts::blob::blob_tree::TreeNode;
    let mut out: Vec<[u8; 32]> = Vec::new();
    let mut stack: Vec<[u8; 32]> = vec![root_hash];
    while let Some(node_hash) = stack.pop() {
        out.push(node_hash);
        // Each tree node is stored as a chunk under the same
        // `MeshBlobAdapter::chunk_channel_for_hash` shape as data
        // chunks. The test exists outside the adapter, so we read
        // directly off the Redex.
        let channel = MeshBlobAdapter::chunk_channel_for_hash(&node_hash);
        let Some(file) = redex_a.get_file(&channel) else {
            // Some hashes the recursion produces may be data chunks
            // (not decodable as TreeNode) — the read returns the
            // bytes; we attempt decode and fall through on parse
            // error. If the file doesn't exist at all, the blob
            // is malformed for this test's purposes.
            panic!(
                "walk_tree_hashes_on_a: chunk {} missing on A — tree malformed",
                hex(&node_hash),
            );
        };
        let events = file.read_range(0, file.len() as u64);
        let Some(payload) = events.into_iter().next() else {
            // Empty channel — leave node_hash recorded and
            // continue; the apply-side will surface NotFound if
            // it's actually needed.
            continue;
        };
        // Try to decode as TreeNode; if it fails, this hash is a
        // leaf data / parity chunk, not a tree node.
        let Ok(node) = TreeNode::decode(&payload.payload) else {
            continue;
        };
        match node {
            TreeNode::Internal { children } => {
                for (child_hash, _) in children {
                    stack.push(child_hash);
                }
            }
            TreeNode::Leaf { chunks } => {
                for c in chunks {
                    out.push(c.hash);
                }
            }
            TreeNode::ErasureLeaf { stripes } => {
                for stripe in stripes {
                    for c in stripe.chunks {
                        out.push(c.hash);
                    }
                }
            }
        }
    }
    out
}

fn hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
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
    assert_eq!(fetched.as_ref(), payload.as_slice());
}

/// v0.3 Tree blobs traverse `drive_chunk_roles_for` without
/// panicking. Pre-fix the helper hard-panicked on BlobRef::Tree,
/// blocking every Tree-related e2e from being writable. The
/// underlying wire path's behavior under multi-chunk Tree
/// replication is incomplete in v0.3 (the cross-node prefetch
/// only opens the root channel; leaf channels open lazily as
/// the walker descends — that requires a substrate hook not yet
/// wired). This test pins what's currently shipped: a Tree
/// BlobRef CAN flow through the helper, the tree-walk hash
/// enumeration produces the expected chunk set, and the
/// per-chunk replication coordinators can be transitioned for
/// every hash without panicking.
///
/// A follow-up commit pairs this with the cross-node Tree-walk
/// prefetch path to assert byte-equal round-trips on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_blob_tree_chunk_role_drive_does_not_panic() {
    use bytes::Bytes;
    use futures::stream;
    use net::adapter::net::dataforts::Encoding;

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

    // Small chunk size so the wire layer's per-event u16 size cap
    // (MAX_PAYLOAD_SIZE) doesn't bite. The internal test helper
    // takes an explicit chunk size; the production
    // `store_stream_tree` is fixed at 4 MiB which overflows.
    let chunk_size: u32 = 16 * 1024;
    let payload: Vec<u8> = (0..(chunk_size as usize * 3))
        .map(|i| (i % 251) as u8)
        .collect();
    let payload_clone = payload.clone();
    let s = stream::once(async move {
        Ok::<_, net::adapter::net::dataforts::BlobError>(Bytes::from(payload_clone))
    });
    let blob_ref = adapter_a
        .store_stream_tree_internal(Box::pin(s), Encoding::Replicated, chunk_size)
        .await
        .expect("A store_stream_tree_internal");
    assert!(
        matches!(blob_ref, BlobRef::Tree { .. }),
        "store_stream_tree_internal with a multi-chunk payload must emit a Tree BlobRef"
    );

    // walk_tree_hashes_on_a enumerates every chunk hash the
    // tree references. With 3 leaf chunks at chunk_size, the
    // root is a depth=1 Leaf carrying 3 ChunkRefs → 4 total
    // hashes (root + 3 data chunks).
    let all_hashes =
        walk_tree_hashes_on_a(*blob_ref.tree_root_hash().expect("Tree has root"), &redex_a).await;
    assert!(
        all_hashes.len() >= 4,
        "expected at least 4 hashes (root + 3 leaves); got {}",
        all_hashes.len()
    );

    // Pre-open every chunk channel on B WITH the replication
    // config (the chunk files on A were opened with replication;
    // reopens omitting the same config error per the replication
    // contract).
    let chunk_cfg =
        net::adapter::net::redex::RedexFileConfig::new().with_replication(Some(rep_cfg.clone()));
    for h in &all_hashes {
        let channel = MeshBlobAdapter::chunk_channel_for_hash(h);
        redex_b
            .open_file(&channel, chunk_cfg.clone())
            .expect("B pre-open chunk channel");
    }

    // Pre-fix this would panic on the Tree variant in
    // `drive_chunk_roles_for`. Post-fix, it walks the tree and
    // transitions every chunk's coordinator without error.
    drive_chunk_roles_for(&blob_ref, &redex_a, &redex_b).await;
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
        fetched.as_ref(),
        payload.as_slice(),
        "B's adapter must serve the migrated blob bytes"
    );
}

/// 3-node parallel migration: A publishes + heats; B and C
/// **independently** observe A's `heat:blob:` advertisement via
/// the capability gossip path and each runs
/// `drive_blob_migration_tick` against its own
/// `capability_index`. Both call `adapter.prefetch` and end up
/// holding the chunk through the per-chunk replication runtime.
///
/// Demonstrates that the gravity migration loop scales to >2
/// peers: every peer that observes the heat tag and admits the
/// `should_migrate_blob_to` verdict independently kicks off its
/// own pull. The shipped controller (`drive_blob_migration_tick`)
/// is local-decision-only, so the parallelism is natural — no
/// coordination across migrators required.
///
/// Setup: 3 nodes fully connected. All three adapters configured
/// with `Pinned([a, b, c])` so the per-chunk replication runtime
/// recognizes every peer as a valid replica when B / C call
/// `prefetch`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_node_parallel_migration_lands_blob_on_two_peers() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    let node_c = build_node().await;
    // Fully-connected mesh: A↔B, A↔C, B↔C. Order matters —
    // `MeshNode::accept` after `start()` is rejected, so do
    // every pair-handshake before any `start()`.
    handshake_no_start(&node_a, &node_b).await;
    handshake_no_start(&node_a, &node_c).await;
    handshake_no_start(&node_b, &node_c).await;
    node_a.start();
    node_b.start();
    node_c.start();

    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    let redex_c = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());
    redex_c.enable_replication(node_c.clone());

    let a_id = node_a.node_id();
    let b_id = node_b.node_id();
    let c_id = node_c.node_id();
    let rep_cfg = ReplicationConfig::new()
        .with_heartbeat_ms(150)
        .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id, c_id]));

    // A's adapter feeds the heat registry via fetch-path bumps.
    let heat_registry_a = Arc::new(parking_lot::Mutex::new(BlobHeatRegistry::new()));
    let adapter_a = MeshBlobAdapter::new("mesh-a", redex_a.clone())
        .with_replication(rep_cfg.clone())
        .with_blob_heat(
            heat_registry_a.clone(),
            net::adapter::net::dataforts::blob::DEFAULT_BLOB_HEAT_HALF_LIFE,
        );
    let adapter_b =
        MeshBlobAdapter::new("mesh-b", redex_b.clone()).with_replication(rep_cfg.clone());
    let adapter_c = MeshBlobAdapter::new("mesh-c", redex_c.clone()).with_replication(rep_cfg);

    // A announces participating caps for B and C's chain_caps
    // lookup. The mesh gossip path delivers them to every peer's
    // capability_index.
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

    // Wait for B AND C to learn A's caps before publishing.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let b_tags = node_b
            .capability_index()
            .get(a_id)
            .map(|c| c.tags.len())
            .unwrap_or(0);
        let c_tags = node_c
            .capability_index()
            .get(a_id)
            .map(|c| c.tags.len())
            .unwrap_or(0);
        if b_tags >= 9 && c_tags >= 9 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A stores + heats via 8 fetches.
    let payload = b"three node migration payload".to_vec();
    let hash: [u8; 32] = blake3::hash(&payload).into();
    let blob_ref = BlobRef::small(format!("mesh://{:?}", hash), hash, payload.len() as u64);
    adapter_a.store(&blob_ref, &payload).await.expect("A store");
    for _ in 0..8 {
        adapter_a.fetch(&blob_ref).await.expect("A fetch");
    }

    // A emits `heat:blob:<hex>=<rate>` via tick → capability
    // rebroadcast → B and C both observe.
    let policy = DataGravityPolicy::default();
    let sink: &dyn BlobHeatSink = &*node_a;
    let emitted = adapter_a
        .tick_blob_heat(&policy, sink)
        .await
        .expect("A tick_blob_heat");
    assert!(emitted >= 1, "A's tick must emit at least one heat:blob");

    // Wait for the heat:blob tag to land on both B and C.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut b_saw = false;
    let mut c_saw = false;
    while tokio::time::Instant::now() < deadline {
        let blob_heat_present = |idx: &net::adapter::net::behavior::capability::CapabilityIndex| {
            idx.get(a_id)
                .map(|c| {
                    c.tags.iter().any(|t| match t {
                        net::adapter::net::behavior::tag::Tag::Reserved { prefix, body } => {
                            prefix == "heat:" && body.starts_with("blob:")
                        }
                        _ => false,
                    })
                })
                .unwrap_or(false)
        };
        if !b_saw {
            b_saw = blob_heat_present(node_b.capability_index().as_ref());
        }
        if !c_saw {
            c_saw = blob_heat_present(node_c.capability_index().as_ref());
        }
        if b_saw && c_saw {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        b_saw && c_saw,
        "both B (saw={b_saw}) and C (saw={c_saw}) must surface A's heat:blob tag within 5s"
    );

    // B and C each run the migration tick independently. Each
    // builds its own local caps + queries its own capability
    // index. Both should admit + prefetch.
    let local_caps_for_migrator = CapabilitySet::new()
        .with_blob_capability(BlobCapability::storage_participating(100, 50))
        .with_gravity_capability(GravityCapability {
            enabled: true,
            scope: TopologyScope::Mesh,
            proximity: 128,
        });
    let report_b = drive_blob_migration_tick(
        &local_caps_for_migrator,
        node_b.capability_index().as_ref(),
        &adapter_b,
        |_| Some(payload.len() as u64),
    )
    .await;
    let report_c = drive_blob_migration_tick(
        &local_caps_for_migrator,
        node_c.capability_index().as_ref(),
        &adapter_c,
        |_| Some(payload.len() as u64),
    )
    .await;
    assert!(
        report_b.admitted >= 1 && report_c.admitted >= 1,
        "both B and C must admit the hot blob; got B={report_b:?}, C={report_c:?}"
    );
    assert_eq!(
        report_b.prefetch_errors + report_c.prefetch_errors,
        0,
        "no prefetch errors expected"
    );

    // Drive the per-chunk replication roles for every replica
    // pair (A leader, B+C replicas). The 2-node helper only
    // covers two adapters; do all three here inline.
    let channel = MeshBlobAdapter::chunk_channel_for_hash(&hash);
    let coord_a = redex_a
        .replication_coordinator_for(&channel)
        .expect("coord A");
    let coord_b = redex_b
        .replication_coordinator_for(&channel)
        .expect("coord B");
    let coord_c = redex_c
        .replication_coordinator_for(&channel)
        .expect("coord C");
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
    coord_c
        .transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
        .await
        .expect("C → Replica");

    // Both B and C should converge their chunk files to seq=1
    // (the Small blob is one event). Use a generous deadline
    // because the catch-up is heartbeat-driven on a 150ms cadence.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while tokio::time::Instant::now() < deadline {
        let b_has = redex_b
            .get_file(&channel)
            .map(|f| f.next_seq() >= 1)
            .unwrap_or(false);
        let c_has = redex_c
            .get_file(&channel)
            .map(|f| f.next_seq() >= 1)
            .unwrap_or(false);
        if b_has && c_has {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let fetched_b = adapter_b
        .fetch(&blob_ref)
        .await
        .expect("B fetch (after parallel migration)");
    let fetched_c = adapter_c
        .fetch(&blob_ref)
        .await
        .expect("C fetch (after parallel migration)");
    assert_eq!(
        fetched_b.as_ref(),
        payload.as_slice(),
        "B's migrated bytes must equal source"
    );
    assert_eq!(
        fetched_c.as_ref(),
        payload.as_slice(),
        "C's migrated bytes must equal source"
    );
}

/// Pair-handshake without `start()` — used by the 3-node test
/// because `accept()` after `start()` is rejected. The 2-node
/// tests can call `handshake()` directly which does both;
/// larger topologies (3+ nodes) need to batch the accepts
/// before any `start()` lands.
async fn handshake_no_start(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
}

// ============================================================================
// Active-overflow wire integration (P3)
//
// The full overflow loop is sender-tick → controller picks coldest →
// `send_overflow_push` → receiver's nRPC handler runs admission →
// receiver opens chunk channel → bytes pull via the existing
// replication runtime. Tests below exercise the *wire* slice (nudge +
// admission + ack); the controller / tick driver is unit-tested in
// the `dataforts::blob::overflow` module, and chunk-replication is
// already covered by `mesh_blob_prefetch_replicates_chunks_from_peer`
// above.
// ============================================================================

#[cfg(feature = "cortex")]
fn overflow_enabled_caps(disk_free_gb: u64) -> CapabilitySet {
    // Local caps for an overflow-participating node: storage
    // + overflow opt-in + headroom + mesh-wide gravity scope.
    // Mirrors the unit-test fixtures in
    // `dataforts::blob::overflow::tests`.
    BlobCapability {
        storage: true,
        disk_total_gb: 100,
        disk_free_gb,
        overflow_enabled: true,
    }
    .write_into(
        GravityCapability {
            enabled: true,
            scope: TopologyScope::Mesh,
            proximity: 128,
        }
        .write_into(CapabilitySet::new()),
    )
}

#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overflow_push_nudge_round_trips_through_mesh_rpc() {
    // Sender A nudges receiver B over the active-overflow nRPC.
    // Both nodes carry the `dataforts.blob.overflow` capability
    // tag + matching gravity scope; A's caps reach B via the
    // standard gossip path. B registers `serve_overflow_push`
    // pointing at a real `MeshBlobAdapter`; A calls
    // `send_overflow_push`. The ack proves: postcard encode
    // works, nRPC dispatch reaches B, B's admission ran and
    // returned Admit, B's `adapter.prefetch` opened the chunk
    // channel without erroring, the typed ack encode came back
    // through the same path.
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    let a_id = node_a.node_id();
    let b_id = node_b.node_id();
    let rep_cfg =
        ReplicationConfig::new().with_placement(PlacementStrategy::Pinned(vec![a_id, b_id]));

    let adapter_b =
        Arc::new(MeshBlobAdapter::new("mesh-b", redex_b.clone()).with_replication(rep_cfg.clone()));

    // Receiver registers the overflow-push handler FIRST so the
    // subsequent `announce_capabilities` call merges the
    // `nrpc:dataforts.blob.overflow_push` tag (without it, the
    // v0.4 capability-auth callee-side gate would deny the
    // inbound REQUEST — A's announcement of B in A's index has
    // the nrpc tag too, but B's local self-announcement is the
    // ground truth the bridge consults). The ServeHandle drops
    // when the test ends, deregistering the handler automatically.
    let _handle = node_b
        .serve_overflow_push(Arc::clone(&adapter_b))
        .expect("serve overflow push");

    // Both nodes advertise overflow-participating caps. The
    // admission gate reads from `user_caps_snapshot` on B and
    // from the capability index for the sender — both must be
    // populated.
    node_a
        .announce_capabilities(overflow_enabled_caps(80))
        .await
        .expect("A announce");
    node_b
        .announce_capabilities(overflow_enabled_caps(90))
        .await
        .expect("B announce");

    // Wait for gossip to settle so A's caps land in B's
    // capability index (B's admission needs to see A's
    // `dataforts.blob.overflow` tag). The
    // `with_min_announce_interval(10ms)` in `test_config`
    // makes this fast; 500ms is generous.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        let caps = node_b.capability_index_arc().get(a_id).unwrap_or_default();
        let blob = BlobCapability::from_capability_set(&caps);
        if blob.overflow_enabled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Fire the nudge. A 1 KiB blob — well under B's
    // advertised disk-free.
    let hash: [u8; 32] = blake3::hash(b"overflow-push-test-payload").into();
    let ack = node_a
        .send_overflow_push(b_id, hash, 1024)
        .await
        .expect("send overflow push");

    use net::adapter::net::dataforts::blob::overflow::OverflowPushAck;
    assert_eq!(
        ack,
        OverflowPushAck::Accepted,
        "B must accept the nudge; got {:?}",
        ack
    );
}

#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overflow_push_rejected_when_receiver_not_participating() {
    // Receiver B has storage + gravity but did NOT opt into
    // overflow. A's nudge round-trips but B returns
    // `Rejected(NotParticipating)`. Proves the rejection path
    // also rides the wire correctly.
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    let redex_b = Arc::new(Redex::new());
    redex_b.enable_replication(node_b.clone());

    let adapter_b = Arc::new(MeshBlobAdapter::new("mesh-b", redex_b.clone()));

    // Receiver registers the overflow-push handler FIRST so the
    // subsequent `announce_capabilities` call merges the
    // `nrpc:dataforts.blob.overflow_push` tag — required by the
    // v0.4 capability-auth callee-side gate.
    let _handle = node_b
        .serve_overflow_push(Arc::clone(&adapter_b))
        .expect("serve overflow push");

    // A opts in; B does NOT.
    node_a
        .announce_capabilities(overflow_enabled_caps(80))
        .await
        .expect("A announce");

    // B's caps: storage + gravity, but NO overflow tag.
    let b_caps = BlobCapability::storage_participating(100, 90).write_into(
        GravityCapability {
            enabled: true,
            scope: TopologyScope::Mesh,
            proximity: 128,
        }
        .write_into(CapabilitySet::new()),
    );
    node_b
        .announce_capabilities(b_caps)
        .await
        .expect("B announce");

    // Wait for A's caps to propagate to B (admission reads
    // sender_caps from B's index).
    let a_id = node_a.node_id();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        let caps = node_b.capability_index_arc().get(a_id).unwrap_or_default();
        let blob = BlobCapability::from_capability_set(&caps);
        if blob.overflow_enabled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let hash: [u8; 32] = blake3::hash(b"overflow-rejected-test").into();
    let ack = node_a
        .send_overflow_push(node_b.node_id(), hash, 1024)
        .await
        .expect("send overflow push");

    use net::adapter::net::dataforts::blob::admission::OverflowReject;
    use net::adapter::net::dataforts::blob::overflow::OverflowPushAck;
    assert_eq!(
        ack,
        OverflowPushAck::Rejected(OverflowReject::NotParticipating),
        "B must reject NotParticipating; got {:?}",
        ack
    );
}

#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn announce_blob_overflow_state_syncs_local_caps_with_adapter_toggle() {
    // `MeshNode::announce_blob_overflow_state` reads the
    // adapter's current `overflow_enabled` boolean and pushes
    // a cap-set rebroadcast that matches. Operators who flip
    // `set_overflow_enabled` use this helper instead of
    // hand-rolling the snapshot + tag mutation + announce
    // sequence. Regression: prior to the helper, peers would
    // see stale caps after a toggle and the sender's tick
    // self-check would short-circuit.
    let node = build_node().await;
    let adapter = Arc::new(MeshBlobAdapter::new("mesh-toggle", Arc::new(Redex::new())));

    // Seed local caps with overflow OFF (the adapter default).
    node.announce_capabilities(
        BlobCapability::storage_participating(100, 80).write_into(
            GravityCapability {
                enabled: true,
                scope: TopologyScope::Mesh,
                proximity: 128,
            }
            .write_into(CapabilitySet::new()),
        ),
    )
    .await
    .expect("seed caps");

    // Flip the adapter's master switch ON, then sync. The
    // helper must rewrite local caps so subsequent reads
    // observe the `dataforts.blob.overflow` tag.
    adapter.set_overflow_enabled(true);
    node.announce_blob_overflow_state(&adapter)
        .await
        .expect("sync caps after on");
    let live = node.capability_index_arc().get(node.node_id()).unwrap();
    assert!(
        BlobCapability::from_capability_set(&live).overflow_enabled,
        "after sync(on), local caps must carry the overflow tag"
    );

    // Flip back OFF. The helper must clear the tag.
    adapter.set_overflow_enabled(false);
    node.announce_blob_overflow_state(&adapter)
        .await
        .expect("sync caps after off");
    let live = node.capability_index_arc().get(node.node_id()).unwrap();
    assert!(
        !BlobCapability::from_capability_set(&live).overflow_enabled,
        "after sync(off), local caps must NOT carry the overflow tag"
    );
}
