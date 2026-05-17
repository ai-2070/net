//! Integration tests for NetDB.
//!
//! Covers the unified `NetDb` handle end-to-end: build with multiple
//! models, CRUD through `db.tasks()` / `db.memories()`, filter-based
//! `find_many` / `count_where` / `exists_where`, whole-db snapshot
//! and restore.

#![cfg(feature = "netdb")]

use net::adapter::net::cortex::memories::OrderBy as MemoriesOrderBy;
use net::adapter::net::cortex::tasks::{OrderBy as TasksOrderBy, TaskStatus};
use net::adapter::net::netdb::{MemoriesFilter, NetDb, NetDbSnapshot, TasksFilter};
use net::adapter::net::redex::Redex;

const ORIGIN: u64 = 0xABCD_EF01;

#[tokio::test]
async fn test_netdb_build_with_both_models() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build()
        .await
        .unwrap();

    assert!(db.try_tasks().is_some());
    assert!(db.try_memories().is_some());
}

#[tokio::test]
async fn test_netdb_build_with_only_tasks() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .build()
        .await
        .unwrap();

    assert!(db.try_tasks().is_some());
    assert!(db.try_memories().is_none());
}

#[tokio::test]
async fn test_netdb_crud_through_tasks_handle() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .build()
        .await
        .unwrap();

    let tasks = db.tasks();
    tasks.create(1, "write plan", 100).unwrap();
    tasks.create(2, "ship adapter", 200).unwrap();
    let seq = tasks.complete(1, 150).unwrap();
    tasks.wait_for_seq(seq).await.unwrap();

    let state = tasks.state();
    let guard = state.read();
    assert_eq!(guard.len(), 2);
    assert_eq!(guard.find_unique(1).unwrap().status, TaskStatus::Completed);
}

#[tokio::test]
async fn test_netdb_find_many_on_tasks_state() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .build()
        .await
        .unwrap();

    for i in 1..=10u64 {
        db.tasks().create(i, format!("t-{}", i), 100 * i).unwrap();
    }
    // Complete the even ids.
    for i in (2..=10u64).step_by(2) {
        db.tasks().complete(i, 1000 + i).unwrap();
    }
    let last = db.tasks().complete(10, 9999).unwrap();
    db.tasks().wait_for_seq(last).await.unwrap();

    let state = db.tasks().state();
    let guard = state.read();

    // Pending tasks, ordered by id.
    let filter = TasksFilter {
        status: Some(TaskStatus::Pending),
        order_by: Some(TasksOrderBy::IdAsc),
        ..Default::default()
    };
    let pending = guard.find_many(&filter);
    let ids: Vec<_> = pending.iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![1, 3, 5, 7, 9]);

    // Count via filter.
    assert_eq!(guard.count_where(&filter), 5);

    // Exists.
    assert!(guard.exists_where(&filter));

    // Completed tasks, limit 2, ordered by updated desc.
    let completed_filter = TasksFilter {
        status: Some(TaskStatus::Completed),
        order_by: Some(TasksOrderBy::UpdatedDesc),
        limit: Some(2),
        ..Default::default()
    };
    let completed = guard.find_many(&completed_filter);
    assert_eq!(completed.len(), 2);
    // id=10 had its updated_ns bumped twice (latest = 9999).
    assert_eq!(completed[0].id, 10);
}

