# Python — payments

Read `../bindings.md` first. This page is only what is Python-specific.

## Package and import — the low-level one

```python
from net import CapabilityGateway, PaymentProvider, build_pricing_terms
```

**Everything here is in `net` (the PyO3 binding, published as `net-mesh`), not
`net_sdk`.** `net/crates/net/sdk-py/src/` contains no payments code at all. An
`from net_sdk import CapabilityGateway` will fail and the failure looks like a
missing feature.

Feature-gated `payments`, on by default in the Python build.

## Demand — pay to invoke

Source: `net/crates/net/bindings/python/src/capability_gateway.rs`, module
`net._net`.

```python
gw = CapabilityGateway(
    mesh,
    pin_store_path=None,
    delegation_leaf=None, delegation_chain=None,
    payment_policy_path=None,
    payment_profile=None,               # "production" | "dev_test" (aliases "dev-test"/"devtest"); unknown → ValueError, no silent fallback
    payment_unsafe_mock_auto_allow=False,
    payment_signer_address=None,
    payment_signer=None,                # eip155: (typed_data_json: str) -> "0x..." (65-byte EIP-712 sig)
    payment_signer_svm_address=None,
    payment_signer_svm=None,            # solana: (intent_json: str) -> base64 partially-signed tx
    payment_signer_xrpl_address=None,
    payment_signer_xrpl=None,           # xrpl:   (intent_json: str) -> hex presigned Payment blob
)
gw.describe(cap_id)                     # JSON string; includes "pricing_terms" (null = free)
gw.invoke(cap_id, arguments_json="{}")  # JSON string; status discriminant (+ "failure" on denials)

# Operator approval verbs — resolve a requires_payment_approval:
gw.approve_payment(quote_id)            # {"status":"ok","quote_id","changed"}
gw.reject_payment(quote_id)             # {"status":"ok","quote_id","changed"}
gw.pending_payments()                   # {"status":"ok","pending":[quote_id,...]}
gw.spent_today(network, asset)          # {"status":"ok","spent":"<atomic>"}  (x402 wire values)
```

`AsyncCapabilityGateway` has the same surface as coroutine duals.

### Return convention — a JSON string, never a raised gate outcome

**Methods return a structured JSON *string* with a `status` discriminant. They
never raise on a gate outcome.** `invoke` can return
`status="requires_payment_approval"` with `{cap_id, quote_id, policy_reason,
approve_hint}`, mirroring `GatedOutcome::RequiresPaymentApproval`. `describe`
carries the announced `net.pricing.terms@1` JSON under `pricing_terms`.

**Denials carry a `failure` object** — the `net.payment.failure@1` schematic
beside `error`, when the provider attached one. Branch on `failure["reason"]` /
`failure["recovery"]` rather than parsing prose; its absence means no schematic
rode the refusal (`failure-schematic.md`).

### Approval verbs are the operator surface

`approve_payment` / `reject_payment` / `pending_payments` / `spent_today` are
thin wrappers over `SpendPolicyEngine` on the same shared spend-policy store the
flow reserves against — the store, lock protocol and Pending→Approved transition
stay in Rust. Without `payment_policy_path` they return a structured
`no_payment_policy`; without the `payments` feature, `unsupported`.

`invoke` only *requests* approval; these grant it (`spend-policy.md`).

### Identity and the signer boundary

Payments wiring builds a `CallerPaymentFlow` over `SpendPolicyEngine`,
`default_registry_v1` and `MeshPaymentChannel`. `payment_profile` maps to
`SpendProfile`. The payment identity is the **node's mesh ed25519 identity**
(`mesh.entity_keypair()`), borrowed in-process.

**The signer never sees a key — for every scheme.** Each `payment_signer*` is a
Python callable `(typed_intent_json) -> artifact_str`, bridged into
`ExternalSigner` (`eip155`) / `ExternalSvmSigner` (`solana`) /
`ExternalXrplSigner` (`xrpl`). Only the typed document and the returned artifact
cross the boundary. Each address+callable pair is **both-or-neither** and
**requires `payment_policy_path`**; the callable is validated as callable at
construction, not on first invoke; all three schemes coexist on one gateway.
Each runs on a **blocking worker thread** (`spawn_blocking` + `Python::attach`)
so the GIL never stalls the mesh reactor.

**Fail-closed when payments is compiled out:** if the `payments` feature is off,
passing any payment kwarg **raises `ValueError`** — never a silent free serve.

## Supply — price and charge

Behind `payments` + `publish`, both on by default. Source:
`net/crates/net/bindings/python/src/payment_provider.rs`.

