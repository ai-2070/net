# Paid A2A — charging for a bounded unit of agent work

A2A hands a long job to another agent. When that job is **paid**, the one-shot
handoff becomes **prepare → purchase → submit**, and the payments objects you
already know carry it: `PricingTerms` at discovery, one `PaymentQuote` per
purchase, one `PaymentEngine` behind both the payment wire and the admission
gate.

This file is the **money half**. The mesh half — the service catalog, the
admission journal's own states, `interrupted{detail}`, the Python caller
walkthrough and the runnable example — belongs to the `net-event-bus` skill's
A2A chapter; read it for the task lifecycle and do not duplicate it here.

Read `provider.md` and `caller.md` first. A paid task is the same lifecycle as a
paid tool with two additions: the quote is bound to **one exact reservation**
(`input_hash`), and **each side keeps a durable record** — the provider an
admission journal, the caller a purchase store.

**The ordering invariant:** everything that can refuse the work happens
**before a quote exists**. After payment there are no unpaid rejections, only
reconciliation.

## What makes a task capability paid

Pricing is announced the same way it is for a tool — `net.pricing.terms@1` as
capability metadata — but the capability id is the task service:

```rust
// "{node_id}/net.a2a.task/{service_id}" — A2A_TASK_SERVICE is "net.a2a.task"
let capability = format!("{provider_node}/{A2A_TASK_SERVICE}/{SERVICE}");
let terms = PricingTerms::new(provider.entity_id().clone(), &capability,
                              vec![template_carry], registry.reference()?);
```

The canonical JSON goes on the offer (`A2aOffer::pricing_terms: Option<String>`,
`sdk/src/a2a.rs`), and the catalog entry declares the intent:
`A2aServicePolicy::Free(offer)` or `A2aServicePolicy::Paid(offer)`
(`sdk/src/mesh_a2a.rs`). **The two must agree, and `serve_a2a_configured`
refuses to start if they do not** — the same fail-closed pair `serve_tool_paid`
uses (`provider.md`):

| Catalog entry | Offer has `pricing_terms` | Result |
|---|---|---|
| `Paid` | yes, plus a gate and a journal | serves, gated |
| `Paid` | no | `ServeError::MissingPricingTerms` |
| `Free` | yes | `ServeError::UnenforceablePricing` |
| `Paid` | no gate, or no journal | `ServeError::A2aPaidMisconfigured` |

Free is still not *unpoliced*: a free configured service runs the application
`TaskPreflight` and enforces capacity. It simply never mints a quote.

Inside the quote, the **tool id** is the capability's tail:
`net.a2a.task/{service_id}`. That is what `redeem_for_task` compares, so a quote
bought for one service can never admit work on another.

## The provider seam — one engine, two doors

```rust
let engine = Arc::new(PaymentEngine::new(keys, facilitator, admission, registry,
                                         dir.join("payment-engine.json"))?
    .with_billing_log(billing));

// door 1 — quote + pay (net.payments.quote.v1 / net.payments.pay.v1)
let _payments = serve_payments(&mesh,
    Arc::new(InProcessProvider::new(Arc::clone(&engine), clock.clone())))?;

// door 2 — redeem, at admission time
let _serving = mesh.serve_a2a_configured(
    task_registry,
    executor,
    A2aServiceConfig::new(services)
        .with_payment(Arc::new(EngineTaskAdmissionGate::new(Arc::clone(&engine))))
        .with_journal(A2aAdmissionJournal::open(dir.join("admissions.json")).await?),
)?;
```

**It must be the same engine — the same `Arc`, and therefore the same state
file.** The quote record the payment wrote is the record redemption reads. Two
engines over two paths means the caller pays one store and presents its proof to
another, which answers `unknown_quote` — a denial that *claims no funds moved*
and invites a second purchase, on a payment that actually settled.

`EngineTaskAdmissionGate` (`payments/src/flow/mesh.rs`) implements
`net_sdk::a2a_payment::TaskAdmissionGate`; it is the task twin of
`EngineToolPaymentGate` and shares its denial rendering. Its claim is
`TaskPaymentClaim { tool_id, quote_id, binding, expected_input_hash }`; on
success it returns `TaskPaymentEvidence`, and on refusal a `GateDenial` carrying
the `net.payment.failure@1` schematic (`failure-schematic.md`).

