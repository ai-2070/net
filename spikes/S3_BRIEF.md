# Stage 3 — Native `webrtc` feature: driver, dedicated socket, STUN, loopback harness

**Authorized by the product owner (2026-09-11), stacked on the Stage 2
closure head `01e4b0f20`** while Kyra's C2 follow-up re-review and its CI
run are pending. Consequence, as before: **strictly additive** — a new
`adapter/net/rtc/` module and the `PeerAddr::Rtc` variant, all behind a
`webrtc` feature that is **off by default**, plus the RTC arms of the
seams Stage 1 already introduced. The default build must be
byte-for-byte the Stage 2 behaviour and its export set unchanged. A
Stage 2 fix, if one comes, must not conflict: do not reshape Stage 1/2
code beyond adding feature-gated arms.

Source of truth, in this order:

1. `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` §Stage 3
   (scope + every exit criterion — each one is an acceptance test), §1
   ("send path", "What S0d established"), §2 (driver, admission, "What
   S0b established", "Hard bound vs advisory", single-mutation
   invariant, one dispatch owner), §3 (one packet per message,
   `MAX_PAYLOAD_SIZE`), §6 (dedicated socket, STUN, mDNS note), §11
   (registry), §Dependencies (`str0m` pin and the `aws-lc-sys` finding).
2. `docs/internal/spikes/S0B_RTC_LOOP.md` and **`spikes/s0b-rtc/native/src/main.rs`**
   — the working driver loop, admission probe, `WSAECONNRESET` handling,
   single `drain()` choke point, `Rtc::accepts` routing. Lift its shape;
   do not rediscover it.
3. `docs/internal/spikes/S0D_SEND_INGRESS_INVENTORY.md` §3.2–§3.4 — the
   RTC submission contract, the drain split, the bounded
   `IngressReceiver` variant; and its row table's "RTC disposition"
   column, which is the per-class pressure policy Stage 3 must assert.
4. `S1_REPORT.md` §2 (`PeerSink` as implemented) and `S2_REPORT.md` §2
   (`PeerAddr` lives in `net_wire::peer_addr`).
5. `AGENTS.md`: pre-push checklist, feature-flag trap, per-member lint
   lists, `tests/*.rs` must be pinned by name in CI.

## Decisions fixed for this stage

- **`aws-lc-sys`: option (a) — accept it as a build-time C dependency
  behind the off-by-default `webrtc` feature.** Rationale: (b)
  per-platform providers add three dependency trees and platform
  matrices; (c) is an upstream wait. Document the host requirements in
  the feature's doc comment and in `CONTRIBUTING.md` (cmake + MSVC or
  clang; NASM or `prebuilt-nasm`), and record the measured build cost
  in the report so the choice can be revisited. The default build stays
  C-free; the CI job that builds `--features webrtc` is Linux-only and
  installs nothing beyond what `aws-lc-sys` needs there (it builds with
  the runner's gcc/cmake). `str0m = { version = "0.23.1", optional = true,
  default-features = false, features = ["rust-crypto"] }` exactly.
- **Feature:** `webrtc = ["nat-traversal", "dep:str0m"]` in the core.
  `PeerAddr::Rtc(RtcPeerId)` is `#[cfg(feature = "webrtc")]` in
  `net_wire::peer_addr` (the wire crate gains a `webrtc` feature that
  only adds the variant — no str0m there). `RtcPeerId { slot: u32,
  generation: u32 }`, never reused: closing bumps `generation`.
- **Advisory threshold below str0m's cap:** `buffered_amount_advisory`
  default **96 KiB** (`MAX_BUFFERED_ACROSS_STREAMS` is 128 KiB,
  `str0m/src/sctp/mod.rs:30`); reserved bound defaults
  `send_queue_packets = 256`, `send_queue_bytes = 256 KiB`.
- **Retention:** retain-and-retry. A packet accepted at admission is held
  by the driver until `Channel::write` returns `Ok(true)` or the channel
  closes; on close the remainder is discarded **and counted**
  (`RtcStats::discarded_at_close`). Never dropped on `Ok(false)`.
