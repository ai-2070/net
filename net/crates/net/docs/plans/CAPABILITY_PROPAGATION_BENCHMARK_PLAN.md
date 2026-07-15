# Capability Propagation Benchmark Plan (CPB) — v0.2

**Status:** v0.2 AS-BUILT — CPB-0..6 landed. Revised per Kyra's 2026-07-15 HOLD review; all
twelve corrections + D2 applied (§0). Baselines + data-derived thresholds in §7. CPB-0
authorized (Kyra: "After those edits, I approve the implementation sequence"). Plan direction
SOUND; D1 APPROVED, D3 APPROVED, D2 REJECTED-as-written → revised in §3.
**Provenance:** Kyra's 2026-07-15 review. Sibling to `SENSING_INTEREST_COALESCING_PLAN.md`.
The burst benchmark (CPB-4) is a **coalescing-efficiency** benchmark — *not* a stale-sleeper
correctness guard (that needs a deterministic time-controlled regression test; see §0 C7).

---

## 0. v0.2 disposition — the five equivalences and the twelve corrections

Kyra's thesis: v0.1 mislabeled what its timestamps and counters actually prove. Five false
equivalences must never appear in a published row:

```
watch wake              ≠ query visibility          (C1)
version delta           ≠ packets emitted           (D2, C4-note)
encoded payload size    ≠ bytes sent                (D2)
capability query        ≠ scheduler decision        (C3)
ordinary burst behavior ≠ stale-sleeper ownership   (C7)
```

| # | Correction | Landing |
|---|---|---|
| D2 | "bytes sent = count × encoded size" is dishonest (variable sizes, version≠emissions, serve_rpc over-bumps, fan-out/relay copies, framing/encryption/retransmits excluded). | Drop "bytes sent". Report instead: **remote fold updates accepted**, **final-state convergence**, **manifest size of the tested state**, and **origin version delta labeled as a version delta** (`capability_announce_version()`), never "emissions". §3 D2. |
| C1 | `signal_changed()` fires while the fold write locks are still held; a waiter can wake *before* it can read. | The measured endpoint is **watch wake → exact target state read successfully → stop timer**, never `rx.changed()` alone. Realized as `await_capability_state(rx, predicate)`; the timer stops only after the predicate's exact fold/query check returns true. Still poll-free. §2, CPB-0. |
| C2 | Public API can't timestamp the internal commit inside `announce_from_baseline`. | Start boundary is **publication/mutation API invocation**, not "commit". Headline reworded (§1). Every row named by its exact start event. |
| C3 | `find_nodes_by_filter` = capability-index reaction, not a scheduling decision. | CPB-2 ends a sample only after `match_islands`/`match_islands_sensed` returns a **changed** result over seeded island topology. Cases: viable island appears / island disappears. **Rank-change removed** (separate inputs; not smuggled in). Split CPB-2a (→ scheduler-input wake) / CPB-2b (→ recomputed match changes; the headline). |
| C4 | RT-3 auto-announce covers **tool-registry + nRPC service** mutations only; a GPU manifest baseline does not generate that local signal. | CPB-3 drives a **tool / nRPC service** mutation and says so. No live GPU registry (would change production behavior — non-goal). Note: `serve_tool` may install an RPC metadata service → another reason version-delta ≠ emission count. |
| C5 | With `min_announce_interval = 10 s`, the trailing flush can be delayed toward the 10 s floor — not "~100 ms". | Split: **debounce-only** (`announce_debounce = 100 ms`, `min_announce_interval = 0`) isolates RT-3 burst settling; **default-policy** (both default) as a *small* labeled scenario. Do not call the first "production defaults". |
| C6 | CPB-4 conflated RT-3 registry debounce with RT-1 explicit-announce rate limiting (different contracts). | Two separate benchmark groups. RT-3: 128 rapid registry mutations → ~one publication after debounce. RT-1: one leading + many in-window explicit announces → one trailing. Outcome is **not** universally "1 leading + 1 trailing". |
| C7 | An ordinary burst bench stays green while a stale delayed sleeper still owns a newer window. | CPB-4 is a coalescing-efficiency benchmark only. **Removed** the "guards PR #557" claim. Provenance corrected: PR #557's RT-1/RT-4 token fixes and SI-6.1's fold-reconciliation token work are *related patterns*, not this slice. The real guard is a deterministic time-controlled regression test (out of CPB scope). |
| C8 | Exact-state verification differs by operation. | add/remove where the tag controls membership → `find_nodes_by_filter` suffices. **update preserving membership** → read the exact fold entry / synthesized set and assert the exact version/tag/metadata. Every await loop: await change → read exact target → if absent, await next change. |
| C9 | A single node pair yields only one *cold* insert per provider; the rest are replacements. | Label samples: **cold publication** (fresh provider insert; fixture setup excluded from timing; smaller N, stated), **warm update** (existing provider replaced with newer version), **warm add/remove** (alternate equal-sized states to flip membership). Never report thousands of replacements as "cold/added". |
| C10 | p99.9 promised everywhere with no sample protocol. | Every row states: warm-up count, measured sample count, worker count, topology-reused flag, timeout count, rejected/outlier count. **p99.9 only for sufficiently large wire-floor runs**; small policy runs report p99 (p99.9 shown as `-`). |
| C11 | CPB-1 "sub-ms" acceptance conflicts with CPB-6 "derive thresholds after measurement". | Pre-CPB-6 acceptance = valid distributions, exact-state correctness, zero timeouts, clean mechanism/policy separation, routed reported separately. "sub-ms / low single-digit ms" are **orientation only** (§1), not gates. |
| C12 | Fan-out/intake need batch-completion, not first-wake. | A→16: report first-visible, **all-16-visible**, and the per-consumer distribution. 16→B: endpoint = **all 16 exact provider versions visible** → scheduler recompute sees the expected population. First scheduler-input wake ≠ completion (one watch generation coalesces several changes). |

