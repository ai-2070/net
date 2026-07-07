# Billing — the signed usage record + lifecycle hooks

## What a billing event is (say it right)

> A billing event is a **signed technical record linking invocation, quote,
> settlement verification, and amount — input to accounting systems, never an
> accounting artifact itself.**

This sentence is verbatim in the source and API docs, by doctrine. A billing
event is **not** an invoice, receipt, statement, or anything with legal
sufficiency. Net emits the event; the customer's accounting system renders
whatever their posture requires. Naming stays `net.billing.*` — never
`net.invoice.*` / `net.tax.*` / `net.receipt.*`. See `object-model.md` §5 for
the `BillingEvent` fields.

## The billing stream + export (`billing/mod.rs`)

`BillingLog` is an append-only JSONL file plus an in-process broadcast stream.

```rust
use net_payments::billing::BillingLog;
use std::sync::Arc;

let log = Arc::new(BillingLog::new(billing_path));
let engine = PaymentEngine::new(..)?.with_billing_log(log.clone());  // engine appends every fresh event

// subscribe (in-process fan-out):
let mut rx = log.subscribe();                       // tokio::sync::broadcast::Receiver<BillingEvent>
while let Ok(event) = rx.recv().await { /* project it */ }

// read / export:
let all: Vec<BillingEvent> = log.read_all().await?; // verifies every record's signature + id derivation
let n = log.export_jsonl(&dest).await?;             // -> usize records written
```

`BillingError`: `Io { path, reason }`, `BadRecord { path, line, reason }`.
`append` is locked + fsync'd, then publishes to subscribers. **Idempotent
retries republish nothing** — one event per idempotency key (the engine only
appends a *freshly-emitted* billing event). `read_all` re-verifies every record
as it loads, so a corrupted or forged line fails loudly rather than flowing
into accounting.

Billing surfaces exist in the SDKs that have payments: **Rust and Python** in
P0/P1; other languages are verifier-level only (`bindings.md`).

## Immutability

Billing events are never rewritten. A reorg, adjustment, refund, or dispute
emits a *new* event that **references** the original `billing_event_id`.
Event-sourced all the way down — the audit trail is the append-only log, and
`verification_ref` on a billing event points into the verification chain it was
emitted under. There are no auto-refunds in v1.

## Lifecycle hooks — the enterprise surface (Net ships zero dashboards)

Positioning line, verbatim for docs: *every lifecycle moment is a hook; render
it wherever you already look.* This is the doctrine for the hook layer (built
out across stages; the primitives below are the contract).

- **Every immutable event kind is a hook point, and nothing else is:** quote
  `issued/accepted/expired/declined`, payload received, settled, verification
  tier reached, `invalidated{reorg}`, billing event emitted, batch netted,
  spend cap hit, approval raised/resolved, cancellation. **No hook-only
  events** — a hook fires because a signed event exists in the log. Missed
  delivery is recoverable by replaying the stream; hooks are a *projection* of
  the log, never a second truth.
- **Payloads are the signed events themselves, byte-preserved** — consumers
  verify the signature and hold a protocol fact, not a notification rendering.
- **Delivery is an in-process adapter interface only.** Net calls registered
  adapters with signed lifecycle events from the local log. Built-in adapters
  are limited to test/dev examples; production sinks are customer/partner code.
  Net is not responsible for delivery semantics, retention, or downstream
  integrations.
- **Adapter invocation contract** (pinned, not discovered in an incident):
  every adapter call carries the signed event byte-preserved + `event_id` +
  event-log sequence (the dedupe key), `hook_point`, and `subscription_id`.
  Everything downstream — delivery ids, attempts, acks, retries, dead-letters —
  is the adapter author's domain. At-least-once fan-out means **duplicates will
  happen; sinks dedupe on event identity, never delivery identity.**
- **Hooks never block the payment path.** Adapter dispatch is asynchronous off
  the local log; a slow/failing adapter stalls its own projection, never
  settlement, serving, or the log. Net invokes adapters in log order **per key
  (per quote/settlement chain), not globally** — consumers must not infer
  causality from arrival order across keys; the event-log sequence is the
  causal truth. There is no Net-side durable delivery queue and no dead-letter;
  an adapter that fell behind catches up by replaying from the stream.
- **Subscriptions are versioned config objects** (`net.hook.subscription@1`):
  predicates (capability, counterparty, amount threshold, event kind), accepted
  event versions, and the registered adapter id. A subscription names an
  adapter, **never a sink or a credential.** Auditable, exportable, diffable.
- **Adapter credentials follow the forwarding doctrine:** Net-side objects and
  logs carry no sink credentials. Whatever a production adapter needs (webhook
  auth, Kafka SASL) is the customer's secret handling, outside Net —
  `secret_ref` names in adapter config, never credential material in
  subscription objects, logs, or agent-visible surfaces.

## What billing is NOT (the guardrail)

No dashboards, reports, digests, or report generators of any kind — **zero,
not merely not-source-of-truth.** No invoice/receipt/statement generation, no
tax/VAT logic, no ERP connectors. Net emits signed billing events; partners
and customers turn them into invoices, accounting records, and dashboards
under their own policy and posture. If a request asks Net to *render* or
*store* an accounting artifact, it's out of scope (`gotchas.md`).
