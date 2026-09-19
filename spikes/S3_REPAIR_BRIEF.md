# Stage 3 repairs — Kyra's HOLD on `0fcff7a16`

Kyra's independent review **holds** Stage 3: "a working native RTC happy
path with concrete integration/lifetime defects, not 17/18 exit
criteria met." Stages 1–2 credit stands; the UDP/wire work is not to be
undone. Exact-head CI: 49/50 (the one red is the `Documentation` job,
R5-B). Evidence under
`C:/Users/chief/AppData/Local/hermes/cache/webrtc-stage3-0fcff7a16/`
— read `rtc-driver-review.md`, `rtc-integration-review.md`,
`rtc-evidence-review.md` and `kyra_runtime_probes.rs` before starting;
the runtime probes are the acceptance tests in all but name.

Same branch, prefix `fix(net): stage 3 repair S3-Rn —`. Grouping below
is by acceptance outcome, not commit boundary. Append **§11 "Repairs
after Kyra's HOLD on `0fcff7a16`"** to `docs/internal/spikes/S3_REPORT.md`
with, per item, the reproduction before, the proof after (command +
output line), and the inverse that goes red. Do not touch plan
documents. **C2 is not yours** — the reviewer (Fable) owns
`heartbeat_api_drift_check.rs`.

## S3-R1 (P1) — scheduled RTC packets are consumed without a transport handoff

`mesh.rs:12154–12186, 39744–39771`; `router.rs:693–707, 735–744,
1050–1063, 1098–1118`. Construction installs the RTC transport in
`PeerSink` but never calls the router's `set_rtc_transport`; the
scheduler's RTC drain returns silently when the option is empty. Kyra:
16 scheduled fire-and-forget sends over a real connected pair with the
pump paused → all `Ok`, scheduler depth 0, RTC admission +1 (background
traffic only); after wiring the transport, +16.

Closure: wire the constructor/drain handoff. A witness sends N
scheduled fire-and-forget packets and asserts RTC admission rises by
exactly N (identify them by stream id, not by a global counter);
separately assert the drain's stated disposition on refusal (re-queue
once, then count and drop) with the pump paused and the reservation
full. Removing `set_rtc_transport` from construction must turn the
witness red. UDP grouping/deadlines/batching unchanged.

## S3-R2 (P1) — healthy RTC pressure becomes terminal `StreamError::Transport`

`rtc/transport.rs:60–70`; `transport.rs:404–438`;
`mesh.rs:39665–39686, 39766–39801`. RTC refusal is `WouldBlock`, but
the unscheduled stream seam wraps every sink error as `Transport`, which
`send_with_retry` does not retry. Kyra: fill the four reservations,
`send_on_stream` reliable → `Err(Transport("send failed: rtc admission:
reserved queue slots exhausted"))`.

Closure: at the unscheduled arm, map an RTC `WouldBlock` to
`StreamError::Backpressure` **only for `PeerAddr::Rtc`**; UDP keeps its
`Transport` mapping byte-for-byte (Stage 1 exit criterion). Witnesses:
refused first packet → `Backpressure`, credit refunded, sequence rolled
back, no retransmit entry; retry after draining → `Ok`, one entry;
prefix-accepted/suffix-refused → the committed prefix is **not**
replayed and the caller sees `Backpressure` only for the suffix. Prove
the UDP mapping is unchanged by running the Stage 1 `deliver_stream_packet`
witness.

## S3-R3 (P1) — close and node lifetime are not coherently owned

A. **Submit/close race** (`rtc/transport.rs:147–180, 239–263`): the
closed check precedes the queue lock. Kyra's barrier harness: after
close, `Ok(())`, queued=1, accepted=1, discarded=0 — an orphan. Re-check
`closed` **under the queue mutex**; the control produced
`Err(UnknownPeer)`, queued=0. Conservation witness: accepted ==
written + discarded_at_close + retained, exactly, across a
submit/close interleaving.

B. **Node shutdown leaks the driver/socket** (`rtc/driver.rs:328–359,
383–399, 498–526`; `mesh.rs:41914–41954, 42004–42055`): the join
handle is discarded, shutdown does not signal the driver, channel
disconnection is not terminal. Kyra: after `shutdown` + drop, the RTC
address stays bound. Closure: `MeshNode` retains the driver handle;
`shutdown` signals and **joins** it (bounded); signalling-channel
disconnection is a terminal loop condition. Witness: shutdown → rebind
the exact RTC address succeeds.

