# Stage 0 independent review packet — ORG_SCOPED_STREAMING_PLAN

**Reviewer:** S0Review (independent; HOLD authority, no edit authority).
**Reviewed head:** `736469448` on `LZL0/org-streaming` (base `23f33bf98`; two
commits: `89a95b26a` model closure, `736469448` evidence and record).
**Probe worktree:** `C:/Users/chief/orca/workspaces/net/org-streaming-s0review`
(detached at `736469448`, own `CARGO_TARGET_DIR`s `target-s0rev` / `target-s0probe`).
Every mutation ran only there; both model files ended byte-identical to the
pinned head (`org_stream_registry.rs` sha256 `348fc6f4…`,
`org_stream_lifecycle.rs` sha256 `b2dce203…`). The session tree moved during
this review (the owner's post-findings fix pass: report corrections, labels,
an F15 fix in the lifecycle model) — **the verdict is on the evidence at
`736469448` only**; post-head repairs are outside it.

**Citation convention (as requested):** source citations are
**pristine-at-736469448** line numbers (from the pinned worktree). Every quoted
panic states its numbering explicitly — `observed-under-mutation` numbers shift
with each mutation's line delta above the assertion; the delta is given.

---

## 1. Verdict: **ACCEPT**

All Stage 0 acceptance rows (0.1–0.5) and all ten composition-check rows pass.
Findings F-1..F-3 (below) are P2/P3 record-quality defects; none blocks Stage 1
dispatch. No row is held. (The pre-agreed conditional — F-3 flipping to HOLD on
a green P6 — did not trigger: P6 went red, see §3 F-3.)

## 2. Per-row acceptance answers

**0.1 Baseline benches — MET** (bench run attested by the report, not
re-executed here; source verified). `Pair::protected()` adopts a node authority
and mints an `OrgProofIntent` (`sdk/benches/nrpc_common/mod.rs:426-514`);
`org_unary_open` is bench-only and built after the public groups so the control
bars measure an untouched process state (`sdk/benches/nrpc_unary.rs:113-133`);
`Pair::new()` untouched. §5 records both public groups and `org_unary_open`
beside the June-13 audit (`docs/internal/misc/PERF_AUDIT_2026_06_13_NRPC_FOLLOWUP.md`
carries c1/32B ≈ 42.5 µs and the ~5 µs floor exactly as quoted).

**0.2 External consumer probe — MET (executed).** `guards/org_api_probe/` is its
own workspace with committed `Cargo.lock` + `MANIFEST` (43 pinned symbols incl.
the C1/C3/C4 literal surfaces). CI step text sane (`.github/workflows/ci.yml:1820-1845`:
`cargo metadata --locked` first, `cargo check --locked --message-format short`
second, named-break diagnostics naming ledger C1/C3/C4 and the same-commit probe
update). Executed at the pinned head in the probe worktree: `cargo metadata
--locked --format-version 1` exit **0**; `cargo check --locked --message-format
short` `Finished … in 50.89s`, exit **0**. The CI step itself has not run
(branch unpushed) — §5.

**0.3 Lifecycle model — MET**, one named literal deviation. 39 tests (≥20);
obligations (a)–(n) each map to named witnesses; counts regenerated from
source: lifecycle 37→39, registry 37→37, total **74→76** — exact match to the
report's accounting. Inverses executed/red for (a)–(k), (m), and (n)'s
cancel/dequeue half (lane receipts) — spot-re-executed by me: (m)/comp-3 and
comp-9 equivalents, plus my own P1/P2 probes on §2.1/§2.2 sites no lane touched.
*Deviation (named):* (l) `two_calls_are_independent` (org_stream_lifecycle.rs:1400)
has no model-level inverse — honest plumbing classification (F7); the row's
literal "each obligation has an inverse → red" is satisfied for (l) at property
level by the registry witnesses `requalify_keeps_the_unaffected_sibling_and_retires_the_affected_call`
(receipt 4 red) and `serve_handle_drop_retires_only_its_own_registration`
(receipt 24 red), and for (n) by receipt 17's skip-a-release red plus my P6 red
(§3 F-3). Owner may overrule this granularity reading; recorded for that purpose.