---

## 1. Intent — the number we cannot currently quote

The existing suite measures announcement *construction*, serialization, fold *insertion*,
queries, and placement *scoring*. None measures the latency a user experiences. Reworded per
C2 (start boundary is an API invocation, not an internal commit):

> A capability **mutation begins** on node A (a publish call, or a registry mutation). How long
> until node B can make a different **scheduling decision** because of it?

Integration tests prove convergence but poll every 20–25 ms against multi-second deadlines —
they cannot resolve 400 µs vs 8 ms vs 80 ms. **Orientation only, not acceptance gates** (C11):
wire-floor visibility sub-ms locally · one-hop scheduler reaction low single-digit ms · routed =
bounded additive hop cost · default-policy convergence debounce/rate-limit-dominated.

Deliverable sentence this evidence must support:

> Net measures capability changes from publication through remote scheduling visibility, rather
> than quoting serialization or in-process scheduler overhead as end-to-end latency.

---

## 2. Grounding — honest boundaries, no new plumbing

Every endpoint is public; no subscription surface needs to be added. The corrections below are
about **where the timer stops** and **what a counter is allowed to claim**.

| Boundary | Endpoint | Discipline |
|---|---|---|
| Remote state query-visible | `capability_fold().subscribe_changes()` **wake** + an **exact-state read** | C1: `signal_changed()` runs under the write locks, so stop the timer only after `await_capability_state(rx, predicate)` returns — the predicate performing the exact fold/query check. The watch is the wake mechanism; the read is the endpoint. |
| Scheduler decision changed | `match_islands(criteria)` / `match_islands_sensed(...)` returns a **changed** result | C3: `find_nodes_by_filter` is index reaction, not a decision. Seed real `island_fold` topology; sample ends on a changed match. |
| Scheduler-input wake (CPB-2a only) | `subscribe_sensing_scheduler_inputs()` | Needs `.with_sensing_coalescing(true)`; aggregates several planes → attribute by re-checking the fold generation. |
| Origin **version delta** (NOT emissions) | `capability_announce_version()` delta | D2/C4: labeled "version delta"; over-bumps on `serve_rpc` nodes; not a packet count. |
| Candidate/consumer population | `find_nodes_by_filter` | A recompute after the endpoint read — reporting context, not the endpoint itself. |
| Manifest size of tested state | `postcard`-encoded `CapabilitySet` length | The size of the *state*, reported as such; not "bytes sent". |

Harness discipline (D1): use **`start_arc()`** (installs the weak self-ref; spawns the
change-driven announcer + deferred flush) for any RT-3 / deferred-announcement scenario. The
node-pair build (`accept`/`connect`, public-API only) mirrors `tests/common::connect_pair`.
Start boundary (C2): timer starts immediately before `announce_capabilities(...)` (publication)
or the registry mutation (`serve_tool`), never claimed as an internal commit.

---

