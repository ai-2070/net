# Code review — `meshos-sdks` branch (second pass)

**Date:** 2026-05-17
**Branch:** `meshos-sdks`
**Base:** `master`
**Scope:** 69 files, +28,500 / −1,649 LOC, 67 commits ahead.
**Predecessor:** [`CODE_REVIEW_2026_05_17_MESHOS_SDKS.md`](CODE_REVIEW_2026_05_17_MESHOS_SDKS.md) (Phase 1/2/3 review + closeout).

This pass verifies that the fixes claimed in the first review's closeout actually landed, and flags concerns not covered by that review.

---

## Verification — fixes claimed in the first-pass closeout

All claims spot-checked against current branch HEAD. Citations are line numbers in the post-fix tree.

| First-pass item | Verified at |
|---|---|
| 1.2 — `ClusterNode` fields → `pub(crate)` + accessors | `sdk/src/testing/cluster.rs:99-141` |
| 2.1 — `ClusterConfig.verifier` threads to every node, gated on `feature = "deck"` | `sdk/src/testing/cluster.rs:79-85`; regression test `sdk/tests/cluster_harness.rs:165` |
| 3.1 — Standalone `DeckClient` constructor (Node + Python) | Node `bindings/node/test/deck.test.ts:98-128`; Python `bindings/python/tests/test_deck.py:89-123` |
| 3.2 — `build_core_proposal` returns `Err("unknown_action")` for unmapped variants | Node `bindings/node/src/deck.rs:1178`; Python `bindings/python/src/deck.rs:1293`; Go `bindings/go/deck-ffi/src/lib.rs:2140, 2278` |
| 3.3 — `consumed: bool` flag replaces `u64::MAX` sentinel; `issued_at_ms` survives `_simulate` | `bindings/go/deck-ffi/src/lib.rs:1829, 2135` |
| 3.5 — TS `DeckClient.close()` + `[Symbol.asyncDispose]` | `sdk-ts/src/deck.ts:381-391` |
| 3.6 — Python `DeckClient.from_seed` + `__enter__` / `__exit__` / `close` | `sdk-py/src/net_sdk/deck.py:469-499` |
| 3.7 — Go `pumpStop` channel closed by `Free()`; pump exits cleanly | `bindings/go/net/meshos.go:560-682` |

All fixes are real (code-level, not doc-only) and the regression test for the verifier wire-through (the most behavior-affecting fix) is in place.

---

## New concerns

### N1. Go `MeshOsDaemonHandle.Free()` is not safe under concurrent calls — `bindings/go/net/meshos.go:661-682`

```go
func (h *MeshOsDaemonHandle) Free() {
    if h == nil || h.ptr == nil {
        return
    }
    if h.pumpStop != nil {
        select {
        case <-h.pumpStop:       // already closed by a prior Free
        default:
            close(h.pumpStop)    // panics if a second goroutine raced here
        }
    }
    C.net_meshos_handle_free(h.ptr)
    h.ptr = nil
    // ...
}
```

Two failure modes when two goroutines call `Free()` concurrently:

1. Both pass the `if h.ptr == nil` guard, both enter the `select`, both land in `default`, both call `close(h.pumpStop)` — the second one panics with `close of closed channel`.
2. Both call `C.net_meshos_handle_free(h.ptr)` on the same pointer (double-free in the cdylib).

The doc comment on `Free` only says "Idempotent on nil receivers" — it does not promise thread safety, so this may already be considered out-of-contract. But the new `pumpStop` guard *looks* like it's defending against concurrent callers (the `select { case <-h.pumpStop: ... }` pattern is the idiomatic shape used for that purpose), which makes the gap easy to misread.

**Fix options, in increasing order of work:**

- **Doc**: explicit one-liner — "Free is not safe to call concurrently with itself; serialize at the caller, or call once on the goroutine that owns the handle."
- **`sync.Once`**: wrap the body in `h.freeOnce.Do(...)`. Adds one field, eliminates the race for both the channel-close and the FFI-free. Matches the pattern Go users expect for `Close`-style idempotent finalizers.

Recommend `sync.Once`. The pattern is two lines of code and removes a foot-gun that the current shape invites.

### N2. `Free()` still races the goroutine started by `runtime.SetFinalizer`

