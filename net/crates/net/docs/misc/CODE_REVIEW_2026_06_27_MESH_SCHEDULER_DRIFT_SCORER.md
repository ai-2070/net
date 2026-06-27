# Mesh-scheduler drift-scorer code review — 2026-06-27

Branch: `mesh-scheduler-integration` (~1.26K insertions across 5 files).
Baseline: `master`. Implements
[`MESH_SCHEDULER_IMPL_PLAN.md`](../plans/MESH_SCHEDULER_IMPL_PLAN.md)
Phases 0–3 onto the as-built MeshOS reconcile arm.

Commits in scope:

- `8dc6dea25` Create MESH_SCHEDULER_IMPL_PLAN.md
- `3bf8ec925` perf(meshos): diff_scheduler avoids per-tick whole-chain alloc+sort
- `1f8b5cbd5` feat(meshos): Phase 1 — loop-side score sampling + ScoreHistory sidecar
- `cf83be672` feat(meshos): Phase 2 — dirty-gate + coarse backstop for score sampling
- `11d82f4eb` feat(meshos): Phase 3 — MigrationCost + net-benefit gate
- `7a92cf33e` Format files.
- `315bd1be4` docs(meshos): fix private intra-doc link in ScoreHistory::len

Files touched:

- `src/adapter/net/behavior/meshos/scheduler.rs` (new types:
  `LocalScheduler`, `ScoreSnapshot`, `SnapshotScorer`, `ScoreHistory`,
  `Trend`, `MigrationCost`, `MigrationCostModel`; trait extensions on
  `PlacementScorer`)
- `src/adapter/net/behavior/meshos/reconcile.rs` (`diff_scheduler`
  Phase 0 refactor + Phase 3 net-benefit gate + tests)
- `src/adapter/net/behavior/meshos/event_loop.rs` (`run_reconcile`
  sampling split)
- `src/adapter/net/behavior/meshos/mod.rs` (re-exports)
- `docs/plans/MESH_SCHEDULER_IMPL_PLAN.md` (new design/impl plan)

Three correctness finder passes (line-by-line + removed-behavior +
cross-file; language/determinism pitfalls; a fresh gap sweep) plus a
cleanup/altitude pass. Decision determinism (I3) holds: sampling is
loop-side and anchored on `now` (= `actual.last_tick`), never
`Instant::now()` inside the fold; Phase 0's byte-stable emission order
and lowest-`NodeId` tie-break are preserved.

## Status

**Open.** Findings this pass: **1 Critical / 5 Important / 7 Nit**.
Per the "no review-tracking IDs in code or commit messages" feedback
rule, the C/I/N labels are for this doc only — do not bake them into
code comments or commit messages.

---

## A. Critical

### C1 — `MigrationCost::default()` silently disables the entire Phase 3 net-benefit gate

**Where:** `scheduler.rs` — `MigrationCost.reliability_factor` field
(struct ~`L675-686`) and `MigrationCostModel::score_equivalent`
(~`L712-715`); consumed in `reconcile.rs:544-548`.

`score_equivalent` is:

```rust
self.cost_per_sec * secs * cost.reliability_factor.max(0.0)
```

`reliability_factor` is an `f32` with no manual `Default`, so
`MigrationCost::default()` makes it `0.0`. The factor is
**multiplicative**, so a zero (or unset) weight annihilates the whole
cost regardless of `state_transfer` / `disruption`.

A scorer that follows the documented construction idiom —

```rust
Some(MigrationCost {
    state_transfer: Duration::from_secs(60),
    disruption:     Duration::from_secs(60),
    ..Default::default()        // reliability_factor = 0.0
})
```

— gets `cost_score = 0.1 * 120.0 * 0.0 = 0.0`. In `diff_scheduler`
the gate is `if score_gain - cost_score <= 0.0 { continue; }`; with
`cost_score == 0.0` and `score_gain` already past the hysteresis gap
(so `> 0`), the condition is false and the eviction **always** fires.
The shipped Phase 3 feature is a no-op for any cost that does not
explicitly set `reliability_factor > 0`.

