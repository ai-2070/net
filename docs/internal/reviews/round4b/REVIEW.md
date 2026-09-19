# A2A round-4 re-review — one D2 residual

**HOLD, narrowly: D2's existing-archive branch still bypasses the exact live incarnation's operator disposition.** D1, D3, D4 and the strengthened C5 witness receive closure credit. No architecture or scope expansion is requested.

## Exact scope and verified evidence

- Runtime candidate: `1067c6fee6032f14752a9c217338dfbc089f2b2b`.
- Handoff: `44750cc0a0f12c61aeff2452a0937fb984185e2e`; its only change after the runtime candidate is the plan document. Executions below are attributed to the runtime candidate, not to a CI run on the documentation successor.
- Previous review: `84d178407d05664f2372fb62b6a0ddae1d235c43`.
- Exact-head CI: https://github.com/ai-2070/net/actions/runs/35395627084 — completed successfully; all 46 jobs returned by the API were successful. Page annotations do not override the actual job conclusions.
- Local SDK closure/predecessor selection: **17 passed**, 15 copied/nonselected tests filtered.
- Local payment closure/predecessor selection: **29 passed**, 43 copied/nonselected tests filtered.
- All five probes that failed in the previous packet now pass.
- Exact-head Python CI logs independently confirm `tests/test_a2a_history_boundary.py::test_review_python_resolves_history_without_touching_live PASSED` in both developer-binding job **105763696676** and shipped-wheel job **105763696804**.
- Additional unchanged-production-source probe: **1 failed**, 15 fixture tests filtered; normal compile, nextest exit 100.
- A separate read-only reviewer independently identified the same missed branch. Its source opinion is distinct from the executed probe below.

Local commands/outcome names, the new witness, its log, and exact-head CI metadata are included in the packet. No real funds moved; the witness uses the existing real-engine/mock-facilitator fixture. No production edits were made.

## Remaining finding — D2 / C7, P2

**Witness:** `review_existing_archive_inherits_live_disposition_on_late_success` in `payments/tests/review_round4_disposition.rs`.

The repair handles a live disposition when there is no archive entry. The sibling branch where the archive already exists ignores that same disposition.

### Executed schedule

1. Prepare A and pause its actual mock-facilitator settlement.
2. Publish the sibling's exposed refusal using the existing identity-guarded store transition, as the earlier D2 witness does.
3. Release settlement. Its successful financial result cannot enter the live RefusedExposed row, so the production caller flow retains an actual **PaidUnexecutable** archive entry with proof/billing.
4. Resolve live A through `resolve_attempt(Closed)` with an operator outcome/evidence. Both records name the same key and generation.
5. Deliver another authoritative completion for that exact incarnation through the production `retain_superseded` operation, using the actual retained settled state. No JSON file edits or invented payment evidence are involved.
6. The live operator document remains untouched, but the archive stays **PaidUnexecutable** instead of retaining the late evidence with that disposition.

Observed output:

```text
same generation: live=Resolved, archive=PaidUnexecutable
assertion `left == right` failed:
existing archive bypassed the live disposition of the same charge
  left: PaidUnexecutable
 right: Resolved
```

The probe verifies **one billing event**. This is not a demonstrated second charge, lost payment proof, or overwrite of the live disposition. It is the remaining violation of the agreed rule that late evidence must respect the disposition of the exact incarnation whichever map holds it.

**Evidence boundary:** the first settlement and archive creation use the real caller flow. The exposed refusal is staged through the production guarded store verb, and the duplicate late completion is delivered through the production retention API. This is a store-composition witness, not a claim that two network replies were independently paused end-to-end.

### Source cause

At `net/crates/net/payments/src/flow/a2a.rs:1276–1286`:

- the code correctly finds the live exact-generation Resolved disposition;
- if an archive already exists, it calls `merge_retained(existing, incoming)` and returns;
- only the archive-absent branch at `1288–1302` consumes the discovered disposition.

Thus the relative order of archive creation and live resolution changes whether the same operator decision is honored during late evidence retention.

### Required closure

Make both existing-archive and absent-archive paths reconcile the exact-incarnation disposition and retained financial evidence under the same lock. Preserve existing proof/billing and opaque operator evidence; do not overwrite an unrelated generation or a separately resolved archive disposition. Keep the original empty-archive D2 witness green and add this complementary schedule to the enforced suite.

This is the same D2 obligation, not a request to redesign storage or add another framework.

## Credited repairs and filter ruling

- **D1/C2:** acquisition time has its own monotonic, persisted per-service floor; both paid/free old-reservation reacquisition probes pass. Retention time is not repurposed.
- **D2:** the original empty-archive schedule is fixed and passes; only the branch above remains held.
- **D3/C7:** the outer envelope preserves the complete opaque operator JSON value, including colliding field names; the strengthened maintained witness and the original reviewer collision probe pass. This is JSON-value preservation, not a promise to preserve source whitespace.
- **D4/C6:** the deserializer classifies prior-format rows by stored map ID versus the embedded logical key, rather than suffix spelling; historical addressability is restored on first read. The old-format probe passes.
- **C5:** the populated historical/live Python witness is landed, preserves the review assertions, and is genuinely executed in both exact-head Python CI jobs. The previous false-green witness is no longer offered as the closure proof.
- **Filtering copied fixtures is accepted.** `review_round3_caller.rs` contains a frozen copy of the old `a2a_caller_identity` fixture plus three reviewer probes. Its copied old evidence-shape assertion conflicts with the D3 repair by design. The workflow preserves and pins all three `review_*` probes, while running the maintained identity binary with its updated stronger assertion. Keeping the frozen file and filtering its obsolete duplicate fixture is not weakening the review obligation.

Previously credited C1/C3/C4/C8 are not reopened. Real rails, protected paid lifecycle qualification, cross-process/cross-host authority evidence and power-loss proof remain separately unproven; the ordinary C5 shipped-wheel witness is not a protected paid lifecycle witness.

## Reproduction

Copy `test-witnesses/net/crates/net/payments/tests/review_round4_disposition.rs` into a detached checkout at the runtime candidate. Run from `net/crates/net`:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo nextest run -p net-payments --all-features \
  --test review_round4_disposition \
  -E 'test(=review_existing_archive_inherits_live_disposition_on_late_success)' \
  --no-tests=fail --retries 0 --no-fail-fast -j 4
```

Expected before repair: one assertion failure, not a compile failure. The remaining tests in the file are copied fixtures, not additional review findings.

**Next action:** return this one bounded D2 branch repair to the same implementer. Preserve the closure credit above, then return a new exact candidate and green CI. The user branch was untouched; the temporary witness was exported and removed from the detached checkout.
