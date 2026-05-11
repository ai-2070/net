# Code Review — `dataforts-feature` vs `master` (2026-05-11)

Review pass on the `dataforts-feature` branch (50 files, +10,984 / −33). The
branch implements the Rebel Yell phases of [`DATAFORTS_PLAN.md`](DATAFORTS_PLAN.md):
greedy-LRU dataforts (Phase 1), data-gravity heat counters (Phase 4),
read-your-writes (Phase 5), and the BlobRef + BlobAdapter foundation (Phase 3),
across Rust core + Python + Node + Go + C FFI.

Overall the pure-logic modules (greedy admission, heat counter, gravity policy,
blob_ref codec, WriteToken plumbing) are defensively coded and well-tested.
Risk concentrates in four areas:

1. **Operator-visible correctness gaps** in greedy (silent drops, missing
   withdraw on eviction, byte-accounting drift on reopen).
2. **Trust-boundary holes** in the blob adapter surface (no hash verification
   on store, channel-config-selected adapter executes attacker-shaped URIs,
   single-byte magic causes payload misclassification).
3. **RYW contract weakness** at the `wait_for_token` layer (folded ≠ applied,
   fold-task-stop returns `Ok(())`, in-process forgeable tokens).
4. **Cross-binding parity gaps** (Python async-adapter run-loop, Go cgo
   externs link-fail without `dataforts` feature, panic-across-FFI on
   token waits and blob calls).

Tagged `[B | H | M | L]`:

- B — blocker, fix before merge.
- H — correctness / security / API-shape issue worth fixing before merge.
- M — operator-visible footgun or robustness hole.
- L — hygiene, dead code, doc drift.

## Status

