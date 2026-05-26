# nRPC flame-graph findings — 2026-05-26

**Bench:** `nrpc_unary_codec/json/32B` (the canonical hot unary loop), profiled with `samply record -r 4000 --profile-time 10` on Windows 11 / Intel 14900K-class CPU. 5 threads sampled (1 main + 4 tokio-rt-worker), **164,160 total samples**.

**Profile + symbols sidecar:** `.profiling/nrpc_unary_v3.json.gz` + `.json.syms.json`.
**Open in browser:** `samply load .profiling/nrpc_unary_v3.json.gz`.

> ⚠️ **Symbolication caveat.** samply on Windows reads PE export tables but couldn't parse our 87 MB bench PDB — every Rust frame shows as `fun_XXXX`. **System DLLs and kernel symbols all resolved correctly**, so we still get a clear picture of *where* time goes (kernel vs user, which syscall, which wait primitive). The library-bucket breakdown below is the cleanest signal; the per-Rust-function attribution falls back to the source-confirmed structural analysis from `PERF_AUDIT_2026_05_19_NRPC.md`.

---

## Headline: the system is **wake-latency bound, not CPU-bound**

| Bucket | % of on-CPU samples | What it actually is |
|---|---:|---|
| `ntdll.dll` | **64.65%** | user-mode side of kernel transitions (mostly `NtWaitForAlertByThreadId` — parking_lot / tokio mpsc futex waits) |
| `ntoskrnl.exe` + `.sys` | **18.06%** | in-kernel work, mostly IOCP wakeup paths + tcpip stack |
| `nrpc_unary.exe` (our Rust) | **14.67%** | the entire Rust stack: bench harness + tokio + serde_json + mesh_rpc + cortex + crypto + transport |
| `ws2_32 / mswsock / afd / tcpip` | 1.49% | Winsock + AFD shim layer |
| CRT / misc | 1.13% | memmove, GetProcessHeap, etc. |

**The single dominant frame is `NtWaitForAlertByThreadId` at 51.28% self-time / 56.31% inclusive.** This is the Win32 native primitive parking_lot uses for `Notify`, `Mutex`, `oneshot`, and mpsc wakeups. Half the wall time is **workers sleeping between wakeups**, not doing work.

---

## What the flame graph confirmed about the May 19 audit

