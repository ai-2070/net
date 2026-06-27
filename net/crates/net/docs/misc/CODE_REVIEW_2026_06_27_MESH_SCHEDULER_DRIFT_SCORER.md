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

**Resolved.** Findings this pass: **1 Critical / 5 Important / 7 Nit**, all
fixed on `mesh-scheduler-integration` with regression tests, one commit per
finding. Full crate lib suite (4529 tests) and clippy are green. Per the "no
review-tracking IDs in code or commit messages" feedback rule, the C/I/N labels
are for this doc only — they do not appear in the code or commit messages.

Resolution summary:

- **C1** — `MigrationCost` now has a hand-written `Default` (weight `1.0`) and
  `score_equivalent` clamps non-positive / non-finite weights to `1.0`.
- **I1** — `PlacementScorer::live_score` (overridden by `SnapshotScorer`)
  re-confirms the victim live on the sub-floor path before evicting.
- **I2** — cost-target approximation + cross-tick skew documented in
  `diff_scheduler` and the impl plan.
- **I3 / I4** — added a discriminating snapshot-machinery test and a
  net-benefit boundary sweep.
- **I5** — NaN scores are skipped in both the decision arm and the sampler.
- **N1 / N5** — shared `behavior::hash` FNV-1a helper; fingerprint comment
  corrected to "order-sensitive".
- **N2** — single `reconcile()` call in `run_reconcile`.
- **N3** — one shared `FixedScorer` test helper.
- **N4** — sidecar GC retains gated on led-set membership delta.
- **N6** — `ScoreSnapshot::new/insert` are `#[cfg(test)] pub(crate)`.
- **N7** — dropped the unused `MigrationCost.bandwidth_bytes`.

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

**Resolved** (`e8a5bb27e`): `MigrationCost` now has a hand-written `Default`
that sets `reliability_factor = 1.0`, and `score_equivalent` clamps a
non-positive / non-finite weight to the neutral `1.0`, so a cost built via
`..Default::default()` always charges its real transfer + disruption time.
Regression test `migration_cost_default_weight_does_not_zero_the_gate`.

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

**Resolved** (`b6af68917`): added `PlacementScorer::live_score` (default =
`score`, overridden by `SnapshotScorer` to bypass the snapshot); `diff_scheduler`
re-confirms the chosen victim live on the rare sub-floor path before the
floor/hysteresis checks, so a recovered holder is no longer evicted on a stale
score. One node, candidate-path only, so steady-state zero-scoring holds. Tests
`clean_chain_does_not_evict_on_stale_snapshot_after_live_recovery` and
`clean_chain_still_evicts_when_live_confirms_sub_floor`; the plan's claim was
corrected in `106e61072`.

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

**Resolved** (`106e61072`): documented the approximation in `diff_scheduler`
(the best alternative is the optimistic proxy refill target; Phase C refill
scores candidates with the same placement logic so it lands on a
comparably-good node) and in the impl plan. Pinning the refill target through
the eviction→placement handshake is genuinely a larger two-stage change and is
deferred to Phase 4 — so this is documented, not re-architected.

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

**Resolved** (`8b05a5dc3`): added `sampler_snapshot_is_per_chain_correct_and_isolated`
— two led chains with distinct holders/scores; asserts each score lands in its
own `(chain, holder)` cell, the worst-holder history is per chain, and no
cross-chain holder leakage. Fails on a mis-keyed `set_chain` or a wrong `get`.

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

**Resolved** (`52a59c241`): added `net_benefit_gate_boundary_tracks_cost_magnitude`
— sweeps the migration cost across the score gain and asserts the outcome flips
(emit below the gain, suppress at/above it). Fails if the Phase 3 block is
removed, the cost magnitude is miscomputed, or the comparison direction is wrong.

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

**Resolved** (`25b1049c3`): NaN scores are skipped as "no opinion" (like `None`)
in both the decision arm and the loop-side sampler, and the comment is
corrected. Tests `nan_score_holder_is_skipped_not_picked_as_victim` and
`all_nan_scores_emit_no_eviction`.

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

**Resolved** (`933473f74`): extracted `behavior::hash::{FNV1A_OFFSET,
FNV1A_PRIME, fnv1a_step}`; `loadbalance::hash_key`, `proximity::hash_capabilities`,
and `chain_fingerprint` all route through it. Output is byte-identical (the
existing proximity / loadbalance hash tests pass unchanged, plus a new
`step_matches_open_coded_fnv1a`).

### N2 — `run_reconcile` duplicates the entire 7-arg `reconcile(...)` call

**Where:** `event_loop.rs:1542-1578`.

The `if let Some(live)` / `else` arms differ only in the last argument
(`Some(&snap_scorer)` vs `None`); the other six args are repeated
byte-for-byte. Build the optional `SnapshotScorer` first, then make a
single `reconcile` call with
`snap_scorer.as_ref().map(|s| s as &dyn PlacementScorer)`. Removes the
duplicated arg list and the drift risk on the next signature change.

**Resolved** (`16d13bbb4`): `run_reconcile` builds the optional `SnapshotScorer`
first and makes one `reconcile` call.

### N3 — Duplicate `FixedScorer` definitions