| ID    | Pri | Area         | Title                                                                       | Status |
|-------|-----|--------------|-----------------------------------------------------------------------------|--------|
| D-1   | B   | greedy       | Cluster-cap eviction never calls `withdraw_chain`                          | ✅ |
| D-2   | B   | greedy       | Bandwidth-budget rejection silently drops events with no retry             | ✅ |
| D-3   | B   | greedy       | `cache.upsert` does not subtract old `bytes` on reopen → `total_bytes` drift | ✅ |
| D-4   | B   | python blob  | `PyBlobAdapter::drive_if_coroutine` builds a fresh `asyncio.run` event loop  | ⏳ |
| D-5   | H   | greedy       | `chain_caps` resolves last-hop peer, not chain publisher                   | ⏳ |
| D-6   | H   | greedy       | TOCTOU on `is_new_channel` → duplicate file open / announce_chain          | ⏳ |
| D-7   | H   | greedy       | Unbounded `tokio::spawn` per inbound event                                 | ⏳ |
| D-8   | H   | greedy       | `colocation_target_held` hard-coded to `None`                              | ⏳ |
| D-9   | H   | gravity      | Default `origin_hash == 0` collapses per-chain heat into single bucket     | ⏳ |
| D-10  | H   | gravity      | `HeatRegistry` unbounded; no eviction wired to greedy LRU                  | ⏳ |
| D-11  | H   | gravity      | No inbound auth / origin / rate-limit on `heat:` tags                      | ⏳ |
| D-12  | H   | blob         | `FileSystemAdapter::store` never verifies bytes hash to `blob_ref.hash`    | ⏳ |
| D-13  | H   | blob         | Adapter selected by channel config, not URI scheme — authority confusion  | ⏳ |
| D-14  | H   | blob         | `classify_payload` first-byte `0xB0` collides with arbitrary binary payloads | ⏳ |
| D-15  | H   | blob         | No max-blob-size on resolve / `u64::MAX` size accepted                     | ⏳ |
| D-16  | H   | blob         | `BlobAdapter` trait is all-in-memory; no streaming hooks                   | ⏳ |
| D-17  | H   | RYW          | `wait_for_token` returns `Ok` on fold-watermark including skipped events  | ⏳ |
| D-18  | H   | RYW          | `wait_for_token` returns `Ok` on `running == false` (fold-task stopped)    | ⏳ |
| D-19  | H   | RYW          | `WriteToken` is forgeable plain data; `pub` fields + `pub const fn new`    | ⏳ |
| D-20  | H   | go binding   | cgo externs link-fail without `dataforts` feature in `libnet`             | ⏳ |
| D-21  | H   | FFI          | Panic across FFI on `net_*_wait_for_token` + blob publish/resolve         | ⏳ |
| D-22  | H   | FFI          | Vtable per-field null-check missing in `net_blob_register_adapter`        | ⏳ |
| D-23  | H   | FFI          | `timeout_ms == 0` silently rewritten to 1 ms in token wait                | ⏳ |
| D-24  | M   | greedy       | Wire `channel_hash` 16-bit → silent cross-chain pollution                  | ⏳ |
| D-25  | M   | greedy       | `gravity_tick` emits one full capset rebroadcast per chain                 | ⏳ |
| D-26  | M   | greedy       | `entry.bytes` saturating drift under retention trim                        | ⏳ |
| D-27  | M   | greedy       | `NIC_PEAK_BYTES_PER_S` hard-coded; no override on `GreedyConfig`           | ✅ (folded into D-2) |
| D-28  | M   | greedy       | 5 separate `cache.lock()` acquisitions per dispatch                        | ⏳ |
| D-29  | M   | gravity      | `should_emit_heat` `inf`-prone with near-zero `prev`                      | ⏳ |
| D-30  | M   | gravity      | Wire-side `(rate/(rate+1))` saturation at top end                          | ⏳ |
| D-31  | M   | blob fs      | `BlobError::NotFound(uri)` propagates raw attacker URI → log injection     | ⏳ |
| D-32  | M   | blob fs      | Concurrent stores race on shared `<hash>.tmp` filename                     | ⏳ |
| D-33  | M   | blob fs      | No `fsync` of temp file or parent dir before rename — durability gap      | ⏳ |
| D-34  | M   | blob reg     | Process-wide singleton registry; multi-tenant hijack possible              | ⏳ |
| D-35  | M   | blob adapter | No concurrency bound on `spawn_blocking` calls                             | ⏳ |
| D-36  | M   | blob conf    | Conformance suite shallow (no idempotency / range / mismatch / parallel)   | ⏳ |
| D-37  | M   | RYW          | "Wait queue" is `Semaphore::try_acquire` — not FIFO                        | ⏳ |
| D-38  | M   | RYW          | 1024 cap is per-adapter; no process-wide bound                             | ⏳ |
| D-39  | M   | FFI cortex   | `mesh_arc` drop duplicated per error branch — RAII guard wanted            | ⏳ |
| D-40  | M   | node blob    | `await_tsfn_promise` applies 30s timeout twice → 60s worst-case           | ⏳ |
| D-41  | M   | node cortex  | `DataGravityConfigJs.*_secs/_ms` u32 vs Python u64 vs Go uint64           | ⏳ |
| D-42  | M   | python blob  | `Py<PyAny>` adapters can outlive interpreter finalization                  | ⏳ |
| D-43  | M   | python blob  | `data.to_vec()` happens before GIL is released for large payloads          | ⏳ |
| D-44  | M   | go binding   | Greedy/gravity numeric fields can't express literal `0` via `omitempty`   | ⏳ |
| D-45  | M   | go binding   | No RYW surface — `wait_for_token` not exposed                              | ⏳ |
| D-46  | L   | greedy       | Heat normalization compression at the top end                              | ⏳ |
| D-47  | L   | greedy       | `metrics.rs` channel-cap race under contention                             | ⏳ |
| D-48  | L   | greedy       | `_force_use_hashmap` dead allow                                            | ⏳ |
| D-49  | L   | blob         | `BlobError` not `#[non_exhaustive]`                                       | ⏳ |
| D-50  | L   | blob redex   | `RedexFileConfig::blob_adapter_id` unset surfaces `UnsupportedScheme`     | ⏳ |
| D-51  | L   | RYW          | `wait_duration_nanos_sum` u128→u64 saturating cast                         | ⏳ |
| D-52  | L   | FFI blob     | `OpaqueCtx(AtomicPtr<c_void>)` unnecessary atomicity                       | ⏳ |
| D-53  | L   | node blob    | Adapter `timeout` not user-tunable                                         | ⏳ |
| D-54  | L   | go binding   | `runtime.SetFinalizer` runs blocking `Close` on GC thread                  | ⏳ |

## Findings

### D-1 — B — greedy/runtime.rs:550 — cluster-cap eviction never calls `withdraw_chain`

The cluster-cap eviction branch zeroes `bytes_resident` and bumps
`evictions_total`, but the inline comment (`// Note: we don't have the
origin_hash`) leaves the announce/`withdraw_chain` step as a TODO. The
`origin_hash` is in fact stored on `GreedyCacheEntry` (`cache.rs:54-58`); the
data is there, the path just isn't plumbed. Evicted nodes still advertise the
`causal:<hex>` tag and route reads to a `RedexFile` whose contents have been
truncated to zero — guaranteed misroute under the docs' own contract.

