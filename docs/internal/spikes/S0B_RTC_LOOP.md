# S0b — RTC loop spike

Stage 0 / S0b of
[`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
(§2 driver ownership, §3 channel config, §6 dedicated socket,
§Dependencies). Scratch code: [`spikes/s0b-rtc/`](../../../spikes/s0b-rtc).
Throwaway evidence, not production code.

**Result.** A real headless Chromium and a native str0m anchor complete a
DataChannel, an NKpsk0 handshake and a reliable-stream round-trip through
the S0a wire crate on **both** ends, in **both** ICE roles. This is the
first execution anywhere of the S0a wasm build — the `chacha20poly1305`
AEAD, `web_time`, snow's default resolver and `getrandom`'s `wasm_js`
backend all ran in the browser, with **zero runtime panics**.

`spikes/s0b-rtc/run.ps1` exits 0. Verdict lines, from the browser console:

```
S0B OK role=answerer dir=b2n payload=64 bytes
S0B OK role=answerer dir=n2b payload=69 bytes
S0B OK role=offerer  dir=b2n payload=63 bytes
S0B OK role=offerer  dir=n2b payload=68 bytes
```

Two findings contradict the plan as written and are the most important
output of this slice:

1. **The pinned `str0m` configuration does not avoid the C toolchain.**
   `default-features = false, features = ["rust-crypto"]` still pulls
   `aws-lc-rs` + `aws-lc-sys` (cmake/NASM/`cl.exe`) — see §6, Q3.
2. **`str0m` caps SCTP buffering at 128 KiB, so §2's 256 KiB
   `buffered_amount_advisory` default can never fire.** Observed: 127 069
   post-acceptance refusals with `advisory_over = 0` (§4).

---

## 1. Environment

| Item | Value |
|---|---|
| `s0a-wire` commit used | `4a95691d4e689762dca8190a320c8d5140f05c82` (S0a, unmodified — see §7) |
| `str0m` | 0.23.1, resolved features: **`rust-crypto` only** (`cargo tree -e features -i str0m` → `str0m feature "rust-crypto"` ← `s0b-native`) |
| transitive crypto actually linked | `str0m-rust-crypto` 0.6.0, `dimpl` 0.7.3, `str0m-proto` 0.7.0, `is` 0.11.0, **and `aws-lc-rs` 1.18.1 / `aws-lc-sys` 0.45.0** (Q3) |
| toolchain | `rustc 1.96.1 (31fca3adb 2026-06-26)`, `cargo 1.96.1` |
| wasm tool | **`wasm-bindgen-cli` 0.2.128** (`cargo install wasm-bindgen-cli`); the crate is pinned `wasm-bindgen = "=0.2.128"`. `wasm-pack` was not used. |
| wasm artifact | `s0b_bg.wasm`, 247 733 B after `wasm-bindgen --target web` (S0a's raw `cdylib` was 576 461 B; the bindgen pass consumes the describe metadata) |
| Chromium | `C:\Users\chief\AppData\Local\ms-playwright\chromium-1228\chrome-win64\chrome.exe`, **149.0.7827.55**, UA `HeadlessChrome/149.0.0.0`, `--headless=new` |
| OS | Windows 11 Pro (10.0.22631), x86_64-pc-windows-msvc |
| STUN/TURN | none. Host candidates only, over the LAN address (`192.168.50.161:<ephemeral>`) |

Reproduce: `pwsh -File spikes/s0b-rtc/run.ps1`. It builds both sides,
starts the anchor, runs Chromium headless, greps the verdict lines out of
both the relayed `/result` log and Chromium's own `--enable-logging=stderr`
console output, and exits 0/1. Logs land in `spikes/s0b-rtc/logs/`.

Only `run.ps1` exists (this host is Windows); a `run.sh` would differ only
in the Chromium path and process plumbing.

## 2. The driver loop as implemented, and whether §2 survived contact

Three threads. Nothing but the driver ever touches an `Rtc`; there is no
lock around one.

```text
  http threads                mesh thread                    driver thread
  (signalling + static)       ("the mesh side")              (owns the UDP socket
                                                              and every Rtc)
      |                            |                                |
      |-- Cmd::{Answer,Offer,      |                                |
      |   AcceptAnswer,Candidate,  |                                |
      |   Close}  (mpsc) ------------------------------------------>|
      |                            |                                |
      |                            |<--- ingress queue --------------|
      |                            |     sync_channel(1024)         |
      |                            |                                |
      |                            |---- per-peer send queue ------->|
      |                            |     sync_channel(256)          |
      |                            |     try_send == admission       |
```

Driver iteration, in order:

1. drain control commands — **each command is one mutation followed by a
   complete `poll_output` drain to `Output::Timeout`**;
2. outbound pump — per session: take the retained packet or pop **one**
   from the bounded queue, read `Channel::buffered_amount()`, publish it
   to the admission side, `Channel::write`, then drain again;
3. due timeouts → `handle_input(Input::Timeout)` + drain;
4. reap dead sessions (bump generation, count and discard the backlog,
   remove the peer handle);
5. one `recv_from` with a read timeout of `min(next_timeout, 5 ms)`;
   route the datagram by `Rtc::accepts(&input)`, `handle_input`, drain.

**§2's contract survived, with three corrections:**

- **The single-mutation invariant is easy to obey and did not misbehave
  once** across ~1 000 sessions' worth of runs. Making `drain()` the only
  way out of every mutation site is sufficient; no ordering surprise
  appeared in either ICE role.
- **`Ok(false)` needs a *named* retention policy or the "bounded queue"
  is a lie.** The first implementation dropped a refused packet on the
  floor: the queue then never backs up, so admission never refuses, and
  the loss is invisible — one run silently dropped **19 476** packets
  while reporting only 1 admission refusal. The spike now **retains and
  retries** the refused packet and stops that peer's pump for the
  iteration; with the identical workload the same run refuses **2 343**
  packets *at admission* and drops **0** in flight. Same bytes offered,
  completely different failure surface. Stage 3 must pick one explicitly;
  §2 currently only says the policy exists.
- **`buffered_amount` is not merely stale, it is bounded far below the
  configured advisory** — see §4. §2's "advisory reading vs reserved-bytes
  bound" framing is right, but its default number is unreachable.

One more shape note: the ingress queue must be bounded *and* the driver
must survive it being full. It was never full here (the mesh side keeps
up), but the drop path is wired and counted.

## 3. Both roles

Both roles complete the full sequence (channel → NKpsk0 → Net packet →
echo → decrypt). Channel-open time from the page's point of view, final
run:

| Role | Who offers | Channel open |
|---|---|---|
| `answerer` (native is ICE controlled) | browser | 28 ms, 20 ms |
| `offerer` (native is ICE controlling) | native | 187 ms |

**Asymmetry: yes, one, and it is signalling-shaped, not str0m-shaped.**
Native-as-offerer is consistently ~150 ms slower to open. The native
offer is produced *before* the browser exists in the exchange, so the
browser must fetch the offer, set it remote, create and post an answer —
one extra HTTP round trip and one extra `setRemoteDescription` before ICE
can start. The ICE/DTLS/SCTP phase itself is the same length in both
directions. No str0m p2p defect surfaced in either role: no
renegotiation, no stuck `IceConnectionState`, no role conflict.
(str0m's README p2p caveat did not bite at this scale; this is one
browser peer per `Rtc`, on loopback-ish LAN, which is exactly the anchor
shape §7 needs.)

## 4. Admission probe

Method: an established session; the page sends `TAG_PROBE_START` and then
**blocks its event loop for 3 000 ms** (`while (Date.now() < end);`), so
nothing drains the DataChannel. The mesh side pushes 8 000-byte Net
packets through `try_send` for 3 500 ms, retrying refusals. The page then
closes the peer connection — deliberately mid-backlog.

Final run (`spikes/s0b-rtc/logs/native.log`, `S0B PROBE after_close`):

| Metric | Value |
|---|---|
| packets accepted at admission | 32 030 (≈256 MB offered) |
| **admission refusals** (`try_send` full — the hard bound) | **2 343** |
| **`Channel::write` → `Ok(false)`** (post-acceptance) | **127 069** |
| `buffered_amount` at the first `Ok(false)` | **129 424 B** |
| max `buffered_amount` ever observed | **129 424 B** |
| advisory threshold crossings (`> 256 KiB`) | **0** |
| max staleness (published vs actual at next drain) | **8 089 B** |
| mean staleness | **3 235 B** |
| packets still queued when the channel closed | **243** (discarded, generation bumped) |
| packets the browser actually received while blocked | 31 727 |
| Windows `WSAECONNRESET` readings swallowed by the driver | 2 |

Readings, in order of importance:

- **(a) Yes, `write` returns `Ok(false)`, constantly, and at a fixed
  ceiling.** 127 069 refusals — ~80 % of all write attempts once the peer
  stops reading. The ceiling is not incidental: `str0m` 0.23.1 has
  `const MAX_BUFFERED_ACROSS_STREAMS: usize = 128 * 1024`
  (`src/sctp/mod.rs:30`), and `Channel::write` refuses whenever
  `buf.len() > MAX_BUFFERED_ACROSS_STREAMS - buffered`
  (`src/channel.rs:58-66`, via `RtcSctp::available`, `src/sctp/mod.rs:592`).
  With 8 KB packets that pins the observed maximum at 129 424 B.
  **Consequence for §2: `RtcConfig::buffered_amount_advisory` default of
  256 KiB is unreachable — `advisory_over` is 0 in a run with 127 069
  actual refusals.** The advisory number must be set below 128 KiB (and
  the cap is *across streams*, not per channel), or the advisory input is
  dead code. This is a plan correction, not a spike limitation.
- **(b) Staleness is bounded by one packet, not by a drain cycle.** Max
  8 089 B, mean 3 235 B — i.e. the published snapshot is off by at most
  the size of the write in flight, because the driver publishes
  immediately before each `write`. That is much better than §2 assumed,
  but it is still advisory: it is only refreshed when the driver is
  *already* writing to that peer, so an idle-then-burst peer reads a
  stale zero. It cannot be a bound.
- **(c) A close mid-backlog discards the remainder, and the count is
  real: 243 packets** (242 queued + 1 retained for retry) at the moment
  the channel closed. The driver bumps the peer generation, removes the
  peer handle, and every later `submit` for that generation is refused
  rather than resurrecting a dead channel. Nothing hangs; nothing is
  retried against a closed peer. Stage 3 owns whether that discard is
  silent (it is, here — it only increments a counter).

## 5. Round-trip and wasm runtime surprises

Verdict lines are emitted per direction per role. `dir=b2n` is proven by
the native side decrypting the browser's packet well enough to echo the
plaintext back; `dir=n2b` by the browser decrypting the native reply.
Both sides go through the S0a `NetSession` (native: `NetSession::new` +
`rx_cipher`/`PacketBuilder`; browser: the same code compiled to wasm).

```
S0B OK role=answerer dir=b2n payload=64 bytes
S0B OK role=answerer dir=n2b payload=69 bytes
S0B OK role=offerer dir=b2n payload=63 bytes
S0B OK role=offerer dir=n2b payload=68 bytes
```

**Which wasm paths this proves executed** (the round-trip cannot complete
otherwise):

- **`chacha20poly1305` (RustCrypto) AEAD** — S0a's wasm backend. Every
  browser-built packet was sealed by it and opened by `ring` natively,
  and every native packet sealed by `ring` was opened by it. That is a
  live cross-backend interop test of the S0a §6.1 seam, which S0a itself
  could only assert by construction.
- **snow's default resolver + `getrandom`'s `wasm_js` backend** — the
  NKpsk0 initiator's ephemeral keypair, plus an explicit
  `keygen_probe()` (`S0B INFO wasm probes: keygen_pub_len=32
  first_byte=228`, a different byte each run).
- **`web_time`** — `clock_probe()` returned `0.200 ms` from a busy-wait
  measured with `s0a_wire::clock::SystemClock` (monotonic), and
  `wallclock_probe()` returned `1789094935206 ms`, a plausible epoch
  time, through S0a's `current_timestamp()`. On `std::time` both calls
  would have panicked — this is the S0a §4 finding, executed.
- **The S0a packet pool / `NetSession` / stream sequencing** in a
  single-threaded wasm context (`thread_local!`, `parking_lot::Mutex`,
  `dashmap`) — no panic, no deadlock.

**Runtime surprises: none.** No wasm panic fired at any point (a panic
hook routes them to `console.error`; the console log is clean). No
missing import. The only wasm-side friction was compile-time, not
runtime: `wasm-bindgen` refuses to run unless the CLI version equals the
crate version, so the crate is pinned `=0.2.128`.

**S0c hook (defined, deliberately not measured):**
`window.s0cHook({ n, size, rateHz })` in `web/page/app.js` opens a
session and sends `n` packets of `size` bytes at `rateHz`, returning
per-packet `build` (Net-layer wasm seal) and `send` (DataChannel handoff)
timings plus wall time. `LeafEndpoint::bench_build(n, size)` is its
Rust-side half. Neither is called during the S0b run.

## 6. The four questions

### Q1 — Does str0m 0.23.1 support ICE-TCP passive candidates? **Partially: the candidate type exists and is accepted; the transport is the caller's problem.**

Evidence (printed by the native binary at startup, before anything else):

```
ICE-TCP: Candidate::builder().tcp().tcptype(Passive) constructed OK ->
  "candidate:fffeff5a2adc95d05158f004 1 tcp 1526726399 192.168.50.161 53776 typ host tcptype passive";
  add_local_candidate accepted=true
```

- The API exists and is public: `str0m::Candidate::builder()` →
  `CandidateBuilder::tcp()` → `.tcptype(TcpType::Passive)` → `.host(addr)`
  (`is` 0.11.0 `src/candidate.rs:151,753,888`), re-exported by str0m
  along with `str0m::net::{Protocol, TcpType}` (`src/lib.rs:726,857`).
- `Rtc::add_local_candidate` accepts it and it serializes to a
  well-formed `tcptype passive` SDP line.
- **But str0m is sans-IO**: `Output::Transmit` carries a `Protocol`, and
  the application owns the TCP listener, the RFC 4571 length framing and
  the connection lifecycle. str0m will not open or accept TCP sockets.
  So "supported" means "the ICE agent can carry and pair TCP candidates",
  not "turn it on and it works". Not exercised end-to-end here (out of
  S0b's scope: no TCP path was wired).

### Q2 — Is `RTCPeerConnection` available in a dedicated `Worker` and/or a `SharedWorker` in this Chromium? **No, in both.**

```
S0B WORKER dedicated: err: ReferenceError: RTCPeerConnection is not defined
S0B WORKER shared:    err: ReferenceError: RTCPeerConnection is not defined
```

Chromium 149.0.7827.55, `--headless=new`. Both workers were real blob-URL
workers that constructed the object inside the worker scope; the error is
the constructor being absent from the global, not a permissions or
secure-context failure. Consequence for §7/§8: the leaf's RTC driver must
live on the **main thread**; only non-RTC work (fold, storage, hashing)
can move to a worker. A `SharedWorker`-owned single mesh connection
shared across tabs is not available in this Chromium.

### Q3 — Does the pinned `rust-crypto` configuration build with no C toolchain involvement? **No. The pin does not do what §Dependencies says it does.**

`cargo tree -i aws-lc-sys` with the pinned configuration:

```
aws-lc-sys v0.45.0
└── aws-lc-rs v1.18.1
    ├── dimpl v0.7.3
    │   ├── str0m v0.23.1  <- default-features = false, features = ["rust-crypto"]
    │   └── str0m-rust-crypto v0.6.0
    └── rcgen v0.14.10
        └── dimpl v0.7.3
```

Root cause, exactly: `str0m-rust-crypto` 0.6.0 depends on
`dimpl = { default-features = false, features = ["rust-crypto", "rcgen"] }`,
and `dimpl` 0.7.3 defines `rcgen = ["dep:rcgen", "aws-lc-rs"]` — the
self-signed-DTLS-certificate feature **implies the aws-lc backend**.
Choosing str0m's pure-Rust provider therefore cannot avoid `aws-lc-sys`.

A clean release build of that unit (`cargo clean --release -p aws-lc-sys`
then `cargo build --release -vv`) produces ~11 700 lines of aws-lc-sys
build-script output, compiles C with
`…\MSVC\14.35.32215\bin\HostX64\x64\cl.exe` via `cc-rs`, and takes ~40 s
on this host. NASM is *not* installed here — the build only survives
because `dimpl` requests `aws-lc-rs`'s `prebuilt-nasm` feature
(`CARGO_FEATURE_PREBUILT_NASM=1` in the log), which ships prebuilt
assembly objects. `cmake` 0.1.58 and `cc` 1.4.5 are both in the graph.

So on a Windows host **without** MSVC, or on any target aws-lc-sys has no
prebuilt objects for, this configuration fails to build. Stage 3 has
three options and must pick one: accept the C dependency and say so in
CI's supported-target matrix; carry a patch/fork of `dimpl` that lets
`rcgen` use a pure-Rust signer; or supply the DTLS certificate from
outside so the `rcgen` feature is not needed. §Dependencies' claim that
the pin keeps cmake/C out of the build is **withdrawn by this spike**.

### Q4 — Trickle vs gather-complete: offer-created → DataChannel-open, 5 runs each.

| Mode | min | median | max | raw (ms) |
|---|---|---|---|---|
| **trickle** | **20.8 ms** | **22.5 ms** | **22.7 ms** | 20.8, 22.7, 22.6, 22.5, 22.5 |
| **gather-complete** | **146.9 ms** | **150.2 ms** | **177.2 ms** | 150.2, 147.1, 146.9, 177.2, 161.0 |

Trickle is **~6.6× faster (≈128 ms saved at the median)**, and the gap is
a floor, not a ceiling: this is a single-interface LAN host with no STUN
and no TURN, i.e. the cheapest possible gathering. A real client with
several interfaces and a STUN round trip waits longer for
`icegatheringstatechange == complete`, while trickle's first usable pair
is unaffected.

Stage 4 reading: the WebSocket (or any bidirectional signalling channel
that can deliver candidates after the offer) is worth it. A
single-POST/single-response bootstrap costs ≥130 ms of added
connect latency for no benefit — and that is *before* the enrollment
exchange §12 puts on top.

## 7. What did NOT go cleanly

Same discipline as S0a §6, ordered by how much Stage 3/4 should care.

### 7.1 The `str0m` pin does not exclude the C toolchain (see Q3)

The single most consequential finding: §Dependencies' pinned
configuration was chosen precisely to keep cmake/NASM/C out, and it does
not. Everything else here is smaller.

### 7.2 `str0m`'s 128 KiB SCTP cap versus §2's 256 KiB advisory (see §4)

`MAX_BUFFERED_ACROSS_STREAMS = 128 * 1024`, hard-coded, **across all
streams of the `Rtc`** — not configurable from the public API. Two
consequences: the plan's default advisory can never fire, and once a node
runs several channels on one `Rtc`, they compete for one 128 KiB budget.
This spike used one channel per `Rtc`, so the multi-channel interaction
is untested.

### 7.3 Chrome hides host candidates behind mDNS by default

Without `--disable-features=WebRtcHideLocalIpsWithMdns`, Chrome publishes
host candidates as `<uuid>.local` names. `Candidate::from_sdp_string`
takes them, but str0m has no mDNS resolver, so no pair ever forms and the
channel never opens. The spike passes the flag. **This is not a
test-harness quirk that disappears in production**: a real browser will
send mDNS candidates to the anchor, and the anchor must either resolve
them (mDNS client), rely on server-reflexive/prflx candidates arriving
from the browser's own STUN checks, or accept the peer-reflexive
candidate str0m learns from the inbound binding request. §6 does not
mention mDNS at all; Stage 4 needs an answer.

### 7.4 Windows: `WSAECONNRESET` on a UDP `recv_from`

After a peer disappears, an ICMP port-unreachable makes the **next**
`recv_from` on the shared RTC socket fail with
`os error 10054 / ConnectionReset` — on a *connectionless* socket, about
a *different* peer. Treated as fatal it kills the whole driver (and with
it every other session on that socket). The driver must swallow it
explicitly; the spike counts them (2 per run). The `str0m` `http-post`
example does not handle this, so the obvious copy-paste starting point is
wrong on Windows.

### 7.5 One `poll_output` drain contract, two easy ways to violate it

Both were hit while writing this: (a) returning early from the pump after
`Channel::write` without draining — the write's transmits then sit until
the next loop iteration, which shows up as a mysterious ~5 ms latency
floor; (b) calling `Rtc::channel(cid)` twice around a `buffered_amount`
read (it borrows `&mut Rtc`), which forces the code into a shape where
it is tempting to interleave. Neither is a str0m bug; both are the sort
of thing a `RtcDriver` needs a single choke-point function for.
`drain()` is that function here.

### 7.6 `buffered_amount` is only observable while writing

`Channel::buffered_amount` needs `&mut Rtc`, so only the driver can read
it, and in this shape it is refreshed only in the pump — i.e. only for
peers that are *currently* being written to. A peer that goes idle keeps
whatever it published last. Stage 3's "how the snapshot is factored" is
therefore also "when is it refreshed", which §2 does not say.

### 7.7 The verdict-line channel

The acceptance criterion asks for browser-console lines. Chromium's
`--enable-logging=stderr` does emit them (`INFO:CONSOLE(30) "S0B OK …"`),
but it interleaves them with hundreds of unrelated lines and its
formatting is version-dependent, so the page **also** POSTs each line to
`/result` and the native process prints it. `run.ps1` asserts on the
relayed copy and prints both. Worth knowing before CI depends on parsing
Chromium's stderr.

### 7.8 Smaller notes

- **No change to `spikes/s0a-wire/` was needed.** The brief allowed
  additive `pub`s or a wasm-bindgen-friendly wrapper; none were required —
  S0a's public surface (`crypto`, `session`, `protocol`, `pool`,
  `parsed_packet`, `clock`, `time`) was sufficient as-is, and the wrapper
  lives entirely in `spikes/s0b-rtc/web/`.
- **Signalling carries no JSON.** SDP and candidate lines are `text/plain`
  bodies; the native side hand-rolls ~150 lines of HTTP/1.1. That removed
  a JSON dependency from a spike whose payload is newline-heavy SDP, and
  it means the endpoint shapes here say nothing about the real bootstrap
  API.
- **The PSK and the responder's static public key are served over plain
  HTTP** by `/config`. That is a spike shortcut standing in for §5's
  authenticated key discovery; it is not a proposal.
- **One channel per `Rtc`, one `Rtc` per session, all on one UDP socket**
  (§6's shape). Routing inbound datagrams by `Rtc::accepts` over a linear
  scan is O(sessions) per packet — fine for 14 sessions, obviously not
  the production data structure.
- **The reliable-stream round-trip uses `PacketFlags::RELIABLE` and the
  S0a stream sequencing, but no packet was lost**, so
  `reliability.rs`'s NACK/retransmit machinery never ran. "Reliable
  stream" here means the flag and the sequence state, not a proven
  recovery path. Testing recovery needs deliberate loss injection —
  worth a Stage 3 exit criterion, since `maxRetransmits: 0` means Net's
  reliability is the *only* recovery mechanism.