Same file, same function. The finalizer registered at construction also calls `Free()`. If a consumer explicitly `Free()`s and the runtime finalizer fires concurrently (because `runtime.SetFinalizer(h, nil)` runs *after* the close-channel and C-free), both paths can interleave on the `pumpStop` close and the C-free pointer.

The explicit-Free path *does* call `runtime.SetFinalizer(h, nil)` at `meshos.go:681` to unregister — but only at the end of the function, after the unsafe work. The window between "panic-able close" and "finalizer cleared" is small but real.

`sync.Once` from N1 closes this too, since the finalizer hits the no-op path on the second call.

### N3. Consume-on-failure regression in `simulate()` / `commit()` across all three FFIs

Distinct from N1/N2. Introduced by the interaction of `22f885eb` (refuse unknown ICE variants → fallible `build_core_proposal`) with `d1ccd36c` (consumed-flag model). All three FFIs flip the proposal into the consumed state **before** calling the new fallible `build_core_proposal`, so an `Err("unknown_action")` leaves the husk consumed-but-not-simulated/committed.

**Node** — `bindings/node/src/deck.rs:1332-1340`:

```rust
let action = self.state.lock().await.take().ok_or_else(...)?;   // takes from mutex
let action_for_commit = action.clone();
let proposal = build_core_proposal(&self.client, action)?;       // fallible
```

If `build_core_proposal` returns `Err`, the action is already out of the mutex with no `SimulatedIceProposal` produced. A retry hits `already_simulated`, masking the real `unknown_action`.

**Python** — `bindings/python/src/deck.rs:1443-1456`. Identical shape.

**Go cdylib (`_simulate`)** — `bindings/go/deck-ffi/src/lib.rs:2135-2143`:

```rust
p.consumed = true;
clear_last_error_inner();
let core_proposal = match build_core_proposal(inner, action.clone()) {
    Ok(p) => p,
    Err(msg) => { set_last_error("unknown_action", &msg); return NET_DECK_ERR_CALL_FAILED; }
};
```

Same. The husk is consumed; retry returns `already_simulated`.

**Go cdylib (`_simulated_commit`)** — `bindings/go/deck-ffi/src/lib.rs:2271-2281`. `s.committed = true` flips before the fallible build; retry returns `already_committed`.

**Severity.** Latent today — fires only when the substrate adds a new `IceActionProposal` variant before the binding is updated. Not a HEAD-time bug, but a real user-visible footgun the moment it triggers, and the kind of regression a v1 SDK should not ship with.

**Fix.** Three FFIs × two methods. Clone-first, mutate-on-success:

```rust
// Validate first against a clone — leaves the consumed/committed flag
// untouched if build_core_proposal rejects the variant.
let core_proposal = build_core_proposal(inner, action.clone()).map_err(...)?;
p.consumed = true;
// ... continue with core_proposal ...
```

The `ice_issued_at_ms_survives_consumption_by_simulate` test exercises only the success branch, which is why this slipped through.

This also overlaps with first-pass item 3.4 ("`commit` re-runs `simulate` from scratch"). The clone-first pattern needs uniform application across both proposal classes in all three FFIs, not just the spot where the consumed flag flips.

### N4. Go `Free()` doesn't block on the pump's in-flight `NextControl` before the C-free

Distinct from N1/N2 (concurrent `Free`-vs-`Free` races) — this is a `Free`-vs-pump race that the `3131f710` polish narrowed but didn't close.

`bindings/go/net/meshos.go:661-682`:

```go
if h.pumpStop != nil {
    select {
    case <-h.pumpStop:
    default:
        close(h.pumpStop)
    }
}
C.net_meshos_handle_free(h.ptr)   // pump may be blocked inside NextControl(50) here
h.ptr = nil                        // data race vs the pump's reads of h.ptr
```

The pump observes `pumpStop` only between polls (`:1105-1113`). If it's currently blocked inside `h.NextControl(50)`, `Free` proceeds to `net_meshos_handle_free(h.ptr)` while that C call is still in flight against the same pointer. Window narrowed from "forever" to "≤50 ms" but not eliminated. There is also a non-atomic write to `h.ptr` racing the pump's reads.

**Fix.** Add a `done chan struct{}` the pump closes via `defer`, and `<-h.done` in `Free` before the C call:

```go
// pumpControlEvents:
defer close(h.done)
// Free():
close(h.pumpStop)
<-h.done                        // wait for the pump to actually exit
C.net_meshos_handle_free(h.ptr)
```