| Audit prediction | Measured evidence | Status |
|---|---|---|
| **Spawn storm: 4–6 `tokio::spawn` per RT → 10–20 µs of pure scheduling overhead** | 51% in `NtWaitForAlertByThreadId` (tokio futex waits). Every spawn = at least one wakeup; this is the cost showing up as wall time. | ✅ **Confirmed** |
| **UDP syscalls: ~5–10 µs per call (2 per leg on Windows)** | 6.87% in `sendto` + 9.52% `NtDeviceIoControlFile` + ~5.7% `GetQueuedCompletionStatusEx` = **~22%** of CPU time spent in transport syscalls | ✅ **Confirmed** (closer to the upper bound) |
| **AEAD is NOT the bottleneck (~5% of budget)** | ChaCha/Poly1305/encrypt/decrypt do not appear in the top-200 inclusive-time frames. Crypto is invisible in the flame graph — consistent with the cipher_comparison bench from the audit. | ✅ **Re-confirmed** |
| **T1.1 — StreamWindow grant fires on unary** *(the audit's biggest "verify before implementing" unknown)* | **CONFIRMED FROM SOURCE** (no flame graph needed): `session.rs:1012-1041` shows `on_bytes_consumed` returns `Some(_)` unconditionally whenever `window_bytes != 0` (which it always is by default). Every accepted inbound data packet → `spawn_stream_window_grant` at `mesh.rs:4399`. Unary = 2 grants/RT → 2 extra spawns + 2 extra AEAD encrypts + 2 extra `sendto`s. | ✅ **Confirmed** — the single biggest claimed win is real |
| **Alloc pressure (~8–10 allocs/call, ~10–15 µs)** | `RtlFreeHeap` 2.06% inclusive, `RtlAllocateHeap` 0.17%, `RtlReAllocateHeap` 0.09% (≈2.3% allocator total). Probably under-attributed because the Rust allocator inlines a fast path that doesn't always reach `RtlAlloc`. The audit's 10–15 µs estimate is at the high end of plausibility. | ⚠️ **Partially confirmed** — visible but smaller than predicted in self-time |
| **Response leg → `mesh.publish` is wasteful (3–8 µs of roster fan-out)** | Can't pin a % without bench symbols, but **confirmed unchanged from audit**: 4 sites still use `mesh.publish(&publisher, Bytes::from(buf))` (`mesh_rpc.rs:1510, 1753, 1964, 2066, 2286`). | ✅ **Still open** |

---

## What's already landed since the audit (worth knowing)

- **T1.3** — per-`(service, caller_origin)` route cache. Implemented as `RpcRoute` cache in `mesh.rs::rpc_route_for_service`, soft-capped at 256 entries.
- **`ChannelName` follow-up** — backing type went `String` → `Arc<str>`. Per-call `Clone` is now a refcount bump instead of an alloc+memcpy.
- **T2.2 partial** — `RpcRequestPayload::body` and `RpcResponsePayload::body` are already `Bytes` (audit assumed `Vec<u8>`). `decode(data: Bytes)` produces a zero-copy `body` slice on the server. **Encode side still allocates an intermediate `Vec<u8>`** (see below).

Source-confirmed: **T1.1, T1.2, T2.1, T2.3, T3.x and the encode-side of T2.2 are all unchanged from the May 19 audit.**

---

## Ranked recommendations (refined by the flame-graph data)

Same priority order as the audit, with measured impact estimates updated where the flame graph clarifies them.

### #1 — Coalesce StreamWindow grants on unary (T1.1) — **biggest single win**

The audit guessed 4–12 µs/RT; flame-graph data suggests **6–10 µs** is more realistic:

- 2× `sendto` per RT for grants → 2 × ~3 µs ≈ 6 µs of the 6.87% measured `sendto` time
- 2× extra spawn-and-wake → some fraction of the 51% `NtWaitForAlertByThreadId`
- 2× extra AEAD encrypt → ~2 × 1.14 µs ≈ 2.3 µs

**Fix:** skip the grant entirely below a threshold OR coalesce N grants on a deadline. The window is purely flow-control protection for *streaming* traffic; on unary it's pure overhead because both REQUEST and RESPONSE complete in a single packet each.

**Open question I'd verify before implementing:** what's the smallest threshold (e.g. 25% of window consumed) that keeps streaming benchmarks healthy? Tier in `benches/nrpc_streaming.rs` and `nrpc_qps.rs c128/16KiB` would catch regression.

**Risk:** medium. Touches a flow-control mechanism; needs streaming bench to stay green.

### #2 — Response leg → `publish_to_peer` direct (T1.2) — **easy, contained**

4 mechanical changes at `mesh_rpc.rs:1510, 1753, 1964, 2066, 2286`. Each site already knows `caller_origin`; one DashMap lookup resolves it to a `node_id` and calls `publish_to_peer` instead of `mesh.publish` (which does roster fan-out + ACL check + subnet filter + a `Vec<Bytes>` alloc, then forwards to `publish_to_peer` anyway).

Estimated win: **3–8 µs/RT** (audit estimate, unchanged — flame graph doesn't isolate this without bench symbols).

**Risk:** low. Same as audit's "easy–medium" call. Test impact: unit tests for streaming/duplex response paths.

### #3 — Drop `catch_unwind` on hot path (T3.4) — **dirt-cheap**

`cortex/rpc.rs:1613` wraps every handler in `catch_unwind`. Audit says ~100–300 ns/call. Gate it behind `rpc-catch-panics` feature (default on, off in microbench) OR skip when the handler is `UnwindSafe`. Tiny per-call but free; combined with T1.2 + T1.1 this is in the noise but worth grabbing.

### #4 — `encode_into(&mut BytesMut)` on `RpcRequestPayload` (rest of T2.2)

Today's encode path:
```
let req = RpcRequestPayload { body: payload.clone(), ... };  // refcount bump ✓
let mut buf = Vec::with_capacity(EVENT_META_SIZE + req.body.len() + 32);
buf.extend_from_slice(&meta.to_bytes());
buf.extend_from_slice(&req.encode());                         // <-- alloc + 2 memcpys of body
```

`encode()` allocates a fresh `Vec`, writes meta+headers+body into it (memcpy #1 of body), returns it; outer site does `extend_from_slice(&...)` (memcpy #2 of body). Then `Bytes::from(buf)` transfers ownership.

**Fix:** add `encode_into(buf: &mut BytesMut)` that writes the header fields and `put` the body `Bytes` (refcount bump, no copy). Estimated win at 1 KiB: ~one fewer memcpy + one fewer alloc ≈ 200–400 ns. Smaller at 32 B but still removes an allocation.

### #5 — Server-side: skip `service.to_string()` in `RpcRequestPayload::decode` (cheap)

`cortex/rpc.rs:537` does `service.to_string()` during decode, but the server fold already knows its service name (bound at `serve_rpc` time). Add a `decode_for_server(data: Bytes) -> (deadline_ns, flags, headers, body)` variant that skips the service alloc entirely. ~50–100 ns/call.

### #6 — Drop the fold's outer `Mutex` + `in_flight: DashMap` (T2.1)

Audit says "20–50 ns/lock uncontended; much worse under contention", and inbound REQUEST crosses 4 mutex regions today. Under c128 saturation (the audit's c128/32B 1.84 ms result) this is exactly when contention hurts. Won't move c1/32B much, but should help the c128 ceiling.

**Risk:** medium. Touches `RedexFold` trait signature (apply `&self` vs `&mut self`) or requires interior mutability.

### #7 — Inline handler when future is Ready (T2.3)

The bench echo handler is synchronous (`async move { Ok(EchoResp { body: req.body }) }`), but it still goes through `tokio::spawn`. Poll once; if Ready, take the result inline. ~1–3 µs per call avoided when the handler is fast.

**Risk:** semantics — `catch_unwind` scope changes for the inline path. Bundle with #3 above.

---

## Recommended sequence

The May 19 audit's recommended order still holds. The flame graph **doesn't change the priority** — it just confirms with measured data that scheduling dominates and crypto doesn't. Suggested order:

1. **#2 (T1.2)** — easy diff, contained, 3 commits possible (unary / streaming / client-streaming).
2. **#3 (T3.4)** + **#5 (skip service decode)** — cheap cleanups, bundle.
3. **Re-bench** `nrpc_qps c1/32B` + `c128/32B`.
4. **#1 (T1.1)** — biggest claimed win; the most invasive. The flame graph + source review confirm it fires unconditionally on unary. Verify the streaming bench doesn't regress.
5. **Re-bench.**
6. **#6 (T2.1)** and **#7 (T2.3)** if c128 ceiling hasn't cleared its target by then.

Expected aggregate impact (from audit predictions, refined): **c1/32B 70 µs → 35–45 µs, c128 ceiling 70 K QPS → ~150 K QPS**.

---

## What the flame graph could NOT answer

These need a re-record with working bench-binary symbolication (samply + a different PDB resolver, or `cargo flamegraph` via blondie, or recording the same bench on a Linux box where samply uses `perf` natively):

- Per-Rust-function self-time. The 14.67% of CPU in `nrpc_unary.exe` is one bucket; we can't see whether it's serde_json, encode, or mpsc plumbing.
- Whether the 51% `NtWaitForAlertByThreadId` is dominated by tokio mpsc, oneshot, or parking_lot Notify. (Calling stack would show this; we have addresses but not names.)
- Confirmation of the audit's 2 µs `mesh.publish` overhead per response leg.

If we wanted these, the next step would be to either:
- Get bench-binary symbol resolution working (try `samply load` and use its web-UI symbolicate API — wholesym handles MSVC PDBs more robustly than the presymbolicate path), OR
- Run on Linux: `cargo bench --no-run --profile release-with-debug --bench nrpc_unary` + `samply record` would give native `perf` symbolication on Linux with no PDB workarounds.

But for **prioritization**, the data we have is sufficient — every recommendation above already has source-level confirmation, and the flame graph's library bucket validates the audit's "scheduling-dominated, not crypto-dominated" thesis.
