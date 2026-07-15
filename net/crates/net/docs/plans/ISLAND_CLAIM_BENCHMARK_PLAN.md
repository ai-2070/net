# Island / Gang Claim Benchmark Plan (ICB) — v0.2

**Status:** v0.2 DRAFT — revised per Kyra's 2026-07-15 19:48 review (HOLD FOR v0.2). All twelve
corrections folded (§0). Still pre-implementation, no code. Review disposition: **ICB direction
APPROVED; matcher / local-claim measurement APPROVED WITH CORRECTIONS; distributed AP convergence
BLOCKED (merge is arrival-order-dependent); distributed fallback BLOCKED (losers do not recognize
loss); recovery framing CHANGES REQUESTED.** The revised suite is a **divergence diagnostic**, not
a convergence-latency suite.
**Provenance:** Kyra's 2026-07-15 recommendation + review. Sibling to
`CAPABILITY_PROPAGATION_BENCHMARK_PLAN.md` (CPB) and `SENSING_INTEREST_COALESCING_PLAN.md`; the
system under test is `MESH_SCHEDULER_GANG_CLAIM_PLAN.md` (Thunderdome).
**Sequencing (Kyra):** finish the payment measurement/storage disposition first, then start ICB.
Disposition-only; commits no code. Kyra's post-ICB priority: (2) route-failure → first successful
replacement RPC, (3) Dataforts remote materialization, (4) durable-job dispatch and recovery.

**What review exposed (the load-bearing finding):** the plan's v0.1 commercial headline — *concurrent
requests begin → stable non-conflicting allocation across every observer* — **is not currently
implemented.** `ReservationFold::merge` is arrival-order-dependent across publishers; there is no
total-order tie-break, authoritative host CAS, or on-wire quorum on the `MeshNode::reserve_island`
path. That makes the v0.1 "observers converge / losers recognize loss / allocation completes"
assertions false. This is the most valuable thing the suite can produce: it identifies the missing
distributed-arbitration primitive **before** a fleet-level claim is made publicly. ICB v0.2
measures the *degree of divergence*, and the ICB-7 close-out produces an architecture disposition
— it does **not** choose or build an authority model. **No arbitration plumbing inside the
benchmark workstream** (Kyra, closing line).

---

## 0. v0.2 disposition — the six equivalences and the twelve corrections

The gang-claim substrate is optimistic AP on the hot path; CP exists only on `→ Active`, and that
edge is **not wired on the benchmarked path** (in-process `ReplicaCohort` only; no on-wire
quorum-ack RPC — `MESH_SCHEDULER_GANG_CLAIM_PLAN.md` "Not yet built"). Six naïve readings are false;
a published ICB row must never assert any of them. Every one is confirmed against code (file:line
in §2 and the table).

```
reserve_island returns Won (awaits fan-out) ≠ local commit ≠ remote visibility   (E1)
all concurrent claims delivered             ≠ a common converged holder           (E2 — the finding)
direct-peer fold broadcast                  ≠ mesh-wide gossip / relay            (E3)
minimum-unit eligibility                    ≠ whole-island reservation           (E4)
input candidate population                  ≠ viable output population           (E5)
deadline-enabled takeover                   ≠ automatic reclaim (≠ runtime TTL sweep) (E6)
```