Combined with the `sync.Once` from N1, this gets both classes of race.

### N5. Audit-query setters skip `set_last_error` on the NULL-pointer arm

Incomplete `d0c52f0e`. `bindings/go/deck-ffi/src/lib.rs:1438-1490` — the success path now calls `clear_last_error_inner()`, but the `query.is_null()` arms return `NET_DECK_ERR_NULL` without calling `set_last_error`. A caller that passes NULL after an unrelated prior failure still reads the stale `kind`/`message` from `net_deck_last_error_*`.

Either set `("invalid_argument", "query is NULL")` (matching the rest of the file) or document that `NET_DECK_ERR_NULL` paths intentionally don't touch the envelope.

### N6. Python `PyDeckClient.close()` swallows shutdown errors — asymmetric vs Node

`bindings/python/src/deck.rs:692`:

```rust
py.detach(move || {
    runtime.block_on(async {
        let _ = sdk.shutdown().await;
    });
});
```

Node's matching path (`bindings/node/src/deck.rs:650-653`) surfaces the failure via `deck_err("shutdown_failed", ...)`. Failures during graceful drain are rare, but silently dropping them in Python while throwing in Node is exactly the kind of behavioural asymmetry the first review flagged on adjacent surfaces.

### N7. `PyDeckClient` standalone path has no `Drop` / `__del__` — defeats the `close()` fix

`bindings/python/src/deck.rs:540-622`. The `_owned_sdk: Option<CoreSdk>` field is taken by `close()` but there is no `Drop` impl on `PyDeckClient` and no `__del__` exposed to Python. A `from_seed`-built client that gets GC'd without an explicit `close()` drops `CoreSdk` naively — the private tokio runtime's workers are abandoned rather than drained. This is the same leak `close()` was added to plug, available unless the caller remembers to use it.

The Python test at `bindings/python/tests/test_deck.py:93-102` keeps the supervisor alive for the whole test process, which masks the leak.

**Fix.** Either implement a `Drop` that does best-effort `runtime.block_on(sdk.shutdown())` (mind the GIL — use `Python::with_gil` only if needed), or add `__del__` via `#[pyo3(text_signature = ...)]`. Drop is cleaner because GC-driven; `__del__` is unreliable in cycles.

### N8. `_owned_sdk` naming — misleading underscore prefix

`bindings/node/src/deck.rs:528, 642-654` and `bindings/python/src/deck.rs:550, 684`. Rust's `_` prefix is the "intentionally unused" convention, but the field is actively mutated by `close()` / `shutdown()`. Rename to `owned_sdk` (or `owned_supervisor`).

### N9. TS `DeckClient.new` casts `Uint8Array` to `Buffer`

`sdk-ts/src/deck.ts:343-344` does `operatorSeed as unknown as Buffer`. napi-rs's `Buffer` accepts `Uint8Array` at runtime so this works today, but the `a8489a93` polish eliminated the same anti-pattern on `__rawNapiSdk()`; this one slipped in two commits later (`1c914f9d`). Use the napi-side input type directly or accept `Uint8Array` in the TS signature without the cast.

### N10. Stale test comment in `bindings/node/test/deck.test.ts:113-115`

> "No explicit teardown yet (close() lands in a follow-up). The supervisor releases on GC."

`close()` did land — in `216e47a2`. The comment should be removed (and ideally the test should call `close()`).

### N11. `sdk-py/src/net_sdk/deck.py::DeckClient.from_seed` + context-manager dunders are uncovered

The Python tests at `bindings/python/tests/test_deck.py:18-24` import the raw pyo3 `DeckClient`, not the wrapper at `sdk-py/src/net_sdk/deck.py:468-499`. The new wrapper-level `from_seed` classmethod and `__enter__`/`__exit__` dunders have zero regression coverage. A trivial `with DeckClient.from_seed(seed) as c:` test would catch any wrapper-level wiring drift.

---

## Carried-forward open items (first-pass closeout)

These were tracked-but-deferred in the first review; no action taken in the verification pass other than confirming they're still open and not blockers.