**Fix.** Return the evicted `origin_hash` out of `evict_oldest` / `evict` (as
part of an `EvictionSweep` struct), call `sink.withdraw_chain(origin_hash)`
inline. Test: pump two channels past `total_cap_bytes`, assert withdraw fires
+ `bytes_resident` zeros.

### D-2 — B — greedy/runtime.rs:467 — bandwidth-budget rejection drops events permanently

When `BandwidthBudget::try_consume` fails, the event is counted as a `Capacity`
reject and never retried — even if admission, scope, intent, colocation, and
the per-channel slot are all clear. Combined with the hard-coded
`NIC_PEAK_BYTES_PER_S = 125_000_000` constant (`runtime.rs:107`), any
deployment with > 1 Gbps NIC sees silent cache gaps that the operator can't
tell apart from a real admission rejection.

**Fix.** Either (a) skip the bandwidth gate for events that have already
passed all other gates (treat bandwidth as a soft hint, not a reject), or
(b) increment a distinct `admit_throttled_bandwidth_total` counter and
document the behaviour, or (c) couple with NIC-peak override on
`GreedyConfig` (see D-27). Recommended: (a) — bandwidth budget exists to
protect peers from runaway pulls; we're already running a per-channel and
per-cluster byte cap.

### D-3 — B — greedy/cache.rs:171 — `upsert` on update leaks `total_bytes`

`upsert` on an already-registered channel replaces `file` and refreshes LRU
but does NOT subtract the previous entry's `bytes` from `total_bytes`. The
insert branch zeroes `bytes`; the update branch does not. Any benign reopen
via `dispatch_event`'s `is_new_channel` path (D-6 is the realistic trigger)
accumulates `total_bytes` that no `evict` can ever drain — eventually the
cluster-cap budget reads "full" while disk reads near-empty, and every
admission rejects on `Capacity`.

**Fix.** Subtract `prev.bytes` from `total_bytes` before replacing the entry's
file, or zero `prev.bytes` and re-anchor on the next append.

### D-4 — B — python/src/blob.rs:300 — fresh `asyncio.run` per async adapter call

`drive_if_coroutine` calls `asyncio.run(coro)` from inside
`tokio::task::spawn_blocking`, building a *new* event loop on every adapter
invocation. Any user code that shares state with another event loop (an
`aiobotocore` client, an open `httpx.AsyncClient`, a SQLAlchemy async engine)
explodes with "attached to a different loop" or hangs forever. The contract
is also fragile: `asyncio.run` raises if the calling thread already runs an
event loop.

**Fix.** Document the contract: adapter coroutines run on a binding-owned
loop on a binding-owned thread; users who need to share state with their app
loop must use `asyncio.run_coroutine_threadsafe`. As a defensive measure,
build one binding-owned loop on a dedicated thread at module load and
schedule coroutines onto it via `run_coroutine_threadsafe` (futures observed
from the calling thread).

### D-5 — H — mesh.rs:3756 — `chain_caps` resolves last-hop peer, not publisher

