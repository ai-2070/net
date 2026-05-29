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

All implementation slices landed (2026-05-29). Status + as-built notes per
slice; divergences from the original sketch are flagged. See §8 for the
consolidated outcome.

- **E-1 — substrate fold-change notify.** ✅ DONE. As-built diverged from the
  "two mutation sites + `bump_capability_version`" sketch: the change signal is a
  `watch::Sender<u64>` *inside the `Fold`* (`behavior/fold/mod.rs`), fired by a
  private `signal_changed()` from every real mutation path — `apply`
  (Inserted/Replaced), `evict_node`, `restore`, and `sweep_expired` (the expiry
  task holds a `Weak` clone, `expiry.rs`). Consumers subscribe via
  `Fold::subscribe_changes() -> watch::Receiver<u64>`. `watch<u64>` over `Notify`
  because the generation counter is missed-wakeup-safe. The local self-index runs
  through `capability_fold.apply`, so it fires the signal too — no separate mesh
  site needed. Unit test: `subscribe_changes_fires_on_real_mutations_only`.
- **E-2 — `watch_tools` push loop.** ✅ DONE. `MeshNode::watch_tools` subscribes
  `subscribe_changes()` *before* the baseline snapshot, then loops over a
  `tokio::select!` of `change_rx.changed()`, an `Option<Interval>` ceiling arm
  (`None => pending()`), and `tx.closed()`. `interval` is now a debounce ceiling,
  not a cadence. Latency test
  (`watch_tools_delivers_change_well_under_the_debounce_ceiling`) proves sub-ceiling
  delivery with a 30 s ceiling.
- **E-2b — cancelable `watch_tools`** (extra slice not in the original sketch).
  A parked FFI/iterator `next()` *owns* the receiver, so dropping it can't
  interrupt a blocked recv. Added a `cancel: Arc<Notify>` to `ToolListWatch` +
  `cancel()` / `cancel_handle()`; the substrate loop gained a
  `cancel.notified() => return` arm so firing it exits the task → drops the sender
  → unblocks the parked recv with `None`. Substrate + the Go FFI both use it.
- **E-3 — Go `WatchTools`.** ✅ DONE. Added a dedicated `net_rpc_watch_tools*` FFI
  surface (`ToolWatchHandleC` + `_next`/`_close`/`_free`, all `#[cfg(feature =
  "tool")]`) — the serve-streaming surface didn't fit. `go/tool.go` drops
  `time.NewTicker` + `diffToolIndex` and consumes it via a two-goroutine
  closer/freer + watcher model (coordinated by a `watcherDone` channel) so `_free`
  happens exactly once and ctx-cancel unblocks a parked `_next`. `WatchOptions.Interval`
  kept as the debounce ceiling (`0` = pure event-driven). Go cgo can't compile on
  the dev box (no C toolchain); rpc-ffi compiles with `--features tool` and the
  extern signatures are cross-checked against the Rust exports — link is
  CI-validated.
- **E-4 — Python `watch_tools`.** ✅ DONE. pyo3 `AsyncToolWatchIter` (PEP 525)
  wraps the substrate stream; `NetMesh.watch_tools(interval_ms)` enters the shared
  tokio runtime to spawn the diff task. `net.tool.watch_tools` async-fors over it,
  parsing each JSON `ToolListChange` (substrate **snake_case** serde shape) into its
  dataclass. `interval=None` is pure event-driven. Fake-mesh + live single-node
  tests pass.
- **E-5 — Node `watchTools`.** ✅ DONE. napi `ToolWatchIter` (`async next()` /
  `close()`); `async NetMesh.watchTools(intervalMs)` spawns the diff task on the
  napi runtime. The TS wrapper async-fors over it. **Divergence:** the Node wire
  JSON is emitted **camelCase** (matching `ToolDescriptorJs` / `listTools`), unlike
  Python/Go which consume the substrate snake_case shape. `close()` eagerly drops
  the stream so an active watch doesn't keep the node un-shutdownable. Fake-iter +
  live single-node tests pass.
- **E-6 — Rust SDK `Mesh::watch_tools`.** ✅ DONE. The method already delegated
  straight to the substrate watch (no re-poll); updated the stale doc (interval is
  a ceiling, `None` is pure event-driven, `cancel`/drop ends the stream) and added a
  single-node SDK test (sub-ceiling delivery + prompt cancel).
- **E-7 — audit the "to verify" surfaces** — ✅ DONE (2026-05-29, see §2). Memory/
  task watchers + redex tail are already push; only deck polls. No binding work
  for memory/task/redex.
- **E-8 — MeshOS snapshot publish notify.** ✅ DONE. Paired the
  `Arc<ArcSwap<MeshOsSnapshot>>` with an `Arc<Notify>` (ArcSwap kept for the
  lock-free read fast-path); `publish_snapshot()` fires `notify_waiters()` right
  after the store. `MeshOsSnapshotReader` carries a clone and gained `async fn
  changed(&self)` plus `changed_owned()` — a `'static` future for driving a
  `Stream`'s sync `poll_next` where there's no `self` to borrow across polls.

  **Caveat — `publish_snapshot()` fires every Tick, not only on change.** The
  MeshOS loop *already* republishes on its own tick cadence. Consequences:
  - **The win is real for deck**: deck dropped its *second, independent* timer
    (`snapshot_poll_interval`) and consumes the loop's existing publish signal —
    latency is "next publish", no phase-lag.
  - **It does NOT eliminate tick-rate wakeups** — those happen on the MeshOS loop
    regardless. Driving deck to "zero periodic wakeups on an idle node" needs
    *change-gating* `publish_snapshot` (a reconcile-bumped generation counter
    rather than a deep `MeshOsSnapshot` eq) — **deferred as E-10.**
