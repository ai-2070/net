# Implementation Plan: Node/TS delegation + A2A surface (Hermes parity)

**Status: DEFERRED — not scheduled.** Written 2026-07-09 to capture scope; no
code landed. Pick up when the Node SDK's Hermes surface is prioritized.

**Implements:** the last Node/TS gap versus Python in the Hermes surface —
delegated agent identity (`delegation`), device enrollment (rides the
`delegation` gate), and agent-to-agent task handoff (`a2a`). Node's payment
surface reached parity in [`PAYMENTS_PY_TS_SDK_GAP_PLAN.md`](PAYMENTS_PY_TS_SDK_GAP_PLAN.md);
this is the same move for the Hermes phases specified in
[`HERMES_INTEGRATION_PLAN_V2.md`](HERMES_INTEGRATION_PLAN_V2.md) (Phase 1
enrollment, Phase 3 delegation + A2A). Python already ships all three; the Rust
SDK (`net_sdk::{delegation, enrollment, devices, operator, mesh_a2a, a2a}`) is
the single implementation both bindings wrap.

**The sentence:** Node/TS gains the three Hermes subsystems Python already has —
`DelegationChain` + `RevocationRegistry` + child-identity derivation, the
invite → join → approve device-enrollment handshake, and serve/submit/status/
cancel A2A task handoff — every new surface a napi marshaling layer over the one
Rust lifecycle, deciding nothing itself and holding no key material.

**Why this is a port, not a flag:** `delegation` and `a2a` are **not features of
the node binding** (`net-node/Cargo.toml` has no such entries) and **no node
source is gated on them** (`grep 'feature = "(delegation|a2a)"' bindings/node/src`
is empty). "Enable for npm" therefore means authoring the wrapper modules, not
flipping a default. The underlying SDK surface is already available to the node
binding (it links `net-sdk` today via the `consent` feature).

---

## Ground truth (surveyed 2026-07-09)

| Subsystem | Python | Node/TS | Underlying SDK (available to both) |
|---|---|---|---|
| **delegation** | ✅ `delegation.rs` (298 lines): `derive_child_identity`, `RevocationRegistry`, `default_revocation_store_path`, `DelegationChain` | ❌ nothing | `net_sdk::delegation` |
| **enrollment** | ✅ `enrollment.rs` (688 lines): `fingerprint`, `InviteToken`, `JoinRequest`, `JoinOutcome`, `DeviceRecord`, `OperatorEnrollment`, `DeviceEnrollment`, `EnrollmentServeHandle` | ❌ nothing | `net_sdk::{enrollment, operator, devices}` |
| **a2a** | ✅ `a2a.rs` (192 lines): `TaskExecutor` bridge, `A2aServeHandle` | ❌ nothing | `net_sdk::{a2a, mesh_a2a}` |
| **NetMesh methods** | ✅ 13 gated: `rendezvous_string`, `join`, `serve_enrollment_auto`, `renew` (delegation); `serve_a2a`, `submit_task`, `task_status`, `cancel_task` (a2a); + async duals | ❌ none | — |
| **Cargo features** | `delegation = ["net","dep:net-sdk","net-sdk/net","net-sdk/cortex"]`, `a2a = ["delegation","cortex"]`, enrollment on the `delegation` gate | none | — |
| **Tests** | ✅ `test_delegation.py`, `test_enrollment.py`, `test_a2a.py` | ❌ none | — |

Two structural facts (mirroring the payments gap plan):

1. **No dependency gaps — only binding-authoring gaps.** Node already links
   `net-sdk` (behind the default `consent`/`mcp` features: `consent =
   ["dep:net-sdk"]`). The delegation/a2a features only need to turn on the SDK's
   own `net`/`cortex` sub-features on that existing dep — no new crate.
   `net_sdk::{delegation, enrollment, devices, operator, mesh_a2a, a2a}` are all
   `pub mod` and unconditionally compiled in the SDK.
2. **The handle + async-callback machinery already ships in the node binding.**
   `Identity` is an `#[napi]` class (`node/src/identity.rs:146`), so
   `derive_child_identity(parent: &Identity, label)` has a handle to take and
   return. The A2A task-executor is an async JS callback — the exact
   `ThreadsafeFunction`→`Promise` seam already used by `publish.rs`,
   `payment_signer.rs`, and `blob.rs`. Serve-handle lifecycles (`close()`/`stop()`
   before `NetMesh.shutdown()`) are the established `CapabilityGateway` /
   `PaymentProvider` / `LocalPublicationHandle` pattern.

## Doctrine (the crate's, restated at the Node edge)

- **No logic in bindings.** The delegation chain, revocation, enrollment
  handshake, and A2A lifecycle are decided in `net-sdk`. The binding builds the
  flow, marshals arguments, and projects results.
- **Non-custodial; keys never cross the boundary.** `derive_child_identity`
  returns an `Identity` **handle**, never key bytes; enrollment exchanges
  invite/join tokens, not keys. No raw-key path is added.
- **Structured results / handle lifecycles.** Serve handles
  (`EnrollmentServeHandle`, `A2aServeHandle`) retain a node clone and MUST expose
  `close()`/`stop()` (awaited `withdraw()` where the Rust handle has one) so a JS
  caller can release the node before `NetMesh.shutdown()` — a `#[napi]` class is
  GC-finalized, not scope-dropped (the `close()` gotcha in `bindings.md`).

---

## Workstreams

### WS-0 — Feature scaffolding + build/CI/release wiring