| # | Kyra's correction | Landing |
|---|---|---|
| 1 | `ReservationFold::merge` is non-commutative for concurrent fresh reserves: `merge(merge(∅,A),B)=A` but `merge(merge(∅,B),A)=B`. No tie-break token. v0.1's "observers converge / losers recognize loss / allocation completes / losers rematch" are **false**. | **E2 rewritten** to "all concurrent claims delivered ≠ common converged holder"; "authoritative-for-the-AP-view" removed. **ICB-3 becomes a divergence diagnostic** (§4). Reason stated as arrival-order-sensitive merge, not merely temporary optimism. |
| 2 | `Active` authority is **not** on the benchmarked path: `reserve_island` = local CAS + broadcast; no on-wire quorum-ack; `gang::claim`'s `Active` is a plain optimistic CAS; the cohort is in-process. | ICB stays **entirely on `Reserved`** and treats distributed contention as an architecture diagnostic. No `→ Active` measurement. The plan must not imply the benchmarked `Reserved` path is reconciled later by an on-wire `Active` edge — that edge is not available in this topology. §3 D2, §6. |
| 3 | `publish_fold_broadcast` sends only to `self.peers`; the inbound `SUBPROTOCOL_FOLD` router applies locally and does **not** rebroadcast. `A↔R↔B` does not deliver A's reservation to B. Distributed full-view ⇒ a full logical-peer graph; 128 schedulers ⇒ thousands of sessions. | **ICB-0 delivery preflight** (§4); report **direct vs routed logical peers separately**; never call ordinary fold fan-out "gossip beyond connected peers"; no star-topology "everyone converged". Distributed matrix = **2 / 4 / 8 / 16** claimants + reported logical session count; **remove 128** from the distributed full-view matrix (128 only as a same-node CAS-pressure diagnostic, **not** called distributed). |
| 4 | ICB-3 must be a divergence benchmark; a simultaneous "all show one holder" read doesn't prove convergence (later packets); rejected applies generally emit **no** fold-watch event, so an exact-holder watcher can't prove all competitors were processed. | ICB-3 endpoint = **all unique claim announcements delivered → inspect every participant's final local holder** via a **bench-local counting router** (wraps the `FoldRegistry`, decodes reservation envelopes, tracks unique `(publisher, island, generation)`, delegates to the real registry, wakes when all expected claims routed). Snapshot holder agreement only after full delivery. Bench-only. §4 ICB-0/ICB-3. |
| 5 | Distributed fallback does not exist: a claimant that locally inserted itself never sees itself as loser, so "local Won → foreign winner arrives → local fold flips away → rematch" cannot happen. | **ICB-4 = known-state fallback** (pre-converge every scheduler on an already-held island A → `claim_island` attempts A → local CAS `Lost` → walk to B → B commits → observer sees B), labeled "fallback from a pre-converged reservation". Alternative: defer ICB-4. **No benchmark-side winner-coordinator** reported as Net's behavior. §4 ICB-4. |
| 6 | `reserve_island` awaits `publish_fold_broadcast` (encode + fan-out + UDP send) **before** returning, so "claim call → `Won` return" includes broadcast time — it is not local-commit latency. | **ICB-2** subscribes first, actively polls the claim future, **independently** awaits the local exact-holder read (stops the local timer there), then asserts the future returned `Won`. Yields two boundaries from one start: "→ local exact-holder visibility" and "→ `reserve_island` returns after fan-out attempt". §4 ICB-2, E1. |
| 7 | A `Reserved` deadline passing emits **no** transition: `until_unix_us` is consulted only when a foreign `Reserved` is applied; time passing does not change the entry, fire a watch, or change the queried holder. | ICB-6 cannot measure "deadline fires → converge → rematch". Honest boundary = **deadline-enabled takeover**: configured deadline wait (policy, reported separately) / first foreign takeover CAS returns `Won` (mechanism) / observer reads new holder (visibility). Not "automatic reclaim." §4 ICB-6, E6. |
| 8 | Runtime entry TTL (`DEFAULT_TTL = 30 s`, removes the whole entry, fires a sweep watch on reap) is distinct from `until_unix_us` (leaves the entry present, no event) and from the sweep **cadence** (`DEFAULT_SWEEP_INTERVAL = 500 ms`). "30 s store sweep" was wrong. Removal is local, not broadcast; `Active` entries also carry the runtime TTL. | ICB-6 has **two separately labeled groups**: deadline-enabled takeover + runtime-entry expiry (announcement TTL + 500 ms sweep → entry absent from this observer, watch fires on reap). Default 30 s case → small sample, no p99; fast diagnostic → deliberately short envelope TTL only if the fixture sets it without changing production defaults. §4 ICB-6, §2. |
| 9 | "Candidates examined" overclaims internal execution work from a separate, mixed-time query. | Rename to **matched hosts / candidate islands before numeric filter (bench-reconstructed) / viable islands returned**. E5 = "input candidate population ≠ viable output population". The fixture is static during a matcher row, so the reconstructed *input population* is honest. §4 ICB-1, E5. |
| 10 | ICB-1 claimed both `match_islands` and `match_islands_sensed`, but its target is `required-features = ["net"]`; the sensed path is ICB-5 (`net redex`). | **ICB-1 = ordinary matcher scaling only**; **ICB-5 = sensed** projection/ordering/fallback. Removes duplicate axes and the feature inconsistency. §4 ICB-1/ICB-5. |
| 11 | Claims are stateful; samples need clean state, and capability/island fixtures carry runtime TTLs that can silently expire mid-run. | **Fixture-reset + TTL discipline** (§4 ICB-0, applied everywhere): uncontended → release + `await exact Free` on every observer outside timing; distributed-race → **fresh island id (or fresh fixtures) per sample**; matcher → assert exact capability-host + island population **before and after** every timed batch, refreshing outside timing or with a long explicit fixture TTL. |
| 12 | Do not expand the **closed** CPB harness into a general framework — later ICB changes would regress the CPB suite. | New **ICB-local harness `benches/bench_island_claim/mod.rs`**. It may copy or call narrow generic pair/runtime/reporting helpers, but reservation-delivery accounting, full-mesh construction, fresh-island allocation, and conflict reports are ICB-local. §3 D1. |

