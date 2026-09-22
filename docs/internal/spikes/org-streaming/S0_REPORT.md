# Stage 0 report — executable lifecycle and ownership models

**Plan:** `docs/internal/plans/ORG_SCOPED_STREAMING_PLAN.md`, revision 2026-09-19
(Q1–Q7 resolved). **Branch:** `LZL0/org-streaming`. **Base:** `23f33bf98`
("Commit partial state." — slices 0.1/0.2/0.5 deliverables and the two model
files as first written, 74 witnesses). **This report:** 2026-09-22, covering the
Stage 0 closure work on top of that base.

Stage 0 changes no production wire, behaviour or export: both models are
`#[cfg(test)]` modules (`behavior/mod.rs` carries the reason inline — "Stage 1
ungates them as it wires the folds"), the probe is a separate guard workspace,
and the bench additions are bench-only. Everything below distinguishes
**executed** from **source-established** and names what never ran (§8).

## 1. Slice inventory and acceptance mapping

### 0.1 Baseline benches — delivered

- `sdk/benches/nrpc_common/mod.rs`: `Pair::protected()` (adopts an authority,
  mints an `OrgProofIntent`) and `call_protected_raw`; the public `Pair::new()`
  path is untouched and is the regression control.
- `sdk/benches/nrpc_unary.rs`: the `org_unary_open` group (raw codec, direct
  routing, the same three payloads), built **after** the public groups finish
  so the control bars are measured with the process state that existed before
  the group did.
- Acceptance (both groups run, numbers recorded beside the June-13 audit):
  met — `cargo bench --bench nrpc_unary --features net,cortex -p net-mesh-sdk`
  executed 2026-09-22; numbers in §5.

### 0.2 External consumer probe — delivered

- `guards/org_api_probe/`: own workspace, committed `Cargo.lock`, `MANIFEST`
  pinning 42 symbols — the org facade verbs, the typed streaming veneer, and
  the compatibility-ledger C1/C3/C4 literal surfaces (`CallOptions { .. }`,
  `RpcStreamingContext { .. }`, `AdmissionContext`, `AdmissionDenied`, …).
- CI step `Organizations — external org/streaming API probe` (after the
  fixtures-off probe): `cargo metadata --locked` first, then
  `cargo check --locked --message-format short`, each with the named-break
  diagnostic text pointing at ledger C1/C3/C4.
- **Executed here:** `cargo check` and `cargo metadata --locked` in the probe
  workspace — both green; the probe's `Cargo.lock` is unchanged by the runs
  (`git status` clean over `guards/org_api_probe/`), so the `--locked` legs
  will pass as written. **The CI step itself has not executed** (branch not
  pushed at report time); what is verified is that its exact two commands
  succeed at this tree.

### 0.3 Lifecycle model — delivered

`src/adapter/net/behavior/org_stream_lifecycle.rs`: `CallLifecycle` (the §2.6
record: `input` incl. `Closed`, `output` incl. `Draining(HandlerResult)`,
`terminal`, `incarnation`), `resolve_deadline`/`LifetimePolicy` (§2.1's three
bounds), `run_supervisor` (§2.2 tokio driver over abstract handler/pump/
semaphore/grant/terminal pieces — no fold, no network), `pull::PullCall` (the
same decision logic driven by an explicit `advance(now)` step with no executor
— the Q5 leaf has none; this shape was adopted at slice 0.5's finding 5),
`ProducerGate`, `sink_send` (§2.7's two refusal classes), and
`TerminalDisposition` (§2.8).

**39 witnesses, all green** (`cargo tl org_stream`: 76/76 across both models,
run before and after closure; acceptance bar ≥ 20). Obligations map:

| Obligation | Witness(es) |
|---|---|
| (a) SS starts `input = Ended` | `server_streaming_starts_with_its_input_half_already_ended` |
| (b) END idempotent, output untouched | `end_is_idempotent_and_never_touches_the_output_half` |
| (c) handler return + queued + zero credit ≠ terminal; grant completes the drain → `Completed(Ok)` | `a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it`, `runtime_free::a_blocked_drain_is_not_terminal_and_a_grant_completes_it_without_any_runtime`, `a_credited_drain_publishes_everything_then_completes` (positive control) |
| (d) deadline/cancel/revocation preempt a drain, discard the remainder | `the_deadline_fires_while_draining_and_discards_the_remainder`, `cancel_is_admissible_while_draining_and_preempts_completion`, `cancel_during_a_drain_preempts_the_completion_end_to_end`, `revocation_while_parked_on_credit_retires_within_the_bound`, `runtime_free::the_deadline_preempts_a_blocked_drain_and_discards_the_remainder`, `runtime_free::retirement_preempts_a_blocked_drain` |
| (e) handler `Err` is the terminal after drain, never `Ok` | `a_handler_error_survives_the_drain_as_the_terminal`, `runtime_free::a_handler_error_is_the_terminal_here_too` |
| (f) handler return closes open input; later CHUNKs dropped, END no-op | `handler_return_closes_an_open_input_half_and_preserves_the_result`, `an_undeliverable_admitted_input_item_kills_the_call` (late-input half) |
| (g) retire first-writer-wins incl. `Draining` | `retire_is_first_writer_wins_from_every_state` |
| (h) frames after terminal dropped; GRANT credited while `Draining` | `every_frame_after_the_terminal_is_dropped`, `grants_stay_admissible_while_draining_and_stop_once_output_ended` |
| (i) pump parked on zero credit stopped by retire; blocked producer wakes closed | `revocation_while_parked_on_credit_retires_within_the_bound`, `runtime_free::retirement_preempts_a_blocked_drain` |
| (j) terminal emitted exactly once, after pump stop | `the_terminal_is_emitted_exactly_once`, `run_supervisor`'s emit-after-pump-stop ordering (asserted by every supervisor witness's `emission` field) |
| (k) queued-data policy per reason = §2.2 table | `only_a_completion_drains_queued_output`, `the_deadline_fires_while_draining_and_discards_the_remainder` (discard leg) |
| (l) two calls independent | `two_calls_are_independent` (see §7 F7 — classified) |
| (m) undeliverable/over-budget item → `ResourceExhausted`, no `Ok` after | `an_undeliverable_admitted_input_item_kills_the_call`, `an_oversized_item_never_waits_for_permits_it_cannot_get`, `protected_output_refusal_cannot_complete_ok`, `runtime_free::an_item_larger_than_the_budget_is_refused_instead_of_queued` |
| (n) node-level refusal rolls back call+caller; cancel/dequeue/handoff single release | registry side: `node_refusal_rolls_back_call_and_caller_reservations`, `cancel_dequeue_handoff_consumes_one_permit` (§1.4) |

Deadline semantics (§2.1) additionally pinned by: default-for-omitted-only,
honour-not-clamp, refuse-over-cap, credential clamp with reason change,
provider-authority bound, exact-tie precedence, already-elapsed refusal,
checked-arithmetic overflow refusal, `default_live ≤ max_live` validation.

### 0.4 Transaction model — delivered

`src/adapter/net/behavior/org_stream_registry.rs`: `ProtectedCallRegistry`
(`reserve`/`release`/`install`/`begin_confirm`→`ConfirmTxn::transfer`/`retire`/
`complete`) with per-record incarnations, `authority_epoch`, selective
requalification, provisional and verified quota charging, byte budgets
(`ByteBudgets`, `ItemPermit` release-once bundles), an abstract authority
(floors, generation, exhaustion marker, poison, replacement) and an abstract
fold effect boundary. **37 witnesses, all green** (acceptance bar ≥ 14).

The `ConfirmTxn` carries the registry `MutexGuard` across check→transfer, and
`begin_commit` across check→commit: the interleavings are driven directly
through `RetireAttempt::Blocked` rather than left to a scheduler (§4).

### 0.5 Browser/SDK mapping — delivered

`docs/internal/spikes/org-streaming/S0_MAPPING.md` (read-only trace at
`85ecc77c9`): the portable-core extraction target, leaf caller/provider gaps,
the three JS surfaces (`LeafNode`, `MeshSession`, `ProxyBody`), pure-SDK
forwarding, fixture consumption paths, and eight named obstacles with their
smallest resolutions. No runtime acceptance is claimed by that document, and
none is claimed here.

## 2. Composition checks — all ten named rows executable

| Plan name | Witness (exact name) | Where | Separating observation asserted |
|---|---|---|---|
| `retire_between_confirm_check_and_owner_transfer` | same | registry | retire mid-transaction observes `RetireAttempt::Blocked` (bool-check/spawn mutant gets `Applied(true)` here), zero admitted effects before transfer, armed owner receives the later retire |
| `publication_before_notification_cannot_authorize_commit` | same | registry | a stale captured authority view cannot install or commit after revocation becomes authoritative |
| `retained_sink_clone_cannot_extend_drain` | same | lifecycle | handler returns with a live clone: clone's sends refused, admitted items drain, completion comes from the gate (600 s deadline arm present and not the cause), preserved handler result not overwritten |
| `client_stream_single_response_completes_without_pump` | same | lifecycle | early `Ok` on the upload shape completes `Completed(Ok)` with input still open at return and no pump event, no later END |
| `protected_output_refusal_cannot_complete_ok` | same | lifecycle | in-budget leg completes and publishes (positive control); over-budget leg latches `ResourceExhausted` and a later handler `Ok` cannot complete |
| `item_credit_does_not_imply_byte_reservation` | same | registry | item credits held by call A cannot oversubscribe the node byte cap against call B's charge |
| `cancel_dequeue_handoff_consumes_one_permit` | same | registry | one permit consumed per path; another call's live bytes stay charged across cancel/dequeue/handoff |
| `oversized_item_never_waits_for_impossible_permits` | same | registry | size over the item/call limit refuses immediately |
| `terminal_queue_refusal_is_not_peer_receipt` | same | lifecycle | positive leg: `Queued` + the receiver observes the terminal; full-queue leg `Refused`, gone-session leg `Unreachable` — both still complete ownership and the receiver never observes this call's terminal |
| `pretransfer_retirement_has_one_cleanup_owner` | same | registry | failed install, lost bridge and revocation converge on exactly one removal, no ownerless reservation |

Two of these (`protected_output_refusal_cannot_complete_ok`,
`terminal_queue_refusal_is_not_peer_receipt`) did not exist in the `23f33bf98`
tree and were written during closure; two (`retained_sink_clone_cannot_extend_drain`,
`client_stream_single_response_completes_without_pump`) were renamed to the
plan's exact names (count-neutral). Each refusal witness carries its positive
control in the same test or a named sibling.

## 3. Count accounting

| Tree | lifecycle | registry | total |
|---|---|---|---|
| `23f33bf98` (as delivered by the lanes) | 37 | 37 | 74 |
| after Stage 0 closure | 39 | 37 | 76 |

Delta +2 = the two missing composition checks (§2). Renames are count-neutral.
No witness was deleted. One assertion was rewritten — finding F1.

## 4. Interleavings and schedules — deterministic, no loom

The acceptance allows "loom … or deterministic interleaving otherwise"; Stage 0
took the deterministic branch. The windows that matter sit between an explicit
check and its commit, and the model exposes them (`begin_confirm`/`begin_commit`
hold the registry lock across the transfer; `retire_nonblocking` makes a
competing retire directly observable as `RetireAttempt::Blocked`), so each
schedule is driven by hand at the real boundary instead of searched for:

1. raise-between-reserve-and-install → `raise_between_reserve_and_install_denies_with_zero_effects`
2. retire-between-check-and-transfer (open transaction) → `retire_between_confirm_check_and_owner_transfer`
3. retire-before-transfer → `retire_before_transfer_prevents_every_admitted_effect`
4. retire-after-transfer-before-task-run → `retire_after_transfer_reaches_the_owner_before_its_task_runs`
5. publication-before-notification → `publication_before_notification_cannot_authorize_commit`
6. fold-prerequisite-refusal-before-transfer → `fold_prerequisite_refusal_before_transfer_releases_exactly_once`
7. scheduling-failure-after-transfer → `scheduling_failure_after_transfer_does_not_orphan_a_running_record`
8. pre-transfer triple convergence (failed install / lost bridge / revocation) → `pretransfer_retirement_has_one_cleanup_owner`
9. stale-incarnation late ops after key reuse → `late_operations_with_a_stale_incarnation_cannot_touch_the_successor`
10. check→commit window → `check_and_commit_are_one_ownership_operation`
11. byte-permit races → `cancel_dequeue_handoff_consumes_one_permit`, `removal_consumes_the_permits_its_call_still_owned`, `node_refusal_rolls_back_call_and_caller_reservations`

Loom is therefore **unused** at Stage 0 — stated, not inferred away. The
primitive choices (a `parking_lot::Mutex` whose guard is held across the
transaction) are the ones Stage 1 wires; a loom model would need those
primitives swapped, and the deterministic schedules above already redden the
bool-check/spawn mutant the check row names.

## 5. Baseline numbers (slice 0.1)

Host: Windows 11 Pro, i9-14900K, 2026-09-22. Command:
`cargo bench --bench nrpc_unary --features net,cortex -p net-mesh-sdk`
(criterion; 50 samples on `org_unary_open`, default sampling on the public
groups). Means with 95% CI, µs/call:

| Public control — `nrpc_unary_codec` | empty | 32B | 1KiB |
|---|---|---|---|
| raw | 39.58 [39.25, 39.92] | 46.06 [43.90, 48.54] | 45.64 [44.80, 46.52] |
| postcard | 40.68 [40.21, 41.17] | 42.13 [41.27, 43.14] | 45.25 [44.81, 45.72] |
| json | 40.98 [40.32, 41.69] | 42.63 [41.98, 43.35] | 48.78 [47.02, 50.75] |

| Public control — `nrpc_unary_routing` | empty | 32B | 1KiB |
|---|---|---|---|
| direct | 42.91 [42.19, 43.71] | 42.70 [42.10, 43.40] | 49.53 [47.38, 51.87] |
| discovery | 42.76 [42.00, 43.58] | 42.66 [41.58, 43.95] | 44.72 [44.04, 45.45] |

| `org_unary_open` (protected, raw, direct) | empty | 32B | 1KiB |
|---|---|---|---|
| protected | 236.13 [233.79, 238.80] | 245.34 [242.24, 248.57] | 242.56 [239.60, 246.02] |

Context: the June-13 audit (`docs/internal/misc/PERF_AUDIT_2026_06_13_NRPC_FOLLOWUP.md`)
recorded c1/32B ≈ **42.5 µs** after its optimizations and a ~5 µs physics
floor. This run's public 32B rows (42.1–46.1 µs) are the same order on a
different host and configuration; they are the **regression control** for this
change and were measured on an untouched process state by construction (see
§1.0.1). June-13's numbers are historical context, not a comparison baseline.

**Opening cost (the number Stage 0 exists to establish):**
`org_unary_open` − `nrpc_unary_codec/raw` ≈ **+195–200 µs per admitted call**
(236–245 µs vs 39.6–46.1 µs). Payload-independent (empty ≈ 32B ≈ 1KiB), as an
opening-bound cost should be: proof mint (Ed25519 sign over the transcript) +
credential/stamp/replay/policy verification + admission bookkeeping. Steady
state per-item cost of streaming is **not measurable yet** — no protected
streaming exists before Stage 1 — and is not extrapolated here.

## 6. Inverse receipts

Two receipt documents, one per model, generated by isolated lanes (one file
each, separate `CARGO_TARGET_DIR`s, reverse-anchored restores only — no
whole-file writes, no `cp`, no `git checkout`):

- **`S0_RECEIPTS_LIFECYCLE.md`** — obligations (a)–(m) and the four
  lifecycle-side composition checks. 23 mutation/restore cycles, every restore
  sha256-proven byte-identical against the campaign baseline `7e7fbfcc…`. One
  prescribed inverse executed literally was non-discriminating (finding F9);
  its discriminating replacement (d)-3b) is receipted alongside it, and the
  non-discriminating run is kept as executed evidence. (l) is classified
  plumbing (F7) with no inverse credit claimed.
- **`S0_RECEIPTS_REGISTRY.md`** (sha256 `c60f77c0…`) — the 24 items covering
  the §4 schedules, the registry-side composition checks, byte-permit,
  quota and incarnation behaviour. 30 witnesses across 24 receipts (grouped
  where one mutation covers several); **all 30 red under their assigned
  inverse, zero green-under-inverse**; the pre-mutation baseline sweep was
  30/30 green and every witness name was validated against the binary first.
  Every red is a runtime failure of the named witness (20 assertion failures
  with left/right and custom messages, 10 `expect` panics carrying the
  witness's expectation string) — zero compile-error reds. Receipt 1 is the
  `ConfirmTxn` fix's inverse: the pre-fix re-locking shape turns
  `RetireAttempt::Blocked` into `Applied(true)` at the "retirement is
  serialized with the check→transfer window" assertion. File restored
  byte-identical against `348fc6f4…` after all 24 cycles (a self-caught
  restore defect along the way — finding F12).

**Coordinator spot-checks** — one receipt per lane reproduced independently
before acceptance, verbatim matching the lane's recorded red:

| Spot-check | Mutation | Red (exit 100) | Restore |
|---|---|---|---|
| lifecycle comp-3 `protected_output_refusal_cannot_complete_ok` | drop `output_admission_failed()` at `sink_send`'s `len > budget` refusal | `Completed(Ok)` vs `ResourceExhausted`, `org_stream_lifecycle.rs:1810` | green |
| registry receipt 1 `retire_between_confirm_check_and_owner_transfer` | `ConfirmTxn` re-locking pre-fix shape | `Applied(true)` vs `Blocked`, `org_stream_registry.rs:2585` | sha `348fc6f4…`, green |

Post-lane coordinator delta (disclosed to the lifecycle lane via forensics,
recorded here): the queued rustfmt hunk in `terminal_queue_refusal_is_not_peer_receipt`
leg (c) and the spot-check mutation/restore cycles above moved the lifecycle
file from `7e7fbfcc…` (the lanes' campaign baseline) to `b2dce203…`; the
registry file is unchanged at `348fc6f4…`. No lane receipt is affected — every
lane cycle was hash-proven against its own baseline before any of these writes.

## 7. Findings

- **F1 — a pinned assertion contradicted §2.7 (executed).**
  `runtime_free::an_item_larger_than_the_budget_is_refused_instead_of_queued`
  asserted `is_live()` after a refused over-budget item — i.e. the call
  survives a silently dropped item, which §2.7 explicitly forbids ("a refused
  protected item latches `ResourceExhausted` and retires the call: it cannot
  drop an item and subsequently report complete success"). Rewritten to the
  latching rule (`run_to_terminal` yields `ResourceExhausted`, never
  `Completed(Ok)`). This is a property strengthening, named here because a
  rewritten assertion is otherwise indistinguishable from a weakening.
- **F2 — two named composition checks were absent (executed).**
  `protected_output_refusal_cannot_complete_ok` and
  `terminal_queue_refusal_is_not_peer_receipt` did not exist under any name in
  the `23f33bf98` tree. Both were written during closure, with in-test
  positive controls. Two further rows existed under near-miss names and were
  renamed to the plan's exact names (§2).
- **F3 — `TerminalDisposition::Sent` is source-established only.** The model
  has no transport; `Queued`, `Refused` and `Unreachable` are executed
  (`terminal_queue_refusal_is_not_peer_receipt`), `Sent` is the transport-accept
  seam Stage 1 fills. Never upgraded to executed.
- **F4 — pull-driver transient saturation is a caller-visible refusal, not a
  wait.** The async sink waits on satisfiable bounds (§2.7 `send_wait`); the
  runtime-free sync driver cannot wait, so `PullCall::submit` refuses without
  latching when the item fits the configured budget but not current capacity,
  and latches only for unsatisfiable/non-deliverable items. Documented at the
  method; Stage 1's production sink must implement the wait, not this
  approximation.
- **F5 — the confirm-transaction fix.** The `23f33bf98`-era `ConfirmTxn`
  re-acquired the registry lock in `transfer`, reopening the exact check→transfer
  window the transaction exists to close (a retire could win the terminal and
  `transfer` would then resurrect the record over an unarmed owner). Fixed by
  carrying the `MutexGuard` through the transaction; the inverse receipt is
  Registry item 1 in §6.
- **F6 — naming drift against the plan's check table** (two rows) resolved by
  rename (§2); no behaviour change.
- **F7 — `two_calls_are_independent` is plumbing at model level.** Two records
  with distinct incarnations cannot interfere in a value-semantic model; no
  production-code inverse exists that this witness can see. Classified per
  witness discipline and kept out of the inverse roster (the real
  cross-independence is witnessed in the registry by
  `two_independent_calls_both_reach_running`,
  `serve_handle_drop_retires_only_its_own_registration` and
  `requalify_keeps_the_unaffected_sibling_and_retires_the_affected_call`, which
  do have inverses).
- **F8 — `an_oversized_item_never_waits_for_permits_it_cannot_get` (lifecycle)**
  describes an output item but drives the input-side latch
  (`input_admission_failed`). Kept as the §2.7 request-direction witness and
  re-described in §1.0.3's map under (m); the output-direction counterpart is
  `protected_output_refusal_cannot_complete_ok`.
- **F9 — retirement of a credit-parked pump is overdetermined (executed).**
  `run_supervisor`'s retire path stops the pump through two independent
  mechanisms — `Semaphore::close` and `JoinHandle::abort`+join — so removing
  the closes alone reddens nothing (receipt (d)-3a). The discriminating
  inverse removes both (d)-3b). Both mechanisms stay in the design for
  Stage 1: the closes are also what wakes `send_wait` credit waiters with a
  closed error (§2.2's table), a role `abort` cannot play.
- **F10 — the retained-sink witness does not attribute the refusing
  mechanism (executed).** In the witness's schedule (clone sends after the
  call completed) the send is refused by the closed channel, so removing
  `sink_send`'s producer-gate check changes nothing (receipt appendix); the
  discriminating inverse is omitting `gate.finish()` at handler return. The
  behavioral claim ("new sends fail, drain does not extend") is witnessed;
  the gate check's mid-drain attribution is source-established only.
- **F11 — driver separation is real.** `runtime_free::retirement_preempts_a_blocked_drain`
  drives `pull::PullCall` and is unreachable from any `run_supervisor`
  mutation; its credited inverse is the state machine's retire-admissibility
  (receipt (i)). Each driver's witnesses are credited under their own
  driver's mutations.

Lane L executed 23 mutation/restore cycles (all sha256-verified byte-identical
restores) for obligations (a)–(m) and the four lifecycle-side composition
checks. One of its receipts was reproduced independently by the coordinator
before acceptance: the `protected_output_refusal_cannot_complete_ok` latch
mutation (drop `output_admission_failed()` at the `len > budget` refusal) →
exit 100 with the same assertion (`Completed(Ok)` vs `ResourceExhausted`, line
1810) → restored → green.
- **F12 — a restore defect caught by the lane's own restore-proof gate
  (executed).** Registry receipt 3's first reverse edit left two stray blank
  lines; the per-restore sha256 check refused it, the delta was localized with
  a read-only diff, one anchored edit repaired it, and the receipt was
  re-proven byte-identical with a fresh green run. Recorded because
  "restored" without a hash is a claim, and the hash is the gate.
- **F13 — a mutant double-panic can outshout the recorded red (executed).**
  Registry receipt 16's mutant panicked in `Drop` during unwind *after* the
  witness's own assertion had already failed (`left: 0, right: 1500` at
  `org_stream_registry.rs:3365`), and Windows fail-fast turned the second
  panic into an abort (0xc0000409). The credited red is the witness
  assertion; the abort is mutant fallout and proves nothing about the
  property either way.
- **F14 — cross-lane file drift is real; hashing is the defense (executed).**
  The coordinator's rustfmt write landed in the sibling model file mid-campaign
  (the forensics exchange is preserved in the lifecycle lane's transcript and
  its receipt doc's post-handoff note). The registry lane detected the drift
  via per-run sibling sha256 tags, re-attributed its baseline to the
  post-write state (30/30 green sweep) and completed unaffected. Stage 1 lanes
  touching shared files keep the same discipline: hash at start, tag every run.

Lane-process findings (F12–F14) and the two below are recorded because the
receipt discipline is part of the stage's evidence: each was caught by a hash
gate or a checklist the stage had adopted before the incident.

- **F15 — the pre-push clippy matrix catches what the test runs cannot
  (executed).** Adding the §2.8 control path as `run_supervisor`'s ninth
  argument crossed clippy's arity threshold: `clippy --all-features
  --all-targets` with CI's flag set (the only leg that compiles lib-test under
  clippy) failed with `too_many_arguments` while every witness run stayed
  green. Fixed with a targeted `#[expect(clippy::too_many_arguments, reason =
  …)]` following the repo's established convention (`mesh.rs` has the exact
  precedent — "pushed it one over the arg-count lint"; `rtc/driver.rs` the
  expect-with-reason form). 76/76 re-verified green after the change.
- **F16 — an out-of-space write truncated a source file; hash discipline
  recovered it (executed).** A write during the arity fix hit `ENOSPC` and
  truncated `org_stream_lifecycle.rs` to 0 bytes (empty-file sha `e3b0c44…`)
  while the VCS showed an ordinary modification — the exact failure mode the
  harness warns about. Recovery: `git restore` from the immediately preceding
  commit (the file had no uncommitted changes), verified byte-exact by sha256
  (`b2dce203…`) and line count (2045), then the fix re-applied and re-verified
  (2052 lines, `45aef9d6…`). Cause: the build-artifact cache had grown to
  29 GB and both local volumes were at 100%. Rule carried to Stage 1 lanes:
  while free space is under a few GB, verify size and hash after every write.

## 8. What never ran

- No Linux or macOS leg (Windows 11 workstation); `#[cfg(unix)]` code is not
  compiled here. Nothing in Stage 0 is platform-gated, but that is a
  source-level observation, not an executed one.
- No loom model-checking (deliberate — §4).
- CI itself has not executed (branch not pushed at report time), including the
  new probe step; its exact commands were run locally instead (§1.0.2).
- No production witnesses of any kind: the models stand in for fold/registry
  wiring that does not exist yet. Model passes do not establish measured
  production capacity (the plan's own caveat, repeated here because these
  numbers will be quoted).
- Bench figures are one host, one run; criterion `change` deltas reference only
  the previous local run and are not reported as findings.
