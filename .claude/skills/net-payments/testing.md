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
| Go | `go/payments_golden_vectors_test.go` (repo root, not `bindings/go/`) |

**All four run in lockstep** — adding a case means updating all four. (The
"Go has no verifier" claim in older notes was wrong; it exists.)

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
- **Failure-schematic tolerance** (`failure_schematic_vectors`) — the
  `net.payment.failure@1` header contract: a valid case, an unknown-reason +
  preserved-extras case, and the reject cases (foreign `@2` tag, malformed JSON,
  non-object, invalid UTF-8, **plus the structural rejects**: correct tag with no
  fields, missing the required `recovery`, a wrong-typed field, and a body with a
  non-standard JSON number like `Infinity`). Every language runs the *same
  tolerant predicate* — but it is **not tag-only**: it mirrors
  `FailureSchematic::from_header_bytes`, which does a full typed `serde`
  deserialize *before* the tag check. So the predicate is "decode as strict
  UTF-8 JSON (no `Infinity`/`NaN`) and accept iff the value has the full
  schematic shape (all required fields with the right types — `object`, `code`,
  `stage`, `reason`, `message`, `funds_moved`, `prior_payment` strings;
  `retryable`, `handler_executed` bools; `recovery` an object of `class`/`actor`
  strings + `safe_to_retry`/`safe_to_requote` bools) **and** `object` == the
  tag." A tagged-but-incomplete/mistyped object is rejected, same as Rust —
  non-Rust verifiers must validate the shape, not just the tag, or they drift.
  Rust decides via the real `FailureSchematic` (a `net-sdk` dev-dep) with
  byte-stable re-emission; the fixture is generated from that type
  (`failure-schematic.md`).

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
- `exact_svm_scheme_flow.rs` / `exact_xrpl_scheme_flow.rs` — the SVM and XRPL
  seams: the paid lifecycle on an enabled solana/xrpl network (wallet authors
  the blob), and the structured refusal when no wallet is registered.
- `svm_checker.rs` / `xrpl_checker.rs` / `eip155_checker.rs` — the independent
  per-namespace chain checkers (delivered-amount / invoice-binding cross-check).
- `native_tool_gate.rs` — `EngineToolPaymentGate` (SDK-native `serve_tool_paid`):
  redeem-exactly-once, fail-closed store errors, and the failure-schematic
  denial fields + the typed `tracing` emit (`failure-schematic.md`).
- `mesh_paid_capability_e2e.rs` — the canonical mega-e2e: a paid capability
  serves once across two real mesh nodes, discovering pricing over the wire.
- `mcp_wrap_paid_e2e.rs` — MCP wrap `publish_tools` + real `EnginePaymentAdmission`
  over two nodes.
- `adversarial_p1.rs` — facilitator-receipt replay, payload/requirements
  mismatch, CAIP network/asset confusion, amount/decimals per network.
- `http402_outbound.rs` — the outbound HTTP 402 two-way door.

Python-binding payment tests (driven Rust tests + pytest, in
`bindings/python/`): `capability_gateway.rs`'s test modules pin the
`outcome_to_json` projections, the approval-verb store round-trip, and a driven
paid invoke through the actual demand surface; `tests/test_capability_gateway.py`
+ `tests/test_payment_http.py` are the pytest twins (skip cleanly when a feature
is absent).

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

The Python-binding payment tests build the extension with the demand-side
features (they link libpython, so use a feature subset, not the extension-module
build):

```
cargo test -p net-python --no-default-features --features net,cortex,consent,mcp,payments             # gateway + approval verbs
cargo test -p net-python --no-default-features --features net,cortex,consent,mcp,payments,payments-http # + the HTTP-402 client
```

`payments-http` is an **opt-in** feature (not in the default wheel — it pulls
reqwest/rustls); the CI python-tests `maturin develop` enables it so
`test_payment_http.py` runs. Set `CARGO_INCREMENTAL=0` if the incremental cache
fills the disk on these heavy builds.

## The key-invariant negative test (per binding)

Doctrine 8 demands a testable invariant: **no binding can accept, return,
serialize, log, or request raw signing of arbitrary bytes.** Each binding
carries a negative test proving key material is unrepresentable in its API —
`SchemeSigner` has no raw-bytes method (the SVM `sign_svm_transfer` / XRPL
`sign_xrpl_payment` also take typed intents, never bytes), and the Python bridge
only ever passes a typed document (eip155 typed-data / svm / xrpl intent JSON) +
gets an artifact string back. The Python negative test lives in
`bindings/python/tests/test_capability_gateway.py` — it pins that **no** gateway
kwarg accepts key material (extended to the svm/xrpl signer namespaces), and
that every signer pair is both-or-neither and requires the policy store. When
you touch a binding's signer surface, that negative test must still hold.

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
