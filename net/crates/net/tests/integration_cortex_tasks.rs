//! Integration tests for the CortEX tasks model.
//!
//! Covers the typed `TasksAdapter` surface end-to-end: CRUD through
//! the adapter, queries over materialized state, unknown-id no-ops,
//! multi-producer origin_hash separation, replay after close, and
//! durability with `redex-disk`.

#![cfg(feature = "cortex")]

use futures::StreamExt;
use net::adapter::net::channel::ChannelName;
use net::adapter::net::cortex::tasks::{OrderBy, TaskStatus, TasksAdapter, TASKS_CHANNEL};
use net::adapter::net::cortex::{compute_checksum, EventMeta, EVENT_META_SIZE};
use net::adapter::net::redex::Redex;
#[cfg(feature = "redex-disk")]
use net::adapter::net::redex::RedexFileConfig;

const ORIGIN: u64 = 0xABCD_EF01;

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

#[tokio::test]
async fn test_full_task_lifecycle() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    let t0 = now_ns();
    let _ = tasks.create(1, "write docs", t0).unwrap();
    let _ = tasks.create(2, "ship adapter", t0 + 1).unwrap();
    let _ = tasks.rename(1, "write better docs", t0 + 2).unwrap();
    let seq = tasks.complete(2, t0 + 3).unwrap();
    tasks.wait_for_seq(seq).await;

    let state = tasks.state();
    let guard = state.read();
    assert_eq!(guard.len(), 2);

    let t1 = guard.get(1).unwrap();
    assert_eq!(t1.title, "write better docs");
    assert_eq!(t1.status, TaskStatus::Pending);
    assert_eq!(t1.created_ns, t0);
    assert_eq!(t1.updated_ns, t0 + 2);

    let t2 = guard.get(2).unwrap();
    assert_eq!(t2.title, "ship adapter");
    assert_eq!(t2.status, TaskStatus::Completed);
    assert_eq!(t2.updated_ns, t0 + 3);

    assert_eq!(guard.pending().count(), 1);
    assert_eq!(guard.completed().count(), 1);
}

#[tokio::test]
async fn test_delete_removes_task() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    tasks.create(1, "temp", 100).unwrap();
    let seq = tasks.delete(1).unwrap();
    tasks.wait_for_seq(seq).await;

    let state = tasks.state();
    let guard = state.read();
    assert!(guard.is_empty());
    assert!(guard.get(1).is_none());
}

#[tokio::test]
async fn test_rename_on_unknown_id_is_noop() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    // Rename before create — fold silently drops (log is the truth).
    let seq = tasks.rename(42, "ghost", 100).unwrap();
    tasks.wait_for_seq(seq).await;

    let state = tasks.state();
    let guard = state.read();
    assert!(guard.is_empty());
}

#[tokio::test]
async fn test_complete_on_unknown_id_is_noop() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    let seq = tasks.complete(99, 100).unwrap();
    tasks.wait_for_seq(seq).await;

    let state = tasks.state();
    let guard = state.read();
    assert!(guard.is_empty());
}

#[tokio::test]
async fn test_replay_after_close_reconstructs_state() {
    // Open → drive CRUD → close → reopen fresh, state replays from log.
    let redex = Redex::new();

    {
        let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
        tasks.create(1, "a", 100).unwrap();
        tasks.create(2, "b", 101).unwrap();
        tasks.complete(1, 102).unwrap();
        let seq = tasks.rename(2, "b-renamed", 103).unwrap();
        tasks.wait_for_seq(seq).await;
        tasks.close().unwrap();
    }

    // Fresh handle; the Redex manager still owns the file (close on
    // the adapter doesn't drop the file), so reopen replays.
    let tasks2 = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    // 4 events were appended → wait for fold to catch up.
    tasks2.wait_for_seq(3).await;

    let state = tasks2.state();
    let guard = state.read();
    assert_eq!(guard.len(), 2);
    assert_eq!(guard.get(1).unwrap().status, TaskStatus::Completed);
    assert_eq!(guard.get(2).unwrap().title, "b-renamed");
    assert_eq!(guard.get(2).unwrap().status, TaskStatus::Pending);
}

#[tokio::test]
async fn test_multi_producer_same_file_different_origins() {
    // Two TasksAdapters against the same RedEX channel, each with its
    // own origin_hash and its own app_seq counter. Both see the same
    // materialized state because they share the underlying file.
    let redex = Redex::new();

    let a = TasksAdapter::open(&redex, 0x0000_0001).await.unwrap();
    let b = TasksAdapter::open(&redex, 0x0000_0002).await.unwrap();

    a.create(1, "from-a", 100).unwrap();
    let seq = b.create(2, "from-b", 101).unwrap();
    a.wait_for_seq(seq).await;
    b.wait_for_seq(seq).await;

    let state_a = a.state();
    let state_b = b.state();
    let ga = state_a.read();
    let gb = state_b.read();
    assert_eq!(ga.len(), 2);
    assert_eq!(gb.len(), 2);
}