- **Advisory refresh cadence:** on every drain, for every peer whose
  queue is non-empty **or** whose last published reading is above zero
  (so an idle peer's stale high reading decays to truth), the driver
  re-reads `buffered_amount` and publishes it.
- **Ingress:** a third `IngressReceiver::Rtc(mpsc::Receiver<(Bytes, RtcPeerId)>)`
  variant (bounded; default 1 024) fed by the driver; `dispatch_packet`
  stays the single owner; a full input **drops with a counter**
  (`RtcStats::ingress_dropped`), never blocks the driver.
- **`ConnectionReset` on the RTC socket:** swallowed and counted
  (`RtcStats::udp_conn_reset`) inside the driver; the driver's sender
  being dropped is the only fatal ingress condition.
- **Loopback harness only.** No bootstrap listener, no `0x0D02`, no
  announcement fields, no mDNS, no browser — those are Stage 4/5. The
  harness establishes the DataChannel between two native nodes with a
  test-only in-process signalling exchange (`connect_rtc_loopback`,
  `cfg(any(test, feature = "fixtures"))`).

## Target

New: `net/crates/net/src/adapter/net/rtc/{mod,config,driver,transport,stun,stats}.rs`
(all `#[cfg(feature = "webrtc")]`), `tests/rtc_loopback.rs` +
`tests/rtc_backpressure.rs` (pinned by name in CI; `--features webrtc`).
Touched with feature-gated arms only: `transport.rs` (`PeerSink` RTC
half), `router.rs` (drain partition `Rtc` arm), `mesh.rs`
(`IngressReceiver::Rtc`, `MeshNodeConfig::rtc: Option<RtcConfig>`,
`RtcStats` on `MeshNode`, the `validate()` rejection counter on the RTC
ingress path, `PairAction::Ice` short-circuit in the upgrade path),
`traversal/classify.rs` (`PairAction::Ice`), `wire/src/peer_addr.rs`,
both `Cargo.toml`s, `Cargo.lock`, `ci.yml`, `CONTRIBUTING.md`.

**Frozen:** everything Stage 1 froze (`proxy.rs`, traversal internals,
wire formats, config `SocketAddr`s) plus all Stage 2 re-exports.
`exports.baseline` untouched — the feature is off in the cdylib build.

## Change

1. **`RtcConfig`** (`rtc/config.rs`): `bind_addr` (default
   `bind_addr.ip():0`), `public_addr: Option<SocketAddr>`, `ice_deadline`
   (10 s), `max_peers`, `send_queue_packets`, `send_queue_bytes`,
   `buffered_amount_advisory`, `ingress_queue_packets`, `serve_stun`
   (bool, default false), `serve_bootstrap` (bool, default false —
   Stage 4 consumes it; here it is config only). `MeshNodeConfig::rtc:
   Option<RtcConfig>`; `None` ⇒ no RTC socket, no driver, no behaviour
   change even with the feature compiled.
2. **Driver** (`rtc/driver.rs`): one task owning the RTC `UdpSocket` and
   every `str0m::Rtc`, exactly S0b's loop: recv → (STUN binding
   responder if `serve_stun` and the datagram is an unsolicited binding
   request) or `Rtc::accepts` → `handle_input` → single `drain()` to
   `Output::Timeout`; per-`Rtc` timer; per-peer bounded queue with
   reserved slots+bytes; `Channel::write` one packet per drain;
   retention as decided; advisory publication as decided; `generation`
   bump on close and discard of stale queued packets; tear-down through
   the existing peer-removal path so `addr_to_node`/`PeerTransport`
   unwind in the normal order. `WSAECONNRESET` swallowed + counted.
3. **`PeerSink` RTC half** (`transport.rs`): `try_send(packet, RtcPeerId)`
   → synchronous, total admission: reserved slots/bytes **and** the
   published advisory; refuse with `io::ErrorKind::WouldBlock`; accepted
   ⇒ the driver owns the packet. `PeerSink::send` / `send_bounded` on an
   `Rtc` endpoint delegate to `try_send` (they cannot block the driver)
   and map `WouldBlock` per the row's disposition (S0d §3.2: rows
   marked *refuse-at-admission* propagate it; rows marked *drop with
   counter* count and return `Ok`). The mapping table in the report
   must cover every S0d row.
4. **Scheduler drain** (`router.rs`): partition by `PeerAddr` variant —
   `Udp` exactly as today (`sendmmsg` grouping intact); `Rtc` submitted
   one at a time via `try_send`, `WouldBlock` ⇒ re-queue at the front
   once, then count and drop (`RtcStats::drain_refused`). `MAX_DRAIN` and
   the drain instrumentation stay whole-drain.
5. **Ingress** (`mesh.rs`): `IngressReceiver::Rtc` as decided;
   `dispatch_packet(data, PeerAddr::Rtc(id), ctx)` — the existing
   source-keyed paths (partition filter, `addr_to_node`, direct/routed
   ownership, stale-session and address-reuse protections) run unchanged
   on the new variant. The RTC ingress path counts `NetHeader::validate`
   rejections (`RtcStats::validate_rejected`).
6. **`PairAction::Ice`** (`traversal/classify.rs`): returned for any pair
   with a `transport:rtc` side so the punch logic never runs; the
   direct-upgrade scan (`mesh.rs`, `SKIPPUNCH_RECHECK` region) treats it
   as "no punch".
