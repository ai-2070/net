# The lifecycle

One `PaymentEngine` runs the provider side of a paid capability; one
`CallerPaymentFlow` runs the caller side. They meet at the quote. Nothing here is
decided in a language binding — the bindings marshal arguments and project
results; the lifecycle lives in the `net-payments` core.

## Provider side: quote → verify → settle → serve → bill

1. **Price at discovery.** The provider authors `net.pricing.terms@1` for a
   capability and announces it. Displaying a price never implies authorization to
   spend it.
2. **Quote.** On request, the engine issues a signed, expiring
   `net.payment.quote@1` bound to the caller and the invocation's input hash.
3. **Verify.** When the caller presents proof of an x402 payment, the engine
   verifies it — at a [tier](./verification-tiers), never as a bare boolean.
4. **Settle.** Settlement happens on-chain via the facilitator; Net records a
   `net.settlement.ref@1` pointing at the transaction.
5. **Serve.** The capability handler runs **only after** the quote is redeemed,
   at-most-once, against the same engine. A paid capability with no payment
   configured **fails closed** — the handler never sees an unpaid call.
6. **Bill.** The engine emits an immutable `net.billing.event@1`.

The gate is the seam: the SDK exposes `ToolPaymentGate` (native) and the MCP
adapter exposes `PaymentAdmission`; `net-payments` implements both over the one
engine, so a quote paid over the wire is the quote the gate redeems.

## Caller side: pricing → spend policy → pay → invoke

1. **Read the price** from discovery (`describe` surfaces `pricing_terms`; `null`
   = free).
2. **Spend policy runs first.** Before anything leaves, the [spend
   policy](./spend-policy-and-approvals) either clears the spend, asks for a
   human approval, or denies. The model does not decide.
3. **Pay.** On clearance, the caller settles the x402 payment (signing only a
   typed intent — see [Non-custodial signing](./non-custodial-signing)) and
   attaches the proof to the invocation.
4. **Invoke.** The call carries the quote; the provider's gate redeems it and
   serves.

If the provider refuses, the denial can carry a machine-actionable [failure
schematic](./failure-schematic) beside the human error, so the caller's agent
can branch on *why* and *what's safe to do next* rather than parse prose.

## One engine, one source of truth

The same `PaymentEngine` serves the quote/pay wire **and** gates the priced
tools. That's the invariant that makes the lifecycle honest: there is no second
place a payment could be "counted" — settled, verified, billed, and redeemed all
run against one store under its lock, at-most-once.
