# Implementation Plan: Payments — language SDK surfaces (Rust · TypeScript · Python · Go · C)

**Implements:** the multi-language half of [`PAYMENTS_SDK_PLAN.md`](PAYMENTS_SDK_PLAN.md) ("Ships as `net-payments` (Rust core) + `net_payments` (Python) + `@net-mesh/payments` (TS)"), extended to Go and C. Subsumes the deferred N4a scoping item (Python HTTP-402 client surface). Builds on everything through the failure schematic ([`PAYMENTS_FAILURE_SCHEMATIC_PLAN.md`](PAYMENTS_FAILURE_SCHEMATIC_PLAN.md)) — the `failure` projection is part of the surface every language gets.

**The sentence:** every language speaks the same payment surface — the lifecycle decided once in Rust, each binding marshaling it in its own house style: constructor kwargs and status-JSON in Python, prefix-errors and JSON strings in Node/TS, handles and colon-delimited kinds over a versioned C ABI for Go and C.

---

## Ground truth (as surveyed 2026-07-08)

| Language | Payments today | Binding house style (load-bearing receipts) |
|---|---|---|
| **Rust** | Full reference surface: demand (`CallerPaymentFlow::run`, `X402HttpFlow::fetch_paid`, `SpendPolicyEngine` incl. `approve/reject/pending/spent_today`, `SchemeSigner` + `External{,Svm,Xrpl}Signer`), provider (`PaymentEngine`, both gates, `serve_tool_paid`, registry, packs, checkers, `BillingLog`) | `net-payments` crate doctrine: one-way dependency (core/SDK never depend on payments); features `mesh`/`mcp-gate`/`http-facilitator`; no defaults |
| **Python** | The only live surface: 5 `CapabilityGateway` kwargs (`payment_policy_path/profile/unsafe_mock_auto_allow/signer_address/signer`), `requires_payment_approval` + `failure` passthroughs, eip155-only signer callback | Doctrine #1 "no logic in bindings"; "results are structured, never exceptions" (status-discriminant JSON strings); sync/async duals sharing `do_*` bodies (`src/README.md` checklist); H8 keys-stay-in-Rust; "no arbitrary signing oracle" (`capability_gateway.rs:453-460`) |
| **TS/Node** | None. No gateway class at all (`gated_invoke` unbound); consent primitives + shared pin store ARE bound; `pricingTerms` rides as an opaque display string pointing at a `@net-mesh/payments` that doesn't exist | Stable error prefixes + `errors.ts` typed classes (`nrpc:<kind>: <detail>`); structured payloads as JSON strings; u64→BigInt; vitest primary, Rust unit tests assert format strings only (napi Drop can't link under cargo test) |
| **Go** | None (golden vectors only — `payments_golden_vectors_test.go`: "no payments binding exists yet — logic never lives in bindings") | Sibling `*-ffi` cdylib crates; `Box`→`*mut T` + `_free` exactly once; `(ptr,len)` UTF-8; `ffi_guard!`/`catch_unwind` at every entry; colon-delimited error kinds via `format_rpc_error`; ABI stamp + `check_abi_version`; dispatcher-callback pattern with pre-registered ids |
| **C** | None — but the C ABI is a real standalone SDK: hand-written canonical headers in `include/` ("the canonical drop-in for C / C++ / Zig / Swift / Java JNI"), ABI versioning with changelog, header-drift regression tests | Hand-maintained headers (no cbindgen); `net_rpc.h` doctrine; `nrpc:app_error:` typed-error wire contract shared napi/pyo3/Go |

Two structural facts shape everything below:

1. **`net-payments` is a pure rlib with no FFI**, and the dependency doctrine (core/SDK never depend on payments) means a C surface must be a **new sibling `payments-ffi` cdylib** (`libnet_payments` + `include/net_payments.h`) — never folded into `libnet`/`libnet_rpc`.
2. **Cross-language conformance already has a mechanism**: `tests/cross_lang_payments/payment_vectors.json`, verified today by Rust + Python + TS + Go golden-vector tests (canonical JSON, ed25519, x402 byte-preservation). New surface work extends these vectors; it never invents a second mechanism.

## Doctrine (holds in every language, no exceptions)

