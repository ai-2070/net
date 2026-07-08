# The two-way door — paying external x402 HTTP APIs (outbound)

The objects Net signs around **are** x402, so a Net agent can pay an external
x402-speaking HTTP API with the *same* spend policy, the *same* signer, and the
*same* payload authoring — **zero translation.** That's the two-way door:
x402-speaking agents pay Net capabilities, and Net agents pay external x402
APIs, with one object model.

Source: `payments/src/flow/http402.rs`, feature `http-facilitator`.

## What's built vs deferred

- **Outbound (shipped):** a Net agent pays an external x402 HTTP endpoint —
  parse the 402 demand, run spend policy + signer, retry with the payload.
- **Inbound (deferred, demand-driven):** x402-speaking HTTP agents paying Net
  *capabilities* would need an HTTP endpoint surface Net doesn't ship in P1.
  The deferral is deliberate — the deferral is the deliverable.

## The v2 HTTP transport is header-only

A spec fact worth pinning: x402 **v2** HTTP transport is **header-only** — not
v1's `X-PAYMENT` body scheme. Bodies are the server's business.

```rust
pub const HDR_PAYMENT_REQUIRED:  &str = "payment-required";   // server → client demand on 402
pub const HDR_PAYMENT_SIGNATURE: &str = "payment-signature";  // client → server payload on retry
pub const HDR_PAYMENT_RESPONSE:  &str = "payment-response";   // server → client settlement on success
```

## `X402HttpFlow`

```rust
use net_payments::flow::http402::{X402HttpFlow, X402HttpOutcome};

let flow = X402HttpFlow::new(
    Arc::new(caller_keypair),
    SpendPolicyEngine::new(policy_path, SpendProfile::Production),
    default_registry_v1(caller_keypair.entity_id().clone()),
    Arc::new(SystemClock),
)?.with_signer("eip155", Arc::new(external_signer));

let outcome = flow.fetch_paid("https://api.example.com/paid-resource").await;
```

`fetch_paid(url)` does: GET → if `402`, parse the `PaymentRequired` demand →
run the **same spend engine** (under capability key `x402-http/<host>`, via a
local pseudo-quote) → author the x402 payload with the **same signer** → retry
with the `PAYMENT-SIGNATURE` header → read the `PAYMENT-RESPONSE` settlement.

```rust
pub enum X402HttpOutcome {
    Ok { status: u16, body: Vec<u8> },                                   // 200 on first hit, no payment needed
    Paid { status, body, settlement: Option<X402Carry<SettlementResponse>> }, // paid and served
    RequiresPaymentApproval { quote_id, policy_reason, approve_hint },   // over policy — same shape as everywhere
    Denied { policy_reason },                                            // network not allowed / no signer
    PaymentRejected { status, message },                                 // server refused the payload
    Failed { message, retryable },                                       // transport/authoring failure
}
```

The spend-policy surface is identical to `caller.md`: over-cap yields
`RequiresPaymentApproval`, an operator `approve`s, the next call proceeds. A
network with no configured signer is `Denied`, never a silent fallback.

## Python — `PaymentHttpClient` (opt-in `payments-http`)

The Python demand surface for this flow. Behind an **opt-in `payments-http`
feature** (it pulls `net-payments/http-facilitator` = reqwest/rustls, kept out
of the default wheel — `try/except ImportError` before promising it):

```python
from net import PaymentHttpClient       # or AsyncPaymentHttpClient
client = PaymentHttpClient(
    payment_policy_path,                 # REQUIRED — the caller's spend policy is the entire gate
    payment_profile="dev_test",
    payment_signer_address=None, payment_signer=None,   # eip155 seam (svm/xrpl deferred on this path)
    identity=None,                       # optional payer Identity; ephemeral if omitted
)
status_json, body = client.fetch_paid(url)   # (str, bytes)
```

The status-JSON projects `X402HttpOutcome` to `fetched | paid |
requires_payment_approval | denied | provider_refused | transport_error`
(`paid` carries the byte-preserved settlement as base64); `body` is the raw
response bytes (empty for the non-body outcomes). The lifecycle stays in Rust —
the binding only marshals typed intent in / status-JSON + bytes out
(`bindings.md`).

## Why this is the same code, not a bridge

There is no translation layer because there is nothing to translate — the
payment payload the flow authors for an external HTTP API is the exact same
`X402Carry<PaymentPayload>` it authors for a Net capability. The registry, the
spend engine, the `SchemeSigner`, and the byte-preservation discipline are all
shared. If you find yourself writing an x402↔Net "adapter," stop: the objects
already *are* x402 (that's doctrine 1, and building a parallel wire format is a
rejected PR — see `gotchas.md`).