**The journal is required, and it is a second durable step.**
`redeem_for_task` marks the quote consumed in `payment-engine.json`; the
provider then writes `Paid` into the admission journal. A crash between those
two writes is exactly why task redemption is *idempotent per purchase hash*
rather than strictly at-most-once — see the next section. Opening an
`A2aAdmissionJournal` takes lifetime-exclusive ownership of the file; a second
owner is a serve-time error, not a second writer.

## `input_hash` — the quote prices one exact reservation

`PaymentQuote::input_hash` is no longer always `None`. On the task path the
provider's prepare mints an `admission_id` and an `AdmissionReservation` whose
`purchase_hash` (blake3 of `admission_id` + `commitment`) **is** the quote's
input hash.

It feeds `terms_hash`, and `terms_hash` feeds `quote_id`, so:

- two purchases of two different briefs can never share a quote;
- the caller's invocation-binding signature over `quote_id ‖ tool_id`
  transitively proves *which* purchase the payer authorized;
- a shape that is not one lowercase-hex blake3 digest is refused at issuance
  (`EngineError::MalformedInputHash`) — an empty or off-shape hash folds into
  `terms_hash` exactly as absence does, so accepting one would let a "bound"
  quote collide with the unbound capability-level quote.

Redemption is `PaymentEngine::redeem_for_task(tool_id, quote_id, binding,
expected_input_hash)` (`payments/src/engine/mod.rs`), and it differs from
`redeem_for_invocation` in three ways:

| | `redeem_for_invocation` (tools) | `redeem_for_task` (A2A) |
|---|---|---|
| Binding | `Option<&[u8]>` — absent degrades to bearer | **required** `&[u8]`; there was never a bearer mode |
| Input | not compared | `rec.input_hash` must equal `expected_input_hash`, or `RedeemDenialReason::InputBindingMismatch` — before the redeemed arm, with no durable write |
| Repeat | strictly at-most-once (`AlreadyRedeemed`) | idempotent **for that same purchase hash**; redeemed for anything else is `AlreadyRedeemed` |

`expected_input_hash` is the **provider's own** purchase hash, recomputed from
its reservation — never a value read off the request. So a proof replayed under
another owner, another reservation, or a foreign task computes a different hash
and dies before consuming the quote.

Idempotent redemption is safe because at-most-once *execution* is not owned
here: the journal's launch claim and ledger decide whether the task runs.
Redemption is idempotent only so the crash described above reconciles instead of
charging twice — and because that window outlives the quote record, every check
also runs against the retained redemption tombstone, in the same order, so a
compacted settled purchase re-admits rather than reading as `unknown_quote`.

## The caller seam — `A2aCallerFlow`

```rust
let flow = A2aCallerFlow::new(
    Arc::new(CallerPaymentFlow::new(
        Arc::clone(&caller_keys),
        SpendPolicyEngine::new(&dir.join("payment-policy.json"), SpendProfile::DevTest),
        registry.clone(),
        Arc::new(MeshPaymentChannel::new(Arc::clone(&mesh), Arc::clone(&caller_keys), clock.clone())),
        clock.clone(),
    )),
    Arc::new(MeshA2aChannel::new(Arc::clone(&mesh))),     // Arc<dyn A2aProviderChannel>
    Arc::new(A2aPurchaseStore::new(&dir.join("a2a-purchases.json"))),
    clock.clone(),
);

let prepared = flow.prepare_task(provider_node, &offer, &brief).await?;  // no money
let purchase = flow.purchase_task(provider_node, task_id).await;         // the one charge
let ack      = flow.submit_task(provider_node, task_id).await;           // no money
```

Four seams, and each is composed rather than re-implemented
(`payments/src/flow/a2a.rs`):

- **`CallerPaymentFlow`** supplies the staged payment verbs — `quote_bound`
  (the same `run` lifecycle, with the purchase hash passed as `input_hash`) and
  `pay_exact`. This flow never authors a payload or talks to a facilitator
  itself.
