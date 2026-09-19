# Native A2A paid admission — repair review

**Verdict: HOLD.** The first repair round fixes the original counterexamples, but integrated financial recovery, launch identity, and journal ownership still have reachable failures.

## Candidate and evidence

- Reviewed/pushed HEAD: `0743ca779abc212e4bfd9f7f838307b812da5c5a`.
- Original implementation: `1ed757629249421b5c09dd625f767368d6f8f8ac`.
- Accepted plan: `2546927c856aa3d6c93b6786125808001c67f6eb`.
- User worktree: `C:/Users/chief/orca/workspaces/net/a2a-payments`; remained clean at the pinned SHA. Remote `LZL0/a2a-payments` matched it.
- Review worktree: `C:/Users/chief/AppData/Local/Temp/net-a2a-review-0743ca7`.
- Exact-head CI: https://github.com/ai-2070/net/actions/runs/35316417956 — **completed, success; all 46 returned jobs successful**. Full exact-head JSON is included in the packet.
- Local selected SDK tests: **129 passed, zero skipped**.
- Local selected payment tests: **128 passed, zero skipped**.
- Those **257 passing tests include all original reviewer binaries and the new admission/caller identity suites**. This is genuine repair credit, not a failed-CI HOLD.
- Additional probes against unchanged production source: **six failures**, plus **one passing caller convergence control**.
- Additional startup publication probe: **one failure using an explicitly supplied test-only scheduling barrier**. With the same barrier and an owner-capture control, it **passes**. This instrumented result is separate from the six pristine-source failures.
- No real funds moved. Payment probes use the existing mock facilitator/real payment-engine fixture. No local installed-Python wheel test or real settlement-rail qualification is claimed.

The two new test files copy existing fixtures; run the filters below rather than treating every copied test as a new witness. All source paths below are relative to `net/crates/net/` and line references refer to the pinned production source, not the temporary instrumented overlay.

## Remaining blockers

### C1 — P1: expiry of a concurrent retry strands a settlement already in progress

**Executed:** `review_expired_in_flight_payment_is_not_proven_unpaid`.

Schedule: prepare and approve an exact quote; pause its real-engine settlement in the existing facilitator barrier; advance the fixture clock beyond quote expiry plus tolerance; retry the identical purchase; then release the original successful settlement.

The retry publishes `RefusedExposed` with `funds_ambiguous=false`. The original settlement succeeds, but the caller cannot publish Paid over that state. Later purchase calls read the stored denial instead of recovering the exact successful payment. The probe verifies one settlement, logs the final durable RefusedExposed state, and fails its first safety assertion against the false non-ambiguity verdict. This is different from the now-fixed case where settlement has already completed before the expired retry.

Anchors:
- `payments/src/engine/mod.rs:1161–1181`: fresh-expiry rejection precedes outstanding `in_flight` handling.
- `payments/src/flow/mod.rs:1294–1306`: rejection handling can release spend reservation.
- `payments/src/flow/a2a.rs:2191–2207,2252–2258,2334–2380`: success cannot converge over the sibling's terminal refusal.
- `payments/src/flow/a2a.rs:1843–1851`: subsequent purchases return the denial.

**Required closure:** distinguish an expired fresh claim from an unresolved admitted claim; retain/converge authoritative exact-attempt success regardless of a weaker sibling verdict. Require correct spend accounting and exact-payload recovery, not merely an error relabel.

### C2 — P1: a pre-expiry clock sample can reacquire an already occupied capacity slot

**Executed store-level schedule:** `review_pre_pin_stale_clock_cannot_reacquire_an_occupied_slot`.

A is reserved at time T. At T+2 its slot has lapsed and B acquires the only slot. A opens its decision using the previously sampled T. The store calls A unexpired, skips reacquisition capacity checking, and returns Open. Its hold now coexists with B's reservation.

This is a deterministic production-store test with supplied timestamps, not a claimed network scheduling reproduction. The production submit path samples `now` before awaited lookup/decision acquisition, making that stale sample reachable.

Anchors: `sdk/src/mesh_a2a.rs:1750,1812–1816,1883–1891`; `sdk/src/a2a_journal.rs:1237–1263`.

The free inline path also needs coverage: preflight, insertion, and claim use an earlier clock without one continuous capacity hold (`mesh_a2a.rs:1900–1921,1988–1991`; `a2a_journal.rs:1432–1440`). This free-path variant is source-traced, not separately executed here.

**Required closure:** decide current slot ownership under the same exclusion as acquisition, without reviving a released slot from pre-await time. Cover both prepared and free-inline paths; preserve payment-independent free service.

### C3 — P1: operator replacement retargets a live launch claim to different work

