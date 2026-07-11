# Implementation Plan: Node/TS delegation + A2A surface (Hermes parity)

**Status: LANDED 2026-07-10** (branch `node-hermes-parity`). Written 2026-07-09
to capture scope; implemented the next day. All four workstreams shipped:
`delegation` + `a2a` features (in `default` and the release/CI build matrices),
the three napi modules (`delegation.rs` ~300 lines, `enrollment.rs` ~660,
`a2a.rs` ~250), all 13 `NetMesh` methods, and three vitest suites mirroring the
Python twins — 12 delegation + 14 enrollment (incl. live join + renewal over
real UDP loopback) + 3 a2a, full node suite green (508 passed), clippy and
`cargo doc -D warnings` clean, `typecheck:tests` clean. Implementation notes
and resolved decisions are recorded inline below (search "Landed").

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

> **Status: WS-0 landed 2026-07-10.** Features added exactly as specified and
> to `default`; `delegation,a2a` added to the ci.yml vitest-job feature list
> AND (release-matrix explicitness, requested in review) to all three
> `package.json` build scripts. One extra seam: `mod common`'s cfg gate gained
> `feature = "delegation"` so the new modules reuse the canonical
> `common::bigint_u64` BigInt→u64 validation instead of growing a duplicate
> (`delegation::u64_arg` wraps it to name the offending argument). The sdk-ts
> CI job's feature list is untouched — the TS SDK layer has no Hermes surface.

### WS-1 — `delegation.rs` (~300 lines)

Port, as `#[napi]` classes / functions:
- `derive_child_identity(parent: &Identity, label: String) -> Identity`
- `RevocationRegistry` (open at a store path; check/revoke) + `default_revocation_store_path() -> Option<String>`
- `DelegationChain` (root → machine → gateway → subagent; the handle + its accessors)
- `NetMesh` methods: `rendezvousString()`, `join(...)`, `renew(...)`.

> **Status: WS-1 landed 2026-07-10.** As specified, plus the conventions the
> rest of the port reuses: u64s cross as `BigInt` (the `TokenInfo` precedent),
> TTL/window knobs as `number` (u32, the `handlerTimeoutMs` precedent);
> `TokenError`s map through the existing `identity::token_err_for` so the
> `token: <kind>` discriminators stay single-sourced;
> `GATEWAY_DELEGATION_CHANNEL` exports as a real `#[napi] pub const`.
> `Identity` gained three `pub(crate)` seams (`from_keypair_arc`,
> `entity_id_ref`, `secret_seed`, all `delegation`-gated; `to_sdk_identity`'s
> cfg widened) — the H8 audit point: the seed accessor is crate-internal and
> feeds only the Rust-side KDF; JS still sees handles only.

### WS-2 — `enrollment.rs` (~690 lines — the largest)

Port the device-lifecycle facade + value types:
- free fn `fingerprint(entity: Buffer) -> string`
- value classes: `InviteToken`, `JoinRequest`, `JoinOutcome`, `DeviceRecord`
- facades: `OperatorEnrollment` (invite/approve/list/revoke), `DeviceEnrollment`
  (join side)
- `EnrollmentServeHandle` (**needs `stop()`/`close()`** — retains the node)
- `NetMesh.serveEnrollmentAuto(...)`.

> **Status: WS-2 landed 2026-07-10.** As specified. Store-touching facade
> methods (`approve`, `handleJoinRequest`, `revoke`, `devices`, `forget`,
> `DeviceEnrollment.load/save`, `RevocationRegistry.loadFromStore`) are napi
> `async fn`s so file IO rides a tokio worker instead of the JS thread — the
> Node analog of the Python binding's `py.detach`. **The one real deviation
> from the survey's assumption:** napi sync methods have NO tokio runtime
> context (the substrate's `serve_rpc` spawns a response drainer →
> "no reactor running" panic, caught by the live vitest test on first run), so
> `serveEnrollmentAuto` is an `async fn` resolving to the handle — the same
> reason the Python binding wraps serve registration in `runtime.enter()`.

### WS-3 — `a2a.rs` (~200 lines)

- `NodeTaskExecutor`: the `net_sdk::a2a::TaskExecutor` impl backed by a JS async
  callback `(taskJson: string) => Promise<resultJson>`, via the
  `ThreadsafeFunction`→`Promise` bridge (reuse the `publish.rs`/`payment_signer.rs`
  shape, incl. the one-sided-timeout note).
- `A2aServeHandle` (**needs `stop()`/`close()`**).
- `NetMesh` methods: `serveA2a(executor, …)`, `submitTask(nodeId, taskJson)`,
  `taskStatus(nodeId, taskId)`, `cancelTask(nodeId, taskId)`.