- **No logic in bindings.** The lifecycle — describe → validate → consent → spend policy → pay → invoke, and the HTTP-402 door — is decided once in Rust (`gated_invoke`, `CallerPaymentFlow`, `SpendPolicyEngine`, `X402HttpFlow`). Bindings build the flow, marshal arguments, and project results.
- **Non-custodial; keys never cross the language boundary.** The only thing a language surface may be asked to sign is a logged, typed document (EIP-712 typed data, SPL transfer intent, XRPL payment intent) via a per-scheme callback seam — typed intent in, signature out. No raw-bytes path, no arbitrary signing oracle. Key material is unrepresentable in every binding.
- **Byte-preservation.** x402 material crosses every boundary as opaque strings/bytes (`X402Carry` base64, `net.pricing.terms@1` canonical JSON). No language-native JSON round-trip, ever — the golden vectors enforce this per language.
- **Structured results, never exceptions, one vocabulary.** Payment outcomes are status-discriminant objects (`ok` / `requires_payment_approval` / `denied` / …) with the `failure` field carrying `net.payment.failure@1` where a provider attached one — unknown reasons/fields tolerated, per the schematic's `@1` contract. Transport/programming errors keep each binding's native error idiom (exceptions in Python, prefix-classified errors in TS, `c_int` + out-string in C).
- **Fail-closed defaults.** `production` spend profile unless overridden; no flow configured ⇒ a paid capability is a structured `denied`, never a silent unpaid serve; payments feature absent ⇒ payment arguments are a loud construction-time error.
- **Wire vocabulary is single-sourced.** Header names, `ERR_PAYMENT`, the schematic tag and value vocabularies live in `net_sdk::tool_payment` / `net-payments`; language surfaces re-export or transcribe under drift tests — never redefine.
- **The provider side stays in Rust (v1).** `PaymentEngine`, the gates, and billing are one implementation of a money-path state machine; a per-language engine would fork the invariants the last three review cycles hardened. Non-Rust providers front a Rust daemon/gateway. (Entry criteria to revisit: a concrete non-Rust provider demand + the conformance suite of WS-X proving lifecycle parity is testable.)

## The surface contract (parity matrix)

The demand-side surface every language converges on ("—" = out of scope for that tier):

| Capability | Rust | Python | TS | Go | C |
|---|---|---|---|---|---|
| `pricing_terms` passthrough on describe | ✅ | ✅ | ✅ (string on descriptor) | WS-G | WS-G |
| Consent-gated paid invoke (`gated_invoke` path) | ✅ | ✅ | WS-T1 | WS-G | WS-G |
| `requires_payment_approval {quote_id, policy_reason, approve_hint}` | ✅ | ✅ | WS-T2 | WS-G | WS-G |
| `failure` (`net.payment.failure@1`) on denials | ✅ | ✅ | WS-T2 | WS-G | WS-G |
| Spend-policy config (path, profile, limits) | ✅ | partial (kwargs only) | WS-T2 | WS-G | WS-G |
| Approval verbs (`approve/reject/pending/spent_today`) | ✅ | ✅ | WS-T2 | WS-G | WS-G |
| Outbound HTTP-402 (`fetch_paid`) | ✅ | WS-P2 | WS-T3 | deferred | deferred |
| Signer seam: eip155 typed-data callback | ✅ | ✅ | WS-T2 | WS-G | WS-G |
| Signer seams: svm / xrpl intents | ✅ | WS-P3 | deferred | deferred | deferred |
| Golden-vector conformance | ✅ | ✅ | ✅ | ✅ | WS-C1 |
| Provider engine / gates / billing | ✅ | — | — | — | — |

## Workstream R — Rust: pin the contract (mostly prose)

The reference surface exists; the work is making it *mirrorable*.