C. **Generation ignored by driver signals** (`rtc/driver.rs:879–880,
919–920, 943–956`): `AwaitOpen`/`Close` with the live slot and a wrong
generation act on the live session. Validate `(slot, generation)` in
**every** signal; a stale handle gets `UnknownPeer`. Witness: stale
`Close` cannot evict a successor.

D. **Retired storage grows with churn** (`rtc/transport.rs:126–138,
239–277`; `rtc/driver.rs:963–983, 702–717`): production always opens a
fresh slot; close retains slot + deque capacity (32 lifetimes → 32
slots, 128 queue positions). Closure: recycle closed slots with a
bumped generation (the unit-tested reopen path becomes the production
path), free deque capacity on close, and define honest exhaustion:
`generation` wrap on a slot is an error surfaced as
`RtcError::IdentityExhausted`, never a silent reuse. Witness: 32
lifetimes ≤ `max_peers` slots retained; stale handles from lifetime N
are refused after slot reuse in lifetime N+1.

E. **Close/upgrade transition gaps** (source-established): driver reap
closes transport state without a mesh peer-removal notification;
queued handshake completion is not fenced against the exact live RTC
identity at installation; fixture `connect_rtc`/`accept_rtc` install
with no expected-prior-session CAS and bypass the quiescence gate.
Closure: reap → the existing peer-removal path; installation fences on
`(slot, generation)` still live; the fixtures go through the ordinary
installer with the CAS and quiescence gate (they may *drive* the SDP
exchange in-process, but not the installation). Witnesses: queued
handshake completion cannot install a dead endpoint; cancelled waiters
are reclaimed; a busy or newer incumbent survives a failed/superseded
upgrade. **No new guards held across awaits.**

## S3-R4 (P1) — input classification breaks valid ICE and protected Net traffic

A. **STUN steals ICE** (`rtc/driver.rs:738–762`; `rtc/stun.rs:34–46`):
with `serve_stun`, any Binding Request gets the generic response before
the session's ICE agent sees it — and ICE checks are Binding Requests.
Kyra: the same pair succeeds with STUN off, fails "rtc session closed"
with STUN on. Closure: route a Binding Request to `Rtc::accepts` first;
only a request **no session accepts** (no matching ICE credentials /
USERNAME attribute) is answered by the bare responder. Witness: a bare
STUN exchange on the dedicated socket **while** an RTC session stays
live and delivering, both directions, `serve_stun` on both ends.

B. **RTC prefilter excludes protected route-hop envelopes and headerless
pingwaves** (`mesh.rs:752–761, 24880–24899`): the filter admits routing
magic or a validating `NetHeader`, but shared dispatch also handles the
authenticated route-hop outer magic (`wire/src/route_hop.rs`) and the
72-byte headerless pingwave (`mesh.rs:24692–24733`). Closure: inventory
every outer format `dispatch_packet` accepts (§Context's five) and admit
exactly those on the RTC path, preserving each one's downstream
authentication/authority check. Witnesses: a sealed route-hop opened
after RTC ingress (paired with the UDP equivalent); native route
learning from a pingwave over RTC; and **separate** cases for valid
size + bad magic, excessive declared payload, actual message size, and
malformed routed-inner — 9 000 bytes of bad magic does not isolate size
validation.

## S3-R5 (P2 / build gate) — wire feature unification and default docs

A. A standalone consumer with default `net-mesh` + `net-mesh-wire/webrtc`
fails: `PeerAddr::Rtc` exists in the shared type while the core's
matches are guarded by the core's `webrtc` → six errors (non-exhaustive
patterns, refutable binding). Closure: the core's `webrtc` feature must
**imply** `net-mesh-wire/webrtc`, and the core must compile with the
wire variant present regardless of its own feature — either the core's
matches use `#[cfg(feature = "webrtc")]` arms **plus** a
`#[cfg(not(...))] _ => unreachable!()`-free total match (a wildcard arm
that returns the "not an RTC endpoint" answer), or the variant is
introduced in the wire crate behind a feature the core always enables
when it is present. Prove: the standalone consumer graph (default core
+ wire/webrtc), normal `core/webrtc`, default core, and the lean FFI
matrix all compile. Do not mask it by enabling the full transport in
the consumer control.

