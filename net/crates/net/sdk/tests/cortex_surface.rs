//! Rust SDK smoke test for the CortEX surface.
//!
//! Exercises the end-to-end re-export chain in `sdk/src/cortex.rs`:
//! `Redex` → `NetDb::builder` → `TasksAdapter` / `MemoriesAdapter` →
//! `snapshot_and_watch`. If any public type or fluent-builder method
//! disappears from the re-export list, this test stops compiling.

#![cfg(feature = "cortex")]

use futures::StreamExt;
use net_sdk::cortex::{MemoriesAdapter, NetDb, Redex, TaskStatus, TasksAdapter};

const ORIGIN: u64 = 0xABCD_EF01;

#[tokio::test]
async fn netdb_builder_bundles_both_adapters() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build()
        .await
        .expect("builder completes");

    let t_seq = db.tasks().create(1, "write docs", 100).unwrap();
    let m_seq = db
        .memories()
        .store(1, "hello", Vec::<String>::new(), "alice", 100)
        .unwrap();
    db.tasks().wait_for_seq(t_seq).await.unwrap();
    db.memories().wait_for_seq(m_seq).await.unwrap();

    assert_eq!(db.tasks().count(), 1);
    assert_eq!(db.memories().count(), 1);
}

#[tokio::test]
async fn standalone_adapter_opens_without_netdb() {
    // Users who only need one model can open it directly without the
    // NetDb facade. The re-export surface must cover this path.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    let seq = tasks.create(1, "hello", 100).unwrap();
    tasks.wait_for_seq(seq).await.unwrap();

    let state = tasks.state();
    let guard = state.read();
    let task = guard.get(1).unwrap();
    assert_eq!(task.status, TaskStatus::Pending);
    // Touch the memories handle so the import isn't spurious-warnings.
    let _ = memories.count();
}

#[tokio::test]
async fn snapshot_and_watch_round_trip() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .build()
        .await
        .unwrap();

    let seq = db.tasks().create(1, "seed", 100).unwrap();
    db.tasks().wait_for_seq(seq).await.unwrap();

    let watcher = db.tasks().watch();
    let (snapshot, mut stream) = db.tasks().snapshot_and_watch(watcher);
    assert_eq!(snapshot.len(), 1);

    let seq = db.tasks().create(2, "next", 200).unwrap();
    db.tasks().wait_for_seq(seq).await.unwrap();

    let delta = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .expect("stream must emit after mutation")
        .expect("stream must not end");
    assert_eq!(delta.len(), 2);
}

#[tokio::test]
async fn netdb_snapshot_bundle_round_trips() {
    // Snapshot bundle serializes state for both enabled models and
    // round-trips through `build_from_snapshot`.
    let redex_a = Redex::new();
    let db_a = NetDb::builder(redex_a)
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build()
        .await
        .unwrap();
    let t_seq = db_a.tasks().create(1, "task", 100).unwrap();
    let m_seq = db_a
        .memories()
        .store(1, "memory", Vec::<String>::new(), "alice", 100)
        .unwrap();
    db_a.tasks().wait_for_seq(t_seq).await.unwrap();
    db_a.memories().wait_for_seq(m_seq).await.unwrap();

    let snapshot = db_a.snapshot().unwrap();

    let redex_b = Redex::new();
    let db_b = NetDb::builder(redex_b)
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build_from_snapshot(&snapshot)
        .await
        .unwrap();

    assert_eq!(db_b.tasks().count(), 1);
    assert_eq!(db_b.memories().count(), 1);
}