- [x] Commit the parity matrix above as the contract, cross-referenced from `payments/src/lib.rs`'s module doc ("Language SDK surfaces — the parity contract") — a language surface is "done" when its column matches.
- [x] Extend `payment_vectors.json` with failure-schematic vectors: a valid schematic (decode + field access), an unknown-reason/extra-field tolerance case, and malformed cases (must be treated as absent). Landed as `failure_schematic_vectors` (generated from the real `net_sdk::tool_payment::FailureSchematic` by `gen_payments_fixtures`, so the bytes ARE what a producer emits): six cases — `valid_already_redeemed`, `unknown_reason_and_extra_fields` (reserved reason + preserved extras), `foreign_major_version_tag` (`@2` ⇒ absent), `malformed_json`, `not_an_object`, `invalid_utf8`. Each case carries `header_utf8`/`header_base64` + `accepted` + (for accepted) `expect{...}`; every language runs the same tolerant predicate — decode UTF-8 JSON, accept iff an object tagged `net.payment.failure@1` (mirrors `FailureSchematic::from_header_bytes`) — pinned in all four suites (Rust source-of-truth via the real type + byte-stable re-emission; Python/TS/Go via the predicate).
- [x] Audit the demand-side API for FFI-hostility. No API changes needed; the marshaling story per entry point, recorded:
  - **Handle in, JSON/bytes out.** `CapabilityGateway` (native) and `X402HttpFlow` are `Arc`-held handles behind an opaque pointer/pyclass; a language never sees a trait object or a lifetime. `search`/`describe`/`invoke` take a cap-id string + args JSON string and return a status-discriminant JSON string (`GatedOutcome` → the `ok`/`requires_payment_approval`/`denied`/… projection). `X402HttpOutcome` projects the same way; its body is bytes.
  - **Async is hidden behind the runtime seam.** The lifecycle is `async` in Rust; each binding owns the reactor bridge (Python `spawn_blocking`/mesh-runtime spawn, Node `ThreadsafeFunction`, Go dispatcher, C blocking call). The boundary itself is synchronous JSON/bytes — no `Future` crosses it.
  - **Signer seam is a per-scheme callback.** Typed-intent JSON in → signature string out; no raw-bytes path, key material unrepresentable (doctrine 2). Bridged via each binding's proven callback pattern.
  - **x402 material and the schematic are opaque.** `X402Carry` base64 and `net.pricing.terms@1`/`net.payment.failure@1` cross as strings/bytes, never re-serialized through a language-native type (byte-preservation; the vectors enforce it).
  - Nothing crossing a boundary is anything but a handle, a JSON string, or bytes — `CallerDecision`/`X402HttpOutcome`/`SpendDecision`/`GatedOutcome` all already serialize.

**Acceptance:** the matrix is committed prose (crate module doc + this plan); the extended vectors run green — Rust + Python locally, Go compiles (cdylib link is CI-only), TS is CI-run (vitest, no local `node_modules`).

## Workstream P — Python: complete the demand surface (house style: kwargs + status-JSON + Async duals)

Python is closest to done and unblocks Hermes. Everything lands per the binding's own rules: sync/async duals sharing `do_*` bodies (the `src/README.md` checklist governs), structured JSON-string results, loud `PyValueError` on misuse, stub + `__init__.py` re-export + drift tests.

- [x] **P0 — stub drift fix**: documented the `failure` field (shape + branch-on-reason contract) in `_net.pyi`'s `CapabilityGateway` invoke-result docstring (the async dual defers to it).
- [x] **P1 — approval verbs**: `approve_payment(quote_id)` / `reject_payment(quote_id)` / `pending_payments()` / `spent_today(network, asset)` on both gateway classes (sync + async duals sharing feature-split `do_*` bodies), thin wrappers over `SpendPolicyEngine` — the store, lock protocol, and Pending→Approved transition stay in Rust. Retain `payment_policy_path` + profile on the gateway to reopen the shared store; results as status-JSON (`ok` + `changed`/`pending`/`spent`, or structured `no_payment_policy` / `unsupported` / `error`). Closes the `approve_hint` loop: an agent resolves its own `requires_payment_approval` under operator policy. Rust driven tests (`--features payments`) + pytest rows (both duals) + stub entries.
- [ ] **P2 — HTTP-402 client** (subsumes deferred N4a): `PaymentHttpClient` (+ `AsyncPaymentHttpClient`) over `X402HttpFlow::fetch_paid` — same payment kwargs as the gateway, returns the status-JSON projection of `X402HttpOutcome` (`fetched` / `paid` / `requires_payment_approval` / `denied` / `provider_refused` / `transport_error`), body as bytes. Feature-gated with `http-facilitator`.
- [ ] **P3 — svm/xrpl signer seams**: `payment_signer_svm` / `payment_signer_xrpl` kwargs mirroring the eip155 contract (typed intent JSON in → signed artifact string out), bridged to `ExternalSvmSigner`/`ExternalXrplSigner` under the same `spawn_blocking` + `Python::attach` pattern. Absent kwarg = that namespace is skipped at selection (existing Rust semantics, no new policy).
- [ ] Tests: pytest rows per verb (both duals), the no-key-material negative test extended to the new signers, stub-coverage tests pick up the new classes automatically.