**0.4 Transaction model — MET.** `reserve`/`release`/`install`/`confirm`
(`begin_confirm`→`ConfirmTxn::transfer`) /`retire`/`complete` with incarnations
and `authority_epoch` over an abstract authority and fold effect boundary
(`FoldEffects::admit` reachable only via `RunningCall` — zero-effects is
structural). 37 tests (≥14); 30 carry receipted inverses across 24 receipts
(census cross-checked item-by-item). All 14 plan-named schedules map to named
witnesses (raise-between-reserve-and-install; retire-between-install-and-confirm;
retire-after-transfer-before-task-run; bool-only check/spawn gap; policy veto;
fold-prerequisite refusal; scheduling failure; stale-incarnation late ops;
complete-once; requalify sibling; store replacement; budget N+1 at all scopes;
duplicate-while-live `ActiveCallOwned` before decode; duplicate-in-guard-window
`Replay`/`CallIdCollision`). Deterministic-interleaving branch of the
"loom or deterministic" allowance, driven at real boundaries
(`RetireAttempt::Blocked`); schedules named in the report §4; loom deliberately
unused and stated.

**0.5 Mapping — MET.** `S0_MAPPING.md` is a read-only trace with `path:line`/
`[inferred]` labels and 8 named obstacles + smallest resolutions, explicitly
carrying "no runtime acceptance claim" and "Not covered: no commands were run" —
exactly the 0.5 scope.

**Ten-row composition-check table — ALL TEN MET under the plan's exact names**
(5 lifecycle, 5 registry; source-regenerated). Each separating observation is
genuinely discriminating (four-weakening audit — widened window / relaxed
assertion / vacuous precondition / deleted subject: none found; refusal legs
carry in-test positive controls) and each has a receipted red; I re-executed
`protected_output_refusal_cannot_complete_ok` (comp-5) and
`retire_between_confirm_check_and_owner_transfer` (comp-1) myself (§4 R1/R2),
plus adversarial probes P2 (comp-1's obligation family), P5 (comp-7), P6 (comp-6
neighbourhood). Row-by-row detail was verified against source: comp-1
`Blocked` mid-transaction vs mutant `Applied(true)`; comp-2 paused-notifier
stale-view refusals; comp-3 over-budget latch vs `Completed(Ok)` (with in-test
positive control); comp-4 early-CS completion without pump/END; comp-5 latch
after refusal; comp-6 item-credit vs hard node byte cap; comp-7 one-permit
consumption across cancel/dequeue/handoff with a sibling call's bytes
untouched; comp-8 `ItemTooLarge`/`ExceedsCallBudget` prompt refusals with
satisfiable-vs-not distinction; comp-9 `Queued`/`Refused`/`Unreachable`
dispositions with receiver-side receipt attribution; comp-10 triple-convergence
exactly-once removal via the `removals` ledger.

## 3. Findings