```python
from net import build_pricing_terms, PaymentProvider

# 1) Stand up a provider over a STARTED mesh. state_path is the settlement store
#    (durable + single-owner). One shared PaymentEngine serves the quote/pay wire
#    AND gates the priced tools.
provider = PaymentProvider(mesh, state_path, billing_log_path=None)
provider.provider_entity_id                 # 32 bytes — the identity that issues quotes
provider.read_billing()                     # [net.billing.event@1 JSON, ...] (needs billing_log_path)

# 2) Author the price with the provider's entity id. `requirements_json` is a JSON
#    array of x402 PaymentRequirements (camelCase wire names). Returns canonical,
#    byte-preserved net.pricing.terms@1.
terms = build_pricing_terms(provider.provider_entity_id, capability, requirements_json)

# 3) Publish priced tools, gated by this provider's engine. pricing maps a tool
#    NAME -> its terms. A priced tool serves once, after payment.
handle = provider.publish_paid_tools(
    tools,          # [(name, description|None, input_schema_json), ...]
    callback,       # async (tool_name, args_json) -> str | (str, bool)
    pricing,        # {tool_name: net.pricing.terms@1 JSON}  — every tool must be priced
    version="", owner_origin=None, allow_any_caller=False,
)
```

- **Fail-closed:** an empty `pricing` map, or any published tool missing a
  pricing entry, raises `ValueError` — a missing entry would publish that tool
  **free**. Use `NetMesh.publish_tools` for genuinely unpriced tools.
- **Provider identity IS the node's mesh identity** — quotes are signed by, and
  settlement tracked against, the same ed25519 identity peers see (`caller.md`).
- **Free publish:** `NetMesh.publish_tools(tools, callback, ...)` publishes a
  node's own tools unpriced (behind `publish`); `publish_paid_tools` layers
  pricing and the engine gate on the same machinery. A started node built with
  `permissive_channels=True` is required — served tools ride dynamic channels.

## Outbound HTTP-402 — opt-in `payments-http`

`PaymentHttpClient` / `AsyncPaymentHttpClient` pay an external x402 HTTP API
through the same spend policy and signers (`http402.md`). Behind an **opt-in
`payments-http` feature** — it pulls `net-payments/http-facilitator`
(reqwest/rustls) and is kept **out of the default wheel**. `try/except
ImportError` before promising it.

```python
from net import PaymentHttpClient       # present iff built with payments-http
client = PaymentHttpClient(
    payment_policy_path,                 # REQUIRED — the caller's spend policy is the entire gate
    payment_profile="dev_test",
    payment_signer_address=None, payment_signer=None,   # same eip155 seam as the gateway
    identity=None,                       # optional payer Identity handle; ephemeral if omitted
)
status_json, body = client.fetch_paid(url)   # SYNC — (str, bytes)
```

`AsyncPaymentHttpClient` is the awaitable dual with the same constructor; its
`fetch_paid` is a **coroutine** — `await` it, never call it bare:

```python
from net import AsyncPaymentHttpClient   # same payments-http feature gate
aclient = AsyncPaymentHttpClient(payment_policy_path, payment_profile="dev_test")
status_json, body = await aclient.fetch_paid(url)
```

Status is `fetched | paid | requires_payment_approval | denied | provider_refused
| transport_error`; `body` is the raw HTTP bytes, empty for the non-body
outcomes. **The HTTP client wires `eip155` only** — svm and xrpl are deferred on
this path, even though the gateway supports all three.

## Gaps

`bindings/coverage.md` is authoritative. The one to know: **tiered verification
is Rust-only.** There is no `ChainChecker` projection, so a settled result here
is the facilitator's claim rather than an independent confirmation.

## Where to look when this page is not enough

- **Authoritative source:**
  `net/crates/net/bindings/python/src/capability_gateway.rs`,
  `net/crates/net/bindings/python/src/payment_provider.rs`,
  `net/crates/net/bindings/python/src/payment_http.rs`.
- **Conformance:**
  `net/crates/net/bindings/python/tests/test_payments_golden_vectors.py`.

## Never infer from another binding

- Signer callables are **synchronous** here and **async (Promise-returning)** in
  Node. Handing an `async def` to `payment_signer` is not the Node pattern
  transplanted; it is a different contract.
- Node requires an explicit `close()` before mesh shutdown because napi classes
  are GC-finalized. Python's binding does not have that obligation in the same
  form — do not port the symptom, and do check `signer.md`.
- Rust returns typed `GatedOutcome` values; here every gate method returns a
  JSON string you must parse.