**Where:** `reconcile.rs:~1909` vs the canonical `pub(crate)` one at
`scheduler.rs:~592`.

Byte-identical bodies; the scheduler copy is already `pub(crate)`
precisely so other test modules can reuse it. The two will drift — the
reconcile copy will keep the default `migration_cost` /
`node_fingerprint` even if the canonical one later gains overrides.
Import the shared one.

**Resolved** (`4f42b09eb`): the scheduler copy actually lived inside a private
`#[cfg(test)] mod tests`, so it wasn't reachable — hoisted it to scheduler
module level (`#[cfg(test)] pub(crate)`) and imported it from reconcile; the
duplicate is gone.

### N4 — `sample()` builds a `led` HashSet + four `retain` scans every tick

**Where:** `scheduler.rs:979-1037`.

Even on a steady tick with no leadership change and zero dirty chains,
`sample` allocates a `HashSet` of all led chains and runs four `retain`
passes over the sidecar maps. The expensive scoring is correctly gated
(this is cheap bookkeeping, not the polling wall the plan removed), but
the GC is only needed when the led set actually changes. Gate the four
retains on a membership delta so the true steady-state cost is
O(dirty), not O(led-chains) alloc + 4×O(tracked) scans.

**Resolved** (`e152f4fca`): the four retains now run only when
`last_sampled.len() != led.len()` — `last_sampled` holds exactly one entry per
led chain plus any stale ones, so a size mismatch is precisely the
"membership shrank" signal; the steady state skips the scans. Test
`losing_leadership_gcs_snapshot_too_not_just_history`.

### N5 — `chain_fingerprint` comment claims "order-independent inputs" but the fold is order-dependent

**Where:** `scheduler.rs:562`.

`acc = (acc ^ h).wrapping_mul(PRIME)` is sequential and order-sensitive;
it is deterministic only because `holders` is a sorted `BTreeSet`. The
comment would mislead a refactor that feeds it a `HashSet`/`Vec` (e.g.
the `exclude` slice), silently reintroducing nondeterministic
dirty-gating. Fix the comment to say "order-sensitive; relies on
`BTreeSet` sorted iteration."

**Resolved** (`933473f74`, with N1): the comment now states the fold is
order-sensitive and relies on `BTreeSet` sorted iteration; the shared
`fnv1a_step` helper carries the same caveat.

### N6 — `ScoreSnapshot::insert` / `new` are `pub` with no validation

**Where:** `scheduler.rs:~284`.

The internal sampler only uses the private `set_chain`; `insert` is a
public back-door that lets an external caller hand `reconcile` a
snapshot with NaN / out-of-`[0,1]` scores, feeding `diff_scheduler`'s
floor/hysteresis math directly. Narrow to `pub(crate)`, or validate
range/NaN on insert.

**Resolved** (`62b71e883`, `7eb8f87a5`): `new`/`insert` are now
`#[cfg(test)] pub(crate)` (their only callers are in-crate tests; production
builds snapshots via `sample`/`set_chain`), closing the external back-door.

### N7 — `MigrationCost.bandwidth_bytes` is a dead-carried field

**Where:** `scheduler.rs:~681`.

Only ever written (struct literals + `0` in a test); `score_equivalent`
and everything else in meshos never read it. Its own doc-comment admits
it is "carried for richer models, not used by the default conversion,"
and the plan defers richer models to Phase 4. Low priority — either
drop it until a model consumes it, or leave as an explicit
forward-carry (current state is acceptable but adds a field every
`MigrationCost` literal must spell).

**Resolved** (`fde221e98`): dropped the field (YAGNI — the plan's Phase 4 note
says "don't build speculatively"); a comment marks where to re-add it with the
saturation-aware model that consumes it.

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

## Resolution log

All findings are fixed on `mesh-scheduler-integration`, one commit per finding
(per-finding SHAs above), applied in this order:

1. **C1** — restored the Phase 3 gate (neutral `1.0` default + clamp). `e8a5bb27e`
2. **I5** — NaN-safe worst-holder selection. `25b1049c3`
3. **I1** — live victim re-confirm against stale snapshot. `b6af68917`
4. **I2** — cost-target approximation + cross-tick skew documented. `106e61072`
5. **I3 / I4** — discriminating snapshot + net-benefit boundary tests.
   `8b05a5dc3`, `52a59c241`
6. **N1 / N5** — shared FNV-1a helper + corrected comment. `933473f74`
7. **N2** — single `reconcile()` call. `16d13bbb4`
8. **N3** — one shared `FixedScorer`. `4f42b09eb`
9. **N4** — membership-gated sidecar GC. `e152f4fca`
10. **N6** — `ScoreSnapshot::new/insert` test-only. `62b71e883`, `7eb8f87a5`
11. **N7** — dropped `bandwidth_bytes`. `fde221e98`

Verification: `cargo test --lib` → 4529 passed / 0 failed; `cargo clippy --lib
--all-features` → clean (only the pre-existing Cargo.toml bench-profile note).

Note `I3` here refers to the review finding; the determinism invariant of the
same name (`I3`: no wall-clock in the fold) is unrelated and was preserved
throughout — see the header and the "Checked and NOT findings" section.
