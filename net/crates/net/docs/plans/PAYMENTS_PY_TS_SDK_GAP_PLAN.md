# Implementation Plan: Payments — the Python supply side + the Node/TS payment surface

**Implements:** the two remaining language gaps in the payments SDK parity
matrix (the surface contract in [`PAYMENTS_LANGUAGE_SDKS_PLAN.md`](PAYMENTS_LANGUAGE_SDKS_PLAN.md)):
**Python can pay but cannot price/charge** (the supply side), and **Node/TS has
no payment surface at all** beyond a read-only price at discovery. Supersedes
Workstream T of that plan
(the Node demand side) and extends both languages to demand+supply parity with
Rust; Go/C stay with that plan's WS-G/WS-C.

**The sentence:** Python gains the provider half it lacks (price a tool at
publish, charge over the mesh through a payment gate on the same engine the
quote/pay wire runs), and Node/TS gains the whole demand half Python already has
(gateway → caller flow → approval → signers → HTTP-402) — every new surface a
marshaling layer over the one Rust lifecycle, deciding nothing itself.

---

## Ground truth (as surveyed 2026-07-08)

| | Python | Node/TS |
|---|---|---|
| **Demand** (pay to invoke) | ✅ full `CapabilityGateway` (search/describe/invoke, `requires_payment_approval`, `failure`, approval verbs, eip155/svm/xrpl signers, `PaymentHttpClient`) | ❌ **nothing** — no `MeshGateway`/`gated_invoke`/gateway binding (confirmed by grep); only local consent/pin primitives bound (`node/src/consent.rs`) |
| **Supply** (price + charge) | ❌ `publish_tools` hard-wires `pricing: Default::default()`, `payment_admission` left `None` (`python/src/publish.rs:151`) — every published tool is free; no `PaymentEngine`/gate binding | ❌ no publish-side pricing; **no `publishTools`/`ServerPublisher` binding at all** |
| **Price at discovery** (read) | ✅ `describe()` carries `pricing_terms` | ⚠️ `watchTools` surfaces `pricingTerms`; **`listTools` omits it** (`ToolDescriptorJs` has no pricing field, `node/src/tool.rs:35`) — an asymmetry to fix |
| **Golden-vector verifier** | ✅ | ✅ (incl. `failure_schematic_vectors`) |

Two structural facts shape everything below:

1. **No dependency gaps — only binding-authoring gaps.** The Python provider
   path needs no new crate features: `net-payments` is already pulled with
   `["mcp-gate","mesh"]`, and `PaymentEngine`/`BillingLog`/`MockFacilitator`/
   `default_registry_v1` are all unconditionally compiled. Node already links
   `net-mcp` (default `mcp` feature), so `net_mcp::serve::{MeshGateway,
   gated_invoke, …}` is ready to bind; only `net-payments` must be added behind a
   new Node `payments` feature (mirroring Python's `payments = ["mcp","net",
   "dep:net-payments"]` with the dep's `["mcp-gate","mesh"]`).
2. **The composition already ships — twice.** The demand-side gateway is
   `MeshGateway::new(SdkMesh::from_node_arc(node, channel_configs, None))` over
   `gated_invoke`; Node's `compute.rs` already builds an `SdkMesh` that exact way
   from the same `NetMesh` accessors Python uses. The provider side is the MCP
   wrap `ServerPublisher::publish_tools(tools, invoker, ctx, config)` that
   Python's `publish.rs` **already calls** — it only ever passes empty pricing.
   Neither half is new machinery; both are marshaling the config in.

## Doctrine (unchanged — the crate's, restated at the edges)

- **No logic in bindings.** The lifecycle — quote → verify → settle → serve →
  bill (provider) and describe → consent → spend policy → pay → invoke (caller)
  — is decided in Rust. Bindings build the flow, marshal arguments, and project
  results.
- **Non-custodial; keys never cross the boundary.** The only thing a language
  surface signs is a typed document (EIP-712 / SPL intent / XRPL intent) via a
  per-scheme callback. No raw-bytes path. The **provider** signing identity is
  borrowed in-process, never handed in as key bytes (see Open Decision 1).
- **Byte-preservation.** x402 material and `net.pricing.terms@1` cross as
  opaque strings/bytes, never re-serialized through a language-native type.
- **Structured results, never exceptions, one vocabulary.** Payment outcomes are
  status-discriminant JSON (`ok`/`requires_payment_approval`/`denied`/…) with the
  `failure` field carrying `net.payment.failure@1`. Transport/programming errors
  keep each binding's native idiom.
- **Fail-closed defaults.** A paid capability with no gate configured is a
  structured `denied`, never a silent free serve; a priced publish with no
  payment admission is a loud construction-time error (mirrors Rust's
  `WrapError::PricedWithoutPaymentGate` / `ServeError::UnenforceablePricing`).
- **The provider engine stays one Rust implementation.** Python/Node providers
  front the *same* `PaymentEngine` state machine; no per-language engine.

## The parity target (what this plan closes)

| Capability | Rust | Python → | Node/TS → |
|---|---|---|---|
| Price at publish (`net.pricing.terms@1`) | ✅ | **A2** | **B5** |
| Provider payment gate (charge) — `serve_payments` + admission | ✅ | **A2** | **B5** |
| Billing read/stream | ✅ | **A3** | B5 |
| Consent-gated invoke (`gated_invoke`) | ✅ | ✅ | **B1** |
| `requires_payment_approval` + `failure` | ✅ | ✅ | **B2** |
| Caller flow + spend-policy config | ✅ | ✅ | **B2** |
| Approval verbs (`approve/reject/pending/spent_today`) | ✅ | ✅ | **B2** |
| Signer seams (eip155 / svm / xrpl) | ✅ | ✅ | **B2** |
| Outbound HTTP-402 (`fetch_paid`) | ✅ | ✅ | **B3** |
| Price at discovery — `listTools` | ✅ | ✅ | **B4** |

---

## Part A — Python: the supply side (price + charge)

Python already has the whole demand surface; it lacks the ability to *be* a paid
provider. All three of these ride the MCP wrap path `publish.rs` already uses —
they add pricing + a payment gate over one shared engine, nothing more.

### A1 — Author `net.pricing.terms@1` (the prerequisite)

A raw pricing string won't do: the announced terms are a canonical envelope
built from `PricingTerms::new(provider_entity_id, capability,
Vec<X402Carry<PaymentRequirements>>, registry.reference())` + `canonical_bytes`,
and a hand-written string fails the registry/reference checks. So the provider
surface needs a typed builder.

- [x] Bind `build_pricing_terms(provider_entity_id, capability, requirements_json)`
  (module `net._net`): `provider_entity_id` is the node's 32-byte mesh entity id
  (`mesh.entity_id` — public only, keys never cross), `requirements_json` is a
  JSON array of x402 `PaymentRequirements` (camelCase wire names: `scheme`,
  `network`, `amount`, `asset`, `payTo`, `maxTimeoutSeconds`, optional `extra`).
  Authors each through `X402Carry::author` (the sanctioned serialization point
  for locally-originated x402 — no byte-preservation violation) under
  `default_registry_v1` (signer-independent `reference()`, so it matches any
  caller's default registry), and returns the canonical `net.pricing.terms@1`
  JSON string. Fail-closed on empty/malformed/non-32-byte id (`ValueError`).
  Rust unit tests (canonical + decodable + multi-accept + rejects) + pytest +
  stub + `__init__` re-export.

### A2 — The payment provider: engine + wire + priced publish

One handle owns the whole provider side. Construction wires a single
`PaymentEngine` and stands up the quote/pay wire; publishing a tool attaches the
admission gate over that same engine.

- [ ] **`PaymentProvider` handle** (`net._net`), constructed from a started
  `NetMesh`:
  - Builds `PaymentEngine::new(provider_keypair, facilitator, admission,
    registry, state_path)` where **`provider_keypair` is the node's mesh
    identity** (`mesh.entity_keypair()`, borrowed in-process — consistent with
    the caller side's payment identity, H8-clean; see Open Decision 1),
    `facilitator` defaults to `MockFacilitator` (real settlement is the
    `payments-http` follow-up), `admission` defaults **fail-closed** (see
    below), `registry = default_registry_v1(entity_id)`, and `state_path` is a
    caller-supplied durable directory (the settlement store must survive
    restarts — never a temp path if quotes outlive the process).
  - Calls `serve_payments(&mesh, InProcessProvider::new(engine,
    Arc::new(SystemClock)))` and holds the returned `PaymentServeHandle` (drop =
    unregister) so callers can quote + pay against this node.
  - Optionally attaches `with_billing_log(BillingLog::new(path))` when a billing
    path is supplied (A3).
  - Exposes the engine's `EnginePaymentAdmission` to the publish path.
- [ ] **Priced publish**: extend `NetMesh.publish_tools(...)` (or add a
  `publish_paid_tools`) to accept `pricing: dict[str, str]` (tool name →
  `net.pricing.terms@1` JSON from A1) and a `payment_provider: PaymentProvider`.
  It sets `config.pricing = pricing` and `config.payment_admission =
  Some(provider.admission())` on the existing `WrapConfig` path
  (`publish.rs:141-167`), so a priced tool is gated by the same engine the wire
  serves. **Fail-closed guard:** non-empty `pricing` without a `payment_provider`
  is a construction-time `ValueError` (mirrors `WrapError::PricedWithoutPaymentGate`);
  a `payment_provider` with empty pricing publishes free tools normally.
- [ ] **Admission policy default is fail-closed, not `AdmitAll`.** `AdmitAll` is
  flagged in-source as "tests and dev harnesses only." The Python default admits
  based on the wrap config's owner scope (as free tools do today); an explicit
  opt-in exposes a broader policy. Never default a paid provider to admit
  everyone.
- [ ] Tests: a driven Rust test (feature `payments`) standing up a
  `PaymentProvider`-shaped node that prices + serves a tool once and bills one
  event (the composition `mesh_paid_capability_e2e.rs` / `mcp_wrap_paid_e2e.rs`
  prove, reached through the binding helpers); pytest rows for the fail-closed
  guard + a paid publish handle; stub + `__init__` re-export + drift tests.

### A3 — Billing read surface

A provider that charges must see what it charged.

- [ ] Bind `BillingLog` read/stream from Python (`read_all` / a tail iterator,
  sync + async), returning the immutable `net.billing.event@1` records as JSON
  strings. Doctrine holds: billing is emitted by the engine; the binding only
  reads. No mutation surface.
- [ ] Tests: pytest asserting a paid serve appears as exactly one billing event
  (idempotent retries republish nothing).

**Acceptance (Part A):** a Python node can price a tool at publish, serve it
paid across the mesh (quote → pay → gate → serve → bill) on the same engine, and
read its own billing stream — a Python *provider*, not just a Python payer.

---

## Part B — Node/TS: the demand surface (then supply)

Node's gap is a layer deeper than payments: there is no capability gateway.
Payments ride behind it. Package decision (recorded, unchanged from
`PAYMENTS_LANGUAGE_SDKS_PLAN.md`): ship inside `@net-mesh/core` behind a Cargo
`payments` feature — one cdylib, one runtime, the Python precedent.

### B1 — `CapabilityGateway` (the demand core)

- [x] Added `net-payments` to `node/Cargo.toml` behind a new `payments` feature
  (`payments = ["mcp", "net", "dep:net-payments"]`, dep features
  `["mcp-gate","mesh"]`; `payments` in `default`). `net-mcp` was already linked,
  so `net_mcp::serve::{MeshGateway, gated_invoke, CapabilityDetail, GatedOutcome,
  GatewayError, CapabilityId, PaymentFlow, ConsentPolicy, PinStore}` and
  `net_sdk::mesh::Mesh` are available; `consent` already pulls `dep:net-sdk`.
- [x] New `node/src/capability_gateway.rs`: a **single async napi class**
  `CapabilityGateway` (napi auto-Promises `async fn` — no sync/async split, no
  per-instance runtime field). Built as `MeshGateway::new(Arc::new(
  SdkMesh::from_node_arc(mesh.node_arc_clone()?, mesh.channel_configs_arc(),
  None)))` — the `compute.rs`/`DaemonRuntime::create` pattern. `search` /
  `describe` / `invoke` resolve to **JSON strings** with the status vocabulary,
  driving `gated_invoke` + a byte-for-byte mirror of Python's `outcome_to_json`
  (including the `failure` schematic projection on denials). `payment: None` in
  B1 (a paid capability fails closed as `denied`); the flow arrives in B2. Each
  method clones the `Arc`s from `&self` before the await (the `PinStore` pattern).
- [x] Extended the `node_arc_clone` / `channel_configs_arc` `cfg` gates
  (`node/src/lib.rs`) to include `payments`.
- [x] Errors: outcome statuses are **data (JSON), not throws** — a malformed
  cap-id / arguments is a structured `invalid_capability_id` / `invalid_arguments`
  status, never a throw. The only throw is the constructor on a shut-down mesh,
  behind a new `gateway:`-prefixed `GatewayError` class in `errors.ts` +
  `classifyError` branch (matching the `nrpc:`/`cortex:` prefix doctrine).
- [x] **Runtime (Open Decision 2):** the gateway drives mesh I/O on napi's
  process-wide runtime, the same way `compute.rs`'s `DaemonRuntime` already does
  over a shared `MeshNode` — the precedent this leans on (verified at runtime by
  the vitest e2e in CI).
- [x] Tests: Rust marshaling tests (denied+schematic projection,
  requires_payment_approval — green under the `-undefined dynamic_lookup` napi
  test-link); a vitest e2e (`test/capability_gateway.test.ts`, mirrors the Python
  gateway basics — empty-mesh search, unreachable-provider structured errors,
  malformed id/args); `payments` added to the node-tests napi build + the node
  clippy matrix. napi build regenerates `index.d.ts` with the class.

### B2 — Payment options + signer seam + approval verbs

- [x] Constructor options `paymentPolicyPath` / `paymentProfile` /
  `paymentUnsafeMockAutoAllow` build a `CallerPaymentFlow` over
  `SpendPolicyEngine` + `default_registry_v1` + `MeshPaymentChannel` (the Python
  `build_payment_flow` composition), payment identity = the node's mesh identity.
  Fail-closed: a profile/unsafe flag without a policy path is a construction
  error; an unknown profile is a construction error; a paid capability with no
  flow is `denied`. Mock-network paid capabilities work with no signer.
- [x] `requires_payment_approval` + the `failure` schematic pass through
  untouched — the flow (over-cap) yields `requires_payment_approval`, projected
  by the B1 `outcome_to_json`.
- [x] Approval verbs `approvePayment` / `rejectPayment` / `pendingPayments` /
  `spentToday` (thin wrappers over `SpendPolicyEngine`, retaining the store path
  + profile on the gateway) — structured `no_payment_policy` without a path.
  Rust config tests + vitest round-trip rows.
- [ ] **B2-signers (follow-up):** the three signer pairs
  (`paymentSignerAddress`/`paymentSigner` eip155, `paymentSignerSvm*`,
  `paymentSignerXrpl*`) bridged via `ThreadsafeFunction<String /*typed intent
  JSON*/, Promise<String> /*artifact*/>` (the `node/src/blob.rs`
  `NodeAsyncBlobAdapter` + `await_tsfn_promise` pattern) → `ExternalSigner` /
  `ExternalSvmSigner` / `ExternalXrplSigner`. Needed only for REAL-network
  settlement; typed intent in, artifact out; key material unrepresentable; each
  pair both-or-neither. Split out because the TSFN signer bridge is intricate
  enough to warrant its own focused pass.

### B3 — Outbound HTTP-402 client

- [x] `PaymentHttpClient` over `X402HttpFlow::fetch_paid`, same shape as Python's
  `payment_http.rs`: `fetchPaid(url)` resolves to `[statusJson, body]` (the
  `X402HttpOutcome` projection — `fetched` / `paid` (base64 settlement) /
  `requires_payment_approval` / `denied` / `provider_refused` /
  `transport_error` — + the raw body as a `Buffer`). `paymentPolicyPath`
  required; ephemeral payer identity (bookkeeping on this path). Behind a Node
  opt-in `payments-http` feature (`net-payments/http-facilitator` + `base64`,
  kept out of the default `.node`; built in the vitest CI job). The flow is
  built lazily on the first `fetchPaid` (inside the async fn, so reqwest finds
  napi's reactor — the JS-thread constructor has none) and cached behind a
  `parking_lot::Mutex` (the `Arc` is cloned out before the await). Rust
  projection + profile tests + a vitest `transport_error` row. Real-network
  paid HTTP waits on the shared signer bridge (B2-signers).

### B4 — Close the `listTools` pricing asymmetry (small, independent)

- [ ] Add `pricingTerms?: string` to `ToolDescriptorJs` + `descriptor_to_js`
  (`node/src/tool.rs:35,52`) so `listTools()` surfaces the announced price like
  `watchTools()` already does. Pure read-side; no payments dependency. Pinned by
  the existing camelCase wire-JSON Rust test + a vitest row.

### B5 — Node supply side (deferred; entry criteria pinned)

Node has **no `publishTools`/`ServerPublisher` binding at all** — pricing at
publish requires first binding the publish path, then Part A's provider surface.
Larger than the demand work and not the highlighted gap.

- [ ] **Deferred.** Entry criteria: (a) a Node tool-publish binding exists
  (`publishTools` over `ServerPublisher`), and (b) demand parity (B1–B3) has
  shipped. When it lands it mirrors Part A (pricing at publish + a payment
  provider handle + billing read), reusing the same `PaymentEngine`/
  `EnginePaymentAdmission` wiring.

- [ ] Tests (B1–B4): vitest e2e per outcome status (search/describe/invoke,
  approval loop, `failure.reason`, the HTTP-402 statuses); Rust unit tests **only
  for pure marshaling** (format strings — the napi cargo-test linking limit is
  doctrine, `node/src/mesh_rpc.rs:2103`); the existing
  `payments_golden_vectors.test.ts` already carries the schematic vectors.

**Acceptance (Part B):** the Python acceptance sentence, in Node — discover a
price, attempt, `requires_payment_approval`, approve under policy, retry to `ok`,
read a `failure.reason` on a denial, and pay a 402 URL, without leaving Node or
seeing a key — and `listTools` reports the price `watchTools` already did.

---

## Part C — cross-cutting

- [ ] **One lifecycle-conformance vitest** against the mock facilitator (quote →
  approval-required → approve → pay → served; denial → `failure.reason`),
  asserting the same status sequences Python's driven test does — the runtime
  twin of the golden vectors.
- [ ] **CI:** add the `payments` feature to the Node build + a payments vitest
  job; add the Python provider tests (`--features payments`) to the maturin
  build. The `payments-http` opt-ins ride their own feature line (both languages).
- [ ] The wire vocabulary stays single-sourced (`net_sdk::tool_payment` /
  `net-payments`); the `failure_schematic_vectors` remain the executable
  cross-language contract — no per-binding redefinition.

## Rollout order

1. **A1** (pricing-terms authoring) + **B4** (`listTools` price) — small,
   independent, unblock the rest.
2. **B1 → B2 → B3** — the Node demand surface (the long pole; the highlighted
   gap). Gateway first; payment options are mechanical after it.
3. **A2 → A3** — the Python provider path (price + charge + billing).
4. **B5** — Node supply, when its entry criteria are met.
5. **Part C** rides each landing.

## Non-goals

- Provider engine / gates / billing outside Rust — one money-path state machine;
  Python/Node front the same `PaymentEngine`. (Unchanged doctrine.)
- Real (non-mock) settlement in the default build — `MockFacilitator` is the
  default; real facilitators ride the opt-in `payments-http`/`http-facilitator`
  feature.
- A second native module for Node payments (`@net-mesh/payments` stays a
  reservable re-export name; ship inside `@net-mesh/core`).
- `serve_tool_paid` (the typed Rust path) from a binding — it's generic over
  `Req/Resp: Serialize`, which the untyped `(name, schema, callback)` binding
  model can't satisfy. Both languages use the MCP wrap `publish_tools` path.
- Any new scheme/network, custody, invoicing, or dynamic pricing — the category
  line stands.

## Risks

| Risk | Containment |
|---|---|
| **Provider payment identity ambiguity** (Open Decision 1) | The Rust e2e tests use a *separate* generated keypair; the caller side uses the node's mesh identity. Resolve to **the node's mesh identity for both** (H8-clean, consistent, discoverable) — flagged for payments-owner sign-off before A2 lands. |
| **napi shared-runtime vs. node reactor** (Open Decision 2) | `compute.rs`'s `DaemonRuntime` already drives node I/O on napi's runtime; confirm explicitly in B1 before building on it. |
| Authoring `net.pricing.terms@1` wrong (unsigned/uncanonical string) | A1 is a *typed* builder over `PricingTerms::new` + `canonical_bytes`, never a raw string; the golden vectors pin the shape. |
| A paid publish with no gate ships a free tool | Fail-closed construction-time error (mirrors `WrapError::PricedWithoutPaymentGate`); the engine is required, never defaulted to `AdmitAll` in production. |
| Signer callback deadlock across runtimes (napi TSFN) | Reuse the proven `blob.rs` `NodeAsyncBlobAdapter` TSFN-Promise bridge; never invent a new one. |
| Status vocabulary drifts Python↔Node | Single-sourced constants + the `failure_schematic_vectors` as executable contract; the conformance vitest asserts the same status sequences as Python's driven test. |
| Node supply (B5) scope creep | Deferred with explicit entry criteria; demand parity ships first. |

## Open decisions (resolve before the dependent workstream)

1. **The provider payment keypair.** Recommend the **node's mesh identity**
   (`mesh.entity_keypair()`, borrowed in-process) for both pay and charge —
   consistent with the caller side, H8-clean, and the identity callers already
   know from routing. The tests' separate keypair is isolation hygiene, not
   doctrine. Blocks A2.
2. **napi runtime affinity for `MeshGateway`.** Confirm `from_node_arc`-driven
   I/O is correct on napi's shared runtime (the `DaemonRuntime` precedent says
   yes). Blocks B1→B2.
