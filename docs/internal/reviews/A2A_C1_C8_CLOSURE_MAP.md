# A2A paid admission — C1–C8 closure map

**Candidate:** `84d178407` on `LZL0/a2a-payments`.
**Reviewed HOLD:** `A2A_PAID_ADMISSION_REVIEW_0743ca7.md` (candidate `0743ca779`).
**Exact-head CI:** https://github.com/ai-2070/net/actions/runs/35333578622 — **completed, success, 46/46 jobs**; all 13 A2A enforced steps green.

Everything below is **implementer-reported**. The numbers are runs I executed
myself at this head, not lane claims; where a lane is the only source it says so.

## Probe reproduction, before any repair

Reproduced at `f7e6b4bba`, which is `0743ca779` — the head the packet itself
pins — plus exactly one commit: "A2A review round 2: land the reviewer's
repair probes and packet verbatim". It is the minimum needed to run the probes
at all, since they do not exist at `0743ca779`.

The byte-identity of the production code is established by the diff, not by
reachability. An earlier version of this paragraph cited
`git merge-base --is-ancestor`, which was the wrong evidence: ancestry says
nothing about how many commits separate two heads or what they touched. The
claim rests on this instead, and it is cheap to re-run:

```
git rev-list --count 0743ca779..f7e6b4bba   # 1
git diff --name-only 0743ca779 f7e6b4bba    # 5 files
```

All five are review artifacts or probe files —
`docs/internal/reviews/A2A_PAID_ADMISSION_REVIEW_0743ca7.md`, the two
`startup-*.patch` files, `payments/tests/review_repair_expiry.rs` and
`sdk/tests/review_repair_provider.rs`. Filtering that list to anything outside
`tests/` or `docs/` returns **zero** paths, so no production source differs
between the reviewed head and the head the probes ran at.

The candidate being closed, `84d178407`, is the later repaired head — what the
repairs land on, not what the probes were reproduced against.

Reproducing the packet at that head:

| Suite | Reviewer's expectation | Observed |
|---|---|---|
| `review_repair_expiry` | 3 failed / 1 passed | identical |
| `review_repair_provider` (excl. C4) | 3 failed | identical |
| C4 with `startup-barrier.patch` | fails | fails — `new record afterwards=None` |
| C4 with `startup-owner-control.patch` | passes | passes |

Packet integrity: all five sha256s matched `SHA256.json` before landing.

## Closure

| # | P | Evidence | Repair | Acceptance witness |
|---|---|---|---|---|
| C1 | 1 | executed | In-flight decided **before** the expiry bound; expiry now bounds only what takes a *new* settlement. A lapsed-but-unresolved claim on an expired quote answers `InProgress`, never proven-unpaid. `settle()` converges on, or retains, authoritative success over a sibling's terminal verdict. | `review_expired_in_flight_payment_is_not_proven_unpaid` + `an_expired_quote_with_a_live_settlement_answers_ambiguity` (the anchored arm her probe's clock cannot reach) |
| C2 | 1 | executed | `slot_now` raises a caller's clock to a floor the store can prove; `decide` no longer takes a pre-await `now`; the free inline path holds one continuous slot from insert to claim. | `review_pre_pin_stale_clock_cannot_reacquire_an_occupied_slot` + `a_free_inline_claim_cannot_launch_on_a_slot_a_competitor_took` (her source-traced free variant, now executed) |
| C3 | 1 | executed | Launch bound to the exact admission identity (`claim_launch_exact`). Key-only `claim_launch` survives as the identity-unbound operator verb, bounded like `transition`. Operator resolve/forget stay legal mid-decision. | `review_operator_replacement_cannot_retarget_a_live_launch` |
| C4 | 1 | executed (overlay) | `open()`'s recovery publication runs under the same guards an ordinary mutation does, ownership handle included, released only when its I/O completes. Writer audit recorded: two publish sites, both now owning lifetime guards. | `review_aborted_startup_writer_cannot_outlive_its_owner`, **barrier only, no owner-control patch** |
| C5 | 1 / 2 | provider executed; Python caller source-established | `resolve` refuses an ambiguous key naming every candidate; `resolve_exact` closes a named incarnation. Python gains `generation=` on both operator exits, routed to the archive verb. | `review_provider_resolution_refuses_ambiguous_incarnations`, `an_ambiguous_key_resolves_only_the_incarnation_it_names`, `test_an_admission_is_resolvable_by_its_exact_generation`, `test_a_generation_scoped_resolution_never_reaches_the_live_attempt` |
| C6 | 2 | executed | Structural namespace separation: two maps, a private `RecordClass` is the only place a class becomes a map, so no caller-influenced id decides which map a write reaches. | `review_superseded_key_cannot_overwrite_a_live_purchase` + both insertion orders |
| C7 | 2 | executed | `retain_superseded` merges under the store lock by explicit evidence rank. Two-directional per the clarification: a settlement after an operator disposition neither reopens it nor vanishes — the disposition stands and the evidence is retained beside it. | `review_retained_success_cannot_be_downgraded_by_late_ambiguity` + `a_resolved_disposition_keeps_a_late_settlement_findable` |
| C8 | 2 | executed | The **finalized** request — after the signed org header is appended — is measured against one packet and refused before the pending oneshot is registered. | `a_protected_call_refuses_a_finalized_frame_over_one_packet` |

