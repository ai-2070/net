# Cross-language surface — who has what

**Logic never lives in bindings.** The whole payment engine — envelopes,
canonicalization, facilitator interface, spend policy, verification, signing
seam — is the Rust crate `net-payments`. Bindings expose *references* into it;
they never re-implement money logic. This shapes what each language can do.

**Get the language right first**, then check this table before promising a
caller flow that doesn't exist in that language.

| Language | Native caller flow (demand) | Price + charge at publish (supply) | Price at discovery | `requires_payment_approval` | Golden-vector verifier |
|---|---|---|---|---|---|
| **Rust** (`payments/`, `sdk/`, `adapters/mcp/`) | ✅ full | ✅ `serve_tool_paid` / MCP wrap `publish_tools` | ✅ | ✅ `GatedOutcome::RequiresPaymentApproval` | ✅ source of truth |
| **Python** | ✅ via `CapabilityGateway` | ✅ `PaymentProvider` + `build_pricing_terms` | ✅ `describe()` JSON | ✅ `invoke()` JSON | ✅ |
| **Node / TS** | ✅ via `CapabilityGateway` | ✅ `PaymentProvider` + `buildPricingTerms` | ✅ `listTools`/`watchTools` | ✅ `invoke()` JSON | ✅ |
| **Go** | ❌ | ❌ | ❌ | ❌ | ✅ (`go/payments_golden_vectors_test.go`) |

**Rust, Python, AND Node** now have a full native **demand + supply** payment
flow — pay to invoke *and* price + charge for a capability. Only **Go** is
verifier-only (no flow; its verifier is at the repo root `go/`, not
`bindings/go/`).

**Built state** (as of 2026-07-09): Python and Node both reach **demand+supply
parity with Rust** — gateway (search/describe/invoke, approval verbs, `failure`),
eip155/svm/xrpl signers, HTTP-402 client, a free `publishTools`/`publish_tools`
path, and the paid `PaymentProvider` (`buildPricingTerms` + `publishPaidTools` +
`readBilling`). The wire vocabulary stays single-sourced (`net_sdk::tool_payment`
/ `net-payments`); the cross-language `failure_schematic_vectors` are the
executable contract, projected + status-tested per binding. A
`bindings/go/payments-ffi` cdylib and a hand-written `include/net_payments.h` are
**not built** — don't promise them.

## Rust — the whole thing