**F-1 — Mixed line-numbering conventions in the record (executed + source-established).**
S0_REPORT.md's spot-check table cited `org_stream_registry.rs:2585` while
S0_RECEIPTS_REGISTRY.md:123 quoted `:2593:9` for the same red. Measured
resolution: the `assert_eq!` starts at **:2592 pristine-at-736469448** (message
literal :2596); receipt 1's `:2593:9` and my R1's `:2593:9` are both
observed-under-mutation with identical +1-line deltas (my red is byte-identical
to the receipt's); the report's `:2585` is observed-under-mutation under a
shortening inverse (the owner's attribution claim: three reverse-anchored hunks,
net −7 above the assertion — unverified by me, and immaterial to the red's
identity). The lifecycle counterpart behaves the same way: pristine :1811,
receipt comp-3 and my R2 both observe `:1810:9` under a −1-line delta,
byte-identical reds. Impact: two documents quoted different conventions
without saying so. The owner has since corrected the table to dual numbering.
Priority 3.

**F-2 — Findings F4/F6/F7/F8 unlabeled against the report's own standard (source-established).**
S0_REPORT.md:5-6 promises "Everything below distinguishes executed from
source-established"; F4 (:267), F6 (:280), F7 (:282), F8 (:291) carry no label
(F5/F11 name receipts without the word). None misrepresents its basis. Being
fixed in the owner's edit pass. Priority 3.

**F-3 — Receipt gap: (n)'s node-rollback witness had no lane-receipted inverse (executed; closed by probe).**
`node_refusal_rolls_back_call_and_caller_reservations`
(org_stream_registry.rs:3392-3467) was one of the registry lane's 7 never-run
tests, yet S0_REPORT.md:77/:158 cited it as (n) evidence and row 0.3 demands
inverse reds. **Executed closure (P6):** deleting the node-refusal rollback in
`ByteBudgets::reserve` (removing the two counter restorations in the
`NodeBudgetFull` arm) reddens the witness — `assertion 'left == right' failed:
the call counter was rolled back / left: 600 / right: 400`, panic at
org_stream_registry.rs:3418:9 (pristine), exit 100. The witness is genuinely
discriminating; the defect is receipt coverage only, so F-3 stays
**P2** and no row flips. The owner's concurrently dispatched registry-lane
receipt for this witness is consistent with my independent probe.

## 4. Raw receipts (executed; all in the probe worktree, `net/crates/net/` unless noted)

Common command: `CARGO_TARGET_DIR=…/target-s0rev cargo nextest run --lib
--no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,
meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex"
-E 'test(=NAME)'`. Exit 100 = witness failure, 0 = pass.

- **E1 baseline:** filter `test(org_stream)` → `Summary 76 tests run: 76 passed,
  5755 skipped`, exit **0**. Roster regenerated from source matches name-for-name.
- **E2 probe** (`guards/org_api_probe/`, `target-s0probe`): `cargo metadata
  --locked --format-version 1` **exit 0**; `cargo check --locked --message-format
  short` `Finished dev profile in 50.89s`, **exit 0**.
- **R1 (registry receipt 1 — the `ConfirmTxn` fix; REQUIRED re-execution):**
  exact pre-fix re-locking mutation (4 replacements, git diff 6+/5−). Red:
  **exit 100**, panic `org_stream_registry.rs:2593:9` (observed-under-mutation,
  +1 delta), `assertion 'left == right' failed: retirement is serialized with
  the check→transfer window / left: Applied(true) / right: Blocked` —
  **byte-identical** to receipt 1's quote. Restore: sha `348fc6f4…` == baseline;
  green exit 0.
- **R2 (lifecycle composition check `protected_output_refusal_cannot_complete_ok`):**
  dropped `state.lock().output_admission_failed();` at `sink_send`'s `len >
  budget` refusal (−1 line). Red: **exit 100**, panic `org_stream_lifecycle.rs:1810:9`
  (observed-under-mutation, −1 delta), `assertion 'left == right' failed: the
  refusal latches; 'Completed(Ok)' after a dropped item is the defect:
  SupervisorOutcome { terminal: Completed(Ok), published: 0, emission: Some(Queued), discarded: 0 }
  / left: Completed(Ok) / right: ResourceExhausted` — **byte-identical** to the
  receipt's quote. Restore: sha `b2dce203…`; green exit 0.
- **R3 (registry receipt 2):** `commit_check_locked` `let captured =
  record.captured;` → `let captured = live;` (0-line delta). Red: **exit 100**,
  `assertion 'left == right' failed: a commit inside the pre-notification window
  is refused / left: Proceed / right: Retired(Revoked)` — matches receipt 2
  (the panic's line number fell outside my tail window; assertion identity
  exact). Restore: sha `348fc6f4…`; green exit 0.
- **P1 (own inverse; §2.1 tie precedence):** `if *candidate <= end_ns` → `<` in
  `resolve_deadline`. Red: **exit 100**, `an_exact_tie_reports_credential_expiry_not_timeout`:
  `left: Deadline / right: Credential`. Restore sha `b2dce203…`; green.
- **P2 (own inverse; §2.2 producer-finished-is-terminal at the supervisor):**
  added `break None;` after `gate.finish();` in `run_supervisor`'s handler arm
  (+1 delta). Red: **exit 100**, panic `org_stream_lifecycle.rs:1482:9`
  (observed-under-mutation; pristine :1481), `handler finished, so output is
  draining`. Restore; green.
- **P3 (own inverse; §3 incarnation fencing at `retire_locked`):** dropped
  `record.incarnation != incarnation ||`. Red: **exit 100**, panic
  `org_stream_registry.rs:2789:9` (pristine), `assertion failed:
  !h.registry.retire(k, first.incarnation, TerminalReason::Timeout)`.
  Restore (first attempt refused by a non-unique anchor BEFORE any write — tree
  provably still mutated (`b56679…`) and the witness still red; second attempt
  with a unique anchor — self-caught, like the lanes' F12); sha `348fc6f4…`;
  green.
- **P4 (own inverse; §2.3 requalification refresh):** dropped `record.captured
  = live;` from `commit_check_locked`'s `GenerationOnly` keep-arm. Red: **exit
  100**, `assertion 'left != right' failed: the captured generation is
  refreshed, not merely tolerated / left: Some(0) / right: Some(0)` — the
  refresh assertion specifically, not receipt 4's verdict check (a genuinely
  different falsification). Restore; green.
- **P5 (own inverse; §2.7 handoff leaves one live permit):** dropped
  `self.settled = true;` in `ItemPermit::transfer`. Credited red: **exit 100**,
  panic `org_stream_registry.rs:3511:9` (pristine), `assertion 'left == right'
  failed: transfer keeps the charge / left: 700 / right: 800`. Then a second
  panic in `Drop` (`:732:14: byte permit released twice, or against the wrong
  call`) and a Windows fail-fast abort `0xc0000409` — mutant fallout (the
  double-release guard firing is itself §2.7's checked-subtraction defense
  working; F13-class). Restore; green.
- **P6 (own inverse; F-3 decider):** deleted the two counter restorations in
  `ByteBudgets::reserve`'s `NodeBudgetFull` arm. Red: **exit 100**, panic
  `org_stream_registry.rs:3418:9` (pristine), `assertion 'left == right'
  failed: the call counter was rolled back / left: 600 / right: 400`. Restore;
  green. ⇒ witness discriminating; F-3 remains P2.

## 5. Preserved credit — verified holds; do not rework

1. Counts and roster honest: 74→76 exact, plan-exact composition names, no
   witness deleted (executed: E1 76/76).
2. The `ConfirmTxn` lock-carrying fix and its receipt are sound (executed: R1
   byte-identical red); `FoldEffects::admit` reachable only via `RunningCall`
   makes "zero fold effects" structural.
3. All ten composition checks exist under the plan's exact names, with in-test
   positive controls and receipted reds (two re-executed byte-identical: R1/R2).
4. Receipt discipline exemplary: sha-gated restores, baseline sweeps, name
   enumeration, collateral-red listing, red-kind census (20 assertion failures +
   10 expectation panics, zero compile-error reds — consistent with every quote
   I inspected and with my 9 re-executions), self-caught defects disclosed
   (F12/F13/F14).
5. F1 is a property strengthening, disclosed as such; F9/F10/F11 are honest
   weakenings-analysis with intent-realizing replacement inverses.
6. Deterministic interleavings at real boundaries (`RetireAttempt::Blocked`);
   the bool-check/spawn mutant fails by construction (executed: R1).
7. §2.1 boundary coverage is thorough and genuinely discriminating (executed:
   P1 reddens the tie rule; the nine boundary witnesses are non-vacuous).
8. Incarnation fencing, requalification-with-refresh and permit
   release-once/handoff all survive independent adversarial probes at sites the
   lanes never mutated (executed: P3/P4/P5 red).
9. The F-3 witness is discriminating despite its missing receipt (executed:
   P6 red) — the lane's coverage gap cost nothing in substance.
10. 0.5's mapping honesty ("no runtime acceptance claim", complete "Not
    covered") and §8's complete "what never ran" list.

## 6. HOLD rows and closure properties

**None.** For the record, the pre-agreed conditional that did NOT trigger: had
P6 come back green, row 0.3 would be held with closure property "the node-rollback
half of obligation (n) must have a named witness that reddens under a bounded
production-site inverse (e.g. deleting the node-refusal rollback in
`ByteBudgets::reserve`)". P6 was red.

## 7. Never executed here (complete)

- The 0.1 benches (report's executed claim; source verified; one host).
- CI itself (branch unpushed); the concurrent pre-push sweep is out of scope.
- Loom (deliberately unused at Stage 0).
- Linux/macOS; `#[cfg(unix)]` legs.
- Production fold/registry wiring witnesses (none exist before Stage 1).
- 44 of the 47 lane receipts were audited by red-kind census and source
  cross-reference, not re-executed (3 re-executed: R1–R3).
- R3's panic line number (tail window clipped it; assertion identity and exit
  code captured).
- Verification of the owner's post-head fixes (report corrections, labels, F15)
  — they postdate `736469448` and are outside this verdict.
- The owner's attribution claim for the report's `:2585` (net −7 mutation) —
  plausible and consistent with my measurements, not independently executed.