`chain_caps` uses `ctx.addr_to_node[peer_addr]` to look up capabilities —
that's the last-hop session peer, not the chain's publisher. For any
relayed event, scope / intent / colocation admission is evaluated against
the relay's caps, silently inverting the semantics. The default `scopes =
Vec::new()` (config.rs:82) means empty-caps admits, so the visible failure
mode is "admits traffic that should have been rejected." The e2e test only
exercises the direct-publisher path.

**Fix.** Look up caps by the chain's `origin_hash` via the capability index
(query holders, take the publisher entry), or fall through to evaluation
against an explicit `chain_caps_override` field on the inbound event.

### D-6 — H — runtime.rs:482 — TOCTOU on `is_new_channel`

`is_new_channel = !cache.contains(channel)` (line 482) and the subsequent
`upsert` (line 497) are separate lock acquisitions. Two concurrent
`dispatch_event` calls on the same channel both observe `is_new_channel =
true`, both open a `RedexFile`, both call `sink.announce_chain`, and the
second `upsert` replaces the first `file` while leaving an orphaned
`RedexFile` open. Triggers the D-3 byte-accounting drift.

**Fix.** Fold lazy-open into a single `cache.get_or_insert_with`-style call
holding the cache lock for the whole contains/open/upsert/announce sequence
— announce can be issued *after* the lock is released, but the cache mutation
must be one atomic step.

### D-7 — H — runtime.rs:573 — unbounded `tokio::spawn` per event

`observe_event` fans out one `tokio::task` per inbound event with no
admission or queue-length cap. A flooding peer creates one outstanding task
per packet before the admission Mutex inside `dispatch_event` serializes
them. Memory grows with task-spawn fanout; the per-task `Bytes` and
`Arc<CapabilitySet>` clones pile up before admission runs.

**Fix.** Funnel through a bounded `mpsc::channel(N)` with `try_send` (drop on
full with a counter), or evaluate the admission gate synchronously inside
`observe_event` and only spawn on Admit. The latter is simpler.

### D-8 — H — runtime.rs:430 — `colocation_target_held` hard-coded `None`

`dispatch_event` unconditionally passes `colocation_target_held: None` to
the admission evaluator. `admission.rs:213` treats `None` as "target not
held," so any chain advertising `colocate-with` / `colocate-with-strict`
fails under `StrictRequired` and benefits nothing under `SoftPreference` for
the strict key. The colocation axis is dead-on-`False`.

**Fix.** Resolve the colocation target's `origin_hash` from the cache (by
hex hash equality) and pass `Some(target_held: cache.contains_origin(hex))`.

### D-9 — H — gravity/runtime.rs:335 — `origin_hash == 0` collapses per-chain heat

`note_read` keys the heat counter on `origin_hash` from the cache entry, but
the default `ChannelPublisher` doesn't stamp identity — every distinct chain
on a node with default publishers collides into the `origin_hash = 0`
bucket. The whole per-chain heat abstraction silently degrades to "one
global counter."

**Fix.** Either (a) refuse to install gravity unless publishers are
configured to stamp `origin_hash`, or (b) fall back to the cache-channel
synthesized name (which is already derived from `(origin_hash, channel_id)`)
when `origin_hash == 0`. (b) preserves the API; (a) is the correct contract.

### D-10 — H — gravity/counter.rs:141 — `HeatRegistry` is unbounded

`HashMap<u64, HeatCounter>` with no LRU, no cap, no tick-time pruning, no
hook into greedy eviction. Long-running nodes accumulate counters
indefinitely; `tick` is O(N) per heartbeat.

**Fix.** (a) Wire `remove(origin_hash)` into greedy eviction (D-1 plumbs
the origin out — same path can call `gravity.heat.lock().remove`). (b) Add
a cap with LRU-style replacement for counters that decay to zero without
emitting. (c) During `tick`, drop entries whose rate is < EPSILON and that
haven't been touched in N ticks.

### D-11 — H — mesh.rs:1055 — inbound `heat:` tags accept attacker payloads

There is no validation on inbound heat tags: no bound on how many a peer
can publish, no check that the peer holds the chain it's annotating, no
rate-limit on heat-tag churn from a single peer. A peer can publish
`heat:<arbitrary_hex>=0.99` for chains it has nothing to do with.

**Fix.** On inbound heat ingestion: (a) cap the number of `heat:` tags
indexed per peer (drop excess, count in metrics), (b) only accept
`heat:<hex>=...` when the same peer also advertises `causal:<hex>` (you can
only annotate heat for chains you claim to hold), (c) rate-limit heat-tag
delta-per-second per peer.

### D-12 — H — blob/fs.rs:67 — `store` does not verify bytes hash to `blob_ref.hash`

`FileSystemAdapter::store` writes whatever bytes the caller provides to the
hash-derived path, with no check that `blake3(bytes) == blob_ref.hash`. A
caller can pre-seed any hash slot with arbitrary content; a later honest
`BlobRef` resolves to attacker bytes. The conformance suite doesn't require
this either (D-36).

**Fix.** Hash bytes inside `store`, reject with `BlobError::HashMismatch` if
the computed hash doesn't match `blob_ref.hash`. Add a conformance test that
asserts every adapter rejects mismatched bytes-vs-hash.

### D-13 — H — dispatch.rs:57 — adapter selected by channel config, not URI scheme

`resolve_payload` and `RedexFile::resolve_one` route by the channel's
configured `blob_adapter_id`, NOT by the URI scheme on the inbound
`BlobRef`. An event payload's `BlobRef.uri = "s3://attacker/key"` is fetched
against whatever adapter the channel is configured with, with that adapter's
authority. A publisher with append rights can dictate paths the privileged
FS adapter then reads.

**Fix.** Adapters declare the set of URI schemes they accept (`fn schemes(&self) -> &[&str]`).
On resolve, validate `blob_ref.uri` scheme is in the bound adapter's scheme
list; reject otherwise. D-12's hash check is the final defense (attacker-shaped
URIs that happen to resolve to bytes still fail hash check), but scheme
validation closes the attack surface earlier.

### D-14 — H — dispatch.rs:43 + blob_ref.rs:98 — single-byte magic collides with binary payloads

`classify_payload` treats first byte `0xB0` as "is a `BlobRef`." Protobuf
wire bytes, MessagePack, and compressed payloads can start with `0xB0`. A
false match either fails decode (visible — `UnsupportedScheme`) or
succeeds decode (silent — fetches attacker URI).

**Fix.** Switch to a multi-byte magic (`0xB0 0xB1 0xB2 0xB3` or a 4-byte
`b"BRf1"`) that is genuinely improbable in arbitrary user bytes. Bump
`BlobRef` version byte on the wire to signal the change. Decoder rejects
the old single-byte form to avoid mixed-version confusion.

### D-15 — H — blob_ref.rs:117 + fs.rs:132 — `size` accepts `u64::MAX`

Wire decode accepts arbitrary `size: u64` with no cap. `fetch_range` does
`vec![0u8; len as usize]` — on 64-bit this OOMs; on 32-bit `as usize`
silently truncates and `read_exact` returns success on a shorter buffer
than the caller asked for.

**Fix.** Add `BlobRef::MAX_SIZE` constant (default 16 GiB). Reject larger
sizes in `BlobRef::decode` and `publish_blob`. Make `RedexFileConfig`
configurable for higher.

### D-16 — H — blob/adapter.rs — trait is all-in-memory

`fetch -> Vec<u8>`, `store(&[u8])`. Multi-GB blobs are impossible without
holding the full payload in RAM through every binding boundary. Adding
streaming variants later breaks every existing impl — the trait is the
long-term contract for adapter authors.

**Fix.** Add `fetch_stream(&self, blob: &BlobRef) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>`
and `store_stream(&self, blob: &BlobRef, src: Pin<Box<dyn Stream...>>)` as
required methods with default implementations that route through the
existing `fetch` / `store` (so existing impls keep working). Document the
escalation path: any adapter that wants real streaming overrides the
defaults.

### D-17 — H — cortex/adapter.rs:439 — `wait_for_token` returns Ok on skipped events

`wait_for_token` delegates to `wait_for_seq`, which returns when the
*folded* watermark passes `seq` — including events that `FoldErrorPolicy`
silently skipped via `RedexError::is_recoverable_decode`. A producer
whose write hit a skip gets `Ok(())` and then reads state that doesn't
reflect its write.

**Fix.** Add `folded_through_seq()` (already tracked) vs new
`applied_through_seq()` (events that actually ran through the fold). RYW
waits on applied, not folded. Document in `wait_for_token` rustdoc.

### D-18 — H — cortex/adapter.rs:438 — `wait_for_token` returns Ok on fold stop

`wait_for_seq` returns `Ok` when `running == false` (line 299-301). An
adapter that crashed under `FoldErrorPolicy::Stop` resolves every pending
RYW wait with `Ok(())` without ever folding `seq`.

**Fix.** When the wait wakes due to `running == false`, check
`folded_through_seq() >= seq`; if not, return new
`WaitForTokenError::FoldStopped`.

### D-19 — H — redex/write_token.rs:21 — `WriteToken` is forgeable

`pub` fields, `pub const fn new(any_u64, any_u64)`, `FromStr` implemented.
Any caller in-process can fabricate a token claiming any origin and any
seq. The doc says "treat as opaque"; the type API contradicts.

**Fix.** Make `version`, `origin_hash`, `seq` `pub(crate)`. Hide
constructor (`#[doc(hidden)]` on `new`). Remove `FromStr` from the public
API; gate it behind `#[cfg(test)]` or a `wire-debug` feature. Document the
threat model: tokens are unforgeable only against the adapter that issued
them (via origin binding).