7. **STUN** (`rtc/stun.rs`): RFC 5389 binding request/response only,
   XOR-MAPPED-ADDRESS, ~200 lines, unit-tested against a hand-built
   request and against `stunclient` if present on the host (record).
8. **Harness** (`tests/rtc_loopback.rs`): two `MeshNode`s on loopback,
   both with `rtc: Some(..)`; `connect_rtc_loopback(a, b)` performs the
   SDP offer/answer + candidate exchange in-process (no signalling
   subprotocol), opens one DataChannel `{ordered: false, maxRetransmits: 0}`,
   runs the Noise handshake over it via the **existing**
   `connect_direct`-style path with the peer's static key supplied, and
   installs `PeerTransport::Direct { owned: PeerAddr::Rtc(id) }`. Then
   re-run, against that pair, the existing stream / reliability /
   backpressure / nRPC / fold / routing-plane witness scenarios the plan
   names — by **calling the same helpers those test files use**, not by
   copying them. Where a witness file cannot be parameterized over the
   transport without editing it, add a thin `#[cfg(feature = "webrtc")]`
   variant beside it and say so in the report.
9. **Backpressure harness** (`tests/rtc_backpressure.rs`): every §Stage 3
   exit criterion that names admission, retention, advisory staleness,
   `Ok(false)` after a passing precheck, loss injection, `validate()`
   rejection, `ConnectionReset` survival, per-class disposition, and the
   §5 delivery sequence (routed → authenticated direct → forced direct
   failure → restored routed). Inject via test-only hooks on the driver
   (`cfg(any(test, feature = "fixtures"))`): pause the drain, stub
   `Channel::write` to return `Ok(false)`, drop N% of DataChannel
   messages, inject `ConnectionReset` on the socket.
10. **CI**: a Linux job `webrtc-feature` that runs `cargo clippy
    --features webrtc --lib --bins -- -D warnings`, `cargo test --lib
    --features "$UNIT_FEATURES webrtc"`, and the two pinned test files;
    the integration-test pin guard updated; `cargo doc` with the feature;
    `net-mesh-wire` linted with its `webrtc` feature too. The default
    jobs are untouched. Record the job's wall time and the `aws-lc-sys`
    build time separately.

## What must survive, exactly

- Default build: `cargo test --lib --features "$UNIT_FEATURES"` count
  and the six floors unchanged; export checker 568/568 on a fresh
  `net-ffi` build; `git diff 01e4b0f20..<candidate> -- go/ bindings sdk
  cli deck adapters payments` empty or `Cargo.lock`-only.
- With `webrtc` on and `rtc: None`: identical behaviour — a witness runs
  a UDP-only two-node scenario under the feature and asserts the same
  packets/counters as without it.
- UDP rows' blocking/error-mapping/batching columns unchanged (Stage 1
  exit criterion, re-asserted).
- No new guards across awaits; the driver holds no `DashMap` guard
  across `poll_output`.

## Constraints

- Additive, feature-gated. Do not touch plan documents or earlier
  reports; do not touch `spikes/**` except to read.
- Windows host: the feature **builds here** (S0b proved `aws-lc-sys`
  compiles with MSVC + `prebuilt-nasm`); run everything locally. The
  Linux arms remain CI's.
- Skip formatters until validation; then `cargo fmt -p` per member and
  per-file `rustfmt --check`.
- Commits on `LZL0/webrtc-transport`, prefix `feat(net): stage 3 —`:
  (1) feature + `PeerAddr::Rtc` + config + stats, no driver; (2) driver
  + STUN; (3) `PeerSink`/drain/ingress arms; (4) harness; (5)
  backpressure harness + hooks; (6) CI + `CONTRIBUTING.md`; (7)
  candidate: fmt, validation, report. Each checkpoint passes
  `cargo check --workspace --all-targets` with and without the feature.

## Validation

Everything in the Stage 2 brief's list, plus, with `--features webrtc`:
strict clippy (`--lib --bins -D warnings`), permissive `--all-targets`,
`cargo doc`, `cargo test --lib --features "$UNIT_FEATURES webrtc"`, the
two pinned test files, the six floors under the feature too, the
export checker (feature off in the cdylib), and the exit-criterion
table with one row per criterion → test name → pass/fail.

## Report

`docs/internal/spikes/S3_REPORT.md`: three-heads table from the start;
`RtcConfig` as shipped; the driver's ownership diagram and its
divergences from S0b; the **per-row disposition table** (every S0d row →
RTC behaviour → asserting test); the exit-criterion table; the
`aws-lc-sys` cost (clean-build seconds, host and CI); wasm/leaf-relevant
notes for Stage 5; validation list; deviations; "did not go cleanly".
Reply in the terminal with the candidate hash, the exit-criterion table
verbatim, the validation pass/fail list, and the last section verbatim.
Then stop — **no Stage 4**.
