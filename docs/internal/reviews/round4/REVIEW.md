# A2A paid admission — bounded third-round review

**HOLD at `84d178407d05664f2372fb62b6a0ddae1d235c43`.** Substantial closure is verified. Remaining findings are within C2/C6/C7 and the previously required C5 witness, not a new architecture or hardening round.

## Candidate and verified evidence

- Runtime/test candidate: `84d178407d05664f2372fb62b6a0ddae1d235c43`.
- Pushed handoff: `6875665244f33f188f9bfc0b7dcf8de7f2b42242`. Its only change after the runtime candidate is `docs/internal/reviews/A2A_C1_C8_CLOSURE_MAP.md`; do not describe the earlier run as CI executed on the documentation successor.
- Prior HOLD: `0743ca779abc212e4bfd9f7f838307b812da5c5a`.
- Exact-head CI: https://github.com/ai-2070/net/actions/runs/35333578622 — completed successfully, all 46 returned jobs successful.
- Detached review: `C:/Users/chief/AppData/Local/Temp/net-a2a-review-84d1784`.

| Local execution | Result |
|---|---|
| Selected SDK acceptance binaries, excluding the overlay-only C4 row | 147 passed; 1 excluded |
| Selected payment binaries | 107 passed; 0 skipped |
| C8 finalized protected-frame core witness | 1 passed; 5976 other core tests filtered |
| Actual Python binding: full `test_a2a_paid.py` plus reviewer C5 fixture | 25 passed; 0 skipped |
| C4 original startup witness with scheduling-only overlay adapted to this head | 1 passed; no owner-control patch |
| New capacity probes, paid and free | 2 failed |
| New caller/evidence/persisted-format probes | 3 failed |
| C5 wrong-live-route mutation | reviewer witness failed; landed negative witness still passed |
| Restored Python source/rebuilt binding | full selected Python set passed again, 25/25 |

These are selected sets, not a claim to have repeated every suite from the author's ledger. Commands, complete logs and machine-parsed outcome names are included. The Python build used an isolated CPython 3.11 environment and `maturin develop` with the workflow's explicit feature list; it was not a release-profile installed-wheel qualification. All money fixtures use the mock facilitator. No real funds moved.

## Closure ledger

| Item | Review disposition |
|---|---|
| C1 | Closure credited for both live and lapsed in-flight expiry branches; targeted original/new tests pass. |
| C2 | **HOLD:** floor covers newly inserted competitors but not an older reservation reacquiring capacity. |
| C3 | Closure credited: production launch re-presents admission identity atomically; original real-mesh replacement witness passes. |
| C4 | Closure credited: both publication workers retain guards/owner; original barrier-only witness passes on the repair. |
| C5 | Production historical caller route passes a reviewer-authored real-Python witness. **Landed witness remains false-green under wrong routing; adopt the stronger witness.** Provider exact-resolution source and Rust two-incarnation witness receive credit. |
| C6 | New-format structural separation and both insertion orders pass. **Prior-format persisted archives are silently accepted but no longer historically addressable.** |
| C7 | Archive-local precedence receives credit. **Live-resolved-before-success and arbitrary operator-evidence preservation remain broken.** |
| C8 | Closure credited: final envelope including signed org header is checked before registration; production-call witness passes. |

Paths below are relative to `net/crates/net/` at the candidate.

## D1 / C2 — P1: an older competitor's reacquisition contributes no clock-floor evidence

**Executed for paid and free records:**
- `review_paid_capacity_survives_old_competitor_reacquisition`
- `review_free_capacity_survives_old_competitor_reacquisition`

Both use the real journal/store with deterministic time inputs and `max_in_flight=1`:

1. B is admitted at 100 and expires at 101.
2. A is admitted at 102 after B lapses; A expires at 103.
3. At 104, B reacquires its old reservation with `open_decision`; B now owns a pinned slot.
4. A using fresh 104 correctly gets Busy — a positive control within each probe.
5. A using its delayed pre-expiry sample 102 gets Open. The measured occupied count is **2**.

`slot_now` takes the maximum live-row `updated_at`, but `open_decision` sets `deciding` without advancing that timestamp. B's successful reacquisition is therefore invisible to the floor. Both rows remain capacity-bearing after expiry because both are deciding.