- **`MeshPaymentChannel`** is the money wire; **`MeshA2aChannel`** is the task
  wire (`Mesh::prepare_a2a` + `Mesh::submit_task_paid`). Two channels because
  they are two services — the quote is addressed directly to the node that will
  sign it.
- **`SpendPolicyEngine`** runs inside `purchase_task`, exactly as for a tool
  call. Approvals key on `quote_id`, and the held-quote lookup is
  `approved_quote_for_input(capability, input_hash)`, so a sibling purchase's
  approved hold is neither returned nor disturbed (`spend-policy.md`).
- **`A2aPurchaseStore`** is the durable attempt: `a2a-purchases.json`, the same
  locked-JSON + sidecar-lock idiom as the spend store, **one authoritative
  attempt per `(caller entity, provider node, task id)`**, every change a
  compare-and-set under the lock.

The store is what makes a lost reply survivable. The quote and the authored
payload are persisted **before** the pay call, so re-sending the *identical*
payload resolves to the provider's original verdict; re-quoting would buy the
work twice. `purchase_task` on an in-flight attempt re-sends; it does not
re-quote. A `Preparing` or `Paying` lease is honored for `PREPARE_LEASE_NS`
(two minutes) before another caller may take it over.

`prepare_task` against a free service is `A2aPrepareError::Unpriced` — there is
nothing to purchase, and minting an attempt with no quote would create a record
nothing can resolve. Submit it directly with `Mesh::submit_task`.

## The purchase state machine

`PurchaseState` serializes `#[serde(tag = "state", rename_all = "snake_case")]`,
so these tags are what an operator tool and the Python rows show.

| State | Where it stands | Money | Exit |
|---|---|---|---|
| `preparing` | CAS-inserted before any network call, under a lease | none | becomes `quoted`, or another caller takes the stale lease |
| `quoted` | reservation + provider-signed quote persisted | none — **nothing reserved on the spend side**; this is the state a price is displayed from | `purchase_task` |
| `awaiting_approval` | spend policy is holding **this exact quote** for an operator | none | `approve(quote_id)`, then `purchase_task` again |
| `paying` | payload authored and persisted; a pay request may be in flight | **exposed** — the authorizing bytes may have reached the provider | re-send the stored payload |
| `paid` | settled; carries the `TaskPaymentProof` and the signed billing event | charged once | `submit_task` |
| `unknown` | the pay reply was lost, or settlement is still pending | **may have moved** | `purchase_task` again — recovers *only* through the stored payment; or an operator's `resolve_attempt` |
| `refused_unexposed` | refused before any authorization left this process | **proven none moved** | re-`prepare_task` (a new quote, so policy and approval run again) |
| `refused_exposed` | provider refused *after* a bearer authorization was exposed | ambiguous; the spend reservation is **kept** | operator `resolve_attempt` only — never re-quoted |
| `submitted` | accepted by the provider; the task is running | charged once | the task's own lifecycle (status / cancel) |
| `paid_unexecutable` | paid, and the provider will not execute it | charged, unexecuted; proof + billing retained beside the refusal | operator `resolve_attempt` |
| `resolved` | an operator closed it — refunded, written off, executed elsewhere | whatever they established, with their evidence | terminal |

Only the table's own pairs are legal: `A2aPurchaseStore::transition` refuses
anything else as `PurchaseError::NotATransition`, so a new path through the flow
cannot invent a state change. `PurchaseError::Superseded` is a **retryable write
refusal**, never a claim about money: a decision taken before an await must
re-present the record incarnation *and* the quote id it was decided against.
When such a refused write carried a real financial result, it is retained beside
the live attempt as a superseded incarnation and closes through
`resolve_superseded_attempt`.

### The three distinctions people get wrong

`purchase_task` answers `A2aPurchase`, and these four arms are not
interchangeable:

- **`Unknown { quote_id }` is not a failure.** It is a lost reply. Call
  `purchase_task` again: it re-sends the stored payload, which the engine
  answers idempotently. Treating it as a refusal and re-preparing is the bug the
  store exists to prevent.
