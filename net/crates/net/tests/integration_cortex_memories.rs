//! Integration tests for the CortEX memories model.
//!
//! Covers the typed `MemoriesAdapter` surface end-to-end: full
//! lifecycle, tag-based queries through the live adapter, unknown-id
//! no-ops, replay after close, and coexistence with the tasks model
//! on the same Redex manager.

#![cfg(feature = "cortex")]

use futures::StreamExt;
use net::adapter::net::channel::ChannelName;
use net::adapter::net::cortex::memories::{MemoriesAdapter, OrderBy, MEMORIES_CHANNEL};
use net::adapter::net::cortex::{compute_checksum_with_meta, EventMeta, EVENT_META_SIZE};
use net::adapter::net::redex::Redex;

const ORIGIN: u64 = 0x0BAD_F00D;

#[tokio::test]
async fn test_full_memory_lifecycle() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    memories
        .store(
            1,
            "notes from the standup",
            vec!["work".into(), "notes".into()],
            "alice",
            100,
        )
        .unwrap();
    memories
        .store(
            2,
            "grocery list for the week",
            vec!["personal".into(), "todo".into()],
            "alice",
            200,
        )
        .unwrap();
    memories
        .retag(2, vec!["personal".into(), "shopping".into()], 250)
        .unwrap();
    memories.pin(1, 260).unwrap();
    let seq = memories.pin(2, 270).unwrap();
    memories.wait_for_seq(seq).await;

    let state = memories.state();
    let guard = state.read();
    assert_eq!(guard.len(), 2);

    let m1 = guard.get(1).unwrap();
    assert!(m1.pinned);
    assert_eq!(m1.tags, vec!["work".to_string(), "notes".to_string()]);
    assert_eq!(m1.created_ns, 100);
    assert_eq!(m1.updated_ns, 260);

    let m2 = guard.get(2).unwrap();
    assert!(m2.pinned);
    assert_eq!(
        m2.tags,
        vec!["personal".to_string(), "shopping".to_string()]
    );
    assert_eq!(m2.updated_ns, 270);
}

#[tokio::test]
async fn test_pin_and_unpin_toggle() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    memories
        .store(1, "toggle me", Vec::<String>::new(), "tester", 100)
        .unwrap();
    memories.pin(1, 110).unwrap();
    let seq = memories.pin(1, 120).unwrap();
    memories.wait_for_seq(seq).await;
    assert!(memories.state().read().get(1).unwrap().pinned);

    let seq = memories.unpin(1, 130).unwrap();
    memories.wait_for_seq(seq).await;
    assert!(!memories.state().read().get(1).unwrap().pinned);
}

#[tokio::test]
async fn test_delete_removes_memory() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    memories
        .store(1, "temp", Vec::<String>::new(), "alice", 100)
        .unwrap();
    let seq = memories.delete(1).unwrap();
    memories.wait_for_seq(seq).await;

    let state = memories.state();
    let guard = state.read();
    assert!(guard.is_empty());
}

#[tokio::test]
async fn test_retag_on_unknown_id_is_noop() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    let seq = memories.retag(999, vec!["ghost".into()], 100).unwrap();
    memories.wait_for_seq(seq).await;

    let state = memories.state();
    let guard = state.read();
    assert!(guard.is_empty());
}