#[tokio::test]
async fn test_pending_and_completed_queries() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    for i in 1..=10u64 {
        tasks.create(i, format!("task-{}", i), 100 + i).unwrap();
    }
    // Complete the even ids.
    for i in (2..=10u64).step_by(2) {
        tasks.complete(i, 200 + i).unwrap();
    }
    let last = tasks.complete(10, 9999).unwrap(); // idempotent-ish; refreshes updated_ns
    tasks.wait_for_seq(last).await;

    let state = tasks.state();
    let guard = state.read();
    assert_eq!(guard.len(), 10);

    let mut pending_ids: Vec<_> = guard.pending().map(|t| t.id).collect();
    pending_ids.sort();
    assert_eq!(pending_ids, vec![1, 3, 5, 7, 9]);

    let mut completed_ids: Vec<_> = guard.completed().map(|t| t.id).collect();
    completed_ids.sort();
    assert_eq!(completed_ids, vec![2, 4, 6, 8, 10]);
}

#[tokio::test]
async fn test_query_through_live_adapter() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    // Build a mixed corpus.
    for (id, title, now) in [
        (1u64, "alpha", 1000u64),
        (2, "beta", 2000),
        (3, "gamma", 3000),
        (4, "delta", 4000),
        (5, "epsilon", 5000),
    ] {
        tasks.create(id, title, now).unwrap();
    }
    tasks.complete(2, 2500).unwrap();
    tasks.complete(4, 4500).unwrap();
    let last = tasks.rename(5, "EPSILON", 5500).unwrap();
    tasks.wait_for_seq(last).await;

    let state = tasks.state();
    let guard = state.read();

    // Pending only → ids 1, 3, 5.
    let mut pending_ids: Vec<_> = guard
        .query()
        .where_status(TaskStatus::Pending)
        .collect()
        .iter()
        .map(|t| t.id)
        .collect();
    pending_ids.sort();
    assert_eq!(pending_ids, vec![1, 3, 5]);

    // Completed, ordered by updated desc, limit 1 → id 4 (updated_ns 4500).
    let top = guard
        .query()
        .where_status(TaskStatus::Completed)
        .order_by(OrderBy::UpdatedDesc)
        .first()
        .unwrap();
    assert_eq!(top.id, 4);

    // Title contains "psi" (case-insensitive) → id 5 (EPSILON).
    let match_title: Vec<_> = guard
        .query()
        .title_contains("PSI")
        .collect()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(match_title, vec![5]);

    // created_after(2500) AND pending → id 3, 5.
    let mut recent_pending: Vec<_> = guard
        .query()
        .created_after(2500)
        .where_status(TaskStatus::Pending)
        .collect()
        .iter()
        .map(|t| t.id)
        .collect();
    recent_pending.sort();
    assert_eq!(recent_pending, vec![3, 5]);

    // exists with no match.
    assert!(!guard.query().title_contains("does-not-exist").exists());
    assert!(guard.query().where_status(TaskStatus::Pending).exists());
}

/// Regression for BUG_AUDIT_2026_04_30_CORE.md #142: pre-fix
/// `created_after`/`created_before` were strict (`>` / `<`), so
/// an event with `created_ns == cutoff` was dropped by both
/// `created_after(cutoff)` AND `created_before(cutoff)`. A
/// caller paginating using "last sync ns" as cutoff would
/// silently lose events at the boundary. Two events written in
/// the same nanosecond (achievable on Windows with ~15ms wall-
/// clock granularity) would have one elided in any window
/// using either bound.
///
/// Post-fix the comparators are inclusive (`>=` / `<=`).
#[tokio::test]
async fn time_filter_cutoff_is_inclusive() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    // Three tasks at distinct timestamps; cutoff = exactly one
    // of them.
    tasks.create(1, "before", 1000).unwrap();
    tasks.create(2, "at-cutoff", 2000).unwrap();
    let last = tasks.create(3, "after", 3000).unwrap();
    tasks.wait_for_seq(last).await;

    let state = tasks.state();
    let guard = state.read();

    // created_after(2000) MUST include id 2 post-fix (pre-fix
    // dropped it because 2000 was not strictly > 2000).
    let mut after_2000: Vec<u64> = guard
        .query()
        .created_after(2000)
        .collect()
        .iter()
        .map(|t| t.id)
        .collect();
    after_2000.sort();
    assert_eq!(
        after_2000,
        vec![2, 3],
        "created_after must include the cutoff itself (inclusive)"
    );

    // created_before(2000) MUST include id 2 too — the cutoff
    // is in BOTH directions because both bounds are inclusive.
    let mut before_2000: Vec<u64> = guard
        .query()
        .created_before(2000)
        .collect()
        .iter()
        .map(|t| t.id)
        .collect();
    before_2000.sort();
    assert_eq!(
        before_2000,
        vec![1, 2],
        "created_before must include the cutoff itself (inclusive)"
    );
}

#[tokio::test]
async fn test_watch_initial_emission() {
    // A watcher opened against a non-empty state should yield the
    // current filter result on the first .next().await.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    // Pre-populate.
    tasks.create(1, "a", 100).unwrap();
    tasks.create(2, "b", 200).unwrap();
    let seq = tasks.complete(2, 250).unwrap();
    tasks.wait_for_seq(seq).await;

    let mut stream = Box::pin(
        tasks
            .watch()
            .where_status(TaskStatus::Pending)
            .order_by(OrderBy::IdAsc)
            .stream(),
    );

    let initial = stream.next().await.unwrap();
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].id, 1);
}

