---
title: Paid Agent Work
description: "Charging for a bounded job another agent runs, not a single invocation: admission is claimed before money exists, and the purchase is durable enough to survive a crash between paying and launching."
---

# Paid agent work

Every other page in this section prices **one invocation**: the caller pays, the
provider serves, the call returns with the answer. Net Payments also prices **a
bounded job** — work handed to another agent over native
[A2A](/docs/guides/agent-to-agent), which outlives the call that started it.

That difference is not cosmetic. A single round trip can carry a quote, a
payment and a result; a task cannot, so the sale has to be settled against
something other than "the call succeeded."

| You are selling                                                  | Use                                                                          |
| ---------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| a capability whose answer comes back inside the call             | the ordinary paid invocation — [the lifecycle](/docs/payments/the-lifecycle) |
| a job that runs for minutes while the caller does something else | **paid A2A** — this page                                                     |

The mesh-side mechanics of the handoff — briefs, bounds, status, cancellation,
what a restart surfaces — belong to the
[agent-to-agent guide](/docs/guides/agent-to-agent). This page is the commercial
half: where money enters, what it binds to, and who owns the disagreement when
the two sides cannot agree that it moved.

## Free or paid is the provider's configuration

A caller never selects it. `Mesh::serve_a2a_configured` takes a catalog in which
every entry is explicitly `A2aServicePolicy::Free(offer)` or `Paid(offer)`, and
every way a paid service could degrade to a free one is a **refusal to start**,
not a warning: a `Paid` entry with no `net.pricing.terms@1` on its offer is
`ServeError::MissingPricingTerms`; a `Paid` entry with no `TaskAdmissionGate` or
no admission journal is `ServeError::A2aPaidMisconfigured`; and terms announced
on a `Free` entry — a price with no gate behind it — is
`ServeError::UnenforceablePricing`.

Free means no _payment_, never no _policy_. A free entry still runs the
application's `TaskPreflight` and still admits against capacity, so the
commercial decision is the only thing the catalog switch changes.

## Admission is claimed before money exists

One ordering rule carries the design:

> Everything that can refuse the work happens **before a quote exists**.

`prepare` validates the brief, runs the provider's preflight, reserves capacity,
and mints an `admission_id` — and charges nothing. It is read-only on the money
side, which is what makes it safe to call merely to display a price. Only then
does `purchase` quote against _that_ reservation and pay once, and `submit`
carry the evidence.

Commercially, the ordering is the product. Every "no" a provider has — unknown
service, stale revision, brief outside the bounds, at capacity, not authorized —
lands where nothing has been spent. The refusals still possible after payment
are not rejections at all; they are reconciliation, and they are handled as such
below.

## The quote binds to the reservation, not the service

An invocation's quote binds to a capability and, optionally, a hash of its
input. A task's quote binds to **one reservation of exactly one brief**:

```text
purchase_hash = H(admission_id, task_commitment(offer, brief))
```

That value rides the quote as its `input_hash`, and `input_hash` feeds
`terms_hash` and therefore the quote id — so two purchases of two different
briefs can never share a quote. `PaymentEngine::redeem_for_task` recomputes what
it expects from the provider's **own** record of the reservation the submission
arrived against, never from a value read off the request, and refuses anything
else. A settled, unredeemed, perfectly valid payment for one reservation is
worth nothing against another reservation, another brief, or another owner.

The possession proof is **mandatory** here. A paid tool invocation can be
configured to admit on the quote id alone; a task never can — holding an id is
not evidence that the payer authorized this long-running side effect — so a
bearer submit is refused `binding_required` before the gate is consulted at all.

The quote names the capability `{node_id}/net.a2a.task/{service_id}`, so a sold
task emits the same immutable `net.billing.event@1` an invocation does
([billing](/docs/payments/billing)), under a capability id that says which
service was sold.

## The purchase is durable, so a lost reply is not a lost payment

The caller side is three verbs over one durable record:

```rust
use net_payments::flow::a2a::{
    A2aCallerFlow, A2aPurchase, A2aPurchaseStore, MeshA2aChannel,
};

let flow = A2aCallerFlow::new(
    payments,                                                   // the CallerPaymentFlow
    Arc::new(MeshA2aChannel::new(Arc::clone(&mesh))),
    Arc::new(A2aPurchaseStore::new(dir.join("a2a-purchases.json"))),
    clock,
);

let prepared = flow.prepare_task(provider_node, &offer, &brief).await?;  // no money
match flow.purchase_task(provider_node, &brief.task_id).await {          // the one charge
    A2aPurchase::Paid { .. } => {}
    A2aPurchase::Unknown { .. } => { /* call purchase_task again — see below */ }
    A2aPurchase::RequiresPaymentApproval { quote_id, .. } => { /* an operator decides */ }
    A2aPurchase::Denied { funds_ambiguous, .. } => { /* see below */ }
    A2aPurchase::Failed { retryable, .. } => {}
}
flow.submit_task(provider_node, &brief.task_id).await;
```

Each verb reads and writes one `PurchaseAttempt`, keyed by
`(caller entity, provider node, task id)` in `a2a-purchases.json` — the same
locked compare-and-set store idiom as the spend policy file beside it. The quote
_and_ the authored payment payload are persisted **before** the pay call, which
is what makes the rest possible:

- **A lost pay reply is `Unknown`, not `Denied`.** Calling `purchase_task` again
  re-sends the _identical_ stored payload, and redemption on the task path is
  idempotent per purchase, so it resolves to the original verdict. Re-quoting
  would buy the work twice, so nothing in this flow re-quotes an attempt that
  may have paid.
- **A crash between paying and launching resumes.** `submit_task` re-sends the
  stored proof byte-for-byte, and a retry of an identical submission returns the
  original task rather than starting a second run.
- **Concurrent callers converge.** Two callers in one process, two processes on
  one machine, or one process either side of a crash all resolve to the same
  attempt — the same reservation, the same quote, the same payload — rather than
  minting a second purchase of the same work.

Re-preparing is legitimate only from a state where nothing was exposed: a quote
that expired unpaid, or a refusal that provably preceded any authorization. It
mints a new quote, so
[spend policy](/docs/payments/spend-policy-and-approvals) runs again and any
operator approval held against the old quote id is cleared rather than carried
over — approving quote _X_ never authorizes quote _Y_ here either.

## After payment, a refusal is reconciliation

A provider can still decline to execute work that was paid for: its reservation
aged out, or its preflight revoked authority between the prepare and the submit.
Neither side is permitted to call that "refused" and move on.

| Outcome                                          | Where the evidence lives                                                                                                     | Exit                               |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------- | ---------------------------------- |
| paid, and the provider will not execute it       | the caller's attempt is `PaidUnexecutable`, proof and billing event retained beside the refusal                              | operator `resolve_attempt`         |
| admission revoked after payment                  | the provider's admission record is `Reconcile`                                                                               | operator `AdmissionStore::resolve` |
| refused after a bearer authorization was exposed | the attempt is `RefusedExposed`; `A2aPurchase::Denied` renders `funds_ambiguous: true` and the spend reservation is **kept** | operator `resolve_attempt`         |
| refused before anything left the process         | `RefusedUnexposed`; `funds_ambiguous: false` — no money moved                                                                | prepare again                      |

`funds_ambiguous` is what keeps the accounting honest. Every real scheme authors
a self-contained bearer pull authorization the counterparty could settle
regardless of what it reports back, so a refusal that arrived _after_ those
bytes left is not proof of non-settlement — and this flow refuses to relabel it
as one.

Both sides therefore keep an operator queue, and **neither drains itself**:
`A2aCallerFlow::attempts` and `retained_attempts` on the caller,
`AdmissionStore::unresolved` on the provider. Records in the
unresolved-financial class are never pruned automatically — money moved, or may
have — and an operator resolving them is the only exit. Wire both into whatever
you already page on.

## Bindings

Availability is symmetric: where a language has no paid A2A, it has neither
half — it can neither sell agent work nor buy it.

| Binding           | Selling paid work                      | Buying paid work                                                   |
| ----------------- | -------------------------------------- | ------------------------------------------------------------------ |
| Rust              | `Mesh::serve_a2a_configured`           | `A2aCallerFlow::prepare_task` / `purchase_task` / `submit_task`    |
| Python            | `PaymentProvider.serve_a2a_configured` | `CapabilityGateway.prepare_task` / `purchase_task` / `submit_task` |
| Node / TypeScript | **none**                               | **none**                                                           |
| Go                | **none**                               | **none**                                                           |
| C                 | **none**                               | **none**                                                           |

Node's `submitTask` is the **free** A2A verb and is unchanged by any of this:
there is no `serveA2aConfigured`, no prepare/purchase pair, and no paid submit.
Go and C have no A2A surface at all, paid or free.

In Python the caller verbs live on the synchronous `CapabilityGateway`,
constructed with `a2a_purchase_path` (the durable purchase store) beside
`payment_policy_path`; `AsyncCapabilityGateway` does not carry them. A refused
paid submission raises `PaymentRefused`, which exposes the
[failure schematic](/docs/payments/failure-schematic) rather than a message to
parse. The operator queues are `CapabilityGateway.a2a_attempts` /
`a2a_resolve_attempt` and `PaymentProvider.a2a_unresolved` / `a2a_resolve`.

## Settlement: mock only, so far

Everything on this page has been exercised end to end against `MockFacilitator`
and `default_mock_registry` — the same conformance backbone the rest of this
section runs on. **No paid-A2A run against a real settlement rail is claimed
here.** Enabling a real network is the per-deployment ladder in
[Networks](/docs/payments/networks) — `allowed_networks`, a facilitator config
pack, an [external signer](/docs/payments/non-custodial-signing), and a chain
checker to serve above `observed` — and that gate is unqualified independently
of this path.

## See also

- [Agent-to-Agent Task Handoff](/docs/guides/agent-to-agent) — the brief, the bounds, status, cancellation, and what a restart surfaces
- [The lifecycle](/docs/payments/the-lifecycle) — the one-invocation loop this path is built on
- [Spend policy & approvals](/docs/payments/spend-policy-and-approvals) — the caller-side decision that runs before the one charge
- [The failure schematic](/docs/payments/failure-schematic) — the machine-actionable half of every refusal above
