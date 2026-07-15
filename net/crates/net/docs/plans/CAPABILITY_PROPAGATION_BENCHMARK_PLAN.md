# Capability Propagation Benchmark Plan (CPB) — v0.1 DRAFT

**Status:** DRAFT — awaiting sign-off on the three decisions in §3 before CPB-0 starts.
**Provenance:** Kyra's 2026-07-15 review ("we want these benchmarks now"). Sibling to
`SENSING_INTEREST_COALESCING_PLAN.md`; the burst/coalescing benchmark (CPB-4) is the
standing regression guard for the delayed-sleeper ownership fixes shipped in PR #557
(SI-6.1 / SI-6.1R2 fold-gate token work).

---

## 1. Intent — the number we cannot currently quote

The existing suite measures announcement *construction*, serialization, fold *insertion*,
queries, and placement *scoring* (all Criterion microbenches in `net/crates/net/benches/`).
None of them measures the latency a user actually experiences:

> A capability changes on node A. How long until node B can make a different scheduling
> decision because of it?

Integration tests prove *convergence* but poll every 20–25 ms against multi-second
deadlines — they cannot tell us whether a path takes 400 µs, 8 ms, or 80 ms. This plan
adds end-to-end latency benchmarks that measure from **local mutation commit** through
**remote scheduling visibility**, so the public claim is a measured boundary, never
"capability announcements take X."

The deliverable sentence this evidence must support:

> Net measures capability changes from publication through remote scheduling visibility,
> rather than quoting serialization or in-process scheduler overhead as end-to-end latency.

---

## 2. Grounding — the measurement is already honest without new plumbing