#[tokio::test]
async fn test_watch_emits_on_relevant_change() {
    // After the initial emission, the stream should yield again when
    // a new event changes the filter result.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    let mut stream = Box::pin(
        tasks
            .watch()
            .where_status(TaskStatus::Pending)
            .order_by(OrderBy::IdAsc)
            .stream(),
    );

    // Initial: empty.
    let initial = stream.next().await.unwrap();
    assert!(initial.is_empty());

    // Create one pending task → stream should yield [task-1].
    tasks.create(1, "first", 100).unwrap();
    let next = stream.next().await.unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].id, 1);

    // Create another pending → [1, 2].
    tasks.create(2, "second", 200).unwrap();
    let next = stream.next().await.unwrap();
    assert_eq!(next.len(), 2);
    assert_eq!(next[0].id, 1);
    assert_eq!(next[1].id, 2);

    // Complete task 1 → no longer matches Pending; result becomes [2].
    tasks.complete(1, 300).unwrap();
    let next = stream.next().await.unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].id, 2);
}

#[tokio::test]
async fn test_watch_dedupes_unchanged_results() {
    // Events that advance the log but don't change the filter result
    // must NOT cause a duplicate emission.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    // Seed one pending + one completed.
    tasks.create(1, "p", 100).unwrap();
    tasks.create(2, "c", 200).unwrap();
    let seq = tasks.complete(2, 250).unwrap();
    tasks.wait_for_seq(seq).await;

    let mut stream = Box::pin(tasks.watch().where_status(TaskStatus::Pending).stream());
    let initial = stream.next().await.unwrap();
    assert_eq!(initial.len(), 1);

    // Append events that DON'T change the pending filter:
    //   - complete on already-completed id 2 (refresh updated_ns, still completed)
    //   - rename on completed id 2 (still completed, filter unaffected)
    tasks.complete(2, 9999).unwrap();
    let seq = tasks.rename(2, "c-renamed", 9999).unwrap();
    tasks.wait_for_seq(seq).await;

    // No duplicate should have fired. Assert the next emission only
    // comes after we do something that DOES change Pending set.
    tasks.create(3, "p2", 300).unwrap();
    let next = stream.next().await.unwrap();
    let ids: Vec<_> = next.iter().map(|t| t.id).collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&3));
    assert_eq!(ids.len(), 2);
}

#[tokio::test]
async fn test_watch_multiple_subscribers_independent() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    let mut pending_stream = Box::pin(tasks.watch().where_status(TaskStatus::Pending).stream());
    let mut completed_stream = Box::pin(tasks.watch().where_status(TaskStatus::Completed).stream());

    // Both get an empty initial emission.
    assert!(pending_stream.next().await.unwrap().is_empty());
    assert!(completed_stream.next().await.unwrap().is_empty());

    // Create → pending gets [1], completed stays empty (no emit).
    tasks.create(1, "x", 100).unwrap();
    let p = pending_stream.next().await.unwrap();
    assert_eq!(p.len(), 1);

    // Complete → pending becomes empty, completed becomes [1].
    tasks.complete(1, 200).unwrap();
    let p = pending_stream.next().await.unwrap();
    assert!(p.is_empty());
    let c = completed_stream.next().await.unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].id, 1);
}

#[tokio::test]
async fn test_watch_with_limit_and_order() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    let mut stream = Box::pin(
        tasks
            .watch()
            .where_status(TaskStatus::Pending)
            .order_by(OrderBy::CreatedDesc)
            .limit(2)
            .stream(),
    );

    // Initial empty.
    assert!(stream.next().await.unwrap().is_empty());

    for id in 1..=5u64 {
        tasks.create(id, format!("t-{}", id), 100 * id).unwrap();
    }

    // Drain until the result stabilizes at [5, 4] (newest two).
    let mut last: Vec<_> = Vec::new();
    for _ in 0..5 {
        last = stream.next().await.unwrap();
        if last.len() == 2 && last[0].id == 5 && last[1].id == 4 {
            break;
        }
    }
    assert_eq!(last.len(), 2);
    assert_eq!(last[0].id, 5);
    assert_eq!(last[1].id, 4);
}

#[tokio::test]
async fn test_regression_open_from_snapshot_rejects_u64_max_last_seq() {
    // Regression: `open_from_snapshot` used to compute `last_seq + 1`
    // unchecked. A corrupted or malicious snapshot with
    // `last_seq = u64::MAX` would panic in debug, wraparound to 0 in
    // release, and silently resume tailing from seq 0 — replaying
    // the entire log as "new". The fix uses `checked_add` and returns
    // `CortexAdapterError::Redex(Encode)` on overflow.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    let (state_bytes, _) = tasks.snapshot().unwrap();
    tasks.close().unwrap();

    // Fresh Redex to avoid channel re-use interference.
    let redex2 = Redex::new();
    let result =
        TasksAdapter::open_from_snapshot(&redex2, ORIGIN, &state_bytes, Some(u64::MAX)).await;
    assert!(result.is_err(), "u64::MAX last_seq must be rejected");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("u64::MAX"),
        "error should mention u64::MAX overflow; got: {}",
        msg
    );
}

