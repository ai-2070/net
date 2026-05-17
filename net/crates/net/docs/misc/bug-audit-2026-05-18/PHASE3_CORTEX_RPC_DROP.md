# Phase 3 — `cortex/rpc.rs` significant-drop audit

**File:** `net/crates/net/src/adapter/net/cortex/rpc.rs` (3861 lines)
**Lint cluster:** 19 instances of `clippy::significant_drop_in_scrutinee` /
`clippy::significant_drop_tightening`.
**Headline:** null result. All 19 sites are STYLE. No held-across-await guard,
no `Arc<Mutex<...>>` deadlock window, no throughput cliff.

## Summary

| Category | Count |
|---|---|
| Real bug | 0 |
| Minor | 0 |
| Style | 19 |

The lock type used throughout this file is `parking_lot::Mutex` (synchronous,
non-async, no fairness queue). `tokio::sync::Mutex` is not used. The pending-
call map uses `dashmap::DashMap` (line 1916). All call paths follow one of
two patterns:

1. **Expression-temporary** — `self.foo.lock().remove(&key)` / `.get(&k).cloned()`
   / `.insert(...)`. The guard is dropped at the statement's `;`.
2. **Explicit scope or `drop`** — author already mitigated the lifetime
   (e.g. lines 1586-1611 has an explicit `{ let g = lock(); ... drop(g); ... }`
   block around the duplicate-REQUEST check; the `deliver` path at lines
   1986/2014/2039 has explicit `drop(entry)` before a `senders.remove(...)` to
   avoid the well-known DashMap same-shard self-deadlock).

No site holds a guard across `.await`. The five `tokio::spawn` blocks (1258,
1566, 1606, 1644, 1678) capture `Arc<Mutex<...>>` handles into the task and
do their `lock().remove(&key)` *after* the handler future resolves — the
guard's lifetime is one statement, not the whole task.

## Real-bug findings

None.

## Minor / style sites

Production code (4 sites):

- `rpc.rs:1352` — `in_flight.lock().remove(&key);` inside the spawned unary
  handler task, right before a sync `emit(...)`. Guard dropped at `;`. Style.
- `rpc.rs:1357` — `if let Some(token) = self.in_flight.lock().remove(&key)`
  in the unary CANCEL path. Scrutinee temporary; lives until end of `if let`,
  but body only calls `token.cancel()` (sync, cheap, on a separate
  `RpcCancellationToken` — not the same lock). Style. Tightening optional:
  `let removed = self.in_flight.lock().remove(&key); if let Some(t) = removed { t.cancel(); }`.
- `rpc.rs:1813` — same shape as 1357 for the streaming fold. Style.
- `rpc.rs:1845` — `if let Some(sem) = self.flow_control.lock().get(&key).cloned()`
  in the STREAM_GRANT path. Guard held across `sem.add_permits(safe)`, which
  is a sync atomic op on a separate `tokio::sync::Semaphore` — no lock-order
  hazard and no `.await`. Style.
- `rpc.rs:1982` — `let entry = self.senders.get(&call_id);` (DashMap `Ref`,
  not a `MutexGuard`). The author explicitly `drop(entry)`s before every
  `self.senders.remove(...)` in the `match` arms (1986, 2014, 2039), which
  is the correct DashMap discipline. The Streaming/Continue arm holds the
  ref across `tx.send(...)` on a non-blocking `UnboundedSender` — safe.
  Style.

Test code (15 sites, all `mod tests` below line 2125):

- `rpc.rs:2579, 2616, 2644, 2675, 2729, 2782, 2854, 2933, 2991, 3040, 3523,
  3580, 3622, 3678, 3852` — all are `let captured = captured.lock();`
  immediately before assertions, with the guard living to the end of the
  test function. Single-threaded test bodies; no contention; no `.await`
  after the lock. Style.

## Adjacent hazards (panics / spawn / block_on / clone)

- **`block_on` in async:** none.
- **`tokio::spawn` without `JoinHandle`:** five sites in production
  (1258, 1566, 1606, 1644, 1678). 1678's handle *is* captured (`let pump =`)
  and `await`ed at line 1735 — that's the streaming pump and ordering matters.
  The other four are fire-and-forget handler tasks. This is **intentional**
  (matches the unary RPC dispatch model — handlers self-clean by removing
  themselves from `in_flight` at lines 1352 and 1798) and panic-safe (every
  handler invocation is wrapped in
  `futures::FutureExt::catch_unwind(AssertUnwindSafe(...))` at lines 1286
  and 1727; panics become `RpcStatus::Internal` responses). Not a hazard.
- **Panics on user input:** none in production. All `unwrap` / `expect` /
  `panic!` matches are inside `#[cfg(test)]` (line 2125+). The wire-decoders
  return `Result` and the fold logs+returns on `Err` (e.g. line 1544-1570
  for the streaming fold's malformed-payload path).
- **`clone` on hot paths:** the per-request clones in the spawn prologues
  (1253-1257, 1627-1643) are `Arc` clones (handler, emit closure, metrics,
  flow_control, in_flight, cancellation token) — refcount bumps, not deep
  copies. The trace-context header decode (1248-1252, 1633-1637) is the
  only allocation, and it's gated on `FLAG_RPC_PROPAGATE_TRACE`. Fine.
- **DashMap discipline:** the `deliver` method at line 1978 is the one
  subtle spot. It correctly `drop(entry)`s a `Ref` before any
  `senders.remove(...)` on the same map (lines 1986, 2014, 2039). If a future
  edit removes one of those explicit drops, a same-shard self-deadlock
  becomes possible. Worth a comment but not a current bug.

## Verdict

`rpc.rs` is one of the cleaner files in the crate for this lint family
despite having the highest hit count — the count is a function of file
length and aggressive testing, not real lock-lifetime sins. The author
has already applied explicit-scope/explicit-drop discipline at every
spot where the lifetime matters (1586-1611 in particular).

**Recommendation:** allow this file to keep these 19 `clippy::pedantic`
warnings (or tighten the 4 production sites mechanically) — no behavioral
fix is warranted. If we tighten, prefer extracting the temporary into a
named binding rather than introducing `drop(...)` calls (cleaner diff,
same effect):

```rust
// 1357
let cancelled = self.in_flight.lock().remove(&key);
if let Some(token) = cancelled {
    token.cancel();
}
```

Time spent: ~15 min. No follow-on tickets.
