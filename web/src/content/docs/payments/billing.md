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