#[tokio::test]
async fn test_regression_fold_rejects_checksum_mismatch() {
    // Regression (originally): neither `TasksFold` nor `MemoriesFold`
    // verified `EventMeta::checksum` against the payload tail.
    //
    // Updated 2026-04-30: both folds now stamp
    // `RedexError::Decode` on checksum mismatch (instead of
    // `Encode`), and the cortex adapter's `Stop` policy treats
    // `Decode` as skip-and-continue. The original test asserted
    // the fold halted on checksum mismatch; that was a DoS vector
    // (one bad event wedges a multi-tenant cortex). The new
    // contract: the bad event is logged + skipped, fold_errors
    // increments by one, and state is NOT poisoned.
    use bytes::Bytes;
    use net::adapter::net::cortex::tasks::{TasksFold, TasksState};
    use net::adapter::net::cortex::{
        CortexAdapter, CortexAdapterConfig, EventEnvelope, FoldErrorPolicy, StartPosition,
    };

    let redex = Redex::new();
    let cfg = CortexAdapterConfig {
        start: StartPosition::FromBeginning,
        on_fold_error: FoldErrorPolicy::Stop,
    };
    let adapter = CortexAdapter::<TasksState>::open(
        &redex,
        &ChannelName::new(TASKS_CHANNEL).unwrap(),
        Default::default(),
        cfg,
        TasksFold,
        TasksState::new(),
    )
    .unwrap();

    // Stamp an EventMeta with a deliberately-wrong checksum.
    let tail = b"any bytes would have matched some xxh3 except this one".to_vec();
    let wrong_checksum = compute_checksum(&tail).wrapping_add(1);
    let wrong_meta = EventMeta::new(
        0x01, // DISPATCH_TASK_CREATED
        0,
        ORIGIN,
        0,
        wrong_checksum,
    );
    let seq = adapter
        .ingest(EventEnvelope::new(wrong_meta, Bytes::from(tail)))
        .unwrap();

    adapter.wait_for_seq(seq).await;

    // Post-#141: the fold task is STILL running — Decode-class
    // errors are recoverable per-event failures, not stream-fatal.
    assert!(
        adapter.is_running(),
        "fold task must continue after checksum mismatch — \
         decode errors are recoverable under Stop policy"
    );
    assert_eq!(
        adapter.fold_errors(),
        1,
        "the bad event must be counted in fold_errors"
    );

    // State must NOT contain the poisoned task — the fold rejected
    // the event before mutating state.
    let state = adapter.state();
    let guard = state.read();
    assert!(
        guard.get(1).is_none(),
        "checksum-mismatched event must NOT have folded into state"
    );
}

#[tokio::test]
async fn test_regression_open_from_snapshot_bumps_app_seq_past_replayed_events() {
    // Regression: `open_from_snapshot` restored `app_seq` from the
    // snapshot payload without accounting for events ingested AFTER
    // the snapshot was taken but before the adapter closed. Those
    // events have already-assigned `seq_or_ts` values that will be
    // replayed by the fold task on restore — if `app_seq` is just
    // set to `payload.app_seq`, the next ingest re-emits a
    // `seq_or_ts` that a replayed event already used.
    //
    // The fix scans the replay range `(last_seq, next_seq)` for
    // events from our origin and bumps `app_seq` past the highest
    // matching `seq_or_ts` before installing it.
    //
    // Setup: ingest 2 events, snapshot, ingest 2 more events to
    // the SAME file (simulating periodic-snapshot while work
    // continues), then restore from the snapshot on the same
    // Redex. The restored adapter must see `seq_or_ts = 4` (not 2)
    // on its first new ingest.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    // Events 0, 1 — pre-snapshot.
    tasks.create(1, "a", 100).unwrap();
    let seq1 = tasks.create(2, "b", 200).unwrap();
    tasks.wait_for_seq(seq1).await;
    let (state_bytes, last_seq) = tasks.snapshot().unwrap();
    assert_eq!(last_seq, Some(1), "snapshot must capture seqs 0..=1");

    // Events 2, 3 — post-snapshot (still folding on the live adapter).
    tasks.create(3, "c", 300).unwrap();
    let seq3 = tasks.create(4, "d", 400).unwrap();
    tasks.wait_for_seq(seq3).await;
    tasks.close().unwrap();

    // Restore on the SAME Redex so the file already contains seqs
    // 2, 3 in the replay range.
    let restored = TasksAdapter::open_from_snapshot(&redex, ORIGIN, &state_bytes, last_seq)
        .await
        .unwrap();

    // The restored adapter should fold in the replay range (seqs
    // 2, 3) and then accept a new ingest. The new ingest's
    // `seq_or_ts` must be 4 (continuing past the replayed events)
    // NOT 2 (which would collide with the replayed event at seq 2).
    let new_seq = restored.create(5, "e", 500).unwrap();
    restored.wait_for_seq(new_seq).await;

    // Read the event we just ingested from the file and decode its
    // EventMeta to inspect `seq_or_ts`.
    let file = redex
        .open_file(
            &ChannelName::new(TASKS_CHANNEL).unwrap(),
            Default::default(),
        )
        .unwrap();
    let events = file.read_range(new_seq, new_seq + 1);
    assert_eq!(events.len(), 1);
    let meta = EventMeta::from_bytes(&events[0].payload[..EVENT_META_SIZE]).unwrap();
    assert_eq!(
        meta.seq_or_ts, 4,
        "post-restore ingest must continue past replayed events' seq_or_ts (got {}, expected 4)",
        meta.seq_or_ts
    );
}