**Executed over the real mesh/provider executor:** `review_operator_replacement_cannot_retarget_a_live_launch`.

A is Paid and its submit pauses in preflight with a decision open. Through the supported journal API, resolve A and forget it. Prepare different free work B under the same owner/task key in a mixed paid/free catalog. Resume A.

The old submit is Accepted and the executor runs A's service. The journal's replacement B is no longer merely Reserved: the key-only launch claim has consumed B. Executed work and launch-ledger identity diverge.

Anchors:
- `sdk/src/a2a_journal.rs:1496–1548`: resolution/forget allow replacing the deciding incarnation.
- `sdk/src/a2a_journal.rs:1420–1468`: launch claims only owner/task key.
- `sdk/src/mesh_a2a.rs:1957–1989`: local Paid snapshot proceeds to that key-only claim.

**Required closure:** bind launch to the exact original admission identity, and make maintenance cooperate with active decision ownership. Do not assume a row is unreplaceable merely because ordinary pruning pins it. This schedule uses supported concurrent operator maintenance, not an unauthenticated remote ability to resolve another owner's records.

### C4 — P1: cancelled startup recovery releases ownership while its writer remains live

**Executed with a scheduling-only source overlay:** `review_aborted_startup_writer_cannot_outlive_its_owner`.

Create an inherited journal with a deciding row. Start `open()` and hold its existing recovery publication immediately before `files.publish`. Abort the open future. A successor now opens successfully and durably adds a new admission. Release the abandoned startup writer: its older snapshot publishes, and the successor's new admission reads back as **None**.

The overlay delays the existing write; it does not change the state image or ownership being tested. `startup-barrier.patch` contains the exact overlay. `startup-owner-control.patch` adds an owner capture to that same worker: the successor is then refused until the worker finishes, and the test passes. Both runs/logs are included. Production source was restored afterward; no repair was applied to the user's branch.

Anchors: `sdk/src/a2a_journal.rs:2108–2116,2140–2158` versus the repaired normal mutation at `2282–2302`.

**Required closure:** every recovery/compaction/publication writer must retain lifetime ownership and relevant exclusion until actual I/O completion. Unique temporary filenames do not prevent an abandoned whole-snapshot writer from overwriting newer state.

### C5 — P1 provider / P2 Python: operator recovery does not select exact historical identity

**Provider executed:** `review_provider_resolution_refuses_ambiguous_incarnations`.

Retain paid incarnation A as detached evidence, then create paid replacement B under the same owner/task key. A key-only resolve intended to close historical A instead terminalizes live B. The store unconditionally prefers a live unresolved row; its API has no admission/generation selector. Even multiple detached records can only be resolved by the implementation's oldest-first choice.

Anchor: `sdk/src/a2a_journal.rs:988–1004,1496–1539`; Python provider wrapper `bindings/python/src/payment_provider.rs:786–800`.

**Python caller source-confirmed, not locally executed:** the ordinary provider selector now works, but retained superseded purchases introduce multiple generations at that same complete key. `bindings/python/src/a2a_paid.rs:802–829` either reports duplicate provider IDs or always calls the live-key resolver. Rust's `resolve_superseded_attempt(..., generation, ...)` exists at `payments/src/flow/a2a.rs:2633–2652` but has no Python exit.

**Required closure:** expose exact historical admission/attempt selectors and reject ambiguous mutations. A reconciliation list is not usable if its selected record cannot be addressed without closing another charge first.

### C6 — P2: archive keys collide with valid live task IDs

**Executed store primitive:** `review_superseded_key_cannot_overwrite_a_live_purchase`.

`PurchaseKey::id()` ends in unrestricted task ID. Appending `#superseded/<generation>` therefore shares a namespace with a valid live task whose ID contains that suffix. The witness purchases both tasks, retains A through the actual store API, then reads B by its unchanged key: B's paid record has been replaced by A's archived record.

Anchors: `payments/src/flow/a2a.rs:151–167,1081–1095,1208–1214`.

**Required closure:** structurally separate/tag live and historical namespaces. Test both insertion orders and exact evidence preservation.

Evidence boundary: this directly exercises the retention-store operation; it does not claim to force a supersession through ordinary repaired pruning, which no longer removes Paying.

### C7 — P2: late ambiguity overwrites retained settlement evidence

**Executed store primitive:** `review_retained_success_cannot_be_downgraded_by_late_ambiguity`.

Retain actual settled proof/billing as PaidUnexecutable, then deliver the same incarnation's delayed Unknown result through `retain_superseded`. The retained state becomes Unknown and loses the proof/billing. The unconditional insert also permits overwriting an operator disposition.

Anchors: `payments/src/flow/a2a.rs:1087–1095,2283–2294,2354–2368`.

