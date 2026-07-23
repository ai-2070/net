# Net v0.27.2 — "Purple Rain"

## A security release — one critical auth fix, and the nRPC wire path keeps shrinking

Where v0.27.1 was pure performance and *nothing on the wire moved*, v0.27.2 leads with a four-pass security audit of the `net` crate and the fixes it surfaced — headlined by a **critical authorization-bypass** in the capability fold — then continues the hot-path work on the nRPC dispatch layer the [hot-path audit](../misc/PERF_AUDIT_2026_06_09_HOT_PATH.md) opened. The full security review is recorded in [`docs/misc/SECURITY_AUDIT_2026_06_09_NET_CRATE.md`](../misc/SECURITY_AUDIT_2026_06_09_NET_CRATE.md); this log is the operator-facing summary.

The reassuring part first: **the audit found the crate unusually well-hardened.** Untrusted-wire parsing, token/chain auth, nonce/randomness handling, handshake identity binding, secret hygiene, and the filesystem/path surfaces all came back clean *and verified* (not assumed) — most classic hazards already had named, tested mitigations. One finding stood apart and is fixed below; the rest are medium/low hardening and defense-in-depth.

**Interop:** honest v0.27.1 peers are unaffected. The critical fix only *rejects forged input* that the old code silently trusted — a legitimate node always announced its own `node_id`, so nothing on the honest path changes. No wire-format change.

---

## 🔴 The critical fix — the capability fold now binds wire `node_id` to the verified signer

`SignedAnnouncement::verify` checked that an announcement carried a valid Ed25519 signature over a transcript that *included* `node_id` — but never that the claimed `node_id` was actually the signer's. The dispatch and apply layers then keyed all capability/reservation state on that attacker-supplied `node_id`.

The exploit chain (all four links confirmed against the code):

1. Peer A — legitimately authenticated via PSK + Noise — signs a `CapabilityMembership` envelope with **its own** entity key but sets the internal `node_id` to victim C's.
2. `verify` passes: it *is* a valid signature by A over those bytes; nothing required the node id to be A's.
3. `apply` installs the entry under key `(class_hash, C)` — a forged capability now lives in **C's** state (e.g. `tags:[nrpc:<service>], allowed_nodes:[A]`).
4. A calls the gated service; the callee gate reads `by_node[C]`, finds the forged entry, and returns `true`.

**Impact:** complete bypass of the per-node nRPC capability allow-list — any authenticated participant could invoke any capability-gated service on any node — plus global forge/overwrite/strip of other nodes' advertised capabilities (cap-stripping DoS, scheduler-placement poisoning). The **same unbound-`node_id` primitive** hit `ReservationFold`, enabling reservation/lock hijacking on behalf of arbitrary node ids.

**The fix** is exactly the one the audit prescribed — surgical, outsized impact: `verify` / `decode_and_verify` (and the reservation path) now reject any envelope where `ann.node_id != publisher.node_id()`, returning `WireError::NodeIdMismatch`. This closes capability injection and reservation hijack simultaneously. The check is effectively free — Ed25519 verification (~50 µs) already dominates every inbound envelope. Pinned by a full-dispatch multi-publisher regression test, and the `Fold::restore` trust invariant ("only restore from local snapshots") is now documented alongside it.

---

## FFI hardening — the aggregator handles join the crate's UAF protection