#[tokio::test]
async fn test_netdb_find_many_on_memories_state() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_memories()
        .build()
        .await
        .unwrap();

    db.memories()
        .store(
            1,
            "meeting notes",
            vec!["work".into(), "notes".into()],
            "alice",
            100,
        )
        .unwrap();
    db.memories()
        .store(2, "grocery list", vec!["personal".into()], "alice", 200)
        .unwrap();
    db.memories()
        .store(
            3,
            "api design",
            vec!["work".into(), "design".into()],
            "bob",
            300,
        )
        .unwrap();
    let seq = db.memories().pin(1, 310).unwrap();
    db.memories().wait_for_seq(seq).await.unwrap();

    let state = db.memories().state();
    let guard = state.read();

    // Tag filter → any memory with "work".
    let work_filter = MemoriesFilter {
        tag: Some("work".into()),
        order_by: Some(MemoriesOrderBy::IdAsc),
        ..Default::default()
    };
    let work_memories = guard.find_many(&work_filter);
    let ids: Vec<_> = work_memories.iter().map(|m| m.id).collect();
    assert_eq!(ids, vec![1, 3]);

    // Pinned filter.
    let pinned_filter = MemoriesFilter {
        pinned: Some(true),
        ..Default::default()
    };
    assert_eq!(guard.count_where(&pinned_filter), 1);
    assert!(guard.exists_where(&pinned_filter));

    // Source filter + content search combined.
    let combo = MemoriesFilter {
        source: Some("bob".into()),
        content_contains: Some("API".into()),
        ..Default::default()
    };
    let results = guard.find_many(&combo);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 3);
}

#[tokio::test]
async fn test_netdb_whole_snapshot_and_restore() {
    // Fill both models in one NetDb, snapshot, encode, decode,
    // rebuild a FRESH NetDb from the decoded snapshot, and verify
    // both models' state matches.
    //
    // Each NetDb owns its Redex so they don't share underlying
    // files — state comes entirely from the snapshot blob. Continued
    // ingest on the restored db is tested separately at the tasks/
    // memories layer (same-file continuation is already covered by
    // integration_cortex_{tasks,memories}).
    let snapshot_blob: Vec<u8>;

    {
        let db = NetDb::builder(Redex::new())
            .origin(ORIGIN)
            .with_tasks()
            .with_memories()
            .build()
            .await
            .unwrap();

        db.tasks().create(1, "alpha", 100).unwrap();
        db.tasks().create(2, "beta", 200).unwrap();
        let t_seq = db.tasks().complete(1, 150).unwrap();

        db.memories()
            .store(1, "hello", vec!["x".into()], "alice", 100)
            .unwrap();
        let m_seq = db.memories().pin(1, 110).unwrap();

        db.tasks().wait_for_seq(t_seq).await.unwrap();
        db.memories().wait_for_seq(m_seq).await.unwrap();

        let snapshot = db.snapshot().unwrap();
        assert!(snapshot.tasks.is_some());
        assert!(snapshot.memories.is_some());
        snapshot_blob = snapshot.encode().unwrap();
        db.close().unwrap();
    }

    // Decode the blob and rebuild against a fresh Redex.
    let restored = NetDbSnapshot::decode(&snapshot_blob).unwrap();
    let db2 = NetDb::builder(Redex::new())
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build_from_snapshot(&restored)
        .await
        .unwrap();

    // Tasks state restored.
    let t_state = db2.tasks().state();
    let t_guard = t_state.read();
    assert_eq!(t_guard.len(), 2);
    assert_eq!(
        t_guard.find_unique(1).unwrap().status,
        TaskStatus::Completed
    );
    assert_eq!(t_guard.find_unique(2).unwrap().title, "beta");
    drop(t_guard);

    // Memories state restored.
    let m_state = db2.memories().state();
    let m_guard = m_state.read();
    assert_eq!(m_guard.len(), 1);
    assert!(m_guard.find_unique(1).unwrap().pinned);
    assert_eq!(m_guard.find_unique(1).unwrap().content, "hello");
}

#[tokio::test]
async fn test_netdb_build_from_empty_snapshot_is_fresh_open() {
    // A model listed in `with_*()` but with None in the snapshot is
    // opened from scratch — equivalent to build() for that model.
    let empty = NetDbSnapshot {
        tasks: None,
        memories: None,
    };
    let db = NetDb::builder(Redex::new())
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build_from_snapshot(&empty)
        .await
        .unwrap();
    assert_eq!(db.tasks().count(), 0);
    assert_eq!(db.memories().count(), 0);
}

