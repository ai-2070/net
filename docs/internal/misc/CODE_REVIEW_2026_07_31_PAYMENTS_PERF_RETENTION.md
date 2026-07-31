# CODE REVIEW 2026-07-31 — Payments hot path: store compaction + lazy hot-path work (`performance-x402`)

> **STATUS: OPEN.** No finding in this document has a landed fix. One blocker
> (B1, formatting) is mechanical. §1–§5 are scope/justification findings against
> otherwise-correct code — none of them is a live defect on the mock rail, and
> each is stated with the condition under which it becomes one. The nits are
> optional.
>
> **This review did not exercise the branch's central durability claim.** The
> P5e "a sweep makes an otherwise-clean pass dirty" assertion, plus
> `read_only_writes_audit.rs` and `redeem_denial_no_write.rs`, are unix-gated and
> ran **0 tests** on the Windows host this pass ran on. See
> [Verification](#verification). A Linux/macOS run is required before merge.
> The audit doc already flags this; it is repeated here because it is the one
> gap this pass could not close itself.

**Scope:** the full branch diff `master..26de2e7ae` (merge base `3feeb811b`) —
11 commits, 5 files, +1529/−48.

| file | what |
|---|---|
| `payments/src/engine/mod.rs` | terminal-record compaction; lazy `QuoteRecord` construction |
| `payments/src/policy/spend.rs` | lazy quote canonicalization on the approval path |
| `payments/src/policy/store.rs` | compact (not pretty) store writes |
| `payments/tests/engine_retention.rs` | new — 15 retention tests |
| `docs/internal/misc/PERF_AUDIT_2026_07_31_PAYMENTS_HOT_PATH.md` | new — the audit these commits discharge |

**Overall.** Careful work. The retention design reasons about the right hazards
— resurrection, owner-checked co-pruning, fail-closed migration, tombstone
permanence — and the test file targets each one individually rather than
asserting on aggregate state. The findings below are predominantly about
*residual scope* (what compaction deliberately does not bound) and about two
comments whose stated justification is narrower than the behavior it defends.
Per the review-tracking rule, the `B*` / `§N` labels are for this document only
— they do not belong in code or commit messages.

---

## Blocker

### B1 — `cargo fmt --check` fails in 8 places, all branch-added

```
cargo fmt -p net-payments -- --check
```

fails at `src/engine/mod.rs:688` (the `prune_terminal(...)` call, which rustfmt
collapses to one line) and at seven `assert_eq!` sites in
`tests/engine_retention.rs` (lines 221, 258, 427, 456, 484, 513, 547). Every
diff is in a line this branch introduced; `master` is clean.

**Fix:** `cargo fmt -p net-payments`.

---

## P2 findings

### §1 — The growth bound covers only fully-redeemed quotes; the case compaction was introduced to bound is still unbounded

`QuoteRecord::is_prunable_at` (`src/engine/mod.rs:358`) requires `redeemed`. Two
classes of record are therefore permanent:

- **Settled, billed, published, never redeemed.** A *normal* outcome — the
  caller pays and then crashes, times out, or simply abandons before invoking
  the tool. These are full-fat records (both preserved base64 carries).
- **Frozen.** Every invalidation path (`network_mismatch`, transaction replay,
  amount mismatch) sets `frozen` and leaves `redeemed` false.

The exclusion is deliberate and pinned (`an_unredeemed_record_is_never_pruned`),
and it is the right call as the code stands: `redeem_for_invocation` performs no
expiry check at all, so a paid entitlement never expires, and pruning its record
would destroy a thing the caller paid for. The problem is not the behavior — it
is that the stated bound does not hold in the presence of it.
`DEFAULT_TERMINAL_RECORD_RETENTION_NS`'s doc comment claims:

> At 1 000 paid calls/day this retains ~250 records (~0.8 MB) instead of letting
> months of fat records ride every whole-file transaction.

That arithmetic assumes ~100% redemption. At 90% redemption the retained set
grows by ~100 fat records/day forever, and every one of them rides every
subsequent whole-file transaction — which is precisely the cost term §1 of the
audit was opened against.

**Suggested disposition:** (a) state the residual in that doc comment rather
than leaving the ~250-record figure unqualified; (b) consider whether an
entitlement horizon (a redemption deadline, which would make the unredeemed
class prunable *and* bound how long a paid quote can sit outstanding) belongs on
the roadmap; (c) at minimum make the unbounded class observable — a
`tracing::warn!` when `s.quotes.len()` crosses a threshold, since today the only
symptom is gradual latency on every payment.

### §2 — Prunability is time-based, not tier-based: an `observed`-tier record can retire before it was ever independently verified

`is_prunable_at` gates on `billing.is_some() && billing_published && redeemed &&
!in_flight` plus the expiry horizon. It does **not** consult the record's
verification tier. A provider that calls
`accept_payment(.., VerificationTier::Observed, ..)` — settling on the
facilitator's receipt alone, and the facilitator is explicitly *not in the trust
root* — produces a record that becomes prunable 6h past quote expiry having
never been checked against the chain. After the sweep,
`re_verify_with_checker` (`src/engine/mod.rs:1360`) returns
`Rejected { BadQuote("unknown quote") }` and a revert or reorg can no longer be
attributed to that payment at all.

The const's doc comment argues the horizon is "comfortably above the Base pack's
~1h final-depth posture (`FINAL_DEPTH_BASE` = 1800 L2 blocks ≈ 1h at 2s/block)".
That is a claim about one network's current configuration, not an invariant the
code enforces. It silently stops holding for a deployment on a slower rail, one
that raises `FINAL_DEPTH_*`, or one that runs re-verification as a nightly batch
rather than inline.

**Suggested disposition:** gate prunability on the record's last verified tier
having reached `final` (or the deployment's configured required tier) in
addition to the time horizon. This is strictly stronger than the current test —
the time floor stays, the tier floor is added — and it converts a documented
assumption about Base into something the engine checks. If the tier gate is
rejected, the const's comment should say plainly that a deployment serving at
`observed` with out-of-band re-verification must configure `None`.

### §3 — The `consumed` co-prune's stated justification is not the guard that actually holds on a real rail

`prune_terminal` (`src/engine/mod.rs:393`) removes the retiring record's payload
replay entry, justified inline as:

> The payload hash protects claim-time concurrency before a settlement
> transaction is known; once the quote is terminal its enduring guard is the
> transaction tombstone.

The tombstone is keyed `network|transaction`. It therefore catches a replay only
when the replayed payload resolves to the **same transaction id**.
`a_retired_settlement_transaction_is_still_rejected_later` passes because — as
its own doc comment states — the mock facilitator derives its transaction id
from the payload bytes. On a real rail the property that actually prevents a
second settlement of the same authorization is scheme-level and single-use:
the EIP-3009 nonce, the SVM transaction's recent blockhash / signature, the XRPL
sequence number. Those hold, so this is not a live defect — but the guard is not
the tombstone.

What makes it worth recording is that the *same function's* doc comment refuses
to lean on scheme-level invariants when justifying the opposite decision:

> the generic engine/checker path does not universally establish that a
> transaction fell inside a given quote's validity interval […] Bounding *their*
> storage needs a scheme-level invariant about when a settlement identity may be
> forgotten

Two opposite stances on scheme-level reasoning, ~20 lines apart in one function.
The asymmetry may well be correct (forgetting an identity is a stronger ask than
relying on single-use authorization), but it should be argued, not left implicit.

**Suggested disposition:** amend the co-prune comment to say the payload guard's
retirement rests on the scheme's single-use authorization semantics, name them,
and state why that is a weaker dependency than the one the tombstone paragraph
declines to take.

### §4 — `canonical_bytes` moved *inside* the cross-process advisory lock

`check_and_reserve` previously canonicalized + base64'd the quote before opening
the store transaction; it now does so inside `require`, at
`src/policy/spend.rs:329`, i.e. while holding the sidecar advisory lock.

The win is real and correctly argued: the allowed path and the already-pending
path — the common case under a duplicate storm — no longer pay for a full
`to_value` + recursive canonical write + base64 over the whole envelope. But the
first-approval path now pays that cost *under the lock*, on a store whose
contention has its own document (`payments-spend-contention.md`) and whose
lock-wait is a 1ms-doubling poll loop shared by every mutator in the deployment.

This is probably fine — an approval-required decision is human-gated and rare,
and one quote's canonical bytes are small. But it is the one direction in which
this branch regresses, and neither the inline comment nor audit §3 mentions the
critical-section move. A reader of either would come away believing the change
is a pure win.

**Suggested disposition:** record the tradeoff in the comment and in audit §3.
If the first-approval path is ever measured to matter, the fix is to
canonicalize before the transaction *only* when a pre-check suggests approval is
likely — but that re-introduces a second read, so leaving it as-is with the
tradeoff documented is the right call.

### §5 — New public surface and an observable behavior change, documented nowhere outside the crate

The branch adds three public items —
`DEFAULT_TERMINAL_RECORD_RETENTION_NS`, `PaymentEngine::with_terminal_record_retention_ns`,
`PaymentEngine::prune_terminal_records` — and changes observable behavior:
**`PaymentEngine::status()` now returns `None` for a completed quote 6h past its
expiry.** Anything polling `status` for receipts, audit display, or reconciliation
sees a completed payment disappear.

`bed1fa4de` is marked `refactor(payments)!`, but neither
`.claude/skills/net-payments/provider.md` nor
`web/src/content/docs/payments/the-lifecycle.md` was touched, and the skill's
`gotchas.md` — which already carries the P5e clean-denial gotcha — says nothing
about compaction.

Not a parity violation: the knob is absent from the Python/Node bindings, but so
are `with_expiry_tolerance_ns` and `in_flight_ttl_ns`, so the bindings remain
internally consistent. The gap is documentation only.

**Suggested disposition:** a paragraph in `provider.md` (the knob, the default,
and `None` as the opt-out), and one line in the lifecycle doc noting that
`status` is not an audit surface past the horizon — the `BillingLog` is.

---

## P3 / nits

### §6 — `prune_terminal`'s removal loop re-looks-up a key it just produced

`src/engine/mod.rs:422`:

```rust
if let Some(rec) = s.quotes.get(quote_id) {
    let payload_hash = rec.payload_hash.clone();
    …
}
s.quotes.remove(quote_id);
```

The `get` is always `Some` — `quote_id` came from `s.quotes` in the same
transaction, unmutated in between — so the `if let` reads as defense against a
state that cannot occur. `if let Some(rec) = s.quotes.remove(quote_id)` performs
the removal and yields owned access in one lookup, dropping both the second
lookup and the `payload_hash.clone()`.

### §7 — `prune_terminal_records`'s count could underflow

`src/engine/mod.rs:1846` computes `before - s.quotes.len()`. Correct today, but
it debug-panics if `prune_terminal` ever grows the map. Having `prune_terminal`
return `retiring.len()` instead of `bool` removes the subtraction entirely and
lets the `accept_payment` site use `count > 0` for its dirty flag — same
information, one fewer invariant to hold.

### §8 — The compactness assertion is weaker than it needs to be

`src/policy/store.rs:322` asserts `!raw.windows(2).any(|w| w == b"\n ")`.
Compact `serde_json` output contains no `\n` at all (string contents are escaped
as `\\n`), so `!raw.contains(&b'\n')` is both simpler and strictly stronger.

### §9 — `Some(0)` normalization is silent in the only channel that matters

`with_terminal_record_retention_ns` refusing `Some(0)` and normalizing to the
default is well reasoned — 0 conventionally reads as "off" but would mean the
most aggressive setting. Given the builder returns `Self` there is no error
channel, so `tracing::warn!` is the only option available. Worth noting only
that config-validation warnings emitted at construction are easy to miss in a
deployment that initializes before its subscriber is installed.

---

## Verified correct

Recorded so a later pass does not re-derive them:

- **Prune-before-claim cannot resurrect the quote being accepted.** The expiry
  rejection at `src/engine/mod.rs:637` runs strictly before the claim
  transaction, and `retention_ns >= 1` is guaranteed by the `Some(0)`
  normalization — so `now_ns >= expiry + tolerance + retention` implies the
  quote was already rejected. Two independent guards, as the comment claims.
- **A caller-supplied `now_ns` cannot force an early sweep.** Reaching the sweep
  at all requires `now_ns < expires_at_ns + tolerance`, which bounds the clock a
  payer can present to inside the quote's own validity window. A far-future
  `now_ns` is rejected as `QuoteExpired` before the transaction opens.
- **Owner-checked `consumed` co-prune** — pinned by
  `a_payload_guard_owned_by_another_quote_survives_the_prune`.
- **`expires_at_ns: Option<u64>` migration is fail-closed** — `None` means never
  prunable, never inferred from the advisory x402 `maxTimeoutSeconds`.
- **The sweep folds into the `dirty` flag** (P5e), each disjunct derived from the
  branch that mutated. *Unverified on this host — see below.*
- **`consumed_transactions` is untouched under every retention setting.**
- **Verify-rejected records do not accumulate:** `release_claim`
  (`src/engine/mod.rs:1889`) removes the record and its payload entry when the
  chain is still empty, so the unbounded classes are exactly the two named in §1.
- **The compact-JSON switch is backward compatible** and does not touch
  `core::canonical`; the cross-language golden vectors pin a different encoder.

---

## Verification

Run on Windows 11 from `net/crates/net`:

| check | result |
|---|---|
| `cargo test -p net-payments` | green |
| `cargo test -p net-payments --test engine_retention` | **14 passed**, 0 failed |
| `cargo fmt -p net-payments -- --check` | **8 diffs** — see B1 |
| `cargo clippy -p net-payments --lib --tests` | no new lint classes; the ~600 `unwrap`/`expect` restriction-lint hits are pre-existing repo-wide and the new test file is consistent with the rest |

**Not exercised on this host.** `engine_retention.rs` reports 14 of its 15 tests;
`a_sweep_persists_on_an_otherwise_clean_pass_then_settles` is unix-gated (it
needs an inode witness) and did not run. `read_only_writes_audit.rs` and
`redeem_denial_no_write.rs` both reported `running 0 tests` for the same reason.
Those three suites are the entire evidence base for the P5e discipline this
branch extends — **a Linux/macOS run is required before merge.**

**Pre-existing, not from this branch.** `cargo clippy --all-targets` fails to
compile bench `spend_contention` on Windows (`std::os::unix`,
`Metadata::ino()`). Unrelated to these commits; noted so a later pass does not
misattribute it.
