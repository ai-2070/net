# Island / Gang Claim Benchmark Plan (ICB) — v0.3

**Status:** v0.3 DRAFT — folds Kyra's 2026-07-15 20:05 v0.2 verdict (one blocker + bounded
corrections). v0.2 was "architecturally honest"; the only substantive error was treating known-state
fallback as a concurrent multi-scheduler allocation. Still pre-implementation, no code. Review
disposition (v0.2): D1–D6 **APPROVED** (D6 with an exactness note); ICB-1/2/6 **APPROVED**; ICB-3
**APPROVED WITH CENSORING FIX**; ICB-4 **BLOCKED AS WRITTEN** → fixed to single-claimant; ICB-5
**APPROVED WITH BOUNDARY SPLIT**; ICB-7 **APPROVED WITH DOC FIXES**. After v0.3 the plan is
approvable for implementation.
**Sequencing (unchanged, Kyra):** complete P5 and the payment-storage disposition **before** starting
ICB code. Disposition-only; commits no code.
**Provenance:** Kyra's 2026-07-15 recommendation + two review rounds. Sibling to
`CAPABILITY_PROPAGATION_BENCHMARK_PLAN.md` (CPB); system under test is
`MESH_SCHEDULER_GANG_CLAIM_PLAN.md` (Thunderdome).

**The load-bearing finding (unchanged from v0.2):** `ReservationFold::merge` is arrival-order-dependent
across publishers (`reservation.rs:228-242`; the cross-publisher path skips the generation check), so
the cross-node `Reserved` path **does not converge** — there is no total-order tie-break, authoritative
host CAS, or on-wire quorum on `MeshNode::reserve_island`. ICB measures the **degree of divergence**;
the ICB-7 close-out produces an architecture disposition. **No arbitration plumbing inside the
benchmark workstream** (Kyra).

---

## 0. Disposition

### 0.1 The six equivalences (never violate in a published row)

```
reserve_island returns Won (awaits fan-out) ≠ local commit ≠ remote visibility   (E1)
all concurrent claims delivered             ≠ a common converged holder           (E2 — the finding)
direct-peer fold broadcast                  ≠ mesh-wide gossip / relay            (E3)
minimum-unit eligibility                    ≠ whole-island reservation           (E4)
input candidate population                  ≠ viable output population           (E5)
deadline-enabled takeover                   ≠ automatic reclaim (≠ runtime TTL sweep) (E6)
```

### 0.2 v0.1 → v0.2 corrections (Kyra's first HOLD — all twelve landed)

| # | Correction | Landing |
|---|---|---|
| 1 | `merge` non-commutative for concurrent fresh reserves; v0.1's convergence/loss-recognition/completion assertions are false. | E2 rewritten; ICB-3 is a divergence diagnostic. |
| 2 | `Active` authority not wired on the benchmarked path (local CAS + broadcast; no on-wire quorum-ack; in-process cohort only). | Reserved-only; no `→ Active` measurement; §3 D2, §6. |
| 3 | `publish_fold_broadcast` → `self.peers` only; inbound `SUBPROTOCOL_FOLD` applies locally, no rebroadcast; 128 ⇒ thousands of sessions. | ICB-0 delivery preflight; direct vs routed reported separately; distributed matrix 2/4/8/16. |
| 4 | ICB-3 needs a divergence benchmark + a counting router (rejected merges emit no watch event). | Bench-local counting router; snapshot only after full delivery. §3 D6. |
| 5 | Distributed fallback impossible (a self-inserter never sees itself lose). | ICB-4 = known-state fallback; no coordinator. |
| 6 | local-commit boundary includes broadcast time (`reserve_island` awaits fan-out). | ICB-2 splits into local commit / API return / remote visibility. |
| 7 | deadline passing emits no fold event. | ICB-6 "deadline-enabled takeover", not automatic reclaim. |
| 8 | runtime TTL (30 s, removes entry, fires sweep watch) ≠ `until_unix_us` ≠ sweep cadence (500 ms). | ICB-6 two labeled groups; corrected. |
| 9 | "candidates examined" overclaims. | matched hosts / candidate islands before numeric filter / viable returned. |
| 10 | sensed matching mis-placed in ICB-1 (feature inconsistency). | ICB-1 ordinary only; ICB-5 sensed (`net redex`). |
| 11 | fixture-reset + TTL discipline. | release+await-Free; fresh island per race sample; assert population before/after matcher batches. |
| 12 | ICB-local harness, not an expansion of the closed CPB harness. | `benches/bench_island_claim/mod.rs`. §3 D1. |