Two aggregator FFI handles (`RegistryClientHandle`, `FoldQueryClientHandle`) did an unconditional `drop(Box::from_raw(handle))` on free, lacking the `HandleGuard` every other opaque handle in the crate carries — so a caller racing `free` against an in-flight op (a pattern the handles' own docs invite) could deallocate the client out from under a live read. Closed:

- Both handles adopt the standard **`HandleGuard` + leak-on-free + `try_enter()`-gated ops** treatment, with quiesce-on-free, and **no longer hold the guard across the blocking RPC**.
- `net_registry_last_error_detail` / `net_fold_query_last_error_detail` now return a **caller-owned `char*`** (freed with `net_free_string`) instead of a pointer into a `Mutex`-owned `CString` a concurrent erroring op could free out from under the reader; `net.h` documents the ownership, and the **Go bindings free the returned strings**.
- Free now warns only on a *genuine* drain timeout (via `begin_free_detailed`), and a new-handle adoption checklist was added to `handle_guard` so the next FFI handle gets this by construction.

---

## Filesystem — symlink-escape closed in directory reconstruction, including the subtle FS bypasses

`fetch_dir` sanitized a symlink's *link path* via `safe_join` but wrote its *target* verbatim from the attacker-controlled manifest — the classic "symlink in an archive" exposure. `v0.27.2` rejects absolute / escaping symlink targets, and — crucially — closes the bypasses a naïve check misses:

- **Composed-link and symlinked-parent escapes** (a link whose escape only materializes through an earlier-reconstructed link).
- **Case- and normalization-insensitive FS bypasses** — default macOS APFS compares filenames both case- *and* normalization-insensitively, so the lexical traversal check now folds component **case** and applies **NFC** before comparing. (Reconstruction was already strictly ordered — dirs, then files, then symlinks last — so this was never a traversal *write*; the fix removes the residual risk to whatever later reads the tree.)

---

## Hardening grab-bag (medium/low, from the audit's backlog)

- **Constant-time secret compares.** `GroupId` (32-byte) / `SubnetId` (16-byte) bearer secrets now compare via `subtle::ConstantTimeEq` instead of derived `PartialEq` / `Vec::contains` (early-exit, data-dependent timing). Remote timing recovery of a 128/256-bit secret was already impractical; closed for completeness.
- **PSK config permissions.** The aggregator daemon now **warns** when its TOML config (which holds the mesh PSK) is group/world-readable — mirroring the `0600` discipline the CLI identity seed already enforces. The check runs *before* parse and warns on non-Unix too.
- **Cap-filters documented as advisory.** `subscribe_caps` / `publish_caps` are self-asserted matchmaking, **not** an access boundary — the real boundary is `require_token` + `token_roots` (root-anchored `TokenChain`). This is now prominently documented so no one mistakes a cap-filter for access control.
- **Fuzz coverage widened.** New fuzz targets for the **nRPC request decode**, **channel-membership decode**, **migration bindings decode**, and **blob-transfer header decode** — attacker-reachable, manually-hardened decoders that previously lacked a continuous regression guard. The fuzz crate gained the `bytes` / `postcard` deps and the `cortex` / `dataforts` features to reach them.

---

## Correctness — a node no longer expires its own capability entry (self-inflicted outage)

Surfaced while baselining the nRPC QPS bench (audit §15): capability fold entries carry a TTL (default 300 s) and the sweeper reaps them on expiry, but **nothing periodically re-announced the node's own entry** — `serve_rpc`'s announce is one-time. So **any node serving RPC continuously past one TTL (≈5 min) without re-announcing would start rejecting all inbound calls** (the callee-side cap gate finds no self-entry → `CapabilityDenied`) **and drop off peer discovery** — masked until now because every test/bench runs far under 300 s.

Fixed with a **periodic re-announce loop** (`spawn_capability_reannounce_loop`) that re-broadcasts the node's capabilities every `MeshNodeConfig::capability_reannounce_interval` (default 150 s), refreshing both the local self-index (callee gate) and peers' folds (discovery). Re-broadcasting needs an owned `Arc`, so `MeshNode::start_arc(self: &Arc<Self>)` stores a `Weak` the loop upgrades each tick; the SDK (`Mesh::start`) and FFI (`net_mesh_start`) call it. A bare `start(&self)` keeps its signature — no test-caller churn — and simply omits the loop. A/B tests pin both directions.

A follow-up review hardened the TTL math: because `announce_capabilities_with` rate-limits the network broadcast to `min_announce_interval`, a re-announce interval set *below* it would have let **peer** entries expire before the throttle released the next broadcast. The stamped TTL is now `2 × max(reannounce_interval, min_announce_interval)` — sized to the cadence peers actually see a refresh at, not the bare tick. (The local self-index is refreshed every call regardless, so only peers were ever at risk.)

A companion bench-harness fix bounds `call_*_retrying` backpressure retries with a 20 s `RETRY_DEADLINE`, so a saturated benchmark bar fails fast (`transport saturated … not measurable here`) instead of livelocking past a TTL.

---

## nRPC hot path — fewer wakeups, fewer allocations per round-trip

The audit's headline conclusion holds — the system is **syscall- and wakeup-bound, not compute-bound** (51% wake/scheduling, 22% transport syscalls, ~5% AEAD). v0.27.2 lands the contained, no-wire-change items that attack the wakeup count on the unary response leg:

- **§8a — one fewer `tokio::spawn` per response.** The response emit closure used to spawn a task for *every* response publish (~1–2 µs of scheduling on a wake-bound path). It now builds the wire payload synchronously and hands an `RpcResponseJob` to a single per-service **drain task** (the same drainer pattern as grant-coalescing) — bounded channel, drop-on-overflow, FIFO. Streaming/duplex variants keep their per-emit spawn; unary is the QPS hot path.
- **§8b — reply-channel name cached per caller.** `format!("{service}.replies.{caller_origin:016x}") + ChannelName::new()` was two heap allocs on every response, deterministic from `(service, caller_origin)`. Now cached alongside the response path's origin cache — a cache hit is an `Arc` bump.
- **T2.2 — `RpcPayload::encode_into`.** The encode path allocated a `Vec` then `extend_from_slice`'d it into the caller's buffer — a double copy. `encode_into(&mut buf)` writes once (≈200–400 ns saved at 1 KiB).

**Measurement honesty:** these are µs-and-below wins per round-trip — **below this dev box's Windows-loopback variance floor**. A before/after on `nrpc_qps` moved `c1/32B` ~−3.6% with the other bars inside noise; the authoritative measurement remains the Linux flamegraph environment, consistent with the rest of the wire-path audit. They are landed because they are correct, contained, and directly reduce the 4–5 wakeups/RT the flamegraph attributes 51% of CPU to — not because the dev-box microbench can prove the delta.

---

## Memory-amplification guard — and an unexpected speedup

A review of the §8b work flagged that the per-`serve_rpc` caller-keyed caches (the reply-channel cache and the response-path origin→node cache) were **unbounded** maps keyed by the **wire-claimed** caller origin — and the origin cache is populated *before* the capability gate. A single authenticated peer could spray distinct origins and amplify server memory without limit; "bounded by peer count" held only for well-behaved callers.

Both are now a single bounded **`OriginKeyedLru`** (`parking_lot::Mutex<lru::LruCache>`) capped at the legitimate active-caller working set per service (4096). Eviction is always safe — a miss just rebuilds the channel name or falls back to the roster lookup, never correctness.

The bound turned out to be **faster**, not a trade-off: `DashMap`'s hash-to-shard + per-shard `RwLock` buys nothing when one bridge/fold task owns the cache, and the LRU under an uncontended mutex skips it. Measured on the single-accessor workload `serve_rpc` actually produces:

| Operation | Before — `DashMap` | After — `Mutex<LruCache>` | Change |
|---|---|---|---|
| **hit** (per response) | 23.35 ns | 17.50 ns | **−25%** |
| **insert** (first time an origin is seen) | ~39.5 ns | ~30.7 ns | **−22%** |

So the hardening **caps memory *and* shaves ~6 ns off the per-response hot path.** A `before/after` micro-bench (`benches/origin_cache_bench.rs`) ships alongside it.

---

## Benchmark hygiene — corrections and a first capture

- **FailureDetector rows annotated** (audit §14). `failure_detector/check_all` (~342 ms) and `stats` (~80–100 ms) in `BENCHMARKS.md` are **benchmark-fixture artifacts of an O(nodes) scan reported per-element**, not hot-path costs — `check_all` runs once per `heartbeat_interval` (default 5 s) and costs ~204 µs at 5,000 nodes (~40 µs/s amortized); `stats` is observability-only. The genuine per-heartbeat costs are 14–242 ns. The rows now carry a warning so nobody chases them.
- **`bench_append_batch_disk` run for the first time** (audit §10). 64-event batches: **64 B → 20.2 µs/batch (~3.17 M ev/s)**, **1 KiB → 62.5 µs/batch (~1.02 M ev/s)** ≈ ~315 ns/event. The policy companion confirms the RedEX Phase 3/4 background-fsync design directly: `never`/default ~23 µs/batch vs `every_n_1` (synchronous fsync per batch) ~853 µs/batch — coalescing is **~40×** faster than per-batch fsync.

---

## What's deliberately parked (and why)

The audit's three highest-leverage levers are **identified, scoped, and intentionally not in this release** — each is gated on something this release isn't the place to spend:

1. **Recv-loop batching** (§1) — `recvmmsg` batched ingress is built through Stage 5 but stays default-off behind a Cargo feature + runtime flag, pending the c128 latency measurement (and a ~40-LoC channel-hop gap-fix that must land first, or the measurement understates the design).
2. **Ack-piggyback** (§2) — the one lever on the unary QPS ceiling (~70K → 150–200K target), but it's a **wire-format change** with cross-binding compat work; scheduled as its own effort.
3. **Crypto SIMD** (§3) — `RUSTFLAGS="-C target-feature=+avx2"` recovers 5–10× on the per-packet AEAD cost, but a baked-in floor would `SIGILL` on pre-AVX2 x86-64. The committed-config decision stands; the open question is per-artifact build flags for the published wheels/prebuilds.

---

## Breaking changes

**None on the wire, and none for honest peers.** v0.27.2 interoperates with honest v0.27.1 / v0.27.0 peers freely — the capability `node_id` binding only rejects *forged* envelopes (a `node_id` that doesn't match the signer) that no legitimate node ever produced; they now return `WireError::NodeIdMismatch` instead of being silently accepted.

Two **source/ABI-level** notes for FFI consumers (no in-tree non-test callers affected):

- **Aggregator error strings are now caller-owned.** `net_registry_last_error_detail` / `net_fold_query_last_error_detail` return a heap `char*` the caller must release with `net_free_string` (the bundled Go bindings already do). Previously they returned a borrowed pointer.
- The aggregator handle free path now leaks-on-free + quiesces (matching every other handle); double-free / free-during-op transitions to a logged drain warning rather than UB.

The SDK surface (`Mesh::start`, etc.) is unchanged — `start_arc` is wired internally.

---

## How to upgrade

**Upgrade promptly** — this release closes a critical authorization bypass. Bump the dependency to `0.27.2`; for the common case (Rust core + SDK) it is drop-in, with no atomic peer roll and no config changes. Notes:

1. **C/Go FFI consumers of the aggregator error-detail accessors** must free the returned string with `net_free_string` (or use the updated Go bindings, which do).
2. **Capability re-announce is automatic** — long-lived RPC servers now refresh their own entry every 150 s by default; tune via `MeshNodeConfig::with_capability_reannounce_interval` (set to `Duration::MAX` to disable). If you set it *below* `min_announce_interval`, the TTL is sized to the throttle, so peers stay live regardless.
3. **Optional, unchanged from v0.27.1:** rebuild the x86-64 target class with `RUSTFLAGS="-C target-feature=+avx2"` to unlock the AEAD fast path. Default builds are unchanged.

---

## Dependency updates

`subtle` is now pulled by the `net` feature (constant-time secret compares — already in the tree via the dalek stack). The fuzz crate gained `bytes` / `postcard` and the `cortex` / `dataforts` features for the new decode targets. Otherwise routine maintenance, no behavioral surface change: **`regex`** (Rust crate → 1.12.4), **`napi`** (Node binding → 3.9.1), and JS-side / docs bumps (**`better-auth`** → 1.6.16, **`next`** → 16.2.9). `Cargo.lock` / `package-lock.json` carry the exact pinned versions.

---

Released 2026-06-10.

## License

See [LICENSE](../../LICENSE-APACHE).
