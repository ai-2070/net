# Net v0.27.3 — "Purple Rain"

## 🟣 Packet-path AEAD swapped to `ring`

The June-9 flamegraph concluded "~5% AEAD, nothing to do." A raw-AEAD decomposition bench revised that: **~700 ns of the ~975 ns fixed per-message cost** in the RustCrypto `chacha20poly1305` stack was `poly1305` 0.8's AVX2 backend re-deriving the `r¹..r⁴` key powers *per message* — paid on **seal AND open, on every packet**. `ring`'s assembly AEAD has both a lower fixed cost and a higher bulk rate.

`PacketCipher` now backs onto `ring::aead::LessSafeKey` behind the same method surface (`seal_in_place_separate_tag` / `open_in_place` map 1:1 onto the detached/in-place API and the wire's `ct||tag` layout). The wire format is unchanged — both implement RFC 8439, ciphertexts are byte-identical. `chacha20poly1305` stays a dependency for the `IdentityEnvelope` XChaCha sealed-box and as the cross-impl test oracle; the Noise handshake (cold path) is untouched.

Measured on i9-14900K, full `PacketBuilder::build`:

| Size | Before | After | Delta |
|---|---|---|---|
| **64 B** | 1139 ns | 222 ns | **−81%** |
| **256 B** | 1205 ns | 288 ns | **−76%** |
| **1 KiB** | 1585 ns | 544 ns | **−66%** |
| **4 KiB** | 3111 ns | 1538 ns | **−51%** |

`ring`: ~115 ns fixed + 0.31 ns/B (3.2 GB/s) vs RustCrypto ~950 ns fixed + 0.47 ns/B (2.1 GB/s) — wins at every size, both directions. The decrypt leg gets the same fixed-cost removal, so **per round-trip this shaves ~1.6 µs off every small packet** — unary nRPC, grants, acks, heartbeats. These are real, above-noise wins, not the below-loopback-floor µs items v0.27.2 landed on faith.

It also **retires one of v0.27.2's three parked levers.** The "crypto SIMD" item (rebuild x86-64 with `RUSTFLAGS="-C target-feature=+avx2"`, but a baked-in floor would `SIGILL` on pre-AVX2) is moot — `ring` dispatches to the best backend at **runtime**, so the AEAD fast path is on by default with no build-flag dance and no `SIGILL` risk.

---

## Boxing the cipher key regression fixed

Honestly told: the swap above bloated `PacketBuilder`. `ring` sizes its `UnboundKey` to the **largest** AEAD variant (AES-256-GCM key schedule + GHASH tables) — **544 bytes** — even though this path only ever holds a 32-byte ChaCha20-Poly1305 key. `PacketCipher` is embedded by value in every `PacketBuilder`, and builders move through the packet pool's `ArrayQueue` on every `get()`/`release()`. The inline key grew the builder **304 → 816 bytes**, so every pool pop/push memcpy'd ~2.7× more data — regressing the pure pool path ~70%, where **no crypto runs at all**. Pure struct bloat, caught before release.

The fix is one word: `cipher: Box<LessSafeKey>`. The pool now moves an 8-byte pointer and `PacketBuilder` is **264 bytes** — leaner than the pre-swap 304. The heap allocation is paid only on cipher construction (pool pre-fill / refill / rekey — all cold), never on steady-state reuse; the extra indirection inside seal/open is negligible against the AEAD.

| Operation | Before (post-swap) | After (boxed) |
|---|---|---|
| `net_packet_pool` get/return | 88 ns | 38 ns |
| `pool_comparison` shared_pool_10x | 820 ns | 355 ns |
| `pool_contention` fast_acquire_release | — | **−35..−40%** |

Net: recovered **past** the pre-swap baseline while keeping all of the encryption wins.

---

## The full-crate sweep — six subsystems

The sweep covered the whole crate in six parallel passes — core bus, mesh transport datapath, routing/nRPC/reliability, behavior/capability folds, RedEX/CortEX/state, and the Dataforts blob layer. Of **70 findings: 56 fixed-and-tested, 1 folded into another fix, 1 accepted by design, and 12 deliberately parked** (7 structural, 3 deferred, 2 partial). The full per-item resolution table is in the audit doc; the marquee items, by subsystem:

- **Dataforts store path (§6.1–§6.3).** BLAKE3 and Reed-Solomon no longer run inline on the tokio runtime (now offloaded via `spawn_blocking`/rayon); dedup hits compare **lengths** instead of re-reading + re-hashing the whole existing chunk (a 16 MiB dedup hit was costing a 16 MiB read + ~5 ms hash); chunk store is prehashed. Multiplicative win on dedup-heavy ingest.
- **Replication (§5.1, §5.2/§5.4, §5.5).** Leader catch-up is now bounded and **budget-gated before the read** (was O(N²) — read the whole backlog per request); replicated payloads thread through as `Bytes` end-to-end (removes 3 of 5 per-record copies); replica-apply fsync moved off the async worker via `spawn_blocking`.
- **nRPC / routing / reliability (§3).** `FairScheduler::dequeue` no longer allocates a `Vec` + walks the whole DashMap per packet (now an `ArcSwap` active-stream snapshot); latency histograms are non-cumulative (**14 contended atomic RMWs/RPC → 3**); the client stream-grant path coalesces through one drainer instead of spawning a task + reliable packet per chunk; the per-call reply-subscription check is off the process-wide mutex; the retransmit window trims in order.
- **Behavior / capability folds (§4).** `synthesize_capability_set` caches a change-generation-keyed `Arc<CapabilitySet>` instead of re-parsing tags on every call; fold primary store and inverted indexes moved off SipHash; the predicate planner no longer re-plans on every `evaluate()`; single-pass resource-axis extraction. *(The ~40 ns fold-index lookup itself is **by design** — it scales to millions of nodes — and is untouched. These target the re-parsing and re-allocation **around** the index.)*
- **Mesh transport datapath (§2).** O(1) `session_id` reverse index (was an O(peers) scan per routed-local packet, on the single receive task); in-place relay forward (no per-packet copy + `tokio::spawn`); `PacketBuilder` frames events directly into the packet buffer (eliminates the second full-payload memcpy per built packet).
- **Core bus (§1).** The global `SeqCst` in-flight counter is striped (was one cache line ping-ponged across all producers); bus stats derive from per-shard counters; dynamic-scaling metrics are subsampled; the FFI poll path splices raw event bytes via `RawValue` instead of parse-to-DOM-and-reserialize per event.

**Security-relevant aside (§3.8).** Call-id minting moved from a `getrandom` syscall per RPC to a thread-local pooled-entropy CSPRNG — a latency win, but the review pass also found the interim `SplitMix64` it briefly used had an **invertible public finalizer** (a callee could recover the PRNG state from one `call_id` and predict every future id on that thread). The shipped version uses pooled OS entropy; no `call_id` predictability in the release.

---

## The review pass

This keeps v0.27.2's discipline: after the 45 fix commits landed, every one was re-reviewed one-by-one (six parallel subsystem reviewers + a docs pass). **17 follow-up commits repaired 10 real bugs introduced by the fixes themselves**, added 40+ regression tests, dispositioned 11 external review-bot (cubic) findings, and fixed 2 CI gates. The bugs spanned stale-data, liveness, correctness, security, and one **data-loss**: §6.7's new `RedexFile` handle cache wasn't invalidated by `sweep_gc`, so a post-sweep re-store hit the stale idempotent path and **silently skipped the append**. All repaired before release.

Validation at the end of the pass: clippy clean under the default feature set **and** `--no-default-features --features {net, cortex}`; `RUSTDOCFLAGS="-D warnings" cargo doc` clean; **4,300+ lib tests green**; `dir_transfer`, `integration_cortex_*`, and `integration_redex` suites green.

---

## Capability queries — borrowed index buckets, and a misread corrected

Single-constraint capability queries (tag-only / state-only / region-only) now **borrow** the index bucket (`CandidateKeys::Borrowed`) instead of cloning it; composite queries still own. The work also corrected a stale read: the "2.56 ms, linear scan, sad at fleet scale" figure turned out to be measuring the cost of **returning half the fleet**, not the lookup. With a fixed-cardinality probe (`query_tag_rare` — exactly 100 matches at every fleet size), the indexed lookup is **3.0 µs at 50,000 nodes — flat from 5K to 50K** (a 50× fleet costs ~2× on a constant-cardinality discovery query). The borrowed fast path gave a real **−20% at 10K** on the half-fleet query; 1K/5K/50K moved within noise.

---

## Benchmark hygiene

- **Multi-producer ingest bench de-skewed — and its first numbers invalidated.** The new `EventBus::ingest_raw` multi-producer bench (the audit's first bench-coverage gap) initially cloned **one** `RawEvent` template; `RawEvent` caches its xxh3 routing hash, so every producer routed to a single shard — it measured shard-mutex contention, not the striped-counter layer it was written to expose. Fixed with a pool of 256 distinct templates, round-robined with per-thread stagger. **The numbers in the first commit are not comparable — re-baseline before drawing conclusions.**
- **Per-match throughput.** `query_tag` / `query_complex` now report cost **per match** (not per query), so the half-fleet queries read correctly: ~24 ns/match at 1K drifting to ~50 ns/match at 50K (cache-footprint drift).
- **New benches and captures.** `raw_ring/{64,256,1024,4096}` keeps the cipher-vs-cipher AEAD profile visible alongside the RustCrypto reference; `query_tag_rare` isolates index-lookup cost from result cardinality. Fresh i9-14900K and M1 Max benchmark sets were recorded.

---

## Breaking changes

**None on the wire, and none for honest peers.** There is no wire-format change anywhere in this release; mixed-version meshes interoperate (the AEAD swap is byte-identical RFC 8439, proven by cross-impl tests + the interop smoke). The SDK and FFI surfaces are unchanged.

One **build-time** note for packagers (no API/ABI change): `ring` is now a build dependency of the packet path.

- Native release targets — linux gnu x64/aarch64, macOS (both), win x64 — build `ring` routinely.
- Two jobs gain a **hard toolchain dependency** to verify on the next release run: the zig-as-musl-cross napi/CLI builds (`ring`'s C compiles through `zig cc`), and **aarch64-pc-windows-msvc** (`cc-rs` requires `clang` on the build host — GitHub Windows runners preinstall LLVM, but it is now a hard dependency of those jobs, not an assumption).
- Distributions that bundle compiled objects (wheels, npm prebuilds, FFI staticlibs, release binaries) should carry `ring`'s third-party license notice — **ISC-style, with BoringSSL / OpenSSL-derived portions**. The crate README now documents it.

---

## How to upgrade

**Drop-in.** Bump the dependency to `0.27.3`; no atomic peer roll, no config changes, no wire change.

1. **Nothing is required for honest peers** — the AEAD swap is invisible on the wire, and a mixed v0.27.3 / v0.27.2 mesh interoperates freely.
2. **The AVX2 `RUSTFLAGS` dance from v0.27.1/v0.27.2 is no longer needed for the AEAD.** `ring` auto-detects CPU features at runtime and picks the fastest backend; the flag is now harmless-but-unnecessary for crypto.
3. **Packagers:** see the build-time note above for the two cross-compile jobs (`zig cc` musl, aarch64-windows clang) and the `ring` license notice.

---

## Dependency updates

- **`ring`** (Rust crate → 0.17) — **new**; backs the packet-path AEAD. `chacha20poly1305` is retained for the XChaCha sealed-box and as the cross-impl test oracle.
- Routine maintenance otherwise, no behavioral surface change: **`pyo3-build-config`** → 0.29.0, **`sharp`** → 0.35.1, **`node`** → 24.x. `Cargo.lock` / `package-lock.json` carry the exact pinned versions.

---

Released 2026-06-12.

## License

See [LICENSE](../../LICENSE-APACHE). `ring`'s third-party notice (ISC-style, with BoringSSL / OpenSSL-derived portions) applies to distributions bundling compiled crypto objects — see the crate README.