> **Status: WS-3 landed 2026-07-10.** The executor callback is
> `(brief: TaskBriefJs) => Promise<string>` (a typed `#[napi(object)]` brief,
> not positional args or JSON blobs); submit takes
> `(targetNodeId, prompt, contextRefs?, tags?)` mirroring the Python
> signature. `serveA2a` is the `publish.rs` "sync setup, async continuation"
> shape — TSFN built on the JS thread (the `Function` is `!Send`), then
> `env.spawn_future` for the registration (same no-reactor constraint as
> WS-2) — resolving to the handle. **The one-sided-cancellation note became a
> documented divergence:** Python cancels the executor's *coroutine*; a JS
> Promise can't be aborted from outside, so a cancel discards the handler's
> eventual result and records `Cancelled` — the wire-visible contract (state
> flips and stays `cancelled`, no result served) is what the tests pin.
> JS names pinned with `js_name` (`serveA2a`, `A2aServeHandle`) — napi's
> auto-camelCase would emit `serveA2A` / `A2AServeHandle`.
>
> **Review follow-up (cubic, 2026-07-11):** `serveA2a` gained
> `ServeA2aOptions.handlerTimeoutMs` (default 1 hour, `0` disables — the
> Python-parity opt-out), one deadline across the handler returning its
> Promise and that Promise settling. Past it the task records a `Failed`
> terminal state, so a wedged event loop / never-settling Promise can no
> longer strand an accepted task in `Running` (and in the registry) forever;
> pinned by a never-settling-executor vitest case. And every store-IO facade
> method (`OperatorEnrollment.approve/handleJoinRequest/revoke/devices/
> forget`, `DeviceEnrollment.load/save`, `RevocationRegistry.loadFromStore`)
> now runs its synchronous file IO under `tokio::task::spawn_blocking` so a
> slow or large store can't starve a napi runtime worker.

### WS-4 — TS types + tests

- napi regenerates `index.d.ts`; verify the new classes/methods surface with the
  right camelCase shapes.
- Mirror the Python suites: `test/delegation.test.ts`, `test/enrollment.test.ts`,
  `test/a2a.test.ts` (twins of `test_delegation.py` / `test_enrollment.py` /
  `test_a2a.py`), plus Rust `#[cfg(test)]` unit tests for any pure marshaling
  (runnable under the napi test-link workaround — `RUSTFLAGS="-C
  link-arg=-undefined -C link-arg=dynamic_lookup"`). Build the `.node` with the
  new features and run the vitest suites end-to-end.

> **Status: WS-4 landed 2026-07-10.** `index.d.ts` regenerated (it is
> gitignored — each build regenerates it) and every shape verified; the three
> vitest suites mirror the Python twins test-for-test, with the a2a "zombie
> coroutine" case adapted to assert the JS contract (state stays `cancelled`
> past the handler's completion point) per the WS-3 divergence. No new Rust
> unit tests: the modules' only pure logic is the one-line `u64_arg` wrapper
> over the already-tested `common::bigint_u64`. Full verification: 29 new
> tests green, whole node suite 508 passed, clippy clean (one
> `wrong_self_convention` allow on `JoinOutcome.intoChain`, same as Python),
> `cargo doc -D warnings` clean, both typechecks clean (two pre-existing
> vitest typecheck errors — the `cross_lang_compat` `RawMeshRpc` stub missing
> the bidi-streaming members, an implicit-any in `deck.test.ts` — fixed in
> passing).

---

## Open decisions (all resolved 2026-07-10)

1. **Enrollment scope.** ~~Options (a)/(b)~~ **Resolved: (a), mirror.**
   Enrollment rides the `delegation` gate exactly as in Python — one gate, full
   parity in one cut.
2. **Async duals.** **Resolved: no separate async class.** napi `async fn` =
   Promise covers everything single-form. The nuance discovered in
   implementation runs the other way: several methods that are *sync* in
   Python had to become async in Node — store-IO facade methods (off the JS
   thread) and the serve registrations (napi sync calls have no tokio runtime
   context; the substrate's `serve_rpc` spawns a drainer task and panics with
   "no reactor running" otherwise). `serveEnrollmentAuto` is a plain
   `async fn`; `serveA2a` is sync TSFN setup + `env.spawn_future` (its
   `Function` arg is `!Send`) — both resolve to their handles.
3. **Handle `close()` semantics.** **Resolved: `stop()`** (drop, unregister) +
   a `serving` getter on both `EnrollmentServeHandle` and `A2aServeHandle`,
   mirroring the Python handles one-for-one. No awaited `withdraw()`: the
   underlying SDK `ServeHandle`s have no re-announce path (that is a
   `LocalPublicationHandle`/announcement concept; these are plain nRPC service
   registrations).
4. **`Identity` non-custodial boundary.** **Resolved: holds.**
   `deriveChildIdentity` derives inside Rust (`derive_child_seed` over a
   `pub(crate)` seed accessor) and returns a handle built by the new
   `Identity::from_keypair_arc`; `DeviceEnrollment.device` shares the keypair
   `Arc` with a fresh token cache. No new JS-visible byte path beyond the
   pre-existing explicit `Identity.toBytes()` persistence escape hatch.

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