`net-payments` (this skill's `provider.md` / `caller.md` cover it). The mesh
wire (`serve_payments`, `MeshPaymentChannel`), the MCP gate
(`gated_invoke` → `PaymentFlow` / `PaymentAdmission`), and the publish-side
price setter (`sdk/src/tool.rs`: `ToolMetadataBuilder::pricing_terms(terms_json)`
→ `descriptor.pricing_terms`, announced opaquely under
`pricing_terms_metadata_key(tool_id)`) all live here.

## Python — the demand surface (pay to invoke)

The caller-side flow is exposed through the capability gateway, feature-gated
`payments` (on by default in the Python build). File:
`bindings/python/src/capability_gateway.rs`, module `net._net`.

```python
gw = CapabilityGateway(
    mesh,
    pin_store_path=None,
    delegation_leaf=None, delegation_chain=None,
    payment_policy_path=None,
    payment_profile=None,               # "production" | "dev_test"
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

`AsyncCapabilityGateway` has the same surface (coroutine duals). Key facts:

- **Methods return a structured JSON *string* with a `status` discriminant —
  they never raise on a gate outcome.** `invoke` can return
  `status="requires_payment_approval"` with `{cap_id, quote_id, policy_reason,
  approve_hint}` (mirrors `GatedOutcome::RequiresPaymentApproval`). `describe`
  carries the announced `net.pricing.terms@1` JSON under `pricing_terms`.
- **Denials carry a `failure` object** — the `net.payment.failure@1` schematic
  beside `error` when the provider attached one. Branch on `failure["reason"]` /
  `failure["recovery"]` instead of parsing prose; its absence means no schematic
  rode the refusal (`failure-schematic.md`).
- **Approval verbs close the loop:** `approve_payment` / `reject_payment` /
  `pending_payments` / `spent_today` are thin wrappers over
  `SpendPolicyEngine` on the same shared spend-policy store the flow reserves
  against — the store, lock protocol, and Pending→Approved transition stay in
  Rust. Without `payment_policy_path` they return a structured
  `no_payment_policy`; without the `payments` feature, `unsupported`. This is
  the **operator** surface — `invoke` only *requests* approval; these grant it
  (`spend-policy.md`).
- **Payments wiring** builds a `CallerPaymentFlow` over `SpendPolicyEngine`,
  `default_registry_v1`, and `MeshPaymentChannel`. `payment_profile` maps to
  `SpendProfile`. The payment identity is the **node's mesh ed25519 identity**
  (`mesh.entity_keypair()`), borrowed in-process.
- **The signer never sees a key — for every scheme.** Each `payment_signer*` is
  a Python callable `(typed_intent_json) -> artifact_str`, bridged into
  `ExternalSigner` (`eip155`) / `ExternalSvmSigner` (`solana`) /
  `ExternalXrplSigner` (`xrpl`). Only the typed document and the returned
  artifact cross the boundary — doctrine 7/8 holds at the language edge. Each
  address+callable pair is **both-or-neither** (shared validator) and
  **requires `payment_policy_path`**; the callable is validated as callable at
  construction, not on first invoke; all three schemes coexist on one gateway.
  Each runs on a **blocking worker thread** (`spawn_blocking` + `Python::attach`)
  so the GIL never stalls the mesh reactor.
- **Fail-closed when payments is compiled out:** if the `payments` feature is
  off, passing any payment kwarg **raises `ValueError`** — never a silent free
  serve.

## Python — the outbound HTTP-402 client (opt-in `payments-http`)

`PaymentHttpClient` / `AsyncPaymentHttpClient` pay an external x402 HTTP API
through the same spend policy + signers (`http402.md`). Behind an **opt-in
`payments-http` feature** (it pulls `net-payments/http-facilitator` =
reqwest/rustls, kept OUT of the default wheel), so `try/except ImportError`
before promising it.

```python
from net import PaymentHttpClient       # present iff built with payments-http
client = PaymentHttpClient(
    payment_policy_path,                 # REQUIRED — the caller's spend policy is the entire gate
    payment_profile="dev_test",
    payment_signer_address=None, payment_signer=None,   # same eip155 seam as the gateway
    identity=None,                       # optional payer Identity handle; ephemeral if omitted
)
status_json, body = client.fetch_paid(url)   # SYNC — (str, bytes): the X402HttpOutcome projection + raw body
```

`AsyncPaymentHttpClient` is the awaitable dual (same constructor); its
`fetch_paid` is a **coroutine** — `await` it (the `AsyncCapabilityGateway`
coroutine-dual pattern above), never call it bare:

```python
from net import AsyncPaymentHttpClient   # same payments-http feature gate
aclient = AsyncPaymentHttpClient(payment_policy_path, payment_profile="dev_test")
status_json, body = await aclient.fetch_paid(url)   # coroutine — await required
```

`fetch_paid` returns `(status_json, body)` (the sync form directly, the async
form once awaited) — status is
`fetched | paid | requires_payment_approval | denied | provider_refused |
transport_error`; `body` is the raw HTTP bytes (empty for the non-body
outcomes). The HTTP client wires **eip155 only** in v1 (svm/xrpl on this path
are deferred).

## Python — the supply surface (price + charge)

Behind `payments` + the `publish` feature (both on by default). File:
`bindings/python/src/payment_provider.rs`. A node can now be a *paid provider*,
not just a caller.

```python
from net import build_pricing_terms, PaymentProvider

# 1) Stand up a provider over a STARTED mesh. state_path is the settlement store
#    (durable + single-owner). One shared PaymentEngine serves the quote/pay wire
#    AND gates the priced tools. It exposes the provider's entity id.
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
  pricing entry, is a `ValueError` (a missing entry would publish that tool
  **free**). Use `NetMesh.publish_tools` (free path) for unpriced tools.
- **Provider identity IS the node's mesh identity** — quotes are signed by, and
  settlement tracked against, the same ed25519 identity peers see (`caller.md`).
- **Free publish:** `NetMesh.publish_tools(tools, callback, ...)` publishes a
  node's own tools *unpriced* (behind `publish`); `publish_paid_tools` layers
  pricing + the engine gate on the same machinery. A started node built with
  `permissive_channels=True` is required (the served tools ride dynamic
  channels).

## Node / TS — full demand + supply (parity with Python)

Node reached parity this cycle. `bindings/node/` now binds the whole surface —
the gateway (demand), the paid provider (supply), the HTTP-402 client, and the
free publish path — behind `payments` (default), `publish` (default), and the
opt-in `payments-http`. Callbacks are **JS async functions returning Promises**
(not sync callables like Python), and every gate method resolves to a JSON
**string** (never a throw for a gate outcome).

```ts
import { CapabilityGateway, PaymentProvider, PaymentHttpClient, buildPricingTerms } from '@net-mesh/core'

// --- Demand: pay to invoke (bindings/node/src/capability_gateway.rs) ---
const gw = new CapabilityGateway(
  mesh, pinStorePath ?? null,
  paymentPolicyPath ?? null, paymentProfile ?? null,   // "production" | "dev_test"
  paymentUnsafeMockAutoAllow ?? false,
  paymentSignerAddress, paymentSigner,                 // eip155: async (typedDataJson) => "0x..." (65-byte sig)
  paymentSignerSvmAddress, paymentSignerSvm,           // solana: async (intentJson) => base64 tx
  paymentSignerXrplAddress, paymentSignerXrpl,         // xrpl:   async (intentJson) => hex Payment blob
)
await gw.search(query)                    // JSON string; capabilities[] each with requires_approval
await gw.describe(capId)                  // JSON string; includes pricing_terms (null = free)
await gw.invoke(capId, argumentsJson)     // JSON string; status discriminant (+ failure on denials)
await gw.approvePayment(quoteId)          // operator verbs — resolve a requires_payment_approval
await gw.rejectPayment(quoteId); await gw.pendingPayments(); await gw.spentToday(network, asset)
gw.close()                                // RELEASE the node clone before mesh.shutdown() (see below)

// --- Supply: price + charge (bindings/node/src/payment_provider.rs) ---
const provider = new PaymentProvider(mesh, statePath, billingLogPath /* ? */)
provider.providerEntityId                 // Buffer (32B); the identity that issues quotes
await provider.readBilling()              // string[] of net.billing.event@1
// Author the price with the provider's own entity id:
const terms = buildPricingTerms(provider.providerEntityId, capability, requirementsJson)
const pub = await provider.publishPaidTools(
  tools,                                  // [{ name, description?, inputSchema }]
  handler,                                // async ({ toolName, argumentsJson }) => ({ text, isError? })
  pricing,                                // { [toolName]: termsJson } — every tool must be priced
  { version?, ownerOrigin?, allowAnyCaller?, handlerTimeoutMs? },
)
provider.close()                          // tears down the quote/pay wire + releases the node

// --- Free publish (behind `publish`): NetMesh.publishTools ---
const handle = await mesh.publishTools(tools, handler, { allowAnyCaller: true })
handle.serving; handle.tools; await handle.withdraw(); handle.stop()

// --- Outbound HTTP-402 (opt-in `payments-http`): pay an external x402 API ---
const client = new PaymentHttpClient(paymentPolicyPath, paymentProfile, false, signerAddress, signer)
const [statusJson, body] = await client.fetchPaid(url)   // [string, Buffer]
```

Node-specific facts (state them if the user hits them):

- **`invoke` status vocabulary** matches Python: `ok | requires_approval |
  requires_payment_approval | validation_error | denied | not_found |
  transport_error | no_daemon | error`. A denial carries a `failure` object
  (the `net.payment.failure@1` schematic) **only when the provider attached
  one** — branch on `failure.reason` / `failure.recovery` when present, else the
  prose (`failure-schematic.md`).
- **Signers are async** (`(typedIntentJson) => Promise<string>`), bridged to
  `ExternalSigner`/`ExternalSvmSigner`/`ExternalXrplSigner` via a
  `ThreadsafeFunction`→Promise seam (the `blob.rs` pattern). Both-or-neither per
  scheme; requires `paymentPolicyPath`. **One-sided timeout:** a signer timeout
  drops the Rust wait but does NOT cancel the JS callback — treat it as
  indeterminate (`signer.md`).
- **`close()` is mandatory before `NetMesh.shutdown()`.** A `#[napi]` class is
  GC-finalized, not scope-dropped, so `CapabilityGateway` / `PaymentProvider`
  retain a node clone that makes `shutdown()`'s `Arc::try_unwrap` fail with
  "outstanding references" until GC runs. `close()` drops it deterministically;
  publish handles use `withdraw()` (awaited) / `stop()`.
- **`permissiveChannels: true`** on `NetMesh.create({...})` (the Node twin of
  Python's `permissive_channels`) is required for `publishTools` /
  `publishPaidTools` — the served tools ride dynamically-named channels the
  default channel-config registry would reject. Default `false`.
- **`NetMesh.localAddr()`** returns the OS-assigned `ip:port` (for a two-node
  handshake); `listTools()` now also carries `pricingTerms` (was
  `watchTools`-only). `@net-mesh/payments` stays a reservable re-export name —
  everything ships inside `@net-mesh/core`.

## Go — verifier only, no flow

The Go SDK has **no** payment flow, no publish-side pricing, and no
`payments-ffi` binding. What it *does* have (and the skill previously denied) is
a **golden-vector verifier** at the repo root: `go/payments_golden_vectors_test.go`
runs the same cross-language fixture (canonical encoding, ed25519, x402
byte-preservation, and the `failure_schematic_vectors` tolerance predicate). A
real `bindings/go/payments-ffi` cdylib + Go wrapper is **not built** — for a
payment flow today, the honest answer is still Rust or Python.

## The one invariant every binding upholds

x402 documents are always carried as base64 of preserved bytes and **never
re-serialized through a binding's own JSON encoder.** The golden-vector
verifiers in each language exist precisely to prove byte-preservation holds
across the language boundary — that's their whole job (`testing.md`).