#[tokio::test]
async fn test_tag_queries_through_live_adapter() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    memories
        .store(
            1,
            "morning standup",
            vec!["work".into(), "meetings".into()],
            "alice",
            100,
        )
        .unwrap();
    memories
        .store(
            2,
            "reading list",
            vec!["personal".into(), "reading".into()],
            "alice",
            200,
        )
        .unwrap();
    memories
        .store(
            3,
            "api design session",
            vec!["work".into(), "design".into()],
            "bob",
            300,
        )
        .unwrap();
    let seq = memories
        .store(
            4,
            "book recommendations",
            vec!["personal".into(), "reading".into(), "books".into()],
            "bob",
            400,
        )
        .unwrap();
    memories.wait_for_seq(seq).await;

    let state = memories.state();
    let guard = state.read();

    // where_tag("work") → 1, 3.
    let mut work_ids: Vec<_> = guard
        .query()
        .where_tag("work")
        .collect()
        .iter()
        .map(|m| m.id)
        .collect();
    work_ids.sort();
    assert_eq!(work_ids, vec![1, 3]);

    // where_any_tag({books, design}) → 3 (design), 4 (books).
    let mut any_ids: Vec<_> = guard
        .query()
        .where_any_tag(["books".into(), "design".into()])
        .collect()
        .iter()
        .map(|m| m.id)
        .collect();
    any_ids.sort();
    assert_eq!(any_ids, vec![3, 4]);

    // where_all_tags({personal, reading}) → 2, 4.
    let mut all_ids: Vec<_> = guard
        .query()
        .where_all_tags(["personal".into(), "reading".into()])
        .collect()
        .iter()
        .map(|m| m.id)
        .collect();
    all_ids.sort();
    assert_eq!(all_ids, vec![2, 4]);

    // where_source("bob") → 3, 4.
    let mut bob_ids: Vec<_> = guard
        .query()
        .where_source("bob")
        .collect()
        .iter()
        .map(|m| m.id)
        .collect();
    bob_ids.sort();
    assert_eq!(bob_ids, vec![3, 4]);

    // content_contains("api") → 3.
    let ids: Vec<_> = guard
        .query()
        .content_contains("API")
        .collect()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, vec![3]);

    // Composed: where_source=bob AND where_tag=reading → only 4.
    let ids: Vec<_> = guard
        .query()
        .where_source("bob")
        .where_tag("reading")
        .collect()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, vec![4]);

    // Order by CreatedDesc, limit 2 → 4, 3.
    let ids: Vec<_> = guard
        .query()
        .order_by(OrderBy::CreatedDesc)
        .limit(2)
        .collect()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, vec![4, 3]);
}

#[tokio::test]
async fn test_replay_after_close_reconstructs_state() {
    let redex = Redex::new();
    {
        let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
        memories
            .store(1, "alpha", vec!["x".into()], "alice", 100)
            .unwrap();
        memories.pin(1, 110).unwrap();
        memories
            .store(2, "beta", vec!["y".into()], "alice", 200)
            .unwrap();
        let seq = memories
            .retag(2, vec!["y".into(), "z".into()], 210)
            .unwrap();
        memories.wait_for_seq(seq).await;
        memories.close().unwrap();
    }

    // Fresh adapter, same file — state replays from log.
    let memories2 = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
    memories2.wait_for_seq(3).await;

    let state = memories2.state();
    let guard = state.read();
    assert_eq!(guard.len(), 2);

    let m1 = guard.get(1).unwrap();
    assert!(m1.pinned);
    assert_eq!(m1.tags, vec!["x".to_string()]);

    let m2 = guard.get(2).unwrap();
    assert!(!m2.pinned);
    assert_eq!(m2.tags, vec!["y".to_string(), "z".to_string()]);
}

#[tokio::test]
async fn test_watch_initial_emission() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    // Pre-populate with one pinned + one unpinned.
    memories
        .store(1, "pinned content", vec!["urgent".into()], "alice", 100)
        .unwrap();
    memories
        .store(2, "other content", vec!["later".into()], "alice", 200)
        .unwrap();
    let seq = memories.pin(1, 210).unwrap();
    memories.wait_for_seq(seq).await;

    let mut stream = Box::pin(
        memories
            .watch()
            .where_pinned(true)
            .order_by(OrderBy::IdAsc)
            .stream(),
    );

    let initial = stream.next().await.unwrap();
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].id, 1);
}

#[tokio::test]
async fn test_watch_emits_on_tag_change() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    // Watch memories tagged "urgent".
    let mut stream = Box::pin(
        memories
            .watch()
            .where_tag("urgent")
            .order_by(OrderBy::IdAsc)
            .stream(),
    );
    let initial = stream.next().await.unwrap();
    assert!(initial.is_empty());

    // Store a memory without the "urgent" tag → no emission expected
    // (the next tagged store should produce the next emission, not
    // this irrelevant one).
    memories
        .store(1, "routine", vec!["later".into()], "alice", 100)
        .unwrap();

    // Store a matching memory → emission [1] where id=2 and has tag.
    memories
        .store(
            2,
            "fire in the datacenter",
            vec!["urgent".into()],
            "alice",
            200,
        )
        .unwrap();
    let next = stream.next().await.unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].id, 2);

    // Retag #1 to include "urgent" → now it matches; emission [1, 2].
    memories
        .retag(1, vec!["later".into(), "urgent".into()], 300)
        .unwrap();
    let next = stream.next().await.unwrap();
    let mut ids: Vec<_> = next.iter().map(|m| m.id).collect();
    ids.sort();
    assert_eq!(ids, vec![1, 2]);

    // Retag #2 to drop "urgent" → drops out of filter; emission [1].
    memories.retag(2, vec!["resolved".into()], 400).unwrap();
    let next = stream.next().await.unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].id, 1);
}