**Required closure:** merge under the store lock with explicit evidence precedence. Ambiguity/refusal cannot erase settlement, and delayed completion must respect resolved disposition. Add success-first/ambiguity-second and resolved-first/completion-second schedules.

Evidence boundary: archive-store semantics reproduced; an end-to-end concurrent supersession schedule is not claimed.

### C8 — P2: finalized organization proof is omitted from the aggregate packet bound

**Source-confirmed only; no oversized protected-wire runtime reproduction here.**

`check_proof` budgets the body, payment headers, service and fixed fields. `submit_task_paid` runs it before composing the organization intent. Core then appends the signed organization header; its final validation checks field/count ceilings, not total packet size. A request near the accepted aggregate ceiling can become undeliverable after this addition.

Anchors: `sdk/src/mesh_a2a.rs:552–580,2799–2804`; `src/adapter/net/mesh_rpc.rs:5432–5442`; `src/adapter/net/cortex/rpc.rs:586–624`.

**Required closure:** measure/refuse the actually finalized envelope before pending-call registration. This is a delivery/error-boundary finding, not an organization-authority bypass.

## Repair credit and remaining qualification

- Original quote-approval identity, input binding, Paying retention, settled-before-expiry replay, engine redemption retention, and oversized discovery witnesses now pass.
- Initial capacity check/insertion is transactional, and a decision already pinned survives expiry. The remaining capacity schedule occurs before acquisition.
- Ordinary journal mutations retain I/O ownership, temporary names preserve the full destination, and Unix publication adds directory synchronization. The startup writer is a separate missed path.
- Provider waiter verdicts now preserve structured ordinary decision failures; caller Unknown-to-Paid convergence has a passing control. General expiry/refusal convergence is still incomplete.
- Native exact-provider organization intent is composed without an observed downgrade. Missing qualification is not an asserted authorization bypass.
- The original review functions were not weakened to erase their failures. Changes identified in copied fixtures were a required generation initializer and an adapted non-review retired-task refusal shape, retaining no-second-redemption/no-second-execution assertions.
- CI includes whole original reviewer binaries and new identity suites with floors. Green CI is verified, but it does not exercise these newly found schedules.
- Remaining evidence gaps include installed-Python protected paid lifecycle/Granted mode/payer mismatch; a Python 4097-byte proof boundary witness; new redemption-tombstone restart/binding controls; real provider-observed lapsed reservation reacquisition; and fault evidence for platform power-loss claims. Windows power-loss closure is not established by a published-file flush or the startup process-cancellation test.
- Generation restart tests should remove all retained rows before reopening if they intend to prove counter persistence independently of max-retained-generation reconstruction.
- The Python "lapsed reservation reacquired" test still permits equal expiry and cached preparation; that is not reacquisition evidence.
- Release notes still describe paid Retired as an in-body rejection rather than the structured payment refusal now returned.

## Reproduction and packet

Copy the two `test-witnesses/net/crates/net/...` files into a detached worktree at the exact candidate. Commands below run from `net/crates/net`. The SDK features are:

```sh
FEATURES='net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros fixtures'
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0

cargo nextest run -p net-payments --all-features --test review_repair_expiry \
  -E 'test(review_) or test(=a_later_authoritative_success_survives_a_duplicates_ambiguity)' \
  --no-tests=fail --retries 0 --no-fail-fast -j 4

cargo nextest run -p net-mesh-sdk --features "$FEATURES" --test review_repair_provider \
  -E 'test(review_) and not test(=review_aborted_startup_writer_cannot_outlive_its_owner)' \
  --no-tests=fail --retries 0 --no-fail-fast -j 4
```

Expected at this candidate: caller **three failed / one passed**; provider **three failed**. The startup probe requires its explicit barrier overlay; do not run it without that overlay and mistake a missing-barrier assertion for the ownership counterexample.

From the detached repository root, apply `startup-barrier.patch`, then run only:

```sh
cargo nextest run -p net-mesh-sdk --features "$FEATURES" --test review_repair_provider \
  -E 'test(=review_aborted_startup_writer_cannot_outlive_its_owner)' \
  --no-tests=fail --retries 0 --no-fail-fast -j 4
```

Run Cargo from `net/crates/net`, not the root. That probe should fail with the successor's new record missing. Restore the journal source to the candidate before applying the separate `startup-owner-control.patch`; the same test should then pass. These patches are review instrumentation/control, not proposed production commits.

Packet contains the report, both witness files, baseline runner and logs, new probe logs, both startup overlays, parsed test outcomes, and exact-head CI JSON. Return these findings to the same implementer; no architecture rewrite is requested. Acceptance requires repaired exact-head witnesses plus green CI, with platform/real-rail qualification stated separately.