**Acceptance:** an agent embedding the Python SDK can complete the full demand lifecycle — discover a price, attempt, get `requires_payment_approval`, approve under policy, retry to `ok`, and read a `failure.reason` on a denial — plus pay a 402 URL, without leaving Python or seeing a key.

## Workstream T — TypeScript/Node: gateway first, then payments (house style: prefix errors + JSON strings)

Node's gap is one layer deeper than payments: there is no capability gateway at all. Payments rides in behind it. Package decision, recorded: **ship inside `@net-mesh/core` behind a `payments` Cargo feature** (one cdylib, one runtime — the Python precedent), deviating from `PAYMENTS_SDK_PLAN.md`'s `@net-mesh/payments` name; a scoped npm re-export package can adopt that name later without a second native module. Correct `tool.ts`'s dangling pointer when T2 lands.

- [ ] **T1 — CapabilityGateway parity**: bind `MeshGateway` + `gated_invoke` (`search`/`describe`/`invoke`) as `CapabilityGateway` over the *already-bound* shared pin store. Results as JSON strings with the status vocabulary (the binding's convention for discriminated shapes); errors via a new `gateway:`-prefixed class in `errors.ts` only for transport/programming failures — outcome statuses are data, not throws.
- [ ] **T2 — payment options**: constructor options object mirroring Python's kwargs (`paymentPolicyPath`, `paymentProfile`, `paymentUnsafeMockAutoAllow`, `paymentSignerAddress`, `paymentSigner`), signer as a JS callback bridged via `ThreadsafeFunction` (the established handler pattern — typed-data JSON string in, `Promise<string>` hex out); `requires_payment_approval` + `failure` pass through untouched; approval verbs as in P1. New `payments` feature forwarding to `net-payments` (`mcp-gate`, `mesh`).
- [ ] **T3 — HTTP-402 client**: `PaymentHttpClient` over `X402HttpFlow`, same shape as P2.
- [ ] Tests: vitest e2e per outcome status; Rust unit tests only for pure marshaling (format strings — the napi cargo-test linking limit is doctrine); the existing `payments_golden_vectors.test.ts` gains the WS-R schematic vectors.

**Acceptance:** the Python acceptance sentence, in Node — and `ToolDescriptor.pricingTerms`' doc pointer no longer names a package that doesn't exist.

## Workstream G — Go over a new C ABI (house style: rpc-ffi doctrine verbatim)

One new sibling crate, `bindings/go/payments-ffi` → `libnet_payments`, plus the Go wrapper package. Everything per the rpc-ffi rulebook: `Box`→`*mut T` + matching `_free` exactly once (idempotent on NULL); `(ptr,len)` UTF-8 in; `ffi_guard!` catch_unwind at every entry; its **own ABI stamp** starting `0x0001` with `net_payments_check_abi_version`; colon-delimited error kinds.

- [ ] **Surface shape — JSON-in/JSON-out deliberately**: the gateway/flow entry points take and return JSON strings (`net_payments_gateway_invoke(handle, cap_id, args_json, …, out_json, out_err)`), reusing the exact status vocabulary. This keeps the C signature count small and stable while the vocabulary (which is additive-tolerant by design) evolves — the schematic's own forward-compat rules do the version-skew work.
- [ ] Entry points (first cut): flow construction from config JSON (policy path, profile, signer addresses) + `_free`; gateway describe/invoke; approval verbs; `fetch_paid` (deferred if reqwest-in-cdylib sizing objects — record either way); signer **dispatcher** registration (`net_payments_set_signer_dispatcher` + per-flow `signer_id`, the pre-registration pattern from rpc-ffi — typed-intent JSON in, malloc'd signature string out, Rust frees).
- [ ] Go wrapper in the reference tree (`bindings/go/net/payments.go`) as the canonical contract, mirrored downstream like `mesh_rpc.go`; error kinds surfaced via `errors.As`-able types keyed on the colon-delimited kind.
- [ ] Tests: the 39-test rpc-ffi Rust-unit idiom for marshaling; Go golden vectors extended with WS-R's schematic rows; a Go lifecycle test against the mock facilitator (the same fixture world `mcp_gate_composition` uses, reached through the FFI).

**Acceptance:** a Go agent completes the demand lifecycle through `libnet_payments` with no payment logic in Go; the ABI-drift regression test covers the new header.

## Workstream C — C: the header IS the SDK

- [ ] **C1 — `include/net_payments.h`**: hand-written canonical header (no cbindgen — house rule), documenting build/link lines, ownership, the ABI stamp, the JSON result contract with the status vocabulary, and the signer-dispatcher contract. Covered by the header-drift regression test like its siblings.
- [ ] `include/README.md` section: the demand-lifecycle walk-through in C (construct flow → invoke → parse status JSON → approve → retry → free), with the fail-closed and key-custody doctrine stated for non-Go consumers (Zig/Swift/JNI per the `net_rpc.h` audience).
- [ ] Golden vectors: a tiny C test harness (or the Go tests standing in, recorded explicitly) verifying the schematic tolerance rows through the C surface.

**Acceptance:** a C consumer needs only the header + cdylib to run the lifecycle; the header passes the drift test.

## Workstream X — cross-cutting conformance + CI

- [ ] The WS-R vector extension lands first; every language's golden suite consumes it (Rust, pytest, vitest, `go test`, C harness).
- [ ] One **lifecycle conformance script** per language against the mock facilitator (quote → approval-required → approve → pay → served; denial → `failure.reason`), asserting the same status sequences — the runtime twin of the encoding vectors.
- [ ] CI: `payments` feature added to the python-tests maturin build (already present), the node build features, and new jobs for `payments-ffi` (cargo test) + Go payments tests.

## Rollout order

1. **R** (contract + vectors — small, unblocks everything).
2. **P** (P0 immediately; P1–P3 next — closest surface to done, Hermes consumer waiting).
3. **T1** then **T2/T3** (the gateway is the long pole; payments options are mechanical after it).
4. **G + C** together (the header and the FFI crate are one artifact reviewed twice).
5. **X** rides each landing.

## Non-goals

Provider engine / gates / billing outside Rust (doctrine above, entry criteria pinned); browser/wasm TS (napi is Node-only); a second native module for `@net-mesh/payments`; svm/xrpl signer seams beyond Python in v1 (deferred per matrix); per-language facilitator clients (the HTTP client lives in Rust behind `fetch_paid`); any new scheme/network (the ladder governs); custody, invoicing, tax — the category line from `PAYMENTS_SDK_PLAN.md` stands.

## Risks

| Risk | Containment |
|---|---|
| Signer callbacks deadlock across runtimes (GIL / napi TSFN / cgo) | Each binding already has a proven pattern for exactly this: Python's `spawn_blocking` + `Python::attach` (shipping today), Node's `ThreadsafeFunction` handler bridge, Go's dispatcher + pre-registered ids. Payments reuses; never invents. |
| C ABI churn as the payments surface evolves | JSON-in/JSON-out keeps signatures stable; vocabulary evolution rides the schematic's additive-tolerance rules; separate ABI stamp so payments never forces an rpc-ffi bump |
| Status vocabulary drifts between languages | Single-sourced constants + the WS-R vectors as executable contract; per-binding drift tests (stub coverage, abi_stability, header scan) extended to payments |
| TS package-name deviation surprises `PAYMENTS_SDK_PLAN.md` readers | Decision recorded here and cross-referenced there when T2 lands; the npm name stays reservable as a re-export |
| A binding accidentally grows payment logic (the standing temptation) | Review rule: a payments PR touching `bindings/` may contain marshaling only; anything resembling a decision cites the Rust function it defers to |
| `payments-ffi` cdylib + `libnet` feature-set mismatch corrupts shared types (the documented rpc-ffi hazard) | Same containment as rpc-ffi: the feature list is pinned in the crate manifest with the warning comment; the abi_stability tests load both |
