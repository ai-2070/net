# Slice 2 — Stage 0 / S0b: RTC loop spike

Source of truth: `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`
(HEAD of this branch — re-read §2, §3, §6, §Stage 0 S0b and §Dependencies;
§7 now carries your S0a findings). Only Stage 0 is authorized. This slice
is S0b only; S0c (double-AEAD cost) is the next slice — leave a hook for it
(the browser page must be able to send N packets of size M at rate R and
report timings) but do not measure yet.

## S0a review verdict (for context)

Accepted at `4a95691d4`. Every claim reproduced (test lines, wasm sizes,
`cargo check --target wasm32`). The plan was updated from your report
(§Context ring note, §7 "What S0a established", Stage 2 scope/exit,
sequencing correction). One gap, now yours in S0b: the wasm build was
compiled but **never executed** — `wasm_wire_probe`, the `chacha20poly1305`
AEAD path, `web_time`, and snow's default-resolver on wasm have not run
anywhere. Your own §4 finding (`Instant::now()` compiles then panics on
wasm32) is exactly why execution is the proof. S0b runs that code in a real
browser.

## Goal

Validate the §2 driver ownership shape end to end with a real browser:
str0m `Rtc` behind one owning loop on a dedicated UDP socket, a headless
Chromium page running the S0a wire crate via `wasm-bindgen`, a DataChannel
between them, then a Noise NKpsk0 handshake and one reliable-stream
round-trip **over the DataChannel** using the S0a code on both ends. Both
ICE roles. Answer the four yes/no questions the plan lists.

## Target

- `spikes/s0b-rtc/native/` — a scratch binary crate (own `Cargo.toml`, own
  `[workspace]`, not a member of `net/crates/net`). Depends on
  `../../s0a-wire` by path and on
  `str0m = { version = "0.23.1", default-features = false, features = ["rust-crypto"] }`
  (the pinned configuration from §Dependencies — **do not** use defaults).
  tokio is allowed here (native anchor side only); std threads are fine too.
- `spikes/s0b-rtc/web/` — the browser side: a `wasm-bindgen` cdylib that
  wraps `s0a-wire` (`Cargo.toml` with `[workspace]`, path-dep on
  `../../s0a-wire`), plus a minimal static page + JS that opens
  `RTCPeerConnection`, negotiates via a tiny HTTP signalling endpoint served
  by the native binary, opens one DataChannel with
  `{ ordered: false, maxRetransmits: 0 }`, and drives the wasm handshake +
  round-trip. Use `wasm-pack` or `wasm-bindgen-cli` — record which and the
  version.
- `spikes/s0b-rtc/run.ps1` (and/or `run.sh`) — builds both, starts the
  native binary, launches headless Chromium against it, waits for the
  verdict line, exits 0/1. Chromium: use whatever is installed (Playwright's
  bundled Chromium via `npx playwright`, or a system Chrome with
  `--headless=new`); record the exact binary and version.

## Change

1. **Driver loop (§2 shape).** One task/thread owns the UDP socket and every
   `Rtc`. Follow str0m's single-mutation invariant exactly: mutate →
   `poll_output` drained to `Output::Timeout` → next mutation. Outbound Net
   packets arrive on a bounded per-peer queue (`crossbeam`/`std::sync::mpsc`
   with capacity, say 256); the driver pops one, calls `Channel::write`,
   drains. Inbound DataChannel data comes out of `poll_output` as Net packet
   bytes and is pushed to a bounded ingress queue the "mesh side" consumes.
   No shared locks around `Rtc`.
2. **Admission probe.** Before each `Channel::write`, read
   `Channel::buffered_amount()` and publish it (atomic is fine for the spike)
   so the send side can consult it. Then deliberately drive the channel
   past a small threshold with the browser side paused (`sleep` in the page
   before reading) and record: (a) does `write` ever return `Ok(false)`, and
   under what `buffered_amount`; (b) how stale the published reading is
   relative to the actual value at the next drain; (c) what happens to
   queued packets when the channel closes mid-backlog. This is evidence for
   §2 "advisory reading vs reserved-bytes bound" — report numbers, not
   adjectives.