The two passing gate tests dodge this by hard-coding
`reliability_factor: 1.0`, and `migration_cost_model_is_monotonic`
even asserts `score_equivalent(&MigrationCost::default()) == 0.0` —
codifying the footgun as intended behavior. (Same root cause: a NaN
`reliability_factor` also collapses to `0.0` via `.max(0.0)`, since
`f32::max` returns the non-NaN operand.)

**Fix:** default the weight to `1.0` (a neutral multiplier — manual
`Default` impl for `MigrationCost`, or treat `0.0`/NaN as `1.0` inside
`score_equivalent`), or make importance additive (`1.0 + factor`).
Also reconsider the name: `reliability_factor` reads like a 0–1
reliability probability, but the tests push it to `2.0` as an
unbounded importance multiplier — `importance` / `service_value_weight`
would not mislead.

---

## B. Important

### I1 — Stale snapshot victim score + live `best_alternative` can fire an eviction `master` would not

**Where:** `reconcile.rs:511-538` (victim via snapshot `score()`, alt
via live `best_alternative()`), enabled by the dirty-gate at
`scheduler.rs:994-1002` and `chain_fingerprint` at `scheduler.rs:561-571`.

On `master`, `diff_scheduler` scored every holder live each tick, so
victim and alternative scores were always from the same instant. Now
`SnapshotScorer::score()` returns a possibly-stale snapshot value while
`best_alternative()` is delegated **live**. `chain_fingerprint` folds
only the holders' capability `node_fingerprint`; per the design note,
RTT/inventory drift is *not* in the fingerprint — it rides only the
coarse backstop.

Concrete sequence (`decision_interval = 30s`): chain `C` led by this
node, victim `A` snapshotted at `0.45` (sub-floor). Within the next
30s, `A`'s true score recovers to `0.55` (RTT improves) but its
capability generation does not move → `C` stays "clean" → the snapshot
still reports `0.45`. Meanwhile live `best_alternative` now returns
`(E, 0.9)`. `diff_scheduler` computes `score_gain = 0.9 - 0.45 = 0.45 >
hysteresis` and evicts `A` — even though `A`'s *current* score is above
floor and the chain should be left alone.

This contradicts the impl-plan's "Snapshot/decision skew … no mid-tick
staleness window" claim: it holds *within* a tick, but the dirty-gate
intentionally serves stale victim scores for up to `decision_interval`
*across* ticks, while the alternative is always fresh. Bounded by the
30s backstop and the 5-min `last_rebalance` cooldown, so it self-heals,
but it is a real, disruptive eviction the all-live path would not emit.

**Fix:** either sample `best_alternative`'s candidate score through the
same snapshot/cadence path (so victim and alt share an instant), or
re-score the victim live before committing an eviction on a clean
chain, or document this as an accepted bounded-staleness tradeoff and
correct the "no staleness window" claim in the plan.

### I2 — Cost gate is computed for `alt_node`, but the two-stage refill never targets it

**Where:** `reconcile.rs:544` (`migration_cost(chain, alt_node)`) vs the
emitted action at `reconcile.rs:556-557` and the Phase C refill path.

The net-benefit gate calls `migration_cost(chain, alt_node)`, but the
arm emits only `RequestEviction { chain, victim }` — `alt_node` is
discarded. Phase C refill then emits an *untargeted* `RequestPlacement`
(contrast `diff_forced_placements`, which pins `target`), so the
dispatcher re-picks the destination. The cost that gated the
accept/reject decision can therefore be for a node the migration never
lands on: `best_alternative` returns a cheap-to-reach node (gate
passes), refill places the daemon on an expensive one.

The net-benefit guarantee is only as strong as the assumption that
refill lands on `alt_node`, which the two-stage execution does not
enforce.

**Fix:** either pin the refill target to `alt_node` (carry it through
the eviction→placement handshake), or compute the cost against the
chain's worst-case / expected refill target rather than the specific
`best_alternative` winner, and note the approximation.

### I3 — `snapshot_backed_scorer_matches_raw_scorer_decision` is near-tautological