#[tokio::test]
async fn test_watch_dedupes_unchanged_results() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    // Seed one memory tagged "work" + one tagged "home".
    memories
        .store(1, "work note", vec!["work".into()], "alice", 100)
        .unwrap();
    memories
        .store(2, "home note", vec!["home".into()], "alice", 200)
        .unwrap();
    let seq = memories.pin(2, 210).unwrap();
    memories.wait_for_seq(seq).await;

    let mut stream = Box::pin(memories.watch().where_tag("work").stream());
    let initial = stream.next().await.unwrap();
    assert_eq!(initial.len(), 1);

    // Changes that DON'T affect the "work"-tagged set:
    //   - pin/unpin the home memory
    //   - retag the home memory (still tagged home)
    memories.unpin(2, 300).unwrap();
    memories.pin(2, 310).unwrap();
    memories
        .retag(2, vec!["home".into(), "archive".into()], 320)
        .unwrap();

    // Now make a change that DOES affect the "work" set: store a new
    // work-tagged memory.
    memories
        .store(3, "another work note", vec!["work".into()], "alice", 400)
        .unwrap();

    let next = stream.next().await.unwrap();
    let mut ids: Vec<_> = next.iter().map(|m| m.id).collect();
    ids.sort();
    assert_eq!(ids, vec![1, 3]);
}

#[tokio::test]
async fn test_watch_multiple_subscribers_independent() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    let mut pinned_stream = Box::pin(memories.watch().where_pinned(true).stream());
    let mut tagged_stream = Box::pin(memories.watch().where_tag("flagged").stream());

    // Both emit empty initial.
    assert!(pinned_stream.next().await.unwrap().is_empty());
    assert!(tagged_stream.next().await.unwrap().is_empty());

    // Store a memory with tag "flagged" but not pinned.
    memories
        .store(1, "flagged mem", vec!["flagged".into()], "alice", 100)
        .unwrap();

    // tagged_stream yields [1]; pinned_stream stays empty (no emit).
    let t = tagged_stream.next().await.unwrap();
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].id, 1);

    // Pin it. Now pinned_stream yields [1]; tagged_stream stays
    // unchanged (still tagged flagged, no re-emission needed).
    memories.pin(1, 200).unwrap();
    let p = pinned_stream.next().await.unwrap();
    assert_eq!(p.len(), 1);
}

