# Caller side — discover a price, pay under policy, invoke

The caller is whoever invokes a paid capability. The caller-side flow lives in
`payments/src/flow/`. It reads pricing from discovery, applies **spend policy
before anything leaves the machine**, authors the x402 payment payload, pays
over a `ProviderChannel`, and returns a structured decision. Read
`concepts.md`, `object-model.md`, and `spend-policy.md`.

**The caller invariant:** the model requests invocation; the **policy engine
decides.** Displaying a price never implies authorization to spend it.

## `ProviderChannel` — the transport seam

```rust
#[async_trait]
pub trait ProviderChannel: Send + Sync {
    async fn quote(&self, caller: &EntityId, capability: &str, template: &X402Carry<PaymentRequirements>) -> Result<Vec<u8>, ChannelError>;
    async fn pay(&self, quote_bytes: &[u8], payload: &X402Carry<PaymentPayload>) -> Result<PayResponse, ChannelError>;
}
```

Two implementations ship:

- `InProcessProvider::new(Arc<PaymentEngine>, Arc<dyn Clock>)` — same-process
  (tests, single-node). Options: `.with_quote_ttl_ns(u64)` (default 60s),
  `.with_required_tier(VerificationTier)` (default `Observed`).
- `MeshPaymentChannel::new(Arc<Mesh>)` (feature `mesh`) — cross-machine over
  nRPC. Resolves the provider node id from the `<node_id>/<capability>`
  segment and calls `net.payments.quote.v1` / `net.payments.pay.v1`.

`ChannelError { message, retryable }`. `PayResponse` is the wire-facing map of
`PaymentDecision` (`#[serde(tag = "status")]`): `Served {billing_event,
transaction}`, `PendingTier {reached, required}`, `Rejected {reason}`,
`Invalidated {reason}`, `Exception {kind}`, `InProgress`, `Failure {retryable,
message}`.

## `CallerPaymentFlow` — the whole caller lifecycle in one call

```rust
use net_payments::flow::{CallerPaymentFlow, CallerDecision, SystemClock};
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};
use net_payments::core::registry::default_registry_v1;

let flow = CallerPaymentFlow::new(
    Arc::new(caller_keypair),                       // the mesh identity that pays (and signs the invocation binding)
    SpendPolicyEngine::new(policy_path, SpendProfile::Production),
    default_registry_v1(caller_keypair.entity_id().clone()),
    provider_channel,                               // Arc<dyn ProviderChannel>
    Arc::new(SystemClock),                          // Arc<dyn Clock>
).with_signer("eip155", Arc::new(external_signer)); // settlement signer per namespace (see signer.md)

let decision: CallerDecision = flow.run("prov/fixture-tool", pricing_terms_json).await;
```

`run(capability, pricing_terms)` does the entire lifecycle:

1. `PricingTerms::from_json_bytes(pricing_terms)` — parse the announced terms.
2. **Select a settleable `accepts[]` entry** (`can_settle`): the network must
   be in the spend policy's `allowed_networks` **and** have a configured
   signer for its namespace. A real-network entry with no configured signer is
   a structured `Denied`, **never a silent fallback**.
3. Get a quote: `spend.approved_quote(capability)` (a previously operator-
   approved held quote) or `provider.quote(...)`.
4. `PaymentQuote::from_json_bytes` + verify the quote is for *this* caller and
   capability, its `requirements` match the announced template **byte-for-byte**,
   and it hasn't expired.
5. `spend.check_and_reserve(&quote, &registry, now_ns)` → `SpendDecision`
   (one locked read-modify-write: check limits *and* reserve the per-day
   counter atomically).
6. Author the x402 payload by namespace: mock JSON on the mock network;
   `exact` on **eip155** → `exact_evm::typed_data` → `signer.sign_typed_data`
   → `exact_evm::payload_object`; `exact` on **solana** →
   `exact_svm::transfer_intent` → `signer.sign_svm_transfer` (the wallet
   builds + partially signs) → `exact_svm::payload_object`. See `signer.md`.
7. `provider.pay(quote_bytes, &payload)` → `PayResponse`.
8. On success, sign the invocation binding
   (`invocation_binding_transcript(quote_id, tool_id)`) and return it in the
   decision.

`CallerDecision`:

| Variant | Meaning / next step |
|---|---|
| `Paid { quote_id, binding_sig: Option<Vec<u8>>, proof: Value }` | Settled. Invoke, passing `quote_id` + `binding_sig` as the payment proof (the provider redeems it). |
| `RequiresPaymentApproval { quote_id, policy_reason, approve_hint }` | Over policy. Surface to the user; an operator calls `spend.approve(quote_id)`, then re-`run` (it picks up the held approved quote). |
| `Denied { policy_reason }` | Policy refused (e.g. network not allowed, no signer). Terminal. |
| `Failed { message, retryable }` | Channel/authoring failure. Retry if `retryable`. |

`SpendDecision::RequiresPaymentApproval` mirrors the consent shape exactly —
same `{quote_id, policy_reason, approve_hint}` fields. **Pinning a capability
is consent, not spending consent:** a pinned paid tool still hits spend policy.

## The approval loop

```rust
match flow.run(cap, terms).await {
    CallerDecision::RequiresPaymentApproval { quote_id, .. } => {
        // render the prompt in agent UX; the DECISION lives in shared policy state, not the model
        // operator/user approves out of band:
        spend.approve(&quote_id).await?;             // true if a pending approval was flipped
        // re-run: approved_quote() returns the held quote and the flow redeems it
        flow.run(cap, terms).await
    }
    other => other,
}
```

`spend.reject(quote_id)`, `spend.pending()` (list quote_ids awaiting
approval), and `spend.clear_approval(quote_id)` complete the operator surface.
On a terminal pre-settlement failure the flow calls
`spend.release_reservation(&quote, now_ns)` so a reserved-but-unspent counter
is given back.

## The MCP gateway path (agent integration)

For agent runtimes, `CallerPaymentFlow` implements `net_mcp::serve::PaymentFlow`
(feature `mcp-gate`): `pay(id, pricing_terms, tool_args) -> PaymentFlowDecision`,
mapping `CallerDecision` → the gateway's decision enum. The gateway auto-runs
the paid lifecycle under policy and otherwise surfaces
`requires_payment_approval` — the **same shape** as consent. This is what the
Python `CapabilityGateway.invoke()` returns as
`status="requires_payment_approval"`. See `bindings.md`.

Wire headers the gate uses (MCP adapter): `net-payment-quote` (the quote),
`net-payment-quote-sig` (the caller binding signature). `PaymentProof {
quote_id, binding_sig }` travels from caller decision to provider redeem.

## Paying an external x402 HTTP API (the two-way door)

If the "provider" is an external x402-speaking HTTP endpoint rather than a Net
capability, use `X402HttpFlow` instead of `CallerPaymentFlow` — same spend
policy, same signer, same objects (because the objects *are* x402). See
`http402.md`.

## The `Clock` seam

```rust
pub trait Clock: Send + Sync { fn now_ns(&self) -> u64; }
pub struct SystemClock;   // wall-clock ns
```

Inject a fixed clock in tests to exercise expiry deterministically. There is
no global clock in the protocol — expiry uses signer timestamps with bounded
policy tolerance (`verification.md`, `spend-policy.md`).