- **`Denied { funds_ambiguous: false }`** is proven non-payment — refused before
  anything left the process. The key may be prepared again.
- **`Denied { funds_ambiguous: true }` is not proven non-payment.** Every real
  scheme authors a self-contained bearer pull authorization the counterparty
  could settle regardless of what it reports back, so an *exposed* refusal keeps
  the spend reservation and needs an operator. The per-scheme judgement is
  `CallerPaymentFlow::pay_exact`'s; this flow records what it did by reading the
  reservation back.
- **`PaidUnexecutable` is not a refund and not a refusal.** `submit_task`
  answering `A2aSubmit::Unexecutable { refusal }` means the payment stands and
  the provider will never run the work. The proof, the billing event and the
  provider's `RefusalRecord` stay on the attempt; `resolve_attempt` is the exit.
  `A2aSubmit::Retry` is the opposite case — resubmit the **same** proof, never
  re-purchase.

## The operator queue accumulates by design

Both sides keep a queue of things money touched and automation cannot close.
Neither drains itself:

| Side | Read | Close |
|---|---|---|
| Caller (live) | `A2aCallerFlow::attempts()` | `resolve_attempt(provider_node, task_id, AttemptResolution::{Paid, NotPaid, Closed})` |
| Caller (superseded) | `retained_attempts()`, `superseded_attempt(node, task_id, &generation)` | `resolve_superseded_attempt(.., &generation, resolution)` |
| Provider | the admission journal's unresolved records (Python: `PaymentProvider.a2a_unresolved()`) | `PaymentProvider.a2a_resolve(..)` — the journal side belongs to the `net-event-bus` skill |

`AttemptResolution::Paid` moves `unknown → paid` with the evidence the operator
established it from; `NotPaid` moves `unknown → refused_unexposed` and re-opens
the key for a fresh prepare; `Closed` records an outcome plus evidence from
`unknown`, `refused_exposed` or `paid_unexecutable`.

`A2aPurchaseStore::prune(now_ns, retention_ns)` exists, **no production path
calls it**, and it keeps every unresolved-financial row regardless of age —
`paying`, `paid`, `unknown`, `refused_exposed`, `paid_unexecutable`. A growing
queue is the system working: it is a list of charges a human has not accounted
for. If a settlement lands after an operator already resolved a superseded row,
the disposition stands and the proof is retained beside it under the
`net.payments.a2a.late_settlement@1` envelope, so the charge stays findable.

The provider journal prunes on its own retention clock, and it makes the same
exception: an unresolved admission is kept whatever its age. Both queues are
lists of charges a human has not accounted for, and growth is the design, not a
leak.

## What exists where

| Surface | Rust | Python | Node | Go / C |
|---|---|---|---|---|
| Serve a paid catalog | `Mesh::serve_a2a_configured` | `PaymentProvider.serve_a2a_configured` | — | — |
| Buy a task (prepare / purchase / submit) | `A2aCallerFlow` | `CapabilityGateway.prepare_task` / `purchase_task` / `submit_task` | — | — |
| Operator queue (caller) | `attempts` / `resolve_attempt` (+ superseded twins) | `a2a_attempts()` / `a2a_resolve_attempt()` | — | — |
| Operator queue (provider) | `AdmissionStore::unresolved` / `resolve` on the journal | `a2a_unresolved()` / `a2a_resolve()` | — | — |
| Free A2A requester | `Mesh::submit_task` | `mesh.submit_task` | `submitTask` | — |

**Node, Go and C have no paid A2A in either direction** — not as a provider and
not as a buyer. Node's `submitTask` is the free requester verb and has no paid
twin; Go and C have no A2A at all. State both halves when you answer this: "paid
serving is Rust/Python only" without saying that purchasing is equally bound has
been misread more than once.

Everything above is exercised against the **mock facilitator**. The lifecycle is
real; no claim is made here about a real settlement rail
(`facilitator.md`, `networks.md`).

## Further reading

- [Agent-to-Agent Task Handoff](https://ai2070.net/docs/guides/agent-to-agent)
- [The Lifecycle](https://ai2070.net/docs/payments/the-lifecycle)
