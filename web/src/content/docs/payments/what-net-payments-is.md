# What Net Payments is (and is not)

An agent that invokes a capability needs to answer commercial questions the
transport layer can't: *what does this cost? did I actually pay for what I got?
am I allowed to spend this? what do I bill?* Net Payments answers those by
**signing the commercial facts** around an invocation. It does not move money.

## The category line

> **Net standardizes the commercial facts around capability invocation; it does
> not intermediate the money.**

x402 (the payment wire) moves the funds on-chain. Net signs the facts around
them: who the provider is, what a capability costs, whether a payment verified
and to what depth, what spend policy allowed, and what was billed.

## What it is NOT

This is the load-bearing half of the positioning. Net Payments does **not**:

- **custody funds** — no wallet, no balance, no escrow;
- **process or clear payments** — x402 + the chain do that;
- **issue invoices**, determine **taxes**, or run **KYB / sanctions / identity**
  checks — providers own those in their own systems;
- **carry customer PII** — see the data boundary below.

If a page (or your mental model) has Net holding a balance or running a
compliance check, it's wrong.

## The doctrines that shape everything

1. **x402 is the wire; Net signs around it.** Net envelopes *wrap* x402
   structures — they never replace, translate, or re-encode them.
2. **Byte-preservation is law.** x402 documents ride as base64 of their original
   bytes; Net never re-serializes a received x402 doc. See [x402 and
   Net](./x402-and-net).
3. **Non-custodial by construction.** Identity keys are not settlement keys, and
   there is no raw-bytes signing path. See [Non-custodial
   signing](./non-custodial-signing).
4. **Verification is a tier, not a boolean.** A facilitator receipt is
   `observed`; depth and finality come from an independent on-chain check. See
   [Verification tiers](./verification-tiers).
5. **The policy engine decides, not the model.** Spend policy runs before money
   leaves; handlers never see an unpaid call. See [Spend policy &
   approvals](./spend-policy-and-approvals).
6. **Enabling a network is config, not code.** See [Networks](./networks).

## The data boundary

Net payment, billing, lifecycle, and failure objects carry references,
commitments, signatures, quote IDs, verification outcomes, and policy decisions
— **not** customer tax IDs, billing addresses, shipping addresses, or KYB
records. This holds *by construction*: identities on the wire are public keys
(entity IDs), the invocation input is carried as a **hash**, amounts are opaque
atomic-unit integers, and there is no PII field on any envelope. Provider and
customer records live in provider or partner systems.

**Terms acceptance**, where used, means a **signed acceptance commitment plus a
terms hash/ID** — Net does not host terms text, validate legal authority, store
customer identity, or adjudicate enforceability.

## What's reserved (not shipped)

Named here so you don't build on them: **disputes / refunds**
(`net.payment.dispute@1` is a reserved tag with no semantics), **RFQ / dynamic
pricing**, **accounts / postpaid (Mode E)**, and **inbound HTTP-402 serving**
(only the outbound client ships). These are roadmap, not behavior.
