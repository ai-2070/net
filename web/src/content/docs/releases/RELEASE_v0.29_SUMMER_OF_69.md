# Net v0.29 — "Summer of '69"

*Named after Bryan Adams' 1985 single off* Reckless *— the first-real-six-string, played-'til-my-fingers-bled anthem every bar band mistook for its own autobiography, "those were the best days of my life."*

## A scheduler that watches itself drift

The continuous-rebalance arm — Phase D-1's `diff_scheduler` — has shipped for releases: a pure function over `MeshOsState` that evicts the worst sub-floor holder of a led chain when a better alternative clears the hysteresis gap and the chain is off cooldown, two-stage (evict → Phase C refills). It worked. It was also a **polling wall**: every reconcile tick it called the installed scorer's `score()` for every holder of every led chain, *inside* the pure reconcile pass, whether or not anything had moved. On a stable mesh of N led chains that is N pointless capability evaluations per tick, forever.

v0.29 turns that arm into a drift scorer: history-aware, cost-aware, and — in the common case — almost free. It is the [drift-scorer implementation plan](../plans/MESH_SCHEDULER_IMPL_PLAN.md) Phases 0–3 landed onto the **as-built** MeshOS reconcile architecture, not a notional standalone daemon. One branch, ~20 commits, ~1.5K lines of new Rust and tests, entirely inside the MeshOS reconcile arm.

The organizing observation is the one that has shaped every release since the substrate stopped being a prototype: **the hard parts already existed — the work was a control loop over them, not new infrastructure.** The `PlacementScorer` trait, the two-stage evict→refill, the `last_rebalance` cooldown, the capability-fold `generation` counter that already ticks when a node's tags change, the type-enforced migration veto — all shipped already. v0.29 is the loop that decides *when to sample, what to remember, and whether a move is worth its cost*. No new fold, no new transport, no `Instant::now()` in the fold.

Below: the wins, grouped by the phase they land in.

---

## Phase 0 — killing the per-tick allocation wall

The contained, zero-behavior-change refactor that ships independently of everything else. `diff_scheduler` used to `collect()` every chain id into a `Vec` and `sort()` it **every tick** — sorting even the chains this node doesn't lead — purely to make the eviction emission order byte-stable. It then re-looked-up each chain's holder set and collected the holders into a fresh `Vec` per led chain, on every tick, in the overwhelmingly common case where nothing is evicted.

v0.29 iterates `actual.replicas` in native order, accumulates the *rare* `(chain, victim)` candidates into a small `Vec`, and sorts only that (k ≈ 0–1) before emitting — `O(C·log C)` per tick collapses to `O(k·log k)`. The redundant second `replicas.get(&chain)` is gone (iterating yields the `BTreeSet` holders directly), and the `holders` `Vec` for `best_alternative`'s exclude slice is materialized only on the rare sub-floor candidate path. **Byte-stable output is preserved** — one victim per chain means sorting candidates by chain id reproduces the prior emission contract exactly, and the existing reconcile determinism/idempotence tests pass unchanged because they pin that ordering.

---

## Phase 1 — sampling/decision split + the `ScoreHistory` sidecar

The keystone. Today's `diff_scheduler` *samples* (calls `score()`) and *decides* (compares against the floor + hysteresis) in one pass, so there is nowhere to put a cadence or a dirty-gate. v0.29 splits them. Sampling moves **out** to the loop, into a new loop-owned `LocalScheduler` sidecar that sits beside the `SchedulerRegistry` on the event loop. Before each `reconcile`, the loop samples the scores it needs into a `ScoreSnapshot { (chain, node) → f32 }` and hands the decision pass a `SnapshotScorer` — a `PlacementScorer` adapter that answers `score()` from the precomputed snapshot and delegates only the rare, expensive `best_alternative()` (the capability-index query, still gated behind a sub-floor victim) to the live scorer.

`diff_scheduler` thus becomes a pure decision over `(MeshOsState, ScoreSnapshot, config)` — still deterministic, still anchored on `last_tick`. The decision logic is byte-for-byte the same; only the *source* of the scores changed.

