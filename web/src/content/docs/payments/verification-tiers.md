# Verification tiers

"Did the payment go through?" is not a yes/no question on a blockchain — a
transaction can be seen, then confirmed to some depth, then (rarely) reorged out.
Collapsing that to a boolean is how systems serve work against money that later
disappears. Net makes the confidence level a **first-class tier**.

## The three tiers

```
observed | confirmed(n) | final
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

## Where it shows up

Each check is recorded as a signed `net.payment.verification@1` envelope carrying
the tier, the status, the verifier reference, and a link to the prior check —
an append-only chain of confidence over the life of a payment, not a single
mutable flag.
