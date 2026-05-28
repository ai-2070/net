# Polling → Event-Driven SDK Migration Plan

Status: implemented (E-1..E-9; E-10 deferred)
Owner: TBD
Related: `CAPABILITY_BROADCAST_PLAN.md`, `NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md`,
`AI_TOOL_INTEGRATION_PLAN.md`

## 1. Problem

Several SDK "watch" surfaces are implemented as **interval poll loops**: a timer
fires every N seconds, the watcher re-queries a full snapshot, diffs it against
the previous snapshot, and emits the delta. This has three costs:

1. **Latency floor** — a change is invisible until the next tick (default 1 s).
2. **Idle CPU / wakeups** — the loop re-walks the capability fold every interval
   even when nothing changed; on a busy node `list_tools` is an O(fold) walk.
3. **Duplicated work across bindings** — every binding wraps the *same* substrate
   poll loop, so the waste is multiplied, not shared.

The load-bearing discovery from the audit: **the polling is baked at the
substrate layer, not the bindings.** `MeshNode::watch_tools`
(`src/adapter/net/mesh.rs:5534`) is itself a `tokio::time::interval` + repeated
`list_tools` + diff. Every binding (`watchTools` / `watch_tools` / `WatchTools`)
just forwards its `ToolListChange` stream. **Fixing it once at the substrate
fixes all four SDKs.**

## 2. Confirmed polling inventory

| Surface | File:line | Mechanism | Default interval |
|---|---|---|---|
| substrate `MeshNode::watch_tools` | `src/adapter/net/mesh.rs:5534` | `tokio::time::interval` + `list_tools` + diff | 1 s |
| Go `WatchTools` | `go/tool.go:~1024` | `time.NewTicker` + `ListTools` + `diffToolIndex` | 1 s |
| Python `watch_tools` | `bindings/python/python/net/tool.py:~457` | `asyncio.sleep` loop + `list_tools` | 1 s |
| Node `watchTools` | `bindings/node/tool.ts:~567` | `setTimeout` loop + `listTools` | 1000 ms |
| Rust SDK `watch_tools` | (not yet exposed; would wrap substrate) | — | — |

> **Note — the bindings' loops are partly redundant with the substrate loop.**
> Go/Node/Python re-implement their *own* poll + diff on top of the unary
> `list_tools` RPC rather than consuming the substrate's `ToolListWatch` stream.
> Part of this migration is collapsing them onto the single substrate event
> source.

### Audited candidates — verdicts (2026-05-29)

The four flagged surfaces were traced from API down to substrate source.
**Three of four are already event-driven** (correctly designed as push). Only the
deck cohort polls.

| Surface | Backing | Verdict |
|---|---|---|
| Memories `watch` / `snapshot_and_watch` | `tokio::sync::watch::channel` (`cortex/memories/watch.rs:175`); adapter `cortex/memories/adapter.rs:309` | ✅ already push — **no work** |
| Tasks `watch` / `snapshot_and_watch` | `tokio::sync::watch::channel` (`cortex/tasks/watch.rs:152`); adapter `cortex/tasks/adapter.rs:286` | ✅ already push — **no work** |
| Redex `tail` | `mpsc::channel`, watcher registered under the state lock, pushed on append (`redex/file.rs:966`) | ✅ already push — **no work** |
| Deck `watch` / `watch_timeout` / `SnapshotStream` / `StatusSummaryStream` | `tokio::time::sleep` re-reading `snapshot_reader`, `snapshot_poll_interval` (default 100 ms) — `deck.rs:1110`, `deck.rs:670`, `deck.rs:693`; cadence doc at `deck.rs:234` | ❌ **POLLING — real candidate** |
| aggregator `FoldQueryClient` TTL-cache | TTL cache, not a watch | out of scope — a staleness budget is a feature |

**The memory/task watch iterators and redex tail iterators in the bindings wrap
these already-push substrate sources, so they need no migration.** (The earlier
worry that they polled was unfounded.)

### Deck is a second, independent polling cohort

Deck's `watch` / `watch_timeout` / `SnapshotStream` / `StatusSummaryStream` all
poll the **MeshOS snapshot fold** via `snapshot_reader.read()` on a
`snapshot_poll_interval` timer — a *different* fold from the capability fold that
backs `watch_tools`. So it's the same shape of fix (fold-change notify →
await instead of sleep) applied to a second fold. It can't share E-1's notify
(different fold) but it reuses the pattern. Tracked as slices E-8..E-9 below.