#[tokio::test]
async fn test_regression_snapshot_restore_preserves_app_seq_monotonicity() {
    // Regression: `TasksAdapter::open_from_snapshot[_with_config]`
    // used to recreate the adapter with `app_seq: AtomicU64::new(0)`,
    // so post-restore events re-emitted `EventMeta::seq_or_ts` values
    // that pre-snapshot events already used. The fix wraps `app_seq`
    // into the snapshot payload and restores it so per-origin
    // monotonicity is preserved.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    // Ingest 3 events — their EventMeta::seq_or_ts values run 0, 1, 2.
    tasks.create(1, "a", 100).unwrap();
    tasks.create(2, "b", 200).unwrap();
    let seq = tasks.create(3, "c", 300).unwrap();
    tasks.wait_for_seq(seq).await;

    let (state_bytes, last_seq) = tasks.snapshot().unwrap();
    tasks.close().unwrap();

    // Restore on a FRESH Redex — same origin.
    let redex2 = Redex::new();
    let tasks2 = TasksAdapter::open_from_snapshot(&redex2, ORIGIN, &state_bytes, last_seq)
        .await
        .unwrap();

    // Next ingest on the restored adapter.
    let new_seq = tasks2.create(4, "d", 400).unwrap();
    tasks2.wait_for_seq(new_seq).await;

    // Read the raw RedEX event (seq 0 is the first ingest on the fresh
    // redex2 file — which is this post-restore create). Decode its
    // EventMeta.seq_or_ts: it MUST be 3 (continuing from pre-snapshot
    // counter), NOT 0 (which would be a duplicate of the very first
    // pre-snapshot event's seq_or_ts).
    let file = redex2
        .open_file(
            &ChannelName::new(TASKS_CHANNEL).unwrap(),
            Default::default(),
        )
        .unwrap();
    let events = file.read_range(0, 1);
    assert_eq!(events.len(), 1, "first post-restore event must be present");
    let meta = EventMeta::from_bytes(&events[0].payload[..EVENT_META_SIZE]).unwrap();
    assert_eq!(
        meta.seq_or_ts, 3,
        "post-restore app_seq must continue from pre-snapshot value, not reset to 0"
    );
}

#[tokio::test]
async fn test_open_returns_with_state_already_caught_up() {
    // The fix changed `TasksAdapter::open[_with_config]` to await
    // the inner fold task's catch-up before returning. After this
    // change a fresh adapter opened against a non-empty Redex sees
    // the full state synchronously — no `wait_for_seq` required.
    //
    // Pre-fix the constructor returned immediately and the fold task
    // ran concurrently, so reading state right after `open` could
    // observe a partial replay. Pin the new "fully caught up" guarantee.
    let redex = Redex::new();
    {
        let a = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
        a.create(1, "first", 100).unwrap();
        a.create(2, "second", 200).unwrap();
        let seq = a.create(3, "third", 300).unwrap();
        a.wait_for_seq(seq).await;
        a.close().unwrap();
    }

    // Reopen and read state IMMEDIATELY — no wait_for_seq.
    let b = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    let state = b.state();
    let guard = state.read();
    assert_eq!(
        guard.len(),
        3,
        "post-open state must be fully caught up — saw {} tasks, expected 3",
        guard.len(),
    );
}

#[tokio::test]
async fn test_open_on_empty_redex_does_not_block() {
    // Edge case: `open` against a fresh empty Redex must not block
    // on `wait_for_seq` (file.next_seq() == 0 → no events to await).
    // Wrap in a tight tokio::time::timeout so a regression that
    // accidentally awaits an unreachable seq surfaces as a test
    // failure rather than a hung CI run.
    let redex = Redex::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        TasksAdapter::open(&redex, ORIGIN),
    )
    .await;
    assert!(
        matches!(result, Ok(Ok(_))),
        "open() on an empty Redex must complete promptly; got {result:?}",
    );
}

#[tokio::test]
async fn test_open_from_snapshot_with_empty_replay_tail_keeps_snapshot_app_seq() {
    // When a snapshot's `last_seq` already covers every event in the
    // file, the wrapper fold sees nothing during catch-up and the
    // snapshot's persisted `app_seq` survives unchanged. The first
    // post-restore ingest stamps `seq_or_ts = persisted_app_seq`.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    tasks.create(1, "a", 100).unwrap();
    tasks.create(2, "b", 200).unwrap();
    let seq = tasks.create(3, "c", 300).unwrap();
    tasks.wait_for_seq(seq).await;

    // Snapshot covers every event — replay tail will be empty.
    let (state_bytes, last_seq) = tasks.snapshot().unwrap();
    tasks.close().unwrap();

    // Restore on a fresh Redex so `next_seq == 0` post-restore.
    let redex2 = Redex::new();
    let restored = TasksAdapter::open_from_snapshot(&redex2, ORIGIN, &state_bytes, last_seq)
        .await
        .unwrap();

    // Persisted app_seq was 3 (three pre-snapshot ingests). The first
    // post-restore ingest must stamp seq_or_ts = 3.
    let new_seq = restored.create(4, "d", 400).unwrap();
    restored.wait_for_seq(new_seq).await;

    let file = redex2
        .open_file(
            &ChannelName::new(TASKS_CHANNEL).unwrap(),
            Default::default(),
        )
        .unwrap();
    let events = file.read_range(new_seq, new_seq + 1);
    let meta = EventMeta::from_bytes(&events[0].payload[..EVENT_META_SIZE]).unwrap();
    assert_eq!(
        meta.seq_or_ts, 3,
        "post-restore counter must continue from snapshot's persisted app_seq (got {}, expected 3)",
        meta.seq_or_ts,
    );
}

