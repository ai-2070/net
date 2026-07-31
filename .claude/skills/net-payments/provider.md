# Provider side — issue quotes, verify payments, serve, bill

The provider is whoever publishes a paid capability and runs the handler. The
`PaymentEngine` (`engine/mod.rs`) owns the whole provider-side lifecycle. Read
`concepts.md` and `object-model.md` first.

**The invariant:** handlers **never see unpaid calls.** The gate runs before
the handler; a payment is verified to the required tier, provider policy
re-checks, and only then does the handler fire.

## Construct the engine

```rust
use std::sync::Arc;
use net_payments::engine::{PaymentEngine, ProviderAdmissionPolicy, AdmitAll};
use net_payments::facilitator::mock::MockFacilitator;
use net_payments::core::registry::default_registry_v1;
use net_payments::billing::BillingLog;

let provider = Arc::new(provider_keypair);            // the mesh EntityKeypair — this IS the provider identity
let facilitator = Arc::new(MockFacilitator::new());   // swap for HttpFacilitator on a real network (facilitator.md)
let admission = Arc::new(AdmitAll);                    // dev only; real provider policy is your impl
let registry = default_registry_v1(provider.entity_id().clone());

let engine = PaymentEngine::new(provider.clone(), facilitator, admission, registry, state_path)?
    .with_expiry_tolerance_ns(2_000_000_000)          // bounded clock tolerance on quote expiry (default 0)
    .with_terminal_record_retention_ns(None)          // keep terminal records forever (default: Some(6h))
    .with_billing_log(Arc::new(BillingLog::new(billing_path)));  // optional stream/export sink
```

`state_path` is a locked JSON store (pins pattern) holding the replay index,
per-quote records, and consumed transactions. Use
`policy::store::default_payment_engine_path()` for the standard location
(`<local data>/net-mesh/payment-engine.json`).

### Terminal-record compaction (default-on, 6h)

Every engine operation parses the whole store and rewrites it when dirty, so
store size is a latency term on every payment. The engine therefore **retires
terminal quote records 6 hours past authoritative quote expiry**
(`DEFAULT_TERMINAL_RECORD_RETENTION_NS`), sweeping opportunistically inside
`accept_payment`'s claim transaction. `prune_terminal_records(now_ns)` runs the
same sweep on demand, for a provider that stops accepting but keeps redeeming.

- `Some(longer)` — a wider local re-verification / forensic window;
- `None` — keep terminal records indefinitely;
- `Some(0)` — **refused**, normalized to the default and logged. Zero reads as
  "off" but would mean compaction the instant a quote expires; `None` is off.

**What is never compacted**, at any setting: settlement-transaction tombstones
(the permanent uniqueness index — expiring those means one payment serves
twice); records paid but never redeemed; frozen records, including one frozen
*after* redemption by a checker finding the settlement reverted; and legacy
records carrying no authoritative expiry.

Two consequences worth knowing before you rely on the engine store:

- **`status()` is not an audit surface.** It returns `None` for a completed
  quote past the horizon. The durable record of a payment is the `BillingLog`
  (`billing.md`); the quote record is engine bookkeeping.
- **The horizon is your re-verification window.** Prunability does not consult
  the verified tier, so a record served at `observed` retires on the same clock
  as one a checker drove to `final`. If you re-verify out of band, on a slower
  rail, or at a raised `FINAL_DEPTH_*`, widen the window past your own
  re-verification period or pass `None`.

Compaction bounds the *redeemed* population only. If the store keeps growing,
it is the never-redeemed and frozen classes; the engine warns once as it
crosses `ENGINE_STORE_SIZE_WARN_RECORDS` (10 000).

