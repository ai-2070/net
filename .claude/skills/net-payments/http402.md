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
  *capabilities* would need an HTTP endpoint surface Net does not ship. The
  deferral is deliberate — the deferral is the deliverable.

## The v2 HTTP transport is header-only

A spec fact worth pinning: x402 **v2** HTTP transport is **header-only** — not
v1's `X-PAYMENT` body scheme. Bodies are the server's business.

```rust
pub const HDR_PAYMENT_REQUIRED:  &str = "payment-required";   // server → client demand on 402
pub const HDR_PAYMENT_SIGNATURE: &str = "payment-signature";  // client → server payload on retry
pub const HDR_PAYMENT_RESPONSE:  &str = "payment-response";   // server → client settlement on success
```

### You pay the server's advertised ceiling, exactly

The pinned v2 spec names the price `amount`. Widely-deployed x402 servers
name it **`maxAmountRequired`**, and in that vocabulary it is a *maximum* —
the most the server will accept, not necessarily what it wants.

`PaymentRequirements` accepts both spellings (`#[serde(alias)]`), and Net
then treats the value as an **exact** amount everywhere: the caller authors
an authorization for the full figure, and the provider-side engine's
verification demands exact equality (under-delivery invalidates,
over-delivery routes to a manual `Overpayment` exception).

So paying a server that advertises `maxAmountRequired` means **paying its
ceiling**, not negotiating below it. There is no partial-payment path and
no counter-offer object — that absence is the rule. It is not a
correctness or safety problem (spend policy still caps every outbound
payment, and the byte-preserved requirements are what got signed), but
budget for the advertised maximum when you set `max_per_call` for an
external host.

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

### The destination policy is `PublicOnly`, and `localhost` is refused

`new()` builds with `DestinationPolicy::PublicOnly`. This is the one door in the
crate whose URL can be chosen by a *model*, and a permissive default would put
every integration one model-chosen URL away from an unauthenticated service on
the same host — so an agent-supplied `http://localhost:8080/…` is `Denied`
before a socket is opened, not dialled.

A local or self-hosted x402 server is reached by **asking for it**:

```rust
let flow = X402HttpFlow::with_destination_policy(
    caller, spend, registry, clock,
    DestinationPolicy::PublicOrLoopback,   // or AllowPrivate for a LAN server
)?;
```

The bindings take the same choice as a string — `destination_policy` /
`destinationPolicy`, one of `public_only` (default), `public_or_loopback`,
`allow_private`. An unknown value is a construction error rather than a
fall-back, and `unrestricted` is deliberately not spellable here.

The guard is address-level, so it is not bypassable by spelling: hostnames are
checked at resolve time (rebinding-safe), IP literals before the send, and the
v4-in-v6 embeddings (`::ffff:`, NAT64, 6to4, Teredo) are refused as the v4
addresses they reach.

## Python + Node — `PaymentHttpClient` (opt-in `payments-http`)

The demand surface for this flow, in **both** Python and Node. Behind an
**opt-in `payments-http` feature** (it pulls `net-payments/http-facilitator` =
reqwest/rustls, kept out of the default build — `try/except ImportError` in
Python / feature-probe in Node before promising it):

```python
from net import PaymentHttpClient       # or AsyncPaymentHttpClient
client = PaymentHttpClient(
    payment_policy_path,                 # REQUIRED — the caller's spend policy is the entire gate
    payment_profile="dev_test",
    payment_signer_address=None, payment_signer=None,   # eip155 seam (svm/xrpl deferred on this path)
    identity=None,                       # optional payer Identity; ephemeral if omitted
    destination_policy=None,             # "public_only" (default) — see above; localhost needs "public_or_loopback"
)
status_json, body = client.fetch_paid(url)   # (str, bytes)
```

```ts
// Node — same shape; the eip155 signer callback is async (Promise).
const client = new PaymentHttpClient(
  paymentPolicyPath, 'dev_test', false, signerAddress, signer,
  'public_only',  // destinationPolicy — localhost needs 'public_or_loopback'
)
const [statusJson, body] = await client.fetchPaid(url)   // [string, Buffer]
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

## Further reading

- [x402 and Net](https://ai2070.net/docs/payments/x402-and-net)
