# Rust — payments

Read `../bindings.md` first. This page is only what is Rust-specific.

## Package and import

The crate is `net-payments`, imported as `net_payments`. Rust is the only
binding where payments is a first-class dependency rather than a projection
through the low-level package.

## What lives here

Everything. Rust is the source of truth and the other bindings hold references
into it:

- **The mesh wire** — `serve_payments`, `MeshPaymentChannel`.
- **The MCP gate** — `gated_invoke` → `PaymentFlow` / `PaymentAdmission`.
- **The publish-side price setter** — `net/crates/net/sdk/src/tool.rs`:
  `ToolMetadataBuilder::pricing_terms(terms_json)` → `descriptor.pricing_terms`,
  announced opaquely under `pricing_terms_metadata_key(tool_id)`.
- **The engine lifecycle** — `PaymentEngine`: quote → verify → settle → serve →
  bill. `provider.md` and `caller.md` cover it in full.
- **The spend policy engine** — `SpendPolicyEngine`, `SpendProfile`,
  `SpendLimits`.
- **The signer seam** — `SchemeSigner` and the external signer implementations.

## Rust-only surfaces

Two things exist here and nowhere else, and promising them in another language
is a correctness bug, not a shortfall:

- **Tiered verification.** The `observed / confirmed(n) / final` ladder and the
  independent `ChainChecker`, including reorg freeze and replay handling. No
  binding projects it. Node and Python can drive a payment to settlement but
  cannot independently ask the chain checker what it believes — treat a settled
  result there as the facilitator's claim.
- **The full facilitator trait.** Bindings reach the HTTP facilitator; the trait
  itself, the mock, and config packs are Rust.

## Where to look when this page is not enough

- **Authoritative source:** `net/crates/net/payments/src/`.
- **Conformance:** `net/crates/net/payments/tests/payments_golden_vectors.rs` —
  the source-of-truth verifier the other languages are checked against.
- **Chapters:** `provider.md`, `caller.md`, `facilitator.md`, `verification.md`,
  `spend-policy.md`, `signer.md`.

## Never infer from another binding

- Python and Node return a JSON **string** with a `status` discriminant from
  gate methods. Rust returns typed values and `GatedOutcome` variants — do not
  port the "parse the status field" pattern here.
- The binding gateways (`CapabilityGateway`, `PaymentProvider`) are binding-side
  constructs. Rust composes `PaymentEngine` and `CallerPaymentFlow` directly.
