# SIMD crypto & global-allocator follow-ups

Follow-up investigation spun out of the 2026-06 benchmark review. Two
items were flagged from `BENCHMARKS_LATEST.md`:

1. Encryption "tops out around 400 MiB/s — for ChaCha20 that smells scalar."
2. The 256-byte allocation anomaly (256B slower than 1024B in two
   independent benches).

Both were investigated to root cause on Apple Silicon (M1 Max,
`aarch64-apple-darwin`). The short version: **neither is what it looked
like.** ChaCha20 is *already* running NEON; the crypto ceiling is the
Poly1305 MAC, which RustCrypto only SIMD-accelerates on x86. The 256B
anomaly is purely the macOS system allocator, and a fast global
allocator erases it.

---

## 1. Crypto throughput — it is not scalar ChaCha20

### What is actually running

The wire AEAD is `chacha20poly1305::ChaCha20Poly1305` (IETF, 12-byte
nonce; `src/adapter/net/crypto.rs`). Its two primitives select backends
independently:

| Primitive | crate | x86_64 backend | aarch64 backend |
|-----------|-------|----------------|-----------------|
| ChaCha20  | `chacha20` 0.10 | AVX2/SSE2 (runtime autodetect) | **NEON (compile-time)** |
| Poly1305  | `poly1305` 0.8  | AVX2 (runtime autodetect) | **soft / scalar** |

Key facts established by reading the crate sources and the host cfg:

- `chacha20` 0.10 selects NEON via
  `cfg(all(target_arch = "aarch64", target_feature = "neon"))` —
  **automatic at compile time, no `--cfg chacha20_force_neon` flag**
  (that requirement was removed in the 0.9→0.10 line).
- `rustc --print cfg` on this host emits `target_feature="neon"` (NEON is
  baseline on ARMv8). So the NEON ChaCha20 backend is already compiled in
  and active. **ChaCha20 is not scalar.**
- `poly1305` 0.8 gates its only SIMD backend on
  `any(target_arch = "x86", target_arch = "x86_64")`. On aarch64 it falls
  through to `backend::soft`. **There is no NEON Poly1305 in RustCrypto.**

So on Apple Silicon the cipher half is vectorized and the MAC half is
scalar — the MAC is the per-byte limiter.

### Evidence: the x86/ARM asymmetry

Same code, two machines, `net_encryption/encrypt` (per-byte asymptote at
4096B):

| machine | ChaCha20 | Poly1305 | encrypt/4096 |
|---------|----------|----------|--------------|
| 14900K (x86) | AVX2 | **AVX2** | **1.23 GiB/s** |
| M1 Max (aarch64) | NEON | **soft** | **401 MiB/s** |

~3× per-byte gap, entirely attributable to the missing ARM Poly1305
SIMD. Where both primitives are vectorized (x86) the existing code
already clears 1.2 GiB/s, so the implementation scales — the ceiling is
not the framing path, not `PacketBuilder`, and not ChaCha20.

(At 64B both machines are dominated by fixed per-call AEAD setup, not
throughput, so the small-payload numbers are not informative here.)

### Options, ranked

1. **Do nothing (recommended for now).** 400 MiB/s/core × N cores is well
   above current mesh packet rates; crypto is not on the critical path
   today. Revisit only if a bulk-transfer workload makes it one. The
   honest cost of being wrong here is low.
2. **Switch the wire AEAD to AES-256-GCM on ARM.** M1 has hardware AES +
   PMULL (`target_feature="aes"` is in the host cfg), so `aes-gcm` runs
   multi-GiB/s on aarch64. **But** this changes the wire cipher — a
   protocol/security decision requiring negotiation, a migration story,
   and a fresh security review. Large blast radius; not lean.
3. **Move the AEAD to an assembly-backed stack (`ring` / `aws-lc-rs`).**
   Keeps ChaCha20-Poly1305 on the wire (no protocol change) but uses
   BoringSSL-derived aarch64 assembly that vectorizes the MAC. Lifts the
   M1 ceiling toward the x86 number. Cost: a heavier, build-complex
   dependency replacing pure-Rust RustCrypto — a real tradeoff against
   the project's lean-dependency posture.