**`ScoreHistory` — a short, fading memory.** Each tracked chain carries a bounded ring of recent `(Instant, worst-holder-score)` samples plus an incremental EWMA and a `Trend` (`Stable | Degrading | Improving`). The ring is capped at 64 samples (a few minutes at heartbeat cadence) with an EWMA smoothing factor of `0.3` and a `0.02` trend dead-band — **~1 KB per chain, not the design doc's ~115 KB/artifact estimate**, which budgeted a full 60-minute window the trend detector never needed. Trend is classified against the running mean *before* the new sample folds in, so a single sharp departure registers immediately rather than being averaged away.

**Loop-side, never in the fold.** Scores come from live `PlacementFilter` evaluation — RTT, inventory, capabilities — which are local, non-replicated, and churn every heartbeat. They join `last_rebalance` / `rtt` / `inventory` as loop-maintained observational state and are kept *out* of the replay-deterministic `MeshOsState`. This is the load-bearing invariant of the whole release (see [Determinism](#determinism--the-load-bearing-invariant)).

New public surface: `LocalScheduler`, `ScoreSnapshot`, `SnapshotScorer`, `ScoreHistory`, `Trend`.

---

## Phase 2 — dirty-gate + coarse backstop, each killing a distinct failure mode

Two complementary levers gate *which* led chains get sampled each tick. They are not redundant — used alone, each leaves a hole the other closes.

**Dirty-gate — the steady-state win.** A chain is re-scored only when the fingerprint of its scoring inputs has moved since the last sample. `chain_fingerprint` folds each holder's `NodeId` and a new `PlacementScorer::node_fingerprint(node) -> Option<u64>` — a cheap, monotonic per-node counter the production scorer wires to the capability-fold `generation` that already bumps when a node's tags change. The fold is order-sensitive and stays deterministic by iterating the holders' `BTreeSet` in sorted order. A holder added or removed, or any holder's inputs moving, changes the fingerprint; everything else is a handful of counter compares and **zero scoring**. On a stable mesh the polling wall is simply gone — the steady-state cost per tick is `O(led-chains)` cheap compares. A scorer that can't fingerprint (the default `node_fingerprint` returns `None`) is always treated as dirty, exactly reproducing the old sample-everything behavior.

**Coarse backstop — the correctness net.** A new `SchedulerConfig::decision_interval` (default 30 s) forces every led chain to be re-scored at least once per interval *regardless* of its fingerprint. This catches the slow sub-fingerprint drift the dirty signal misses — notably RTT/inventory movement, which is not (yet) folded into the fingerprint — and any dirty-tracking gap. A missed dirty signal costs at most one `decision_interval` of staleness, never permanent silent staleness.

The split makes both clean: the *loop* owns "when to sample," `reconcile` owns "what to do with the scores." `SchedulerConfig` is `#[non_exhaustive]`, so `decision_interval` rides in additively.

---

## Phase 3 — `MigrationCost` + the net-benefit gate

The hysteresis gap alone answers "is a better home meaningfully better?" It does not answer "is moving there *worth it*?" A daemon with a large state footprint, or a long unavailability window, can clear the gap and still be a bad move. v0.29 adds the cost side of that ledger.

A new optional `PlacementScorer::migration_cost(chain, target) -> Option<MigrationCost>` lets a scorer estimate the cost of a move in physical dimensions — `state_transfer` time, `disruption` (the unavailability window), and a `reliability_factor` importance weight. `MigrationCostModel::score_equivalent` converts that to a score-equivalent via a `cost_per_sec` knob (default `0.1`), and the gate slots into `diff_scheduler` between the hysteresis check and the emit:

```text
net_benefit = (best_alt_score - victim_score) - cost_score
emit RequestEviction only if net_benefit > 0
```

A cheap hysteresis-clearing move still proceeds; a marginal one that costs more than it buys is vetoed. A scorer that returns `None` (the default) disables cost-aware gating and the hysteresis gap alone decides, so the gate is strictly opt-in. The cost model is **monotonic** in each dimension by construction — the property pinned by tests — but its absolute calibration is a deliberately conservative placeholder until production telemetry exists.

**The migration veto rides the existing typed bridge.** A claim-pinned daemon is consulted through `MigrationEligible::check` before any eviction; a held exclusive claim vetoes the move by type, not by a `bool` a contributor can forget. Because execution stays two-stage — `RequestEviction` → Phase C's untargeted `RequestPlacement` refill — the cost is estimated against `best_alternative`'s winner as an optimistic proxy for the actual landing node (Phase C refill scores candidates with the same placement logic, so it lands on a comparably-good home). Pinning the refill target through the eviction→placement handshake is deferred to Phase 4; until then the gate is an honest best-effort damper, documented as such.

New public surface: `MigrationCost`, `MigrationCostModel`, plus the defaulted `migration_cost` / `node_fingerprint` / `live_score` trait methods on `PlacementScorer`.

---

## The hardening pass — what the drift-scorer review forced

A [dedicated code review](../misc/CODE_REVIEW_2026_06_27_MESH_SCHEDULER_DRIFT_SCORER.md) of the landed drift scorer found the gaps between the plan and the shipped code: **1 Critical, 5 Important, 7 Nit.** The pure cores were solid — the snapshot/decision split is deterministic, the dirty fold is stable, the backstop cadence is correct. The findings clustered where a multiplicative default silently neutered a feature, where a cross-tick staleness window met a live query, and where tests asserted things that were structurally guaranteed regardless of the code under test. Every one is fixed on the branch with a regression test; the full crate lib suite (4,529 tests) and clippy are green.

**The Phase 3 gate was dead on arrival (Critical).** `MigrationCost.reliability_factor` is a *multiplier* in `score_equivalent`, but a derived `Default` made it `0.0` — so any cost built via the documented `..Default::default()` idiom collapsed to zero and the net-benefit gate never vetoed anything. The whole shipped Phase 3 feature was a silent no-op for any caller that didn't explicitly set the weight, and the two passing tests dodged it by hard-coding `1.0`. A hand-written `Default` now sets the neutral `1.0`, and `score_equivalent` clamps a non-positive or non-finite weight to `1.0` so a real migration always carries its transfer + disruption cost.

**A stale snapshot could fire an eviction the all-live path would not (Important).** The dirty-gate intentionally retains a clean chain's scores for up to `decision_interval`, but `best_alternative` is always live — so a victim whose score had recovered (via RTT drift the fingerprint doesn't capture) could be evicted on a stale low score against a fresh alternative. A new `PlacementScorer::live_score` (default `= score`, overridden by `SnapshotScorer` to bypass the cache) re-confirms the chosen victim live on the rare sub-floor candidate path before the floor/hysteresis checks — one node, only when a candidate is found, so steady-state zero-scoring still holds.

**Two tests proved nothing.** `snapshot_backed_scorer_matches_raw_scorer_decision` sampled from the same scorer it compared against (so equality was structural — it would pass even with `set_chain` broken), and `net_benefit_gate_allows_cheap_migration` passed even with the entire Phase 3 block deleted. Both were replaced/augmented with discriminating tests: a two-chain snapshot-machinery test that fails on a mis-keyed cell or cross-chain leak, and a cost boundary sweep that asserts the outcome flips as the cost crosses the score gain.

**NaN handling matched the comment, finally.** The victim-selection loop compared scores with `<` while the comment claimed `total_cmp` safety; a NaN score pinned `worst` and then slipped past the floor gate, evicting an arbitrary holder on garbage input. NaN is now skipped as "no opinion" in both the decision arm and the sampler.

**Plus the cleanups:** one shared `behavior::hash` FNV-1a helper replaces three hand-rolled copies (byte-identical, proven by the existing proximity/loadbalance hash tests) and corrects the "order-independent" comment; the duplicated 7-arg `reconcile()` call in `run_reconcile` collapses to one; a single shared `FixedScorer` test helper; the per-tick sidecar GC scans are gated on led-set membership delta (steady state skips them); `ScoreSnapshot::new`/`insert` are crate-/test-internal so no external caller can inject out-of-range scores into the decision math; and the unused `MigrationCost.bandwidth_bytes` field is dropped until a saturation-aware model consumes it.

---

## Determinism — the load-bearing invariant

The reason the entire sampling apparatus lives loop-side is the I3 replay contract: **two replays of the same event stream must converge.** The fold and the `reconcile` decision never see wall-clock or live scores except through the snapshot passed in, and every per-tick timestamp anchors on the loop's `last_tick` — never `Instant::now()` inside the fold. Sampling cadence (the backstop) and dirty-gating both read `last_tick`, never the wall clock. `ScoreHistory`, the snapshot, the fingerprints, and the cadence state are all observational sidecar state on the event loop, never in `MeshOsState`. A property test pins that on a stable mesh the scorer's `score()` call count per tick drops to zero after warm-up — the polling wall is provably gone — and that the arm still converges to a no-emit fixed point under any tag-update sequence.

---

## What's deferred (honestly)

- **Phase 4 — replica / group generalization.** The same snapshot/decision path generalizes to `Artifact::Replica` and group members, keyed by the design doc's `ArtifactId`. It is gated on an actual replica-rebalancing workload — not built speculatively. The meshos arm reuses `ChainId` identity until then.
- **Live-wire dirty inputs.** The implemented dirty signal is the capability-fold `generation` via `node_fingerprint`. RTT/inventory drift is not yet folded into the fingerprint — it rides the coarse backstop. Folding an `rtt`/`inventory` epsilon into the dirty check (LD-3's third input) is a refinement, not a correctness gap (the backstop covers it).
- **Cost-model calibration.** `cost_per_sec` ships as a conservative placeholder; absolute calibration waits on production migration telemetry. The model's *monotonicity* is guaranteed today; its *units* are operator-tunable.
- **Refill-target pinning.** The net-benefit gate estimates cost against the best alternative as a proxy; pinning the actual refill target through the eviction→placement handshake is Phase 4.
- **A production scorer.** `src` still ships only the test scorers; a `PlacementFilter`-backed `PlacementScorer` (wiring `node_fingerprint` to the capability `generation` and `migration_cost` to real state-size/bandwidth) is the consumer-side integration this release enables.

---

## Breaking changes

v0.29 is **additive** — a new loop-side sidecar, new snapshot types, and new *defaulted* trait methods — so existing callers are untouched.

**`PlacementScorer` gained three methods, all with defaults.** `live_score` (defaults to `score`), `node_fingerprint` (defaults to `None` → always dirty, i.e. old behavior), and `migration_cost` (defaults to `None` → cost gate disabled). **Existing scorer implementations compile and behave exactly as before**; implementing the new methods is purely opt-in and unlocks dirty-gating and cost-aware migration respectively.

**`SchedulerConfig` is `#[non_exhaustive]` and gained `decision_interval` (default 30 s) and `cost_model`.** Callers using `..Default::default()` or the struct's `Default` are unaffected; the type's non-exhaustive marker already prevented exhaustive field-by-field construction, so nothing breaks.

**No wire risk.** All new state — `ScoreHistory`, `ScoreSnapshot`, fingerprints, cadence — is in-process loop-side state with no `Serialize`/`Deserialize` and is never written into the fold. The capability path, the reconcile emission contract, and every binding/FFI entry point are unchanged. A peer on an older substrate is unaffected.

---

## How to upgrade

1. **Pull the release** — existing `PlacementScorer` implementations keep working unchanged; the drift scorer behaves as the old polling arm did until you opt into the new hooks.
2. **To get the steady-state win**, implement `PlacementScorer::node_fingerprint` (wire it to your capability/inventory generation counter) so stable chains stop being re-scored every tick.
3. **To get cost-aware migration**, implement `PlacementScorer::migration_cost` and tune `SchedulerConfig::cost_model.cost_per_sec` against observed migration costs; leave it `None` to keep the hysteresis-gap-only behavior.
4. **Tune the cadence** via `SchedulerConfig::decision_interval` if 30 s isn't the right backstop for your churn profile.
5. **Everyone else** gets the new surfaces with no behavior change to existing paths.

---

## Dependency updates

None. v0.29 is an in-crate MeshOS reconcile-arm feature with no new or bumped dependencies; the only version change is the crate itself, `0.28.0 → 0.29.0` (propagated across the CLI, deck, and SDK manifests).

---

Released 2026-06-27.

## License

See [LICENSE](../../LICENSE-APACHE).