## 3. Decisions (v0.2)

**D1 — Location: core `net` crate. APPROVED.** Files:
`benches/capability_propagation.rs`, `benches/capability_scheduler_reaction.rs`,
`benches/capability_burst.rs`, `benches/bench_mesh_pair/mod.rs`. Correct layer — direct access
to `MeshNode`, the capability fold, and the scheduler bridge. Harness uses `start_arc()`.

**D2 — "bytes sent": REJECTED as written.** Not reported. Replaced by (a) remote fold updates
accepted, (b) final-state convergence, (c) manifest size of the tested state, (d) origin
**version delta** labeled as such. If a phase uses a constant-size mutation fixture it may
report *logical payload bytes accepted by one direct consumer* — never called "wire bytes" or
"total bytes sent". A true byte figure would require an exact packet/payload counter (not in v0.1).

**D3 — Targeted compile gate: APPROVED.** Compile only the new targets with their exact features:
```
cargo bench -p net-mesh --bench capability_propagation        --features net         --no-run
cargo bench -p net-mesh --bench capability_scheduler_reaction --features "net redex" --no-run
cargo bench -p net-mesh --bench capability_burst              --features "net tool"  --no-run
```
Do **not** compile every existing bench on every PR.

**As-built override:** the repository already had a broad `--all-features --all-targets` Clippy gate
(the `clippy` CI job) covering these targets. D3's intent — prevent benchmark rot without adding
redundant CI work — is therefore satisfied by the existing gate; the targeted commands above remain
**local** verification commands, and no CI step was added. See §7.4.

---

## 4. Phase breakdown (v0.2)

### CPB-0 — Harness, exact-state await helper, reporting + sample protocol
`benches/bench_mesh_pair/mod.rs` (`#[path]`-included). Node-pair / chain builders on public API
via `start_arc()`; `BenchConfig` presets (`wire_floor`, `wire_floor_scheduler`, `debounce_only`,
`default_policy`); manifest fixtures (small + realistic GPU) with encoded size; the
`await_capability_state(rx, predicate)` helper (C1); and a `LatencyReport` that prints
p50/p95/p99/p99.9/max/mean **plus** the C10 sample protocol (warm-up, samples, workers,
topology-reused, timeouts, outliers) and the C2/D2 metadata (start-event label, topology, hop
count, manifest bytes, version delta, candidate population). p99.9 suppressed below a sample-count
floor. `hdrhistogram` added to `[dev-dependencies]`.
**Acceptance:** targeted `--no-run` compiles; a smoke run records a valid distribution via the
exact-state endpoint (warm replacement, membership-controlling tag), zero timeouts.

### CPB-1 — Publication call → remote exact-state visibility
`benches/capability_propagation.rs`, wire-floor. Timer: just before `announce_capabilities` →
`await_capability_state`. Per C8/C9:
- **warm update** (membership preserved): read the exact fold entry/version, assert the new
  version (large N).
- **warm add / remove** (membership flip via alternating equal-sized states): `find_nodes_by_filter`.
- **cold publication** (fresh provider insert; setup excluded from timing): smaller N, stated.
Axes: {direct A→B, two-hop A→R→B} × {small, realistic GPU}. Routed reported separately.
**Acceptance (C11):** valid distributions, exact-state correctness, zero timeouts, routed separate.

### CPB-2 — Publication call → real match-result change
`benches/capability_scheduler_reaction.rs`. B built `.with_sensing_coalescing(true)`; seed
`island_fold` topology.
- **CPB-2a:** publication → `subscribe_sensing_scheduler_inputs()` wake (attributed via fold gen).
- **CPB-2b (headline):** publication → `match_islands`/`match_islands_sensed` returns a **changed**
  result. Cases: provider's viable island **appears** / **disappears**. No rank-change (C3).
**Acceptance:** match result provably changes; 2a/2b reported distinctly; routed separate.

### CPB-3 — RT-3 registry mutation: debounce-only and default-policy
Second mode in `capability_propagation.rs`, driven by a **tool / nRPC service** mutation through
`start_arc()`'s change-driven announcer (C4). Two configs (C5):
- **debounce-only** (`announce_debounce = 100 ms`, `min_announce_interval = 0`) — isolates RT-3
  burst settling.
- **default-policy** (both default) — small labeled scenario (10 s samples are impractical; report
  a small distribution and say so).