- `net-node/Cargo.toml`: add
  `delegation = ["net", "dep:net-sdk", "net-sdk/net", "net-sdk/cortex"]` and
  `a2a = ["delegation", "cortex"]` (mirroring Python); add both to `default`.
  (Node's `default` currently ends `…,consent,mcp,publish,payments`.)
- `node/src/lib.rs`: `#[cfg(feature = "delegation")] mod delegation;`,
  `#[cfg(feature = "delegation")] mod enrollment;`,
  `#[cfg(feature = "a2a")] mod a2a;` (matching Python's module gating — note
  enrollment shares the `delegation` gate).
- **Release** (`release-npm.yml` → `npm run build`): the `build` script does not
  pass `--no-default-features`, so adding to `default` ships them in
  `@net-mesh/core` and `@net-mesh/sdk` automatically. Optionally list them in the
  `package.json` `build`/`build:debug`/`build:test` scripts for explicitness.
- **CI test build** (`ci.yml` node vitest job) uses
  `--no-default-features --features net,cortex,…` — **must add `delegation,a2a`**
  there or the new suites won't compile/run.

### WS-1 — `delegation.rs` (~300 lines)

Port, as `#[napi]` classes / functions:
- `derive_child_identity(parent: &Identity, label: String) -> Identity`
- `RevocationRegistry` (open at a store path; check/revoke) + `default_revocation_store_path() -> Option<String>`
- `DelegationChain` (root → machine → gateway → subagent; the handle + its accessors)
- `NetMesh` methods: `rendezvousString()`, `join(...)`, `renew(...)`.

### WS-2 — `enrollment.rs` (~690 lines — the largest)

Port the device-lifecycle facade + value types:
- free fn `fingerprint(entity: Buffer) -> string`
- value classes: `InviteToken`, `JoinRequest`, `JoinOutcome`, `DeviceRecord`
- facades: `OperatorEnrollment` (invite/approve/list/revoke), `DeviceEnrollment`
  (join side)
- `EnrollmentServeHandle` (**needs `stop()`/`close()`** — retains the node)
- `NetMesh.serveEnrollmentAuto(...)`.

### WS-3 — `a2a.rs` (~200 lines)

- `NodeTaskExecutor`: the `net_sdk::a2a::TaskExecutor` impl backed by a JS async
  callback `(taskJson: string) => Promise<resultJson>`, via the
  `ThreadsafeFunction`→`Promise` bridge (reuse the `publish.rs`/`payment_signer.rs`
  shape, incl. the one-sided-timeout note).
- `A2aServeHandle` (**needs `stop()`/`close()`**).
- `NetMesh` methods: `serveA2a(executor, …)`, `submitTask(nodeId, taskJson)`,
  `taskStatus(nodeId, taskId)`, `cancelTask(nodeId, taskId)`.

### WS-4 — TS types + tests

- napi regenerates `index.d.ts`; verify the new classes/methods surface with the
  right camelCase shapes.
- Mirror the Python suites: `test/delegation.test.ts`, `test/enrollment.test.ts`,
  `test/a2a.test.ts` (twins of `test_delegation.py` / `test_enrollment.py` /
  `test_a2a.py`), plus Rust `#[cfg(test)]` unit tests for any pure marshaling
  (runnable under the napi test-link workaround — `RUSTFLAGS="-C
  link-arg=-undefined -C link-arg=dynamic_lookup"`). Build the `.node` with the
  new features and run the vitest suites end-to-end.

---

## Open decisions

1. **Enrollment scope.** In Python `enrollment` rides the `delegation` gate (no
   separate feature) and is the biggest piece (688 lines). Options: (a) mirror —
   `delegation` pulls enrollment too (full parity, one gate); (b) split
   enrollment behind its own feature and land delegation + a2a first. Recommend
   (a) for parity unless a smaller first cut is wanted.
2. **Async duals.** Python exposes coroutine duals (`AsyncNetMesh`); Node's napi
   `async fn` already returns a Promise, so most methods are single-form. Confirm
   no separate async class is needed (the `serve_*` handles are the only
   long-lived pieces).
3. **Handle `close()` semantics.** Decide `stop()` (drop, unregister) vs awaited
   `withdraw()` (re-announce then stop) per serve handle, matching the Rust
   handle's capabilities and the `LocalPublicationHandle` precedent.
4. **`Identity` non-custodial boundary.** Confirm `derive_child_identity` returns
   only a handle and never exposes seed/secret bytes across the boundary (audit
   against the existing `identity.rs` surface).

## Test matrix (target)

| Area | Rust unit | vitest (built `.node`) | Parity source |
|---|---|---|---|
| child-identity derivation | ✓ | ✓ | `test_delegation.py` |
| revocation registry | ✓ | ✓ | `test_delegation.py` |
| delegation chain build/verify | — | ✓ | `test_delegation.py` |
| enrollment invite→join→approve | — | ✓ | `test_enrollment.py` |
| enrollment serve handle lifecycle | — | ✓ (`close()` releases node) | `test_enrollment.py` |
| a2a serve + submit/status/cancel | — | ✓ (two-node) | `test_a2a.py` |
| a2a executor callback bridge | ✓ (marshaling) | ✓ | `test_a2a.py` |

## Effort + sequencing

~1,180 lines of new napi source across three modules + 13 `NetMesh` methods +
feature/CI/release wiring + TS types + three vitest suites — comparable to the
payments-parity effort. Suggested commit sequence (one per step, each compiling
under the napi test-link workaround): WS-0 scaffolding → WS-1 delegation → WS-2
enrollment → WS-3 a2a → WS-4 tests, then build the `.node` and run the vitest
suites before the release wiring is exercised.

## Not in scope

- Go/C bindings (no Hermes surface there; consistent with the payments matrix).
- Any change to the Rust SDK's delegation/enrollment/a2a semantics — this is
  purely the Node wrapper layer.