| Item | First-pass disposition | Status |
|---|---|---|
| 3.4 — `commit` re-runs `simulate` from scratch in all 3 FFIs | "Verify — substrate-side audit deferred" | Still open. Real race window if cluster snapshot moves between sim and commit; the substrate may consider it acceptable because `(issued_at_ms, blast_hash)` binds deterministically. Worth a substrate-side audit before any deck UX claims the simulated handle is load-bearing. |
| 1.10 — `sdk/Cargo.toml` default-features expansion `[]` → 8-feature stack | "Out of scope — CHANGELOG belongs in release flow" | Still open. Any Rust consumer pulling `ai2070-net-sdk` with default features now silently picks up the entire stack (compile time, binary size, transitive deps). Needs a CHANGELOG note before the next tag; possibly a semver-major bump if the SDK has external consumers. |

---

## Out-of-scope additions in this branch

### `NET_CLI_PLAN.md` — `docs/plans/NET_CLI_PLAN.md`

New planning doc, 571 LOC, status: **"Not started."** No corresponding code lands in this branch — the `cli/` workspace member exists with an empty `src/` and the workspace `cli` feature flag is wired, but Phase 1 activation waits on a real consumer workflow.

Pure design-only addition; nothing to review at the implementation level.

---

## Test-coverage gaps (carried forward from first pass)

The first review's "Test coverage gaps" section is not yet addressed. Not blocking, but worth tracking:

- No concurrency tests in any binding (parallel admin commits, simultaneous stream + shutdown, two `registerDaemon` calls in parallel).
- No GC / finalization tests on Node/Python bindings — relevant given the `DeckClient` lifetime additions in 3.5 / 3.6.
- No shutdown-while-iterating tests on async streams.
- No filter-actually-filters test for `subscribeLogs`.
- No `droppedControlEvents` counter test on the MeshOS surface.

These would all be cheap to add and would lock in the lifetime + filter contracts the SDKs now advertise.

---

## Recommended actions

| Severity | Action |
|---|---|
| Pre-v1 | Clone-first / mutate-on-success in `simulate()` and `commit()` across Node, Python, and Go cdylib FFIs (item N3). Add failure-branch regression tests. |
| Pre-v1 | `done chan struct{}` wait in Go `Free()` to close the pump-in-flight use-after-free window (item N4). |
| Pre-v1 | `Drop` (or `__del__`) on `PyDeckClient` standalone path so a GC'd-without-`close()` instance still drains the owned supervisor (item N7). |
| Polish | Wrap `MeshOsDaemonHandle.Free()` body in `sync.Once` for concurrent-call + finalizer races (items N1, N2). Composes with N4. |
| Polish | Audit-query setter NULL arm sets last-error (item N5); Python `close()` surfaces shutdown errors symmetrically with Node (item N6); rename `_owned_sdk` → `owned_sdk` (item N8); drop the `Uint8Array as Buffer` cast in TS `DeckClient.new` (item N9); remove the stale test comment (item N10). |
| Tests | Add the wrapper-level `DeckClient.from_seed` + ctx-manager test in `sdk-py` (item N11); broader concurrency / GC / shutdown-while-iterating / filter-correctness tests across bindings (first-pass test-coverage gaps). |
| Tracking | Substrate-side audit for `commit` re-running `simulate` (item 3.4) — overlaps with the N3 fix surface. |
| Release | CHANGELOG entry for `sdk/Cargo.toml` default-features expansion before next tag (item 1.10). |

---

## Bottom line

The first-pass review's closeout is accurate: every fix it claims is wired in code, with regression tests where they matter. The verification pass surfaced new items concentrated in two areas: (a) consume-on-failure regressions in `simulate()` / `commit()` across all three FFIs (N3) — a latent footgun that the first review's `22f885eb` + `d1ccd36c` fixes introduced together but neither test suite exercises on the failure branch; and (b) Go `MeshOsDaemonHandle.Free()` thread-safety (N1, N2, N4), where the `3131f710` pumpStop fix narrowed but did not close the race against the pump's in-flight `NextControl`.

None of N1–N11 break HEAD today. N3 fires only when the substrate introduces a new `IceActionProposal` variant; N4 is a narrow ≤50 ms window; the rest are polish or test gaps. But N3, N4, and N7 are the kind of regressions a v1 SDK release should not ship with — all three are localized fixes (clone-first, `done` channel, `Drop` impl).

**No HEAD-blockers for merge.** Pre-v1 punch list: N3, N4, N7. Polish and tests can land in follow-ups.
