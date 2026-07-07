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
| **Go** | ❌ | ❌ | ❌ | ❌ | ❌ (none) |

Only **Rust and Python** have a native payment flow. Node has pricing as a
read-only discovery field and a golden-vector verifier; Go is entirely absent
from the payments surface.

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
    payment_signer=None,                # callable (typed_data_json: str) -> "0x..." (65-byte EIP-712 sig)
)
gw.describe(cap_id)                     # JSON string; includes "pricing_terms" (null = free)
gw.invoke(cap_id, arguments_json="{}")  # JSON string; status discriminant
```

`AsyncCapabilityGateway` has the same surface. Key facts:

- **Methods return a structured JSON *string* with a `status` discriminant —
  they never raise on a gate outcome.** `invoke` can return
  `status="requires_payment_approval"` with `{cap_id, quote_id, policy_reason,
  approve_hint}` (mirrors `GatedOutcome::RequiresPaymentApproval`). `describe`
  carries the announced `net.pricing.terms@1` JSON under `pricing_terms`.
- **Payments wiring** builds a `CallerPaymentFlow` over `SpendPolicyEngine`,
  `default_registry_v1`, and `MeshPaymentChannel`. `payment_profile` maps to
  `SpendProfile`. The payment identity is the **node's mesh ed25519 identity**
  (`mesh.entity_keypair()`), borrowed in-process.
- **The signer never sees a key.** `payment_signer` is a Python callable
  `(typed_data_json) -> signature_hex`, bridged into `ExternalSigner` under
  scheme `eip155`. Only the typed `eth_signTypedData_v4` doc and the resulting
  signature cross the boundary — doctrine 7/8 holds at the language edge. The
  two signer kwargs are **both-or-neither** and **require `payment_policy_path`**
  (the shared spend-policy store); the callable is validated as callable at
  construction, not on first invoke. It runs on a **blocking worker thread**
  (`spawn_blocking` + `Python::attach`) so the GIL never stalls the mesh
  reactor.
- **Fail-closed when payments is compiled out:** if the `payments` feature is
  off, passing any payment kwarg **raises `ValueError`** — never a silent free
  serve.

Caveats to remember (state them if the user hits them):

- **The gateway wires the signer under `eip155` only.** Even though the Rust
  crate now has an SVM signer seam (`ExternalSvmSigner`, `signer.md`), the
  Python gateway registers only the EVM signer — **solana settlement from
  Python is not yet wired** (needs an SVM-signer bridge in the binding).
- The Python **tool/publish** surface (`net/tool.py`, re-exported by
  `net_sdk.tool`) has **no pricing field.** Python sees pricing only through
  `CapabilityGateway.describe()`, never on the publish side. `sdk-py` has no
  payments module.
- Still pending for Python: a surface for the **outbound HTTP-402 client**
  (`X402HttpFlow` exists in Rust; no Python wrapper yet).

(Both landed 2026-07-06 and are now current in this skill: the payment identity
is the **node's mesh identity** via `Mesh::entity_keypair()`, and the
`_net.pyi` stub documents the `payment_signer_address` / `payment_signer`
kwargs — the stub no longer lags the impl.)

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

## Go — absent

The Go SDK has zero pricing/payment references, no publish-side pricing, and
**no golden-vector verifier** (despite what an old plan line implies — the
`go/payments_golden_vectors_test.go` file does not exist). If a user needs Go
payments, the honest answer is: not built; use Rust or Python.

## The one invariant every binding upholds

x402 documents are always carried as base64 of preserved bytes and **never
re-serialized through a binding's own JSON encoder.** The golden-vector
verifiers in each language exist precisely to prove byte-preservation holds
across the language boundary — that's their whole job (`testing.md`).
