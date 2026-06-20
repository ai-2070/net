# Net v0.27.6 — "Purple Rain"

A **bindings & integration bug-fix release.** A full-workspace bug hunt across the `net` crate (~100k LOC Rust) plus the Go / Python / FFI binding layers landed **34 of 37 findings** across 47 commits and three review rounds. Every concrete first-pass bug lived at the **language-binding / FFI edge** — including three use-after-free races in the shipped Go module that are reachable by ordinary context cancellation.

**No wire-format change, no C-ABI change, no public API change.** Drop-in against honest v0.27.5 and earlier peers. Full audit: [`docs/misc/BUG_AUDIT_2026_06_18_BINDINGS.md`](../misc/BUG_AUDIT_2026_06_18_BINDINGS.md).

> [!IMPORTANT]
> **Go module users should upgrade promptly.** The three use-after-free races are reachable on routine `ctx` cancellation of a streaming call — not an exotic double-close.

## Highlights

- Fixed **3 use-after-free races** in the shipped Go module (`RpcStream`, `MeshOsDaemonHandle`, `MeshBlobAdapter`).
- Added **FFI panic guards** to `rpc-ffi` and `compute-ffi` — no more unwinding across the C ABI (UB).
- Fixed a **reliable-stream sequence gap** under scheduler backpressure (permanent gap + duplicate re-send on the documented bulk-load path).
- ~25 additional **MEDIUM/LOW correctness fixes** across meshdb joins, the load balancer, the aggregator, deck streams, nRPC, and RedEX.
- Rust changes are `cargo check` + `clippy` clean with per-finding regression tests; the core, identity/security, and RedEX recovery passes came back **clean and verified**.

## High-severity fixes

- **Go use-after-free ×3.** `RpcStream`, `MeshOsDaemonHandle`, and `MeshBlobAdapter` guarded their native handle with *check-then-use* (a bare `atomic.Bool`, or a mutex dropped before the cgo call) instead of *claim-then-use*. A concurrent free — ctx-cancel watcher, `Free()`/`Close()`, or the GC finalizer — could `drop(Box::from_raw(...))` the native object while a `Recv`/`Send`/`Store`/`NextControl` was parked in `block_on` → memory corruption. Fixed with a refcount **quiesce guard** (`streamHandleGuard`): ops bracket the cgo call with `enter()`/`leave()` without holding a lock, and the free runs once after the last op leaves and never blocks. Review rounds also covered the post-`Split()` halves and a separate `PublishLog` GC `KeepAlive` bug.
- **FFI panic guards (`rpc-ffi`, `compute-ffi`).** Neither crate had a `catch_unwind` at any entry point and both called tokio's raw `Runtime::block_on`, which panics on runtime re-entry — the unwind crossing `extern "C"` is undefined behavior. All entry points are now wrapped in `ffi_guard!` and `block_on` routes through the abort-on-reentry wrapper. (A review round caught the macro being *defined but never invoked* in `compute-ffi`, leaving all 80 entry points unguarded, plus a `daemon_count` panic-default that collided with a valid result.)
- **Reliable-stream sequence gap under backpressure.** `send_on_stream` consumed a sequence number atomically with byte credit, but a full `FairScheduler` queue surfaced `Backpressure` *after* the seq was taken — credit was refunded, the seq was not, and the retransmit descriptor was never registered. Result on a reliable stream: a permanent gap the receiver NACKs forever, plus duplicate re-send of already-committed batches on retry. Fixed by making the seq refundable and not replaying committed events; a follow-up bounded the committed-prefix retry (`COMMITTED_FLUSH_STALL_BUDGET`, 30 s) so a stalled receiver can't spin the sender forever.

## Other fixes (MEDIUM / LOW)