**Where:** `reconcile.rs:518-568` (test).

Both decision paths sample from the *same* `FixedScorer`, which does
not implement `node_fingerprint` (default `None` → chain always dirty →
fully sampled) or `migration_cost` (default `None` → Phase 3 off), and
`SnapshotScorer` delegates `best_alternative` to that same scorer. So
`raw_actions == snap_actions` is structurally guaranteed: break
`ScoreSnapshot::set_chain` to drop a holder, or have `sample` skip the
worst holder, and the test still passes. It proves only the trivial
single-chain, fully-dirty, cost-free case — not the keystone
snapshot/decision-equivalence property its name claims.

**Fix:** exercise a divergence-sensitive case — multiple chains, a
fingerprint that gates re-sampling so the snapshot is partially stale,
and an assertion that would actually fail if `set_chain`/`get`/worst
selection were wrong.

### I4 — `net_benefit_gate_allows_cheap_migration` does not exercise the gate

**Where:** `reconcile.rs:490-516` (test).

With `cost_score = 0.1 * 0.1 * 1.0 = 0.01` and `score_gain = 0.6`, the
eviction also emits with the entire Phase 3 block deleted (hysteresis
`0.2 < 0.6`). The test passes whether or not the net-benefit gate
exists, so it asserts nothing about the "allow" side — only
`net_benefit_gate_suppresses_costly_migration` is discriminating.

**Fix:** make the cheap-side case one the gate is actually load-bearing
for — e.g. a `score_gain` that is below the hysteresis-only threshold
but above `gain - cost`, or assert via a marginal cost that flips the
outcome.

### I5 — `worst`-holder pick uses `<`, not `total_cmp`; NaN holder is never selected (pre-existing, re-exposed)

**Where:** `reconcile.rs:503-518`. Mirror pattern in the new sampler at
`scheduler.rs:~1010` (`worst.map_or(s, |w| w.min(s))`).

The comment says "Use `total_cmp` on f32 so NaN doesn't surprise us,"
but the code compares with `score < ws`. Under IEEE `<`, a NaN never
compares true, so a genuinely-degraded NaN-scoring holder is silently
excluded from `worst` selection — a healthier holder becomes the
victim, or the chain is skipped. Predates this branch, but the diff
rewrites this loop and re-asserts the false comment, and the new
`sample()` copies the same `f32::min` pattern (observational only there,
feeding `ScoreHistory`).

**Fix:** make the comparison match the comment (`total_cmp`) and decide
NaN handling explicitly (treat NaN as worst, or skip like `None`), or
correct the comment to reflect the `<` semantics.

---

## C. Nit (cleanup / reuse / efficiency / docs)

### N1 — `chain_fingerprint` is a third hand-rolled FNV-1a copy

**Where:** `scheduler.rs:564-568`; existing copies at
`loadbalance.rs:1525` (`hash_key`) and `proximity.rs:1063`
(`hash_capabilities`).

The same FNV-1a constants are now spelled three ways
(`0x100000001b3` vs `0x0000_0100_0000_01b3`), so a grep for one misses
the others. Extract one `fnv1a_fold(acc, x) -> u64` helper (or feed the
already-sorted `BTreeSet` into a `std::hash::Hasher`) and call it from
all three sites.

### N2 — `run_reconcile` duplicates the entire 7-arg `reconcile(...)` call

**Where:** `event_loop.rs:1542-1578`.

The `if let Some(live)` / `else` arms differ only in the last argument
(`Some(&snap_scorer)` vs `None`); the other six args are repeated
byte-for-byte. Build the optional `SnapshotScorer` first, then make a
single `reconcile` call with
`snap_scorer.as_ref().map(|s| s as &dyn PlacementScorer)`. Removes the
duplicated arg list and the drift risk on the next signature change.

### N3 — Duplicate `FixedScorer` definitions

**Where:** `reconcile.rs:~1909` vs the canonical `pub(crate)` one at
`scheduler.rs:~592`.