#[tokio::test]
async fn test_regression_open_advances_app_seq_past_existing_same_origin_events() {
    // Regression: pre-fix
    // `TasksAdapter::open` set `app_seq = AtomicU64::new(0)`
    // unconditionally, so reopening against a Redex (or persistent
    // file) that already had same-origin events caused the next
    // ingest to stamp `EventMeta::seq_or_ts = 0`, colliding with the
    // pre-existing event's `seq_or_ts = 0`. The piggyback fix wires
    // a `WatermarkingFold` wrapper around `TasksFold` that advances
    // `app_seq` via `fetch_max(seq_or_ts + 1)` as the fold task
    // replays existing events; the constructor awaits catch-up
    // before returning, so the first ingest after `open` is
    // guaranteed past every replayed value.
    let redex = Redex::new();

    // Seed via a first adapter (events get seq_or_ts 0, 1, 2).
    {
        let a = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
        a.create(1, "first", 100).unwrap();
        a.create(2, "second", 200).unwrap();
        let seq = a.create(3, "third", 300).unwrap();
        a.wait_for_seq(seq).await;
        a.close().unwrap();
    }

    // Reopen via plain `open` (NOT `open_from_snapshot` — that path
    // has its own coverage via the existing snapshot regression tests
    // above).
    let b = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    let new_seq = b.create(4, "fourth", 400).unwrap();
    b.wait_for_seq(new_seq).await;

    // Read the raw event. Its `seq_or_ts` must be 3 (continuing past
    // the replayed events), NOT 0 (which would duplicate the first
    // seeded event).
    let file = redex
        .open_file(
            &ChannelName::new(TASKS_CHANNEL).unwrap(),
            Default::default(),
        )
        .unwrap();
    let events = file.read_range(new_seq, new_seq + 1);
    assert_eq!(events.len(), 1);
    let meta = EventMeta::from_bytes(&events[0].payload[..EVENT_META_SIZE]).unwrap();
    assert_eq!(
        meta.seq_or_ts, 3,
        "first ingest after reopen-on-existing-log must continue past replayed events' \
         seq_or_ts (got {}, expected 3)",
        meta.seq_or_ts,
    );
}

#[tokio::test]
async fn test_regression_open_ignores_other_origins_when_advancing_app_seq() {
    // The `WatermarkingFold` wrapper installed by the fix
    // only advances `app_seq` for events whose `origin_hash` matches
    // the adapter's. An adapter for origin A reopening against a
    // file populated by origin B should still see `app_seq = 0` for
    // its own first ingest — otherwise two cross-origin daemons
    // sharing a channel would interleave each other's counters and
    // every per-origin sequence space would collide.
    let redex = Redex::new();
    const ORIGIN_A: u64 = 0x0000_00AA;
    const ORIGIN_B: u64 = 0x0000_00BB;

    // Origin B writes some events.
    {
        let b = TasksAdapter::open(&redex, ORIGIN_B).await.unwrap();
        b.create(10, "b1", 100).unwrap();
        b.create(11, "b2", 200).unwrap();
        let seq = b.create(12, "b3", 300).unwrap();
        b.wait_for_seq(seq).await;
        b.close().unwrap();
    }

    // Origin A opens. Its watermarking fold sees three replayed
    // events but ignores them all (origin_hash mismatch), so
    // `app_seq` stays at 0.
    let a = TasksAdapter::open(&redex, ORIGIN_A).await.unwrap();
    let new_seq = a.create(20, "a1", 400).unwrap();
    a.wait_for_seq(new_seq).await;

    let file = redex
        .open_file(
            &ChannelName::new(TASKS_CHANNEL).unwrap(),
            Default::default(),
        )
        .unwrap();
    let events = file.read_range(new_seq, new_seq + 1);
    let meta = EventMeta::from_bytes(&events[0].payload[..EVENT_META_SIZE]).unwrap();
    assert_eq!(
        meta.seq_or_ts, 0,
        "origin A's first ingest must not be polluted by origin B's seq_or_ts values \
         (got {}, expected 0)",
        meta.seq_or_ts,
    );
    assert_eq!(meta.origin_hash, ORIGIN_A);
}

#[tokio::test]
async fn test_regression_checksum_is_computed_not_zero() {
    // Regression: `EventMeta::checksum` used to be hardcoded to 0 in
    // the tasks adapter's `ingest_typed`. The documented contract
    // (see `EventMeta` struct doc in meta.rs) is xxh3 truncation of
    // the payload tail. Verify the on-disk event's meta.checksum
    // matches `compute_checksum(tail)`.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    let seq = tasks.create(42, "distinctive title", 12345).unwrap();
    tasks.wait_for_seq(seq).await;

    // Read the raw RedEX event for this append.
    let file = redex
        .open_file(
            &ChannelName::new(TASKS_CHANNEL).unwrap(),
            Default::default(),
        )
        .unwrap();
    let events = file.read_range(0, 1);
    assert_eq!(events.len(), 1, "tasks channel should have one event");
    let payload = &events[0].payload;
    let meta = EventMeta::from_bytes(&payload[..EVENT_META_SIZE]).expect("decode meta");
    let tail = &payload[EVENT_META_SIZE..];

    assert_ne!(meta.checksum, 0, "checksum must not be hardcoded to 0");
    assert_eq!(
        meta.checksum,
        compute_checksum(tail),
        "meta.checksum must match xxh3 truncation of the payload tail"
    );
}

