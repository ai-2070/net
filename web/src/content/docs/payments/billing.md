---
title: Billing
description: Billing in Net is a record of commercial facts, not an invoicing system.
---
# Billing

Billing in Net is a **record of commercial facts**, not an invoicing system. Each
redeemed payment emits an immutable `net.billing.event@1`; the stream of those
events is what a provider exports into whatever actually invoices, reconciles, or
reports. Net writes the facts; your systems do the accounting.

## Immutable events, one per redemption

When the engine serves a paid invocation, it emits a signed
`net.billing.event@1` carrying references and amounts — a `billing_event_id`, an
`idempotency_key`, the `capability`, the `quote_id`, a `verification_ref`, the
`payer` and `payee` (entity IDs), the `network` and `asset`, the `amount` (atomic
units), and a timestamp. It carries **no customer PII** — no names, addresses, or
account records. The event is append-only; it is never mutated after the fact.

## The billing stream

`BillingLog` is the surface:

- **`subscribe()`** — a live broadcast of billing events as the engine emits
  them;
- **`read_all()`** — the durable history;
- **`export_jsonl(dest)`** — copy the verified lines out to a destination for
  downstream systems.

The idempotency key makes the stream safe to consume more than once: the same
redemption never double-bills.

You attach the log when you build the engine, and it fans out from there:

```rust
use std::sync::Arc;
use net_payments::billing::BillingLog;

let billing = Arc::new(BillingLog::new(dir.join("billing.jsonl")));
let engine = PaymentEngine::new(/* … */)?.with_billing_log(billing.clone());
```

### Streaming events as they happen

```rust
let mut rx = billing.subscribe();
while let Ok(event) = rx.recv().await {
    println!("{} {} {}", event.billing_event_id, event.capability, event.amount);
}
```

### Reading the durable history

Every line is independently signed, so a consumer verifies rather than trusts
the file it read:

```rust
use net_payments::core::canonical::SignedEnvelope as _;

for event in billing.read_all().await? {
    event.verify_signature()?;
    println!("{} — {} for {}", event.billing_event_id, event.amount, event.capability);
}
```

`verify_signature()` is the point. The log is a file on disk; the signature is
what makes a billing event evidence rather than a line someone could have
appended. Export paths should verify too — `export_jsonl` copies verified lines.

The caller gets its own copy without reading the provider's log at all: the
`proof` on `CallerDecision::Paid` carries the signed event, so both sides
persist the same fact independently.

```rust
let signed = proof["billing_event"].as_str().unwrap_or_default();
let event = net_payments::BillingEvent::from_json_bytes(signed.as_bytes())?;
```

## What billing is NOT

- **Not an invoice.** No line items, tax, currency conversion, or customer
  balance. A `net.billing.event@1` is a *usage fact*; turning facts into an
  invoice is the provider's (or a partner's) job.
- **Not a ledger of custody.** Net didn't hold the money; the event references a
  settled on-chain transaction, it doesn't represent a balance Net keeps.
- **Not a customer record.** Identities are entity IDs; commercial identity, if
  needed, is an opaque reference resolved in provider systems — never a customer
  profile embedded in the event.

The lifecycle hooks doctrine: billing is the *last* step of a served invocation,
emitted from the same engine that verified and redeemed it — so a billing event
exists only for work that was actually paid for and served.