## Suite results at this head

| | Result |
|---|---|
| SDK acceptance set | 150 run, 149 passed — the one red is the C4 row run **bare**, failing its own precondition (`startup reached publication barrier`), which the packet warns against misreading |
| SDK, CI's blanket command (from `sdk/`, C4 row excluded) | **709 / 709**, 2 skipped |
| payments, 17 binaries | **187 / 187** |
| Python | **27 / 27** |

## Inverse receipts I re-ran myself

Beyond the lanes' own (6 + 6, each with hash-verified restores):

- **C1 lapsed arm** → reverting it reddens her C1 probe while the live-claim witness stays green: the two arms are independently load-bearing.
- **C2** → removing the atomic capacity guard yields 4 purchasable reservations against `max_in_flight=1`, her original number.
- **C4** → her barrier inserted by hand into the repaired worker (her patch no longer applies; the fix moved its context) passes with **no** owner-control patch. Restored, hash-verified, zero `NET_A2A_REVIEW` left in the product.
- **generation persistence** → forcing `next_generation` unpersisted gives `1 -> 1` reuse. The **pre-strengthening** version of that witness passes under the same mutation, which is the evidence gap she named.

## CI wiring

13 A2A enforced steps, every pinned name validated by parsing the workflow and
resolving it against the binary its own step runs. Floors from reported counts:
`a2a_paid_admission` 43, `a2a_admission_journal` 12, `a2a_call_bounds` 14,
`review_a2a_provider`+`review_a2a_wire` 45, `a2a_admission_identity` 15,
`review_repair_provider` 15, `a2a_caller_purchase` 23, `a2a_task_redeem` 6,
`payment_channel_deadline` 2, `review_a2a_caller` 27, `review_repair_expiry` 12,
`a2a_caller_identity` 15, `a2a_paid_end_to_end` 6.

**One documented exclusion.** `review_aborted_startup_writer_cannot_outlive_its_owner`
is excluded by name from both the floor step and the job's blanket
auto-discovery sweep. It only reaches its subject under her overlay; run bare it
fails a precondition. The three allowed responses were: ship the instrumentation
(forbidden by the packet), weaken her assertion (forbidden), or exclude by name
with the reason recorded — this is the third. Her file stays byte-identical and
the row stays runnable with the overlay.

## Reviewer probe integrity

All seven `review_*` files are byte-identical to what was exported, except the
two disclosed earlier and unchanged since: one required `generation` field in
`review_a2a_caller.rs`'s `seed()` fixture constructor, and one vendored
**non**-`review_` fixture in `review_a2a_provider.rs` retargeted with the
reasoning at the site. No `review_*` assertion has ever been weakened, reworded,
retargeted or deleted.

## Deferred qualifications — now closed

- **Generation-counter persistence isolated.** The witness closes every retained
  row, prunes, and asserts structurally that `records` and `detached` are both
  empty before the successor opens, so `max(rows, persisted)` has only the
  persisted counter to work from. Proven by inverse. (Self-correction recorded at
  the site: my first disk check was a substring search that the persisted
  `"next_generation": 1` matched — it failed a *correct* journal.)
- **Python lapsed-reservation no longer claims reacquisition.** Renamed, the
  `>=` expiry assertion removed. It proves the admission id converges across a
  lapse and the purchase completes. Provider-observed reacquisition is cited to
  the Rust witnesses that can see the store; claiming it from Python would need
  a provider-side prepare observation the binding does not expose, so it is
  named as a gap rather than dressed up.

## Still unproven — separately labelled, unchanged

- Real settlement rails. Every payments row uses the mock facilitator.
- The installed-wheel **protected paid** lifecycle end to end, plus Granted-mode
  and payer-mismatch Python coverage. The proof-intent seam is executed on the
  free path; the paid composition under a protected provider is
  source-established.
- Platform power-loss durability. The durability barrier is witnessed via an
  injected fault, not a machine crash; no unix-gated path compiles on the
  development host, so the parent-directory fsync leg is exercised only by CI.
- The C5 Python **caller** half end to end: the state is unreachable from the
  Python surface alone, and no fixture was fabricated to reach it.
- A cross-process or cross-host org-authority harness. The R11 witness runs both
  nodes in one process over loopback.

## Disclosure

Commit `dea169d1f` is titled as a release-note fix but also contains 34 lines of
`payments/src/engine/mod.rs`: I ran `git add -A` for a docs-only change while a
lane was mid-write. It was already on the remote when found, so history was not
rewritten; `954eee853` owns and completes that engine change. Anyone diffing
`dea169d1f` expecting docs-only will find otherwise.