The pivotal correctness question (Kyra: "polling `find_nodes_by_filter` would quantize the
measurement and make the result dishonest") is **already answered by public API**. No new
subscription surface is needed. Every endpoint below is `pub` and reachable from a bench
(integration tests already use them the same way).

| Boundary | Poll-free endpoint | Location | Notes |
|---|---|---|---|
| Remote fold exposes the new version | `Fold::subscribe_changes() -> watch::Receiver<u64>` | `behavior/fold/mod.rs:321` | `signal_changed()` fires **after** the entry is inserted and query-visible (`apply` Insert `:383`, Replace `:439`) — the wake marks the exact instant B can observe it. Missed-wakeup-safe (`tokio::sync::watch`). |
| Scheduler input regenerated | `MeshNode::subscribe_sensing_scheduler_inputs() -> watch::Receiver<u64>` | `mesh.rs:6081` | Bumped from the inbound capability handler at `mesh.rs:14837`, **gated on `enable_sensing_coalescing`** (config default `false`) — B must be built `.with_sensing_coalescing(true)`. Aggregates several planes, so a wake is attributed to the capability change by re-checking the fold generation. |
| Candidate population (recompute) | `find_nodes_by_filter(&filter)` / `sensed_candidates(...)` | `mesh.rs:18064` / `6049` | Population is recomputed on demand — one recompute post-wake, never a poll loop. |
| Announcements actually emitted | `MeshNode::capability_version()` (delta) | `mesh.rs:11496` | Doc: "delta as an announce-call count (e.g. proving the RT-3 debounce)." Origin-side coalescing efficacy = version delta over N mutations. |
| Bytes sent | derived: `announcements_emitted × encoded_announcement_bytes` | — | Encode a `CapabilityAnnouncement` and take `.len()`; the honest coalescing/traffic figure. No transport byte-counter required (see Decision D2). |

Supporting fixtures (all public API):
- `connect_pair(&a, &b)` pattern (`tests/common/mod.rs:115`) — accept+connect+start, public-API
  only (`node_id`/`public_key`/`local_addr`/`accept`/`connect`). Replicated into the bench
  fixture (benches cannot `mod` a `tests/` module).
- `announce_capabilities(caps)` (`mesh.rs:16769`) broadcasts **immediately**, bypassing the
  debounced auto-announcer — the wire-floor driver. The change-driven auto-announcer
  (`spawn_capability_announce_on_change_loop`, `mesh.rs:7310`, parks on
  `subscribe_local_caps_changes`) with default `announce_debounce = 100 ms` /
  `min_announce_interval = 10 s` is the production-convergence driver.
- hdrhistogram print pattern: `sdk/benches/nrpc_churn.rs` (`Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)`, record ns, `value_at_quantile` for p50/p95/p99/p99.9 + `max()`/`mean()`).

---

## 3. Decisions to confirm before CPB-0

**D1 — Location: honor Kyra's named paths (`net/crates/net/benches/`).**
The two-node latency-bench pattern (hdrhistogram + a handshaken `Pair`) currently lives only
in the **SDK crate** (`sdk/benches/nrpc_common`), which wraps `Mesh`, not `MeshNode`. But the
`net` crate demonstrably stands up two transport-connected `MeshNode`s through public API
(`tests/common::connect_pair`), and hdrhistogram is a one-line dev-dep add. Kyra was explicit:
"the first belongs to the mesh/capability layer; the second should exercise the real fold and
scheduler bridge." Both point at the `net` crate.
→ **Recommendation:** put the benches in `net/crates/net/benches/` (Kyra's paths), with a
self-contained node-pair fixture (a `#[path]`-included `bench_mesh_pair/mod.rs`, mirroring how
`sdk/benches` includes `nrpc_common`). *Alternative:* SDK crate reusing `Pair` — rejected, wrong
layer and reaches the fold only via `.inner()`.

**D2 — "Bytes sent" for the burst bench: derived, not a new transport counter.**
Report `bytes_sent = announcements_emitted × encoded_announcement_bytes`. This is the honest
coalescing/traffic-reduction figure and needs no new observability. *Alternative:* wire a real
socket byte-counter — rejected as scope creep unless you want true on-wire bytes (including
retransmits/framing); say so and I'll check `control_plane_stats`/`traversal_stats` for an
existing counter first.

**D3 — CI: add a `cargo bench --no-run` compile gate, do not run benches in CI.**
Benches are `cargo bench` only and never run in CI today (so `sensing_signature` etc. can
bit-rot). These new benches gate PR #557's fixes, so a cheap compile check is worth it.
→ **Recommendation:** add one `cargo bench --no-run` step over the new bench targets to the
existing bench/build job. *Alternative:* leave CI untouched — rejected, invites silent rot.

**Naming:** phases `CPB-0..6`. Plan doc: this file. Prefix chosen to sit beside `SI-*`/`RT-*`.

---

## 4. Phase breakdown

Each phase is one reviewable unit with a red-green acceptance check, following the standing
working pattern (disposition-first, one commit per sub-phase, as-built note at close).

### CPB-0 — Shared harness + reporting spine
`benches/bench_mesh_pair/mod.rs` (`#[path]`-included by every capability bench). Contents:
- Node-pair / node-chain builders on public API (adapt `connect_pair`): `pair()` (A↔B direct),
  `chain()` (A↔R↔B routed), `fan_out(n)` (A + n consumers), `intake(n)` (n providers + 1 consumer).
- Config builder exposing the knobs Kyra requires: `sensing_coalescing`, `announce_debounce`,
  `min_announce_interval`.
- GPU capability-manifest fixtures: `manifest_small()` and `manifest_realistic_gpu()` (a
  plausible multi-tag GPU worker: model tags, VRAM/hardware summary, service readiness), with
  their encoded byte sizes exposed.
- `LatencyReport` wrapping hdrhistogram: `record(ns)`, and a `print_row` emitting
  **p50/p95/p99/p99.9/max/mean** plus the metadata columns Kyra requires on every result:
  **topology, manifest bytes, hop count, debounce config, announcement count, candidate population.**
- `hdrhistogram = "7"` added to `[dev-dependencies]` of `net/crates/net/Cargo.toml`.

**Acceptance:** fixture compiles under `cargo bench --no-run`; a smoke run stands up A↔B and
records one non-zero sample.

### CPB-1 — Capability propagation latency (mechanism / wire floor)
`benches/capability_propagation.rs`, wire-floor mode: `announce_capabilities` direct (or knobs
= 0). On B: snapshot `subscribe_changes()`, trigger A's mutation, `rx.changed().await`, record
**local-commit → wake** ns; confirm the new version with one `find_nodes_by_filter` post-wake.
- Cases: **added / updated / removed** × **{direct A→B, two-hop A→R→B}** × **{small, realistic GPU}**.
- Report hop count + manifest bytes per row.

**Acceptance:** all six-plus cases produce distributions; direct-added wire floor is sub-ms
locally (Kyra's target hierarchy); two-hop shows a bounded additive hop cost.

### CPB-2 — Scheduler reaction latency (the commercially meaningful number)
`benches/capability_scheduler_reaction.rs`. B built `.with_sensing_coalescing(true)`. Continue
the CPB-1 measurement to `subscribe_sensing_scheduler_inputs()` wake, attribute the wake to the
capability change by re-checking the fold generation, then recompute candidate population and
classify the effect: **provider appears / disappears / changes rank**.
- Cases: appears / disappears / rank-change × {one-hop, routed}. Report candidate population
  before → after.

**Acceptance:** one-hop reaction is low single-digit ms; routed = one-hop + bounded hop cost;
population transitions match the injected mutation.

### CPB-3 — Production-convergence mode
Second mode in `capability_propagation.rs`: drive a **real tool/service/GPU registry mutation**
(`serve_tool`) through the change-driven auto-announcer with **normal** debounce/coalescing.
Report the configured `announce_debounce` **beside** the result so a deliberate 100 ms policy
window is never mistaken for transport latency.

**Acceptance:** production-convergence latency is debounce-dominated and reported next to the
debounce value; wire-floor and production numbers are clearly labeled as mechanism vs policy.

### CPB-4 — Burst / coalescing benchmark (guards PR #557)
`benches/capability_burst.rs`. Bursts of **1 / 16 / 128** related mutations (model loaded →
VRAM changed → cache warm → service ready). Measure:
- time until B sees the **final state** (await fold generation, then verify the re-queried
  membership equals the final version — not merely "a wake happened");
- **announcements emitted** (`capability_version` delta on the origin);
- **bytes sent** (D2 derivation);
- **final-version correctness** (B's membership == the last mutation);
- **leading-to-trailing convergence** — assert the burst collapses toward **one leading + one
  trailing** emission (the coalescer's contract, and the delayed-sleeper ownership guarantee).

**Acceptance:** 128-mutation burst yields ≈1 leading + 1 trailing announcement (not 128),
final state correct, and stays green under repeat runs (the anti-flake guard for #557).

### CPB-5 — Topology matrix: fan-out + intake pressure
Extend the fixture: **A → 16 direct consumers** (per-consumer propagation distribution across
16 `subscribe_changes` receivers) and **16 providers → 1 consumer** (scheduler-input reaction
under concurrent fold churn; candidate population = 16). Multi-machine rack-level runs are
explicitly out of scope for v0.1 (localhost first: both endpoints share a monotonic clock,
eliminating clock-sync error).

**Acceptance:** fan-out distribution reported as a percentile spread; intake-pressure reaction
stays bounded as concurrent providers scale.

### CPB-6 — Baselines, thresholds, close-out
- Run each bench on a quiet machine; capture observed distributions into a short results section
  (this doc or `docs/`), **per Kyra: do not invent hard budgets first** — derive regression
  thresholds from observed behavior.
- Cargo `[[bench]]` entries + minimal `required-features` (confirmed at impl; D3 CI gate).
- Cross-link from `docs/SENSING.md` / `docs/BEHAVIOR.md`; plan as-built; memory update; full
  check pass (fmt + both clippy steps + rustdoc gate); push.

**Acceptance:** baseline table recorded; thresholds justified by data, not asserted a priori;
all gates green.

---

## 5. What we measure vs. what we report

Every published row states **exactly which boundary was measured**. Two modes are always kept
separate so policy latency never masquerades as mechanism latency:

- **Wire floor** (mechanism): explicit announce, `min_announce_interval = 0`,
  `announce_debounce = 0` — isolates transport, decode, fold apply (and, in CPB-2, scheduler wake).
- **Production convergence** (policy): real registry mutation, normal debounce/coalescing —
  what an application actually sees, with the debounce printed alongside.

Reported columns per result: p50 / p95 / p99 / p99.9 / max / mean, plus topology, manifest bytes,
hop count, debounce config, announcement count, candidate population.

Initial target hierarchy (from Kyra — orientation only, not regression gates):
wire-floor propagation sub-ms locally · one-hop scheduler reaction low single-digit ms · routed
reaction bounded additive hop cost · production convergence debounce-dominated & explicit · burst
final-state one leading + one trailing emission.

---

## 6. Non-goals (v0.1)
- Multi-machine / rack-level runs (localhost only, for clock fidelity — revisit later).
- Real on-wire byte accounting (framing/retransmits) — derived bytes only unless D2 is overridden.
- New subscription/observability plumbing — none required (§2); if a phase turns out to need it,
  that becomes a flagged mini-disposition, not a silent addition.
- Changing any production behavior or default. Benches are `dev`-only, observation-only.