- **E-9 — deck `watch` + `SnapshotStream` + `StatusSummaryStream` push loop.**
  ✅ DONE. `DeckClient::watch` now `select!`s `changed()` against the
  `snapshot_poll_interval` ceiling. The two `Stream` impls hold an in-flight
  `changed_owned()` future, re-armed each fire, with the `Interval` as a ceiling
  backstop (its immediate first tick preserves "first poll emits current state").
  **Divergence:** the boxed `Notified` future is `Send` but `!Sync`, so the
  pyo3/napi `#[pyclass]` wrappers (which require `Send + Sync`) forced wrapping it
  in a `parking_lot::Mutex` (`Mutex<T>: Sync where T: Send`), locked only across
  the sync poll. Existing deck watch/stream tests pass unchanged; added
  `watch_is_event_driven_resolving_far_under_the_poll_ceiling` (30 s ceiling).

Dependency order as landed: E-1 → E-2 → E-2b; binding slices E-3/E-4/E-5/E-6
independent on top; E-8 → E-9 independent of the tool-watch track. Memory/task/
redex needed no slices (already push).

## 6. Risks / watch-outs

- **Missed-wakeup safety.** Handled two ways as built: the tool-watch track uses a
  `watch::Sender<u64>` *generation counter* (E-1), which is intrinsically
  missed-wakeup-safe — a bump between diff and await is observed by the next
  `changed()` regardless of registration timing. The deck track uses `Notify`
  (E-8); there the `snapshot_poll_interval` ceiling is the backstop for any publish
  that slips into the gap before the next `changed()` registers, and the MeshOS
  loop republishes every Tick anyway, so a missed edge is bounded by the tick.
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

- ✅ `MeshNode::watch_tools` has no `tokio::time::interval` on its hot path;
  change-detection latency is bounded by fold-apply latency, not the interval.
- ✅ Go/Python/Node watch surfaces no longer run their own ticker/`sleep`/`setTimeout`
  loop; they consume the single substrate event source (Go via the
  `net_rpc_watch_tools*` FFI, Python via `AsyncToolWatchIter`, Node via
  `ToolWatchIter`).
- ✅ Existing `watch_tools` ordering/contract tests pass unchanged; latency tests
  prove sub-ceiling delivery (substrate, Rust SDK, deck).
- ✅ Idle CPU: a node with a live `watch_tools` and a quiet fold does zero periodic
  fold walks (the watcher parks on `change_rx.changed()` / `tx.closed()`).

## 8. Implementation outcome (2026-05-29)

Landed on branch `event-driven-sdks`. Commit map:

- E-1/E-2/E-2b — substrate change signal, push loop, cancel (`5575c20d9` +
  predecessors).
- E-3b — Go `WatchTools` over the FFI watch.
- E-6 — Rust SDK doc + single-node test.
- E-4 — Python pyo3 iter + async-gen.
- E-5 — Node napi iter + TS wrapper (+ camelCase-wire and shutdownable-on-close
  fixes).
- E-8/E-9 — deck snapshot notify + push loops, plus a follow-up making the stream
  `pending` future `Sync` via `parking_lot::Mutex` for the pyclass wrappers, and
  two broken-intra-doc-link fixes surfaced by `-D rustdoc::broken-intra-doc-links`.

**Known limitation.** An active tool-watch holds an `Arc<MeshNode>` (the spawned
diff task captures it) until the iterator is closed/dropped. The Node `close()`
drops the stream eagerly so the node stays shutdownable; the substrate watch
otherwise releases its ref when the receiver drops. Not a leak, but worth knowing
when reasoning about shutdown with a live watcher.

**Post-review fixes (2026-05-29).** A review pass over the branch landed three
follow-ups:
- **Eager subscribe (Node/Python).** The Node TS and Python `watch_tools`
  wrappers subscribed to the substrate watch lazily — on the first iteration of
  the returned async iterable — so a change published between the `watch_tools()`
  call and the first iteration was dropped. Both now subscribe at call time
  (Python fully closes the race since the pyo3 iter takes its baseline
  synchronously; Node kicks off the napi promise eagerly), matching the Rust SDK.
- **camelCase wire-shape guard.** `descriptor_to_camel_json` (Node) now
  destructures `ToolDescriptor`, so a new field is a compile error until it's
  mapped into the camelCase wire shape; unit tests pin the exact key set.
- **Named FFI code.** The Go watch loop's bare `-6` (STREAM_DONE) is now a named
  `cRPCStreamDone` constant.

**Deferred (E-10).** Change-gate `publish_snapshot` (only `store` + notify when the
new snapshot differs, via a reconcile-bumped generation counter) to eliminate
tick-rate wakeups on an idle MeshOS loop. Out of scope for the deck-watch
migration; a separate optimization to the loop itself. The deck `SnapshotStream` /
`StatusSummaryStream` "best-effort per edge" property (a publish landing between a
`Ready` return and the next re-armed `changed()` poll is coalesced, bounded by the
ceiling) is a consequence of the `Notify` design and is resolved by the same E-10
generation-counter work — left as-is until then.

**Validation note.** The dev box has no C toolchain, so Go cgo link is
CI-validated; the Rust sides of all bindings compile locally with `--features
tool`, and Python/Node additionally pass live single-node delivery tests built via
maturin / `napi build`.