- **meshdb executor** — LEFT/RIGHT OUTER join (and `sort_merge_join`) silently dropped preserved-side rows with a missing/non-scalar join key; now emitted unmatched, matching full-outer. Federated query reported a fully-delivered result as failed on a lost trailing `End` frame — sender now always emits a `final = true` terminal batch.
- **load balancer** — half-open circuit probe slot could be permanently claimed (and a `0` recovery window collapsed the breaker — now clamped to ≥ 1 ms); hash-ring re-add leaked/clobbered ~150 stale vnodes; weighted-RR starved endpoints when all weights were < 1.0.
- **aggregator** — a zero `summary_interval` panicked the spawned task; `filter_novel` deduped on `fold_kind` only and re-published multi-row summaries every tick.
- **meshos reconcile / ICE** — duplicate `RequestEviction` per tick; `MarkAvoid` re-emitted every tick; `ThawCluster` blocked by the cluster cooldown (break-glass violation).
- **deck streams** — `deck-ffi` reported genuine stream-end as a timeout (livelocking Go polling loops); `AuditStream`/`LogStream`/`FailureStream` could park forever by not re-arming the waker; exported timestamps printed an epoch hour-count (missing `% 24`).
- **nRPC / routing** — duplicate in-flight `call_id` overwrote the prior caller's response sender; `mint_random_call_id` returned `0` on `getrandom` failure; a route owner couldn't update its own route to a worse metric.
- **RedEX** — `OutstandingRequests` cap only evicted expired entries (unbounded under load) → re-backed with `lru::LruCache` for an O(1) hard bound; per-entry checksum header coverage hardened (corrupt `seq` now caught by a monotonicity walk); age-based retention no longer assumes a monotonic wall-clock; catch-up TOCTOU + a 32-bit overflow guard.
- **cortex FFI** — five `(out_json, out_len)` functions now honor the out-param pre-zero contract; `net_rpc_duplex_into_split` no longer drops the surviving half on partial-consume. Plus out-param null checks and `len > isize::MAX` guards across the Go `*-ffi` crates.

## Investigated / deferred (not shipped)

- **Anti-replay `MAX_FORWARD` (reported HIGH) → downgraded to INFO and reverted.** Not an exploitable replay bypass; the forward-jump tolerance is deliberate design (survives > 1024-packet loss without a forced re-handshake, and stale counters are still caught by the age check). The proposed hardening broke 4 replay-window tests with no security gain.
- **Deferred:** `publish_to_peer` event-count chunking (a `publish_many` > 2028 events still trips a release-mode `assert!`); `C.GoBytes` ≥ 2 GiB truncation (~20 sites); the FFI seed-pointer length check (its companion guards landed). Each needs a non-trivial or breaking-ABI change unsuitable for a bugfix release.

## Dependencies

All in `net/crates/net/Cargo.lock` — **no `Cargo.toml` change**, so crates.io library consumers resolve identically; these reach only the distributed artifacts (CLI, FFI staticlibs, npm prebuilds, Python wheels, deck):

- Transitive bumps: `redis` 1.2.3, `syn` 2.0.118, `napi` 3.9.3, `bytes` 1.12.0, `h2` 0.4.15, `time` 0.3.49, `getrandom` 0.4.3, `webpki-roots` 1.0.8.
- Footprint reduction: a transitive WASM component-model toolchain (`wit-bindgen-*`, `wit-component`, `wit-parser`, `wasm-encoder`/`-metadata`/`-parser`, `wasip3`, `leb128fmt`, `id-arena`, `prettyplease`, `unicode-xid`) dropped out; `foldhash`/`hashbrown` shed a duplicate major. Nothing reaches the datapath, crypto, or wire.

## Upgrade notes

- **Breaking changes: none** on the wire, in the C ABI, or in the public Go/Python API. The Go fixes are internal lock-discipline changes behind unchanged signatures.
- **One behavioural fix to note:** `deck-ffi` stream functions now correctly return `END_OF_STREAM` on a closed stream for non-zero timeouts instead of a silent `OK`/NULL. A Go loop that previously spun on `(nil, nil)` will now terminate as documented.
- **Verification caveat:** this build environment has no cgo toolchain, so the Go module fixes were validated by `gofmt` + manual review + a pure-Go guard test, **not** a cgo compile/link. A cgo build on a release runner is the recommended gate before publishing the Go module tag.

**Full Changelog**: https://github.com/ai-2070/net/compare/v0.27.5...v0.27.6
