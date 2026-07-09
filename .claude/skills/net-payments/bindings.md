# Cross-language surface — who has what

**Logic never lives in bindings.** The whole payment engine — envelopes,
canonicalization, facilitator interface, spend policy, verification, signing
seam — is the Rust crate `net-payments`. Bindings expose *references* into it;
they never re-implement money logic. This shapes what each language can do.

**Get the language right first**, then check this table before promising a
caller flow that doesn't exist in that language.

| Language | Native caller flow | Price at publish | Price at discovery | `requires_payment_approval` | Golden-vector verifier |
|---|---|---|---|---|---|
| **Rust** (`payments/`, `sdk/`, `adapters/mcp/`) | ✅ full | ✅ `tool.rs` `pricing_terms(..)` | ✅ | ✅ `GatedOutcome::RequiresPaymentApproval` | ✅ source of truth |
| **Python** | ✅ via `CapabilityGateway` | ❌ (no pricing on `tool.py`) | ✅ `describe()` JSON | ✅ `invoke()` JSON | ✅ |
| **Node / TS** | ❌ | ❌ | ✅ read-only `ToolDescriptor.pricingTerms` | ❌ | ✅ |
| **Go** | ❌ | ❌ | ❌ | ❌ | ✅ (`go/payments_golden_vectors_test.go`) |

Only **Rust and Python** have a native payment flow. Node has pricing as a
read-only discovery field; Node, Go, and Rust all carry a golden-vector
verifier (Go's is at the repo root `go/`, not `bindings/go/`).

**Built state** (as of 2026-07-08): the full Python demand surface — approval
verbs, HTTP-402 client, svm/xrpl signers — is done, along with the parity
matrix pinned in the crate module doc and the cross-language
`failure_schematic_vectors`. A Node `CapabilityGateway` (the long pole), a
`bindings/go/payments-ffi` cdylib, and a hand-written `include/net_payments.h`
are **not built** — don't promise them.

## Rust — the whole thing

`net-payments` (this skill's `provider.md` / `caller.md` cover it). The mesh
wire (`serve_payments`, `MeshPaymentChannel`), the MCP gate
(`gated_invoke` → `PaymentFlow` / `PaymentAdmission`), and the publish-side
price setter (`sdk/src/tool.rs`: `ToolMetadataBuilder::pricing_terms(terms_json)`
→ `descriptor.pricing_terms`, announced opaquely under
`pricing_terms_metadata_key(tool_id)`) all live here.

## Python — the only binding with a native flow

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

Caveats to remember (state them if the user hits them):

- The Python **tool/publish** surface (`net/tool.py`, re-exported by
  `net_sdk.tool`) has **no pricing field.** Python sees pricing only through
  `CapabilityGateway.describe()`, never on the publish side. `sdk-py` has no
  payments module.

## Node / TS — pricing passthrough on read only

- **No gateway, no `PaymentFlow`, no `gated_invoke`, no `net-payments`
  dependency.** `bindings/node/` doesn't register a `capability_gateway`
  module.
- **Pricing is a read-only discovery field:** `ToolDescriptor.pricingTerms?:
  string` (canonical `net.pricing.terms@1` JSON), surfaced by
  `listTools`/`watchTools`. **The publish side (`ToolOptions`) has no pricing
  field** — Node can't attach a price through the SDK.
- **`@net-mesh/payments` does not exist** in this repo — it's referenced only
  in a doc comment. Don't point a user at it.

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