Anchors: `sdk/src/a2a_journal.rs:1247–1251,1356–1383,621–625`; production prepared submit at `sdk/src/mesh_a2a.rs:1915–1919` samples before the awaited store acquisition.

**Required repair:** make the capacity invariant cover every acquisition/reacquisition, not just fresh insertion. Preserve the distinct retention semantics of timestamps; merely fixing the fresh-row witness does not establish a valid floor. These are store-schedule reproductions, not a claim to have forced this scheduling window over the network.

## D2 / C7 — P2: a live operator disposition is not inherited by late settlement retention

**Executed:** `review_late_settlement_preserves_a_live_operator_disposition`.

Pause actual mock-facilitator settlement, publish the concurrent exposed-refusal state through the same guarded store transition production uses, and resolve that live attempt through `resolve_attempt(Closed)`. Then release settlement.

One billing event lands. The live disposition survives unchanged, but the same key/generation now also appears as a fresh **PaidUnexecutable** archive entry. The result tells the operator to resolve that charge again. Observed same-incarnation states: **[Resolved, PaidUnexecutable]**.

This is not lost settlement evidence, and it does not overwrite the live row. It is the narrower failure of the agreed resolved-first rule: the existing disposition is bypassed when deciding how to retain the late evidence. The archive-only test passes because it begins with the disposition already in that map.

Anchors: `payments/src/flow/a2a.rs:2492–2518` and `:1193–1201`; the landed archive-only witness is `payments/tests/a2a_caller_identity.rs:1635–1666`.

**Required repair:** preserve the disposition of the exact incarnation regardless of which map contains it; retain the authoritative evidence alongside that disposition without turning the same charge into an unexplained second unresolved item. The refusal transition is deliberately staged, as in the landed sibling-refusal test; a full terminal-transport-failure injection is not claimed.

## D3 / C7 — P2: generated late evidence overwrites operator-owned JSON

**Executed:** `review_late_settlement_preserves_arbitrary_operator_evidence`.

Close a retained exposed refusal with valid operator evidence containing a `late_settlement` property. Deliver settled proof/billing afterward. `with_late_settlement` inserts its generated value under the same property and erases the operator's original value.

Anchor: `payments/src/flow/a2a.rs:1389–1393`; operator evidence is unrestricted JSON. The landed test only checks an unrelated `ticket` property.

**Required repair:** preserve the complete opaque operator document in collision-safe structure. Do not reserve fields retroactively inside arbitrary operator evidence. The probe permits preserving the document intact inside an outer envelope; it does not require a particular factoring.

## D4 / C6 — P2: prior persisted archives are accepted but not migrated

**Executed:** `review_prior_format_archive_is_still_exactly_addressable`.

The probe obtains real mock-settlement evidence, persists the previous writer's shape (historical decorated keys inside `attempts`, no `superseded` map), then reopens the current store. The document is accepted and both rows are listed, but the historical key/generation lookup returns absent.

The prior writer stored archives in `attempts[decorated_id]` (`0743ca779` version, `payments/src/flow/a2a.rs:1087–1094`). The candidate adds default-empty `superseded` with ordinary deserialization (`:663–672`); historical lookup/resolution now reads only that new map (`:1210–1239`). New-write separation does not classify existing records.

**Required disposition:** either migrate prior-format entries using their embedded key/generation, or explicitly reject that unsupported format with a recovery route. If pre-release stores are intentionally not supported, state that policy rather than silently parsing them as normal stores. Do not infer archive class merely from a suffix that legitimate live task IDs can spell. This is a persisted-format compatibility/recovery finding, not another failure of new-format namespace separation.

## C5 — the required Python witness is feasible, and the landed test is insufficient

I built and exercised the actual binding. The reviewer test:

1. Purchases A through the real Python gateway/provider and mock rail.
2. Uses a small Rust fixture helper to retain A's real proof/billing through the production `retain_superseded` API. The helper explicitly simulates supersession by removing the live entry through the production locked-store utility.
3. Purchases a new incarnation B through Python at the same complete key, then makes B PaidUnexecutable through the real provider preflight refusal. Thus B is eligible for the same Closed operation — wrong routing would really close it.
4. Calls Python `a2a_attempts`, selects the historical row's returned generation, resolves it through Python, and asserts historical Resolved plus an exactly unchanged live B.