#[tokio::test]
async fn test_regression_watch_without_order_by_is_stable() {
    // Regression for the HashMap-iteration-order false-positive in
    // the watcher's Vec-equality dedup. Before the fix, a watcher
    // opened without `order_by` could emit Vecs whose element order
    // depended on HashMap rehash timing, so a mutation that didn't
    // change the filter output could still trigger a spurious
    // re-emission (element reorder breaks Vec equality). The fix
    // defaults the watcher's `order_by` to `IdAsc` when unset, so
    // the emitted Vec is deterministic and dedup is correct.
    //
    // Seed enough pending tasks that hash iteration order is
    // demonstrably non-ascending, then assert the watch output is
    // IdAsc.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    const N: u64 = 64;
    let mut last = 0;
    for id in 1..=N {
        last = tasks.create(id, format!("t-{}", id), id * 100).unwrap();
    }
    tasks.wait_for_seq(last).await;

    // Open watch *without* order_by. The fix makes this default to
    // IdAsc under the hood.
    let mut stream = Box::pin(tasks.watch().where_status(TaskStatus::Pending).stream());
    let initial = stream.next().await.unwrap();
    assert_eq!(initial.len(), N as usize);
    let ids: Vec<u64> = initial.iter().map(|t| t.id).collect();
    let sorted: Vec<u64> = (1..=N).collect();
    assert_eq!(ids, sorted, "watcher without order_by must emit IdAsc");
}

#[tokio::test]
async fn test_snapshot_and_restore_skips_replay() {
    // Open, do CRUD, snapshot, close. Reopen from snapshot on the
    // SAME redex — state matches without the fold replaying events
    // 0..=last_seq (the adapter tails at FromSeq(last_seq+1)).
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    tasks.create(1, "alpha", 100).unwrap();
    tasks.create(2, "beta", 200).unwrap();
    tasks.complete(1, 150).unwrap();
    let seq = tasks.rename(2, "beta-v2", 250).unwrap();
    tasks.wait_for_seq(seq).await;

    let (bytes, last_seq) = tasks.snapshot().unwrap();
    assert_eq!(last_seq, Some(3)); // 4 events → seq 0..=3
    tasks.close().unwrap();

    // Reopen on the same redex — the file still holds seqs 0..=3,
    // but the restored adapter's fold starts at seq 4 (last_seq+1),
    // so those old events are NOT replayed. State comes from bytes.
    let tasks2 = TasksAdapter::open_from_snapshot(&redex, ORIGIN, &bytes, last_seq)
        .await
        .unwrap();

    {
        let state = tasks2.state();
        let guard = state.read();
        assert_eq!(guard.len(), 2);
        let t1 = guard.get(1).unwrap();
        assert_eq!(t1.status, TaskStatus::Completed);
        let t2 = guard.get(2).unwrap();
        assert_eq!(t2.title, "beta-v2");
        assert_eq!(t2.status, TaskStatus::Pending);
    } // guard dropped here before the await below

    // New ingest flows through normally. The underlying file's
    // next_seq is 4 (persisted across close), so this create
    // appends at seq 4, which the fold task picks up since it
    // tails FromSeq(4).
    let seq = tasks2.create(3, "gamma", 300).unwrap();
    assert_eq!(seq, 4);
    tasks2.wait_for_seq(seq).await;
    assert_eq!(tasks2.state().read().len(), 3);
}

#[tokio::test]
async fn test_snapshot_empty_state_has_no_last_seq() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    let (bytes, last_seq) = tasks.snapshot().unwrap();
    assert_eq!(last_seq, None);
    assert!(!bytes.is_empty()); // even empty state serializes to >0 bytes.
}

#[tokio::test]
async fn test_ingest_after_close_errors() {
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    tasks.create(1, "a", 100).unwrap();
    tasks.close().unwrap();
    assert!(tasks.create(2, "b", 101).is_err());
}