#[tokio::test]
async fn test_netdb_close_is_idempotent() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build()
        .await
        .unwrap();

    db.close().unwrap();
    db.close().unwrap(); // idempotent
}

#[tokio::test]
#[should_panic(expected = "tasks not enabled")]
async fn test_netdb_tasks_without_with_tasks_panics() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_memories()
        .build()
        .await
        .unwrap();
    // Should panic — tasks weren't enabled.
    let _ = db.tasks();
}

/// Regression: under one `NetDb` with BOTH tasks and memories
/// enabled, alternating ingests across the two adapters must
/// not surface `concurrent ingest_typed produced duplicate
/// app_seq` errors.
///
/// The pre-fix `ingest_typed` did `load → ingest → CAS-commit`
/// on `app_seq`. When the `WatermarkingFold` task processed the
/// just-ingested event before the foreground thread reached its
/// CAS, the watermark advanced to the expected post-CAS value
/// and the CAS surfaced a phantom-duplicate error. Single-
/// adapter tests timed the foreground CAS first and didn't see
/// the race; dual-adapter timing (tasks operations between two
/// memories ingests) gave the memories fold task enough head-
/// room to land first and the bug fired deterministically (the
/// node-side `npm test` was the canary). Post-fix the adapter
/// uses `fetch_add` to reserve `app_seq` atomically before
/// ingest — the watermark and the foreground writer no longer
/// fight over the same value.
#[tokio::test]
async fn test_regression_dual_model_alternating_ingests_do_not_produce_duplicate_app_seq() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build()
        .await
        .unwrap();

    db.tasks().create(1, "task", 100).unwrap();
    db.memories()
        .store(1, "mem", vec!["x".to_string()], "alice", 100)
        .unwrap();
    let t_seq = db.tasks().complete(1, 150).unwrap();
    // Pre-fix this returned `Err(...concurrent ingest_typed produced
    // duplicate app_seq=1...)` because the memories fold task had
    // advanced `memories.app_seq` to 2 between the foreground
    // load (1) and the foreground CAS, leaving the load value
    // stale.
    let m_seq = db
        .memories()
        .pin(1, 150)
        .expect("dual-model ingest must not surface phantom-duplicate app_seq");

    db.tasks().wait_for_seq(t_seq).await.unwrap();
    db.memories().wait_for_seq(m_seq).await.unwrap();

    assert_eq!(db.tasks().count(), 1);
    assert_eq!(db.memories().count(), 1);
}

/// Companion regression: concurrent `tasks.create` from N
/// threads on a SINGLE adapter must yield N distinct `app_seq`
/// values and surface no errors. The post-fix `fetch_add` is
/// the load-bearing primitive — it atomically reserves the next
/// `app_seq` per call, so threads can't race onto the same
/// number.
///
/// Pre-fix the load + ingest + CAS shape would have had at
/// least one losing thread on every contended call (CAS retry
/// wasn't even wired — the loser surfaced
/// `concurrent ingest_typed produced duplicate app_seq` and the
/// caller had to "rebuild adapter from snapshot to reconcile").
/// `fetch_add` makes contention free: every caller gets a
/// monotonically-distinct slot, no retry, no error.
#[tokio::test]
async fn test_regression_concurrent_ingest_into_single_tasks_adapter_succeeds() {
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::thread;

    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .build()
        .await
        .unwrap();
    let db = Arc::new(db);

    const N: u64 = 32;
    let barrier = Arc::new(Barrier::new(N as usize));
    let mut handles = Vec::with_capacity(N as usize);
    for i in 1..=N {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            // Each thread creates a distinct task id. Every call
            // MUST succeed — pre-fix the load+CAS race would have
            // produced phantom-duplicate errors for the losing
            // threads under contention.
            db.tasks()
                .create(i, "t", 100 * i)
                .expect("concurrent create on a single adapter must not error")
        }));
    }
    let seqs: Vec<u64> = handles
        .into_iter()
        .map(|h| h.join().expect("thread panicked"))
        .collect();
    assert_eq!(seqs.len(), N as usize, "every thread must have returned");

    // Wait for the fold to apply every event so `count()` is
    // authoritative.
    let max_seq = *seqs.iter().max().unwrap();
    db.tasks().wait_for_seq(max_seq).await.unwrap();

    assert_eq!(
        db.tasks().count(),
        N as usize,
        "every concurrently-created task must have landed in state — \
         a missing one would mean two threads stamped the same app_seq \
         and the second overwrote the first (pre-fix CAS-race regression)",
    );
}

