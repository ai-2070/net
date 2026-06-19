# Net v0.27.6 — "Purple Rain"

## Bindings & integration hardening — a full-workspace bug hunt at the FFI edge

v0.27.6 is the substantive counterpart to the v0.27.5 version-stamp: a full-workspace bug hunt across the `net` crate (~100k LOC Rust) plus the Go / Python / FFI binding layers, recorded in [`docs/misc/BUG_AUDIT_2026_06_18_BINDINGS.md`](../misc/BUG_AUDIT_2026_06_18_BINDINGS.md). **34 of 37 findings are fixed and committed** across 47 commits and three automated review rounds on `bugfix/audit-2026-06-18`.

The headline finding: **every concrete bug in the first pass lived at the language-binding / FFI edge** — three use-after-free races in the *shipped* Go module (`github.com/ai-2070/net/go`), reachable by ordinary context cancellation. Two deeper passes then narrowed the "core is clean" framing: a missing FFI panic-guard in two binding crates, one **core data-path** HIGH (a reliable-stream sequence gap under backpressure), and a tier of behavior / meshdb / RedEX correctness fixes. The Rust core, identity/security, RedEX recovery, and the core `ffi/*` memory-safety pass all came back **clean and verified** — most classic hazards already had named, tested mitigations.

**No wire-format change, no C-ABI change, no public-API change.** The Go fixes are source-level inside the binding module (same method signatures, now race-safe). Honest v0.27.5 / earlier peers interoperate freely.

---

## Three use-after-free races in the shipped Go module

Three Go handle types — `RpcStream` (HIGH 1), `MeshOsDaemonHandle` (HIGH 2), and `MeshBlobAdapter` (HIGH 7) — guarded their native C handle with a **check-then-use** pattern (a bare `atomic.Bool`, or a mutex *dropped before* the cgo call) rather than a **claim-then-use** lock held across it. A concurrent free — the ctx-cancel watcher goroutine, an explicit `Free()`/`Close()`, or the GC finalizer once the handle becomes unreachable mid-call — could `drop(Box::from_raw(...))` the native object while a `Recv`/`Send`/`Store`/`NextControl` was parked inside `block_on`. The result is a dereference of freed Rust memory: **memory corruption / crash**.

This is reachable on the **documented happy path** — `CallStreaming(ctx, …)` then a `Recv()` loop with `ctx` cancelled mid-recv — not an exotic double-close. `MeshBlobAdapter` was the sharpest case: its own struct doc claimed it already serialized `_free` against in-flight ops, while the code dropped the lock before every cgo call.

**The fix** gives each handle a refcount **quiesce guard** (`streamHandleGuard`): ops bracket the cgo call with `enter()` / `leave()` *without* holding a lock, the free runs **once** and only after the last op leaves, and the free path never blocks. The design evolved across review rounds, and the evolution is worth recording:

- The first fix used an `RWMutex` held across the blocking cgo call — correct against the UAF, but it could **wedge `Close`/`Finish`/`Split`** on a deadline-less stream. Replaced with the non-blocking quiesce guard.
- A review round caught that `Split()`'s post-split halves (`DuplexSink`/`DuplexStream`) were left on the original bare-`atomic.Bool` pattern — the exact race #1 closed. Both now carry their own guard.
- A separate **GC use-after-free** surfaced in `meshos.go` `PublishLog`: it took `unsafe.Pointer(&msgBytes[0])` in an inner block that closed before the cgo call without a `runtime.KeepAlive`, so the GC could reclaim the backing array mid-call. Hoisted + `KeepAlive`, matching the sibling `PublishCapabilities`.

**Verification caveat (honest):** this build environment has no cgo C toolchain (`CGO_ENABLED=0`, no gcc), so the Go fixes are verified by `gofmt` + manual review + a **pure-Go unit test** for the quiesce guard (runs in CI), **not** a cgo compile/link. The patterns mirror already-compiling sibling handles.

---

## FFI panic guards for `rpc-ffi` and `compute-ffi`

The `rpc-ffi` and `compute-ffi` binding crates had **no `catch_unwind` at any entry point** and called tokio's raw `Runtime::block_on` (HIGH 8). `block_on` panics ("Cannot start a runtime from within a runtime…") when invoked from a thread already inside a tokio runtime, and any internal panic does the same — the unwind then crosses the `extern "C"` boundary into Go/cgo, which is **undefined behavior**. This narrowed the first pass's "panic-across-FFI catches all sound" note: true for the core `ffi/*` crates, but not these two.

Every `extern "C"` body is now wrapped in `ffi_guard!` / `catch_unwind`, and `block_on` routes through the abort-on-reentry wrapper the sibling FFI crates already use. Two review-round corrections went with it:

- The first pass *defined* the `ffi_guard!` macro in `compute-ffi` but **never invoked it** (P1) — so every one of the **80 entry points** still unwound across the ABI. Now wrapped everywhere.
- `net_compute_runtime_daemon_count`'s caught-panic default was `0` — itself a valid count — so a panic read as "0 daemons" success rather than the `-1` error sentinel the function uses. Default changed to the negative sentinel.