### D-20 — H — bindings/go/net/redex.go:108 — cgo externs unconditional

The cgo extern block declares `net_redex_enable_greedy_dataforts` /
`net_redex_enable_gravity_for_greedy` unconditionally, but the Rust side
gates the symbols on `#[cfg(feature = "dataforts")]`. A `libnet.so` built
without the feature link-fails at Go program startup with `undefined symbol`.

**Fix.** Either (a) expose unconditional `extern "C"` stubs on the Rust
side that return `NET_ERR_FEATURE_NOT_BUILT` when the feature is off, or
(b) gate the Go binding under `// +build dataforts`. (a) is simpler and
keeps the Go API surface symmetric.

### D-21 — H — ffi/cortex.rs:1568 + ffi/blob.rs:217 — panic across FFI

`net_tasks_wait_for_token`, `net_memories_wait_for_token`, and the blob
publish/resolve entries run `block_on` without `catch_unwind`. A panic
from a user-installed adapter callback or from inside `wait_for_token`
unwinds across the FFI into C / cgo / Python — undefined behaviour.

**Fix.** Wrap every FFI entry's `block_on` body in
`std::panic::catch_unwind(AssertUnwindSafe(...))`; on `Err`, return a
distinct `NET_ERR_PANIC` code.

### D-22 — H — ffi/blob.rs:642 — vtable per-field null-check missing