/// Stress companion to the alternating-ingests regression: a
/// tighter loop hammers both adapters under one NetDb to maximize
/// the WatermarkingFold-vs-foreground race window. Pre-fix this
/// would surface `concurrent ingest_typed produced duplicate
/// app_seq` on at least one of the iterations on most runs.
#[tokio::test]
async fn test_regression_dual_model_stress_no_phantom_duplicate_app_seq() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build()
        .await
        .unwrap();

    const N: u64 = 50;
    for i in 1..=N {
        db.tasks().create(i, "t", 100 * i).unwrap();
        db.memories()
            .store(i, "m", vec!["t".to_string()], "alice", 100 * i)
            .unwrap();
    }
    // Final round-trip — each must succeed cleanly and the
    // watermarks must converge.
    let t_last = db.tasks().complete(N, 999).unwrap();
    let m_last = db.memories().pin(N, 999).unwrap();
    db.tasks().wait_for_seq(t_last).await.unwrap();
    db.memories().wait_for_seq(m_last).await.unwrap();

    assert_eq!(db.tasks().count(), N as usize);
    assert_eq!(db.memories().count(), N as usize);
}

#[tokio::test]
async fn test_regression_build_from_snapshot_error_path_is_clean() {
    // Regression: `build_from_snapshot` used to open the tasks
    // adapter, then open memories — if memories failed (e.g. corrupt
    // snapshot bytes), the tasks adapter's fold task would outlive
    // the failed build as an orphan. The runtime fix closes the
    // first adapter before propagating the error (see
    // [`NetDbBuilder::build_from_snapshot`] for the code-level
    // guarantee).
    //
    // This test exercises the error path — corrupt memories bytes
    // must surface as `Err` and a fresh NetDb built afterward must
    // ingest cleanly. It does NOT directly observe the closed
    // first-adapter's fold task on the failing Redex, because
    // `build_from_snapshot` consumes the Redex by value and drops
    // it on the error path — without an `Arc`-backed `Redex`
    // handle the failed manager is unreachable from outside. The
    // atomicity guarantee itself is kept honest by the six-line
    // close-on-error block in the builder; this test protects
    // against regressions in the observable surface only.
    let redex = Redex::new();

    let corrupt_bundle = NetDbSnapshot {
        tasks: None,
        memories: Some((vec![0xFFu8; 32], Some(0))),
    };

    let first = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build_from_snapshot(&corrupt_bundle)
        .await;
    assert!(
        first.is_err(),
        "corrupt memories snapshot must cause build to fail"
    );

    let redex2 = Redex::new();
    let db = NetDb::builder(redex2)
        .origin(ORIGIN)
        .with_tasks()
        .with_memories()
        .build()
        .await
        .unwrap();
    assert!(db.try_tasks().is_some());
    assert!(db.try_memories().is_some());
    // Smoke ingest to prove the fresh handle is functional after a
    // prior failed build in the same test scope.
    let seq = db.tasks().create(1, "t", 100).unwrap();
    db.tasks().wait_for_seq(seq).await.unwrap();
    assert_eq!(db.tasks().state().read().len(), 1);
    db.close().unwrap();
}