**It passes at the candidate.** This is an honestly labelled store-seeded recovery-boundary test. It does not pretend Python manufactured the original supersession race, and it does not substitute a fake implementation or fabricated API response.

I then changed only the binding's generation-supplied route from `resolve_superseded_attempt` to `resolve_attempt`, retaining the error prefix:

- Reviewer test **fails** because the live replacement is changed.
- Landed `test_a_generation_scoped_resolution_never_reaches_the_live_attempt` **passes**. It has no historical row, and Closed is illegal against its live Paid row, so the wrong resolver also produces the expected error while leaving state unchanged.

After restoring/rebuilding production source, all 25 selected Python tests pass again.

**Required closure:** land and enforce the populated historical/live success witness (or an equivalent equally sensitive one). C5 should not be deferred as impossible. Its current production route earns positive runtime credit; the remaining problem here is the closure witness and claim.

Files: `test_review_python_history.py`, `review_python_seed.rs`; inverse overlay `c5-wrong-live-route-mutant.patch`. Source anchors: `bindings/python/tests/test_a2a_paid.py:1381–1424`, `bindings/python/src/a2a_paid.rs:926–931`; Closed's allowed source states are in `payments/src/flow/a2a.rs` at `resolution_target`.

## Accepted qualifications; no scope expansion

- Narrowing/renaming the Python lapse test rather than claiming observed renewal is correct. That evidence correction is separate from C5's explicitly required historical-resolution test.
- The generation-restart witness now clears retained rows structurally before reopening. It passes locally; the author's inverse receipt is not represented as an inverse I ran.
- Paid Retired wording is corrected in both release-note copies.
- C4's manual overlay is disclosed and independently passed here. Green CI alone does not continuously exercise that excluded row. Transient CI instrumentation would not necessarily ship product instrumentation; the exclusion is not the only technically possible arrangement, but no new production hook is demanded here.
- C8's primary over-budget error and zero-pending observation pass. Source ordering proves validation precedes pending registration; weaker control assertions should not be described as independently proving delivery.
- Real rails, released-wheel protected paid lifecycle/Granted/payer-mismatch qualification, cross-host org harness, and platform power-loss proof remain separate. None of these new executions closes them.
- The disclosure that the docs-labelled `dea169d1f` also carried engine code is handled by reviewing the cumulative exact source diff, not by trusting its subject line.

## Reproduction / handoff

Copy the three Rust files from `test-witnesses/net/crates/net/` into a detached worktree at the candidate. Cargo runs from `net/crates/net`:

```sh
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
FEATURES='net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros fixtures'
cargo nextest run -p net-mesh-sdk --features "$FEATURES" --test review_round3_provider \
  -E 'test(review_)' --no-tests=fail --retries 0 --no-fail-fast -j 4
cargo nextest run -p net-payments --all-features --test review_round3_caller \
  -E 'test(review_)' --no-tests=fail --retries 0 --no-fail-fast -j 4
```

Expected at this candidate: provider **2 failures**, caller **3 failures**. The copied original fixture tests are not counted as new probes.

For C4, apply `startup-barrier-84d1784.patch` at the repository root, then run the original `review_repair_provider` binary filtered to `review_aborted_startup_writer_cannot_outlive_its_owner`; expect PASS. Restore the journal afterward. No owner-control repair is embedded in this patch.

For Python, `run_python.py` builds the small seeder, records its executable path, and invokes the actual Python suite plus the reviewer witness. `python-build.log` records the exact maturin feature list. Scripts retain the review machine's absolute paths; adjust roots/venv/target paths if using another checkout. The seeder is fixture-only and requires `NET_REVIEW_PURCHASE_PATH`; it must never target a real operator store.

The packet contains exact-head CI data, all execution logs, parsed outcome names, Python junit results, witnesses, the C4 barrier and the C5 inverse overlay. Production source and the isolated binding were restored after instrumentation; the user branch was not edited. Hand the bounded residuals back to the same implementer.