### 0.3 v0.2 → v0.3 corrections (Kyra's v0.2 verdict — one blocker + bounded fixes)

| # | Correction | Landing |
|---|---|---|
| **B1** *(blocker)* | ICB-4 as written recreates the ICB-3 divergence **on B**: after every scheduler correctly rejects held A, they concurrently hit an *empty* B — `S1` inserts `Reserved{S1}`, `S2` inserts `Reserved{S2}`, … Pre-converging A does not make B authoritative. | **ICB-4 → single-claimant** known-state fallback: one claimant C, one pre-existing holder H, one direct observer O; A held-by-H ranked before free B; `C.claim_island` → C's reserve of A returns `Lost` → walk to B → commit B locally → O reads B held by C. **Remove** "every scheduler ends non-conflicting", fleet allocation spread, multi-scheduler fallback throughput. §4 ICB-4. |
| M1 | ICB-3 split-view duration is **right-censored** — if disagreement persists to the window end there is no completed duration (`≥ observation_window`); such lower bounds must not enter a p50/p95/p99 histogram. | Observe for an explicit fixed window **W** (claim deadlines + runtime TTL kept safely beyond W); record time-to-agreement iff agreement occurs, else record the sample **right-censored at W**. Report incidences + censored counts, not lower bounds. No ICB-7 latency threshold from censored samples. §4 ICB-3, ICB-7. |
| M2 | D6 counting-router **exactness** (feasible; the node's default registry holds exactly the capability/reservation/island folds, publicly obtainable). | Rebuild a **replacement** `FoldRegistry` from the three fold-Arc clones and install via `set_fold_router`; count `ApplyOutcome::{Inserted,Replaced,Rejected}` **only after** verify+dispatch succeed; dedup by `(publisher, island, generation)`; filter to the **fresh bench island**. §3 D6, §2. |
| M3 | The "128-task same-node CAS-pressure" diagnostic is ambiguous — 128 calls through one `MeshNode` share **one publisher identity**, so `Reserved→Reserved` is an allowed *extension*, not distinct-claimant CAS contention. | **Remove** the 128 row. If ever retained, define it exactly as **128 distinct `Claimant` identities → one shared in-process `Fold<ReservationFold>` → same island**, labeled "shared-fold local contention, no transport, not distributed arbitration" — never 128 calls through one `MeshNode`. §4 ICB-3, §6. |
| M4 | ICB-5 conflates readiness-loss and reservation-loss fallback (different boundaries). | Split into **ICB-5a sensed re-selection** (readiness overlay change → on-demand `claim_island_sensed` returns a replacement; no rejection/retry unless one actually occurs) and **ICB-5b reservation fallback** (selected provider stays sensed-viable, its first island pre-held → first apply `Rejected` → walk to next → fallback commits). §4 ICB-5. |
| M5 | ICB-6 short-TTL fast diagnostic **is** feasible bench-only. | "explicit short-TTL diagnostic": manually `SignedAnnouncement::sign` with `entity_keypair()` (own `node_id`, `generation ≥ 1`), fresh island, `EnvelopeMeta.ttl_secs = Some(short)` → `reservation_fold().apply` → `publish_fold_broadcast` → await the local expiry watch → confirm exact absence. Default 30 s row stays (small n, no p99). Local vs remote expiry kept separate. §4 ICB-6, §2. |
| M6 | ICB-7 must not "update memory"; and thresholds only for completed boundaries. | **Remove "update memory"** from the close-out — benchmark/commit status is task progress, not durable personal memory. Update instead: plan as-built, performance docs, architecture docs, cross-links. Derive thresholds only for completed perf boundaries; **no** threshold that normalizes the split view; ICB-3 stays hard semantic evidence, not an SLO. §4 ICB-7. |
| M7 | Wording exactness. | (1) matcher rows don't reserve — "min-units is **eligibility**; any later successful claim reserves the **whole island**", not "each matcher row reserves"; (2) ICB-2's non-claiming peer is an **observer** (the `reserve_island` path does not contact the island host as an authority) — drop "host/observer"; (3) ICB-3 "agreement" is reported three ways — **claimant-holder / observer-holder / all-node** agreement; (4) "zero timeouts" covers **topology establishment + expected-delivery completion + exact-holder waits**, never persistent disagreement (that is a *result*, not a timeout). §§4–5. |

---

## 1. Intent — the honest headline

The current system **cannot** support "concurrent requests begin → stable, non-conflicting
allocation across every observer" (merge is arrival-order-dependent). v0.3 measures:

> Net measures **local GPU-island claim latency**, **direct reservation visibility**, **matcher
> scaling**, **single-claimant known-state fallback**, and the **degree of holder divergence**
> produced by simultaneous cross-node claims (reported as agreement/divergence incidence with
> right-censoring, not a convergence latency).

Four distinct boundaries the repository cannot currently quote:

> **1. Match cost** — converged fixture → ranked eligible islands (matched hosts → candidate islands
>    → viable output). Matching reserves nothing (E4).
> **2. Claim latency** — a single claim, split three ways from one start: **local commit**, **API
>    return** (`reserve_island` returns after the fan-out attempt), **remote visibility** (E1).
> **3. Known-state fallback** — **single claimant**: a pre-converged held island → `Lost` → walk to
>    the next candidate.
> **4. Holder divergence under simultaneous cross-node claims** — a **right-censored** diagnostic
>    (agreement incidence / distinct holders / split-view persistence), replacing the BLOCKED
>    "contended allocation completion" headline.

Pre-ICB-7 acceptance is orientation only (CPB C11): valid distributions; matcher populations
asserted before/after each batch; local-commit / API-return / remote-visibility kept distinct;
contention across **distinct nodes** with delivery **proven** by the counting router; divergence
reported honestly with censoring; zero timeouts **in the M7-(4) sense** (topology / delivery / holder
waits — persistent disagreement is not a timeout).

---

## 2. Grounding — honest boundaries, no new plumbing (verified file:line)

Every endpoint is public; benches are dev-only, observation-only. All API facts below are confirmed
against the public `net::` surface (the lib target is `net`).

| Boundary | Endpoint | Discipline (verified) |
|---|---|---|
| Match result (ICB-1) | `MeshNode::match_islands(&MatchCriteria) -> Vec<IslandId>` (`mesh.rs:18970`) | Ungated. Matched-hosts / candidate-islands bench-reconstructed from `capability_fold()` (`mesh.rs:18939`) + `island_fold().query(IslandQuery::HostedByAny)` (`mesh.rs:18960`). Matching reserves nothing (E4, M7-1). |
| Local commit (ICB-2) | subscribe `reservation_fold().subscribe_changes()` → poll the claim future → **independently** await exact-holder read `query(ReservationQuery::State(island)).holder() == self` | E1: `reserve_island` (`mesh.rs:19058`) applies the CAS then `await`s `publish_fold_broadcast` **before** returning (`mesh.rs:19148→19157→19164`). Stop the local timer on the independent exact-holder read. |
| API return (ICB-2) | `reserve_island(...).await` → `ClaimOutcome::Won` | The verdict is the local CAS only; broadcast failure is logged. Report as "→ returns after fan-out attempt". |
| Observer visibility (ICB-2/3/4/6) | an **observer** node's `reservation_fold().subscribe_changes()` wake + exact-holder read | M7-2: the peer is an observer; `reserve_island` does not contact the island host as an authority. Rejected applies emit no watch event → use the counting router, not the watch, to prove full delivery. |
| Delivery reach (ICB-0) | `publish_fold_broadcast` → `self.peers` only (`mesh.rs:16606-16639`); inbound `SUBPROTOCOL_FOLD` applies locally, no rebroadcast (`mesh.rs:9437-9467`) | E3: `A↔R↔B` does not deliver A→B. Routed rows only when the ICB-0 preflight proves a logical peer session. |
| Distributed completion (ICB-3) | bench **counting router** implementing the public `FoldChannelRouter` (`fold/dispatch.rs`), installed via `set_fold_router(Some(...))` (`mesh.rs:18102`), wrapping a replacement `FoldRegistry::new()` registered with `capability_fold().clone()` / `reservation_fold().clone()` / `island_fold().clone()` (same `Arc<Fold<_>>` instances) | D6/M2: `try_route(publisher: &EntityId, bytes: &[u8]) -> Result<ApplyOutcome, DispatchError>` verifies (`decode_and_verify`, `dispatch.rs:105`) **before** apply; count `ApplyOutcome::{Inserted,Replaced,Rejected}` (`state.rs:285`, **not** `MergeAction`) only on `Ok`; dedup `(publisher, island, generation)`; filter to the fresh bench island. There is **no getter** for the node's live registry → rebuild-and-install is the public path. |
| Deadline takeover (ICB-6) | `Reserved{until_unix_us}` consulted only on foreign apply (`reservation.rs:237`, sole `reservation_expired` call site, `reservation.rs:333`) | E6: time passing changes nothing and fires no watch. Measure the first foreign takeover claim. |
| Runtime expiry (ICB-6) | `DEFAULT_TTL = 30 s` (`reservation.rs:170`); sweep `DEFAULT_SWEEP_INTERVAL = 500 ms` (`expiry.rs:35`); reap fires the **local** watch (`expiry.rs:179-183`) | E6: distinct from `until_unix_us`; removal is local, not broadcast; `Active` carries the runtime TTL (`mod.rs:729-741`). Short-TTL diagnostic (M5): `SignedAnnouncement::sign(entity_keypair(), ReservationFold::KIND_ID, class, node_id(), gen≥1, EnvelopeMeta{ttl_secs: Some(short)}, payload)` → `reservation_fold().apply(ann.clone())` → `publish_fold_broadcast(&ann).await`. No public reservation-broadcast-with-TTL helper exists → sign manually. |

**Harness discipline:** contention runs from **distinct transport-connected `MeshNode`s**, never
concurrent tasks on one node (same-node claimants serialize on one write lock → a false "distributed
arbitration", E2). Public-API node builds mirror `tests/common::connect_pair` and
`tests/gang_claim_node.rs`; islands seeded via the public `IslandRecord` publish path; down-hosts via
`set_liveness_down`.

---

## 3. Decisions (v0.3)

**D1 — Location: core `net` crate; ICB-local harness (item 12). APPROVED.** New
`benches/bench_island_claim/mod.rs` (delivery accounting, full-mesh construction, fresh-island
allocation, `ContentionReport`), reusing narrow generic pair/runtime/reporting helpers by copy/call —
**not** by expanding the closed CPB `bench_mesh_pair`. Targets:
```
benches/island_claim_match.rs        ICB-1        required-features ["net"]
benches/island_claim_contention.rs   ICB-2/3/4    required-features ["net"]
benches/island_claim_sensed.rs       ICB-5        required-features ["net", "redex"]
benches/island_claim_recovery.rs     ICB-6        required-features ["net"]
```

**D2 — Reserved-only divergence diagnostic (items 1, 2, B1). APPROVED.** ICB stays entirely on the
`Reserved` path. Cross-node merge is arrival-order-dependent → no convergence to time; ICB-3 measures
divergence, ICB-4 is **single-claimant** known-state fallback. No `→ Active` measurement. The plan
must not imply the `Reserved` path is reconciled later by an on-wire `Active` edge. **No arbitration
plumbing in the benchmark.**

**D3 — Bench-reconstructed candidate accounting, renamed (item 9). APPROVED.** Columns: matched hosts
/ candidate islands before numeric filter (bench-reconstructed) / viable islands returned. No
production counter.

**D4 — Recovery = two labeled mechanisms + a short-TTL diagnostic (items 7, 8, M5). APPROVED.**
(a) deadline-enabled takeover (no "deadline fired" event; first foreign takeover CAS + observer
visibility; configured deadline reported separately as policy); (b) runtime-entry expiry (30 s TTL,
500 ms sweep, local watch on reap); (c) an **explicit short-TTL diagnostic** for a fast run (M5),
bench-only, no production defaults changed. Local vs remote expiry kept separate.

**D5 — Compile gate: existing broad Clippy job already covers it (mirrors CPB D3). APPROVED.**
Targeted `--no-run` compiles are local; no new CI step.

**D6 — Delivery preflight + counting router, with the M2 exactness note. APPROVED.** ICB-0 proves
direct/routed logical delivery before any timed row. The bench counting router (M2): a
`CountingRouter` implementing the public `FoldChannelRouter`, wrapping a **replacement**
`FoldRegistry` built from the three fold-Arc clones and installed via `set_fold_router`; it delegates
`try_route(publisher, bytes)` to the real registry and counts `ApplyOutcome::{Inserted,Replaced,
Rejected}` **after** verification succeeds, deduped by `(publisher, island, generation)` and filtered
to the fresh bench island. **Expected unique inbound deliveries:** a **claimant** participant sees
**N − 1** foreign claims (its own local apply never traverses its own inbound router); a
**non-claiming observer** sees **N**. Stated explicitly per row. Bench-only.

---

## 4. Phase breakdown (v0.3)

### ICB-0 — ICB-local harness, delivery preflight, exact holder await, counting router
`benches/bench_island_claim/mod.rs`: (a) distinct-node / full-mesh builders (public API); (b) a
**direct/routed delivery preflight** asserting A's reservation reaches each intended observer before
any timed row, recording the logical session count (E3); (c) `await_reservation_holder(...)` exact
holder await (E1); (d) the **counting router** (D6/M2) — replacement `FoldRegistry` + `set_fold_router`,
counting `ApplyOutcome` after verify, deduped `(publisher, island, generation)`, filtered to the fresh
bench island, with the expected N−1 / N inbound counts asserted; (e) **fixture-reset + population
discipline** (item 11): release + `await exact Free` on every observer outside timing; fresh island id
per distributed-race sample; assert exact capability-host + island population before/after every
matcher batch; (f) a `ContentionReport` (hdrhistogram for *completed* boundaries + the §5 metadata +
divergence-incidence fields).
**Acceptance:** targeted `--no-run` compiles; a single-claim smoke records local-commit via the exact
holder endpoint (zero timeouts); the preflight proves or refuses routed delivery; the counting router
wakes exactly when the expected unique claims (N−1 / N) have arrived, counting only verified applies.

### ICB-1 — Ordinary matcher scaling
`benches/island_claim_match.rs` (`net`). `match_islands` across **10 / 100 / 1000 islands × 1 / 8 / 72
units × sparse / dense**. Report **matched hosts → candidate islands before numeric filter
(bench-reconstructed) → viable islands returned**, with match p50/p95/p99. **No sensed path** (ICB-5).
Assert exact host + island population before and after each timed batch (item 11). Wording (M7-1):
"**min-units is eligibility; any later successful claim reserves the whole island**" — a matcher row
reserves nothing.
**Acceptance:** distributions across all axes; population stable across each batch; the three columns
distinct; no sensed axis; no whole-vs-subset ambiguity; no reservation implied.

### ICB-2 — Single-claimant boundaries (local commit / API return / remote visibility)
`benches/island_claim_contention.rs`, pre-converged. Three boundaries from one claim start (item 6,
E1): (a) **local commit** — subscribe first, poll the claim future, independently await the local
exact-holder read, stop there, then assert `Won`; (b) **API return** — `reserve_island` returns after
the fan-out attempt; (c) **remote visibility** — a direct **observer**'s exact holder (M7-2: observer,
not host authority). Topologies: `scheduler ↔ host`, and `scheduler ↔ relay ↔ observer` **only if the
ICB-0 preflight proves routed logical delivery** (routed reported separately). Release + `await exact
Free` on every observer outside timing.
**Acceptance:** three distinct numbers from one start; routed row only when preflight-proven; exact
holder correctness; clean reset between samples.

### ICB-3 — Distributed simultaneous-claim divergence diagnostic (right-censored)
`benches/island_claim_contention.rs`. **2 / 4 / 8 / 16 distinct-node claimants** (report logical
session count; **no 128** — M3), **fresh island id per sample**. Fire concurrent fresh claims; the
counting router waits until every participant received every expected unique claim (N−1 / N); **then**
(M1): (1) snapshot holder assignment; (2) observe for an explicit fixed window **W**; (3) keep claim
deadlines and runtime TTL safely beyond W; (4) record time-to-agreement iff common agreement occurs;
(5) else record the sample **right-censored at W**. Report: divergence incidence; agreement incidence;
samples agreed; samples right-censored; observation window W; distinct holders at the full-delivery
endpoint; distinct holders at end of W; largest agreement cohort; claimant self-belief count; and
**three agreement ratios — claimant-holder / observer-holder / all-node** (M7-3). Agreement-latency
percentiles **only** when enough samples actually reach agreement; if none:
`common agreement 0/N · split view persisted N/N · duration ≥ W for every sample`. The likely, honest
result is divergence (distinct claimant-local holders = N, common converged holder absent, E2) — that
is the deliverable, not a failure. **The 128-task same-node diagnostic is removed** (M3); if ever
needed it is a distinct-`Claimant` shared-`Fold` local-contention row, never 128 calls through one
node.
**Acceptance:** snapshots only after full verified delivery; right-censoring applied (no lower bounds
in a latency histogram); the three agreement ratios reported separately; divergence reported as a
result.

### ICB-4 — Known-state fallback (single claimant)
`benches/island_claim_contention.rs`. **Single claimant** (B1 — a multi-scheduler walk to B recreates
the ICB-3 race on B). Fixture: one claimant **C**, one pre-existing holder **H**, one direct observer
**O**; **A** pre-converged held by H and ranked immediately before **free B**. Time:
`C.claim_island(...)` → C's reserve of A returns `Lost` → the claim loop walks to B → B commits locally
→ O reads B held by C. **Required assertions:** A ranks before B; A is held by H in C's exact local
view before timing; B is free; the reservation-fold **rejected-apply delta increases by exactly one**
for C's attempt on A; the returned island is B; C's local exact holder of B is C; O's exact holder of
B is C; A remains held by H; no other island changes. Distributions: repeat the single-claimant
scenario with **fresh B island ids** (or clean release/reset). **Removed:** "every scheduler ends
non-conflicting", fleet allocation spread, multi-scheduler fallback throughput. Label: **"fallback
from one pre-converged reservation, single claimant."**
**Acceptance:** all nine assertions hold; single claimant only; no coordinator; distributed-race
framing explicitly disclaimed. *(Alternative disposition: defer ICB-4 until an authoritative/
tie-broken arbitration mechanism exists — revisit at ICB-7.)*

### ICB-5 — Sensed selection through a single claim / fallback (two boundaries)
`benches/island_claim_sensed.rs` (`net redex`), single-claimant throughout. Uses **opposing
island-load and sensed-readiness order** as the witness does (`tests/sensing_scheduler_bridge.rs`).
- **ICB-5a — sensed re-selection** (before any claim attempt): exact readiness overlay change applied
  → the caller invokes **on-demand** `claim_island_sensed` → a different provider is selected and
  claimed. Explicitly on-demand (the bench calls the API after the overlay wake — no automatic
  scheduler invocation implied); report **no** claim rejection/retry unless one actually occurs.
- **ICB-5b — reservation fallback** (the sensed equivalent of corrected ICB-4): the selected provider
  stays first in sensed order, but its first island is pre-held → the first reservation apply is
  `Rejected` → `claim_island_sensed` walks to the next candidate → the fallback island commits. Report
  selected provider, first island, rejected-apply delta, final claimed island, fallback latency, direct
  observer visibility.
Also report sensed-projection overhead and that `selected_provider()` receives the first claim (proved
separately). Does **not** restate capability-propagation latency (CPB owns it — §6).
**Acceptance:** 5a is on-demand re-selection with no spurious rejection; 5b matches the corrected
single-claimant fallback shape; sensed-led order verified against the opposing seed.

### ICB-6 — Deadline-enabled takeover + runtime-entry expiry (separately labeled)
`benches/island_claim_recovery.rs` (`net`). Two labeled groups (items 7, 8; E6), plus the M5
diagnostic:
- **Deadline-enabled takeover** — `Reserved` until a deadline → a foreign claim **after** the deadline
  → new holder visible. No "deadline fired" event exists → report configured deadline wait (**policy,
  separate**) / first foreign takeover CAS returns `Won` (**mechanism**) / observer reads new holder
  (**visibility**). Not automatic reclaim.
- **Runtime-entry expiry** — announcement TTL + the 500 ms sweep → entry **absent** from this observer
  (reap fires the local watch; removal is local, not broadcast). Default 30 s case → small sample,
  **no p99**. **Explicit short-TTL diagnostic** (M5) for a fast run: sign a fresh-island reservation
  with `entity_keypair()` (own `node_id`, `generation ≥ 1`) and `EnvelopeMeta.ttl_secs = Some(short)`
  → `reservation_fold().apply` → `publish_fold_broadcast` → await the local expiry watch → confirm
  exact absence; bench-only, production defaults unchanged. `Active` also carries the runtime TTL.
  Keep **local expiry separate from remote expiry** — each observer sweeps independently.
**Acceptance:** configured TTL/deadline is a separate column from mechanism and visibility; the two
groups (and the short-TTL diagnostic) are distinctly labeled; the deadline group makes no "automatic
reclaim" claim.

### ICB-7 — Baselines, derived thresholds, docs, and architecture disposition
Run each bench on a quiet machine; **derive** thresholds only for **completed performance boundaries**
(M6): matcher latency; local exact-holder visibility; API return; direct remote visibility;
known-state fallback; takeover CAS; runtime-expiry observation when statistically valid. **No**
threshold that normalizes the distributed split view — for ICB-3 retain **hard semantic reporting**
(delivery completeness, distinct-holder count, agreement incidence, censored samples) as architecture
evidence, not a performance SLO; **do not** derive a latency threshold from censored divergence
samples. Cargo `[[bench]]` entries + `required-features`; confirm the broad Clippy gate covers the
targets (D5). Update the plan's **as-built** section, **performance documentation**, relevant
**architecture docs**, and **repository cross-links** (`docs/SENSING.md`,
`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`) — **not** personal memory (M6). If ICB-3 confirms divergence,
write an **architecture disposition** enumerating candidate authority models — single scheduler
authority · host-authoritative claim RPC · deterministic convergent reservation merge · defer
distributed `Reserved` authority to the on-wire `Active`/quorum path — **without choosing or
implementing one** (the benchmark phase must not).
**Acceptance:** thresholds only for completed boundaries; ICB-3 kept as semantic evidence; docs +
cross-links updated (no personal-memory step); architecture disposition written iff divergence is
confirmed.

---

## 5. What we measure vs. what we report

Every row names its **exact start event** and **exact endpoint**, keeps **local commit / API return /
remote visibility** distinct (E1), snapshots divergence **only after the counting router confirms full
verified delivery** (M2), applies **right-censoring** to ICB-3 (M1), and carries the required metadata:

- island population; eligible population; units/island;
- claimant count **and logical session count**; topology + relay count; direct vs routed (preflight-proven);
- selection policy; sensed vs ordinary; reservation deadline / runtime TTL (distinct fields);
- p50 / p95 / p99 for **completed** boundaries only (no p99 for small default-TTL runs; no percentiles
  for censored divergence);
- attempts/s; optimistic local `Won`; distinct claimant-local holders; distinct holders at end of W;
  largest agreement cohort; claimant-holder / observer-holder / all-node agreement (M7-3); foreign
  claims rejected; agreement incidence; samples right-censored; observation window W;
- matcher rows: matched hosts / candidate islands before numeric filter (bench-reconstructed) / viable
  islands returned.

"Zero timeouts" (M7-4) = topology establishment + expected-delivery completion + exact-holder waits on
rows that expect one; **persistent disagreement after complete delivery is a result, not a timeout.**
Divergence is a reported result, not a failure. Mechanism stays separate from policy.

---

## 6. Non-goals (v0.3)

- **Distributed AP convergence / stable non-conflicting allocation** — BLOCKED (merge is
  arrival-order-dependent). ICB reports divergence with censoring, not convergence.
- **Distributed / multi-scheduler fallback, allocation spread, fallback throughput** — BLOCKED (B1);
  ICB-4 and ICB-5b are **single-claimant** known-state fallback; no benchmark-side coordinator.
- **The 128-task same-node diagnostic** — removed (M3); a same-node row would need distinct `Claimant`
  identities over one shared `Fold`, labeled non-distributed, never 128 calls through one `MeshNode`.
- **Any `→ Active` / on-wire quorum timing** — in-process cohort only; not measured.
- **Routed delivery assumed from a chain topology** — must be preflight-proven (E3).
- **Capability publication latency** — CPB owns it; ICB-5 composes, never restates it.
- **Actual GPU process startup; utilization-uplift without a workload; external scheduler / K8s /
  Volcano / Kueue comparisons; multi-machine rack claims from localhost** — all out for v0.1.
- **No new production plumbing** — the counting router, preflight, and short-TTL injection are all
  bench-side over the public API. **No arbitration plumbing inside the benchmark workstream** (Kyra).

Does not duplicate: `benches/placement.rs` (scorer), CPB-2 (one island reacting to propagation),
`tests/gang_claim_node.rs` / `contention.rs` / `multi.rs` / `proptest.rs` / `loom_models.rs`
(correctness).

---

## 7. As-built (ICB-7) — files, baselines, thresholds, CI, architecture disposition

*Reserved. Filled at the ICB-7 close-out with the reference-machine baseline table, data-derived
regression thresholds for **completed** boundaries only, the CI note, the six-equivalences-as-built
confirmation, and — iff ICB-3 confirms divergence — the architecture disposition (candidate authority
models, none chosen). ICB-3 is recorded as hard semantic evidence, not a performance SLO. Empty until
the suite is built and measured.*
