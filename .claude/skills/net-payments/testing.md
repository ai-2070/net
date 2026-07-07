# Testing — golden vectors, conformance, the negative tests

Three test surfaces guard payments: **cross-language golden vectors** (byte
equality), the **facilitator conformance suite** (mock ≡ HTTP), and the
**key-invariant negative test** (no binding can reach raw signing). Plus the
env-gated live testnet run. Know which one you're touching before you write.

## Cross-language golden vectors (`tests/cross_lang_payments/`)

One fixture, multiple verifiers, updated in lockstep. **Rust is the source of
truth.**

| Verifier | Path |
|---|---|
| Rust (source of truth) | `payments/tests/payments_golden_vectors.rs` |
| Node | `bindings/node/test/payments_golden_vectors.test.ts` |
| Python | `bindings/python/tests/test_payments_golden_vectors.py` |
| Go | *none* (Go is absent from payments) |

Regenerate the fixture (never hand-edit `payment_vectors.json`):

```
cargo run -p net-payments --example gen_payments_fixtures
```

The emitter is fully deterministic (fixed seeds, fixed timestamps, RFC 8032
ed25519), so `git diff` after regenerating **is** the drift detector.

What the vectors pin:

- **Envelope canonicalization** — every envelope has exactly one canonical
  byte encoding: one JSON object, keys sorted bytewise (known *and* unknown),
  compact separators, raw UTF-8, integers only (floats hard-fail). Signed
  payload = canonical bytes with the top-level `signature` key **absent**;
  signatures are ed25519 by the envelope's signer identity.
- **x402 byte-preservation** — captured v2 fixtures under `fixtures/x402/v2.0/`
  survive every binding byte-identically. x402 travels as base64 of original
  bytes, never re-serialized JSON. **This is the whole reason the non-Rust
  verifiers exist.**
- **CAIP confusion** — chain/asset grammar + distinct-but-confusable pairs;
  comparison is exact and case-sensitive (a verifier that case-normalizes or
  trims **fails** these).
- **Decimals mismatch** — declared-and-mismatched decimals hard-reject;
  unregistered assets hard-reject.
- **Unknown-field preservation** — a quote carrying fields no v1 reader knows;
  they sort into canonical position and the signature covers them (stripping
  them breaks verification).

Verifier-author notes: envelope keys are ASCII, so default string sort agrees
with bytewise order (sort by UTF-8 bytes explicitly if a non-ASCII key ever
appears). Fixture timestamps stay below 2^53 so JS `JSON.parse` round-trips
losslessly. Python: `json.dumps(v, sort_keys=True, separators=(",",":"),
ensure_ascii=False)`. Node: recursive writer, rejects non-`Number.isSafeInteger`,
ed25519 via `node:crypto` with the fixed SPKI DER prefix
`302a300506032b6570032100`. Keep vector strings free of `\b`, `\f`,
U+2028/U+2029 (language escapers differ there).

## Version pinning of fixtures

x402 fixtures are pinned **per spec revision** — `fixtures/x402/v2.0/…`, never
"latest". New spec revisions **add** fixture sets; they don't replace them.
Pinned revision: x402-foundation/x402 commit `087922a5eecc06ea773636b75df205814ba295b5`
(2026-05-29). `terms_hash` covers the version tag → no cross-version replay.

## Facilitator conformance suite (`payments/tests/`)

The design's acceptance test: **the mock and the HTTP client pass the
*identical* suite.** Key files:

- `lifecycle_modes.rs` — each `MockMode` (`Success`, `WrongAmount`,
  `LateFinality`, `ReorgInvalidate`, `Replay`, `ExpiredRequirements`,
  `VerificationTimeout`) has a test asserting the exact event chain. Reorg
  after serve freezes further serving against that quote.
- `http_facilitator_conformance.rs` — the same lifecycle suite parameterized
  over facilitator implementations, run against an in-process HTTP fixture
  server that speaks the spec (including its error vocabulary).
- `flow_end_to_end.rs`, `exact_scheme_flow_e2e.rs` — the caller flow through
  quote → payload → settle → serve.
- `mesh_payments_e2e.rs` — real two-node mesh (the P0 two-machine demo's
  shape), `serve_payments` + `MeshPaymentChannel`.
- `mcp_gate_composition.rs` — the `gated_invoke` composition with the payment
  gate.
- `spend_policy.rs` — auto-allow is silent; over-cap returns the structured
  error; approval unblocks; **two concurrent processes hammering `max_per_day`
  never overspend** (the loop test).
- `billing_stream.rs` — subscribe/read/export; idempotent retries republish
  nothing.
- `checker_verification.rs`, `eip155_checker.rs` — the independent-check tier
  upgrade (`observed` → `confirmed(n)` → `final`) and delivered-amount
  cross-check.
- `exact_evm_signing.rs` — EIP-3009 typed-data authoring + dev-signer digest.
- `exact_svm_scheme_flow.rs` — the exact-SVM seam: the paid lifecycle on an
  enabled solana network (wallet authors the base64 blob), and the structured
  refusal when no SVM wallet is registered.
- `adversarial_p1.rs` — facilitator-receipt replay, payload/requirements
  mismatch, CAIP network/asset confusion, amount/decimals per network.
- `http402_outbound.rs` — the outbound HTTP 402 two-way door.

Run the mock-only suite (no HTTP, no signer) — the default build:

```
cargo test -p net-payments
```

Feature-gated suites need their features:

```
cargo test -p net-payments --features http-facilitator          # HTTP client, checker, http402
cargo test -p net-payments --features http-facilitator,unsafe-dev-signer   # + EIP-3009 signing
cargo test -p net-payments --features mesh                      # two-node mesh e2e
cargo test -p net-payments --features mcp-gate                  # gate composition
```

## The key-invariant negative test (per binding)

Doctrine 8 demands a testable invariant: **no binding can accept, return,
serialize, log, or request raw signing of arbitrary bytes.** Each binding
carries a negative test proving key material is unrepresentable in its API —
`SchemeSigner` has no raw-bytes method (the SVM `sign_svm_transfer` also takes
a typed intent, never bytes), and the Python bridge only ever passes a typed
`eth_signTypedData_v4` doc + gets a signature back. The Python negative test
lives in `bindings/python/tests/test_capability_gateway.py` — it pins that no
gateway kwarg accepts key material, and that the signer kwargs are both-or-
neither and require the policy store. When you touch a binding's signer
surface, that negative test must still hold.

## Live testnet (env-gated, never in CI)

`live_testnet_conformance.rs` is `#[ignore]`d — real testnet money, opt-in
only. Full runbook (env vars, funding, the four checks) is in `networks.md`.
Never enable `unsafe-dev-signer` on mainnet.

## Writing your own tests — the fixtures that make it easy

- `MockFacilitator::new().with_default_mode(..)` / `.arm(requirements_hash, ..)`
  drives deterministic lifecycle outcomes with no chain.
- Inject a fixed `Clock` (`flow::Clock`, not `SystemClock`) to exercise expiry
  deterministically.
- `SpendProfile::DevTest` (or `.with_unsafe_mock_auto_allow(true)`) auto-allows
  the mock network so a test doesn't stall on approval — but never ship that
  posture into a demo that's meant to model production.
- `default_mock_registry(signer)` (`net-default-0`, mock only) keeps a test off
  the real-asset registry.
- Use `tempfile` for the engine/policy/billing store paths so tests don't
  collide on the per-user default paths.