Byte-identical bodies; the scheduler copy is already `pub(crate)`
precisely so other test modules can reuse it. The two will drift — the
reconcile copy will keep the default `migration_cost` /
`node_fingerprint` even if the canonical one later gains overrides.
Import the shared one.

### N4 — `sample()` builds a `led` HashSet + four `retain` scans every tick

**Where:** `scheduler.rs:979-1037`.

Even on a steady tick with no leadership change and zero dirty chains,
`sample` allocates a `HashSet` of all led chains and runs four `retain`
passes over the sidecar maps. The expensive scoring is correctly gated
(this is cheap bookkeeping, not the polling wall the plan removed), but
the GC is only needed when the led set actually changes. Gate the four
retains on a membership delta so the true steady-state cost is
O(dirty), not O(led-chains) alloc + 4×O(tracked) scans.

### N5 — `chain_fingerprint` comment claims "order-independent inputs" but the fold is order-dependent

**Where:** `scheduler.rs:562`.

`acc = (acc ^ h).wrapping_mul(PRIME)` is sequential and order-sensitive;
it is deterministic only because `holders` is a sorted `BTreeSet`. The
comment would mislead a refactor that feeds it a `HashSet`/`Vec` (e.g.
the `exclude` slice), silently reintroducing nondeterministic
dirty-gating. Fix the comment to say "order-sensitive; relies on
`BTreeSet` sorted iteration."

### N6 — `ScoreSnapshot::insert` / `new` are `pub` with no validation

**Where:** `scheduler.rs:~284`.

The internal sampler only uses the private `set_chain`; `insert` is a
public back-door that lets an external caller hand `reconcile` a
snapshot with NaN / out-of-`[0,1]` scores, feeding `diff_scheduler`'s
floor/hysteresis math directly. Narrow to `pub(crate)`, or validate
range/NaN on insert.

### N7 — `MigrationCost.bandwidth_bytes` is a dead-carried field

**Where:** `scheduler.rs:~681`.

Only ever written (struct literals + `0` in a test); `score_equivalent`
and everything else in meshos never read it. Its own doc-comment admits
it is "carried for richer models, not used by the default conversion,"
and the plan defers richer models to Phase 4. Low priority — either
drop it until a model consumes it, or leave as an explicit
forward-carry (current state is acceptable but adds a field every
`MigrationCost` literal must spell).

---

## Checked and NOT findings

- **Phase 0 emission order / tie-break / one-per-chain:** preserved.
  New code iterates `&BTreeSet` holders directly (sorted, same order as
  the old `Vec` collect); candidates are sorted by chain id before
  emit; the per-chain loop pushes at most one victim.
- **Snapshot coverage:** a holder-set change folds into
  `chain_fingerprint` → dirty → re-sample; an un-fingerprintable holder
  forces `None` → always dirty. A *clean* chain's snapshot holder set
  always matches the holders `diff_scheduler` queries within one tick.
- **EWMA / trend math:** `record` classifies against `prev_ewma` before
  folding the new sample (documented, intentional); the
  `0.8→0.2→0.9×5→0.9×30` test transitions Degrading→Improving→Stable as
  the math predicts.
- **Backstop / dirty cadence:** `saturating_duration_since >=
  decision_interval`; first sample always due via `is_none_or`;
  anchored on `now` (= `last_tick`), preserving replay determinism.
- **GC of per-chain state on leadership loss:** all four sidecar maps
  retained/dropped consistently by the `led` set.
- **`HISTORY_CAP` ring:** correctly bounded, no off-by-one.
- **Doc-link fix (`315bd1be4`):** no remaining public→private intra-doc
  links in the new items.

## Recommended order of fixes

1. **C1** — restore the Phase 3 gate (default `reliability_factor` to
   `1.0` or make it additive). The shipped feature is otherwise inert.
2. **I3 / I4** — make the snapshot-equivalence and cost-gate tests
   actually discriminating, so C1-class regressions are caught.
3. **I1 / I2** — decide the staleness and cost-target tradeoffs
   (fix or document; correct the plan's claims either way).
4. **N1–N7** — cleanup, as convenient.