## 3. The event source that already exists

The capability fold is mutated in exactly two places:

- **inbound peer announcements**: `capability_fold.apply(...)`
  (`src/adapter/net/mesh.rs:6490`)
- **local self-index** (a `serve_tool` / `announce_capabilities` registration):
  the `capability_version.fetch_add` path (`src/adapter/net/mesh.rs:8027`)

Tool descriptors are derived purely from the capability fold (`list_tools` is an
in-memory walk of it). Therefore **every** `ToolListChange` is downstream of a
fold mutation. That gives us the hook: fire a signal on fold mutation, and the
watcher only diffs when something actually changed.

`tokio::sync::Notify` is already imported and used across `mesh.rs`
(`shutdown_notify`, `pending_stream_grants_notify`), so the pattern is idiomatic
here.

## 4. Design

### 4.1 Substrate: fold-change notify

Add a `Arc<tokio::sync::Notify>` (or a `watch::Sender<u64>` carrying the existing
`capability_version` so a watcher can detect missed wakeups) that fires whenever
the capability fold mutates:

- bump + `notify_waiters()` at the two mutation sites (inbound apply + local
  self-index). Prefer routing both through one private helper
  (`fn bump_capability_version(&self)`) so the notify can't be forgotten at a
  future third mutation site.
- `watch_tools` replaces

  ```rust
  let mut ticker = tokio::time::interval(interval);
  loop { ticker.tick().await; /* re-walk + diff */ }
  ```

  with

  ```rust
  loop {
      // register BEFORE the diff so a mutation racing the diff wakes us
      let changed = fold_changed.notified();
      // diff current vs prev, emit ToolListChange, prev = next
      changed.await;
      if tx.is_closed() { return; }
  }
  ```

  (same race-safe register-before-check shape as `RpcCancellationToken::cancelled`
  in `cortex/rpc.rs`).

**Compatibility knob.** Keep `interval: Option<Duration>` in the signature.
`None` (or a new `WatchMode::Push`) = pure event-driven. `Some(d)` = a *debounce
ceiling*: coalesce bursts and guarantee a diff at least every `d` as a safety net
(useful if a future mutation path forgets to bump). This preserves the existing
API shape and gives a fallback while confidence builds. **No binding signature
changes** — the param already exists.

### 4.2 Optional: cross-node push (later phase, separate plan)

The above makes the *local* watch event-driven (latency = local fold-apply
latency). A remote consumer ("watch tools on node B from node A") today would
still need either an nRPC server-streaming subscription or a per-fold-kind
pub/sub channel. That is a **larger** change (wire protocol + auth) and should be
its own plan — see `CAPABILITY_BROADCAST_PLAN.md`. **Out of scope here.** This
plan only removes the *interval timer*; it does not add a new network surface.

## 5. Slices

- **E-1 — substrate fold-change notify.** Add the notify + `bump_capability_version`
  helper, wire both mutation sites. Pure addition; no behavior change yet.
  Unit test: announce a tool → notify fires; withdraw → fires.
- **E-2 — `watch_tools` push loop.** Swap the interval loop for the
  notified-await loop; keep `Some(interval)` as the debounce-ceiling fallback.
  Regression: existing `watch_tools` integration test must pass unchanged
  (it asserts Added/Removed/NodeCountChanged ordering, not timing). Add a test
  asserting change latency ≪ interval (e.g. emit within 50 ms with a 5 s
  "interval").
- **E-3 — collapse Go `WatchTools`** onto the substrate stream. Drop
  `time.NewTicker` + `diffToolIndex`; consume the substrate-emitted
  `ToolListChange` via the existing streaming FFI surface
  (`net_rpc_serve_streaming` / the watch FFI if one exists, else add a
  `net_*_watch_tools` streaming export). Keep `WatchOptions.Interval` as the
  debounce ceiling for signature stability.
- **E-4 — collapse Python `watch_tools`** likewise (async-gen wrapping the
  substrate stream instead of `asyncio.sleep`).
- **E-5 — collapse Node `watchTools`** likewise (async-iter over the napi stream
  instead of `setTimeout`).
- **E-6 — Rust SDK `Mesh::watch_tools`** — expose the substrate watch as a public
  SDK method returning the `ToolListWatch` stream directly (no re-poll).
- **E-7 — audit the "to verify" surfaces** — ✅ DONE (2026-05-29, see §2). Memory/
  task watchers + redex tail are already push; only deck polls. No binding work
  for memory/task/redex.