Report the configured debounce/interval beside each result; never label debounce-only as
"production defaults".
**Acceptance:** mutation is registry-driven and described as such; the two policy modes are
distinctly labeled; no production behavior changed.

### CPB-4 — Coalescing efficiency: RT-3 debounce group + RT-1 rate-limit group (SEPARATE)
`benches/capability_burst.rs`. Two groups (C6), not merged:
- **RT-3 debounce:** 1 / 16 / 128 rapid **registry** mutations → expect ≈ one publication after
  the debounce; report the origin **version delta** (labeled) + remote **final-state convergence**
  (await exact final version on B).
- **RT-1 rate limit:** one leading + many in-window **explicit** announces → expect one trailing;
  report the version-delta shape.
Not a stale-sleeper guard (C7): no such claim; provenance corrected. Optional: *logical payload
bytes accepted by one direct consumer* for a constant-size fixture (D2), never "bytes sent".
**Acceptance:** the two groups are separate; final state correct; version-delta shapes reported;
green under repeat (efficiency, not correctness-of-ownership).

### CPB-5 — Fan-out all-visible + intake all-visible (batch completion, C12)
Extend the fixture: **A → 16 consumers** — report first-visible, **all-16-visible**, and the
per-consumer distribution. **16 providers → B** — endpoint is **all 16 exact provider versions
visible** → scheduler recompute sees population = 16 (never first-wake). Multi-machine out of scope.
**Acceptance:** all-visible endpoints (not first-wake); per-consumer spread reported; intake sees
the full expected population.

### CPB-6 — Baselines, thresholds, targeted gates, docs
Run each bench on a quiet machine; capture distributions; **derive** regression thresholds from
observed data (never invented). Cargo `[[bench]]` entries + minimal `required-features`; the D3
targeted `--no-run` gates into CI. Cross-link `docs/SENSING.md` / `docs/BEHAVIOR.md`; plan
as-built; memory; full check pass (fmt + both clippy steps + rustdoc); push.
**Acceptance:** baseline table recorded; thresholds data-justified; targeted gates green.

---

## 5. What we measure vs. what we report

Every row names its **exact start event** and **exact endpoint**, keeps **mechanism** (wire floor)
separate from **policy** (debounce / rate limit), and carries the C10 sample protocol. Columns:
p50 / p95 / p99 / p99.9* / max / mean, plus start-event, topology, hop count, manifest bytes,
version delta (labeled), candidate population, and (warm-up, samples, workers, topology-reused,
timeouts, outliers). *p99.9 only when samples exceed the wire-floor floor; otherwise `-` and p99
is the tail figure.

---

## 6. Non-goals (v0.1)
- Multi-machine / rack-level runs (localhost only — clock fidelity; revisit later).
- Real on-wire byte accounting (D2) — not reported; no synthetic "bytes sent".
- Stale-sleeper ownership correctness (C7) — a deterministic time-controlled regression test, not a
  benchmark.
- Any live GPU registry signal (C4) or other production-behavior change. Benches are dev-only,
  observation-only, and add no new plumbing.

---

## 7. As-built (CPB-6) — files, baselines, thresholds, CI

### 7.1 Files (all under `net/crates/net/`)
- `benches/bench_mesh_pair/mod.rs` — shared harness: `BenchConfig` presets, node-pair / chain /
  `fan_out(n)` / `intake(n)` builders (public API, `start_arc()`), manifest fixtures,
  `await_capability_state`, `LatencyReport` (hdrhistogram + sample protocol).
- `benches/capability_propagation.rs` — CPB-1 (wire-floor matrix), CPB-5 (topology), CPB-3
  (RT-3, `#[cfg(feature = "tool")]`). `required-features = ["net"]`; CPB-3 rides `--features "net tool"`.
- `benches/capability_scheduler_reaction.rs` — CPB-2a/2b. `required-features = ["net", "redex"]`.
- `benches/capability_burst.rs` — CPB-4 (RT-3 + RT-1 coalescing). `required-features = ["net", "tool"]`.
- `hdrhistogram` added to `[dev-dependencies]`.

### 7.2 Reference baselines
Localhost, single machine (both endpoints share a monotonic clock — the whole point of the
localhost run), 4 worker threads, wire-floor unless noted. **Orientation, not CI gates** — benches
do not run in CI (§7.4). Numbers are the reference-machine snapshot the thresholds derive from.