#[tokio::test]
async fn test_watch_with_limit_and_order() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    let mut stream = Box::pin(
        memories
            .watch()
            .where_pinned(true)
            .order_by(OrderBy::CreatedDesc)
            .limit(2)
            .stream(),
    );
    assert!(stream.next().await.unwrap().is_empty());

    for id in 1..=5u64 {
        memories
            .store(
                id,
                format!("m-{}", id),
                Vec::<String>::new(),
                "alice",
                100 * id,
            )
            .unwrap();
        memories.pin(id, 100 * id + 1).unwrap();
    }

    // Drain until we see the top-2 newest pinned: ids 5, 4.
    let mut last: Vec<_> = Vec::new();
    for _ in 0..20 {
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
async fn test_regression_snapshot_restore_preserves_app_seq_monotonicity() {
    // Regression: `MemoriesAdapter::open_from_snapshot[_with_config]`
    // used to recreate the adapter with `app_seq: AtomicU64::new(0)`,
    // so post-restore events re-emitted `EventMeta::seq_or_ts` values
    // that pre-snapshot events already used. The fix wraps `app_seq`
    // into the snapshot payload and restores it so per-origin
    // monotonicity is preserved.
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    memories
        .store(1, "alpha", vec!["x".into()], "alice", 100)
        .unwrap();
    memories
        .store(2, "beta", vec!["y".into()], "alice", 200)
        .unwrap();
    let seq = memories
        .store(3, "gamma", vec!["z".into()], "alice", 300)
        .unwrap();
    memories.wait_for_seq(seq).await;

    let (state_bytes, last_seq) = memories.snapshot().unwrap();
    memories.close().unwrap();

    let redex2 = Redex::new();
    let memories2 = MemoriesAdapter::open_from_snapshot(&redex2, ORIGIN, &state_bytes, last_seq)
        .await
        .unwrap();

    let new_seq = memories2
        .store(4, "delta", vec!["w".into()], "alice", 400)
        .unwrap();
    memories2.wait_for_seq(new_seq).await;

    let file = redex2
        .open_file(
            &ChannelName::new(MEMORIES_CHANNEL).unwrap(),
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
    // `MemoriesAdapter::open[_with_config]` awaits the
    // inner fold task's catch-up before returning. State is fully
    // visible synchronously — no `wait_for_seq` required.
    let redex = Redex::new();
    {
        let a = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
        a.store(1, "first", vec!["a".into()], "src", 100).unwrap();
        a.store(2, "second", vec!["b".into()], "src", 200).unwrap();
        let seq = a.store(3, "third", vec!["c".into()], "src", 300).unwrap();
        a.wait_for_seq(seq).await;
        a.close().unwrap();
    }

    let b = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
    let state = b.state();
    let guard = state.read();
    assert_eq!(
        guard.len(),
        3,
        "post-open state must be fully caught up — saw {} memories, expected 3",
        guard.len(),
    );
}

#[tokio::test]
async fn test_open_on_empty_redex_does_not_block() {
    // Edge case: `open` on a fresh empty Redex must not block on
    // `wait_for_seq`. Wrap in a 2s timeout so a regression that
    // awaits an unreachable seq surfaces as a test failure, not a
    // hung run.
    let redex = Redex::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        MemoriesAdapter::open(&redex, ORIGIN),
    )
    .await;
    assert!(
        matches!(result, Ok(Ok(_))),
        "open() on an empty Redex must complete promptly; got {result:?}",
    );
}

#[tokio::test]
async fn test_open_from_snapshot_with_empty_replay_tail_keeps_snapshot_app_seq() {
    // When the snapshot's `last_seq` already covers every event in
    // the file, the wrapper sees nothing during catch-up and the
    // snapshot's persisted `app_seq` survives. The first post-restore
    // ingest stamps `seq_or_ts = persisted_app_seq`.
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
    memories
        .store(1, "a", vec!["x".into()], "src", 100)
        .unwrap();
    memories
        .store(2, "b", vec!["y".into()], "src", 200)
        .unwrap();
    let seq = memories
        .store(3, "c", vec!["z".into()], "src", 300)
        .unwrap();
    memories.wait_for_seq(seq).await;

    let (state_bytes, last_seq) = memories.snapshot().unwrap();
    memories.close().unwrap();

    let redex2 = Redex::new();
    let restored = MemoriesAdapter::open_from_snapshot(&redex2, ORIGIN, &state_bytes, last_seq)
        .await
        .unwrap();

    let new_seq = restored
        .store(4, "d", vec!["w".into()], "src", 400)
        .unwrap();
    restored.wait_for_seq(new_seq).await;

    let file = redex2
        .open_file(
            &ChannelName::new(MEMORIES_CHANNEL).unwrap(),
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
    // `MemoriesAdapter::open` set `app_seq = AtomicU64::new(0)`
    // unconditionally, so reopening against a Redex with existing
    // same-origin events caused the next ingest to stamp
    // `EventMeta::seq_or_ts = 0`, colliding with the pre-existing
    // event's `seq_or_ts = 0`. Same fix as `TasksAdapter`: a
    // `WatermarkingFold` wrapper advances `app_seq` via fetch_max
    // during replay; the constructor awaits catch-up before
    // returning.
    let redex = Redex::new();

    {
        let a = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
        a.store(1, "first", vec!["a".into()], "src", 100).unwrap();
        a.store(2, "second", vec!["b".into()], "src", 200).unwrap();
        let seq = a.store(3, "third", vec!["c".into()], "src", 300).unwrap();
        a.wait_for_seq(seq).await;
        a.close().unwrap();
    }

    let b = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
    let new_seq = b.store(4, "fourth", vec!["d".into()], "src", 400).unwrap();
    b.wait_for_seq(new_seq).await;

    let file = redex
        .open_file(
            &ChannelName::new(MEMORIES_CHANNEL).unwrap(),
            Default::default(),
        )
        .unwrap();
    let events = file.read_range(new_seq, new_seq + 1);
    assert_eq!(events.len(), 1);
    let meta = EventMeta::from_bytes(&events[0].payload[..EVENT_META_SIZE]).unwrap();
    assert_eq!(
        meta.seq_or_ts, 3,
        "first ingest after reopen must continue past replayed events' seq_or_ts \
         (got {}, expected 3)",
        meta.seq_or_ts,
    );
}

#[tokio::test]
async fn test_regression_open_ignores_other_origins_when_advancing_app_seq() {
    // The watermarking-fold wrapper only advances `app_seq` for
    // events whose `origin_hash` matches the adapter's; cross-origin
    // events sharing the channel must not pollute our counter.
    let redex = Redex::new();
    const ORIGIN_A: u64 = 0x0000_AABB;
    const ORIGIN_B: u64 = 0x0000_CCDD;

    {
        let b = MemoriesAdapter::open(&redex, ORIGIN_B).await.unwrap();
        b.store(10, "b1", vec!["x".into()], "src", 100).unwrap();
        b.store(11, "b2", vec!["y".into()], "src", 200).unwrap();
        let seq = b.store(12, "b3", vec!["z".into()], "src", 300).unwrap();
        b.wait_for_seq(seq).await;
        b.close().unwrap();
    }

    let a = MemoriesAdapter::open(&redex, ORIGIN_A).await.unwrap();
    let new_seq = a.store(20, "a1", vec!["q".into()], "src", 400).unwrap();
    a.wait_for_seq(new_seq).await;

    let file = redex
        .open_file(
            &ChannelName::new(MEMORIES_CHANNEL).unwrap(),
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
    // the memories adapter's `ingest_typed`. Producers now stamp the
    // header-covering v2 checksum (`compute_checksum_with_meta`) so
    // a bit-flip in the EventMeta header — not just the tail — is
    // detected. Verify the on-disk event's meta.checksum matches
    // the v2 hash.
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    let seq = memories
        .store(
            7,
            "non-trivial content for checksum",
            vec!["alpha".into()],
            "alice",
            12345,
        )
        .unwrap();
    memories.wait_for_seq(seq).await;

    let file = redex
        .open_file(
            &ChannelName::new(MEMORIES_CHANNEL).unwrap(),
            Default::default(),
        )
        .unwrap();
    let events = file.read_range(0, 1);
    assert_eq!(events.len(), 1, "memories channel should have one event");
    let payload = &events[0].payload;
    let meta = EventMeta::from_bytes(&payload[..EVENT_META_SIZE]).expect("decode meta");
    let tail = &payload[EVENT_META_SIZE..];

    assert_ne!(meta.checksum, 0, "checksum must not be hardcoded to 0");
    assert_eq!(
        meta.checksum,
        compute_checksum_with_meta(&meta, tail),
        "meta.checksum must match the v2 header-covering hash",
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
    // Seed enough memories that hash iteration order is demonstrably
    // non-ascending, then assert the watch output is IdAsc.
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
    const N: u64 = 64;
    let mut last = 0;
    for id in 1..=N {
        last = memories
            .store(
                id,
                format!("m-{}", id),
                vec!["bench".into()],
                "alice",
                id * 100,
            )
            .unwrap();
    }
    memories.wait_for_seq(last).await;

    // Open watch *without* order_by. The fix makes this default to
    // IdAsc under the hood.
    let mut stream = Box::pin(memories.watch().where_tag("bench").stream());
    let initial = stream.next().await.unwrap();
    assert_eq!(initial.len(), N as usize);
    let ids: Vec<u64> = initial.iter().map(|m| m.id).collect();
    let sorted: Vec<u64> = (1..=N).collect();
    assert_eq!(ids, sorted, "watcher without order_by must emit IdAsc");
}

#[tokio::test]
async fn test_snapshot_and_restore_round_trip() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    memories
        .store(1, "alpha", vec!["x".into()], "alice", 100)
        .unwrap();
    memories.pin(1, 110).unwrap();
    memories
        .store(2, "beta", vec!["y".into()], "alice", 200)
        .unwrap();
    let seq = memories
        .retag(2, vec!["y".into(), "z".into()], 210)
        .unwrap();
    memories.wait_for_seq(seq).await;

    let (bytes, last_seq) = memories.snapshot().unwrap();
    assert_eq!(last_seq, Some(3));
    memories.close().unwrap();

    // Restore on a fresh Redex.
    let redex2 = Redex::new();
    let memories2 = MemoriesAdapter::open_from_snapshot(&redex2, ORIGIN, &bytes, last_seq)
        .await
        .unwrap();
    let state = memories2.state();
    let guard = state.read();
    assert_eq!(guard.len(), 2);
    let m1 = guard.get(1).unwrap();
    assert!(m1.pinned);
    let m2 = guard.get(2).unwrap();
    assert_eq!(m2.tags, vec!["y".to_string(), "z".to_string()]);
}

#[tokio::test]
async fn test_ingest_after_close_errors() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
    memories
        .store(1, "before close", Vec::<String>::new(), "alice", 100)
        .unwrap();
    memories.close().unwrap();

    assert!(memories
        .store(2, "after close", Vec::<String>::new(), "alice", 200)
        .is_err());
}

#[cfg(feature = "cortex")]
#[tokio::test]
async fn test_memories_and_tasks_coexist_on_same_redex() {
    // Two CortEX models sharing one Redex manager, each with its own
    // file. Events on either channel must not leak into the other's
    // state.
    use net::adapter::net::cortex::tasks::{TaskStatus, TasksAdapter};

    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
    let tasks = TasksAdapter::open(&redex, ORIGIN).await.unwrap();

    // Drive both in parallel.
    memories
        .store(1, "mem-1", vec!["m".into()], "alice", 100)
        .unwrap();
    tasks.create(1, "task-1", 100).unwrap();
    memories
        .store(2, "mem-2", vec!["m".into()], "alice", 200)
        .unwrap();
    let task_seq = tasks.complete(1, 210).unwrap();
    let mem_seq = memories.pin(1, 220).unwrap();

    memories.wait_for_seq(mem_seq).await;
    tasks.wait_for_seq(task_seq).await;

    // Memories state has 2 memories, one pinned.
    let mstate = memories.state();
    let mg = mstate.read();
    assert_eq!(mg.len(), 2);
    assert_eq!(mg.query().where_pinned(true).count(), 1);

    // Tasks state has 1 task, completed.
    let tstate = tasks.state();
    let tg = tstate.read();
    assert_eq!(tg.len(), 1);
    assert_eq!(tg.get(1).unwrap().status, TaskStatus::Completed);
}

#[tokio::test]
async fn test_snapshot_and_watch_delivers_post_call_updates() {
    // Baseline functional contract: a mutation that happens strictly
    // after `snapshot_and_watch` returns must land on the delta
    // stream. This path does not exercise the skip-vs-skip_while
    // race — both implementations pass it — but guards against any
    // future change that accidentally over-filters legitimate deltas.
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();
    let seq = memories
        .store(1, "seed", vec!["t".into()], "alice", 100)
        .unwrap();
    memories.wait_for_seq(seq).await;

    let watcher = memories.watch();
    let (initial, mut stream) = memories.snapshot_and_watch(watcher);
    let initial_ids: Vec<_> = initial.iter().map(|m| m.id).collect();
    assert_eq!(initial_ids, vec![1]);

    let seq = memories
        .store(2, "post", vec!["t".into()], "alice", 200)
        .unwrap();
    memories.wait_for_seq(seq).await;

    let observed = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .expect("stream must emit after mutation")
        .expect("stream must not end");
    let ids: Vec<_> = observed.iter().map(|m| m.id).collect();
    assert_eq!(ids, vec![1, 2]);
    assert_ne!(observed, initial);
}

#[tokio::test]
async fn test_regression_snapshot_and_watch_forwards_divergent_stream_initial() {
    // Regression for the `skip(1)` race fix on
    // `MemoriesAdapter::snapshot_and_watch`: the watcher's `stream()`
    // reads state independently of our own snapshot read. If state
    // mutates between those two reads, the watcher's first emission
    // reflects the newer state. The old `skip(1)` silently dropped
    // that divergent emission, leaving the caller on a stale snapshot
    // with no further delta arriving — because the fold's change
    // event had already been consumed into `last` inside the watch
    // task's seed value. `skip_while(== snapshot)` forwards only the
    // emissions that match the snapshot we already handed out.
    //
    // Drive the race by spawning a concurrent mutator around each
    // call. Trials where the mutation fully lands before the snapshot
    // read are skipped (nothing further to deliver). Remaining trials
    // must see the stream emit the post-mutation state within a tight
    // timeout; under the old bug the race trials would hang because
    // the watcher's internal `last` already equals the post-mutation
    // state, so no subsequent emission differs from it.
    for trial in 0..20 {
        let redex = Redex::new();
        let memories = std::sync::Arc::new(MemoriesAdapter::open(&redex, ORIGIN).await.unwrap());
        let seq = memories
            .store(1, "seed", vec!["t".into()], "alice", 100)
            .unwrap();
        memories.wait_for_seq(seq).await;

        let memories_m = memories.clone();
        let mutator = tokio::spawn(async move {
            let seq = memories_m
                .store(2, "race", vec!["t".into()], "alice", 200)
                .unwrap();
            memories_m.wait_for_seq(seq).await;
        });

        let watcher = memories.watch();
        let (initial, mut stream) = memories.snapshot_and_watch(watcher);
        mutator.await.unwrap();

        // Mutation already applied before the snapshot read: no
        // further delta to deliver.
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
            "trial {}: stream must deliver state with both memories",
            trial
        );
    }
}

/// Regression for BUG_AUDIT_2026_04_30_CORE.md #115: pre-fix
/// `MemoriesFold::DISPATCH_MEMORY_STORED` constructed a fresh
/// `Memory { pinned: false, created_ns: now_ns, ... }` and
/// `insert`ed it, silently replacing any existing entry. So
/// `memories.store(42, "updated", ...)` after `memories.pin(42)`
/// dropped the pin flag and overwrote the original
/// creation timestamp — operator had no observable signal that
/// the pin was lost.
///
/// Post-fix: re-storing an existing id treats it as a content
/// update — `pinned` and `created_ns` are preserved; `content`,
/// `tags`, `source`, and `updated_ns` are overwritten.
#[tokio::test]
async fn re_store_preserves_pinned_flag_and_created_ns() {
    let redex = Redex::new();
    let memories = MemoriesAdapter::open(&redex, ORIGIN).await.unwrap();

    // 1. Initial store at created_ns=100.
    memories
        .store(1, "first content", vec!["initial".into()], "alice", 100)
        .unwrap();
    let seq = memories.pin(1, 110).unwrap();
    memories.wait_for_seq(seq).await;

    {
        let state = memories.state();
        let guard = state.read();
        let m = guard.get(1).unwrap();
        assert!(m.pinned, "memory must be pinned after pin()");
        assert_eq!(m.created_ns, 100);
        assert_eq!(m.content, "first content");
    }

    // 2. Re-store with new content at now_ns=200.
    let seq = memories
        .store(1, "updated content", vec!["updated".into()], "bob", 200)
        .unwrap();
    memories.wait_for_seq(seq).await;

    let state = memories.state();
    let guard = state.read();
    let m = guard.get(1).unwrap();
    // Pre-fix: pinned would be false, created_ns would be 200.
    assert!(m.pinned, "pinned flag must be preserved across re-store");
    assert_eq!(
        m.created_ns, 100,
        "created_ns must be preserved across re-store"
    );
    // Updated fields:
    assert_eq!(m.content, "updated content");
    assert_eq!(m.tags, vec!["updated".to_string()]);
    assert_eq!(m.source, "bob");
    assert_eq!(m.updated_ns, 200);
}