- **E-8 — MeshOS snapshot publish notify.** The snapshot lives in an
  `Arc<ArcSwap<MeshOsSnapshot>>` (`meshos/event_loop.rs:73`), read lock-free via
  `MeshOsSnapshotReader::read`/`load` (`:359`). **Single write site**:
  `publish_snapshot()` ends in `self.snapshot.store(Arc::new(snap))`
  (`event_loop.rs:1590`). Pair the `ArcSwap` with an `Arc<Notify>` (keep ArcSwap
  for the lock-free read fast-path — don't swap it for `watch`, which would take a
  read-lock per read; the comment at `:1988` explains why ArcSwap was chosen). Fire
  `notify_waiters()` right after the store; `MeshOsSnapshotReader` carries a clone
  and gains `async fn changed(&self)`. Pure addition; no behavior change.

  **Caveat — `publish_snapshot()` fires every Tick, not only on change**
  (`event_loop.rs:1559`; field doc `:67` "Updated on every Tick after the reconcile
  pass"). So the MeshOS loop *already* republishes on its own tick cadence,
  independent of deck. Two consequences:
  - **The win is real for deck**: deck currently runs a *second, independent* timer
    (`snapshot_poll_interval`, default 100 ms) sampling a value the loop is already
    republishing — two unsynchronized timers with phase-lag between them. E-8 lets
    deck consume the loop's existing publish signal and drop its own timer; latency
    becomes "next publish" instead of "next deck poll", with no phase-lag.
  - **It does NOT eliminate tick-rate wakeups** — those happen on the MeshOS loop
    regardless of whether deck watches. Driving deck to "zero periodic wakeups on an
    idle node" would additionally require *change-gating* `publish_snapshot` (only
    `store` + notify when the new snapshot differs from the stored one, e.g. via a
    reconcile-bumped generation counter rather than a deep `MeshOsSnapshot` eq).
    That's a separate optimization to the MeshOS loop itself — **out of scope for the
    deck-watch migration; note as a follow-up (E-10, deferred).**
- **E-9 — deck `watch` + `SnapshotStream` + `StatusSummaryStream` push loop.**
  Swap the `tokio::time::sleep(snapshot_poll_interval)` re-read loops
  (`deck.rs:1110`, `:670`, `:693`) for notified-await loops driven by E-8. Keep
  `snapshot_poll_interval` as a debounce ceiling (same fallback role as
  `watch_tools`' `Some(interval)`). Regression: existing deck watch tests
  (`deck.rs:3118`+) must pass unchanged; add a sub-interval-latency test.

Each binding slice (E-3..E-6) is independent and can land in any order once E-1/E-2
are in. E-1 → E-2 sequential; E-8 → E-9 sequential and independent of the
tool-watch track. Memory/task/redex need no slices (already push).

## 6. Risks / watch-outs

- **Missed-wakeup safety.** `Notify` only wakes *currently registered* waiters.
  Register the `notified()` future **before** the diff (as in E-2's snippet) so a
  mutation between diff-end and await-start isn't lost. The `Some(interval)`
  debounce ceiling is the belt-and-suspenders backstop.
- **Burst coalescing.** N announcements in one tick should produce *one* diff
  pass that emits N `Added`s, not N diff passes. `Notify::notify_waiters` already
  collapses to a single wake — good. Just don't re-arm inside the diff.
- **Backpressure unchanged.** Keep the bounded mpsc(256) + `send().await`; a slow
  consumer still backpressures, now against the notify loop instead of the ticker.
- **Don't widen scope to cross-node push** — that's `CAPABILITY_BROADCAST_PLAN.md`.
  This plan is strictly "delete the interval timer, keep the same stream contract."
- **TTL caches are not poll loops** — leave `FoldQueryClient`'s TTL cache alone
  unless the audit says otherwise; a staleness budget is a feature, not a defect.

## 7. Done criteria

- `MeshNode::watch_tools` has no `tokio::time::interval` on its hot path;
  change-detection latency is bounded by fold-apply latency, not the interval.
- Go/Python/Node watch surfaces no longer run their own ticker/`sleep`/`setTimeout`
  loop; they consume the single substrate event source.
- Existing `watch_tools` ordering/contract tests pass unchanged; a new
  latency test proves sub-interval delivery.
- Idle CPU: a node with a live `watch_tools` and a quiet fold does zero periodic
  fold walks.
