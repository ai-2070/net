---
title: The Lifecycle
description: One PaymentEngine runs the provider side of a paid capability; one CallerPaymentFlow runs the caller side.
---

# The lifecycle

One `PaymentEngine` runs the provider side of a paid capability; one
`CallerPaymentFlow` runs the caller side. They meet at the quote. Nothing here is
decided in a language binding — the bindings marshal arguments and project
results; the lifecycle lives in the `net-payments` core.

The snippets on this page are compiled and run in CI as
[`examples/docs_payments.rs`](https://github.com/ai-2070/net/blob/master/net/crates/net/payments/examples/docs_payments.rs),
which drives the whole loop in one process against the mock facilitator.

## Provider side: quote → verify → settle → serve → bill

1. **Price at discovery.** The provider authors `net.pricing.terms@1` for a
   capability and announces it. Displaying a price never implies authorization to
   spend it.
2. **Quote.** On request, the engine issues a signed, expiring
   `net.payment.quote@1` bound to the caller and the invocation's input hash.
3. **Verify.** When the caller presents proof of an x402 payment, the engine
   verifies it — at a [tier](/docs/payments/verification-tiers), never as a bare boolean.
4. **Settle.** Settlement happens on-chain via the facilitator; Net records a
   `net.settlement.ref@1` pointing at the transaction.
5. **Serve.** The capability handler runs **only after** the quote is redeemed,
   at-most-once, against the same engine. A paid capability with no payment
   configured **fails closed** — the handler never sees an unpaid call.
6. **Bill.** The engine emits an immutable `net.billing.event@1`.

The gate is the seam: the SDK exposes `ToolPaymentGate` (native) and the MCP
adapter exposes `PaymentAdmission`; `net-payments` implements both over the one
engine, so a quote paid over the wire is the quote the gate redeems.

### Standing up a provider

The engine takes five things: the identity it signs with, a facilitator, an
admission policy, the asset registry it validates requirements against, and a
path for its state. Attaching a `BillingLog` is what turns step 6 on.

```rust
use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::registry::{default_mock_registry, AssetRegistry};
use net_payments::engine::{AdmitAll, PaymentEngine};
use net_payments::facilitator::mock::MockFacilitator;

let provider_keys = Arc::new(EntityKeypair::generate());
let registry: AssetRegistry = default_mock_registry(provider_keys.entity_id().clone());
let billing = Arc::new(BillingLog::new(dir.join("billing.jsonl")));

let engine = Arc::new(
    PaymentEngine::new(
        provider_keys.clone(),
        Arc::new(MockFacilitator::new()),
        Arc::new(AdmitAll),          // replace with a real admission policy
        registry.clone(),
        dir.join("engine.json"),     // the one store, under one lock
    )?
    .with_billing_log(billing.clone()),
);
```

`AdmitAll` is the shape, not the recommendation — it's what the mock path uses.
A real provider implements `ProviderAdmissionPolicy` to decide which callers may
even be quoted, before any money is discussed.

The price itself is an x402 `PaymentRequirements` carried verbatim
(see [x402 and Net](/docs/payments/x402-and-net)) and wrapped in signed terms:

```rust
use net_payments::core::terms::PricingTerms;
use net_payments::facilitator::mock::{MOCK_NETWORK, MOCK_SCHEME};
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

let template = X402Carry::author(&PaymentRequirements {
    scheme: MOCK_SCHEME.into(),
    network: MOCK_NETWORK.into(),
    amount: "2500".into(),                       // atomic units, as a string
    asset: "musd".into(),
    pay_to: "mock-provider-settle-addr".into(),
    max_timeout_seconds: 60,
    extra: None,
})?;

let terms = PricingTerms::new(
    provider_keys.entity_id().clone(),
    "docs-provider/summarize",
    vec![template],
    registry.reference()?,
);
```

Announce `terms` with the capability and the price travels with discovery.

## Caller side: pricing → spend policy → pay → invoke

1. **Read the price** from discovery (`describe` surfaces `pricing_terms`; `null`
   = free).
2. **Spend policy runs first.** Before anything leaves, the [spend
   policy](/docs/payments/spend-policy-and-approvals) either clears the spend, asks for a
   human approval, or denies. The model does not decide.
3. **Pay.** On clearance, the caller settles the x402 payment (signing only a
   typed intent — see [Non-custodial signing](/docs/payments/non-custodial-signing)) and
   attaches the proof to the invocation.
4. **Invoke.** The call carries the quote; the provider's gate redeems it and
   serves.

If the provider refuses, the denial can carry a machine-actionable [failure
schematic](/docs/payments/failure-schematic) beside the human error, so the caller's agent
can branch on _why_ and _what's safe to do next_ rather than parse prose.

### Calling a paid capability

`CallerPaymentFlow` is constructed once per caller identity and reused. It owns
the spend policy, so steps 2–4 above happen inside `run`:

```rust
use net_payments::flow::{CallerDecision, CallerPaymentFlow, InProcessProvider};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};

let flow = CallerPaymentFlow::new(
    caller_keys,
    SpendPolicyEngine::new(&spend_path, SpendProfile::Production),
    registry,
    provider_channel,   // the machine boundary; InProcessProvider in tests
    clock,
);

match flow.run("docs-provider/summarize", &terms_json).await {
    CallerDecision::Paid { quote_id, proof, .. } => {
        // `quote_id` is the redemption binding the invocation must carry.
        // `proof` holds the settlement refs and the signed billing event.
    }
    CallerDecision::RequiresPaymentApproval { quote_id, policy_reason, approve_hint } => {
        // Nothing has been spent. Surface this to a human — see spend policy.
    }
    CallerDecision::Denied { policy_reason } => { /* policy said no */ }
    CallerDecision::Failed { message, retryable } => { /* transport / facilitator */ }
}
```

Four outcomes, and only one of them spends money. `RequiresPaymentApproval` is
not an error — it's the flow refusing to decide something a human should, and
the provider has billed nothing at that point.

The `ProviderChannel` parameter is where the machine boundary lives.
`InProcessProvider` (used above and in the compiled example) puts the engine in
the same process for tests; over a real mesh it's the nRPC channel to the
provider.

## Engine ownership

The same `PaymentEngine` serves the quote/pay wire and gates priced tools in the
integrated path. Settlement, verification, billing, and redemption use one store
under its lock, which is the boundary for the engine's at-most-once bookkeeping.