B. `cargo doc` default features fails: `wire/src/peer_addr.rs:16` links
`[PeerAddr::Rtc]` when the variant is absent. Fix the link
(`cfg_attr(doc)` or plain text) and add
`RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-wire --no-deps` **with
default features** to the validation list; the CI `Documentation` job
already runs it and must be green.

## S3-R6 — replace the non-discriminating witnesses

Kyra's inverse: suppress all public stream sends on the witness stream
ids (0x51, 0x61) → both reliability witnesses still pass (they observe
independent batch events, not their streams). The ordering witness's
count assertion did fail under the same inverse (limited credit; it
still asserts no values or order).

Required:
- Reliability witnesses assert the named stream's **exact values,
  sequence/order, completeness and no duplicates** at the receiver, with
  real selected loss and the recovery evidence (NACK/retransmit counters
  rising). Add the fire-and-forget counterpart: loss observed, no
  retransmit. A reliable-recovery-off inverse (disable NACK handling)
  must fail. Kyra's suppress-the-sends inverse must fail.
- Conservation witness: accepted == written + discarded_at_close +
  retained, exact, including clearing a forced `write(false)` and then
  observing delivery.
- Exercise the actual callers under pressure: a scheduled send, a public
  publish, an nRPC call, a stream send, a forwarded packet, a
  retransmission — each asserting its stated disposition. The 56-row
  table is inventory, not proof.
- Route injected `ConnectionReset` through the **same** handler as real
  `recv` errors (not a separate counter branch); assert sibling sessions
  keep delivering.
- Prove the idle-buffer-refresh arm specifically: a peer with a
  non-empty queue and no writes must have its advisory decay; ordinary
  pumping may not satisfy it.
- CI: pin per-binary counts (`rtc_loopback` ≥ N, `rtc_backpressure` ≥ M)
  and each required witness name, `--no-tests=fail`, retries 0 for the
  RTC binaries in `.config/nextest.toml`.
- Correct the report's fairness claim: `pump_peer` loops until
  empty/refused/closed and signal processing drains until empty — no
  explicit work quantum. Establish a **bounded service policy** (a
  per-peer quantum per outer turn) and a witness with a continuously
  busy peer, a sibling, a timer and ingress all making progress, while
  preserving str0m's mutate-then-drain ordering.
- Scope the retry slot honestly: it is finite storage **outside** the
  released queue reservation; say so, and do not claim the queue-only
  counter includes it.

## Carried criteria — test the native contract now, without Stage 4

Kyra: deferral was not a technical prerequisite. Build the three-node
native fixture: A and B each with a UDP relay link to R and
`connect_via`; the existing in-process SDP exchange for the RTC pair.
Phase-tagged delivery witness:

1. actual pre-direct **routed** delivery A→R→B;
2. **quiescent replacement** by the real RTC/Noise pair through the
   ordinary installer (CAS + quiescence gate, per R3-E);
3. direct delivery, forced DataChannel loss, explicit interruption and
   cleanup (peer-removal path, stale handle refused);
4. a **new routed handshake** and actual restored routed delivery, with
   the stale session/handle rejected.

Label step 4 **manual restoration**, not automatic fallback. If
automatic recovery is deferred, name the unimplemented owner/contract
in §11 explicitly. Likewise re-run the existing two-node **nRPC** and
**fold** consumer witnesses over RTC setup with their reply/state
assertions intact — a disposition class is not an nRPC response.

Not requested: `0x0D02`, bootstrap/enrollment, browser admission,
browser interop.

## Validation

Everything in `S3_BRIEF.md` plus: `RUSTDOCFLAGS="-D warnings" cargo doc
-p net-mesh-wire --no-deps` (default features), the standalone consumer
graph of R5-A (a throwaway crate under `spikes/tools/`, three
configurations), the seven FFI members' clippy, the RTC binaries with
`--no-tests=fail --retries 0`, every inverse named above run and shown
red then restored (hash-checked), and the export checker on a fresh
cdylib. Reply with the repair-candidate hash, the per-item proof lines,
the inverse results, and the validation list. Then stop — **no Stage 4**.
