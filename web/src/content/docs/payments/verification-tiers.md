# Verification tiers

"Did the payment go through?" is not a yes/no question on a blockchain — a
transaction can be seen, then confirmed to some depth, then (rarely) reorged out.
Collapsing that to a boolean is how systems serve work against money that later
disappears. Net makes the confidence level a **first-class tier**.

## The three tiers

```rust
pub enum VerificationTier {
    Observed,       // a facilitator saw it; no depth claim
    Confirmed(u32), // n confirmations, established independently
    Final,          // independently checked on-chain finality
}
```

- **`observed`** — a facilitator (or adapter) *saw* the transaction. **No depth
  claim.** A facilitator receipt is `observed`, full stop.
- **`confirmed(n)`** — `n` confirmations / equivalent chain-native depth,
  established by an **independent on-chain check** — not by trusting the
  facilitator's word.
- **`final`** — independently checked on-chain finality (deterministic where the
  chain provides it; a confirmation-depth threshold otherwise).

A provider states the tier it requires before it serves. Receipt-trust
(`observed`) is fine for low-value or mock flows; higher-value work waits for
`confirmed(n)` or `final`.

## The facilitator is not in the trust root

This is the rule that makes the tiers mean something:

> A facilitator's receipt only ever yields **`observed`**. `confirmed(n)` and
> `final` come **only** from the independent `ChainChecker` — the code that
> queries the chain itself.

So a compromised or optimistic facilitator cannot manufacture finality. The
worst it can do is claim `observed`; the checker is what promotes a payment past
that, and the checker answers to the chain, not the facilitator.

## Reorgs are a first-class outcome

If a previously-confirmed settlement is reorged out, that is not an error to
swallow — it is a **verdict**. The checker surfaces a reverted / invalidated
result in the same family as a reorg, and the engine **freezes** the affected
quote rather than pretending the payment stands. A frozen quote does not serve.

## Requiring a tier

The provider names its bar when it accepts the payment:

```rust
use net_payments::VerificationTier;

let decision = engine
    .accept_payment(&quote, &payload, VerificationTier::Observed, now_ns)
    .await?;
```

The decision is not a boolean either. `PaymentDecision::Served` means the tier
was met and the handler may run; `PendingTier { reached, required }` means the
money settled but confidence hasn't caught up, so the handler does **not** run
and you re-verify later; `Invalidated` means a previously-verified payment was
withdrawn and the quote is frozen. `FacilitatorFailure` is the fail-closed
default — nothing was consumed, and policy chooses retry or fallback.

## Raising the bar after the fact

Re-verification is where `confirmed(n)` and `final` actually come from, and it
takes the `ChainChecker` explicitly — the type signature is the trust boundary:

```rust
let decision = engine
    .re_verify_with_checker(
        &quote_id,
        checker,                          // &dyn ChainChecker — talks to the chain
        VerificationTier::Confirmed(12),
        now_ns,
    )
    .await?;
```

There is no overload that promotes a payment past `observed` using the
facilitator's word. If you want depth, you pass something that queries the
chain; `net_payments::checker` ships EIP-155, SVM, and XRPL implementations.

## Where it shows up

Each check is recorded as a signed `net.payment.verification@1` envelope carrying
the tier, the status, the verifier reference, and a link to the prior check —
an append-only chain of confidence over the life of a payment, not a single
mutable flag.
