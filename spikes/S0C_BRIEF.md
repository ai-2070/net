# Slice 3 — Stage 0 / S0c: double-AEAD cost

Source of truth: `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`
(HEAD — §2 now carries "What S0b established", §6 the mDNS note, §8 the
worker confirmation, §Dependencies the `aws-lc-sys` correction). Only
Stage 0 is authorized. This slice is S0c only.

## S0b review verdict (for context)

Accepted at `4b4d9875f`. `run.ps1` reproduced (`[run] OK`, four verdict
lines), `aws-lc-sys` chain reproduced with `cargo tree`, str0m's
`MAX_BUFFERED_ACROSS_STREAMS = 128 * 1024` confirmed in the registry
source. The plan was updated from all eight of your §7 findings (commit
on HEAD). No follow-ups owed from S0b.

## Goal

Answer §4's deferred question with a number: what does Net's
ChaCha20-Poly1305 cost *on top of* DTLS, in the browser, at the two
workloads the products care about — 60 Hz × 1 KiB (scene state) and
1 MB/s bulk (asset transfer) — and does that number move the
DTLS-exporter shortcut out of "deferred".

## Target

Reuse `spikes/s0b-rtc/` as-is; you already left `window.s0cHook({n, size,
rateHz})` and `LeafEndpoint::bench_build(n, size)`. Add whatever the
measurement needs under `spikes/s0b-rtc/` (a `bench` mode in `run.ps1`, a
`--bench` flag on the native binary, extra page code). You may make small
additive changes in `spikes/s0a-wire/` if a benchmark needs a `pub`; list
each.

## Change

Measure in the browser (headless Chromium, same binary/version as S0b —
record it), over a real DataChannel to the native driver, three
configurations per workload:

| Config | What the page does per packet |
|---|---|
| A — Noise over DTLS (the design) | S0a `PacketBuilder` (ChaCha20-Poly1305 encrypt) → `dc.send` |
| B — DTLS only (the exporter shortcut, approximated) | same 68-byte Net header, payload copied, **no AEAD** → `dc.send` |
| C — no send (encoder cost alone) | S0a `PacketBuilder` → discard |

Workloads:

1. **60 Hz × 1 KiB** for 30 s (1 800 packets). Report per-packet build
   time (median / p95 / p99, µs) for A and C, and end-to-end
   browser-send → native-decrypt latency (median / p95 / p99, ms) for A
   and B. Also report main-thread time consumed per second by A vs B
   (`performance.now()` deltas summed) — that is the number a three.js
   frame budget cares about.
2. **1 MB/s bulk** for 30 s (8 KiB packets, ~128/s). Report achieved
   throughput (native side, decrypted bytes/s) for A and B, browser CPU
   time per MB for A and B, and whether A ever hit admission refusal or
   `Ok(false)` on the driver that B did not.

Receive direction too, once: native → browser at 1 MB/s, browser-side
decrypt cost per MB (A) vs parse-only (B).

Run each cell **three times**; report all three, not an average. Note
whether the machine was otherwise idle. The wasm must be the S0a
`opt-level="z"` build — report if you change the profile and why.

## Report

`docs/internal/performance/WEBRTC_DOUBLE_AEAD.md` (the plan names
`docs/internal/performance/`), in this order:

1. environment (commit, Chromium version, wasm profile, CPU, idle?);
2. the two workload tables, A/B/C, three runs each, receive direction;
3. the delta: what Noise-over-DTLS costs relative to DTLS-only, in
   absolute µs/packet and ms/s of main-thread time at 60 Hz, and in
   CPU-per-MB at bulk;
4. **a recommendation**, with the threshold stated before the number:
   the DTLS-exporter shortcut leaves "deferred" only if A's main-thread
   cost at 60 Hz × 1 KiB exceeds 1 ms per second of wall time (≈ 6 % of a
   16.7 ms frame), or A's bulk throughput is below 1 MB/s on this machine.
   State whether either threshold was crossed; if not, "deferred" stands;
5. what did not go cleanly.

## Constraints

- Only `spikes/**`, `docs/internal/spikes/**` and
  `docs/internal/performance/**` may change. Nothing under `net/**`,
  `go/**`, `web/**`, `.github/**`, no plan document.
- Skip formatters, linters, the project-wide suite.
- Commit on `LZL0/webrtc-transport` with prefix `spike(s0c):`. `git
  status` clean after.
- Reply in the terminal with: the commit hash, the two headline deltas
  (µs/packet at 60 Hz; CPU-per-MB at bulk), the recommendation line, and
  the "did not go cleanly" list verbatim.

## Acceptance

- The bench runs from `spikes/s0b-rtc/run.ps1 -Bench` (or equivalent)
  and exits 0, printing the tables it wrote to the report.
- `docs/internal/performance/WEBRTC_DOUBLE_AEAD.md` has all five
  sections; every number is reproducible from the script.
- The recommendation cites the thresholds above and the measured numbers
  against them.
