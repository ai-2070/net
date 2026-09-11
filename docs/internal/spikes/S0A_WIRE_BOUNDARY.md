# S0a — wire boundary spike

Stage 0 / S0a of
[`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
(§7, §Stage 0). Scratch crate: [`spikes/s0a-wire/`](../../../spikes/s0a-wire).
Throwaway evidence for Stage 2, not production code.

**Result.** The seven wire modules plus the routing-envelope codec compile
for `wasm32-unknown-unknown` with tokio and the `net` crate cut out, and a
routed handshake/envelope round-trip runs through the extracted code.

```
$ cargo check --target wasm32-unknown-unknown      # exit 0
$ cargo test
test an_envelope_addressed_elsewhere_is_refused ... ok
test routed_envelope_round_trips_through_the_extracted_wire_code ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

---

## 1. Source commit

All sources copied from **`4b52454a19758205189220339135e7b7e432b24e`**
(branch `LZL0/webrtc-transport`), working tree clean apart from `spikes/`.

Copies are verbatim except for the changes enumerated in §2 and §6; every
divergence was produced by a scripted rewrite and diffed back against the
original file, so the list below is exhaustive, not a summary.

## 2. What must move into `net-wire` for Stage 2

### 2.1 The eight named items (moved, compile clean)

| Source | Spike file | Notes |
|---|---|---|
| `adapter/net/protocol.rs` | `src/protocol.rs` | verbatim |
| `adapter/net/crypto.rs` | `src/crypto.rs` | **one real change** — AEAD backend seam, §6.1 |
| `adapter/net/pool.rs` | `src/pool.rs` | verbatim + import paths |
| `adapter/net/batch.rs` | `src/batch.rs` | verbatim + import paths |
| `adapter/net/stream.rs` | `src/stream.rs` | verbatim; 3 doc-links to core types de-linked |
| `adapter/net/reliability.rs` | `src/reliability.rs` | import paths + `Clock` seam |
| `adapter/net/session.rs` | `src/session.rs` | import paths + `Clock` seam + the two cuts in §3 |
| `route.rs:1-319` (codec only) | `src/route_codec.rs` | `ROUTING_MAGIC`, `ROUTING_HEADER_SIZE`, `RouteFlags` (+ `NONE`/`CONTROL`/`REQUIRES_ACK`/`PRIORITY`/`END_OF_STREAM`, `from_u8`/`as_u8`/`contains`/`is_control`/`is_priority`), `_MAX_TTL`, `RoutingHeader` and `new`/`control`/`priority`/`to_bytes`/`from_bytes`/`write_to`/`write_at`/`read_from`/`is_expired`/`forward` |

Left behind in core from `route.rs`, as §7 requires: `RouteEntry`,
`RoutingTable`, `next_hop`, the metrics, and `SchedulerStreamStats`.

### 2.2 Extra files dragged in transitively (NOT in §7's list)

These are not optional — the eight above do not compile without them.

| Must move | Pulled in by | Size | Comment |
|---|---|---|---|
| `adapter/net/subnet/route_hop.rs` — **the whole module**, not just `SharedHopReplayWindow` | `session.rs:19,223-266` | 1005 lines | §7 says "move the window type". Insufficient: `NetSession` also calls `seal`, `seal_into`, `open`, `sealed_len` and names `OpenedHop`, `RouteHopError`. Adds a `blake2` + `subtle` dependency to `net-wire`. |
| `ParsedPacket` (`adapter/net/transport.rs:334-376`: struct, `parse`, `expected_payload_len`, `is_valid_length`) | `session.rs:27`, `NetSession::verify_and_touch_heartbeat` | 43 lines | The *rest* of `transport.rs` (`NetSocket`, `PacketSender`, `PacketReceiver`, `BatchedPacketReceiver`) is tokio/UDP and stays. Splitting the file is a Stage 2 task. |
| `current_timestamp()` + `coarse_clock_advance()` + `COARSE_CLOCK_REFRESH_NS` (`adapter/net/mod.rs:281,338`) | `session.rs:1818`, called on every packet | 3 items | Currently `pub(crate)` in `adapter::net`. Wall-clock and monotonic reads both go through the `Clock` seam (§4). `current_timestamp_micros` was **not** needed by the wire surface and stays. |
| `StoredEvent` (`src/event.rs:454`) | `session.rs:16` | struct + 2 ctors | See §3.1. |

### 2.3 New code the spike had to write (Stage 2 must write it too)

| File | Why |
|---|---|
| `src/clock.rs` | The `Clock` trait + `SystemClock` + the `Instant`/`SystemTime`/`UNIX_EPOCH` target aliases (§4). |
| `src/aead.rs` | Packet-AEAD backend seam: `ring` natively, `chacha20poly1305` on wasm32 (§6.1). |

### 2.4 What was NOT brought along, and is therefore unproven

Listed so nothing is hidden:

- **Every `#[cfg(test)] mod`** in the copied files was deleted (`protocol` 313
  lines, `crypto` 1060, `pool` 787, `batch` 154, `reliability` 1398,
  `session` 1647 + the `heartbeat_api_drift_check` tripwire, `route_hop` 433).
  They reach into `crate::adapter::net::subprotocol::stream_window`
  (`reliability.rs:2494`), `serde_json`, and the drift-check scans
  `mesh.rs`/`mod.rs` source text — all core-side. **Stage 2 must decide where
  each test module lands; this spike proves nothing about them.** The
  `heartbeat_api_drift_check` tripwire in particular cannot move as-is: it
  greps `mesh.rs` and `mod.rs`, which will be in a different crate.
- `StoredEvent::from_value` (its only `serde_json` user) — left in core.
- `adapter/net/subprotocol/*` — the wire-level subprotocol codecs §7 mentions.
  **Nothing in the seven modules' production code references them**, so
  none were needed to compile; the only reference is from a `reliability.rs`
  *test*. Their move is therefore driven by the leaf dispatcher's needs
  (§7's `0x0A00`/`0x0B00`/`0x0C00`/`0x1000`/`0x0D02` list), not by a compile
  dependency, and S0a supplies no evidence about them.
- No stubs, no `todo!()`, no no-op shims anywhere in the crate. The two
  new files in §2.3 are complete implementations on both targets.

## 3. The two `session.rs` couplings

### 3.1 `crate::event::StoredEvent` (`session.rs:16`) — **move the type**

`NetSession` uses it in exactly three places: the field
`inbound: SegQueue<StoredEvent>` (`:1113`), `push_event` (`:1703`), and
`pop_event` (`:1709`). It never reads a field. The struct itself is a pure
data carrier (`String`, `Bytes`, `u64`, `u16`, `Option<String>`) with no
dependency on the adapter machinery around it.

Rejected alternative — *move the dependency*, i.e. make the queue element a
type parameter (`NetSession<E>`) or a trait object. It is the "purer" cut, but
`NetSession` is named at hundreds of core sites; a type parameter ripples
through every one of them for a queue whose element the session never
inspects. Moving a 5-field struct and re-exporting it from `crate::event`
costs one `pub use` and keeps `NetSession` non-generic.

**Stage 2 action:** move `StoredEvent` into `net-wire`, re-export as
`crate::event::StoredEvent`. Leave `from_value` (the `serde_json`
convenience ctor) in core as an inherent impl, or accept `serde_json` in
`net-wire` — the spike dropped it and did not need it.

### 3.2 `subnet::route_hop::SharedHopReplayWindow` (`session.rs:19`) — **move the dependency (all of it)**

§7 assumed only the window type was entangled. It is not:
`NetSession::seal_route_hop_into`/`seal_route_hop`/`open_route_hop`
(`session.rs:223-266`) call `route_hop::seal_into`, `seal`, `open`,
`sealed_len` and name `OpenedHop` and `RouteHopError` in their signatures.
Moving only `SharedHopReplayWindow` would leave `net-wire` depending on
`net` — a cycle.

So the whole `subnet/route_hop.rs` module moves. It is self-contained: its
only non-dependency import is `route::{RoutingHeader, ROUTING_HEADER_SIZE}`,
which moves in the same slice, and it is wire-format code by nature (a keyed
BLAKE2s MAC over the hop envelope). Cost: `net-wire` gains `blake2` and
`subtle` dependencies. `net::adapter::net::subnet::route_hop` re-exports from
`net-wire` so `SUBNET_AUTH_PLAN` call sites are untouched.

*Leaf relevance:* a non-forwarding leaf never generates a hop MAC, but it
does sit behind gateways that do, so the module cannot simply be excluded
from the wasm build — and it compiles for wasm32 as-is.

## 4. `Instant` / `Clock` site count

`src/clock.rs` defines `trait Clock { fn now() -> Instant; fn now_unix_nanos() -> u64 }`
with `SystemClock` as the platform impl, and target-selected aliases
(`std::time::Instant`/`SystemTime` natively, `web_time::Instant`/`SystemTime`
on wasm32).

**13 sites across 3 files:**

| File | `Instant::now()` → `SystemClock::now()` | `Instant` in a type position | wall-clock read |
|---|---|---|---|
| `reliability.rs` | 5 (`:748`, `:867`, `:882`, `:943`, `:1033` in the original) | 1 (`RetransmitDescriptor::sent_at`) | — |
| `session.rs` | 3 (`:798`, `:844`, `:882`) | 1 (`recently_closed: DashMap<u64, Instant>`) | — |
| `time.rs` (from `mod.rs:281`) | 1 | 4 (thread-local cell + `coarse_clock_advance`'s 3) | 1 (`SystemTime::now()` → `SystemClock::now_unix_nanos`) |

`crypto.rs`, `pool.rs`, `protocol.rs`, `batch.rs`, `stream.rs` and the routing
codec contain **zero** `Instant` uses. (`route.rs` imports `Instant` at
`:12`, but only the route *table* uses it, and that does not move.)

Not a cosmetic seam: on `wasm32-unknown-unknown`, `std::time::Instant::now()`
and `SystemTime::now()` **compile and then panic at runtime**
("time not implemented on this platform"). `cargo check --target wasm32` can
never catch this; only the explicit seam can.

## 5. Wasm size

`cargo build --release --target wasm32-unknown-unknown`, profile
`opt-level="z", lto=true, codegen-units=1, panic="abort", strip=true`.
The `cdylib` exports `wasm_wire_probe`, which runs the whole round-trip
(Noise NKpsk0 + `PacketBuilder` + AEAD + routing envelope), so none of the
crypto is dead-stripped. No `wasm-opt`, no `wasm-bindgen` CLI pass.

| Artifact | Raw | gzip -9 |
|---|---|---|
| `s0a_wire.wasm` as built | **576 461 B** (563 KiB) | **160 883 B** (157 KiB) |
| same, minus the inert `__wasm_bindgen_unstable` custom section | 395 720 B | 121 093 B |

**Section breakdown** (parsed out of the binary):

| Section | Bytes |
|---|---|
| `code` | 284 655 |
| custom `__wasm_bindgen_unstable` | 180 737 |
| `export` | 79 076 |
| `data` | 27 612 |
| everything else | 4 381 |

**What dominates: not the wire code, and not even the crypto — the JS RNG
glue.** 180 737 B of custom section + most of the 79 076 B export section +
~74 KB of code are `wasm-bindgen`/`js_sys` `__wbindgen_describe_*` shims
pulled in solely by `getrandom`'s browser backend (§6.2). A real
`wasm-bindgen` CLI pass consumes and removes that metadata; the 395 KB /
121 KB row is the closer estimate of what a `net-leaf` bundle would ship,
and it would shrink further under `wasm-opt -Oz`.

**Code-section attribution** (method: built a second, unstripped artifact,
parsed the `name` custom section, demangled each function name to its crate
and summed function-body sizes — `twiggy` was not installed and this needs
no extra tooling; percentages are of the 284 655 B code section and the
long tail below 0.5 % is omitted):

| Crate | Bytes | % of code |
|---|---|---|
| `__wbindgen_describe_*` shims (unnamed to the demangler) | 47 837 | 16.9 % |
| `snow` | 31 403 | 11.1 % |
| `core` | 31 346 | 11.1 % |
| **`s0a_wire` (the copied Net wire code)** | **21 486** | **7.6 %** |
| `alloc` | 19 362 | 6.9 % |
| `blake2` | 18 838 | 6.7 % |
| `js_sys` | 18 819 | 6.7 % |
| `sha2` | 14 261 | 5.1 % |
| `curve25519_dalek` | 8 549 | 3.0 % |
| `wasm_bindgen` | 7 878 | 2.8 % |
| `dlmalloc` | 7 746 | 2.7 % |
| `aes` | 7 482 | 2.7 % |
| `hashbrown` | 7 164 | 2.5 % |
| `bytes` | 6 470 | 2.3 % |
| `std` | 6 257 | 2.2 % |
| `polyval` | 3 119 | 1.1 % |
| `tracing_core` | 2 518 | 0.9 % |
| `dashmap` | 2 502 | 0.9 % |
| `parking_lot_core` | 2 151 | 0.8 % |
| `chacha20` | 1 714 | 0.6 % |

Crypto crates together: **~92 900 B, 32.9 %**. The Net wire code itself is
7.6 % of the code section — **the size problem for the browser leaf is
dependencies, not Net**.

Two obvious Stage 2/4 levers, both outside S0a's scope:

- `snow` with `default-resolver-crypto` links **AES-GCM, SHA-2 and
  Blake2b** as well as the ChaCha/Blake2s/X25519 that NKpsk0 actually uses:
  `aes` + `aes_gcm` + `sha2` + `polyval` + `ghash` ≈ 26 KB of pure dead
  weight. Selecting only `use-chacha20poly1305`, `use-blake2`,
  `use-curve25519`, `use-getrandom` should drop most of it.
- `dashmap` + `parking_lot_core` + `crossbeam_queue` (~5.6 KB) exist for
  multi-threaded contention that a single-threaded browser leaf does not
  have.

## 6. What did NOT go cleanly

Ordered by how much Stage 2 should care.

### 6.1 `ring` cannot build for `wasm32-unknown-unknown` — the packet AEAD needs a backend seam

`crypto.rs:10` binds `PacketCipher` directly to `ring::aead`. `ring`'s
`build.rs` drives `cc`, and on wasm32 it demands a **`clang` that targets
wasm32**. With no clang installed the build dies before any Rust compiles:

```
error occurred in cc-rs: failed to find tool "clang": program not found
```

This is not a `net` defect, but it is a hard toolchain tax on everyone who
builds the browser leaf, and `ring`'s wasm32 story is thin regardless. The
spike did what Stage 2 will have to do: `src/aead.rs` puts one thin seam
(`AeadKey::new` / `seal_detached` / `seal_append_tag` / `open_in_place`,
deliberately ring-shaped) in front of ChaCha20-Poly1305 —

- native: `ring::aead::LessSafeKey`, byte-identical behaviour to today;
- wasm32: the pure-Rust `chacha20poly1305` crate.

Both are RFC 8439 with a 12-byte nonce and a 16-byte tag, so the wire format
is unchanged. Cost inside `crypto.rs`: 6 call sites and the
`cipher: Box<LessSafeKey>` field — no logic moved. **The native test runs
the ring path and the wasm probe runs the RustCrypto path, so both are
exercised, but no cross-backend golden vector was written. Stage 2 should
add one** (same key + nonce + AAD + plaintext ⇒ identical ciphertext) to the
`cross_lang_wire` fixture set §Stage 2 already calls for.

Version note: the wasm side pins `chacha20poly1305` **0.10.1**, not the 0.11
`net` pins. 0.11 moved to `aead` 0.6's `InOutBuf` API, and 0.10.1 is already
in the tree under `snow`'s default resolver, so picking it dedups rather than
linking a second ChaCha20-Poly1305.

### 6.2 `snow`'s `std` feature force-enables `ring`, and two `getrandom` majors both need browser opt-in

Three separate dependency-resolution traps, none visible from the Net source:

1. **`snow = "0.10"` pulls `ring` even on wasm32.** snow 0.10's `std` feature
   (part of `default`) lists `"ring/std"` — not `"ring?/std"` — which
   *activates* snow's optional `ring` dependency. Removing `ring` from this
   crate's own dependencies changed nothing. The wasm build must use
   `default-features = false, features = ["default-resolver",
   "default-resolver-crypto"]`. Dropping `std` had no other consequence here,
   but `net-wire` will need this in its wasm feature set.
2. **`getrandom` 0.2 hard-errors** (`compile_error!`, "the
   wasm*-unknown-unknown targets are not supported by default") unless its
   `js` feature is on. It arrives as `snow`/`chacha20poly1305` → `aead` →
   `crypto-common` → `rand_core` 0.6 → `getrandom` 0.2.
3. **`getrandom` 0.3 needs `wasm_js`** for the same reason. `net` itself
   depends on 0.4. All three majors coexist and each needs its own opt-in;
   the spike names 0.2 twice (once renamed `getrandom_02`) to reach both.

The 0.2/0.3 browser backends are what drag in `wasm-bindgen` + `js_sys`, and
therefore §5's 260 KB of describe metadata. **`net-leaf` will depend on
`wasm-bindgen` anyway, so this is not new weight there — but a
`net-wire`-only wasm CI check pays it for nothing.** Consider
`getrandom`'s `custom`/register-backend path for the `net-wire` check job.

### 6.3 `std::net::SocketAddr` is load-bearing inside two wire types

`NetSession::peer_addr: SocketAddr` (`session.rs:67`, accessor `:318`,
constructor parameter `:164`) and `ParsedPacket::source: SocketAddr`
(`transport.rs:342`). Both compile for wasm32 — `SocketAddr` is plain data;
only the syscall surface is missing — so the spike kept them verbatim rather
than pre-empting the design. But a browser leaf has no socket address for its
peers, so **`net-wire` cannot be extracted at its final shape until Stage 1's
`PeerAddr` lands**. Ordering matters: Stage 2 after Stage 1, as the plan has
it. The round-trip test parses `"127.0.0.1:1"` purely as a placeholder.

### 6.4 `tracing` is a dependency of the wire surface

Seven log sites survive the cut (`crypto.rs` ×1, `reliability.rs` ×1,
`session.rs` ×3, routing codec ×2, including the `RouteFlags::from_u8`
high-nibble warning that is a documented wire-compatibility tripwire).
The brief's dependency list omitted `tracing`; the spike added it (0.1.44,
2.5 KB of code on wasm) rather than writing a no-op shim, which would have
been a hidden stub. `net-wire` should just depend on `tracing` — it is
`wasm32`-clean.

### 6.5 Every test module was dropped, including a tripwire that cannot move

See §2.4. The one to flag now: `session.rs`'s `heartbeat_api_drift_check`
reads `mesh.rs` and `mod.rs` off disk and counts approved `PacketBuilder`
call sites. Once `session.rs` lives in `net-wire`, that test is scanning
another crate's sources. It must either stay in core (pointing at the
re-exported path) or be reformulated. Silently losing it would remove the
guard behind issues #97/#106.

### 6.6 Smaller notes

- **`_MAX_TTL`'s underscore name.** The codec's TTL ceiling is `pub const
  _MAX_TTL` — an underscore-prefixed *public* constant. The spike's routed
  packet uses it; in `net-wire` it should be renamed `MAX_TTL`. (Mentioned
  because it is a public-API rename, not a free change.)
- **`thread_local!` on wasm32 compiles fine** — the coarse clock in
  `current_timestamp` needed no `cfg`. Single-threaded wasm keeps one TLS
  slot; no change required.
- **No `unsafe` blocked the move.** `pool.rs`'s thread-local pool machinery
  and `route_hop.rs`'s buffer handling compiled for wasm32 unchanged.
- **No serde surprises**: nothing in the moved surface derives `Serialize`.
  The wire format here is hand-rolled byte layout throughout, which is why
  `postcard`/`serde` — offered by the brief — were never needed. Neither were
  `blake3`, `ed25519-dalek`, `x25519-dalek`, or `chacha20poly1305` *natively*.
- **`parking_lot::Mutex`** (the anti-replay window in `crypto.rs`) compiles
  for wasm32 unchanged; on a single-threaded target it is uncontended, not
  wrong.
- Actual dependency set the spike needed: `bytes`, `parking_lot`, `snow`,
  `crossbeam-queue`, `dashmap`, `blake2`, `subtle`, `tracing`, plus `ring`
  (native) / `chacha20poly1305` + `web-time` + `getrandom`×2 (wasm32).
  `subtle` was not in the brief's list either — `route_hop.rs` verifies its
  hop tag in constant time.

## 7. The round-trip proof

`spikes/s0a-wire/src/roundtrip.rs`, driven by
`tests/routed_roundtrip.rs` (native, `cargo test`) and exported as
`wasm_wire_probe` (wasm32, so the crypto is reachable and not stripped).
No sockets, no tokio, no threads:

1. NKpsk0 handshake between two in-memory endpoints using the copied
   `crypto.rs`, with the real `handshake_prologue(src_node, dest_node)`
   binding, msg1/msg2 passed as byte vectors.
2. Both sides build a `NetSession` from the derived `SessionKeys`.
3. Initiator frames the payload through the session's `SharedLocalPool` →
   `PacketBuilder::build` (`pool.rs`), producing a real Net packet.
4. The packet is wrapped in `RoutingHeader::new(responder_id, initiator_id,
   MAX_TTL)` — the same `routing_bytes ++ net_packet` layout as
   `MeshNode::send_routed` (`mesh.rs:28390-28394`) — and handed over as a
   plain `Vec<u8>`.
5. Responder discriminates on `ROUTING_MAGIC`, decodes the envelope, checks
   `dest_id` against its own node id (the non-forwarding leaf rule), strips
   18 bytes, `ParsedPacket::parse`s the remainder, validates the length,
   recovers the counter from the header nonce, decrypts with `rx_cipher`,
   admits the counter against the replay window, and reads the event frames.
6. Asserts the recovered payload equals the sent payload, byte for byte.

The second test pins that an envelope addressed elsewhere is refused before
any decryption and that `write_to`/`from_bytes` round-trip the header —
the envelope codec is Layer 2, the leaf's only pre-direct path, which is
why compilation alone would not have been proof.