3. **Both roles.** Run the whole sequence twice: native as ICE
   controlled/answerer (browser offers — the anchor bootstrap shape), and
   native as controlling/offerer (browser answers — the native-initiator ↔
   leaf shape §9 needs). str0m is developed as an SFU; its README says p2p
   "has received less testing". Report any asymmetry.
4. **Handshake + round-trip over the channel.** Browser side: generate the
   initiator keys in wasm (this exercises `getrandom` `wasm_js` and snow's
   default-resolver for real), run NKpsk0 against the native responder's
   static key, build a Net packet with the S0a `PacketBuilder`, send it as
   one DataChannel message, receive the native side's reply packet, decrypt,
   assert. Then the reverse direction. Both must go through the S0a
   `NetSession` on both ends. **This executes the wasm AEAD/clock paths for
   the first time; if anything panics, that is a finding, not a blocker —
   record it and work around it in the spike.**
5. **Four questions, each answered yes/no with evidence:**
   - Does str0m 0.23.1 support ICE-TCP passive candidates? (Search its API
     for TCP candidate types / `Candidate::…` constructors; try one. Cite
     the type or the absence.)
   - Is `RTCPeerConnection` available in a `SharedWorker` and/or dedicated
     `Worker` in the Chromium you ran? (Instantiate it inside each; report
     the exact error text or success, and the Chromium version.)
   - Does the pinned `rust-crypto` configuration build cleanly on this
     Windows host with no C toolchain involvement? (`cargo build -vv` and
     confirm no `cc`/`cmake`/`nasm` invocations for `str0m*`.)
   - Trickle vs gather-complete: measure offer-created → DataChannel-open
     wall time with candidates trickled over the signalling endpoint vs
     with a single POST after `icegatheringstatechange == complete`. Five
     runs each, report min/median/max. This decides Stage 4's WebSocket.
6. **Report** to `docs/internal/spikes/S0B_RTC_LOOP.md`, in this order:
   - commit hash of `s0a-wire` used; str0m version + feature set actually
     resolved (`cargo tree -p str0m -e features`); toolchain, wasm-bindgen
     tool + version, Chromium binary + version, OS;
   - the driver loop shape as implemented (a short diagram or pseudo-code
     of the ownership and the two bounded queues) and whether §2's contract
     survived contact — what you had to change;
   - admission-probe numbers from step 2;
   - both-roles result and any asymmetry from step 3;
   - the round-trip verdict lines from both directions, and every wasm
     runtime surprise from step 4 (panics, missing imports, `web_time`,
     RNG);
   - the four answers from step 5 with evidence;
   - "did not go cleanly" — same discipline as S0a §6.

## Constraints

- Only `spikes/**` and `docs/internal/spikes/**` may change. Do not touch
  `net/**`, `go/**`, `web/**`, `.github/**`, or any plan document. You may
  make small additive changes inside `spikes/s0a-wire/` if S0b needs an
  extra `pub` or a wasm-bindgen-friendly wrapper — list each in the report.
- Do not add either scratch crate to any workspace; do not edit any existing
  `Cargo.toml`/`Cargo.lock` outside `spikes/`.
- Skip formatters, linters, and the project-wide test suite.
- No third-party STUN/TURN. Loopback/host candidates only — this is the
  loop spike, not the NAT spike.
- Commit on `LZL0/webrtc-transport` with prefix `spike(s0b):`. One commit
  is fine. `git status` clean afterwards.
- Then reply in the terminal with: the commit hash, the two round-trip
  verdict lines (one per direction, each role), the admission-probe
  numbers, the four yes/no answers, and the "did not go cleanly" list
  verbatim.

## Acceptance

- `run.ps1` exits 0 and prints, from the browser console, one line per
  direction/role of the form
  `S0B OK role=<answerer|offerer> dir=<b2n|n2b> payload=<n> bytes`.
- `S0B_RTC_LOOP.md` exists with all seven sections; every number in it is
  reproducible from the scripts in `spikes/s0b-rtc/`.
- The wasm AEAD (`chacha20poly1305`), `web_time` clock, snow
  default-resolver and `getrandom` `wasm_js` paths are shown to have
  executed in the browser (the round-trip cannot complete otherwise; say so
  explicitly in the report, and list any runtime panic you hit on the way).
- `git diff --stat HEAD~1` touches only `spikes/` and
  `docs/internal/spikes/`.