---

## 1. Intent — the headline v0.1 could not honestly claim, and the one it can

The current system **cannot** support:

> concurrent requests begin → stable, non-conflicting allocation across every observer.

Merge is arrival-order-dependent (item 1), so there is no converged holder to time. What v0.2
honestly measures:

> Net measures **local GPU-island claim latency**, **direct reservation visibility**, **matcher
> scaling**, **known-state fallback**, and the **degree of holder divergence** produced by
> simultaneous cross-node claims.

Four distinct boundaries the repository cannot currently quote (placement.rs is a *scorer*; CPB-2
is one island *reacting* to propagation; the gang tests are *correctness*):

> **1. Match cost** — converged fixture → ranked eligible islands (matched hosts → candidate
>    islands → viable output).
> **2. Claim latency** — a single claim, split three ways from one start: **local commit** (exact
>    local holder), **API return** (`reserve_island` returns after the fan-out attempt), **remote
>    visibility** (a direct observer's exact holder). E1.
> **3. Known-state fallback** — a pre-converged held island → `Lost` → walk to the next candidate.
> **4. Holder divergence under simultaneous cross-node claims** — the diagnostic that replaces the
>    BLOCKED "contended allocation completion" headline.

Pre-ICB-7 acceptance is **orientation only** (CPB C11): valid distributions; correct matcher
populations asserted before/after each batch; local-commit / API-return / remote-visibility kept
distinct; contention across **distinct nodes** with delivery **proven** by the counting router (not
assumed); divergence reported honestly (distinct holders may equal N); zero timeouts.

---

## 2. Grounding — honest boundaries, no new plumbing (verified file:line)

Every endpoint is public; the benches are dev-only, observation-only, and add nothing to
production. The corrections below are about *where the timer stops*, *what a counter may claim*, and
*what actually gets delivered*.

| Boundary | Endpoint | Discipline (verified) |
|---|---|---|
| Match result (ICB-1) | `MeshNode::match_islands(&MatchCriteria) -> Vec<IslandId>` (`mesh.rs:18970`) | Ungated (`net`). Returns the ranked viable list only. Matched-hosts / candidate-islands are **bench-reconstructed** from `capability_fold().query(...)` and `island_fold().query(IslandQuery::HostedByAny)` — E5. |
| Local commit (ICB-2) | subscribe `reservation_fold().subscribe_changes()` → poll the claim future → **independently** await exact-holder read `query(ReservationQuery::State(island)).holder() == self` | E1/item 6: `reserve_island` (`mesh.rs:19058`) applies the CAS then `await`s `publish_fold_broadcast` **before** returning (`apply_and_broadcast_reservation`, `mesh.rs:19148→19157→19164`) — the return includes fan-out, so stop the local timer on the independent exact-holder read, not the return. |
| API return (ICB-2) | `reserve_island(...).await` returning `ClaimOutcome::Won` | The `Won`/`Lost` **verdict** is decided solely by the local CAS; a broadcast failure is logged, not surfaced. Report the return time as "→ returns after fan-out attempt", never as local commit. |
| Remote / observer visibility (ICB-2/3/6) | observer's `reservation_fold().subscribe_changes()` wake + exact-holder read | Poll-free; `signal_changed()` fires under the write lock, so stop only after the exact-holder read. **Rejected applies emit no watch event** (item 4) → use the counting router, not the watch, to prove full delivery. |
| Delivery reach (ICB-0) | `publish_fold_broadcast` targets `self.peers` only (`mesh.rs:16606-16639`); inbound `SUBPROTOCOL_FOLD` applies locally, **no** rebroadcast (`mesh.rs:9437-9467`) | E3/item 3: `A↔R↔B` does not deliver A→B. A routed row is valid only if A holds a logical peer session to B that `publish_fold_broadcast` enumerates — **proven by the ICB-0 preflight**. Report direct vs routed logical peers separately. |
| Distributed completion (ICB-3) | **bench-local counting router** wrapping the `FoldRegistry`: decode reservation envelopes, track unique `(publisher, island, generation)`, delegate to the real registry, wake when all expected claims routed | Bench-only instrumentation, no production plumbing. The only honest "all competitors processed" signal, since rejected merges are silent. |
| Deadline takeover (ICB-6) | `Reserved{until_unix_us}` consulted **only** on foreign apply (`reservation.rs:237`, sole call site of `reservation_expired`, `reservation.rs:333`) | E6/item 7: time passing past the deadline changes nothing and fires no watch. Measure the **first foreign takeover claim**, not a "deadline fired" event. |
| Runtime expiry (ICB-6) | `DEFAULT_TTL = 30 s` (`reservation.rs:170`) removes the whole entry; swept on `DEFAULT_SWEEP_INTERVAL = 500 ms` (`expiry.rs:35`); reap fires the **local** watch (`expiry.rs:179-183`), not a broadcast | E6/item 8: distinct from `until_unix_us`. `Active` entries carry the same runtime TTL (`mod.rs:729-741`), so an unrefreshed `Active` is swept at 30 s too. |

**Harness discipline:** contention runs from **distinct transport-connected `MeshNode`s**, never
concurrent tasks on one node — same-node claimants serialize on one write lock and yield a
deterministic single winner (`first_claim_wins_on_concurrent_reservation`), which would falsely read
as distributed arbitration (E2). Public-API node builds mirror `tests/common::connect_pair` and the
live wiring in `tests/gang_claim_node.rs`; islands are seeded via the public `IslandRecord` publish
path; down-hosts via `set_liveness_down`.

---

## 3. Decisions (v0.2)

**D1 — Location: core `net` crate; ICB-local harness (item 12). APPROVED-with-scope.**
New `benches/bench_island_claim/mod.rs` (delivery accounting, full-mesh construction, fresh-island
allocation, `ContentionReport`), reusing narrow generic pair/runtime/reporting helpers by copy/call
— **not** by expanding the closed CPB `bench_mesh_pair`. Targets:
```
benches/island_claim_match.rs        ICB-1        required-features ["net"]
benches/island_claim_contention.rs   ICB-2/3/4    required-features ["net"]
benches/island_claim_sensed.rs       ICB-5        required-features ["net", "redex"]
benches/island_claim_recovery.rs     ICB-6        required-features ["net"]
```

**D2 — Reserved-only divergence diagnostic (items 1, 2). REPLACES v0.1's convergence premise.**
ICB stays entirely on the `Reserved` path. Cross-node merge is arrival-order-dependent, so there is
**no convergence to time** — ICB-3 measures divergence. **No `→ Active` measurement** (no on-wire
quorum-ack; in-process cohort only). The plan states plainly: *an in-process quorum/fencing model
exists, but no cross-node authoritative activation is wired into the benchmarked reservation path*;
it must not imply the `Reserved` path is safely reconciled later by an on-wire `Active` edge. **No
arbitration plumbing in the benchmark.**

**D3 — Bench-reconstructed candidate accounting, renamed (item 9).** Columns: **matched hosts** /
**candidate islands before numeric filter (bench-reconstructed)** / **viable islands returned**. No
"candidates examined", no production counter or return-shape change on `match_islands`.

**D4 — Recovery = two labeled mechanisms (items 7, 8).** (a) **deadline-enabled takeover** — no
"deadline fired" event exists; measure the first foreign takeover CAS + observer visibility, with the
configured deadline wait reported separately as policy. (b) **runtime-entry expiry** — 30 s TTL
removes the entry, 500 ms sweep cadence, reap fires a local watch, removal is local (not broadcast).
Neither is a peer-death reclaim.

**D5 — Compile gate: existing broad Clippy job already covers it (mirrors CPB D3). APPROVED.**
`cargo clippy --all-features --all-targets` compile-gates all four targets; no new CI step. Targeted
`--no-run` compiles are **local**:
```
cargo bench -p net-mesh --bench island_claim_match      --features net         --no-run
cargo bench -p net-mesh --bench island_claim_contention --features net         --no-run
cargo bench -p net-mesh --bench island_claim_sensed     --features "net redex" --no-run
cargo bench -p net-mesh --bench island_claim_recovery   --features net         --no-run
```

**D6 — Delivery preflight + counting router (items 3, 4).** ICB-0 proves direct/routed logical
delivery before any timed row; the bench-local counting router provides the honest stable endpoint
for divergence snapshots (snapshot only after all expected `(publisher, island, generation)`
deliveries). Both are bench-only.

---

## 4. Phase breakdown (v0.2 — Kyra's revised shape)

### ICB-0 — ICB-local harness, delivery preflight, exact holder await, unique-delivery counting
`benches/bench_island_claim/mod.rs`: (a) distinct-node / full-mesh builders (public API); (b) a
**direct/routed delivery preflight** that asserts A's reservation actually reaches each intended
observer before any timed row (E3) and records the logical session count; (c) an
`await_reservation_holder(rx, fold, island, expected)` exact-holder await (E1 boundary); (d) the
**counting router** (D6) wrapping the `FoldRegistry`, tracking unique `(publisher, island,
generation)` deliveries and waking on all-expected-delivered; (e) **fixture-reset + population
discipline** (item 11) — release + `await exact Free` on every observer outside timing; fresh island
id per distributed-race sample; assert exact capability-host + island population before/after every
matcher batch; (f) a `ContentionReport` (hdrhistogram + the §5 metadata).
**Acceptance:** targeted `--no-run` compiles; a single-claim smoke records local-commit via the
exact-holder endpoint (zero timeouts); the preflight proves (or refuses) routed delivery; the
counting router wakes exactly when the expected unique claims have arrived.

### ICB-1 — Ordinary matcher scaling
`benches/island_claim_match.rs` (`net`). Real `match_islands` across **10 / 100 / 1000 islands ×
1 / 8 / 72 units × sparse / dense** capability matches. Report **matched hosts → candidate islands
before numeric filter (bench-reconstructed, D3) → viable islands returned**, with match p50/p95/p99.
**No sensed path here** (item 10 — that is ICB-5). Assert exact host + island population before and
after each timed batch (item 11). Every row: "minimum-unit **eligibility** → whole-island
reservation" (E4).
**Acceptance:** distributions across all axes; population asserted stable across each batch;
matched-hosts / candidate-islands / viable-returned distinct and correctly labeled; no sensed axis;
no whole-vs-subset ambiguity.

### ICB-2 — Single-claimant boundaries (three from one start)
`benches/island_claim_contention.rs`, topology + capabilities pre-converged. From one claim start
(item 6, E1): (a) **local commit** — subscribe first, poll the claim future, independently await the
local exact-holder read, stop the timer there, then assert the future returned `Won`; (b) **API
return** — `reserve_island` returns after the fan-out attempt; (c) **remote visibility** — a direct
observer's exact holder. Topologies: `scheduler ↔ host`, and `scheduler ↔ relay ↔ host/observer`
**only if the ICB-0 preflight proves routed logical delivery** (E3); routed reported separately.
Release + `await exact Free` on every observer outside timing (item 11).
**Acceptance:** three distinct numbers from one start; routed row present only when preflight-proven;
exact-holder correctness; clean fixture reset between samples.

### ICB-3 — Distributed simultaneous-claim divergence diagnostic
`benches/island_claim_contention.rs`. **2 / 4 / 8 / 16 distinct-node claimants** (report logical
session count; **no 128** in the distributed matrix — item 3), **fresh island id per sample**. Fire
concurrent fresh claims; the **counting router** waits until every participant has received every
unique `(publisher, island, generation)` claim; *then* snapshot each participant's final local
holder. Report: **optimistic local `Won` count**; **distinct claimant-local holders**; **observer
holders**; **largest agreement cohort**; **observer agreement ratio**; **claimant self-belief
count**; **foreign claims rejected**; **split-view duration** across the observation window;
**whether agreement occurred at all**; **timeouts**. Expected result (the finding): optimistic
winners = N, distinct claimant-local holders = N, observer holders arrival-order-dependent, **common
converged holder absent** (E2). Separately, a **128-task same-node CAS-pressure diagnostic** — an
honest local-write-lock throughput number, explicitly **not** called distributed contention.
**Acceptance:** snapshots taken only after full delivery (counting router); divergence reported
honestly (not a failure); the same-node 128 diagnostic clearly labeled as non-distributed.

### ICB-4 — Known-state fallback (walk to next candidate)
`benches/island_claim_contention.rs`. **Distributed simultaneous-race fallback is BLOCKED** (item 5
— a self-inserter never sees itself lose). Instead: **pre-converge every scheduler on an
already-held island A**, then time `claim_island` attempts A → local CAS returns `Lost` → the claim
loop walks to B → B commits locally → a direct observer sees B. Label **"fallback from a
pre-converged reservation"**; state that it measures the existing `claim_island` walk-to-next
behavior, **not** resolution of a simultaneous distributed race. Report: retries per successful
claim, fallback latency, final allocation spread. **No benchmark-side winner-coordinator.**
*Alternative disposition:* defer ICB-4 until an authoritative/tie-broken arbitration mechanism
exists (revisit at ICB-7).
**Acceptance:** every scheduler ends with a non-conflicting reservation via the walk; labeled as
known-state fallback; no coordinator; distributed-race framing explicitly disclaimed.

### ICB-5 — Sensed selection through a single claim / fallback
`benches/island_claim_sensed.rs` (`net redex`). Ordinary vs **sensed** `claim_island_sensed`, using
**opposing island-load order and sensed-readiness order** exactly as the witness does
(`tests/sensing_scheduler_bridge.rs` — `sensed_readiness_leads_the_claim_order_and_never_suspends`).
Report: sensed-projection overhead; whether `selected_provider()` receives the **first** claim
(proved separately); fallback cost when the selected provider loses readiness/reservation;
readiness change → successful replacement claim. Single-claimant throughout (distributed convergence
is out — D2). **Does not restate capability-propagation latency** (CPB owns it — §6).
**Acceptance:** sensed-led order verified against the opposing seed; first claim targets the selected
provider; replacement-after-readiness-loss measured; no propagation number restated.

### ICB-6 — Deadline-enabled takeover + runtime-entry expiry (separately labeled)
`benches/island_claim_recovery.rs` (`net`). Two labeled groups (items 7, 8; E6):
- **Deadline-enabled takeover** — `Reserved` until a deadline → a foreign claim **after** the
  deadline → new holder visible. No "deadline fired" event exists, so report: configured deadline
  wait (**policy, separate**) / first foreign takeover CAS returns `Won` (**mechanism**) / observer
  reads new holder (**visibility**). Call it takeover, **not** automatic reclaim.
- **Runtime-entry expiry** — announcement TTL + the 500 ms sweep → entry **absent** from this
  observer (the reap fires a local watch; removal is local, not broadcast). Default 30 s case → small
  sample, **no p99**; a fast diagnostic uses a deliberately short envelope TTL **only if** the
  fixture can set it without changing production defaults. Note `Active` also carries the runtime TTL.
**Acceptance:** configured TTL/deadline is a separate column from mechanism and visibility; the two
groups are distinctly labeled; the deadline group makes no "automatic reclaim" claim.

### ICB-7 — Baselines, derived thresholds, docs, and architecture disposition
Run each bench on a quiet machine; **derive** thresholds from observed data (never invented — CPB
C11). Cargo `[[bench]]` entries + `required-features`; confirm the broad Clippy gate covers the
targets (D5). Cross-link `docs/SENSING.md` / `MESH_SCHEDULER_GANG_CLAIM_PLAN.md`; append §7 as-built;
update memory; full check pass; push. **If ICB-3 confirms divergence, produce an architecture
disposition** enumerating candidate authority models — single scheduler authority · host-authoritative
claim RPC · deterministic convergent reservation merge · or defer distributed `Reserved` authority
to the on-wire `Active`/quorum path — **without choosing or implementing one** (the benchmark phase
must not).
**Acceptance:** baseline table recorded; thresholds data-justified; targeted gates green; the six
equivalences confirmed as-built; the architecture disposition written iff divergence is confirmed.

---

## 5. What we measure vs. what we report

Every row names its **exact start event** and **exact endpoint**, keeps **local commit** / **API
return** / **remote visibility** distinct (E1), snapshots divergence **only after the counting
router confirms full delivery** (item 4), and carries the required metadata:

- island population; eligible population; units/island;
- claimant count **and logical session count**; topology + relay count; direct vs routed (preflight-proven);
- selection policy; sensed vs ordinary; reservation deadline / runtime TTL (as distinct fields);
- p50 / p95 / p99 (no p99 for small default-TTL runs);
- attempts/s; optimistic local `Won`; distinct claimant-local holders; largest agreement cohort;
  observer agreement ratio; foreign claims rejected; split-view duration; timeouts;
- matcher rows: matched hosts / candidate islands before numeric filter (bench-reconstructed) /
  viable islands returned.

Divergence is a *reported result*, not a failure. Mechanism (the reservation CAS + direct-peer
fan-out) stays separate from policy (selection order, deadline/TTL sizing).

---

## 6. Non-goals (v0.2)

- **Distributed AP convergence / stable non-conflicting allocation** — BLOCKED (merge is
  arrival-order-dependent, item 1). ICB reports **divergence**, not convergence.
- **Distributed fallback / loser-recognition** — BLOCKED (a self-inserter never sees loss, item 5).
  ICB-4 is **known-state fallback** only; no benchmark-side winner-coordinator.
- **Any `→ Active` / on-wire quorum timing** — in-process `ReplicaCohort` only; not measured (item 2).
- **Routed delivery assumed from a chain topology** — must be **preflight-proven** (item 3); no
  "gossip beyond connected peers"; no star-topology convergence claim.
- **128 distributed claimants** — logical-session blow-up (item 3); 128 appears only as a same-node
  CAS-pressure diagnostic, labeled non-distributed.
- **Capability publication latency** — CPB owns it; ICB-5 composes, never restates it.
- **Actual GPU process startup; utilization-uplift without a workload; external scheduler / K8s /
  Volcano / Kueue comparisons; multi-machine rack claims from localhost** — all out for v0.1.
- **No new production plumbing** — benches are dev-only, observation-only; the counting router and
  preflight are bench-side. **No arbitration plumbing inside the benchmark workstream** (Kyra).

Does not duplicate: `benches/placement.rs` (scorer), CPB-2 (one island reacting to propagation),
`tests/gang_claim_node.rs` / `contention.rs` / `multi.rs` / `proptest.rs` / `loom_models.rs` (all
correctness).

---

## 7. As-built (ICB-7) — files, baselines, thresholds, CI, architecture disposition

*Reserved. Filled at the ICB-7 close-out with the reference-machine baseline table, data-derived
regression thresholds, the CI note, the six-equivalences-as-built confirmation, and — iff ICB-3
confirms divergence — the architecture disposition (candidate authority models, none chosen).
Empty until the suite is built and measured.*
