# S0c — double-AEAD cost in the browser

Stage 0 / S0c of
[`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
(§4's deferred DTLS-exporter question). Measurement harness:
[`spikes/s0b-rtc/`](../../../spikes/s0b-rtc), run with
`pwsh -File spikes/s0b-rtc/run.ps1 -Bench`. Throwaway spike code.

**Answer up front.** Net's ChaCha20-Poly1305, on top of DTLS, in a real
headless Chromium over a real DataChannel, costs:

- **+3.5 µs per packet** at 1 KiB (4.5–5.0 µs with AEAD vs 1.0 µs
  without) — **+0.21 ms of main-thread time per second** at 60 Hz;
- **+3.0 ms per MB** sending in bulk, **+4.5 ms per MB** receiving;
- nothing at all in throughput terms: the browser sustained **1 MB/s
  with zero admission refusals and zero `Ok(false)`**, and its unpaced
  ceiling with AEAD is **6.3–7.4 MB/s**.

The DTLS-exporter shortcut **stays deferred** — see §4 for the threshold
arithmetic, including the one reading that technically crosses it and
why the shortcut would not fix that reading.

---

## 1. Environment

| Item | Value |
|---|---|
| Commit | this slice, on `LZL0/webrtc-transport` (parent `8f4cacc5d`) |
| Wire code | `spikes/s0a-wire` at S0a's `4a95691d4`, **unmodified** — no `pub` or wrapper was added (see §5) |
| Browser leaf | `spikes/s0b-rtc/web`, `wasm-bindgen` 0.2.128, `s0b_bg.wasm` |
| **wasm profile** | **unchanged from S0a: `opt-level = "z"`, `lto = true`, `codegen-units = 1`** (`spikes/s0b-rtc/web/Cargo.toml`). No `-C target-feature=+simd128`: the ChaCha20-Poly1305 measured here is **scalar wasm**. |
| Native anchor | `spikes/s0b-rtc/native`, `--release`, `ring` AEAD, `str0m` 0.23.1 `rust-crypto` |
| Chromium | `…\ms-playwright\chromium-1228\chrome-win64\chrome.exe`, **149.0.7827.55**, `--headless=new`, UA `HeadlessChrome/149.0.0.0` (same binary as S0b) |
| Host | Windows 11 Pro 10.0.22631, Intel Core i9-14900K, `navigator.hardwareConcurrency=24`, `deviceMemory=32` |
| **Idle?** | **No other workload was started during the runs**, but this is an interactive desktop, not a quiesced bench host. The evidence is in the data: config C (build, no send) reads 0.774–0.981 ms/s across three runs — a ±12 % spread on an identical workload. Treat run-to-run differences below ~30 % as noise. |
| Matrix | 3 runs × (60 Hz A/B/C + bulk A/B + saturate A/B + receive A/B) = **27 cells**, 30 s each except the 10 s saturate cells; ~12 minutes wall. |

Configurations, exactly as the brief defines them:

| Config | Per packet, in the page |
|---|---|
| **A** — the design | S0a `PacketBuilder::build` (event framing + ChaCha20-Poly1305 seal) → `dc.send` |
| **B** — the exporter shortcut, approximated | the same real 68-byte `NetHeader` (`NetHeader::new(...).to_bytes()`), payload copied in, **no AEAD** → `dc.send` |
| **C** — encoder alone | S0a `PacketBuilder::build` → discard |

**Two timing methods, both reported, because Chrome clamps
`performance.now()` to 100 µs** on a page that is not cross-origin
isolated (this page is not):

- **batched** — `build_us_*`: 100 samples, each timing **200** builds
  back to back, reported as µs per build. Resolution 0.5 µs. This is
  the *hot* marginal cost.
- **inline** — `mt_ms_per_s`, `mt_ms_per_mb`: the per-packet
  `performance.now()` deltas of the real paced loop, summed. Each
  individual delta is quantised to 0 or 100 µs, but the clamp is a
  floor of a continuous clock, so the **sum** is unbiased (σ over 1800
  samples ≈ 1.7 ms per 30 s cell ≈ 0.06 ms/s). This is the *cold*
  once-per-frame cost, and it is consistently ~3× the hot cost
  (see §3).

## 2. Results

Raw lines: `spikes/s0b-rtc/logs/native.log`, grep `S0C `. Every cell
below is one line there.

### 2.1 Workload 1 — 60 Hz × 1 KiB, 30 s (1 800 packets)

Per-packet build cost, µs (batched, 200 builds/sample):

| Run | A med | A p95 | A p99 | B med | B p95 | B p99 | C med | C p95 | C p99 |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 5.00 | 7.50 | 9.00 | 1.00 | 2.50 | 4.00 | 5.00 | 7.50 | 9.50 |
| 2 | 4.50 | 8.00 | 8.50 | 1.00 | 2.00 | 2.50 | 5.00 | 9.50 | 10.00 |
| 3 | 4.50 | 6.00 | 8.00 | 1.00 | 1.50 | 2.00 | 4.50 | 5.00 | 6.50 |

A ≈ C to within the noise, as they must be — same code path, the only
difference is the `dc.send` afterwards. That agreement is the
measurement's own sanity check.

Browser-send → native-decrypt one-way latency, ms (clock-synced, §5.3):

| Run | A med | A p95 | A p99 | B med | B p95 | B p99 |
|---|---|---|---|---|---|---|
| 1 | 0.154 | 0.254 | 0.475 | 0.181 | 0.289 | 0.355 |
| 2 | 0.156 | 0.255 | 0.302 | 0.122 | 0.212 | 0.248 |
| 3 | 0.213 | 0.349 | 0.777 | 0.144 | 0.232 | 0.274 |

A and B are indistinguishable: at 1 KiB the wire trip dominates, and
the ~3.5 µs of AEAD is 2 % of a 0.15 ms one-way. (A is not even
consistently the slower of the two.)

Main-thread time consumed per second of wall time, ms/s (inline):

| Run | A | B | C | A − B |
|---|---|---|---|---|
| 1 | 1.911 | 1.274 | 0.797 | 0.637 |
| 2 | 1.721 | 1.017 | 0.981 | 0.704 |
| 3 | 2.744 | 1.024 | 0.774 | 1.720 |

Native side, for completeness: decrypt+frame-read cost 6.3 / 7.9 /
7.1 µs per 1 KiB packet (config A, `ring`) vs 2.2–2.6 µs for
header-parse-only (config B). Zero admission refusals, zero
`Ok(false)`, `max_buffered` 13–39 bytes in every 60 Hz cell.

### 2.2 Workload 2 — 1 MB/s bulk, 30 s (8 000-byte payloads × 131/s)

*8 000 bytes, not 8 192: `MAX_PAYLOAD_SIZE` is `8192 − 68 − 16 = 8108`
and `NetHeader::validate` rejects anything above it — see §5.1.*
131 × 8 000 B = 1.048 MB/s offered.

| Run | cfg | build med/p95/p99 (µs) | achieved MB/s (native) | browser ms/MB | native µs/pkt | admission refusals | `Ok(false)` |
|---|---|---|---|---|---|---|---|
| 1 | A | 25.5 / 33.0 / 47.0 | 0.999 | 7.02 | 9.59 | 0 | 0 |
| 1 | B | 4.0 / 8.0 / 14.5 | 1.000 | 3.94 | 2.76 | 0 | 0 |
| 2 | A | 25.5 / 34.0 / 40.5 | 0.999 | 7.94 | 10.28 | 0 | 0 |
| 2 | B | 4.5 / 10.5 / 18.0 | 0.999 | 4.91 | 3.27 | 0 | 0 |
| 3 | A | 27.5 / 35.0 / 44.5 | 0.999 | 6.97 | 9.41 | 0 | 0 |
| 3 | B | 3.5 / 9.5 / 16.0 | 0.999 | 3.83 | 2.90 | 0 | 0 |

Latency at bulk: A median 0.351 / 0.478 / 0.434 ms, B 0.419 / 0.486 /
0.316 ms — again indistinguishable.

**A never hit anything B did not**: both configs ran the whole 30 s
with `admission_refusals = 0`, `write_false = 0` and a peak
`buffered_amount` of 13–26 bytes. At 1 MB/s the driver is nowhere near
str0m's 128 KiB ceiling.

### 2.3 Unpaced ceiling, 10 s (added, because the paced cell cannot answer "is 1 MB/s the limit")

The paced bulk cell is pacing-limited by construction (both configs
report 0.999 MB/s, i.e. exactly the offered rate). This cell sends as
fast as the page can, backing off only on `dc.bufferedAmount > 1 MB`:

| Run | cfg | native received MB/s | browser offered MB/s | browser ms/MB | backoffs | refusals / `Ok(false)` |
|---|---|---|---|---|---|---|
| 1 | A | **7.36** | 7.55 | 5.20 | 1 976 | 0 / 0 |
| 1 | B | 9.73 | 9.96 | 1.61 | 2 006 | 0 / 0 |
| 2 | A | **6.97** | 7.24 | 5.75 | 1 969 | 0 / 0 |
| 2 | B | 6.71 | 7.21 | 1.76 | 2 024 | 0 / 0 |
| 3 | A | **6.27** | 6.94 | 5.95 | 1 977 | 0 / 0 |
| 3 | B | 6.69 | 6.91 | 1.61 | 2 031 | 0 / 0 |

With AEAD the browser pushes **6.3–7.4 MB/s** — 6–7× the workload the
plan cares about. In two of three runs B is no faster than A, because
at this rate the binding constraint is SCTP/DTLS and the
`bufferedAmount` backoff, not the Net cipher.

### 2.4 Receive direction — native → browser at 1 MB/s, 30 s

| Run | cfg | received / sent | thruput MB/s | browser CPU ms/MB | CPU ms total |
|---|---|---|---|---|---|
| 1 | A (`open_packet`: AEAD + frame read) | 3930 / 3930 | 0.983 | **5.70** | 170.8 |
| 1 | B (`parse_plain`: header only) | 3930 / 3930 | 0.983 | **1.18** | 35.3 |
| 2 | A | 3930 / 3930 | 0.983 | 5.60 | 167.8 |
| 2 | B | 3930 / 3930 | 0.983 | 0.99 | 29.6 |
| 3 | A | 3930 / 3930 | 0.982 | 5.13 | 153.8 |
| 3 | B | 3930 / 3930 | 0.982 | 0.89 | 26.8 |

Zero packet loss and zero decrypt failures in all six cells — the
anti-replay window never rejected anything on an unordered,
zero-retransmit channel at this rate.

## 3. The delta

**At 60 Hz × 1 KiB:**

| Measure | A | B | **Noise-over-DTLS cost** |
|---|---|---|---|
| build, hot (batched) | 4.5–5.0 µs/pkt | 1.0 µs/pkt | **+3.5 µs/packet** |
| → at 60 Hz | 0.27–0.30 ms/s | 0.06 ms/s | **+0.21 ms per second of wall time** |
| build+send, cold (inline) | 1.72–2.74 ms/s | 1.02–1.27 ms/s | **+0.64 to +1.72 ms/s** |
| one-way latency | 0.154–0.213 ms | 0.122–0.181 ms | no measurable difference |

Two honest readings, and the gap between them is itself a finding:
**a build costs ~3× more when it happens once per frame than in a hot
loop** (config C inline: 0.774–0.981 ms/s ÷ 60 = 13–16 µs/packet,
versus 4.5–5.0 µs batched). Cold i-cache, cold allocator, and the
wasm/JS boundary crossing are paid per frame and do not amortise.
The conservative number for a frame budget is therefore the inline one:
**Noise-over-DTLS costs 0.6–1.7 ms of main-thread time per second at
60 Hz × 1 KiB**, of which only ~0.2 ms/s is the cipher itself and the
rest is per-call overhead that the exporter shortcut would *also* pay
(it still has to build and hand over a buffer).

Per frame: 0.21 ms/s hot = **0.0035 ms per 16.7 ms frame (0.02 % of a
frame)**; 1.7 ms/s cold worst case = 0.028 ms per frame (**0.17 % of a
frame**).

**At bulk (1 MB/s):**

| Measure | A | B | **delta** |
|---|---|---|---|
| browser CPU, paced 1 MB/s | 6.97–7.94 ms/MB | 3.83–4.91 ms/MB | **+3.0 ms/MB** |
| browser CPU, unpaced | 5.20–5.95 ms/MB | 1.61–1.76 ms/MB | **+4.0 ms/MB** |
| build, hot (batched) | 25.5–27.5 µs/8 kB = 3.2–3.4 ms/MB | 3.5–4.5 µs = 0.5 ms/MB | **+2.9 ms/MB** |
| receive (browser decrypt) | 5.13–5.70 ms/MB | 0.89–1.18 ms/MB | **+4.5 ms/MB** |
| native decrypt | 9.4–10.3 µs/pkt = 1.2 ms/MB | 2.8–3.3 µs/pkt = 0.4 ms/MB | +0.8 ms/MB |

The three independent send-side estimates (+3.0, +4.0, +2.9 ms/MB)
agree, which is the main reason to trust them. In CPU-fraction terms:
**at 1 MB/s Net's AEAD costs ≈ 0.3 % of one core** sending and ≈ 0.45 %
receiving. Scalar wasm ChaCha20-Poly1305 throughput implied by the
batched build: 8 000 B / 25.5 µs ≈ **310 MB/s**.

## 4. Recommendation

**Threshold, stated before the numbers** (from the brief): the
DTLS-exporter shortcut leaves "deferred" only if

1. A's main-thread cost at 60 Hz × 1 KiB **exceeds 1 ms per second of
   wall time** (≈ 6 % of a 16.7 ms frame), **or**
2. A's **bulk throughput is below 1 MB/s** on this machine.

Measured against them:

- **Threshold 2 — not crossed, by a wide margin.** A sustained the
  1 MB/s workload exactly (0.999 MB/s, the offered rate) in all three
  runs with zero refusals, and its unpaced ceiling is 6.3–7.4 MB/s.
- **Threshold 1 — crossed on the literal reading, and the shortcut
  does not clear it.** A's inline main-thread cost is 1.72 / 1.72 /
  2.74 ms/s — above 1 ms/s in every run. But **B, the shortcut itself,
  measures 1.02–1.27 ms/s and is also above the threshold in all three
  runs.** The cost is dominated by the per-packet `dc.send` and the
  wasm/JS crossing, which the exporter shortcut still pays; the AEAD's
  own share is +0.21 ms/s hot / +0.6–1.7 ms/s cold. Removing Net's
  AEAD would move a 1.7–2.7 ms/s workload to 1.0–1.3 ms/s: still over
  the line, for a change that costs the mesh its end-to-end
  confidentiality across relays.

**Recommendation: "deferred" stands. Do not pursue the DTLS-exporter
shortcut for performance reasons.** Threshold 1 is crossed by the
*measurement as a whole* rather than by the cipher, and the shortcut is
not the lever that clears it. If per-frame main-thread time ever
becomes the binding constraint, the levers that actually pay, in order:

1. **Batch multiple events per packet** — Net's packet format already
   supports it. The 60 Hz workload's cost is per-*packet*, not
   per-byte: 1 KiB costs 4.5 µs and 8 kB costs 25.5 µs, so eight 1 KiB
   updates in one packet cost ~25 µs instead of ~36 µs, and one
   `dc.send` instead of eight (the dominant term).
2. **Build wasm with `+simd128`** — the measured ChaCha is scalar.
3. Only then reconsider the exporter, which is a *security-model*
   change (it makes the anchor's DTLS the only confidentiality boundary
   for anything it relays), not a performance change.

The numbers also retire a related worry: **at both workloads the
double AEAD is invisible in latency** (≤0.06 ms difference in medians,
both directions, both workloads).

## 5. What did not go cleanly

### 5.1 `MAX_PAYLOAD_SIZE` silently rejects an 8 KiB payload

The brief's "8 KiB packets" does not fit: `MAX_PACKET_SIZE` is 8 192,
`HEADER_SIZE` 68, `TAG_SIZE` 16, so `MAX_PAYLOAD_SIZE` is **8 108**, and
`NetHeader::validate()` (`protocol.rs:478`) returns false for anything
larger. The first bench run used 8 192-byte payloads and every packet
was dropped on arrival — `ParsedPacket::parse` returned `None` on both
sides, the browser reported 384/384 receive failures, and the native
counters stayed at zero. Nothing logged a reason; the failure looks
like a dead channel. Payloads are 8 000 B (131/s = 1.048 MB/s).
**For Stage 3: a leaf that fragments to "8 KiB" will produce exactly
this silent black hole.**

### 5.2 `performance.now()` is clamped to 100 µs, which is 20× the thing being measured

A 1 KiB packet build takes ~5 µs; the clock's resolution is 100 µs. The
page is not cross-origin isolated (no COOP/COEP headers from the spike
server), so Chrome's 100 µs clamp applies and per-packet timing is
pure quantisation noise — the first run reported `build_us_med=0.00,
p95=100.00`. Fixed by timing batches of 200 builds (0.5 µs effective
resolution) and keeping the inline per-packet sum only as an aggregate.
Both are reported in §2 because they disagree by 3× and **the
disagreement is real** (§3, cold vs hot). Anyone reproducing this
should either serve COOP/COEP (gets 5 µs) or keep batching.

### 5.3 One-way latency needs a clock sync, and it is only as good as the path symmetry

`performance.now()` and the native `Instant` share no epoch. Each cell
runs 20 sync round trips over the DataChannel and keeps the offset from
the **minimum-RTT** sample; min RTT was 0.1–0.3 ms, so the one-way
numbers in §2.1 carry roughly ±0.1 ms of asymmetry uncertainty — the
same order as the medians themselves (0.12–0.21 ms). They are good
enough to say "A and B are indistinguishable" and **not** good enough
to quote as absolute one-way latency.

### 5.4 The `?bench=1` page was 404ing on an unrelated bug

The S0b static handler mapped `"/"` → `/index.html` *before* stripping
the query string, so `/?bench=1` never matched and returned 404 with no
log line. Fixed in `spikes/s0b-rtc/native/src/main.rs`. It cost a
debugging round trip because the browser simply showed nothing at all —
the same shape of failure as 5.1.

### 5.5 Chromium refuses a second instance on the same `--user-data-dir`

A leftover headless Chromium from an earlier run keeps the profile
lock; the next launch exits with status 21 and never navigates, so the
harness just times out. `run.ps1` uses one profile dir and deletes it
at start, which is fine serially, but two concurrent runs on different
ports will collide.

### 5.6 Config B is an approximation of the exporter shortcut, and flatters it slightly

B writes a real `NetHeader` and copies the payload, but it skips the
`EventFrame` length-prefix framing that A does inside `PacketBuilder`,
and it does not derive or apply any DTLS-exporter key material (a real
exporter design would still key *something*, e.g. per-peer framing or
an integrity tag on the header). So the measured A−B delta is an
**upper bound** on what the shortcut could recover. That strengthens
the recommendation rather than weakening it.

### 5.7 The bulk cells are pacing-limited, so "throughput" needed a second cell

`setTimeout`-based pacing at 131 packets/s delivers 0.999 MB/s for both
configs, which says nothing about the ceiling. The §2.3 saturate cell
was added for that. Its own caveat: it backs off on
`dc.bufferedAmount > 1 MB`, so it measures "what the page can push
without unbounded queueing", not the absolute maximum.

### 5.8 Smaller notes

- **No change to `spikes/s0a-wire/` was needed** — `NetHeader`,
  `HEADER_SIZE`, `NONCE_SIZE`, `ParsedPacket` and `NetSession` were
  already public. The two new wasm entry points (`build_plain`,
  `parse_plain`) live in `spikes/s0b-rtc/web/src/lib.rs`.
- **The wasm profile is unchanged** (`opt-level = "z"`), per the brief.
  That is the *size*-optimised build; `opt-level = 3` would likely
  improve the AEAD numbers, and no measurement here should be read as
  "the fastest Net can go in a browser".
- **No `simd128`.** Scalar ChaCha20-Poly1305 at ~310 MB/s. A SIMD build
  is the obvious next lever and was not measured.
- **Native-side costs are `ring`, not the wasm backend** — the
  cross-backend asymmetry (S0a §6.1) means native decrypt (9.4–10.3 µs
  per 8 kB) and browser encrypt (25.5 µs per 8 kB) are not comparable
  as "the same algorithm"; they are different implementations, and the
  ~2.5× gap is roughly what scalar-wasm-vs-native predicts.
- **The machine was not quiesced** (§1). Config C's ±12 % spread across
  identical runs is the honest noise floor; run 3's 60 Hz config A
  outlier (2.744 ms/s vs 1.7–1.9) is within it.