#[cfg(feature = "redex-disk")]
#[tokio::test]
async fn test_persistent_tasks_recover_across_processes() {
    use std::path::PathBuf;

    let mut base: PathBuf = std::env::temp_dir();
    base.push(format!(
        "cortex_tasks_persist_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();

    let cfg = RedexFileConfig::default().with_persistent(true);

    {
        let redex = Redex::new().with_persistent_dir(&base);
        let tasks = TasksAdapter::open_with_config(&redex, ORIGIN, cfg)
            .await
            .unwrap();
        tasks.create(1, "durable", 100).unwrap();
        tasks.create(2, "also durable", 101).unwrap();
        let seq = tasks.complete(1, 102).unwrap();
        tasks.wait_for_seq(seq).await;
        tasks.close().unwrap();
    }

    // Fresh Redex manager, same base_dir — state replays from disk.
    let redex2 = Redex::new().with_persistent_dir(&base);
    let tasks2 = TasksAdapter::open_with_config(&redex2, ORIGIN, cfg)
        .await
        .unwrap();
    tasks2.wait_for_seq(2).await;

    let state = tasks2.state();
    let guard = state.read();
    assert_eq!(guard.len(), 2);
    assert_eq!(guard.get(1).unwrap().status, TaskStatus::Completed);
    assert_eq!(guard.get(2).unwrap().status, TaskStatus::Pending);

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn test_snapshot_and_watch_snapshot_reflects_current_state() {
    // `snapshot_and_watch` returns (initial, delta_stream). This test
    // covers the cheap, deterministic half: the snapshot. The delta
    // half is covered by the existing `test_watch_*` suite that
    // exercises the underlying `watch().stream()` shape — since
    // `snapshot_and_watch` is `(initial, watcher.stream().skip(1))`,
    // any regression in delta behavior is caught upstream.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    // Seed one pending + one completed; snapshot-for-pending must
    // reflect both the positive and negative filter evaluation.
    tasks.create(1, "p1", 100).unwrap();
    tasks.create(2, "c1", 200).unwrap();
    let seq = tasks.complete(2, 250).unwrap();
    tasks.wait_for_seq(seq).await;

    let watcher = tasks.watch().where_status(TaskStatus::Pending);
    let (snapshot, _stream) = tasks.snapshot_and_watch(watcher);
    let ids: Vec<_> = snapshot.iter().map(|t| t.id).collect();
    assert_eq!(
        ids,
        vec![1],
        "snapshot must reflect the filter evaluated against current state"
    );
}

#[tokio::test]
async fn test_regression_snapshot_and_watch_delivers_post_call_updates() {
    // Regression for the `skip(1)` drop-update bug: the watcher's
    // `stream()` computes its own initial emission from a second,
    // independent state read. If that read races with the snapshot
    // read above it in `snapshot_and_watch`, the two values diverge
    // — and a plain `skip(1)` would silently discard the divergent
    // emission, leaving the caller with a stale snapshot and no
    // pending deltas. The fix uses `skip_while(== snapshot)` so any
    // emission that differs from the returned snapshot is forwarded.
    //
    // This test covers the user-visible contract: any state change
    // after the call must eventually land on the stream, including
    // the case where the change races stream construction.
    let redex = Redex::new();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();
    let seq = tasks.create(1, "seed", 100).unwrap();
    tasks.wait_for_seq(seq).await;

    let watcher = tasks.watch();
    let (initial, mut stream) = tasks.snapshot_and_watch(watcher);
    let initial_ids: Vec<_> = initial.iter().map(|t| t.id).collect();
    assert_eq!(initial_ids, vec![1]);

    // Post-call mutation. Under both skip(1) and skip_while this
    // specific case works (the change arrives via the normal delta
    // path), but having it as a baseline guards against any future
    // over-eager filtering that also drops legitimate deltas.
    let seq = tasks.create(2, "post", 200).unwrap();
    tasks.wait_for_seq(seq).await;

    let observed = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .expect("stream must emit after mutation")
        .expect("stream must not end");
    let ids: Vec<_> = observed.iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![1, 2]);
    assert_ne!(observed, initial);
}

#[tokio::test]
async fn test_regression_snapshot_and_watch_forwards_divergent_stream_initial() {
    // Regression: the watcher's `stream()` reads state independently
    // of `snapshot_and_watch`'s own read. If between those two reads
    // the state has mutated — because the mutation was already queued
    // when the call began — the stream's internal initial will
    // differ from the snapshot returned to the caller. With the old
    // `skip(1)`, that divergent initial was dropped silently and the
    // caller's stream hung on an unchanging state. With
    // `skip_while(== snapshot)` the divergent initial is forwarded.
    //
    // Drive the divergence by mutating state concurrently across
    // many trials. The assertion is the functional contract: when
    // the snapshot reflects N tasks and the mutation adds one more,
    // the stream MUST deliver the N+1 state within a short window.
    for trial in 0..20 {
        let redex = Redex::new();
        let tasks = std::sync::Arc::new(TasksAdapter::open(&redex, ORIGIN).await.unwrap());
        let seq = tasks.create(1, "seed", 100).unwrap();
        tasks.wait_for_seq(seq).await;

        let tasks_m = tasks.clone();
        let mutator = tokio::spawn(async move {
            let seq = tasks_m.create(2, "race", 200).unwrap();
            tasks_m.wait_for_seq(seq).await;
        });

        let watcher = tasks.watch();
        let (initial, mut stream) = tasks.snapshot_and_watch(watcher);
        mutator.await.unwrap();

        // Skip trials where the mutation fully landed before the
        // snapshot read — there's no further change to deliver.
        if initial.len() == 2 {
            continue;
        }
        assert_eq!(
            initial.len(),
            1,
            "trial {}: snapshot should be [seed]",
            trial
        );

        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "trial {}: stream must deliver post-snapshot state within timeout",
                    trial
                )
            })
            .expect("stream must not end");
        assert_eq!(
            observed.len(),
            2,
            "trial {}: stream must deliver state with both tasks",
            trial
        );
    }
}