Companion structural hardening from the same family: Go `rpc-ffi` out-params (`write_response`/`find_service_nodes`) gained null checks (#30); the `len > isize::MAX` guard before `slice::from_raw_parts` was extended across the `*-ffi` crates (#31), with three copy-paste siblings the first sweep missed picked up in review.

---

## A reliable-stream sequence gap under backpressure

The one core data-path HIGH (#19, verified end to end). `send_on_stream` allocates a sequence number **atomically with the byte credit**, then builds, delivers, commits, and only afterwards registers the retransmit descriptor. For a **scheduled** stream, `deliver_stream_packet` has a *second* backpressure source — a full `FairScheduler` queue — surfaced as `Backpressure` **after** the seq was consumed. On that early return, `TxSlotGuard::drop` refunds the credit bytes (correct) but **never rolls back `tx_seq`**, and `register_retransmit` never runs — yet the packet was never put on the wire.

**Impact on a reliable stream:** a *permanent, unrecoverable gap* at the skipped seq. The receiver records the next packet out-of-order, never advances past the hole, and NACKs it forever; the sender's `on_nack(seq)` finds no descriptor and can't retransmit → eventual `failed` flag and a spurious `StreamReset`. Compounding it, any partial flush that *did* commit earlier in the same call is **re-sent under new seqs** on retry → duplicate delivery. This is the **documented backpressure path under bulk load**, not a rare edge.

Fixed by making the sequence refundable / rolled back on the failure path and not replaying already-committed events when `send_with_retry` re-enters. A review follow-up also bounded an **unbounded committed-prefix retry**: once the first packet of a multi-batch send commits, `flush_stream_batch` can't surface `Backpressure` (replay would duplicate), so it retried internally with no bound — a *stalled* receiver that never granted credit spun the sender forever. Now bounded by `COMMITTED_FLUSH_STALL_BUDGET` (30 s, the session-dead horizon): past it the peer is treated as dead and a terminal `StreamError::Transport` (which the caller does **not** replay) is surfaced. Paused-time unit tests pin both.

---

## The correctness tail — MEDIUM and LOW across behavior, meshdb, RedEX, and the FFI edge

The deeper passes turned up a tier of logic bugs away from the data path. Representative fixes:

- **meshdb executor (#20, #32).** LEFT/RIGHT OUTER join silently **dropped** preserved-side rows whose join key was missing/non-scalar (they never entered the build table, so the unmatched-emit loop never saw them) — violating OUTER semantics. `sort_merge_join` had the same no-key drop. Both now emit no-key preserved rows unmatched, matching `hash_join_full_outer`.
- **load balancer (#14, #15/#29, #33).** A half-open circuit probe slot could be **permanently claimed** if the caller skipped `record_completion` (and a `circuit_recovery_time_ms == 0` collapsed the breaker entirely — now clamped to ≥ 1 ms); `add_endpoint` re-add leaked / clobbered ~150 stale hash-ring vnodes (a destructive collision-probe `insert` overwrote another node's vnode); weighted-round-robin starved endpoints when all effective weights were < 1.0.
- **aggregator daemon (#10, #13).** A zero `summary_interval` **panicked** the spawned task (`tokio::time::interval(0)`), despite a comment claiming validation; `filter_novel` deduped on `fold_kind` only, re-publishing multi-row summaries every tick.
- **meshos reconcile / ICE (#9, #24, #28).** Duplicate `RequestEviction` for one chain per tick (the count arm wrote the dedup set but never read it); `MarkAvoid` re-emitted every tick; ICE `ThawCluster` was blocked by the cluster-wide cooldown, violating the break-glass invariant.
- **deck streams (#3, #4, #25).** `deck-ffi` reported genuine stream-end as a timeout for any non-zero timeout (`unwrap_or_default()` collapsed `Err(Elapsed)` and `Ok(None)` together) — **livelocking** the idiomatic Go polling loop; `AuditStream`/`LogStream`/`FailureStream` could **park forever** by not re-arming the waker after a consumed empty tick (now centralized in one helper); exported log timestamps printed an epoch hour-count instead of a 24h clock (missing `% 24`).
- **nRPC / routing (#26, #27, #34).** A duplicate in-flight `call_id` overwrote the prior caller's response sender (guarded only by `debug_assert`); `mint_random_call_id` returned `0` on `getrandom` failure, so concurrent failing calls evicted each other; a route owner couldn't update its own route to a *worse* metric, pinning a stale route until TTL.
- **RedEX (#21, #35, #36, #5, #18, #22).** Per-entry checksum covered the payload but not the header — review showed a corrupt `payload_offset`/`len`/`flags` is caught transitively (it reads the wrong region and fails the checksum), and only `seq` escapes, which is exactly why #21 added the seq-monotonicity walk (pinned by a test that corrupts *only* `payload_offset`); `OutstandingRequests`' soft cap only evicted expired entries (unbounded under sustained load) — re-backed with `lru::LruCache` for an O(1) hard bound; age-based retention assumed a monotonic wall-clock; plus a catch-up TOCTOU and a 32-bit overflow guard. **Federated query (#22):** a lost trailing `End` frame reported a fully-delivered result as `ExecutorError` — the sender now *always* emits a `final = true` terminal batch (even on an exact batch-size multiple) and the receiver again treats a missing terminal as a protocol error.
- **cortex FFI (#11, #16).** Five `(out_json, out_len)` functions skipped the documented out-param pre-zero contract (a TIMEOUT left a stale out-param); `net_rpc_duplex_into_split` dropped the surviving half on partial-consume.

**Validation at the end of the branch:** Rust changes are `cargo check`-clean (both `net-mesh` and `net-compute-ffi`) with `cargo clippy` clean and the touched modules' `cargo test` passing; regression tests were added per finding (existing tests that pinned buggy behaviour were updated). Go changes are `gofmt`-clean and mirror already-compiling patterns, with the cgo caveat noted above.

---

## Investigated, downgraded, and deferred

- **#37 — reported anti-replay `MAX_FORWARD` bypass → downgraded to INFO, reverted.** The control is dead on the hot path, but the window math means it is **not** an exploitable replay bypass. The proposed "restore `MAX_FORWARD` in `commit`" hardening breaks four existing replay-window tests that encode deliberate design: `commit` accepts large forward jumps so a receiver that missed > 1024 packets survives heavy loss without a forced re-handshake (stale counters are still caught by the age check). A behavior/policy change with a real reliability downside and no security gain — left to a deliberate decision rather than slipped into a bugfix release.
- **#23 — deferred.** `publish_to_peer` doesn't chunk by event count, so a `publish_many` of > 2028 events trips `build_subprotocol`'s release-mode `assert!`. The fix is a non-trivial hot-path refactor (per-chunk credit/seq loop) that deserves careful reliable-stream testing, not a rushed edit.
- **#12 — deferred.** `C.GoBytes(ptr, C.int(len))` truncates / sign-flips payloads ≥ 2 GiB across ~20 call sites; each needs bespoke error handling, and 20 blind edits without a cgo toolchain to compile-verify is too risky.
- **#17 — open sub-item.** The seed-pointer length check is the one piece of the FFI-guard family still open (its companion `isize::MAX` and panic guards landed); the real fix is a breaking C-ABI change (`seed_len` parameter), disproportionate for a LOW only reachable by a caller violating the documented 32-byte contract (in-tree callers always pass 32).
- **Appendix B-*** — bugs in the *divergent* `bindings/go/net/` copy are catalogued but not addressed here.

---

## Dependency updates

All in `net/crates/net/Cargo.lock` (no `Cargo.toml` change — so crates.io **library** consumers resolve identically; these bumps reach only the distributed artifacts: CLI, FFI staticlibs, npm prebuilds, Python wheels, deck):

- **Transitive bumps:** `redis` 1.2.2 → 1.2.3, `syn` 2.0.117 → 2.0.118, `napi` 3.9.1 → 3.9.3, `bytes` 1.11.1 → 1.12.0, `h2` 0.4.14 → 0.4.15, `time` 0.3.47 → 0.3.49, `getrandom` 0.4.2 → 0.4.3, `webpki-roots` 1.0.7 → 1.0.8.
- **Footprint reduction:** a transitive WASM component-model toolchain dropped out of the graph (`wit-bindgen-*`, `wit-component`, `wit-parser`, `wasm-encoder`/`-metadata`/`-parser`, `wasip3`, `leb128fmt`, `id-arena`, `prettyplease`, `unicode-xid`); `foldhash` and `hashbrown` shed a duplicate major.
- No crates added; nothing reaches the datapath, crypto, or wire.

---

## Breaking changes

**None on the wire, in the C ABI, or in the public Go/Python API.** The Go handle fixes are internal lock-discipline changes behind unchanged method signatures; the FFI panic guards and null/length checks are internal hardening. (#12 and #17 were deferred *precisely because* a real fix would require a breaking C-ABI change.)

One **behavioural** fix a consumer may notice: `deck-ffi` stream functions now correctly return `END_OF_STREAM` on a genuinely closed stream for non-zero timeouts, instead of a silent `OK` with a NULL out-param. A Go polling loop that previously spun on `(nil, nil)` forever will now terminate as documented — a fix, but worth flagging for anyone who built around the buggy behaviour.

---

## How to upgrade

**Go binding consumers should upgrade promptly** — the three use-after-free races are reachable on ordinary context cancellation of a streaming call, not an exotic path. For the common case (Rust core + SDK) it is drop-in: no wire change, no atomic peer roll, no config change. Rebuild any distributed artifacts (wheels / prebuilds / FFI staticlibs / CLI) to pick up both the fixes and the refreshed lock.

Note the verification caveat: the Go module fixes were validated by `gofmt` + manual review + a pure-Go guard test, but **not** cgo-compiled in this environment (no toolchain). A cgo build/link on a release runner is the recommended gate before publishing the Go module tag.

---

Released 2026-06-19.

## License

See [LICENSE](../../LICENSE).
