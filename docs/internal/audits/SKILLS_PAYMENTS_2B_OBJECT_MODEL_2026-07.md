# Skills audit — payments 2b: object model + verification state machine

Slice 2b of [`SKILLS_VERIFICATION_PLAN.md`](../plans/SKILLS_VERIFICATION_PLAN.md).
Audits `.claude/skills/net-payments/object-model.md` (266 lines) and the tier /
chain semantics in `verification.md` against source.

| | |
|---|---|
| **Source SHA** | `a147535e2` (tree unchanged from `38a425b` for `payments/src/`) |
| **Date** | 2026-07-28 |
| **Auditor** | Claude (agent) |
| **Independent review** | not required for this slice (no security boundary) |
| **Result** | **58 struct fields + 6 tag constants + 4 enums + 8 behavioural claims checked. 0 fail.** |

## Method

Field-by-field, not spot-check. Every field of all five envelopes was compared
against its `struct` for **name, type, and optionality** — the last of these
matters because `Option<T>` in a table but `T` in source (or the reverse) is
exactly the kind of error that produces code which compiles for the author and
fails for the reader. Serde attributes were read alongside, since
`skip_serializing_if` changes what a consumer actually sees on the wire.

## Ledger — the five envelopes

| # | Claim | Authoritative source | Method | Result |
|---|---|---|---|---|
| 1 | Six object-tag constants and their exact `@1` strings | `core/{terms,quote,settlement_ref,verification,billing_event}.rs` | constant read | ✅ all six exact |
| 2 | `PricingTerms` — 6 fields | `core/terms.rs` | field-by-field | ✅ exact, incl. `#[serde(flatten)] extra` |
| 3 | `PaymentQuote` — 13 fields | `core/quote.rs` | field-by-field | ✅ exact, incl. `input_hash` / `signature` optional |
| 4 | `SettlementRef` — 10 fields | `core/settlement_ref.rs` | field-by-field | ✅ exact |
| 5 | `VerificationEvent` — 12 fields | `core/verification.rs` | field-by-field | ✅ exact, incl. `transaction` / `prev` / `signature` optional |
| 6 | `BillingEvent` — 17 fields | `core/billing_event.rs` | field-by-field | ✅ exact, incl. all four optionals |
| 7 | `PricingTerms::new(provider, capability, accepts, asset_registry)` | `core/terms.rs` | signature | ✅ order matches |
| 8 | `PaymentQuote::new(provider, caller, capability, input_hash, requirements, asset_registry, issued_at_ns, expires_at_ns)` | `core/quote.rs` | signature | ✅ 8 params, order matches |
| 9 | `SettlementRef::new(quote_id, settlement, facilitator, settled_at_ns, signer)` | `core/settlement_ref.rs` | signature | ✅ order matches |

The struct the skill labels `net.payment.verification@1` is named
`VerificationEvent` in source. The skill uses the correct name in its code block
and the tag as the section heading — not a defect, but noted because it is the
one place a reader searching for `PaymentVerification` finds nothing.

## Ledger — enums and state machine

| # | Claim | Authoritative source | Method | Result |
|---|---|---|---|---|
| 10 | `VerificationTier { Observed, Confirmed(u32), Final }` | `core/verification.rs` | enum read | ✅ |
| 11 | Ordering is `Observed < Confirmed(n) < Confirmed(n+1) < Final` | same, `rank()` | implementation | ✅ `rank()` returns `(0,0)` / `(1,n)` / `(2,0)`, compared as a tuple — gives exactly that total order |
| 12 | Wire form `"observed" \| {"confirmed":6} \| "final"` | same | serde attrs | ✅ `rename_all = "snake_case"` + serde's default externally-tagged representation |
| 13 | `InvalidationReason { Reorg, Expired, Replay, AmountMismatch, Rejected }` | same | enum read | ✅ five variants, exact |
| 14 | `from_facilitator_reason`: unknown → `Rejected` | same | body read | ✅ `_ => Self::Rejected`; `reorg` matched by substring, three others by exact string |
| 15 | `ExceptionKind { Overpayment }` — verifier never auto-satisfies | same | enum read | ✅ single variant |
| 16 | `VerificationStatus { Verified, Invalidated{reason}, Exception{kind} }` | same | enum read | ✅ struct variants, exact |
| 17 | `VerifierRef { identity: Option<EntityId>, endpoint: String }` | same | field read | ✅ |

## Ledger — the two load-bearing behavioural claims

These are the ones a reader would act on, so both were verified in the
implementation rather than from doc comments.

**18 — "`check_chain` freezes the chain at the first `Invalidated`; any event
after it is rejected. This is the reorg-freeze invariant, structurally
enforced."** ✅ **Confirmed.** `check_chain` carries an `invalidated` flag; once
set, the next iteration returns `EnvelopeError::Field("event {i} follows an
invalidation — serving against this quote is frozen")` *before* checking the
chain link. The freeze is a hard early return, not a warning, and the ordering
means a well-linked event after an invalidation still fails.

**19 — "A facilitator receipt is `observed`, full stop — `confirmed(n)` and
`final` come only from the independent on-chain `ChainChecker`, so the
facilitator is never in the trust root."** ✅ **Confirmed structurally**, which
is the strongest available form. `VerificationTier::Confirmed` and
`VerificationTier::Final` are constructed in exactly three places, all under
`checker/`: `eip155.rs:251,253`, `svm.rs:383,386`, `xrpl.rs:282`. Every
construction in `engine/` is `VerificationTier::Observed` (`mod.rs:605,1334,1348`,
all via `unwrap_or`). There is no path by which a facilitator response alone
produces a tier above `observed` — the claim is enforced by construction, not by
convention.

## Findings

**None.** No defect, no documentation gap. This is the first slice of the whole
audit to come back completely clean.

That is worth stating plainly rather than dressing up: `object-model.md` is 266
lines of dense field tables covering 58 fields across five envelopes, and every
one matches — names, types, optionality, constructor parameter order. Whoever
wrote it worked from the source rather than from memory.

## What this audit does not establish

- **Canonical-signing correctness.** The skill's claims about `terms_hash`
  coverage ("covers the version tag → no cross-version replay") and
  `check_integrity` recomputation were read as source but not exercised against
  a crafted cross-version replay. Pinning that behaviour is a candidate for
  Phase 5's `mutation` tier rather than a field-table audit.
- **Cross-language envelope agreement.** Whether the Python/Node
  representations carry the same fields is `bindings.md`'s territory — slice 2c.
- **`x402.md`'s byte-preservation claims.** Deferred to 2c; `X402Carry` appears
  here only as a field type.