`PaymentEngine::new(provider, facilitator, admission, registry, state_path)`
takes `Arc<EntityKeypair>`, `Arc<dyn Facilitator>`, `Arc<dyn
ProviderAdmissionPolicy>`, `AssetRegistry`, `impl Into<PathBuf>`. It runs the
registry `reference()` at build (fails if the registry can't self-reference).

## Provider admission policy — never quote a caller you'd deny

```rust
pub trait ProviderAdmissionPolicy: Send + Sync {
    fn admit(&self, caller: &EntityId, capability: &str) -> Result<(), String>;  // Err(reason) refuses issuance
}
```

`admit` runs **at quote issuance**, before signing. This is doctrine: accepting
a denied caller's payment creates refund obligations the protocol doesn't want.
`AdmitAll` exists for tests/dev only — real policy is caller allowlists,
attestation requirements, network/asset allowlists, exposure caps. The
post-verification check inside the serve gate is a *re-check*, not the first
check.

## The lifecycle (method order)

### 1. Issue a quote

```rust
let quote: PaymentQuote = engine.issue_quote(
    caller_entity_id,
    "prov/fixture-tool",
    requirements_carry,     // X402Carry<PaymentRequirements> — an INSTANTIATED template
    now_ns,
    ttl_ns,                 // quote lifetime; expires_at_ns = now_ns + ttl_ns
)?;
```

Runs admission + registry `check_requirements` (asset allowed, decimals
cross-check), then builds and **signs** the quote with the provider identity.
The returned `PaymentQuote` is the binding, provider-signed offer
(`object-model.md`). Serialize it (`canonical_bytes`) to send to the caller.

### 2. Accept payment — verify + settle + chain + bill (one call)

```rust
let decision: PaymentDecision = engine.accept_payment(
    &quote,
    &payload_carry,         // X402Carry<PaymentPayload> — the client-signed x402 authorization
    required_tier,          // VerificationTier the provider requires before serving
    now_ns,
).await?;
```

Internally, in order: static checks (expiry, payload accepts the quoted
requirements) → **locked claim** (replay index + idempotency; concurrent
same-key attempts get `InProgress`) → `facilitator.verify` → `facilitator.settle`
→ network / tx-replay / delivered-amount policy → a signed `VerificationEvent`
chained per quote → if `Verified` and the tier is satisfied, build + sign a
`BillingEvent` and append it to the `BillingLog`.

`PaymentDecision` (match every arm — this is the fail-closed contract):

| Variant | Meaning |
|---|---|
| `Served { billing: Box<BillingEvent>, tier }` | Verified at/above the required tier — the handler may run. Same-key retries return this same billing event. |
| `PendingTier { reached, required }` | Settled, but confidence hasn't reached the required tier. Re-verify later; handler does **not** run. |
| `Exception { kind: ExceptionKind }` | e.g. `Overpayment` — for provider policy to handle manually. The verifier never auto-satisfies; no auto-refunds in v1. |
| `Invalidated { reason: InvalidationReason }` | A previously-verified payment was withdrawn (reorg &c). Quote frozen; nothing further serves. |
| `InProgress` | Another attempt on the same key is mid-flight right now. |
| `Rejected { reason: RejectReason }` | Terminal rejection (see below). |
| `FacilitatorFailure { kind, retryable, message }` | The facilitator couldn't answer. **Fail-closed default;** policy chooses retry/fallback. Nothing was consumed. |

`RejectReason`: `QuoteExpired`, `QuoteFrozen(s)`, `BadQuote(s)`,
`PayloadMismatch`, `Replay`, `QuoteAlreadyPaid`, `VerifyRejected(s)`,
`SettleFailed(s)`.

### 3. Tier upgrade / reorg watch (optional, repeatable)

```rust
// Facilitator-only re-verify — caps at Observed (spec reports no finality):
engine.re_verify(quote_id, required_tier, now_ns).await?;

// The ONLY path to confirmed(n)/final — an independent on-chain check:
engine.re_verify_with_checker(quote_id, &checker, required_tier, now_ns).await?;
```

A settled-but-tier-unmet retry of `accept_payment` also routes to `re_verify`.
The checker upgrades the chain with `Verified@Confirmed(n)` / `Verified@Final`
events (`VerifierRef.endpoint = "independent-chain-check:<rpc>"`), and a reorg
becomes an `Invalidated{reorg}` that freezes the quote. See `verification.md`.

### 4. Serve gate — redeem the quote for its one invocation

```rust
let redeem: RedeemDecision = engine.redeem_for_invocation(
    "prov/fixture-tool",    // tool_id — bound; a quote for tool A can't invoke tool B
    quote_id,
    binding,                // Option<&[u8]> — the caller's invocation-binding signature (hardening; see below)
).await?;
// RedeemDecision::Admitted   (at most once — sets `redeemed`)
// RedeemDecision::Denied { reason }   (fail-closed; the reason travels to the caller)
```

`redeem_for_invocation` admits the quote's **one** invocation and consumes it.
It requires the quote to be billed, unfrozen, and tool-bound. The optional
`binding` is a caller-signed proof that the invoker *is* the payer, not merely
someone who saw the quote id — ed25519 over
`invocation_binding_transcript(quote_id, tool_id)` (domain-separated,
length-prefixed). Present-but-invalid rejects; absent degrades to bearer
(quote-id) redemption.

### 5. Inspect / bill

```rust
let status: Option<QuoteStatus> = engine.status(quote_id).await?;
// QuoteStatus { frozen: Option<String>, served: bool, tier: Option<VerificationTier>,
//               billing_event_id: Option<String>, chain: Vec<VerificationEvent> }
```

The `None` is **live lifecycle, not "never paid"** — a compacted quote reads the
same as an unknown one. Don't branch on it for reconciliation.

Billing: `BillingLog::subscribe()` (a `tokio::sync::broadcast::Receiver<BillingEvent>`),
`read_all()`, `export_jsonl(dest)`. See `billing.md`.

## Pricing at publish (announce the price at discovery)

Pricing attaches as capability metadata via `net.pricing.terms@1` — no 402
round-trip on the mesh, no CLI. Build the terms with the `accepts[]` templates:

```rust
use net_payments::core::terms::PricingTerms;
let terms = PricingTerms::new(provider.entity_id().clone(), "prov/fixture-tool", vec![template_carry], registry_ref);
```

Attach the canonical JSON to the descriptor with
`ToolDescriptorBuilder::pricing_terms(terms_json)`
(`net/crates/net/sdk/src/tool.rs`) — the substrate carries the string opaquely
and never parses payment objects. The MCP bridge's `publish_server` /
`publish_tools` opts carry the same field. A paid capability is **metadata +
invocation policy, not a different kind of tool.** The caller reads these terms
from discovery and drives its side (`caller.md`). The templates are non-binding
until a quote instantiates one.

**An announced price must be an enforced price, and the SDK refuses to let you
break that.** The two serve paths are a fail-closed pair:

| You call | Descriptor has `pricing_terms` | Result |
|---|---|---|
| `serve_tool` / `serve_tool_streaming` | yes | **refused** — `ServeError::UnenforceablePricing` |
| `serve_tool` / `serve_tool_streaming` | no | serves free, as intended |
| `serve_tool_paid(descriptor, gate, handler)` | yes | serves, gated |
| `serve_tool_paid(..)` | no | **refused** — `ServeError::MissingPricingTerms` |

So you cannot announce a price on a path that has no gate (it would be
discovered as paid and served free to any direct caller), and you cannot put a
gate on an unannounced price (it would refuse every caller with no way to know
why). `serve_tool_paid` takes an `Arc<dyn ToolPaymentGate>` —
`redeem(tool_id, quote_id, binding) -> Result<(), GateDenial>`; the request body
is decoded **before** the gate, so a structurally invalid call is rejected
without consuming the quote.

## The provider gate in the invocation chain

There is no `payment_gate` symbol — the payment gate is a *stage* inside
`gated_invoke` (`net/crates/net/adapters/mcp/src/serve/gated.rs`), driven by
the `PaymentFlow` you pass in:

```
describe → validate → consent gate → payment gate → invoke (handler)
```

Two properties worth holding onto: **consent runs before payment** (never buy
access to something the user hasn't consented to invoke), and **no configured
flow fails closed** (`GatewayError::Denied`) — the handler never sees an unpaid
call.

For MCP-gateway hosts, the `mcp-gate` feature provides
`EnginePaymentAdmission::new(Arc<engine>)` (impls `net_mcp::serve::PaymentAdmission`),
wired via `WrapConfig.payment_admission` — its `redeem` delegates to
`redeem_for_invocation`, fail-closed. See `caller.md` for the demand side and
`bindings.md` for the Python + Node gateway and `PaymentProvider` surfaces (both
now expose a native `publishPaidTools` over this same engine + admission wiring).

## Serving over the mesh (cross-machine)

With the `mesh` feature, wrap the engine as an `InProcessProvider` and serve
it over typed nRPC:

```rust
use net_payments::flow::{InProcessProvider, mesh::serve_payments};
let provider_channel = Arc::new(InProcessProvider::new(Arc::new(engine), clock));
let _handle = serve_payments(&mesh, provider_channel)?;   // registers QUOTE_SERVICE + PAY_SERVICE; RAII handle
```

Service names: `net.payments.quote.v1`, `net.payments.pay.v1`. The caller uses
`MeshPaymentChannel` (see `caller.md`). `serve_payments` returns a
`PaymentServeHandle` — keep it alive for the registration to stand.

## Further reading

- [The Lifecycle](https://ai2070.net/docs/payments/the-lifecycle)
