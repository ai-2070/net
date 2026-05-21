# Performance Analysis: Crypto, Session, Reliability

Supplemental to the unified report. Focuses on the per-packet wire-path code — crypto encrypt/decrypt, session state machine, reliability bookkeeping. Items continue from #127.

This is the layer that runs on every single inbound and outbound packet, on both sides. Any per-packet allocation here pays at packet rate, which is the highest rate in the system. Wins here also widen the UDP-vs-TCP architectural advantage (TCP's stack handles ack/retransmit in-kernel; you handle it in userland, so your per-packet code is the relevant comparison surface).

---

## ✅ Fixed

| # | Item | Notes |
|---|------|-------|
| 128 | RX path allocating `decrypt` per packet → `decrypt_to_bytes` in-place when refcount == 1 | New `PacketCipher::decrypt_to_bytes(counter, aad, ciphertext: Bytes) -> Result<Bytes, _>` tries `ciphertext.try_into_mut()` first. On success (refcount == 1, the common case for freshly-received packets — the `ParsedPacket` parser is the only holder), it decrypts in place via the existing `decrypt_in_place` and freezes back to a `Bytes` that shares the inbound buffer's allocation. On failure (shared buffer) it falls back to the legacy allocating `decrypt`. The two production RX call sites (`mesh.rs::process_local_packet` and `mod.rs::process_packet`) are wired through; `process_local_packet`'s signature changed from `&ParsedPacket` to `ParsedPacket` so we can `std::mem::take(&mut parsed.payload)` and hand the `Bytes` to the cipher by value (the refcount-1 invariant requires moving, not cloning). The downstream `EventFrame::read_events` lost its `Bytes::from(decrypted)` wrapper at 10 call sites — `decrypted` is now `Bytes` directly. Pinned by `decrypt_to_bytes_in_place_when_refcount_is_one` (asserts the returned plaintext shares the input's backing pointer + length shrinks by `TAG_SIZE`) and `decrypt_to_bytes_falls_back_on_shared_buffer` (asserts the allocating fallback path returns the correct plaintext when refcount > 1). |
| 130 | `PacketReceiver::recv` zero-fills the recv buffer per packet (~1500 byte memset) → tokio `recv_buf_from(&mut BufMut)` | The legacy `resize(MAX_PACKET_SIZE, 0)` + `recv_from(&mut [u8])` shape memset ~1500 bytes per packet just for the kernel to overwrite them on the next syscall — pure wasted memory bandwidth at packet rate. `recv_buf_from` writes into the `BytesMut`'s spare capacity (via `BufMut::chunk_mut`) so the kernel's bytes are the first writers and no pre-init is needed. Added `tokio/io-util` feature to expose the API. `clear()` + `reserve(MAX_PACKET_SIZE)` returns the buffer to length 0 while keeping the allocation; once steady-state the `reserve` is a no-op. Pinned by `packet_receiver_recv_must_use_recv_buf_from_not_resize_zero` — source-level invariant assertion since the wasted memset is observable only as a microbenchmark regression at runtime. |
| 138 | `PacketCipher::nonce_from_counter` builds nonce from scratch each call → pre-built `nonce_template` cached on the cipher | The legacy form did `let mut nonce = [0u8; 12]; nonce[0..4].copy_from_slice(prefix); nonce[4..12].copy_from_slice(counter)` per encrypt + per decrypt — the prefix bytes are session-stable so re-doing the copy on every packet is pure overhead. Now `PacketCipher` carries a `nonce_template: [u8; NONCE_SIZE]` filled at construction with the session prefix in `[0..4]` and zeros in `[4..12]`; `nonce_from_counter` (and `next_tx_nonce`) just `let mut nonce = self.nonce_template;` + writes the counter bytes. Both `new` and `with_shared_tx_counter` constructors materialize the template once. Saves the per-packet prefix memcpy (4 bytes) plus the zero-init the legacy form needed before `copy_from_slice` — small in absolute terms but fires twice per packet at the highest-frequency code path in the system. Pinned by `nonce_template_carries_session_prefix_with_zero_counter` (asserts the structural invariant: template's [0..4] matches `session_prefix`, [4..12] is zero, and `nonce_from_counter(N)` materializes a nonce whose [4..12] is N's little-endian bytes). |
| 131A | `BatchedTransport::recv_batch` zero-fills every slot per batch (~512 KiB) → `clear` + `reserve` + `unsafe set_len` | The Linux `recvmmsg` path setup loop ran `recv_buffers[i].resize(MAX_PACKET_SIZE, 0)` for each of MAX_BATCH_SIZE=64 slots — a ~512 KiB memset per batch call, repeated 10K+ times/sec on a high-pps node. Replaced with `clear()` + `reserve(MAX_PACKET_SIZE)` + `unsafe { set_len(MAX_PACKET_SIZE) }` so recvmmsg's iov_base/iov_len point at fresh-but-uninit capacity; the result-collection loop truncates each slot to its actual `msg_len` before any rust code reads through the frozen `Bytes`. Safety analysis in the inline comment: slots that don't receive a packet stay set_len'd until the next call, but the loop re-set_lens them before the next recvmmsg — uninit bytes past `msg_len` are never observed. Pinned by `batched_recv_must_use_set_len_not_resize_zero` (source-level invariant, since the file is `#[cfg(target_os = "linux")]` and the wasted memset is only observable as a microbenchmark regression). Issue B (per-packet 8KB allocation in the post-recv `std::mem::replace`) is deferred — it needs a buffer-pool design and is more invasive than the memset elimination here. |
| 129 | Heartbeat verify allocates a `Vec<u8>` plaintext only to drop it → new `PacketCipher::verify` API | `NetSession::verify_and_touch_heartbeat` used to call `rx_cipher.decrypt(...).is_err()` — for a heartbeat (16-byte tag-only payload) that materialized a 0-length `Vec<u8>` per call only to discard it. New `verify(counter, aad, ciphertext) -> Result<(), _>` runs the AEAD tag check via `decrypt_in_place` over a single scratch `BytesMut::with_capacity(ciphertext.len())` (one reserve, no `Vec`-from-`decrypt` allocation). Plaintext is dropped with the scratch. Pinned by `verify_admits_valid_tag_and_rejects_tampered` — exercises both the genuine-tag-valid and tampered-tag-rejected paths for a heartbeat-shape (16-byte) payload. |

---

## 🔴 High-impact

### 128. RX path uses allocating `decrypt` instead of `decrypt_in_place` on EVERY inbound packet

**Locations:** `mesh.rs:3394`, `mod.rs:710`, `session.rs:693`. The TX path correctly uses `encrypt_in_place` (`pool.rs:180`, `:267`, `:330`). The RX path doesn't:

```rust
let decrypted = match rx_cipher.decrypt(counter, &aad, &parsed.payload) { ... };
```

`PacketCipher::decrypt` returns `Result<Vec<u8>, _>` — a freshly allocated Vec per call. **Every received packet** does this allocation. For 1M pps with 1KB avg payloads, that's 1GB/sec of allocator churn just for plaintext buffers.

The plaintext is then wrapped: `Bytes::from(decrypted)` (mesh.rs:3433) — Bytes::from takes ownership of the Vec without re-copying, so the alloc cost stays at one per packet, not two. But that one is per-packet on the highest-frequency code path in the system.

**Fix paths:**

1. **`decrypt_in_place` on the inbound Bytes.** `parsed.payload` is a `Bytes` slice of the original packet buffer. Try `try_into_mut()` — succeeds if refcount = 1 (the common case for received packets). On success, decrypt in place; on failure, fall back to the allocating path.

2. **Pre-pooled scratch BytesMut.** Thread-local `BytesMut` reused across packets. Add `cipher.decrypt_into(counter, aad, ciphertext, &mut scratch)`. Scratch grows once to max packet size; subsequent decrypts overwrite. After decrypt, `freeze()` + `slice(..len)` gives a Bytes that owns its slot until dropped. Refill the pool on drop via Bytes::with_drop_in_place or similar.

3. **`decrypt_in_place_detached` already exists** in the underlying chacha20poly1305 crate (used by the TX path at crypto.rs:649). Wire the RX path to the same primitive over a pre-allocated buffer.

This is the single biggest per-packet allocation in the entire system on the receive path. Combined with #51 (HeapSegment::read zero-copy), the wire-to-fold path becomes nearly alloc-free.

### 129. `session.rs:693` decrypts a packet purely to verify the auth tag, then discards the plaintext

**Location:** `session.rs:685-702`. This appears to be a packet-validation path (probably a handshake or session-validation check). The code calls `rx_cipher.decrypt(...)`, checks `.is_err()`, and never uses the plaintext:

```rust
if self.rx_cipher.decrypt(counter, &aad, &parsed.payload).is_err() {
    return false;
}
```

The full plaintext is materialized in a fresh `Vec<u8>` and immediately dropped. For a 1KB packet that's a 1KB alloc + memcpy + free per validation, when **only the tag verification is needed.**

**Fix:** ChaCha20Poly1305's underlying RustCrypto trait exposes detached tag verification. Add `cipher.verify(counter, aad, ciphertext, tag) -> Result<(), _>` that runs the auth check without producing plaintext. Per validation: zero allocation.

This is a smaller-frequency path than #128, but is pure waste — the decrypted bytes are never used.

### 130. `PacketReceiver::recv` zero-fills the recv buffer per packet (~1500 bytes memset)

**Location:** `transport.rs:249-255`:
```rust
pub async fn recv(&mut self) -> io::Result<(Bytes, SocketAddr)> {
    self.recv_buf.resize(MAX_PACKET_SIZE, 0);     // <-- memset to zero
    let (len, addr) = self.socket.recv_from(&mut self.recv_buf).await?;
    self.recv_buf.truncate(len);
    Ok((self.recv_buf.split().freeze(), addr))
}
```

`BytesMut::resize(MAX_PACKET_SIZE, 0)` memsets the buffer to zero before each recv, but **the kernel is about to overwrite those bytes anyway**. For 1500-byte MTU at 1M pps, that's 1.5GB/sec of memset bandwidth wasted on zeroing buffers that get immediately overwritten by `recv_from`.

**Fix:** Use the uninit pattern:
```rust
unsafe {
    self.recv_buf.set_len(MAX_PACKET_SIZE);
}
// kernel writes valid bytes
let (len, _) = self.socket.recv_from(&mut self.recv_buf).await?;
self.recv_buf.truncate(len);
```

The `set_len` is unsafe because it claims initialized bytes that aren't, but the immediate `recv_from` initializes them. The window between `set_len` and `recv_from` is one syscall; nothing reads the buffer in between.

Alternative with the maybe-uninit API: `BytesMut::spare_capacity_mut()` returns `&mut [MaybeUninit<u8>]`, then `read_buf` writes into it. Same effect, no unsafe.

Saves 1.5GB/sec of memset on a 1M-pps deployment. This is a free win — no semantic change, no risk to the protocol layer.

### 131. `BatchedTransport::recv_batch` does the same zero-fill PER BATCH SLOT, plus allocates a fresh BytesMut per received packet

**Location:** `linux.rs:309-363`. This is the production-Linux receive path using `recvmmsg`. Two issues:

**Issue A (line 311):** Same as #130 — `recv_buffers[i].resize(MAX_PACKET_SIZE, 0)` per slot. With `MAX_BATCH_SIZE = 64` and `MAX_PACKET_SIZE` likely 8KB (the receive buffers are documented as "64 × 8KB = 512 KiB"), that's a **512KB memset per batch call**. At ~15K batches/sec on a 1M-pps node, that's 7.5GB/sec of pure memset bandwidth.

**Issue B (line 355-358):** After recv, the code does:
```rust
let mut buffer = std::mem::replace(
    &mut self.recv_buffers[i],
    BytesMut::with_capacity(MAX_PACKET_SIZE),    // <-- fresh 8KB alloc per packet
);
buffer.truncate(len);
results.push((buffer.freeze(), addr));
```

Per received packet, a fresh 8KB `BytesMut` is allocated to refill the slot. For 1M pps, that's 1M × 8KB allocator calls/sec = 8GB/sec of allocator churn.

**Fix paths:**

For Issue A: Same `set_len` / `spare_capacity_mut` fix as #130.

For Issue B: **Pool the BytesMut buffers.** When the frozen Bytes drops (i.e. the packet has been fully processed), return its underlying buffer to a pool that the batch loop draws from. Two implementations:

- Use `Bytes::from_owner` with a Drop impl that puts the buffer back. Complex but zero-copy.
- Maintain a fixed-size pool of `MAX_BATCH_SIZE × 2` BytesMut buffers. recv_batch takes from the pool; processors return when done. If the pool runs dry, fall back to fresh alloc (rare under steady-state).

Combined fix: per-batch work drops from "512KB memset + 64 × 8KB allocs" to "near zero on steady-state." On a 1M-pps node, this is the difference between 15GB/sec of wasted memory bandwidth and ~0.

### 132. `PacketCipher::is_valid_rx_counter` and `update_rx_counter` take the same Mutex separately per packet

**Location:** `crypto.rs:736-748`, called from `mesh.rs:3391` and `:3398`:
```rust
if !rx_cipher.is_valid_rx_counter(counter) { return; }   // lock 1
let decrypted = decrypt(...)?;
if !rx_cipher.update_rx_counter(counter) { return; }     // lock 2
```

Two Mutex lock + unlock pairs per inbound packet. The replay window is a single bit-tracking structure that's mutated infrequently relative to lookups, but the current API forces two locks.

**Fix options:**

1. **Single combined API:** `try_admit_rx_counter(received) -> Admit { Valid, AlreadyCommitted, OutsideWindow }`. Caller does it once after decrypt. The pre-decrypt check goes away — replays are rare in steady state, so paying decrypt cost on the occasional replay is cheaper than paying an extra lock on every non-replay packet.

2. **Lock-free replay window:** Common implementation is a sliding 128-bit window stored in an `AtomicU128` (or two `AtomicU64`s). Updates via CAS. Lookups are atomic loads. No Mutex on the hot path.

For 1M pps at ~5-10ns per parking_lot lock op, that's 10-20M lock ops/sec saved, around 50-200ms of CPU/sec on a single core.

### 133. `RetransmitDescriptor` is `Clone` and holds `events: Vec<Bytes>`; cloned per send AND per retransmit

**Location:** `reliability.rs:27-37`. The descriptor is stored in `pending: VecDeque<UnackedPacket>` per send (line 358), and cloned out per timeout via `unacked.descriptor.clone()` (line 469).

For a reliable stream sending batched packets (e.g. 10 events per packet), each pending entry holds a `Vec<Bytes>` of 10 entries. With `max_pending = 1000`, the pending queue holds 1000 such Vecs simultaneously, totaling ~10K Bytes refcount entries.

Each retransmit clones the Vec spine + bumps every Bytes refcount.

**Fix:** Wrap in Arc: `Arc<RetransmitDescriptor>`. `on_send` Arc-wraps once at insert. `get_timed_out` hands back Arc clones — one atomic refcount bump per retransmit instead of a Vec alloc + N Bytes bumps.

For high-throughput reliable streams under loss (where retransmit is hot), this is a meaningful reduction in alloc churn.

### 134. `try_acquire_tx_credit_inner` reads `epoch()` twice per credit acquire

**Location:** `session.rs:262-285`:
```rust
match self.streams.get(&stream_id) {
    Some(state) => {
        if let Some(expected) = expected_epoch {
            if state.epoch() != expected { ... }   // read 1
        }
        let admitted = state.try_acquire_tx_credit(bytes);
        let seq = if admitted { Some(state.next_tx_seq()) } else { None };
        (admitted, state.epoch(), seq)              // read 2
    }
}
```

`state.epoch()` is an atomic load. Two reads per credit acquire. Cache the first read.

Also: `Arc::clone(self)` in the guard construction (line 291) — one atomic bump per acquire. Necessary for the RAII guard pattern but worth noting per-send cost.

Per send: 4 atomic ops minimum (epoch×2, CAS in try_acquire_tx_credit, fetch_add on tx_bytes_sent, Arc clone) + 1 DashMap lookup. Modest individually but per-packet.

## 🟡 Medium-impact

### 135. `session.touch()` per inbound packet does a syscall (`SystemTime::now()` via `current_timestamp()`)

**Location:** `session.rs:701, 707-710`. Every successful packet verify calls `touch()` which calls `current_timestamp()` (likely `SystemTime::now`) and stores it.

Per inbound packet: one clock syscall (vdso usually, ~10-20ns) + atomic store.

**Fix:** Coarse-clock pattern (same as #33, #66, #115). Background ticker updates a shared `AtomicU64` every 1ms. `touch()` reads from the ticker. For session-timeout purposes, 1ms granularity is enormously more than enough.

At 1M pps, that's 10-20ms of CPU/sec saved on a single core just for the clock reads.

### 136. `BatchedTransport::recv_batch` re-zeroes `addrs[i]` and `msg_hdr` per batch slot

**Location:** `linux.rs:318-326`. Even with the buffer-zeroing fix from #131:
```rust
self.addrs[i] = unsafe { std::mem::zeroed() };
self.msgs[i].msg_hdr = unsafe { std::mem::zeroed() };
self.msgs[i].msg_hdr.msg_name = &mut self.addrs[i] as *mut _ as *mut _;
// ... 5 more field assignments
```

`recvmmsg` writes `msg_len` and `addrs[i]`'s relevant fields. The zero-fill is defensive (the docs aren't crystal-clear about what kernel touches), but the msg_hdr fields that get reassigned in the next 5 lines don't need to be zero first.

For `MAX_BATCH_SIZE = 64`, that's 64 sockaddr zeros + 64 msghdr zeros per batch. msghdr is ~56 bytes on Linux, sockaddr_in is 16 bytes. ~4.6KB of memset per batch.

**Fix:** Pre-initialize the static parts of `msgs[i]` once at construction (the iovec pointer + length pattern), only touch the dynamic fields per batch. `addrs[i]` doesn't need pre-zeroing — `recvmmsg` writes the meaningful bytes.

Smaller cost than #131 but in the same hot loop.

### 137. `ReliableStream::on_send` calls `Instant::now()` per reliable packet sent

**Location:** `reliability.rs:360`. Per reliable packet, a clock read. Same coarse-clock fix as #135.

For workloads that are mostly fire-and-forget this doesn't fire (FireAndForget's `on_send` is a no-op). For reliable streams it's per-packet.

### 138. `PacketCipher::nonce_from_counter` constructs nonce into stack array per packet

**Location:** `crypto.rs:625-630`. Two `copy_from_slice` calls into a 12-byte array. Cheap individually (~5-10ns), but it's per-packet on both encrypt and decrypt.

The nonce structure is `[prefix_4_bytes | counter_8_bytes_le]`. For repeated calls within a session, the prefix never changes. A nonce builder cached on the cipher could write only the counter bytes:
```rust
fn nonce_from_counter(&self, counter: u64) -> Nonce {
    let mut nonce = self.nonce_template;  // pre-built with prefix filled
    nonce[4..12].copy_from_slice(&counter.to_le_bytes());
    nonce
}
```

Saves 4 bytes of memcpy per packet. Microscopic individually but pure win.

### 139. `PacketCipher::encrypt_in_place` does `tx_counter.fetch_add(1, Relaxed)` per send

**Location:** `crypto.rs:644`. Counter is per-session, so contention is single-stream-per-session typically. For multi-stream-per-session deployments, this gets contended.

If the protocol allows it (which depends on nonce uniqueness requirements), shard the counter per stream. Otherwise, this is unavoidable — encryption fundamentally needs unique nonces.

Not a fix, just a note: this is one of the few items that resists optimization because it's load-bearing for correctness.

### 140. `BatchedTransport::recv_batch` allocates a fresh `Vec` for results per call

**Location:** `linux.rs:352`:
```rust
let mut results = Vec::with_capacity(received as usize);
```

Per batch call, fresh Vec. For 15K batches/sec, that's 15K Vec allocations/sec. Small but per-syscall.

**Fix:** Take an `&mut Vec` parameter so the caller reuses across batches. Or change the API to a callback: `recv_batch_with(max_count, |bytes, addr| { ... })` — no intermediate Vec needed; processors handle each packet inline as recvmmsg delivers it.

### 141. `parsed.payload` slicing on RX path doesn't try `into_mut`

**Location:** `transport.rs:213` (`data.slice(HEADER_SIZE..)`). The resulting Bytes has the original buffer's refcount, typically = 1 immediately after parse (only the slice holds the buffer at that point).

The RX decrypt path could try `parsed.payload.try_into_mut()` to convert back to BytesMut zero-copy. On success, decrypt in place. On failure (some other consumer cloned the Bytes), fall back to the allocating path.

This is the actual implementation of #128's "Fix path 1." Worth confirming refcount = 1 in practice via a runtime counter or test.

## 🟢 Low-impact / cleanup

### 142. `RxCounter::is_valid` and `commit` share state but live behind one Mutex

`crypto.rs:736-748`. Cleanup of #132 — if you go the single-API route, these inner methods can be replaced.

### 143. `NetHeader::aad()` and `to_bytes()` overlap in encoded fields

`protocol.rs:327, 356`. Same field-by-field memcpy pattern, just with different masks. Could share an internal encode-fields routine. Minor.

### 144. `PacketCipher::encrypt`'s allocating variant exists but TX path doesn't use it

`crypto.rs:660`. Only `identity/envelope.rs:263` uses it (cold path — identity envelope construction). Could be feature-gated or moved out of the hot crypto module to make it clear `encrypt_in_place` is the right choice.

### 145. `ReliableStream::get_timed_out` allocates a fresh `Vec<RetransmitDescriptor>` per call

`reliability.rs:464`. Timer-driven (not per-packet), so low frequency. Fine. Could pass a `&mut Vec` parameter for reuse if the timer rate is high.

### 146. `FireAndForget::on_receive` does `fetch_max(seq, Relaxed)` purely as informational

`reliability.rs:99`. The comment says "informational only." If truly unused except by metrics scrapers, gate behind a metrics-enabled feature so unreliable streams have zero per-packet atomic ops.

### 147. `Session` likely doesn't cache `node_id` (per cross-reference to discovery analysis #108)

Re-flagged from discovery analysis. The session knows which peer it's bound to at handshake time; subsequent inbound packets re-resolve that mapping. Cache on the session.

---

## What I'd actually do with these

**Top 3 wins on the per-packet RX path** (all unambiguous, all big):

1. **#128 — `decrypt_in_place` on RX.** Removes the single biggest per-packet allocation. Implementation: add `decrypt_into_bytesmut` to PacketCipher, plumb through the three call sites.

2. **#130 + #131 — kill the recv-buffer zero-fill on both transports.** Removes 1.5-7.5 GB/sec of pure memset bandwidth. Implementation: replace `resize(N, 0)` with `set_len` (unsafe) or `spare_capacity_mut` API. Trivial diff, no protocol semantics affected.

3. **#131 (continued) — pool the recv BytesMut.** Removes the per-packet 8KB allocation on the production Linux path. Implementation: bytes pool keyed to MAX_PACKET_SIZE, refilled on Bytes drop. More work than the others but biggest win.

These three together would substantially change the RX-path CPU + memory bandwidth profile. For a 1M pps deployment, easily 5-15% of total wire-path CPU.

**Followups worth doing** if those land cleanly:

4. **#132 — single combined replay-window API** (or lock-free window). Per-packet lock op.
5. **#133 — `Arc<RetransmitDescriptor>`.** Per-retransmit alloc.
6. **#135 + #137 — coarse clock for `touch()` and `on_send()`.** Per-packet clock syscall.

**Won't help much:** Most of the session-state machinery is already well-tuned. The atomic ordering in credit accounting (#134, #139) is load-bearing for correctness; you can't really save those.

---

## Honest expectation

The RX-path items (#128, #130, #131) are the kind of "you didn't know they were wasted because the system is fast enough you don't notice" wins. Implementing all three should:

- Drop RX-path allocator pressure by 80-90% (the alloc is currently dominated by these three sources)
- Drop RX-path memory bandwidth by gigabytes/sec on high-pps workloads
- Probably show as 5-15% improvement on the existing single-thread ingest benchmarks
- More if the benchmark machine was previously alloc-bandwidth limited

The reliability + session items (#133, #134, #135) are smaller wins — single-digit % each. The clock items (#135, #137) compound with the same items elsewhere (#33, #66, #115), so doing all of them at once via a "coarse clock everywhere" refactor is the right pattern.

For a system that's already at 5-15× gRPC, **the RX zero-copy work is where the meaningful headroom is.** Nothing else in the rest of my five-round audit is as clearly a "we're just paying a cost we don't have to" win as the recv-side allocations on the production batched transport.