`net_blob_register_adapter` checks `vtable.is_null()` (line 635) but does
not validate that each fn-ptr field is non-null. A partially-initialised
vtable crashes on first dispatch instead of returning
`NET_ERR_BLOB_BACKEND`.

**Fix.** After the outer null check, validate every function-pointer
field is non-null; return `NET_ERR_BLOB_VTABLE_INVALID` if any is null.

### D-23 — H — ffi/cortex.rs:1576 — `timeout_ms == 0` rewritten to 1 ms

`let deadline = Duration::from_millis(timeout_ms.max(1) as u64)`. A caller
passing 0 expecting "poll, don't wait" blocks ~1 ms; there is no way to
ask "is fold caught up yet?" without a real wait.

**Fix.** Honour `0` as a non-blocking poll: call
`adapter.folded_through_seq() >= token.seq` directly without scheduling a
wait. Document the contract.

### D-24 — M — runtime.rs:84 — 16-bit `channel_hash` collision

The wire `channel_hash` is 16 bits → 65,536 buckets. Two colliding
channels share one `RedexFile` and one metrics row. The comment
acknowledges this and points operators at "monitoring," but no collision
counter is surfaced.

**Fix.** Store the channel's true `ChannelName` alongside the synthesized
name when known; add `admit_rejected_collision_total` counter. If true
name is unavailable, log a one-time warning.

### D-25 — M — gravity_tick — N×announce_capabilities pathology

`gravity_tick` (`runtime.rs:367`) walks all heat emissions and calls
`announce_heat` per chain. `announce_heat` (`mesh.rs:6332`) does
`caps.tags.retain(...)` + `caps.tags.push(...)` followed by
`announce_capabilities` — which re-broadcasts the full capability set.
On a 100k-chain node: O(n² × n_tags) per tick, with each emit duplicating
all chains' tags on the wire.

**Fix.** Batch heat emissions: gather the list, do one `caps.tags.retain`
of all stale heat tags, push all new heat tags, then a single
`announce_capabilities`. Keep `announce_heat` for single-shot adjustment;
add `announce_heat_batch` for tick.

### D-26 — M — cache.rs:248 — saturating `bytes` drifts under retention trim

`entry.bytes` saturates on overflow but never reflects retention trim from
`RedexFile`. Over hours of a hot, retention-trimmed channel, the bound
diverges arbitrarily — eventually evicts on every append.

**Fix.** Periodic resync against `file.retained_bytes()`. Either tick-driven
(every 60s, walk active entries, refresh bytes) or on-eviction (when an
entry's `bytes` says it's at the per-channel cap, re-anchor from disk
before evicting).

### D-27 — M — runtime.rs:209 — hard-coded NIC peak

`NIC_PEAK_BYTES_PER_S = 125_000_000` (1 Gbps) regardless of actual NIC.

**Fix.** Add `GreedyConfig::nic_peak_bytes_per_s: Option<u64>` (None →
probe; fallback to current constant).

### D-28 — M — runtime.rs:430+ — 5 cache lock acquisitions per dispatch

Hot path takes `cache.lock()` at lines 482, 497, 506, 523, and (transitively)
in others. Under contention this becomes the bottleneck.

**Fix.** Coalesce into one `let mut cache = self.inner.cache.lock();` scope
after the budget gate; release before any `file.append`.

### D-29 — M — policy.rs:188 — `should_emit_heat` not robust to near-zero `prev`

When `prev` is tiny (1e-300) and `r` is finite, `prev / r` → 0 and `r / prev`
→ +inf, both satisfy `>= ratio`. The counter-side clamp mitigates but
`should_emit_heat` is documented pure.

**Fix.** Inside `should_emit_heat`, treat `prev < EPSILON` as `None`-equivalent;
the logic flows through the bootstrap arm cleanly.

### D-30 — M — runtime.rs:396 — wire normalization saturates

