# Payments

Net Payments is how a capability charges for its work and a caller pays to invoke
it — without Net ever touching the money.

> **Net standardizes the commercial facts around capability invocation; it does
> not intermediate the money.** x402 moves the funds; Net signs the facts around
> them — provider identity, discovery-time pricing, tiered verification,
> immutable billing, and spend policy.

**"Commercial facts" is a bounded term.** They are references, commitments,
signatures, quotes, verification results, policy decisions, and billing events —
**not** customer PII, tax records, KYB files, invoices, shipping data, or
provider account records. If a provider needs commercial identity, Net carries an
opaque reference plus a commitment, never the record itself.

Net Payments does **not** custody funds, process payments, issue invoices,
determine taxes, or clear transactions. If you're looking for a payment
processor, this isn't one — and [that's the point](./what-net-payments-is).

**You don't need an HTTP server.** Net-native paid capabilities are announced and
invoked over the mesh (nRPC); the x402 payment material rides as opaque preserved
bytes inside the invocation. HTTP 402 is an adapter path for web APIs, not a
requirement for Net providers ([x402 and Net](./x402-and-net)).

## Start here

- [What Net Payments is (and is not)](./what-net-payments-is)
- [x402 and Net](./x402-and-net) — the payment wire, and what Net wraps around it
- [The lifecycle](./the-lifecycle) — quote → verify → settle → serve → bill
- [Verification tiers](./verification-tiers) — `observed | confirmed(n) | final`
- [Spend policy & approvals](./spend-policy-and-approvals)
- [Non-custodial signing](./non-custodial-signing)
- [Networks](./networks) — config, not code
- [The failure schematic](./failure-schematic) — machine-actionable denials
- [Billing](./billing)

## The object model at a glance

Five signed Net envelopes wrap the x402 payment; each has exactly one canonical
byte encoding, and each carries references and commitments — never customer data:

| Envelope | What it commits |
|---|---|
| `net.pricing.terms@1` | what a capability costs, announced at discovery |
| `net.payment.quote@1` | a signed, expiring quote binding a caller to terms |
| `net.settlement.ref@1` | a reference to the settled x402 transaction |
| `net.payment.verification@1` | a tiered verification result (see below) |
| `net.billing.event@1` | an immutable usage record |

The [lifecycle](./the-lifecycle) walks these in order.