| Boundary (row) | p50 | p99 | n | notes |
|---|---|---|---|---|
| CPB-1 warm update small, direct | ~124 µs | ~187 µs | 180 | exact-state endpoint |
| CPB-1 warm update GPU (529 B), direct | ~142 µs | ~221 µs | 180 | |
| CPB-1 warm update small, routed A→R→B | ~150 µs | ~212 µs | 180 | +hop |
| CPB-1 warm update GPU, routed | ~234 µs | ~290 µs | 180 | +hop, +payload |
| CPB-1 warm add / remove, direct | ~88 / 89 µs | ~133 / 189 µs | 90 ea | membership flip |
| CPB-1 cold publication (first insert) | ~111 µs | ~172 µs | 30 | fresh pair/sample |
| CPB-2a publication → scheduler-input wake | ~138 µs | ~338 µs | 180 | attributed via fold gen |
| CPB-2b island appears/disappears, direct | ~90 µs | ~166–204 µs | 90 ea | real match change |
| CPB-2b island appears/disappears, routed | ~162–167 µs | ~268–281 µs | 90 ea | +hop |
| CPB-3 RT-3 debounce-only | ~101.6 ms | ~102.6 ms | 35 | debounce-dominated |
| CPB-3 RT-3 default-policy | ~10.0 s | — | 3 | **rate-limit-dominated (100× debounce)** |
| CPB-4 RT-3 burst 1/16/128 | conv ~101.6 ms | — | 20 ea | 1 call, 1 remote update applied, 20/20 correct |
| CPB-4 RT-1 burst 16/128 | conv ~251 ms | — | 20 ea | 16/128 calls → 2 remote updates applied, 20/20 correct |
| CPB-5 fan-out first-visible (A→16) | ~142 µs | ~216 µs | 50 | fastest consumer |
| CPB-5 fan-out all-16-visible | ~355 µs | ~441 µs | 50 | batch completion ≠ first wake |
| CPB-5 intake all-16-visible (16→B) | ~3.3 ms | ~11.4 ms | 50 | concurrent churn, population 16 |

### 7.3 Regression thresholds (derived from 7.2, per C11 — data, not invented)
Manual comparison on the reference machine; generous multiples of observed p99 so ordinary
machine/scheduler variance never trips them. A regression is a *sustained* breach, not one sample.
- Wire-floor visibility (CPB-1/2b direct): **p99 < 1 ms** (observed ≤ ~290 µs → ~3–5×).
- Routed adds a **bounded hop cost**: routed p50 ≤ 2 × direct p50 (observed ~1.2–1.6×).
- Scheduler-input wake (CPB-2a): **p99 < 1 ms** (observed ~338 µs).
- RT-3 debounce (CPB-3/4): convergence **within debounce + 20 ms** (observed ~101.6 ms @ 100 ms).
- Coalescing contracts (CPB-4): RT-3 burst → **1 remote update applied**; RT-1 burst → **≤ 2
  remote updates applied** (a fold-generation delta at B counts updates the consumer *accepted*,
  not origin broadcasts/packets); final-state correctness **must be N/N** (a hard invariant, not a
  latency threshold).
- Fan-out all-16-visible: **p99 < 2 ms**; intake all-16-visible: **p99 < 30 ms** (observed ~11.4 ms).

### 7.4 CI (D3)
The `clippy` job already runs `cargo clippy --all-features --all-targets`, and `--all-features`
enables `tool` + `redex`, so **all three CPB bench targets are already compile-gated** (harness=false
benches are checked as targets) — no separate step added (Kyra D3: don't add a redundant broad
gate; the broad gate already exists). The benches themselves are `cargo bench`-only and are not
executed in CI. Targeted local compile, for the record:
```
cargo bench -p net-mesh --bench capability_propagation        --features net        --no-run
cargo bench -p net-mesh --bench capability_scheduler_reaction --features "net redex" --no-run
cargo bench -p net-mesh --bench capability_burst              --features "net tool"  --no-run
```

### 7.5 The five equivalences, as-built
`watch wake ≠ query visibility` (exact-state await, 0 timeouts everywhere) · `version delta ≠
packets emitted` (CPB-4: 128 calls vs 2 remote updates applied) · `encoded size ≠ bytes sent` (no bytes-sent
reported) · `capability query ≠ scheduler decision` (CPB-2b ends on `match_islands` change) ·
`ordinary burst ≠ stale-sleeper ownership` (CPB-4 makes no such claim). Every published row names
its exact start event and endpoint and carries the sample protocol.