`(rate / (rate + 1.0)).min(1.0)` compresses asymptotically: rate=10 → 0.91,
rate=100 → 0.99. With `{:.2}` wire encoding, every "warm" chain looks like
"blazing."

**Fix.** Log-scale: `rate.ln_1p() / SCALE` with `SCALE` calibrated to
expected max rate (1000 events/s → 1.0). Or pass raw rate + saturation
point on the wire.

### D-31 — M — fs.rs:97 — log injection via `BlobError::NotFound(uri)`

The attacker-controlled `uri` is included verbatim in the error string.
Bindings that log errors get newline / ANSI / control-char injection.

**Fix.** Sanitize: replace control chars with `\xNN` escapes; cap length
at 256 bytes; reject `\r\n` outright in `BlobRef::decode`.

### D-32 — M — fs.rs:67 — concurrent stores race on shared `<hash>.tmp`

Same temp name in same shard dir; concurrent stores corrupt each other,
or (on Windows) the second `rename` fails.

**Fix.** Unique suffix: `<hash>.<pid>.<atomic_counter>.<nanos>.tmp`.

### D-33 — M — fs.rs:67 — no `fsync` of temp or parent dir

Power loss between `rename` and OS flush leaves zero-length or missing
files. The trait's "durability on store return" expectation is unmet.

**Fix.** `temp_file.sync_all()` before rename, `parent_dir.sync_all()`
after rename (or document the adapter as non-durable and route durable
adapters elsewhere).

### D-34 — M — registry.rs:63 — global singleton; no namespacing

Process-wide singleton; any code can replace any adapter id. Multi-tenant
binding hosts cannot isolate tenants.

**Fix.** Document explicitly that the registry is a single-tenant trust
boundary. (Per-tenant registries is a Phase 4 follow-up; not gating this
review.)

### D-35 — M — adapter.rs — unbounded concurrent `spawn_blocking`

A burst of stores fills the default 512-thread blocking pool and starves
RedEX writes / replication.

**Fix.** Add a per-adapter `Semaphore` with configurable bound (default 64);
acquire before `spawn_blocking`.

### D-36 — M — conformance.rs — suite is shallow

Missing: idempotency of same-hash double-store; `fetch_range` past-end;
overlapping ranges; bytes-vs-hash mismatch (the test that would catch
D-12); concurrency; cross-blob isolation. Ghost-hash is well-known
`[0xFE; 32]` (a stub adapter can hard-code it).

**Fix.** Extend the suite (separate commit; carry the new cases).

### D-37 — M — cortex/adapter.rs:421 — "wait queue" is not FIFO

`Semaphore::try_acquire_owned` admits whoever polls first and rejects the
rest with `QueueFull`. Hot caller starves cold caller at cap boundary.

**Fix.** Rename `ryw_wait_queue_cap` → `ryw_inflight_cap`; update doc.
(True FIFO requires switching to blocking acquire, which changes the
QueueFull semantics. Deferred — the naming fix is the immediate win.)

### D-38 — M — config.rs:35 — 1024 cap is per-adapter, not process-wide

Two thousand adapters × 1024 waiters = 2M concurrent `Notified` futures.

**Fix.** Document explicitly in `with_ryw_wait_queue_cap` rustdoc; defer
process-wide cap.

### D-39 — M — ffi/cortex.rs:506 — mesh_arc drop duplicated per error branch

`unsafe { drop(Box::from_raw(mesh_arc)); }` repeated in every error path;
easy to miss one.

**Fix.** Introduce an RAII `MeshArcOwned(*mut ArcMeshNode)` consumed by
`into_raw` on success.

### D-40 — M — node/blob.rs:567 — timeout applied twice in async path

`await_tsfn_promise` runs the 30s timeout on TSFN call AND on Promise
resolve → effective 60s.

**Fix.** Total-budget the two stages against a single `Instant::now() +
timeout` deadline.

### D-41 — M — node/cortex.rs +584 — width mismatch on gravity config

`tick_interval_ms: u32` (max 49 days) vs Python u64 vs Go uint64.

**Fix.** Widen to `BigInt`/`u64` for parity.

### D-42 — M — python/blob.rs:281 — `Py<PyAny>` adapters outlive interpreter

`Py::drop` requires the GIL. After interpreter finalization, `Drop` of an
adapter Arc in the registry panics/aborts.

**Fix.** Register a Python `atexit` hook that drains the global blob
registry while the GIL is still available.

### D-43 — M — python/blob.rs:217 — large `data.to_vec()` before GIL release