4. **Contribute a NEON Poly1305 backend to RustCrypto.** Correct
   long-term fix, keeps the pure-Rust stack, benefits the ecosystem.
   Largest effort; only worth it if crypto becomes a sustained ceiling.

**Recommendation:** option 1 until a workload makes crypto the
bottleneck, then option 3 (no wire change) before considering option 2.

---

## 2. The 256-byte anomaly is the system allocator

### Diagnosis (see also the `bench:` commits in this branch)

`net_event_frame/write_single` and `multihop_packet_builder/build` both
reported 256B slower than 1024B. The shared cost is **per-call
`BytesMut::with_capacity`**, not the write: `write_events` lowers to one
`memcpy` and is perfectly monotonic when the buffer is reused
(`write_single_reused`: 2.85 / 5.69 / 14.68 ns for 64 / 256 / 1024).

The crate sets **no `#[global_allocator]`**, so it uses macOS libmalloc,
whose "nano" zone services allocations ≤256B on a fast path. A 256B
payload plus its 4-byte length prefix (260B) spills out of nano into the
slower magazine zone, while 64B stays in nano — hence 64B fast, 256B
slow, 1024B (already in magazine) faster than 256B.

### Evidence: mimalloc erases it

Measured by temporarily wiring `mimalloc` as the bench-harness
`#[global_allocator]` (reverted after measuring), same machine/session:

| payload | system (libmalloc) | mimalloc | Δ |
|---------|--------------------|----------|---|
| 64B   | 18.6 ns | 8.9 ns  | −52% |
| 256B  | **48.8 ns** | **15.0 ns** | **−69%** |
| 1024B | 36.0 ns | 38.3 ns | ~flat |
| 4096B | —       | 84.6 ns | — |

The inversion disappears — mimalloc is monotonic (8.9 < 15.0 < 38.3 <
84.6) — and small allocations get 2–3× faster. This confirms the anomaly
is 100% the allocator, not our code.

### Options

- **Library crate (`net-mesh`): do NOT set a `#[global_allocator]`.** A
  library must not impose an allocator on the binaries that link it — it
  is the binary's choice, and forcing one would surprise/override every
  downstream consumer (including the Go/Python/Node FFI hosts).
- **Binaries (net's own CLI, and downstream services): opt in to mimalloc
  (or jemalloc).** This is where the win is realized. For any binary in
  this workspace that links `net` and runs on macOS/Linux, adding:
  ```rust
  #[global_allocator]
  static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
  ```
  flattens the 128–512B penalty for every per-call allocation path and
  speeds up small allocations across the board. Cheap, low-risk,
  reversible. Concrete candidates in this workspace, best first:
  `aggregator-daemon` (long-running, allocation-heavy) > `deck` /
  `net` CLI > `net-blob`. Short-lived CLIs benefit least.
- **Benchmarks: optionally standardize on a fast allocator** so bench
  numbers reflect a realistic deployment rather than libmalloc's
  nano-zone quirk — but document it, since it changes the absolute
  numbers (and silently "fixes" the 256B artifact).

**Recommendation:** leave the library allocator-neutral; add a
`#[global_allocator]` to the consuming binaries. The reusable production
escape hatch already exists for the hot path — `PacketPool` /
`ThreadLocalPool` reuse buffers and never hit per-call allocation, which
is why `net_encryption/encrypt` is monotonic regardless of allocator.

---

## Bottom line

Both flagged items were misattributed at the symptom level but real at
the system level:

- Crypto: ChaCha20 is already NEON. The ARM ceiling is scalar Poly1305 in
  RustCrypto; the same code does 1.23 GiB/s on x86 where the MAC is
  AVX2. No easy in-place SIMD win on ARM without changing the crypto
  stack. Defer.
- Allocator: the 256B anomaly is the macOS nano-zone boundary. mimalloc
  removes it (−69% at 256B) and speeds small allocs 2–3×. Realize it in
  the *binaries*, not the library.
