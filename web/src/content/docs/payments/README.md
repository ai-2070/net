---
title: Payments
description: Net Payments is how a capability charges for its work and a caller pays to invoke it — without Net ever touching the money.
---
# Payments

Net Payments attaches pricing, payment evidence, verification, spend policy, and
billing events to capability invocation. The payment rail moves the funds; Net
does not hold them.

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
determine taxes, or clear transactions. Those responsibilities remain with the
payment rail and the participating businesses. See
[What Net Payments is](/docs/payments/what-net-payments-is).

**You don't need an HTTP server.** Net-native paid capabilities are announced and
invoked over the mesh (nRPC); the x402 payment material rides as opaque preserved
bytes inside the invocation. HTTP 402 is an adapter path for web APIs, not a
requirement for Net providers ([x402 and Net](/docs/payments/x402-and-net)).

## Run it first

If you'd rather see the whole loop before reading about it, the compiled
[`examples/docs_payments.rs`](https://github.com/ai-2070/net/blob/master/net/crates/net/payments/examples/docs_payments.rs)
drives announced terms → provider-signed quote → caller spend policy → x402
payload → verify + settle → billing, in one process against the mock
facilitator:

```sh
cargo run -p net-payments --example docs_payments
```

```
paid; redemption binding = 031cc917c742f74e…
billed 2500 for docs-provider/summarize
billing event df9efd87267bfd6d…  — 2500
```

Every Rust snippet in this section comes from that file, so CI compiles the
docs along with the crate.

## Start here

- [What Net Payments is (and is not)](/docs/payments/what-net-payments-is)
- [x402 and Net](/docs/payments/x402-and-net) — the payment wire, and what Net wraps around it
- [The lifecycle](/docs/payments/the-lifecycle) — quote → verify → settle → serve → bill
- [Verification tiers](/docs/payments/verification-tiers) — `observed | confirmed(n) | final`
- [Spend policy & approvals](/docs/payments/spend-policy-and-approvals)
- [Non-custodial signing](/docs/payments/non-custodial-signing)
- [Networks](/docs/payments/networks) — config, not code
- [The failure schematic](/docs/payments/failure-schematic) — machine-actionable denials
- [Billing](/docs/payments/billing)

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

The [lifecycle](/docs/payments/the-lifecycle) walks these in order.