`data.to_vec()` runs inside `py.detach` scope? — check ordering; if the
copy happens with GIL held it blocks the interpreter for hundreds of ms
on big payloads.

**Fix.** Move `data.to_vec()` inside the `py.detach` closure.

### D-44 — M — go/redex.go:401 — `omitempty` swallows literal 0

`uint64` fields with `omitempty` cannot express literal `0`. Substrate
treats `0` as a real value, not "missing."

**Fix.** Switch to `*uint64` for fields where 0 is meaningful.

### D-45 — M — go/redex.go — no RYW surface

`wait_for_token` is exposed in C / Python / Node; Go binding has nothing.

**Fix.** Add `(t *TasksAdapter) WaitForToken(ctx, token, timeout) error`
and the Memories equivalent. (Tasks/Memories surface in Go is itself
deferred per the plan; this is acknowledged.)

### D-46 — L — see D-30 — heat normalization, log-scale

### D-47 — L — metrics.rs:191 — channel-cap race

Two threads can both pass `len() < cap` check and both insert. DashMap
shards bound the explosion to constant overshoot; correct the doc.

### D-48 — L — metrics.rs:419 — `_force_use_hashmap` dead allow

Remove or replace with `use std::collections::HashMap as _;`.

### D-49 — L — error.rs:9 — `BlobError` not `#[non_exhaustive]`

Future variants break exhaustive matches at FFI sites silently.

**Fix.** Add `#[non_exhaustive]`.

### D-50 — L — file.rs:1086 — wrong error variant on missing adapter

`UnsupportedScheme` for both "no `blob_adapter_id` configured" and
"configured id not in registry" — operators can't distinguish.

**Fix.** Add `BlobError::AdapterNotConfigured` and
`BlobError::AdapterNotRegistered`.

### D-51 — L — RYW wait_duration nanos u128→u64 cast

Theoretical overflow at >584 years.

**Fix.** `u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)`.

### D-52 — L — ffi/blob.rs:403 — `OpaqueCtx(AtomicPtr<c_void>)` unnecessary

The pointer is never mutated; atomicity buys nothing. Use
`struct OpaqueCtx(*mut c_void); unsafe impl Send/Sync`.

### D-53 — L — node/blob.rs:296 — adapter `timeout` not user-tunable

`const DEFAULT_TIMEOUT_MS = 30_000` with no override path.

**Fix.** Accept an options object with `timeoutMs: number`.

### D-54 — L — go/redex.go:325 — finalizer runs blocking Close on GC thread

`runtime.SetFinalizer(r, func(r *Redex) { _ = r.Close() })` — Close
calls into `wait_until_quiesced` which can block. Established Go pattern
is to expect explicit `Close()`; document the finalizer as a safety net.

## Commits planned

Each fix lands as its own commit (with tests where reasonable):

```
review doc: dataforts review tracking table
greedy fix: cluster-cap eviction calls withdraw_chain (D-1)
greedy fix: bandwidth-budget gate doesn't reject admitted events (D-2)
greedy fix: upsert subtracts old bytes on reopen (D-3)
python blob: shared binding loop for async adapter coroutines (D-4)
greedy fix: chain_caps resolves publisher via capability_index (D-5)
greedy fix: TOCTOU on new-channel insert (D-6)
greedy fix: bound observe_event spawn fan-out (D-7)
greedy fix: colocation_target_held resolved from cache (D-8)
gravity fix: refuse default origin_hash=0 for per-chain heat (D-9)
gravity fix: HeatRegistry eviction wired to greedy LRU (D-10)
mesh fix: inbound heat tag rate-limit + origin check (D-11)
blob fix: FileSystemAdapter::store verifies bytes hash (D-12)
blob fix: adapter declares URI schemes; dispatcher validates (D-13)
blob fix: multi-byte BlobRef magic (D-14)
blob fix: max blob size cap on decode + resolve (D-15)
blob: streaming hooks on BlobAdapter trait with default routing (D-16)
ryw fix: wait_for_token waits on applied_seq, not folded_seq (D-17)
ryw fix: FoldStopped error variant on fold-task stop (D-18)
ryw fix: WriteToken constructor doc-hidden + crate-visible fields (D-19)
go binding: cgo stubs when dataforts feature off (D-20)
ffi: catch_unwind on blob + RYW FFI entries (D-21)
ffi blob: per-field vtable null check (D-22)
ffi: timeout_ms==0 polls without scheduling wait (D-23)
greedy + gravity + blob + RYW: medium / low fixes batch
```
